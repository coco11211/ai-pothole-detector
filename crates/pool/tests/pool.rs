//! Transaction pool behaviour.

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope, transaction::Recovered};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use chainname_pool::{PoolConfig, PoolError, TxPool};

const CHAIN_ID: u64 = 7717;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const BASE_FEE: u64 = 1_000_000_000;
/// Plenty to cover any transaction these tests build.
const RICH: u128 = 1_000_000_000_000_000_000_000;

fn signer(index: u64) -> PrivateKeySigner {
    PrivateKeySigner::from_bytes(&alloy_primitives::keccak256(index.to_be_bytes())).unwrap()
}

fn config() -> PoolConfig {
    PoolConfig::new(CHAIN_ID, BLOCK_GAS_LIMIT)
}

/// Builds a signed transaction and its 2718 encoding.
fn tx(
    signer_index: u64,
    nonce: u64,
    tip: u128,
    gas_limit: u64,
    chain_id: u64,
) -> (Recovered<TxEnvelope>, Bytes) {
    let wallet = signer(signer_index);
    let inner = TxEip1559 {
        chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas: u128::from(BASE_FEE) + tip,
        max_priority_fee_per_gas: tip,
        to: TxKind::Call(Address::repeat_byte(0xbb)),
        value: U256::ZERO,
        access_list: Default::default(),
        input: Default::default(),
    };
    let signature = wallet.sign_hash_sync(&inner.signature_hash()).unwrap();
    let envelope = TxEnvelope::Eip1559(inner.into_signed(signature));
    let encoded = Bytes::from(envelope.encoded_2718());
    (Recovered::new_unchecked(envelope, wallet.address()), encoded)
}

fn simple(signer_index: u64, nonce: u64, tip: u128) -> (Recovered<TxEnvelope>, Bytes) {
    tx(signer_index, nonce, tip, 100_000, CHAIN_ID)
}

fn add(pool: &mut TxPool, t: (Recovered<TxEnvelope>, Bytes)) -> Result<B256, PoolError> {
    pool.add(t.0, t.1, BASE_FEE, 0, U256::from(RICH))
}

#[test]
fn a_valid_transaction_is_admitted() {
    let mut pool = TxPool::new(config());
    let hash = add(&mut pool, simple(1, 0, 10)).unwrap();
    assert_eq!(pool.len(), 1);
    assert!(pool.contains(hash));
}

#[test]
fn the_same_transaction_twice_is_refused() {
    let mut pool = TxPool::new(config());
    let t = simple(1, 0, 10);
    add(&mut pool, t.clone()).unwrap();
    assert!(matches!(add(&mut pool, t), Err(PoolError::AlreadyKnown(_))));
}

#[test]
fn a_transaction_for_another_chain_is_refused() {
    let mut pool = TxPool::new(config());
    let t = tx(1, 0, 10, 100_000, CHAIN_ID + 1);
    assert!(matches!(add(&mut pool, t), Err(PoolError::WrongChainId { .. })));
}

#[test]
fn a_used_nonce_is_refused() {
    let mut pool = TxPool::new(config());
    let (recovered, encoded) = simple(1, 3, 10);
    // Account is already at nonce 5.
    let result = pool.add(recovered, encoded, BASE_FEE, 5, U256::from(RICH));
    assert!(matches!(result, Err(PoolError::NonceTooLow { found: 3, account: 5 })));
}

#[test]
fn a_sender_who_cannot_pay_is_refused() {
    let mut pool = TxPool::new(config());
    let (recovered, encoded) = simple(1, 0, 10);
    let result = pool.add(recovered, encoded, BASE_FEE, 0, U256::from(1u64));
    assert!(matches!(result, Err(PoolError::InsufficientFunds { .. })));
}

#[test]
fn a_transaction_too_large_for_any_block_is_refused() {
    let mut pool = TxPool::new(config());
    let t = tx(1, 0, 10, BLOCK_GAS_LIMIT + 1, CHAIN_ID);
    assert!(matches!(add(&mut pool, t), Err(PoolError::GasLimitTooHigh { .. })));
}

#[test]
fn a_replacement_must_pay_strictly_more() {
    let mut pool = TxPool::new(config());
    add(&mut pool, simple(1, 0, 10)).unwrap();

    // Equal fee: refused. Free replacement would let the pool be churned.
    assert!(matches!(
        add(&mut pool, simple(1, 0, 10)),
        Err(PoolError::AlreadyKnown(_) | PoolError::ReplacementUnderpriced { .. })
    ));
    assert!(matches!(
        add(&mut pool, simple(1, 0, 5)),
        Err(PoolError::ReplacementUnderpriced { .. })
    ));

    // Higher fee: accepted, and it displaces the old one.
    add(&mut pool, simple(1, 0, 20)).unwrap();
    assert_eq!(pool.len(), 1, "the replaced transaction must be gone");
}

#[test]
fn one_sender_cannot_fill_the_pool() {
    let mut pool = TxPool::new(config());
    for nonce in 0..64 {
        add(&mut pool, simple(1, nonce, 10)).unwrap();
    }
    assert!(matches!(add(&mut pool, simple(1, 64, 10)), Err(PoolError::TooManyFromSender { .. })));
}

#[test]
fn the_pool_evicts_by_fee_rate_when_full() {
    let mut pool = TxPool::new(PoolConfig { max_transactions: 4, ..config() });

    // Four senders, ascending fees.
    for (index, tip) in [(1u64, 40u128), (2, 30), (3, 20), (4, 10)] {
        add(&mut pool, simple(index, 0, tip)).unwrap();
    }
    assert_eq!(pool.len(), 4);

    // A better-paying transaction pushes the cheapest one out.
    add(&mut pool, simple(5, 0, 50)).unwrap();
    assert_eq!(pool.len(), 4);

    let best = pool.best_transactions(BASE_FEE, 10);
    let tips: Vec<u128> = best.iter().map(|t| t.effective_tip(BASE_FEE)).collect();
    assert_eq!(tips, vec![50, 40, 30, 20], "the 10-wei transaction should have been evicted");
}

#[test]
fn best_transactions_orders_by_effective_tip() {
    let mut pool = TxPool::new(config());
    add(&mut pool, simple(1, 0, 5)).unwrap();
    add(&mut pool, simple(2, 0, 50)).unwrap();
    add(&mut pool, simple(3, 0, 25)).unwrap();

    let tips: Vec<u128> =
        pool.best_transactions(BASE_FEE, 10).iter().map(|t| t.effective_tip(BASE_FEE)).collect();
    assert_eq!(tips, vec![50, 25, 5]);
}

#[test]
fn best_transactions_never_reorders_a_senders_nonces() {
    // A sender's nonce N+1 cannot execute before its nonce N, so offering it
    // first would be offering something unusable.
    let mut pool = TxPool::new(config());
    // Deliberately inverted fees: the later nonce pays far more.
    add(&mut pool, simple(1, 0, 1)).unwrap();
    add(&mut pool, simple(1, 1, 1_000)).unwrap();
    add(&mut pool, simple(2, 0, 500)).unwrap();

    let best = pool.best_transactions(BASE_FEE, 10);
    let from_sender_one: Vec<u64> =
        best.iter().filter(|t| t.sender() == signer(1).address()).map(|t| t.nonce()).collect();
    assert_eq!(from_sender_one, vec![0, 1], "nonce order was not preserved");
}

#[test]
fn effective_tip_is_capped_by_the_fee_ceiling() {
    // A transaction offering a huge priority fee but a max fee barely above
    // the base fee can only actually pay the difference.
    let wallet = signer(1);
    let inner = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: u128::from(BASE_FEE) + 3,
        max_priority_fee_per_gas: 1_000_000,
        to: TxKind::Call(Address::repeat_byte(0xbb)),
        value: U256::ZERO,
        access_list: Default::default(),
        input: Default::default(),
    };
    let signature = wallet.sign_hash_sync(&inner.signature_hash()).unwrap();
    let envelope = TxEnvelope::Eip1559(inner.into_signed(signature));
    let encoded = Bytes::from(envelope.encoded_2718());

    let mut pool = TxPool::new(config());
    let hash = pool
        .add(
            Recovered::new_unchecked(envelope, wallet.address()),
            encoded,
            BASE_FEE,
            0,
            U256::from(RICH),
        )
        .unwrap();

    assert_eq!(
        pool.get(hash).unwrap().effective_tip(BASE_FEE),
        3,
        "the miner can only earn what the fee ceiling leaves above the base fee"
    );
}

#[test]
fn pruning_drops_transactions_the_state_has_passed() {
    let mut pool = TxPool::new(config());
    add(&mut pool, simple(1, 0, 10)).unwrap();
    add(&mut pool, simple(1, 1, 10)).unwrap();
    add(&mut pool, simple(2, 0, 10)).unwrap();
    assert_eq!(pool.len(), 3);

    let sender_one = signer(1).address();
    pool.prune(|address| if address == sender_one { 1 } else { 0 });

    assert_eq!(pool.len(), 2, "only sender one's nonce 0 should have been dropped");
}

#[test]
fn removing_a_transaction_frees_its_sender_slot() {
    let mut pool = TxPool::new(config());
    let hash = add(&mut pool, simple(1, 0, 10)).unwrap();
    pool.remove(hash);
    assert!(pool.is_empty());
    // The same nonce can be used again once the old one is gone.
    add(&mut pool, simple(1, 0, 10)).unwrap();
    assert_eq!(pool.len(), 1);
}

#[test]
fn hashes_are_reported_in_a_stable_order() {
    let mut pool = TxPool::new(config());
    for index in 1..=5 {
        add(&mut pool, simple(index, 0, 10)).unwrap();
    }
    assert_eq!(pool.hashes(), pool.hashes());
    let mut sorted = pool.hashes();
    sorted.sort_unstable();
    assert_eq!(pool.hashes(), sorted);
}

#[test]
fn an_empty_pool_offers_nothing() {
    let pool = TxPool::new(config());
    assert!(pool.best_transactions(BASE_FEE, 10).is_empty());
}
