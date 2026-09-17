//! Parallel execution over the merge-set ordering.
//!
//! Block-STM in spirit, narrowed to the structure the ordering rule already
//! provides. [`crate::ordering`] groups transactions into *rounds* whose
//! statically declared access sets are pairwise disjoint, so a round is a set
//! of transactions that are *probably* independent. Probably is not good
//! enough for consensus, so every round is executed speculatively and then
//! validated against what the EVM actually touched.
//!
//! # The contract
//!
//! Parallel execution must produce **exactly** the state sequential execution
//! would. Not equivalent, not usually-the-same: identical, down to the state
//! root. Anything less is a consensus split between nodes that happened to
//! schedule differently.
//!
//! That is guaranteed here by a simple argument. Each transaction in a round
//! runs against the same pre-round state and records which accounts it read
//! and wrote. If no transaction wrote an account that another read or wrote,
//! then running them one after another would have shown each of them exactly
//! the values it already saw — so their results are the sequential results,
//! and the order of application does not matter. When that does not hold, the
//! round is discarded and re-executed sequentially.
//!
//! # The fee-credit problem
//!
//! Every transaction credits its priority fee to the block's beneficiary, so
//! naively every transaction in a round writes the same account and nothing is
//! ever independent. Fee credits are commutative additions, so they are
//! excluded from conflict detection and re-applied afterwards — but only after
//! *proving* the beneficiary's balance moved by exactly the fee and nothing
//! else. A transaction that genuinely touches the miner's account is treated
//! as a conflict, because it is one.

use std::collections::HashSet;

use alloy_consensus::Transaction as _;
use alloy_evm::Evm;
use alloy_primitives::{Address, U256};
use chainname_execution::{ChainBlockCtx, ChainEvm, WorldState, make_evm, set_beneficiary};
use chainname_primitives::ChainParams;
use rayon::prelude::*;
use revm::{
    context::result::{ExecutionResult, ResultAndState},
    database::CacheDB,
    state::EvmState,
};

use crate::bodies::{ChainTx, TxAccessSet};

/// What one speculatively executed transaction produced.
#[derive(Debug)]
pub struct Speculation {
    /// Index within the round.
    pub index: usize,
    /// The execution result, or `None` if the transaction was not executable.
    pub result: Option<ExecutionResult>,
    /// The state diff to apply, with the beneficiary's fee credit removed.
    pub diff: EvmState,
    /// Accounts read during execution.
    pub reads: HashSet<Address>,
    /// Accounts written, excluding a beneficiary entry that was purely a fee.
    pub writes: HashSet<Address>,
    /// Priority fee credited to the beneficiary, which is applied separately
    /// because addition commutes.
    pub fee_credit: U256,
    /// True if the beneficiary's account was touched beyond the fee credit, in
    /// which case this transaction is not independent of any other in the
    /// round.
    pub touched_beneficiary: bool,
}

/// Why a round could not be committed in parallel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundConflict {
    /// Two transactions in the round touched the same account.
    Overlap {
        /// The earlier transaction's index in the round.
        earlier: usize,
        /// The later transaction's index.
        later: usize,
        /// The account they contended on.
        account: Address,
    },
    /// A transaction touched the beneficiary beyond crediting it a fee.
    BeneficiaryTouched {
        /// The transaction's index in the round.
        index: usize,
    },
}

/// Splits a canonical order into rounds.
///
/// Consecutive transactions with pairwise-disjoint static access sets form one
/// round. The ordering rule already assigned rounds; this recovers them from
/// the sorted order, which keeps the two definitions from drifting apart.
pub fn partition_rounds(order: &[usize], access: &[TxAccessSet]) -> Vec<Vec<usize>> {
    let mut rounds: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut touched: HashSet<Address> = HashSet::new();

    for index in order {
        let keys = access[*index].keys();
        let conflicts = keys.iter().any(|k| touched.contains(k));
        if conflicts && !current.is_empty() {
            rounds.push(std::mem::take(&mut current));
            touched.clear();
        }
        touched.extend(keys.iter().copied());
        current.push(*index);
    }
    if !current.is_empty() {
        rounds.push(current);
    }
    rounds
}

/// Executes a round speculatively, in parallel.
///
/// `round` is the transactions of one layer, in canonical order, each tagged
/// with the miner of the DAG block that carried it.
///
/// Returns one [`Speculation`] per transaction, in round order. Nothing is
/// committed; the caller validates and applies.
pub fn speculate(
    state: &WorldState,
    params: &ChainParams,
    ctx: &ChainBlockCtx,
    round: &[(Address, ChainTx)],
) -> Vec<Speculation> {
    let mut out: Vec<Speculation> = round
        .par_iter()
        .enumerate()
        // One EVM per worker thread, reused across the transactions that
        // thread handles, with only its read cache cleared between them.
        //
        // Building a fresh EVM per transaction is what made the first version
        // of this *slower* than sequential execution: revm's construction —
        // context, journal, precompile table — costs more than a value
        // transfer's actual execution, and the sequential path pays it once
        // per block rather than once per transaction.
        .map_init(
            || make_evm(CacheDB::new(state), params, ctx),
            |evm, (index, (miner, tx))| {
                // Reads are cached per transaction, so the cache must not
                // carry another transaction's view across. Cleared in place
                // rather than replaced: reassigning `Default::default()`
                // allocates four fresh maps per transaction, which on a
                // workload of microsecond transactions is a large fraction of
                // the total cost.
                let cache = &mut evm.db_mut().cache;
                cache.accounts.clear();
                cache.contracts.clear();
                cache.logs.clear();
                cache.block_hashes.clear();
                speculate_one(state, evm, index, *miner, tx)
            },
        )
        .collect();
    // `par_iter` preserves order through `collect`, but sorting makes that a
    // property of this function rather than of rayon's.
    out.sort_by_key(|s| s.index);
    out
}

fn speculate_one(
    state: &WorldState,
    evm: &mut ChainEvm<CacheDB<&WorldState>>,
    index: usize,
    miner: Address,
    tx: &ChainTx,
) -> Speculation {
    set_beneficiary(evm, miner);
    let ctx_base_fee = evm.block().basefee;

    let Ok(ResultAndState { result, state: diff }) = evm.transact(tx) else {
        return Speculation {
            index,
            result: None,
            diff: EvmState::default(),
            reads: HashSet::new(),
            writes: HashSet::new(),
            fee_credit: U256::ZERO,
            touched_beneficiary: false,
        };
    };

    // Everything the EVM loaded, which is the read set.
    let reads: HashSet<Address> = evm.db().cache.accounts.keys().copied().collect();

    // What revm credits the beneficiary: gas actually used times the priority
    // fee actually payable at this base fee.
    let effective_tip = tx
        .max_fee_per_gas()
        .saturating_sub(u128::from(ctx_base_fee))
        .min(tx.max_priority_fee_per_gas().unwrap_or(0));
    let expected_fee = U256::from(effective_tip).saturating_mul(U256::from(result.tx_gas_used()));

    let mut diff = diff;
    let mut fee_credit = U256::ZERO;
    let mut touched_beneficiary = false;

    if let Some(account) = diff.get(&miner) {
        let before = state.account(miner).map_or(U256::ZERO, |a| a.info.balance);
        let after = account.info.balance;
        let only_balance_changed = account.storage.is_empty()
            && account.info.nonce == state.account(miner).map_or(0, |a| a.info.nonce)
            && account.info.code_hash
                == state
                    .account(miner)
                    .map_or(alloy_primitives::KECCAK256_EMPTY, |a| a.info.code_hash);

        if only_balance_changed && after == before.saturating_add(expected_fee) {
            // Purely a fee credit. Addition commutes, so it is removed from the
            // diff and re-applied after the round.
            fee_credit = expected_fee;
            diff.remove(&miner);
        } else {
            // The transaction did something else to the miner's account, so it
            // is genuinely not independent.
            touched_beneficiary = true;
        }
    }

    let writes: HashSet<Address> = diff.keys().copied().collect();

    Speculation {
        index,
        result: Some(result),
        diff,
        reads,
        writes,
        fee_credit,
        touched_beneficiary,
    }
}

/// Checks that a round's speculations are genuinely independent.
///
/// Returns the first conflict found, or `None` if the round may be committed
/// in any order.
pub fn validate(speculations: &[Speculation]) -> Option<RoundConflict> {
    for (position, later) in speculations.iter().enumerate() {
        if later.touched_beneficiary {
            return Some(RoundConflict::BeneficiaryTouched { index: later.index });
        }
        for earlier in &speculations[..position] {
            // A conflict is an earlier write that a later transaction read or
            // wrote. Two reads of the same account are fine.
            for account in &earlier.writes {
                if later.reads.contains(account) || later.writes.contains(account) {
                    return Some(RoundConflict::Overlap {
                        earlier: earlier.index,
                        later: later.index,
                        account: *account,
                    });
                }
            }
        }
    }
    None
}

/// Applies a validated round's diffs and fee credits.
///
/// Applied in round order. Order is immaterial once [`validate`] has passed —
/// the diffs are disjoint — but doing it in order keeps the operation
/// reproducible under a debugger and costs nothing.
pub fn commit(state: &mut WorldState, speculations: &[Speculation], miners: &[Address]) {
    use revm::database_interface::DatabaseCommit;

    for speculation in speculations {
        if speculation.result.is_none() {
            continue;
        }
        state.commit(speculation.diff.clone());
    }

    // Fee credits last, accumulated per miner. These were removed from the
    // diffs precisely so they would not look like conflicts.
    for (speculation, miner) in speculations.iter().zip(miners.iter()) {
        if speculation.fee_credit.is_zero() {
            continue;
        }
        let mut account = state.account(*miner).cloned().unwrap_or_default();
        account.info.balance = account.info.balance.saturating_add(speculation.fee_credit);
        state.insert_account(*miner, account);
    }
}
