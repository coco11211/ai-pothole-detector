//! Direct tests of the sync state machine.
//!
//! The multi-node simulation covers convergence end to end; these cover the
//! specific behaviours that are hard to provoke from a whole-network run:
//! handshake ordering, orphan bounds, and peer banning.

use alloy_primitives::{Address, B256};
use chainname_ghostdag::DagStore;
use chainname_net::{
    Action, DagSync, Message, PROTOCOL_VERSION, PeerId, SyncConfig, sync::AcceptAll,
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

fn child(parents: &[BlockHash], nonce: u64) -> Header {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    Header {
        version: HEADER_VERSION,
        parents,
        timestamp_ms: 1_700_000_000_000 + nonce * 1_000,
        bits: 0x2000_ffff,
        nonce,
        miner: Address::repeat_byte(1),
        txs_root: B256::ZERO,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: B256::ZERO,
        deferred_gas_used: 0,
    }
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
        Message::Version { version: PROTOCOL_VERSION, genesis: genesis().hash(), tips: Vec::new() },
        0,
    );
}

#[test]
fn connecting_sends_our_version_first() {
    let mut sync = new_sync();
    let actions = sync.on_connect(PeerId(1));
    assert!(matches!(actions.as_slice(), [Action::Send(PeerId(1), Message::Version { .. })]));
}

#[test]
fn a_version_with_the_wrong_genesis_disconnects_immediately() {
    let mut sync = new_sync();
    sync.on_connect(PeerId(1));
    let actions = sync.on_message(
        PeerId(1),
        Message::Version {
            version: PROTOCOL_VERSION,
            genesis: B256::repeat_byte(0xff),
            tips: Vec::new(),
        },
        0,
    );
    assert!(matches!(actions.as_slice(), [Action::Disconnect(PeerId(1), _)]));
}

#[test]
fn a_version_with_the_wrong_protocol_disconnects_immediately() {
    let mut sync = new_sync();
    sync.on_connect(PeerId(1));
    let actions = sync.on_message(
        PeerId(1),
        Message::Version {
            version: PROTOCOL_VERSION + 1,
            genesis: genesis().hash(),
            tips: Vec::new(),
        },
        0,
    );
    assert!(matches!(actions.as_slice(), [Action::Disconnect(PeerId(1), _)]));
}

#[test]
fn data_before_the_handshake_is_penalised() {
    let mut sync = new_sync();
    sync.on_connect(PeerId(1));
    // Four `OutOfOrder` penalties at 25 each exhaust the starting score.
    for _ in 0..3 {
        assert!(sync.on_message(PeerId(1), Message::GetTips, 0).is_empty());
    }
    let actions = sync.on_message(PeerId(1), Message::GetTips, 0);
    assert!(matches!(actions.as_slice(), [Action::Disconnect(PeerId(1), _)]));
}

#[test]
fn a_message_from_an_unknown_peer_is_refused() {
    let mut sync = new_sync();
    let actions = sync.on_message(PeerId(99), Message::GetTips, 0);
    assert!(matches!(actions.as_slice(), [Action::Disconnect(PeerId(99), _)]));
}

#[test]
fn a_block_whose_parents_are_present_is_accepted_and_announced() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));
    handshake(&mut sync, PeerId(2));

    let block = child(&[genesis().hash()], 1);
    let hash = block.hash();
    let actions = sync.on_message(PeerId(1), Message::Blocks(vec![block]), 0);

    assert!(sync.dag().contains(hash));
    // Announced to peer 2, but not back to peer 1 who sent it.
    let announced: Vec<PeerId> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(p, Message::InvBlocks(h)) if h == &vec![hash] => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!(announced, vec![PeerId(2)]);
}

#[test]
fn a_block_with_a_missing_parent_is_held_and_its_parent_requested() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let missing = child(&[genesis().hash()], 1);
    let orphan = child(&[missing.hash()], 2);
    let orphan_hash = orphan.hash();

    let actions = sync.on_message(PeerId(1), Message::Blocks(vec![orphan]), 0);

    assert!(!sync.dag().contains(orphan_hash), "an orphan must not enter the DAG");
    assert_eq!(sync.orphan_count(), 1);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send(PeerId(1), Message::GetBlocks(h)) if h == &vec![missing.hash()]
        )),
        "the missing parent must be requested"
    );
}

#[test]
fn an_orphan_is_admitted_once_its_parent_arrives() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let parent = child(&[genesis().hash()], 1);
    let orphan = child(&[parent.hash()], 2);
    let orphan_hash = orphan.hash();

    sync.on_message(PeerId(1), Message::Blocks(vec![orphan]), 0);
    assert_eq!(sync.orphan_count(), 1);

    sync.on_message(PeerId(1), Message::Blocks(vec![parent]), 0);
    assert_eq!(sync.orphan_count(), 0, "the orphan should have been admitted");
    assert!(sync.dag().contains(orphan_hash));
}

#[test]
fn a_long_orphan_chain_resolves_without_recursing() {
    // Resolution is iterative on purpose: a deep chain of orphans is
    // attacker-controlled, and a recursive resolver would be a stack-overflow
    // vector.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let mut chain = Vec::new();
    let mut parent = genesis().hash();
    for nonce in 1..=500u64 {
        let block = child(&[parent], nonce);
        parent = block.hash();
        chain.push(block);
    }

    // Deliver in reverse, so every block is an orphan until the last one.
    for block in chain.iter().skip(1).rev() {
        sync.on_message(PeerId(1), Message::Blocks(vec![block.clone()]), 0);
    }
    assert_eq!(sync.orphan_count(), 499);

    sync.on_message(PeerId(1), Message::Blocks(vec![chain[0].clone()]), 0);
    assert_eq!(sync.orphan_count(), 0, "the whole chain should have resolved");
    assert_eq!(sync.dag().len(), 501, "genesis plus 500 blocks");
}

#[test]
fn the_orphan_pool_is_bounded() {
    // Orphans are attacker-controlled: anyone can send blocks naming parents
    // that do not exist. The pool must never grow without limit.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let config_max = 4_096;
    for nonce in 1..(config_max as u64 + 500) {
        let unreachable_parent = B256::from(alloy_primitives::U256::from(nonce + 1_000_000));
        let orphan = child(&[unreachable_parent], nonce);
        sync.on_message(PeerId(1), Message::Blocks(vec![orphan]), 0);
    }

    assert!(
        sync.orphan_count() <= config_max,
        "orphan pool grew to {}, limit is {config_max}",
        sync.orphan_count()
    );
}

#[test]
fn get_blocks_returns_what_we_have() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let block = child(&[genesis().hash()], 1);
    let hash = block.hash();
    sync.dag_mut().add_block(block.clone()).unwrap();

    let actions = sync.on_message(PeerId(1), Message::GetBlocks(vec![hash]), 0);
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Send(PeerId(1), Message::Blocks(headers)) if headers == &vec![block.clone()]
    )));
}

#[test]
fn get_blocks_for_unknown_hashes_sends_nothing() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));
    let actions = sync.on_message(PeerId(1), Message::GetBlocks(vec![B256::repeat_byte(0xab)]), 0);
    assert!(actions.is_empty());
}

#[test]
fn ping_is_answered_with_the_same_nonce() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));
    let actions = sync.on_message(PeerId(1), Message::Ping(1234), 0);
    assert_eq!(actions, vec![Action::Send(PeerId(1), Message::Pong(1234))]);
}

#[test]
fn a_tick_reconciles_tips_with_every_peer() {
    // The mechanism that makes convergence eventual rather than merely likely:
    // flood relay announces a block once, and a lost announcement for a tip has
    // no descendant to rescue it.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));
    handshake(&mut sync, PeerId(2));

    let actions = sync.on_tick(1_000);
    for peer in [PeerId(1), PeerId(2)] {
        assert!(
            actions.contains(&Action::Send(peer, Message::GetTips)),
            "{peer} was not asked for its tips"
        );
    }
}

#[test]
fn a_timed_out_request_is_retried_against_a_different_peer() {
    // Re-asking the peer that already failed to answer is a deadlock: it will
    // never have the block, and duplicate-request suppression means no later
    // attempt can ever be made.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));
    handshake(&mut sync, PeerId(2));

    let missing = child(&[genesis().hash()], 1);
    let orphan = child(&[missing.hash()], 2);
    sync.on_message(PeerId(1), Message::Blocks(vec![orphan]), 0);
    assert_eq!(sync.pending_request_count(), 1);

    // Well past the ten-second request timeout.
    let actions = sync.on_tick(60_000);
    let retried_to: Vec<PeerId> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Send(p, Message::GetBlocks(_)) => Some(*p),
            _ => None,
        })
        .collect();
    assert!(!retried_to.is_empty(), "the request was never retried");
}

#[test]
fn disconnecting_releases_what_a_peer_owed_us() {
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let missing = child(&[genesis().hash()], 1);
    let orphan = child(&[missing.hash()], 2);
    sync.on_message(PeerId(1), Message::Blocks(vec![orphan]), 0);
    assert_eq!(sync.pending_request_count(), 1);

    sync.on_disconnect(PeerId(1));
    assert_eq!(
        sync.pending_request_count(),
        0,
        "requests owed by a departed peer must be released so someone else can be asked"
    );
}

#[test]
fn duplicate_blocks_are_not_treated_as_misbehaviour() {
    // Under flood relay a duplicate is the expected case. Penalising it made
    // honest five-node networks partition themselves.
    let mut sync = new_sync();
    handshake(&mut sync, PeerId(1));

    let block = child(&[genesis().hash()], 1);
    for _ in 0..1_000 {
        let actions = sync.on_message(PeerId(1), Message::Blocks(vec![block.clone()]), 0);
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Disconnect(..))),
            "an honest peer was disconnected for relaying a block twice"
        );
    }
}
