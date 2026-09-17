//! A single-node development chain.
//!
//! Boots a DAG, an executor, a pool and a JSON-RPC server, and mines blocks
//! from the pool on a timer. This is what Foundry, MetaMask and any other
//! Ethereum client connect to.
//!
//! Proof of work is real but the target is deliberately easy: the point of a
//! dev node is to exercise the RPC and execution paths, not to burn CPU.

use std::net::SocketAddr;

use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use chainname_chain::ChainTx;
use chainname_difficulty::CompactTarget;
use chainname_execution::load_genesis;
use chainname_pow::{DoubleKeccak256, PowHash};
use chainname_primitives::{BlockHash, ChainParams, HEADER_VERSION, Header};
use chainname_rpc::{Backend, RpcServerHandle, serve};
use tracing::{info, warn};

/// Genesis timestamp for a dev chain: fixed, so restarts are reproducible.
const DEV_GENESIS_MS: u64 = 1_700_000_000_000;

/// Mining target for the dev chain: roughly 16 hashes per block.
///
/// Real proof of work through the real validator, at a difficulty chosen so a
/// developer's laptop is never the bottleneck.
fn dev_target() -> U256 {
    U256::MAX >> 4
}

/// Balance given to every pre-funded dev account, in wei. 10,000 units.
const DEV_BALANCE_WEI: u128 = 10_000_000_000_000_000_000_000;

/// How many deterministic dev accounts to fund.
const DEV_ACCOUNTS: u64 = 10;

/// The well-known Anvil/Hardhat development key.
///
/// Published in every Ethereum tutorial and deliberately included so a
/// developer's existing tooling, scripts and MetaMask import work unchanged
/// against this chain. **It is public. Never use it for anything of value.**
pub const WELL_KNOWN_DEV_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// A funded development account.
#[derive(Debug, Clone)]
pub struct DevAccount {
    /// Its address.
    pub address: Address,
    /// Its private key, hex encoded. Public by construction.
    pub private_key: String,
}

/// Derives the accounts a dev chain pre-funds.
///
/// Deterministic, so the same addresses appear on every run and a script can
/// hard-code them.
pub fn dev_accounts() -> Vec<DevAccount> {
    let mut accounts = Vec::new();

    if let Ok(key) = WELL_KNOWN_DEV_KEY.parse::<B256>()
        && let Ok(signer) = PrivateKeySigner::from_bytes(&key)
    {
        accounts.push(DevAccount {
            address: signer.address(),
            private_key: WELL_KNOWN_DEV_KEY.to_string(),
        });
    }

    for index in 0..DEV_ACCOUNTS {
        let key = alloy_primitives::keccak256(index.to_be_bytes());
        if let Ok(signer) = PrivateKeySigner::from_bytes(&key) {
            accounts.push(DevAccount {
                address: signer.address(),
                private_key: format!("0x{}", alloy_primitives::hex::encode(key)),
            });
        }
    }
    accounts
}

/// The genesis header for a dev chain.
pub fn dev_genesis_header() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: DEV_GENESIS_MS,
        bits: CompactTarget::from_target(dev_target()).to_u32(),
        nonce: 0,
        miner: Address::ZERO,
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    }
}

/// A running dev chain.
#[derive(Debug)]
pub struct DevNode {
    backend: Backend,
    params: ChainParams,
    miner: Address,
}

impl DevNode {
    /// Creates a dev chain with its accounts pre-funded.
    pub fn new(params: ChainParams, miner: Address) -> Self {
        let mut genesis = Genesis::default();
        for account in dev_accounts() {
            genesis.alloc.insert(
                account.address,
                GenesisAccount { balance: U256::from(DEV_BALANCE_WEI), ..Default::default() },
            );
        }
        let state = load_genesis(&genesis).expect("dev genesis is well formed");
        let backend = Backend::new(params.clone(), dev_genesis_header(), state, params.ghostdag_k);
        Self { backend, params, miner }
    }

    /// The backend, for the RPC server and for tests.
    pub const fn backend(&self) -> &Backend {
        &self.backend
    }

    /// Mines one block containing the best transactions from the pool.
    ///
    /// Returns the block's hash, or `None` if no nonce was found within the
    /// attempt budget (which at the dev target effectively never happens).
    pub fn mine_once(&self, timestamp_ms: u64) -> Option<BlockHash> {
        // How many transactions to pull. The block gas limit is the real
        // bound; this just avoids building an enormous candidate list.
        const MAX_TXS: usize = 512;
        /// Nonce attempts before giving up. At ~16 expected hashes, an
        /// exhausted budget means something is wrong, not bad luck.
        const MAX_ATTEMPTS: u64 = 1_000_000;

        let (transactions, encoded, parents, deferred, bits) = self.backend.write(|state| {
            let base_fee = state.executor.next_base_fee(state.executor.height());
            let chosen = state.pool.best_transactions(base_fee, MAX_TXS);

            let transactions: Vec<ChainTx> = chosen.iter().map(|t| t.tx.clone()).collect();
            let encoded: Vec<alloy_primitives::Bytes> =
                chosen.iter().map(|t| t.encoded.clone()).collect();

            let mut parents = state.dag.tips();
            // A block names at most this many parents. More would blow up the
            // merge set for no benefit.
            parents.truncate(16);
            parents.sort_unstable();
            parents.dedup();

            let next_height = state.executor.height() + 1;
            let deferred = state.executor.deferred_result_for(next_height).cloned();

            // The dev chain keeps a fixed target: retargeting is exercised by
            // the difficulty simulation, and a dev node that gets harder as you
            // use it would be a nuisance.
            let bits = CompactTarget::from_target(dev_target()).to_u32();

            (transactions, encoded, parents, deferred, bits)
        });

        if parents.is_empty() {
            warn!("no tips to build on");
            return None;
        }

        let txs_root = if encoded.is_empty() {
            alloy_trie::EMPTY_ROOT_HASH
        } else {
            alloy_trie::root::ordered_trie_root(&encoded)
        };

        let mut header = Header {
            version: HEADER_VERSION,
            parents,
            timestamp_ms,
            bits,
            nonce: 0,
            miner: self.miner,
            txs_root,
            deferred_height: deferred.as_ref().map_or(0, |d| d.height),
            deferred_state_root: deferred.as_ref().map_or(B256::ZERO, |d| d.state_root),
            deferred_receipts_root: deferred
                .as_ref()
                .map_or(alloy_trie::EMPTY_ROOT_HASH, |d| d.receipts_root),
            deferred_gas_used: deferred.as_ref().map_or(0, |d| d.gas_used),
        };

        let target = dev_target();
        let mut solved = false;
        for nonce in 0..MAX_ATTEMPTS {
            header.nonce = nonce;
            if U256::from_be_bytes(DoubleKeccak256.hash_header(&header).0) <= target {
                solved = true;
                break;
            }
        }
        if !solved {
            warn!("no proof of work found within the attempt budget");
            return None;
        }

        let hash = header.hash();
        match self.backend.add_block(header, transactions, encoded) {
            Ok(outcomes) => {
                for outcome in &outcomes {
                    info!(
                        height = outcome.height,
                        executed = outcome.executed,
                        deferred = outcome.deferred,
                        gas_used = outcome.gas_used,
                        "block executed"
                    );
                }
                Some(hash)
            }
            Err(error) => {
                warn!(%error, "mined block was rejected");
                None
            }
        }
    }

    /// Starts the RPC server.
    pub async fn serve_rpc(&self, addr: SocketAddr) -> std::io::Result<RpcServerHandle> {
        serve(self.backend.clone(), addr).await
    }

    /// Chain parameters.
    pub const fn params(&self) -> &ChainParams {
        &self.params
    }
}

/// Runs a mining loop until cancelled.
///
/// Mines whenever the pool has work, and on a slower heartbeat when it does
/// not, so a chain with no traffic still advances and clients polling
/// `eth_blockNumber` see progress.
pub async fn mining_loop(
    node: DevNode,
    interval_ms: u64,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    let mut cancel = cancel;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms.max(1)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Milliseconds since the epoch fits in u64 until the year
                // 584 million; a clock that reports otherwise is broken, and
                // the genesis timestamp is a safer fallback than a wrap.
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| u64::try_from(d.as_millis()).ok())
                    .unwrap_or(DEV_GENESIS_MS);
                node.mine_once(now_ms);
            }
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    info!("mining loop stopping");
                    return;
                }
            }
        }
    }
}
