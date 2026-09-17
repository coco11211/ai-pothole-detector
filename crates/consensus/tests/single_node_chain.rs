//! M3: a single node produces and validates its own chain.
//!
//! Real proof of work is performed here — no mocking of the hash — at a target
//! easy enough that the test runs in a debug build.

use alloy_primitives::{Address, B256, U256};
use chainname_consensus::validate::ValidationContext;
use chainname_consensus::{
    GenesisConfig, MineOutcome, Miner, ValidationError, genesis_header, validate_header,
};
use chainname_difficulty::{AsertParams, CompactTarget};
use chainname_pow::{DoubleKeccak256, PowHash};
use chainname_primitives::{HEADER_VERSION, Header};

/// Genesis timestamp for the test chain.
const GENESIS_MS: u64 = 1_700_000_000_000;
/// Target block interval, matching the staged 1 bps network.
const BLOCK_INTERVAL_MS: u64 = 1_000;
/// Blocks to mine. Enough to exercise retargeting across many heights while
/// staying fast in a debug build.
const CHAIN_LENGTH: u64 = 200;
/// Nonce budget per block. Comfortably above the expected ~256 attempts at the
/// test target, so an unlucky block does not flake the test.
const MAX_ATTEMPTS: u64 = 1_000_000;

/// An easy target: roughly 256 hashes per block in expectation.
fn test_target() -> U256 {
    U256::MAX >> 8
}

fn asert_params() -> AsertParams {
    AsertParams::new(
        (BLOCK_INTERVAL_MS / 1_000) as i128,
        2 * 60 * 60,
        // Equal to the anchor, so an on-schedule chain keeps a constant target
        // and the test isolates validation from retargeting dynamics (which
        // have their own dedicated simulation in retarget_simulation.rs).
        test_target(),
    )
}

fn genesis_config() -> GenesisConfig {
    GenesisConfig {
        timestamp_ms: GENESIS_MS,
        bits: CompactTarget::from_target(test_target()),
        state_root: B256::repeat_byte(0x11),
    }
}

fn context(height: u64, parent_timestamp_ms: u64, now_ms: u64) -> ValidationContext {
    ValidationContext {
        height,
        anchor_timestamp_ms: GENESIS_MS,
        anchor_target: genesis_config().bits.to_target().unwrap(),
        parent_timestamp_ms,
        now_ms,
        asert: asert_params(),
    }
}

/// Mines one block on top of `parent` at the given height.
fn mine_block(
    miner: &Miner<DoubleKeccak256>,
    parent: &Header,
    height: u64,
) -> (Header, ValidationContext) {
    let timestamp_ms = GENESIS_MS + height * BLOCK_INTERVAL_MS;
    // Generous `now`, so the future-drift rule never fires in these tests; it
    // has its own dedicated test below.
    let ctx = context(height, parent.timestamp_ms, timestamp_ms + 1_000);
    let bits = chainname_consensus::validate::expected_bits(&ctx, timestamp_ms);

    let template = Header {
        version: HEADER_VERSION,
        parents: vec![parent.hash()],
        timestamp_ms,
        bits: bits.to_u32(),
        nonce: 0,
        miner: Address::repeat_byte(0x42),
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: B256::repeat_byte(0x11),
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    };

    match miner.mine(template, 0, MAX_ATTEMPTS).unwrap() {
        MineOutcome::Found { header, .. } => (header, ctx),
        MineOutcome::Exhausted { attempts } => {
            panic!("no solution in {attempts} attempts at height {height}")
        }
    }
}

/// Mines a chain and returns every block with the context it was validated in.
fn mine_chain(length: u64) -> Vec<(Header, ValidationContext)> {
    let miner = Miner::new(DoubleKeccak256);
    let mut parent = genesis_header(&genesis_config());
    let mut chain = Vec::new();

    for height in 1..=length {
        let (header, ctx) = mine_block(&miner, &parent, height);
        parent = header.clone();
        chain.push((header, ctx));
    }
    chain
}

#[test]
fn single_node_mines_and_validates_its_own_chain() {
    let chain = mine_chain(CHAIN_LENGTH);
    assert_eq!(chain.len() as u64, CHAIN_LENGTH);

    let pow = DoubleKeccak256;
    for (header, ctx) in &chain {
        validate_header(header, ctx, &pow)
            .unwrap_or_else(|e| panic!("block at height {} failed validation: {e}", ctx.height));
    }
}

#[test]
fn every_block_links_to_its_parent() {
    let chain = mine_chain(20);
    let genesis = genesis_header(&genesis_config());

    assert_eq!(chain[0].0.parents, vec![genesis.hash()]);
    for pair in chain.windows(2) {
        assert_eq!(pair[1].0.parents, vec![pair[0].0.hash()], "chain link broken");
    }
}

#[test]
fn mined_blocks_actually_meet_their_target() {
    let chain = mine_chain(20);
    let pow = DoubleKeccak256;
    for (header, _) in &chain {
        let target = CompactTarget(header.bits).to_target().unwrap();
        let hash = U256::from_be_bytes(pow.hash_header(header).0);
        assert!(hash <= target, "block {} does not meet its target", header.hash());
    }
}

#[test]
fn tampering_with_a_sealed_block_invalidates_it() {
    let chain = mine_chain(5);
    let pow = DoubleKeccak256;
    let (header, ctx) = &chain[4];

    // Changing the miner address changes the PoW preimage, so the existing
    // nonce no longer solves it. This is the property that makes the coinbase
    // unforgeable.
    let mut tampered = header.clone();
    tampered.miner = Address::repeat_byte(0x99);
    assert!(matches!(
        validate_header(&tampered, ctx, &pow),
        Err(ValidationError::InsufficientWork { .. })
    ));
}

#[test]
fn a_block_claiming_the_wrong_difficulty_is_rejected() {
    let chain = mine_chain(3);
    let pow = DoubleKeccak256;
    let (header, ctx) = &chain[2];

    let mut wrong = header.clone();
    // An easier target than the rule allows.
    wrong.bits = CompactTarget::from_target(test_target()).to_u32().wrapping_add(1);
    assert!(matches!(
        validate_header(&wrong, ctx, &pow),
        Err(ValidationError::WrongDifficulty { .. })
    ));
}

#[test]
fn a_block_from_the_far_future_is_rejected() {
    let chain = mine_chain(3);
    let pow = DoubleKeccak256;
    let (header, ctx) = &chain[2];

    let mut ctx = ctx.clone();
    // Pretend the node's clock is well behind the block's timestamp.
    ctx.now_ms = header.timestamp_ms - 10 * 60 * 1_000;
    assert!(matches!(
        validate_header(header, &ctx, &pow),
        Err(ValidationError::TimestampTooFarInFuture { .. })
    ));
}

#[test]
fn a_block_moving_time_backwards_is_rejected() {
    let chain = mine_chain(3);
    let pow = DoubleKeccak256;
    let (header, ctx) = &chain[2];

    let mut ctx = ctx.clone();
    ctx.parent_timestamp_ms = header.timestamp_ms + 1;
    assert!(matches!(
        validate_header(header, &ctx, &pow),
        Err(ValidationError::TimestampWentBackwards { .. })
    ));
}

#[test]
fn a_block_with_no_parents_is_rejected() {
    let chain = mine_chain(3);
    let pow = DoubleKeccak256;
    let (header, ctx) = &chain[2];

    let mut orphan = header.clone();
    orphan.parents.clear();
    assert!(matches!(validate_header(&orphan, ctx, &pow), Err(ValidationError::Structure(_))));
}

#[test]
fn mining_is_reproducible() {
    // Same template, same nonce search, same block. Without this, nothing
    // downstream can be tested deterministically.
    let a = mine_chain(5);
    let b = mine_chain(5);
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.0.hash(), y.0.hash());
    }
}

#[test]
fn exhausting_the_nonce_budget_is_reported_not_faked() {
    // An impossible target within a tiny budget must report exhaustion rather
    // than returning a block that does not meet it.
    let miner = Miner::new(DoubleKeccak256);
    let mut template = genesis_header(&genesis_config());
    template.parents = vec![B256::repeat_byte(1)];
    template.bits = CompactTarget::from_target(U256::from(1u8)).to_u32();

    assert!(matches!(
        miner.mine(template, 0, 32).unwrap(),
        MineOutcome::Exhausted { attempts: 32 }
    ));
}
