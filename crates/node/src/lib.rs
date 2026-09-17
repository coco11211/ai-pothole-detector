//! CHAINNAME node: configuration, logging, and lifecycle.
//!
//! At M1 this boots, opens storage, reports the resolved consensus parameters,
//! and exits cleanly. Consensus, execution, and networking arrive at M3, M2,
//! and M5 respectively.

pub mod config;

use chainname_storage::{RedbStore, StorageError};
use tracing::info;

pub use config::{ConfigError, Network, NodeConfig};

/// A booted node.
///
/// Holds the resources that must outlive a single call: storage, and later the
/// DAG, the executor, and the network handle.
#[derive(Debug)]
pub struct Node {
    config: NodeConfig,
    store: RedbStore,
}

impl Node {
    /// Boots a node: validates config, opens storage, logs the resolved
    /// consensus parameters.
    ///
    /// Logging the derived parameters at boot is deliberate. Every one of them
    /// is computed from a block rate, and a misconfigured rate silently changes
    /// gas limits, emission, and the deferred state root lag at once.
    pub fn boot(config: NodeConfig) -> Result<Self, NodeError> {
        config.validate()?;
        let params = config.params();

        let store = RedbStore::open(config.database_path())?;

        info!(
            network = ?config.network,
            chain_id = params.chain_id,
            block_interval_ms = params.target_block_interval_ms,
            blocks_per_second = params.blocks_per_second(),
            block_gas_limit = params.block_gas_limit(),
            deferred_state_root_lag = params.deferred_state_root_lag(),
            pruning_window_blocks = params.pruning_window_blocks(),
            emission_halflife_blocks = params.emission_halflife_blocks(),
            mergeset_size_limit = params.mergeset_size_limit(),
            ghostdag_k = params.ghostdag_k,
            database = %config.database_path().display(),
            "CHAINNAME node booted"
        );

        Ok(Self { config, store })
    }

    /// The resolved configuration.
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    /// The block store.
    pub fn store(&self) -> &RedbStore {
        &self.store
    }

    /// Shuts the node down. Explicit rather than relying on `Drop` so the log
    /// line is ordered correctly against the runtime's own teardown.
    pub fn shutdown(self) {
        info!("CHAINNAME node shutting down");
    }
}

/// Reasons a node failed to boot.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// Configuration was missing, malformed, or inconsistent.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Storage could not be opened or initialised.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Installs the global tracing subscriber.
///
/// Returns an error if a subscriber is already installed, which happens when
/// tests boot more than one node in a process; callers that may race should
/// ignore the error.
pub fn init_logging(filter: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(filter))?;
    fmt().with_env_filter(filter).with_target(true).try_init()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boots_and_shuts_down_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let config = NodeConfig { data_dir: dir.path().to_path_buf(), ..Default::default() };
        let node = Node::boot(config).unwrap();
        assert!(node.config().database_path().exists());
        node.shutdown();
    }

    #[test]
    fn boot_is_idempotent_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let config = NodeConfig { data_dir: dir.path().to_path_buf(), ..Default::default() };
        Node::boot(config.clone()).unwrap().shutdown();
        Node::boot(config).unwrap().shutdown();
    }

    #[test]
    fn boot_creates_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let config = NodeConfig { data_dir: nested.clone(), ..Default::default() };
        Node::boot(config).unwrap().shutdown();
        assert!(nested.join("chain.redb").exists());
    }
}
