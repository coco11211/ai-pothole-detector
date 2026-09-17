//! CHAINNAME node binary.

use std::path::PathBuf;

use chainname_node::{Network, Node, NodeConfig, init_logging};
use clap::Parser;

/// CHAINNAME: a proof-of-work L1 with EVM execution and GHOSTDAG consensus.
#[derive(Debug, Parser)]
#[command(name = "chainname-node", version, about)]
struct Cli {
    /// Path to a JSON config file. CLI flags below override its values.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Directory holding the database and node-local state.
    #[arg(long, value_name = "DIR", env = "CHAINNAME_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Which staged network to run.
    #[arg(long, value_enum)]
    network: Option<Network>,

    /// Log filter, in EnvFilter syntax. `RUST_LOG` takes precedence.
    #[arg(long, value_name = "FILTER")]
    log_filter: Option<String>,

    /// Boot, report the resolved configuration, and exit without running.
    ///
    /// This is the M1 gate: it proves the node loads config, initialises
    /// storage, and exits cleanly.
    #[arg(long)]
    check: bool,
}

fn main() -> eyre::Result<()> {
    let cli = Cli::parse();

    let mut config = match &cli.config {
        Some(path) => NodeConfig::from_file(path)?,
        None => NodeConfig::default(),
    };
    if let Some(data_dir) = cli.data_dir {
        config.data_dir = data_dir;
    }
    if let Some(network) = cli.network {
        config.network = network;
    }
    if let Some(log_filter) = cli.log_filter {
        config.log_filter = log_filter;
    }

    // A subscriber may already be installed by an embedding process; that is
    // not a reason to refuse to boot.
    let _ = init_logging(&config.log_filter);

    let node = Node::boot(config)?;

    if cli.check {
        node.shutdown();
        return Ok(());
    }

    // M3 installs the mining loop here; M5 the network; M7 the RPC server.
    tracing::warn!(
        "no runnable subsystems yet: consensus lands at M3, networking at M5. \
         Use --check to validate boot."
    );
    node.shutdown();
    Ok(())
}
