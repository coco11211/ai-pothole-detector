//! The transaction pool.
//!
//! A standard mempool with fee-rate eviction. Nothing DAG-specific: a pool's
//! job is to hold transactions that are not yet in a block and hand miners the
//! most valuable ones, and that is the same job here as anywhere.
//!
//! What *is* specific is what happens after inclusion. Under deferred execution
//! a miner cannot know whether a transaction will still be valid when its block
//! is eventually merged (OPEN-PROBLEMS.md P-013), so admission here checks what
//! can be checked against the current state and accepts that some of it may be
//! stale by execution time.

use std::collections::{BTreeMap, HashMap};

use alloy_consensus::{Transaction as _, TxEnvelope, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, U256};

/// A transaction held in the pool.
#[derive(Debug, Clone)]
pub struct PooledTx {
    /// The transaction with its recovered sender.
    pub tx: Recovered<TxEnvelope>,
    /// The EIP-2718 encoding, kept so relaying never has to re-encode.
    pub encoded: Bytes,
    /// Arrival order, used to break fee ties deterministically.
    pub sequence: u64,
}

impl PooledTx {
    /// The transaction's hash.
    pub fn hash(&self) -> B256 {
        *self.tx.inner().hash()
    }

    /// The sender.
    pub fn sender(&self) -> Address {
        self.tx.signer()
    }

    /// The nonce.
    pub fn nonce(&self) -> u64 {
        self.tx.nonce()
    }

    /// Priority fee actually payable at `base_fee`, in wei per gas.
    ///
    /// `min(max_priority_fee, max_fee - base_fee)`. This, not the declared
    /// maximum, is what a miner earns, so it is what ordering and eviction use.
    pub fn effective_tip(&self, base_fee: u64) -> u128 {
        let max_fee = self.tx.max_fee_per_gas();
        let base = u128::from(base_fee);
        if max_fee <= base {
            return 0;
        }
        let headroom = max_fee - base;
        self.tx.max_priority_fee_per_gas().unwrap_or(0).min(headroom)
    }
}

/// The largest gas limit a single transaction may declare.
///
/// EIP-7825, activated in Osaka: 2^24 gas.
pub const TX_GAS_LIMIT_CAP: u64 = alloy_eips::eip7825::MAX_TX_GAS_LIMIT_OSAKA;

/// Pool limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    /// Largest number of transactions held.
    ///
    /// When full, the lowest fee-rate transaction is evicted — which is the
    /// only eviction policy that cannot be gamed by sending more.
    pub max_transactions: usize,
    /// Largest number of queued transactions from one sender.
    ///
    /// Without this, one account can fill the pool with a long nonce chain
    /// that will never execute.
    pub max_per_sender: usize,
    /// Chain id this pool accepts.
    pub chain_id: u64,
    /// Largest gas limit a single transaction may declare.
    pub max_tx_gas_limit: u64,
}

impl PoolConfig {
    /// Defaults for a network.
    pub const fn new(chain_id: u64, block_gas_limit: u64) -> Self {
        Self {
            // ~5,000 transactions is minutes of backlog at this chain's
            // throughput, which is enough to absorb a burst without becoming a
            // memory liability.
            max_transactions: 5_000,
            // A sender with more than this queued is either broken or hostile.
            max_per_sender: 64,
            chain_id,
            // A transaction that cannot fit in any block can never execute, so
            // it is refused rather than held. EIP-7825 caps a single
            // transaction at 2^24 gas regardless of how large blocks are, so
            // the binding limit is whichever is smaller.
            max_tx_gas_limit: if block_gas_limit < TX_GAS_LIMIT_CAP {
                block_gas_limit
            } else {
                TX_GAS_LIMIT_CAP
            },
        }
    }
}

/// The pool.
#[derive(Debug)]
pub struct TxPool {
    config: PoolConfig,
    by_hash: HashMap<B256, PooledTx>,
    /// Sender -> nonce -> hash. Ordered so a sender's transactions can be
    /// walked in nonce order without sorting.
    by_sender: BTreeMap<Address, BTreeMap<u64, B256>>,
    sequence: u64,
}

impl TxPool {
    /// An empty pool.
    pub fn new(config: PoolConfig) -> Self {
        Self { config, by_hash: HashMap::new(), by_sender: BTreeMap::new(), sequence: 0 }
    }

    /// Number of transactions held.
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// True if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }

    /// True if the pool holds this transaction.
    pub fn contains(&self, hash: B256) -> bool {
        self.by_hash.contains_key(&hash)
    }

    /// A held transaction.
    pub fn get(&self, hash: B256) -> Option<&PooledTx> {
        self.by_hash.get(&hash)
    }

    /// Every held transaction's hash, for inventory announcements.
    pub fn hashes(&self) -> Vec<B256> {
        let mut hashes: Vec<B256> = self.by_hash.keys().copied().collect();
        // Sorted so two nodes announce in the same order and a diff between
        // them is readable.
        hashes.sort_unstable();
        hashes
    }

    /// Admits a transaction.
    ///
    /// `account_nonce` and `account_balance` come from the current executed
    /// state. Both may be stale by the time the transaction executes; see the
    /// module docs.
    pub fn add(
        &mut self,
        tx: Recovered<TxEnvelope>,
        encoded: Bytes,
        base_fee: u64,
        account_nonce: u64,
        account_balance: U256,
    ) -> Result<B256, PoolError> {
        let hash = *tx.inner().hash();
        if self.by_hash.contains_key(&hash) {
            return Err(PoolError::AlreadyKnown(hash));
        }

        if tx.chain_id() != Some(self.config.chain_id) {
            return Err(PoolError::WrongChainId {
                found: tx.chain_id(),
                expected: self.config.chain_id,
            });
        }

        // EIP-4844 is cut: this chain has no L2s to serve and no blob market.
        // Refusing at admission means no blob transaction ever reaches a block.
        if matches!(tx.inner(), TxEnvelope::Eip4844(_)) {
            return Err(PoolError::BlobTransaction(hash));
        }

        if tx.gas_limit() > self.config.max_tx_gas_limit {
            return Err(PoolError::GasLimitTooHigh {
                found: tx.gas_limit(),
                limit: self.config.max_tx_gas_limit,
            });
        }

        if tx.nonce() < account_nonce {
            return Err(PoolError::NonceTooLow { found: tx.nonce(), account: account_nonce });
        }

        // The most this transaction could possibly cost, plus what it sends.
        let max_cost = U256::from(tx.max_fee_per_gas())
            .saturating_mul(U256::from(tx.gas_limit()))
            .saturating_add(tx.value());
        if account_balance < max_cost {
            return Err(PoolError::InsufficientFunds {
                required: max_cost,
                available: account_balance,
            });
        }

        let sender = tx.signer();
        let nonce = tx.nonce();

        let per_sender = self.by_sender.get(&sender).map_or(0, BTreeMap::len);
        let replacing = self.by_sender.get(&sender).and_then(|nonces| nonces.get(&nonce)).copied();

        if replacing.is_none() && per_sender >= self.config.max_per_sender {
            return Err(PoolError::TooManyFromSender { sender, limit: self.config.max_per_sender });
        }

        self.sequence += 1;
        let pooled = PooledTx { tx, encoded, sequence: self.sequence };

        // A replacement must pay strictly more, or replacement is free and the
        // pool can be churned at no cost.
        if let Some(existing_hash) = replacing {
            let existing_tip = self.by_hash[&existing_hash].effective_tip(base_fee);
            if pooled.effective_tip(base_fee) <= existing_tip {
                return Err(PoolError::ReplacementUnderpriced {
                    offered: pooled.effective_tip(base_fee),
                    existing: existing_tip,
                });
            }
            self.remove(existing_hash);
        }

        self.by_sender.entry(sender).or_default().insert(nonce, hash);
        self.by_hash.insert(hash, pooled);

        if self.by_hash.len() > self.config.max_transactions {
            self.evict_worst(base_fee);
        }

        Ok(hash)
    }

    /// Removes a transaction.
    pub fn remove(&mut self, hash: B256) -> Option<PooledTx> {
        let pooled = self.by_hash.remove(&hash)?;
        if let Some(nonces) = self.by_sender.get_mut(&pooled.sender()) {
            nonces.remove(&pooled.nonce());
            if nonces.is_empty() {
                self.by_sender.remove(&pooled.sender());
            }
        }
        Some(pooled)
    }

    /// Drops transactions that a newly executed state has made obsolete.
    pub fn prune(&mut self, account_nonces: impl Fn(Address) -> u64) {
        let stale: Vec<B256> = self
            .by_hash
            .values()
            .filter(|pooled| pooled.nonce() < account_nonces(pooled.sender()))
            .map(PooledTx::hash)
            .collect();
        for hash in stale {
            self.remove(hash);
        }
    }

    /// The most valuable transactions a miner could include, best first.
    ///
    /// Per-sender nonce order is preserved: a sender's nonce N+1 never appears
    /// before its nonce N, because it could not execute if it did.
    pub fn best_transactions(&self, base_fee: u64, limit: usize) -> Vec<&PooledTx> {
        // One candidate per sender at a time: its lowest queued nonce.
        let heads: Vec<(&Address, &BTreeMap<u64, B256>)> = self.by_sender.iter().collect();
        let mut taken_per_sender: HashMap<Address, usize> = HashMap::new();
        let mut out: Vec<&PooledTx> = Vec::with_capacity(limit.min(self.by_hash.len()));

        while out.len() < limit {
            let mut best: Option<(&PooledTx, usize)> = None;

            for (index, (sender, nonces)) in heads.iter().enumerate() {
                let skip = taken_per_sender.get(*sender).copied().unwrap_or(0);
                let Some((_, hash)) = nonces.iter().nth(skip) else { continue };
                let Some(candidate) = self.by_hash.get(hash) else { continue };

                let better = match best {
                    None => true,
                    Some((current, _)) => {
                        let a = candidate.effective_tip(base_fee);
                        let b = current.effective_tip(base_fee);
                        // Fee first, then arrival order. Arrival order is the
                        // tie-break rather than hash so that, all else equal,
                        // first-come is first-served.
                        a > b || (a == b && candidate.sequence < current.sequence)
                    }
                };
                if better {
                    best = Some((candidate, index));
                }
            }

            let Some((chosen, index)) = best else { break };
            out.push(chosen);
            *taken_per_sender.entry(*heads[index].0).or_insert(0) += 1;
        }

        out
    }

    /// Evicts the lowest fee-rate transaction.
    fn evict_worst(&mut self, base_fee: u64) {
        let worst = self
            .by_hash
            .values()
            // Highest sequence breaks ties, so the newest of equally cheap
            // transactions goes first and an established queue is not churned.
            .min_by_key(|pooled| {
                (pooled.effective_tip(base_fee), std::cmp::Reverse(pooled.sequence))
            })
            .map(PooledTx::hash);
        if let Some(hash) = worst {
            self.remove(hash);
        }
    }
}

/// Reasons a transaction was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PoolError {
    /// Already held.
    #[error("transaction {0} is already known")]
    AlreadyKnown(B256),
    /// Signed for a different chain.
    #[error("wrong chain id: transaction says {found:?}, this chain is {expected}")]
    WrongChainId {
        /// What the transaction declared.
        found: Option<u64>,
        /// What this chain is.
        expected: u64,
    },
    /// An EIP-4844 blob transaction. This chain has no blobs.
    #[error("blob transactions are not supported on this chain ({0})")]
    BlobTransaction(B256),
    /// Declares more gas than any block could hold.
    #[error("gas limit {found} exceeds the per-transaction limit {limit}")]
    GasLimitTooHigh {
        /// What the transaction declared.
        found: u64,
        /// The limit.
        limit: u64,
    },
    /// Nonce already used.
    #[error("nonce {found} is below the account nonce {account}")]
    NonceTooLow {
        /// The transaction's nonce.
        found: u64,
        /// The account's current nonce.
        account: u64,
    },
    /// The sender cannot cover the maximum cost.
    #[error("insufficient funds: needs {required}, has {available}")]
    InsufficientFunds {
        /// Maximum the transaction could cost.
        required: U256,
        /// The sender's balance.
        available: U256,
    },
    /// Too many queued from one sender.
    #[error("sender {sender} already has {limit} queued transactions")]
    TooManyFromSender {
        /// The sender.
        sender: Address,
        /// The per-sender limit.
        limit: usize,
    },
    /// A replacement that does not pay more.
    #[error("replacement underpriced: offered {offered}, existing pays {existing}")]
    ReplacementUnderpriced {
        /// The new transaction's effective tip.
        offered: u128,
        /// The existing transaction's effective tip.
        existing: u128,
    },
}
