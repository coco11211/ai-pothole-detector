//! Real TCP, real sockets, real convergence.
//!
//! The deterministic harness proves the *logic* converges. This proves the
//! logic is actually reachable through a socket: framing, handshake,
//! backpressure, and disconnect handling. Between them the two cover what
//! neither does alone.
//!
//! Deliberately small and short. A long multi-node run belongs in the
//! deterministic simulation, where a failure reproduces; a wall-clock TCP test
//! that failed once in twenty runs would tell nobody anything.

use std::{collections::HashSet, time::Duration};

use alloy_primitives::{Address, B256, U256};
use chainname_ghostdag::DagStore;
use chainname_net::{
    BlockPayload, DagSync, P2pConfig, P2pNode, SyncConfig, announce_local_block, sync::AcceptAll,
};
use chainname_primitives::{BlockHash, HEADER_VERSION, Header};

const K: u16 = 18;
const MERGESET_LIMIT: u64 = 180;
const GENESIS_MS: u64 = 1_700_000_000_000;

fn genesis() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: GENESIS_MS,
        bits: 0x2000_ffff,
        nonce: 0,
        miner: Address::ZERO,
        txs_root: B256::ZERO,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: B256::ZERO,
        deferred_gas_used: 0,
    }
}

fn child(parents: &[BlockHash], nonce: u64, miner: u8) -> Header {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    parents.dedup();
    Header {
        parents,
        nonce,
        miner: Address::repeat_byte(miner),
        timestamp_ms: GENESIS_MS + nonce * 1_000,
        ..genesis()
    }
}

fn new_sync() -> DagSync<AcceptAll> {
    let g = genesis();
    let hash = g.hash();
    DagSync::new(DagStore::new(g, K, MERGESET_LIMIT), AcceptAll, SyncConfig::new(hash))
}

/// Waits for a condition, polling, up to a deadline.
///
/// Real networking is asynchronous; a fixed sleep is either flaky or slow.
async fn wait_for(label: &str, timeout: Duration, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for: {label}");
}

/// Every block hash a node holds.
async fn block_set(node: &P2pNode<AcceptAll>) -> HashSet<BlockHash> {
    let sync = node.sync();
    let guard = sync.lock().await;
    let dag = guard.dag();
    let mut seen = HashSet::new();
    let mut stack: Vec<BlockHash> = dag.tips();
    while let Some(hash) = stack.pop() {
        if !seen.insert(hash) {
            continue;
        }
        if let Some(header) = dag.header(hash) {
            stack.extend(header.parents.iter().copied());
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_nodes_connect_over_tcp() {
    let listener = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("listener starts");

    let mut dialer_config = P2pConfig::new("127.0.0.1:0".parse().unwrap());
    dialer_config.dial = vec![listener.local_addr()];
    let dialer = P2pNode::start(new_sync(), dialer_config).await.expect("dialer starts");

    wait_for("both nodes to register a peer", Duration::from_secs(10), async || {
        listener.peer_count().await > 0 && dialer.peer_count().await > 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_mined_on_one_node_reaches_the_other() {
    let listener = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("listener starts");

    let mut dialer_config = P2pConfig::new("127.0.0.1:0".parse().unwrap());
    dialer_config.dial = vec![listener.local_addr()];
    let dialer = P2pNode::start(new_sync(), dialer_config).await.expect("dialer starts");

    wait_for("handshake", Duration::from_secs(10), async || {
        listener.peer_count().await > 0 && dialer.peer_count().await > 0
    })
    .await;

    let genesis_hash = genesis().hash();
    let block = child(&[genesis_hash], 1, 0xaa);
    let hash = block.hash();
    announce_local_block(&dialer, BlockPayload::empty(block)).await;

    wait_for("the block to arrive at the listener", Duration::from_secs(15), async || {
        let sync = listener.sync();
        let guard = sync.lock().await;
        guard.dag().contains(hash)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_of_blocks_syncs_in_full() {
    // Blocks are announced in order but arrive asynchronously, so the receiver
    // exercises both the direct-accept and the orphan-resolution paths.
    let listener = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("listener starts");

    let mut dialer_config = P2pConfig::new("127.0.0.1:0".parse().unwrap());
    dialer_config.dial = vec![listener.local_addr()];
    let dialer = P2pNode::start(new_sync(), dialer_config).await.expect("dialer starts");

    wait_for("handshake", Duration::from_secs(10), async || {
        listener.peer_count().await > 0 && dialer.peer_count().await > 0
    })
    .await;

    const CHAIN: u64 = 25;
    let mut parent = genesis().hash();
    for nonce in 1..=CHAIN {
        let block = child(&[parent], nonce, 0xbb);
        parent = block.hash();
        announce_local_block(&dialer, BlockPayload::empty(block)).await;
    }

    wait_for("the listener to catch up", Duration::from_secs(30), async || {
        let sync = listener.sync();
        let guard = sync.lock().await;
        guard.dag().len() as u64 == CHAIN + 1
    })
    .await;

    assert_eq!(
        block_set(&listener).await,
        block_set(&dialer).await,
        "the two nodes hold different blocks"
    );

    let a = listener.sync();
    let b = dialer.sync();
    let tip_a = a.lock().await.dag().virtual_selected_parent();
    let tip_b = b.lock().await.dag().virtual_selected_parent();
    assert_eq!(tip_a, tip_b, "the two nodes selected different chain tips");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_nodes_relay_transitively() {
    // B connects to A, C connects to B. A block mined on A must reach C, which
    // has never spoken to A. That is flood relay actually relaying, rather
    // than two nodes exchanging directly.
    let a = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("a starts");

    let mut b_config = P2pConfig::new("127.0.0.1:0".parse().unwrap());
    b_config.dial = vec![a.local_addr()];
    let b = P2pNode::start(new_sync(), b_config).await.expect("b starts");

    let mut c_config = P2pConfig::new("127.0.0.1:0".parse().unwrap());
    c_config.dial = vec![b.local_addr()];
    let c = P2pNode::start(new_sync(), c_config).await.expect("c starts");

    wait_for("the line to form", Duration::from_secs(10), async || {
        a.peer_count().await > 0 && b.peer_count().await >= 2 && c.peer_count().await > 0
    })
    .await;

    let block = child(&[genesis().hash()], 42, 0xcc);
    let hash = block.hash();
    announce_local_block(&a, BlockPayload::empty(block)).await;

    wait_for("the block to reach C via B", Duration::from_secs(20), async || {
        let sync = c.sync();
        let guard = sync.lock().await;
        guard.dag().contains(hash)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_with_no_peers_still_runs() {
    // A node that never finds a peer must keep serving and keep ticking, not
    // wedge or spin.
    let alone = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("starts");
    assert_eq!(alone.peer_count().await, 0);

    let block = child(&[genesis().hash()], 7, 0xdd);
    let hash = block.hash();
    announce_local_block(&alone, BlockPayload::empty(block)).await;

    let sync = alone.sync();
    assert!(sync.lock().await.dag().contains(hash), "a solitary node still builds its own DAG");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(alone.peer_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn garbage_on_the_wire_does_not_crash_the_node() {
    // A peer that sends nonsense must be dropped, not take the node with it.
    use tokio::io::AsyncWriteExt;

    let victim = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("starts");

    let mut socket = tokio::net::TcpStream::connect(victim.local_addr()).await.expect("connects");
    // A well-framed message with a body that is not a message.
    let junk = b"this is not a chainname message at all";
    let mut frame = u32::try_from(junk.len()).expect("fits").to_be_bytes().to_vec();
    frame.extend_from_slice(junk);
    socket.write_all(&frame).await.expect("writes");
    let _ = socket.flush().await;

    wait_for("the bad peer to be dropped", Duration::from_secs(10), async || {
        victim.peer_count().await == 0
    })
    .await;

    // Still alive and still serving.
    let block = child(&[genesis().hash()], 3, 0xee);
    let hash = block.hash();
    announce_local_block(&victim, BlockPayload::empty(block)).await;
    let sync = victim.sync();
    assert!(sync.lock().await.dag().contains(hash), "the node survived the garbage");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversized_frame_is_refused() {
    use tokio::io::AsyncWriteExt;

    let victim = P2pNode::start(new_sync(), P2pConfig::new("127.0.0.1:0".parse().unwrap()))
        .await
        .expect("starts");

    let mut socket = tokio::net::TcpStream::connect(victim.local_addr()).await.expect("connects");
    // Four bytes claiming four gigabytes. The node must refuse before
    // allocating anything.
    socket.write_all(&u32::MAX.to_be_bytes()).await.expect("writes");
    let _ = socket.flush().await;

    wait_for("the peer to be dropped", Duration::from_secs(10), async || {
        victim.peer_count().await == 0
    })
    .await;

    let _ = U256::ZERO;
}
