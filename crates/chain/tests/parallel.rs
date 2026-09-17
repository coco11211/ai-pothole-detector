//! M10: parallel execution must equal sequential execution, exactly.
//!
//! Not "usually", not "equivalently": the same state root, the same receipts,
//! the same gas. Two nodes that schedule differently and reach different state
//! is a consensus split, so every test here compares the two paths directly on
//! the same input rather than asserting properties of the parallel one alone.

use alloy_consensus::{Signed, TxEip1559, TxEnvelope, transaction::Recovered};
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use chainname_chain::{BodyStore, ChainExecutor, ChainTx, compute_reorg};
use chainname_execution::load_genesis;
use chainname_ghostdag::DagStore;
use chainname_primitives::{BlockHash, ChainParams, HEADER_VERSION, Header};

const K: u16 = 18;
const MERGESET_LIMIT: u64 = 10_000;
const GENESIS_MS: u64 = 1_700_000_000_000;
const FUNDING: u128 = 1_000_000_000_000_000_000_000_000;

/// ERC-20 runtime, compiled by solc 0.8.30. Used so the parallel path is
/// tested against real contract execution, not only value transfers.
fn erc20_initcode(supply: U256) -> Bytes {
    use alloy_sol_types::SolValue;
    let hex = include_str!("../../execution/testdata/Erc20.bin").trim();
    let mut code = alloy_primitives::hex::decode(hex).expect("valid hex");
    code.extend_from_slice(&supply.abi_encode());
    Bytes::from(code)
}

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

fn miner(n: u8) -> Address {
    Address::repeat_byte(0xa0 + n)
}

/// Builds a transaction with its sender attached.
fn tx(from: u32, nonce: u64, kind: TxKind, data: Bytes, value: u64, tip: u128) -> ChainTx {
    let inner = TxEip1559 {
        chain_id: ChainParams::testnet_1bps().chain_id,
        nonce,
        gas_limit: 3_000_000,
        max_fee_per_gas: 100_000_000_000,
        max_priority_fee_per_gas: tip,
        to: kind,
        value: U256::from(value),
        access_list: Default::default(),
        input: data,
    };
    let signature = Signature::new(U256::from(nonce + 1), U256::from(u64::from(from) + 1), false);
    let envelope = TxEnvelope::Eip1559(Signed::new_unhashed(inner, signature));
    Recovered::new_unchecked(envelope, account(from))
}

fn transfer(from: u32, to: u32, nonce: u64, tip: u128) -> ChainTx {
    tx(from, nonce, TxKind::Call(account(to)), Bytes::new(), 1_000, tip)
}

/// A workload: a list of (miner index, transactions) making up DAG blocks.
type Workload = Vec<(u8, Vec<ChainTx>)>;

/// Everything observable about a run: the two strategies must agree on all of
/// it, not merely on the state root.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    state_root: B256,
    height: u64,
    gas_per_block: Vec<u64>,
    receipts_per_block: Vec<usize>,
}

/// Runs a workload with the given execution strategy.
fn run(workload: &Workload, parallel: bool) -> Observed {
    run_with_stats(workload, parallel).0
}

/// As [`run`], also returning (rounds committed from speculation, rounds that
/// fell back).
fn run_with_stats(workload: &Workload, parallel: bool) -> (Observed, (u64, u64)) {
    let params = ChainParams::testnet_1bps();
    let g = genesis_header();
    let genesis_hash = g.hash();

    let mut alloc = Genesis::default();
    for n in 0..64u32 {
        alloc.alloc.insert(
            account(n),
            GenesisAccount { balance: U256::from(FUNDING), ..Default::default() },
        );
    }

    let mut dag = DagStore::new(g, K, MERGESET_LIMIT);
    let mut bodies = BodyStore::new();
    let mut executor = ChainExecutor::new(params, load_genesis(&alloc).unwrap(), genesis_hash);
    executor.set_parallel(parallel);

    let mut parent = genesis_hash;
    for (index, (miner_index, txs)) in workload.iter().enumerate() {
        let nonce = index as u64 + 1;
        let header = Header {
            parents: vec![parent],
            nonce,
            miner: miner(*miner_index),
            timestamp_ms: GENESIS_MS + nonce * 1_000,
            ..genesis_header()
        };
        let hash: BlockHash = dag.add_block(header).unwrap();
        parent = hash;
        bodies.insert(hash, txs.clone());
    }

    let reorg = compute_reorg(&dag, genesis_hash, parent);
    let outcomes =
        executor.apply_reorg(&dag, &bodies, &reorg.removed, &reorg.added).expect("executes");

    (
        Observed {
            state_root: executor.state_root(),
            height: executor.height(),
            gas_per_block: outcomes.iter().map(|o| o.gas_used).collect(),
            receipts_per_block: outcomes.iter().map(|o| o.receipts.len()).collect(),
        },
        executor.parallel_stats(),
    )
}

/// Asserts the two strategies agree on everything observable.
fn assert_identical(workload: &Workload, label: &str) {
    let sequential = run(workload, false);
    let parallel = run(workload, true);

    assert_eq!(sequential.state_root, parallel.state_root, "{label}: state roots differ");
    assert_eq!(sequential.height, parallel.height, "{label}: heights differ");
    assert_eq!(sequential.gas_per_block, parallel.gas_per_block, "{label}: gas differs");
    assert_eq!(
        sequential.receipts_per_block, parallel.receipts_per_block,
        "{label}: receipt counts differ"
    );
}

#[test]
fn an_empty_chain_agrees() {
    assert_identical(&vec![(1, Vec::new()), (2, Vec::new())], "empty blocks");
}

#[test]
fn fully_independent_transfers_agree() {
    // The best case: every transaction touches a different pair of accounts,
    // so the whole block is one round and everything runs in parallel.
    let txs: Vec<ChainTx> = (0..16).map(|i| transfer(i, i + 32, 0, 1)).collect();
    assert_identical(&vec![(1, txs)], "independent transfers");
}

#[test]
fn a_shared_sender_agrees() {
    // Same sender throughout, so the layering rule puts every transaction in
    // its own round and parallel execution degenerates to sequential. The
    // answer must still be identical.
    let txs: Vec<ChainTx> = (0..10).map(|n| transfer(1, 40, n, 1)).collect();
    assert_identical(&vec![(1, txs)], "shared sender");
}

#[test]
fn a_hot_recipient_agrees() {
    // Everyone pays the same account. Statically they all conflict, so this is
    // the worst case for parallelism and a good test of the fallback.
    let txs: Vec<ChainTx> = (0..12).map(|i| transfer(i, 50, 0, 1)).collect();
    assert_identical(&vec![(1, txs)], "hot recipient");
}

#[test]
fn transactions_paying_the_same_miner_agree() {
    // Every transaction credits its priority fee to the same beneficiary, so
    // naively they all write the same account. Fee credits are excluded from
    // conflict detection and re-applied afterwards; if that were wrong, the
    // miner's balance would come out different here.
    let txs: Vec<ChainTx> = (0..16).map(|i| transfer(i, i + 32, 0, 7)).collect();
    assert_identical(&vec![(1, txs)], "shared beneficiary");
}

#[test]
fn a_transaction_sent_to_the_miner_agrees() {
    // A transaction that genuinely touches the beneficiary's account, rather
    // than merely paying it. The fee-credit shortcut must not apply here, and
    // the round must fall back.
    let miner_account = miner(1);
    let inner = TxEip1559 {
        chain_id: ChainParams::testnet_1bps().chain_id,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: 100_000_000_000,
        max_priority_fee_per_gas: 3,
        to: TxKind::Call(miner_account),
        value: U256::from(5_000u64),
        access_list: Default::default(),
        input: Bytes::new(),
    };
    let signature = Signature::new(U256::from(1u64), U256::from(2u64), false);
    let to_miner = Recovered::new_unchecked(
        TxEnvelope::Eip1559(Signed::new_unhashed(inner, signature)),
        account(3),
    );

    let mut txs: Vec<ChainTx> = (0..6).map(|i| transfer(i + 10, i + 40, 0, 3)).collect();
    txs.push(to_miner);
    let workload = vec![(1u8, txs)];
    assert_identical(&workload, "transaction paying the miner directly");

    // And confirm it passed for the right reason. These transactions have
    // disjoint static access sets, so they form one round; the fee-credit
    // shortcut must refuse to apply and force the round to fall back. If it
    // did not, this test would be checking nothing.
    let (_, (committed, fell_back)) = run_with_stats(&workload, true);
    assert_eq!(fell_back, 1, "the round should have fallen back");
    assert_eq!(committed, 0, "no round here is safe to commit from speculation");
}

#[test]
fn contract_deployment_and_calls_agree() {
    // Real contract execution, where the EVM touches storage the static access
    // set never mentioned. This is precisely the case speculative validation
    // exists to catch.
    let supply = U256::from(1_000_000u64) * U256::from(10u64).pow(U256::from(18u64));
    let deploy = tx(1, 0, TxKind::Create, erc20_initcode(supply), 0, 1);

    // The address the deployment will produce.
    let token = account(1).create(0);

    let mut calls: Vec<ChainTx> = Vec::new();
    for i in 0..6u32 {
        let data = {
            use alloy_sol_types::{SolCall, sol};
            sol! { function transfer(address to, uint256 value) external returns (bool); }
            Bytes::from(
                transferCall { to: account(i + 40), value: U256::from(1_000u64) }.abi_encode(),
            )
        };
        calls.push(tx(1, u64::from(i) + 1, TxKind::Call(token), data, 0, 1));
    }

    assert_identical(&vec![(1, vec![deploy]), (2, calls)], "erc20 deploy then transfers");
}

#[test]
fn many_blocks_across_many_miners_agree() {
    // A longer, more varied workload: different miners, mixed conflicts, and
    // enough blocks that a divergence anywhere would show in the final root.
    let mut workload: Workload = Vec::new();
    for block in 0..20u32 {
        let miner_index = u8::try_from(block % 4).expect("small");
        let txs: Vec<ChainTx> = (0..8)
            .map(|i| {
                // Deliberately overlapping: some transactions share senders
                // across blocks, some share recipients.
                let from = (block * 3 + i) % 24;
                let to = 32 + (i % 5);
                transfer(from, to, u64::from(block / 8), u128::from(i) + 1)
            })
            .collect();
        workload.push((miner_index, txs));
    }
    assert_identical(&workload, "twenty blocks, four miners");
}

#[test]
fn an_unexecutable_transaction_agrees() {
    // A bad nonce is skipped by both paths, and skipping must not shift the
    // gas accounting differently between them.
    let mut txs: Vec<ChainTx> = (0..6).map(|i| transfer(i, i + 32, 0, 1)).collect();
    txs.insert(3, transfer(20, 33, 99, 1));
    assert_identical(&vec![(1, txs)], "one unexecutable transaction");
}

#[test]
fn parallel_execution_is_deterministic_across_runs() {
    // Parallel scheduling must not leak into the result. Running the same
    // workload repeatedly must give the same root every time.
    let txs: Vec<ChainTx> = (0..16).map(|i| transfer(i, i + 32, 0, 3)).collect();
    let workload = vec![(1u8, txs)];
    let first = run(&workload, true);
    for _ in 0..5 {
        assert_eq!(run(&workload, true), first, "parallel execution was not deterministic");
    }
}

/// Times both strategies on a workload and reports the ratio.
fn report(label: &str, workload: &Workload) {
    // Warm up, so neither run pays for first-touch page faults or a cold
    // thread pool.
    let _ = run(workload, false);
    let _ = run(workload, true);

    let sequential_start = std::time::Instant::now();
    let sequential = run(workload, false);
    let sequential_time = sequential_start.elapsed();

    let parallel_start = std::time::Instant::now();
    let (parallel, (committed, fell_back)) = run_with_stats(workload, true);
    let parallel_time = parallel_start.elapsed();

    assert_eq!(
        sequential.state_root, parallel.state_root,
        "{label}: roots differ, so the timing is meaningless"
    );

    #[allow(clippy::float_arithmetic, reason = "a benchmark report, not consensus")]
    let speedup = sequential_time.as_secs_f64() / parallel_time.as_secs_f64().max(1e-9);
    let count: usize = workload.iter().map(|(_, t)| t.len()).sum();
    let gas: u64 = sequential.gas_per_block.iter().sum();
    eprintln!(
        "{label:>18}: {count:>4} txs, {gas:>9} gas | seq {sequential_time:>9.2?}, \
         par {parallel_time:>9.2?} -> {speedup:.2}x | rounds committed={committed} \
         fell_back={fell_back}"
    );
}

#[test]
fn report_parallel_speedup() {
    // The measured-speedup half of the gate. Reported rather than asserted:
    // the honest answer depends entirely on how much work each transaction
    // does, and a threshold here would either be trivially true or flaky.
    eprintln!("threads available: {}", rayon::current_num_threads());

    // Bare value transfers: ~21,000 gas, a few microseconds of EVM work each.
    let transfers: Vec<ChainTx> = (0..64).map(|i| transfer(i, i + 32, 0, 1)).collect();
    report(
        "value transfers",
        &(0..12).map(|b| (u8::try_from(b % 3).unwrap(), transfers.clone())).collect(),
    );

    // Contract calls: ~50,000 gas, real storage access, an order of magnitude
    // more work per transaction.
    let supply = U256::from(1_000_000u64) * U256::from(10u64).pow(U256::from(18u64));
    let mut workload: Workload =
        vec![(1, vec![tx(1, 0, TxKind::Create, erc20_initcode(supply), 0, 1)])];
    let token = account(1).create(0);
    for block in 0..8u32 {
        let calls: Vec<ChainTx> = (0..24u32)
            .map(|i| {
                use alloy_sol_types::{SolCall, sol};
                sol! { function transfer(address to, uint256 value) external returns (bool); }
                let data = Bytes::from(
                    transferCall { to: account(i + 40), value: U256::from(10u64) }.abi_encode(),
                );
                // Distinct senders so the calls land in one round; they all
                // touch the token's storage, which is exactly the dynamic
                // conflict speculation has to detect.
                tx(i, u64::from(block), TxKind::Call(token), data, 0, 1)
            })
            .collect();
        workload.push((u8::try_from(block % 3).unwrap(), calls));
    }
    report("erc20 transfers", &workload);
}
