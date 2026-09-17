//! Block bodies.
//!
//! The DAG stores headers; transactions live here. They are separate because
//! headers drive consensus and can be synced, validated, and ordered without
//! bodies ever being present — and because a header is 200 bytes while a body
//! can be megabytes.

use std::collections::HashMap;

use alloy_consensus::{TxEnvelope, transaction::Recovered};
use alloy_primitives::{Address, TxKind};
use chainname_ghostdag::ordering::AccessSet;
use chainname_primitives::BlockHash;

/// A transaction with its sender already recovered.
///
/// Signature recovery is expensive and happens once, at mempool admission or
/// block validation. Execution takes the recovered form.
pub type ChainTx = Recovered<TxEnvelope>;

/// Static access keys for the merge-set ordering rule.
///
/// The EVM's real access set is dynamic — a call's storage touches are not
/// knowable before execution — so this is an approximation from what the
/// transaction declares. ARCHITECTURE.md §6.2 and OPEN-PROBLEMS.md P-001.
///
/// The sender is always included. That is what makes same-sender transactions
/// always conflict, which is what keeps nonces monotonic through the reorder.
#[derive(Debug, Clone)]
pub struct TxAccessSet {
    keys: Vec<Address>,
}

impl TxAccessSet {
    /// Derives the static access set of a transaction.
    pub fn of(tx: &ChainTx) -> Self {
        use alloy_consensus::Transaction as _;

        let mut keys = Vec::with_capacity(4);
        keys.push(tx.signer());
        if let TxKind::Call(to) = tx.kind() {
            keys.push(to);
        }
        if let Some(list) = tx.access_list() {
            keys.extend(list.iter().map(|item| item.address));
        }
        keys.sort_unstable();
        keys.dedup();
        Self { keys }
    }

    /// The keys, sorted and deduplicated.
    pub fn keys(&self) -> &[Address] {
        &self.keys
    }
}

impl AccessSet for TxAccessSet {
    fn access_keys(&self) -> impl Iterator<Item = Address> {
        self.keys.iter().copied()
    }
}

/// Transactions, keyed by the block that carried them.
#[derive(Debug, Default)]
pub struct BodyStore {
    bodies: HashMap<BlockHash, Vec<ChainTx>>,
}

impl BodyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a block's transactions.
    pub fn insert(&mut self, block: BlockHash, transactions: Vec<ChainTx>) {
        self.bodies.insert(block, transactions);
    }

    /// A block's transactions, or an empty slice if the body is absent.
    ///
    /// Absent is not an error: a block with no transactions and a block whose
    /// body has not arrived are indistinguishable to the executor, and both
    /// contribute nothing.
    pub fn get(&self, block: BlockHash) -> &[ChainTx] {
        self.bodies.get(&block).map_or(&[], Vec::as_slice)
    }

    /// True if a body is present for this block.
    pub fn contains(&self, block: BlockHash) -> bool {
        self.bodies.contains_key(&block)
    }

    /// Number of bodies held.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// True if no bodies are held.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }
}
