//! Chain-block execution.
//!
//! The unit of state transition is the **selected-chain block**, never a DAG
//! block. Chain block N's transition consumes the GHOSTDAG-ordered merge set
//! of N plus N's own transactions.
//!
//! # Per-transaction beneficiary
//!
//! The EVM's beneficiary is reset before every transaction to the miner of the
//! DAG block that carried it. That is why this drives revm directly rather
//! than using `alloy-evm`'s `EthBlockExecutor`, which pays a single address
//! (DECISIONS.md C-004, D-007). It makes `COINBASE` mean "who mined the block
//! this transaction was in", which is the only reading that is well defined
//! inside a merge set.
//!
//! # Deferred state root
//!
//! A chain block's own header cannot carry the state root of its own
//! execution: a DAG block is mined before anyone knows which merge set will
//! contain it. Results are therefore recorded by height, and a header at
//! height N publishes the results of height N - D. See
//! [`ChainExecutor::deferred_result_for`].

use std::collections::BTreeMap;

use alloy_consensus::{Eip658Value, Receipt, ReceiptWithBloom, Transaction as _};
use alloy_eips::eip1559::BaseFeeParams;
use alloy_evm::Evm;
use alloy_primitives::{Address, B256, Log, U256};
use chainname_execution::{ChainBlockCtx, WorldState, make_evm, set_beneficiary};
use chainname_ghostdag::{
    DagStore,
    ordering::{base_sequence, layer_and_sort},
};
use chainname_primitives::{BlockHash, ChainParams};
use revm::{
    context::result::{ExecutionResult, ResultAndState},
    database_interface::DatabaseCommit,
};
use tracing::{debug, trace};

use crate::{
    bodies::{BodyStore, ChainTx, TxAccessSet},
    journal::UndoRecord,
};

/// EIP-1559 parameters.
///
/// Ethereum's values, unchanged. The base fee's job is to keep blocks near
/// half full, and that mechanism is independent of block interval — what
/// changes with the block rate is the per-block gas limit, not how the fee
/// responds to fullness. Wallets and fee estimators already model these
/// constants; changing them would break estimation for no benefit.
pub const BASE_FEE_PARAMS: BaseFeeParams = BaseFeeParams::new(8, 2);

/// Initial base fee, in wei per gas.
///
/// Ethereum's genesis figure. Purely a starting point; 1559 moves it within a
/// few blocks.
pub const INITIAL_BASE_FEE: u64 = 1_000_000_000;

/// What executing one chain block produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBlockOutcome {
    /// Selected-chain height.
    pub height: u64,
    /// The chain block itself.
    pub hash: BlockHash,
    /// State root after this block.
    pub state_root: B256,
    /// Receipts root of the transactions that executed.
    pub receipts_root: B256,
    /// Total gas consumed.
    pub gas_used: u64,
    /// Base fee this block charged, in wei per gas.
    pub base_fee_per_gas: u64,
    /// Transactions that executed, in canonical order.
    pub executed: usize,
    /// Transactions skipped because the block's gas budget ran out. They stay
    /// eligible for a later chain block.
    pub deferred: usize,
    /// Hashes of the transactions that executed, in canonical order.
    ///
    /// This is the order `eth_getBlockByNumber` reports, and the index a
    /// receipt lookup resolves against.
    pub transaction_hashes: Vec<B256>,
    /// Receipts for those transactions, in the same order.
    pub receipts: Vec<ReceiptWithBloom<Receipt<Log>>>,
    /// Timestamp of the chain block, in seconds.
    pub timestamp_secs: u64,
    /// The miner of the chain block itself.
    pub miner: Address,
}

/// Executes selected-chain blocks and maintains the world state.
#[derive(Debug)]
pub struct ChainExecutor {
    params: ChainParams,
    state: WorldState,
    /// Results by height, for the deferred state root and for RPC.
    results: BTreeMap<u64, ChainBlockOutcome>,
    /// Undo records by height, newest last.
    journal: Vec<(u64, BlockHash, UndoRecord)>,
    /// Rounds committed from speculation, and rounds that had to fall back.
    ///
    /// Diagnostic only. Reported so the speedup measurement can distinguish
    /// "parallelism did not help" from "parallelism never happened".
    parallel_rounds: u64,
    /// Rounds discarded because speculation found a real conflict.
    fallback_rounds: u64,
    /// Whether rounds are executed in parallel.
    ///
    /// Off by default. Parallel execution must produce byte-identical state to
    /// sequential; it is opt-in so that a bug in the parallel path cannot
    /// silently become the default behaviour of every node.
    parallel: bool,
    /// Height of the current chain tip.
    height: u64,
    /// Hash of the current chain tip.
    tip: BlockHash,
}

impl ChainExecutor {
    /// Creates an executor over a genesis state.
    pub fn new(params: ChainParams, genesis_state: WorldState, genesis_hash: BlockHash) -> Self {
        let genesis_root = genesis_state.state_root();
        let mut results = BTreeMap::new();
        results.insert(
            0,
            ChainBlockOutcome {
                height: 0,
                hash: genesis_hash,
                state_root: genesis_root,
                receipts_root: alloy_trie::EMPTY_ROOT_HASH,
                gas_used: 0,
                base_fee_per_gas: INITIAL_BASE_FEE,
                executed: 0,
                deferred: 0,
                transaction_hashes: Vec::new(),
                receipts: Vec::new(),
                timestamp_secs: 0,
                miner: Address::ZERO,
            },
        );

        Self {
            params,
            state: genesis_state,
            results,
            journal: Vec::new(),
            parallel: false,
            parallel_rounds: 0,
            fallback_rounds: 0,
            height: 0,
            tip: genesis_hash,
        }
    }

    /// Turns parallel round execution on or off.
    ///
    /// The result must be identical either way; `crates/chain/tests/parallel.rs`
    /// asserts that on every workload it can construct.
    pub const fn set_parallel(&mut self, parallel: bool) {
        self.parallel = parallel;
    }

    /// Whether rounds are executed in parallel.
    pub const fn is_parallel(&self) -> bool {
        self.parallel
    }

    /// Rounds committed from speculation, and rounds that fell back.
    pub const fn parallel_stats(&self) -> (u64, u64) {
        (self.parallel_rounds, self.fallback_rounds)
    }

    /// The current world state.
    pub const fn state(&self) -> &WorldState {
        &self.state
    }

    /// The chain height executed so far.
    pub const fn height(&self) -> u64 {
        self.height
    }

    /// The chain block executed most recently.
    pub const fn tip(&self) -> BlockHash {
        self.tip
    }

    /// The state root at the current height.
    pub fn state_root(&self) -> B256 {
        self.state.state_root()
    }

    /// The outcome recorded for a height, if it has been executed.
    pub fn result_at(&self, height: u64) -> Option<&ChainBlockOutcome> {
        self.results.get(&height)
    }

    /// The result a block at `height` should publish in its header.
    ///
    /// A header at height N carries the results of height `N - D`, where `D`
    /// is the deferred lag. Below `D` there is nothing to publish yet and the
    /// genesis result stands in, which is why genesis is seeded at height 0.
    pub fn deferred_result_for(&self, height: u64) -> Option<&ChainBlockOutcome> {
        let lag = self.params.deferred_state_root_lag();
        let target = height.saturating_sub(lag);
        self.results.get(&target)
    }

    /// Base fee for the block following `parent_height`.
    pub fn next_base_fee(&self, parent_height: u64) -> u64 {
        let Some(parent) = self.results.get(&parent_height) else {
            return INITIAL_BASE_FEE;
        };
        // The fee is steered towards the *amortised* target, not the hard
        // ceiling. Those differ at high block rates, where the ceiling is
        // raised so a maximum-size transaction can still be included
        // (ChainParams::block_gas_limit). Steering on the ceiling instead
        // would let sustained throughput run at several times the advertised
        // target before the fee reacted at all.
        //
        // `calc_next_block_base_fee` derives its target as
        // `gas_limit / elasticity`, so passing twice the target with an
        // elasticity of 2 makes it steer towards exactly the target.
        alloy_eips::eip1559::calc_next_block_base_fee(
            parent.gas_used,
            self.params.block_gas_target().saturating_mul(2),
            parent.base_fee_per_gas,
            BASE_FEE_PARAMS,
        )
    }

    /// Applies a chain reorganisation: undo `removed`, then execute `added`.
    ///
    /// `removed` must be tip-first and `added` oldest-first, which is what
    /// [`crate::compute_reorg`] produces.
    pub fn apply_reorg(
        &mut self,
        dag: &DagStore,
        bodies: &BodyStore,
        removed: &[BlockHash],
        added: &[BlockHash],
    ) -> Result<Vec<ChainBlockOutcome>, ExecutionError> {
        for hash in removed {
            self.undo_one(*hash)?;
        }

        let mut outcomes = Vec::with_capacity(added.len());
        for hash in added {
            outcomes.push(self.execute_chain_block(dag, bodies, *hash)?);
        }
        Ok(outcomes)
    }

    /// Undoes the most recently executed chain block.
    fn undo_one(&mut self, expected: BlockHash) -> Result<(), ExecutionError> {
        let Some((height, hash, record)) = self.journal.pop() else {
            return Err(ExecutionError::NothingToUndo { expected });
        };
        if hash != expected {
            // Put it back: an undo out of order means the caller's reorg does
            // not match what was executed, and silently proceeding would
            // corrupt state in a way no later check would catch.
            self.journal.push((height, hash, record));
            return Err(ExecutionError::UndoOutOfOrder { expected, found: hash });
        }

        record.apply(&mut self.state);
        self.results.remove(&height);
        self.height = height.saturating_sub(1);
        self.tip = self
            .journal
            .last()
            .map(|(_, h, _)| *h)
            .unwrap_or_else(|| self.results.get(&0).map_or(hash, |g| g.hash));
        trace!(height, %hash, "chain block undone");
        Ok(())
    }

    /// Executes one selected-chain block.
    pub fn execute_chain_block(
        &mut self,
        dag: &DagStore,
        bodies: &BodyStore,
        hash: BlockHash,
    ) -> Result<ChainBlockOutcome, ExecutionError> {
        let header = dag.header(hash).ok_or(ExecutionError::UnknownBlock(hash))?.clone();
        let height = self.height + 1;

        // --- 1. The base sequence: merge set in topological order, then this
        //        block, with each transaction tagged by the block that carried
        //        it so fees and COINBASE can be attributed correctly.
        let mut tagged: Vec<(Address, ChainTx)> = Vec::new();
        for source in base_sequence(dag, hash) {
            let miner = dag.header(source).map(|h| h.miner).unwrap_or(Address::ZERO);
            for tx in bodies.get(source) {
                tagged.push((miner, tx.clone()));
            }
        }

        // --- 2. Deduplicate. A transaction included in several parallel
        //        blocks executes once; the first occurrence owns it.
        let mut seen = std::collections::HashSet::new();
        tagged.retain(|(_, tx)| seen.insert(*tx.inner().hash()));

        // --- 3. The canonical order.
        let access: Vec<TxAccessSet> = tagged.iter().map(|(_, tx)| TxAccessSet::of(tx)).collect();
        let order = layer_and_sort(&access);

        // --- 4. Execute.
        let base_fee_per_gas = self.next_base_fee(self.height);
        let gas_limit = self.params.block_gas_limit();
        let ctx = ChainBlockCtx {
            height,
            // Headers carry milliseconds; the TIMESTAMP opcode is specified in
            // seconds and contracts depend on that unit.
            timestamp_secs: header.timestamp_ms / 1_000,
            base_fee_per_gas,
            gas_limit,
            // The chain block's own proof-of-work hash stands in for
            // PREVRANDAO. Miner-grindable and not a randomness beacon:
            // DECISIONS.md D-012, OPEN-PROBLEMS.md P-005.
            prevrandao: hash,
        };

        let outcome_parts = if self.parallel {
            self.execute_rounds_parallel(&tagged, &order, &access, &ctx, gas_limit)
        } else {
            self.execute_sequentially(&tagged, &order, &ctx, gas_limit)
        };
        let ExecutedBlock { undo, gas_used, executed, deferred, receipts, transaction_hashes } =
            outcome_parts;

        let state_root = self.state.state_root();
        // A real receipts root: the Merkle root over RLP-encoded receipts,
        // exactly as Ethereum computes it, so `eth_getTransactionReceipt` and
        // receipt proofs mean what clients expect.
        let receipts_root = alloy_trie::root::ordered_trie_root(&receipts);
        let _ = &receipts;

        let outcome = ChainBlockOutcome {
            height,
            hash,
            state_root,
            receipts_root,
            gas_used,
            base_fee_per_gas,
            executed,
            deferred,
            transaction_hashes,
            receipts,
            timestamp_secs: ctx.timestamp_secs,
            miner: header.miner,
        };

        self.journal.push((height, hash, undo));
        self.results.insert(height, outcome.clone());
        self.height = height;
        self.tip = hash;

        // Prune automatically. Undo records hold the pre-value of every
        // account a block touched, so an unpruned journal grows without bound
        // on a long-running node. Records older than the pruning window can
        // never be needed: a reorg that deep is not something to unwind, it is
        // something to resync from.
        let window = self.params.pruning_window_blocks();
        if height > window {
            self.prune_journal(height - window);
        }

        trace!(height, %hash, executed, deferred, gas_used, "chain block executed");
        Ok(outcome)
    }

    /// Executes the canonical order one transaction at a time.
    ///
    /// The reference implementation. Whatever [`Self::execute_rounds_parallel`]
    /// does, it must produce exactly this.
    fn execute_sequentially(
        &mut self,
        tagged: &[(Address, ChainTx)],
        order: &[usize],
        ctx: &ChainBlockCtx,
        gas_limit: u64,
    ) -> ExecutedBlock {
        let mut block = ExecutedBlock::default();

        let state = std::mem::take(&mut self.state);
        let mut evm = make_evm(state, &self.params, ctx);

        for index in order {
            let (miner, tx) = &tagged[*index];

            // The gas budget is a *block* limit, not a per-transaction one. A
            // transaction that does not fit is skipped, not failed: it stays
            // valid and eligible for a later chain block.
            if block.gas_used.saturating_add(tx.gas_limit()) > gas_limit {
                block.deferred += 1;
                continue;
            }

            set_beneficiary(&mut evm, *miner);

            match evm.transact(tx) {
                Ok(ResultAndState { result, state: changes }) => {
                    for address in changes.keys() {
                        block.undo.note(*address, evm.db());
                    }
                    evm.db_mut().commit(changes);
                    block.record(tx, &result);
                }
                Err(error) => {
                    debug!(
                        hash = %tx.inner().hash(),
                        %error,
                        "transaction not executable, skipping"
                    );
                    block.deferred += 1;
                }
            }
        }

        self.state = evm.into_db();
        block
    }

    /// Executes the canonical order a round at a time, speculatively.
    ///
    /// Each round is executed in parallel against the same pre-round state,
    /// then validated. If any transaction in the round actually touched an
    /// account another read or wrote, the whole round is discarded and
    /// re-executed sequentially — so the result is always the sequential one.
    ///
    /// See [`crate::parallel`] for why that is sufficient.
    fn execute_rounds_parallel(
        &mut self,
        tagged: &[(Address, ChainTx)],
        order: &[usize],
        access: &[TxAccessSet],
        ctx: &ChainBlockCtx,
        gas_limit: u64,
    ) -> ExecutedBlock {
        let layers = crate::parallel::partition_rounds(order, access);
        let mut block = ExecutedBlock::default();

        for layer in layers {
            let round: Vec<(Address, ChainTx)> = layer.iter().map(|i| tagged[*i].clone()).collect();

            let speculations = crate::parallel::speculate(&self.state, &self.params, ctx, &round);

            if crate::parallel::validate(&speculations).is_some() {
                self.fallback_rounds += 1;
                // Not independent after all. Fall back for this round only,
                // reusing the sequential path so there is one implementation
                // of the semantics rather than two.
                let sequential = self.execute_sequentially(tagged, &layer, ctx, gas_limit);
                block.absorb(sequential);
                continue;
            }

            // Independent: apply the same inclusion rules the sequential path
            // would, in canonical order, using results already computed.
            self.parallel_rounds += 1;
            let mut credited: Vec<(Address, U256)> = Vec::new();
            for (position, speculation) in speculations.into_iter().enumerate() {
                let index = &layer[position];
                let (miner, tx) = &tagged[*index];

                if block.gas_used.saturating_add(tx.gas_limit()) > gas_limit {
                    block.deferred += 1;
                    continue;
                }
                let Some(result) = speculation.result.clone() else {
                    block.deferred += 1;
                    continue;
                };

                for address in speculation.diff.keys() {
                    block.undo.note(*address, &self.state);
                }
                block.undo.note(*miner, &self.state);
                // Moved, not cloned: an `EvmState` clone per transaction is
                // pure overhead on the path this optimisation exists to make
                // fast.
                credited.push((*miner, speculation.fee_credit));
                block.record(tx, &result);
                self.state.commit(speculation.diff);
            }

            // Fee credits last: they were removed from the diffs so they would
            // not look like conflicts, and addition commutes.
            for (miner, credit) in credited {
                if credit.is_zero() {
                    continue;
                }
                let mut account = self.state.account(miner).cloned().unwrap_or_default();
                account.info.balance = account.info.balance.saturating_add(credit);
                self.state.insert_account(miner, account);
            }
        }

        block
    }

    /// Discards undo records below `height`, which can no longer be reorged.
    ///
    /// Without this the journal grows forever. The pruning horizon is the
    /// finality window: reorgs deeper than that are not expected, and a node
    /// that sees one must resync rather than unwind.
    pub fn prune_journal(&mut self, below_height: u64) {
        self.journal.retain(|(height, _, _)| *height >= below_height);
    }

    /// How many undo records are held.
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }

    /// Total accounts covered by all held undo records.
    ///
    /// A closer proxy for the journal's real memory cost than its length: one
    /// record for a block that touched a thousand accounts is far larger than
    /// a thousand records for blocks that touched one each.
    pub fn journal_entries(&self) -> usize {
        self.journal.iter().map(|(_, _, record)| record.len()).sum()
    }
}

/// Accumulated results of executing one chain block.
#[derive(Debug, Default)]
struct ExecutedBlock {
    undo: UndoRecord,
    gas_used: u64,
    executed: usize,
    deferred: usize,
    receipts: Vec<ReceiptWithBloom<Receipt<Log>>>,
    transaction_hashes: Vec<B256>,
}

impl ExecutedBlock {
    /// Records one executed transaction's receipt and gas.
    fn record(&mut self, tx: &ChainTx, result: &ExecutionResult) {
        self.gas_used = self.gas_used.saturating_add(result.tx_gas_used());
        self.executed += 1;
        self.transaction_hashes.push(*tx.inner().hash());

        let receipt = Receipt {
            status: Eip658Value::Eip658(matches!(result, ExecutionResult::Success { .. })),
            cumulative_gas_used: self.gas_used,
            logs: result.logs().to_vec(),
        };
        self.receipts.push(receipt.into());

        if !matches!(result, ExecutionResult::Success { .. }) {
            trace!(
                hash = %tx.inner().hash(),
                "transaction reverted; still included and still charged"
            );
        }
    }

    /// Folds another block's results into this one.
    ///
    /// Used when a round falls back to sequential execution: cumulative gas in
    /// the fallback's receipts restarted from zero, so it is rebased here.
    fn absorb(&mut self, other: Self) {
        let base = self.gas_used;
        self.undo.merge(other.undo);
        self.gas_used = base.saturating_add(other.gas_used);
        self.executed += other.executed;
        self.deferred += other.deferred;
        self.transaction_hashes.extend(other.transaction_hashes);
        for mut receipt in other.receipts {
            receipt.receipt.cumulative_gas_used =
                base.saturating_add(receipt.receipt.cumulative_gas_used);
            self.receipts.push(receipt);
        }
    }
}

/// Reasons a chain block could not be executed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExecutionError {
    /// The block is not in the DAG.
    #[error("cannot execute unknown block {0}")]
    UnknownBlock(BlockHash),
    /// An undo was requested with nothing left to undo.
    #[error("nothing left to undo, but {expected} was expected")]
    NothingToUndo {
        /// The block the caller wanted undone.
        expected: BlockHash,
    },
    /// An undo was requested out of order, which would corrupt state.
    #[error("undo out of order: expected {expected}, journal has {found}")]
    UndoOutOfOrder {
        /// The block the caller wanted undone.
        expected: BlockHash,
        /// The block actually at the top of the journal.
        found: BlockHash,
    },
}
