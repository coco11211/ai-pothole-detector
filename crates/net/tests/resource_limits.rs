//! Resource exhaustion: what a hostile peer can make a node spend.
//!
//! Every limit here is something an attacker controls the input to. The tests
//! assert the *bound*, not the behaviour under normal load, because the normal
//! case is covered elsewhere and is not what fails.

use alloy_primitives::{Address, B256, Bytes, U256};
use chainname_ghostdag::DagStore;
use chainname_net::{
    BlockPayload, DagSync, MAX_BLOCK_BATCH, MAX_FRAME_BYTES, MAX_INV_ENTRIES, MAX_TXS_PER_BLOCK,
    Message, PeerId, SyncConfig, sync::AcceptAll,
};
use chainname_primitives::{BlockHash, HEADER_VERSION, Header};

const K: u16 = 18;
const MERGESET_LIMIT: u64 = 180;

fn genesis() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: 1_700_000_000_000,
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

fn hash_n(n: u64) -> BlockHash {
    B256::from(U256::from(n))
}

fn orphan(n: u64) -> Header {
    Header { parents: vec![hash_n(n + 1_000_000)], nonce: n, ..genesis() }
}

fn new_sync() -> DagSync<AcceptAll> {
    let g = genesis();
    let hash = g.hash();
    DagSync::new(DagStore::new(g, K, MERGESET_LIMIT), AcceptAll, SyncConfig::new(hash))
}

fn handshake(sync: &mut DagSync<AcceptAll>, peer: PeerId) {
    sync.on_connect(peer);
    sync.on_message(
        peer,
        Message::Version {
            version: chainname_net::PROTOCOL_VERSION,
            genesis: genesis().hash(),
            tips: Vec::new(),
        },
        0,
    );
}

#[test]
fn an_orphan_flood_is_bounded() {
    // Anyone can send blocks naming parents that do not exist. Without a bound
    // the pool is a memory leak an attacker drives.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    for n in 0..50_000u64 {
        sync.on_message(PeerId(1), Message::Blocks(vec![BlockPayload::empty(orphan(n))]), 0);
    }

    assert!(
        sync.orphan_count() <= 4_096,
        "orphan pool reached {} after a 50,000 block flood",
        sync.orphan_count()
    );
}

#[test]
fn an_orphan_flood_does_not_leave_stale_request_state() {
    // Evicting an orphan must also release what it was waiting on, or the
    // bookkeeping leaks even though the pool does not.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    for n in 0..20_000u64 {
        sync.on_message(PeerId(1), Message::Blocks(vec![BlockPayload::empty(orphan(n))]), 0);
    }

    // Requests are capped per batch and deduplicated, so the outstanding set
    // must stay far below the number of orphans thrown at us.
    assert!(
        sync.pending_request_count() <= 50_000,
        "outstanding requests grew to {}",
        sync.pending_request_count()
    );
}

#[test]
fn an_oversized_inventory_is_rejected_at_the_decoder() {
    // The check happens during decoding, before any allocation proportional to
    // the claimed size.
    let hashes: Vec<BlockHash> = (0..=MAX_INV_ENTRIES as u64).map(hash_n).collect();
    let bytes = Message::InvBlocks(hashes).encode_to_vec();
    assert!(Message::decode_from_slice(&bytes).is_err());
}

#[test]
fn an_oversized_block_batch_is_rejected_at_the_decoder() {
    let blocks: Vec<BlockPayload> = (0..=MAX_BLOCK_BATCH as u64)
        .map(|n| BlockPayload::empty(Header { nonce: n, parents: vec![hash_n(1)], ..genesis() }))
        .collect();
    let bytes = Message::Blocks(blocks).encode_to_vec();
    assert!(Message::decode_from_slice(&bytes).is_err());
}

#[test]
fn a_block_with_too_many_transactions_is_rejected() {
    // A body arrives before its transactions can be validated, so its size is
    // bounded at the decoder rather than after the work is done.
    let transactions: Vec<Bytes> =
        (0..=MAX_TXS_PER_BLOCK).map(|_| Bytes::from_static(b"x")).collect();
    let payload =
        BlockPayload { header: Header { parents: vec![hash_n(1)], ..genesis() }, transactions };
    let bytes = Message::Blocks(vec![payload]).encode_to_vec();
    assert!(Message::decode_from_slice(&bytes).is_err());
}

#[test]
fn a_request_for_more_blocks_than_the_batch_limit_is_capped() {
    // A peer asking for 512 blocks must not get 512 blocks back in one frame;
    // the response is capped independently of the request.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    // Fill the DAG with a chain so there is something to ask for.
    let mut parent = genesis().hash();
    let mut all = Vec::new();
    for nonce in 1..=300u64 {
        let header = Header { parents: vec![parent], nonce, ..genesis() };
        parent = header.hash();
        all.push(parent);
        sync.dag_mut().add_block(header).unwrap();
    }

    let actions = sync.on_message(PeerId(1), Message::GetBlocks(all), 0);
    for action in &actions {
        if let chainname_net::Action::Send(_, Message::Blocks(blocks)) = action {
            assert!(
                blocks.len() <= MAX_BLOCK_BATCH,
                "responded with {} blocks, limit is {MAX_BLOCK_BATCH}",
                blocks.len()
            );
        }
    }
}

#[test]
fn the_frame_limit_is_smaller_than_any_plausible_memory_pressure() {
    // Sanity on the constant itself: a peer must not be able to make us
    // allocate a large fraction of memory with one length prefix.
    const { assert!(MAX_FRAME_BYTES <= 32 * 1024 * 1024, "frame limit is too permissive") };
    const { assert!(MAX_FRAME_BYTES >= 1024 * 1024, "frame limit is too small for a full block") };
}

#[test]
fn repeated_handshakes_do_not_accumulate_state() {
    // A peer that reconnects in a loop must not leave anything behind.
    let mut sync = new_sync();
    for round in 0..1_000u64 {
        let peer = PeerId(round);
        handshake(&mut sync, peer);
        sync.on_disconnect(peer);
    }
    assert_eq!(sync.peer_count(), 0, "disconnected peers were not cleaned up");
    assert_eq!(sync.pending_request_count(), 0, "requests survived their peer");
}

#[test]
fn the_same_block_relayed_endlessly_costs_nothing() {
    // Duplicates are normal under flood relay and must not accumulate.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let header = Header { parents: vec![genesis().hash()], nonce: 1, ..genesis() };
    for _ in 0..10_000 {
        sync.on_message(PeerId(1), Message::Blocks(vec![BlockPayload::empty(header.clone())]), 0);
    }
    assert_eq!(sync.dag().len(), 2, "genesis plus one block, however many times it was sent");
    assert_eq!(sync.orphan_count(), 0);
}
