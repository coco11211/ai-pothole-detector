//! The node state the RPC serves from.
//!
//! One lock over everything. A finer-grained scheme would be faster and would
//! also make it possible to serve a half-applied reorg — a balance from before
//! it and a receipt from after. Consistency matters more than concurrency for a
//! testnet node, so the lock stays coarse until there is a measured reason.

use std::{collections::HashMap, sync::Arc};

use alloy_consensus::{Receipt, ReceiptWithBloom, TxEnvelope, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, Log, U256};
use chainname_chain::{BodyStore, ChainBlockOutcome, ChainExecutor, ChainTx, compute_reorg};
use chainname_execution::WorldState;
use chainname_ghostdag::{DagError, DagStore};
use chainname_pool::{PoolConfig, PoolError, TxPool};
use chainname_primitives::{BlockHash, ChainParams, Header};
use parking_lot::RwLock;
use tracing::debug;

/// Where a transaction was executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxLocation {
    /// Selected-chain height.
    pub height: u64,
    /// The chain block that executed it.
    pub chain_block: BlockHash,
    /// Index within that block's canonical order.
    pub index: usize,
}

/// Everything the node knows.
#[derive(Debug)]
pub struct NodeState {
    /// Consensus parameters.
    pub params: ChainParams,
    /// The block DAG.
    pub dag: DagStore,
    /// Block bodies, decoded and with senders recovered.
    pub bodies: BodyStore,
    /// Chain execution and world state.
    pub executor: ChainExecutor,
    /// The transaction pool.
    pub pool: TxPool,
    /// Transaction hash -> where it executed.
    pub tx_index: HashMap<B256, TxLocation>,
    /// Encoded transactions by hash, so `eth_getTransactionByHash` can answer
    /// for transactions that are mined as well as pooled.
    pub tx_bytes: HashMap<B256, Bytes>,
}

/// A handle to the node, cheap to clone and safe to share.
#[derive(Debug, Clone)]
pub struct Backend {
    inner: Arc<RwLock<NodeState>>,
}

impl Backend {
    /// Creates a backend over a fresh node.
    pub fn new(params: ChainParams, genesis: Header, genesis_state: WorldState, k: u16) -> Self {
        let genesis_hash = genesis.hash();
        let mergeset_limit = params.mergeset_size_limit();
        let pool_config = PoolConfig::new(params.chain_id, params.block_gas_limit());

        let state = NodeState {
            dag: DagStore::new(genesis, k, mergeset_limit),
            bodies: BodyStore::new(),
            executor: ChainExecutor::new(params.clone(), genesis_state, genesis_hash),
            pool: TxPool::new(pool_config),
            tx_index: HashMap::new(),
            tx_bytes: HashMap::new(),
            params,
        };

        Self { inner: Arc::new(RwLock::new(state)) }
    }

    /// Runs a closure with shared access.
    pub fn read<T>(&self, f: impl FnOnce(&NodeState) -> T) -> T {
        f(&self.inner.read())
    }

    /// Runs a closure with exclusive access.
    pub fn write<T>(&self, f: impl FnOnce(&mut NodeState) -> T) -> T {
        f(&mut self.inner.write())
    }

    /// Chain parameters.
    pub fn params(&self) -> ChainParams {
        self.read(|state| state.params.clone())
    }

    /// Current selected-chain height.
    pub fn height(&self) -> u64 {
        self.read(|state| state.executor.height())
    }

    /// Base fee for the next block.
    pub fn next_base_fee(&self) -> u64 {
        self.read(|state| state.executor.next_base_fee(state.executor.height()))
    }

    /// Submits a raw EIP-2718 transaction to the pool.
    pub fn submit_raw(&self, encoded: Bytes) -> Result<B256, BackendError> {
        use alloy_eips::eip2718::Decodable2718;

        let envelope = TxEnvelope::decode_2718(&mut encoded.as_ref())
            .map_err(|e| BackendError::Decode(e.to_string()))?;
        let signer = {
            use alloy_consensus::transaction::SignerRecoverable;
            envelope.recover_signer().map_err(|e| BackendError::Recovery(e.to_string()))?
        };
        let recovered: Recovered<TxEnvelope> = Recovered::new_unchecked(envelope, signer);

        self.write(|state| {
            let base_fee = state.executor.next_base_fee(state.executor.height());
            let nonce = state.executor.state().nonce(signer);
            let balance = state.executor.state().balance(signer);
            let hash = state.pool.add(recovered, encoded.clone(), base_fee, nonce, balance)?;
            state.tx_bytes.insert(hash, encoded);
            Ok(hash)
        })
    }

    /// Adds a block and brings execution up to the new selected chain.
    ///
    /// Returns the outcomes of every chain block executed, oldest first.
    pub fn add_block(
        &self,
        header: Header,
        transactions: Vec<ChainTx>,
        encoded: Vec<Bytes>,
    ) -> Result<Vec<ChainBlockOutcome>, BackendError> {
        self.write(|state| {
            let hash = header.hash();
            state.dag.add_block(header)?;

            for (tx, bytes) in transactions.iter().zip(encoded.iter()) {
                state.tx_bytes.insert(*tx.inner().hash(), bytes.clone());
            }
            state.bodies.insert(hash, transactions);

            Self::advance(state)
        })
    }

    /// Brings execution up to the DAG's selected parent chain.
    fn advance(state: &mut NodeState) -> Result<Vec<ChainBlockOutcome>, BackendError> {
        let new_tip = state.dag.virtual_selected_parent();
        let old_tip = state.executor.tip();
        if new_tip == old_tip {
            return Ok(Vec::new());
        }

        let reorg = compute_reorg(&state.dag, old_tip, new_tip);

        // Drop index entries for blocks leaving the chain *before* undoing
        // them, so a lookup can never resolve to a block that is no longer
        // executed.
        for removed in &reorg.removed {
            if let Some(height) = state
                .executor
                .result_at(state.executor.height())
                .filter(|r| r.hash == *removed)
                .map(|r| r.height)
            {
                let _ = height;
            }
        }
        let removed_set: std::collections::HashSet<BlockHash> =
            reorg.removed.iter().copied().collect();
        state.tx_index.retain(|_, location| !removed_set.contains(&location.chain_block));

        let outcomes =
            state.executor.apply_reorg(&state.dag, &state.bodies, &reorg.removed, &reorg.added)?;

        for outcome in &outcomes {
            for (index, hash) in outcome.transaction_hashes.iter().enumerate() {
                state.tx_index.insert(
                    *hash,
                    TxLocation { height: outcome.height, chain_block: outcome.hash, index },
                );
            }
        }

        // Transactions the new chain executed are no longer pending. Those the
        // old chain executed and the new one did not return to the pool
        // automatically, because they were never removed from it.
        let executed: Vec<B256> =
            outcomes.iter().flat_map(|o| o.transaction_hashes.iter().copied()).collect();
        for hash in executed {
            state.pool.remove(hash);
        }
        let nonces: HashMap<Address, u64> = state
            .pool
            .hashes()
            .into_iter()
            .filter_map(|h| state.pool.get(h).map(|t| t.sender()))
            .map(|sender| (sender, state.executor.state().nonce(sender)))
            .collect();
        state.pool.prune(|address| nonces.get(&address).copied().unwrap_or(0));

        if reorg.depth() > 0 {
            debug!(depth = reorg.depth(), "chain reorganised");
        }
        Ok(outcomes)
    }

    /// The receipt for a transaction, with where it executed.
    pub fn receipt(
        &self,
        hash: B256,
    ) -> Option<(TxLocation, ReceiptWithBloom<Receipt<Log>>, ChainBlockOutcome)> {
        self.read(|state| {
            let location = *state.tx_index.get(&hash)?;
            let outcome = state.executor.result_at(location.height)?.clone();
            let receipt = outcome.receipts.get(location.index)?.clone();
            Some((location, receipt, outcome))
        })
    }

    /// An account's balance at the executed tip.
    pub fn balance(&self, address: Address) -> U256 {
        self.read(|state| state.executor.state().balance(address))
    }

    /// An account's nonce at the executed tip.
    pub fn nonce(&self, address: Address) -> u64 {
        self.read(|state| state.executor.state().nonce(address))
    }

    /// The pending nonce: the executed nonce plus anything queued in the pool.
    ///
    /// This is what wallets ask for when they build a transaction, and
    /// answering with the executed nonce alone makes every second transaction
    /// from a wallet collide with the first.
    pub fn pending_nonce(&self, address: Address) -> u64 {
        self.read(|state| {
            let base = state.executor.state().nonce(address);
            let queued = state
                .pool
                .hashes()
                .into_iter()
                .filter_map(|h| state.pool.get(h))
                .filter(|t| t.sender() == address)
                .map(|t| t.nonce())
                .max();
            match queued {
                Some(highest) if highest >= base => highest + 1,
                _ => base,
            }
        })
    }
}

/// Reasons a backend operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// A raw transaction would not decode.
    #[error("could not decode transaction: {0}")]
    Decode(String),
    /// A transaction's signature would not recover.
    #[error("could not recover sender: {0}")]
    Recovery(String),
    /// The pool refused the transaction.
    #[error(transparent)]
    Pool(#[from] PoolError),
    /// The DAG refused the block.
    #[error(transparent)]
    Dag(#[from] DagError),
    /// Execution failed.
    #[error(transparent)]
    Execution(#[from] chainname_chain::ExecutionError),
}
