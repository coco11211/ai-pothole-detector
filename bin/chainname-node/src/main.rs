//! CHAINNAME node binary.

use std::{net::SocketAddr, path::PathBuf};

use chainname_node::{DevNode, Network, Node, NodeConfig, dev_accounts, init_logging, mining_loop};
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

    /// Run a single-node development chain with JSON-RPC and mining.
    ///
    /// Pre-funds well-known development accounts and mines on a timer, so
    /// Foundry, MetaMask and any other Ethereum client can connect.
    #[arg(long)]
    dev: bool,

    /// Address the JSON-RPC server binds to in `--dev` mode.
    #[arg(long, default_value = "127.0.0.1:8545")]
    rpc_addr: SocketAddr,

    /// Milliseconds between blocks in `--dev` mode.
    #[arg(long, default_value_t = 1_000)]
    dev_block_time_ms: u64,
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

    if cli.dev {
        let params = node.config().params();
        node.shutdown();
        return run_dev_chain(params, cli.rpc_addr, cli.dev_block_time_ms);
    }

    // Peer-to-peer networking is not yet wired into the binary; the sync state
    // machine is exercised by the deterministic harness instead
    // (OPEN-PROBLEMS.md P-012). `--dev` runs a complete single-node chain.
    tracing::warn!("no peer-to-peer transport yet. Use --dev for a single-node chain.");
    node.shutdown();
    Ok(())
}

/// Runs a single-node development chain: RPC plus mining, until interrupted.
fn run_dev_chain(
    params: chainname_primitives::ChainParams,
    rpc_addr: SocketAddr,
    block_time_ms: u64,
) -> eyre::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    runtime.block_on(async move {
        // A fixed miner address, so dev block rewards and fees always land in
        // the same place and are easy to inspect.
        let miner = alloy_primitives::Address::repeat_byte(0xcc);
        let node = DevNode::new(params.clone(), miner);
        let handle = node.serve_rpc(rpc_addr).await?;

        tracing::info!(url = %handle.http_url(), chain_id = params.chain_id, "dev chain ready");
        tracing::warn!(
            "development accounts below are funded with PUBLIC keys. \
             Never use them for anything of value."
        );
        for account in dev_accounts() {
            tracing::info!(address = %account.address, key = %account.private_key, "dev account");
        }

        let (tx, rx) = tokio::sync::watch::channel(false);
        let mining = tokio::spawn(mining_loop(node, block_time_ms, rx));

        tokio::signal::ctrl_c().await?;
        tracing::info!("shutting down");
        let _ = tx.send(true);
        mining.abort();
        handle.stop();
        Ok::<(), eyre::Report>(())
    })
}
