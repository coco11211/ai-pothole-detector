//! Node configuration.
//!
//! Config is resolved in one place so a fresh session can see the whole
//! surface at a glance: CLI flags override a JSON file, which overrides the
//! built-in defaults.

use std::path::{Path, PathBuf};

use chainname_primitives::{ChainParams, ParamsError};
use serde::{Deserialize, Serialize};

/// Which staged network preset to run.
///
/// The rate is staged deliberately: `k = 18` is only proven at 1 bps and must
/// be recomputed from a *measured* propagation bound before 10 bps is used.
/// See OPEN-PROBLEMS.md P-008.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    /// 1 block/second. The configuration for M3 through M8.
    #[default]
    Testnet1Bps,
    /// 10 blocks/second. Not usable until M9; `k` is not yet known at this rate.
    Testnet10Bps,
}

impl Network {
    /// Consensus parameters for this network.
    pub const fn params(self) -> ChainParams {
        match self {
            Self::Testnet1Bps => ChainParams::testnet_1bps(),
            Self::Testnet10Bps => ChainParams::testnet_10bps(),
        }
    }
}

/// Fully resolved node configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Directory holding the database and any node-local state.
    pub data_dir: PathBuf,
    /// Which staged network to run.
    pub network: Network,
    /// Log filter, in `tracing_subscriber::EnvFilter` syntax.
    pub log_filter: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./chainname-data"),
            network: Network::default(),
            log_filter: "info".to_string(),
        }
    }
}

impl NodeConfig {
    /// Reads a config from a JSON file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
        serde_json::from_str(&text)
            .map_err(|source| ConfigError::Parse { path: path.to_path_buf(), source })
    }

    /// Path of the block database inside the data directory.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("chain.redb")
    }

    /// Consensus parameters implied by the selected network.
    pub fn params(&self) -> ChainParams {
        self.network.params()
    }

    /// Checks the config is internally consistent and its parameters derive
    /// exactly.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.params().validate().map_err(ConfigError::Params)
    }
}

/// Reasons a config could not be loaded or is unusable.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("failed to read config at {path}: {source}")]
    Read {
        /// Path attempted.
        path: PathBuf,
        /// Underlying io error.
        source: std::io::Error,
    },
    /// The config file was not valid JSON for this schema.
    #[error("failed to parse config at {path}: {source}")]
    Parse {
        /// Path attempted.
        path: PathBuf,
        /// Underlying parse error.
        source: serde_json::Error,
    },
    /// The implied chain parameters do not derive exactly.
    #[error("invalid chain parameters: {0}")]
    Params(#[source] ParamsError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        NodeConfig::default().validate().unwrap();
    }

    #[test]
    fn both_networks_validate() {
        for n in [Network::Testnet1Bps, Network::Testnet10Bps] {
            NodeConfig { network: n, ..Default::default() }.validate().unwrap();
        }
    }

    #[test]
    fn config_roundtrips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = NodeConfig {
            data_dir: PathBuf::from("/tmp/x"),
            network: Network::Testnet10Bps,
            log_filter: "debug".into(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(NodeConfig::from_file(&path).unwrap(), config);
    }

    #[test]
    fn missing_config_file_is_an_error() {
        let err = NodeConfig::from_file("/nonexistent/config.json").unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }));
    }

    #[test]
    fn malformed_config_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(NodeConfig::from_file(&path).unwrap_err(), ConfigError::Parse { .. }));
    }

    #[test]
    fn database_path_is_under_data_dir() {
        let config = NodeConfig { data_dir: PathBuf::from("/data"), ..Default::default() };
        assert_eq!(config.database_path(), PathBuf::from("/data/chain.redb"));
    }
}
