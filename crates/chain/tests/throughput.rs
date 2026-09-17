//! Measures sequential execution throughput.
//!
//! The figure that decides whether a block rate is honest: a chain must not
//! advertise more gas per second than it can actually execute.

use alloy_consensus::{Signed, TxEip1559, TxEnvelope, transaction::Recovered};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, B256, Signature, TxKind, U256};
use chainname_chain::{BodyStore, ChainExecutor, ChainTx, compute_reorg};
use chainname_execution::load_genesis;
use chainname_ghostdag::DagStore;
use chainname_primitives::{BlockHash, ChainParams, HEADER_VERSION, Header};

const GENESIS_MS: u64 = 1_700_000_000_000;
const FUNDING: u128 = 1_000_000_000_000_000_000_000_000;

fn genesis_header() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: GENESIS_MS,
        bits: 0x2000_ffff,
        nonce: 0,
        miner: Address::ZERO,
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    }
}

fn account(n: u32) -> Address {
    Address::from_slice(&alloy_primitives::keccak256(n.to_be_bytes())[..20])
}

fn transfer(from: u32, to: u32, nonce: u64) -> ChainTx {
    let tx = TxEip1559 {
        chain_id: ChainParams::testnet_1bps().chain_id,
        nonce,
        gas_limit: 100_000,
        max_fee_per_gas: 100_000_000_000,
        max_priority_fee_per_gas: 1,
        to: TxKind::Call(account(to)),
        value: U256::from(1u64),
        access_list: Default::default(),
        input: Default::default(),
    };
    let signature = Signature::new(U256::from(nonce + 1), U256::from(u64::from(from) + 1), false);
    let envelope = TxEnvelope::Eip1559(Signed::new_unhashed(tx, signature));
    Recovered::new_unchecked(envelope, account(from))
}

/// Runs `blocks` chain blocks of `per_block` transfers and reports gas/second.
fn measure(blocks: u64, per_block: usize) -> (u64, f64) {
    let params = ChainParams::testnet_1bps();
    let g = genesis_header();
    let genesis_hash = g.hash();

    let mut alloc = Genesis::default();
    let senders = u32::try_from(per_block).expect("benchmark size fits") + 1;
    for n in 0..=senders {
        alloc.alloc.insert(
            account(n),
            GenesisAccount { balance: U256::from(FUNDING), ..Default::default() },
        );
    }

    let mut dag = DagStore::new(g, 18, 10_000);
    let mut bodies = BodyStore::new();
    let mut executor = ChainExecutor::new(params, load_genesis(&alloc).unwrap(), genesis_hash);

    let mut parent = genesis_hash;
    let mut chain: Vec<BlockHash> = Vec::new();
    for height in 1..=blocks {
        let header = Header {
            parents: vec![parent],
            nonce: height,
            timestamp_ms: GENESIS_MS + height * 1_000,
            ..genesis_header()
        };
        let hash = dag.add_block(header).unwrap();
        parent = hash;
        chain.push(hash);

        let txs: Vec<ChainTx> =
            (0..senders - 1).map(|i| transfer(i, senders, height - 1)).collect();
        bodies.insert(hash, txs);
    }

    let reorg = compute_reorg(&dag, genesis_hash, parent);
    let start = std::time::Instant::now();
    let outcomes =
        executor.apply_reorg(&dag, &bodies, &reorg.removed, &reorg.added).expect("executes");
    let elapsed = start.elapsed();

    let total_gas: u64 = outcomes.iter().map(|o| o.gas_used).sum();
    let executed: usize = outcomes.iter().map(|o| o.executed).sum();
    assert!(executed > 0, "nothing executed; the measurement is meaningless");

    #[allow(clippy::float_arithmetic, reason = "a benchmark report, not consensus")]
    let gas_per_second = total_gas as f64 / elapsed.as_secs_f64();
    (total_gas, gas_per_second)
}

#[test]
fn report_sequential_execution_throughput() {
    // Not an assertion about a target: a measurement, printed, so the gas
    // limit can be chosen from evidence instead of optimism.
    for (blocks, per_block) in [(50u64, 20usize), (50, 100), (20, 400)] {
        let (gas, rate) = measure(blocks, per_block);
        eprintln!(
            "{blocks:>3} blocks x {per_block:>3} transfers: total_gas={gas:>10} \
             throughput={:>12.0} gas/s",
            rate
        );
    }
}

#[test]
fn execution_keeps_up_with_the_one_bps_gas_limit() {
    // The claim the 1 bps configuration makes: 30,000,000 gas per second. If
    // sequential execution cannot sustain that, the chain is advertising
    // capacity it does not have.
    let (_, rate) = measure(40, 200);
    let target = ChainParams::testnet_1bps().target_gas_per_second();
    #[allow(clippy::float_arithmetic, reason = "a benchmark comparison, not consensus")]
    let ratio = rate / target as f64;
    eprintln!("measured {rate:.0} gas/s against a {target} gas/s target ({ratio:.1}x)");
    assert!(
        rate > target as f64,
        "sequential execution sustains only {rate:.0} gas/s but the chain \
         advertises {target} gas/s"
    );
}
