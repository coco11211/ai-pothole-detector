//! M2 gate: execute real transactions through revm against CHAINNAME state.
//!
//! Two things are proven here:
//! 1. A plain value transfer executes and moves balances (the "hardcoded
//!    transaction against genesis state" gate).
//! 2. An ERC-20 compiled by solc 0.8.30 deploys, and `transfer` moves token
//!    balances correctly (the milestone gate proper).
//!
//! The ERC-20 bytecode in `testdata/Erc20.bin` is checked in so CI needs no
//! solc. `solc_bytecode_is_current` recompiles and compares when solc *is*
//! available, so the artifact cannot silently drift from its source.

use alloy_evm::Evm;
use alloy_genesis::{Genesis, GenesisAccount};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_sol_types::{SolCall, SolValue, sol};
use chainname_execution::{ChainBlockCtx, WorldState, load_genesis, make_evm, set_beneficiary};
use chainname_primitives::ChainParams;
use revm::context::{
    TxEnv,
    result::{ExecutionResult, Output},
};

sol! {
    #[allow(missing_docs)]
    function transfer(address to, uint256 value) external returns (bool);
    #[allow(missing_docs)]
    function balanceOf(address account) external view returns (uint256);
    #[allow(missing_docs)]
    function totalSupply() external view returns (uint256);
}

/// Deployer, funded in genesis.
const ALICE: Address = Address::new([0x11; 20]);
/// Transfer recipient.
const BOB: Address = Address::new([0x22; 20]);
/// Miner of the DAG block that carried the transaction under test.
const MINER: Address = Address::new([0x33; 20]);
/// Caller for read-only calls. Funded so view calls pass the fee checks
/// without needing a modified EVM config; nothing it does is ever committed.
const VIEWER: Address = Address::new([0x44; 20]);

/// Enough to cover any gas this test can spend, with room to observe changes.
const FUNDING_WEI: u128 = 1_000_000_000_000_000_000_000;
/// A base fee low enough that gas costs stay legible in assertions.
const BASE_FEE: u64 = 7;
/// Generous per-transaction gas ceiling; the block limit is the real bound.
const TX_GAS_LIMIT: u64 = 5_000_000;

fn params() -> ChainParams {
    ChainParams::testnet_1bps()
}

fn ctx() -> ChainBlockCtx {
    ChainBlockCtx {
        height: 1,
        timestamp_secs: 1_700_000_000,
        base_fee_per_gas: BASE_FEE,
        gas_limit: params().block_gas_limit(),
        prevrandao: B256::repeat_byte(0xab),
    }
}

fn funded_genesis() -> WorldState {
    let mut genesis = Genesis::default();
    for account in [ALICE, VIEWER] {
        genesis.alloc.insert(
            account,
            GenesisAccount { balance: U256::from(FUNDING_WEI), ..Default::default() },
        );
    }
    load_genesis(&genesis).unwrap()
}

fn base_tx(caller: Address, nonce: u64, kind: TxKind, data: Bytes, value: U256) -> TxEnv {
    TxEnv {
        tx_type: 2, // EIP-1559. 1559 is kept intact; wallets depend on it.
        caller,
        gas_limit: TX_GAS_LIMIT,
        gas_price: u128::from(BASE_FEE),
        gas_priority_fee: Some(0),
        kind,
        value,
        data,
        nonce,
        chain_id: Some(params().chain_id),
        ..Default::default()
    }
}

fn erc20_initcode(initial_supply: U256) -> Bytes {
    let hex_src = include_str!("../testdata/Erc20.bin").trim();
    let mut code = hex::decode(hex_src).expect("checked-in ERC-20 bytecode is valid hex");
    // Constructor arguments are ABI-encoded and appended to the init code.
    code.extend_from_slice(&initial_supply.abi_encode());
    Bytes::from(code)
}

#[test]
fn value_transfer_moves_balances() {
    let state = funded_genesis();
    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);

    let amount = U256::from(1_234_567u64);
    let result = evm
        .transact_commit(base_tx(ALICE, 0, TxKind::Call(BOB), Bytes::new(), amount))
        .expect("transfer executes");
    assert!(result.is_success(), "value transfer reverted: {result:?}");
    assert_eq!(result.tx_gas_used(), 21_000, "a bare transfer is exactly the base cost");

    let state = evm.into_db();
    assert_eq!(state.balance(BOB), amount);
    assert_eq!(state.nonce(ALICE), 1, "sender nonce is bumped");

    let spent_on_gas = U256::from(BASE_FEE) * U256::from(21_000u64);
    assert_eq!(state.balance(ALICE), U256::from(FUNDING_WEI) - amount - spent_on_gas);
}

#[test]
fn base_fee_is_burned_not_paid_to_the_miner() {
    // EIP-1559 stays intact and the base fee is burned. With a zero priority
    // fee the miner must receive nothing at all.
    let state = funded_genesis();
    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);

    evm.transact_commit(base_tx(ALICE, 0, TxKind::Call(BOB), Bytes::new(), U256::ZERO)).unwrap();

    let state = evm.into_db();
    assert_eq!(state.balance(MINER), U256::ZERO, "base fee must be burned, not paid out");
}

#[test]
fn priority_fee_goes_to_the_carrying_blocks_miner() {
    let state = funded_genesis();
    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);

    let tip: u128 = 3;
    let mut tx = base_tx(ALICE, 0, TxKind::Call(BOB), Bytes::new(), U256::ZERO);
    tx.gas_price = u128::from(BASE_FEE) + tip;
    tx.gas_priority_fee = Some(tip);

    let result = evm.transact_commit(tx).unwrap();
    assert!(result.is_success());

    let state = evm.into_db();
    assert_eq!(
        state.balance(MINER),
        U256::from(tip) * U256::from(result.tx_gas_used()),
        "the miner receives exactly the priority fee"
    );
}

#[test]
fn erc20_deploys_and_transfers() {
    let supply = U256::from(1_000_000u64) * U256::from(10u64).pow(U256::from(18u64));

    let state = funded_genesis();
    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);

    // --- deploy ---
    let deploy = evm
        .transact_commit(base_tx(ALICE, 0, TxKind::Create, erc20_initcode(supply), U256::ZERO))
        .expect("deploy executes");

    let token = match &deploy {
        ExecutionResult::Success { output: Output::Create(code, Some(addr)), .. } => {
            assert!(!code.is_empty(), "deployed runtime code must not be empty");
            *addr
        }
        other => panic!("deploy did not succeed: {other:?}"),
    };

    // --- totalSupply() reflects the constructor argument ---
    let total = call_view(&mut evm, token, totalSupplyCall {}.abi_encode());
    assert_eq!(U256::abi_decode(&total).unwrap(), supply);

    // --- the deployer holds the whole supply ---
    let alice_before = call_view(&mut evm, token, balanceOfCall { account: ALICE }.abi_encode());
    assert_eq!(U256::abi_decode(&alice_before).unwrap(), supply);

    // --- transfer ---
    let amount = U256::from(42_000u64);
    let transferred = evm
        .transact_commit(base_tx(
            ALICE,
            1,
            TxKind::Call(token),
            Bytes::from(transferCall { to: BOB, value: amount }.abi_encode()),
            U256::ZERO,
        ))
        .expect("transfer executes");
    assert!(transferred.is_success(), "ERC-20 transfer reverted: {transferred:?}");
    assert_eq!(transferred.logs().len(), 1, "transfer emits exactly one Transfer event");

    // --- balances moved ---
    let alice_after = call_view(&mut evm, token, balanceOfCall { account: ALICE }.abi_encode());
    let bob_after = call_view(&mut evm, token, balanceOfCall { account: BOB }.abi_encode());
    assert_eq!(U256::abi_decode(&alice_after).unwrap(), supply - amount);
    assert_eq!(U256::abi_decode(&bob_after).unwrap(), amount);
}

#[test]
fn erc20_transfer_beyond_balance_reverts() {
    let supply = U256::from(100u64);
    let state = funded_genesis();
    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);

    let deploy = evm
        .transact_commit(base_tx(ALICE, 0, TxKind::Create, erc20_initcode(supply), U256::ZERO))
        .unwrap();
    let ExecutionResult::Success { output: Output::Create(_, Some(token)), .. } = deploy else {
        panic!("deploy failed");
    };

    let result = evm
        .transact_commit(base_tx(
            ALICE,
            1,
            TxKind::Call(token),
            Bytes::from(transferCall { to: BOB, value: supply + U256::from(1u64) }.abi_encode()),
            U256::ZERO,
        ))
        .unwrap();

    assert!(matches!(result, ExecutionResult::Revert { .. }), "expected revert, got {result:?}");
}

#[test]
fn state_root_changes_after_execution() {
    let state = funded_genesis();
    let before = state.state_root();

    let mut evm = make_evm(state, &params(), &ctx());
    set_beneficiary(&mut evm, MINER);
    evm.transact_commit(base_tx(ALICE, 0, TxKind::Call(BOB), Bytes::new(), U256::from(1u64)))
        .unwrap();

    assert_ne!(evm.into_db().state_root(), before);
}

#[test]
fn execution_is_deterministic() {
    // Same genesis, same transactions, twice: identical state root. This is the
    // property every later milestone depends on.
    let run = || {
        let mut evm = make_evm(funded_genesis(), &params(), &ctx());
        set_beneficiary(&mut evm, MINER);
        let supply = U256::from(5_000u64);
        evm.transact_commit(base_tx(ALICE, 0, TxKind::Create, erc20_initcode(supply), U256::ZERO))
            .unwrap();
        evm.into_db().state_root()
    };
    assert_eq!(run(), run());
}

/// Runs a read-only call and returns its output, without committing.
fn call_view<DB: alloy_evm::Database>(
    evm: &mut chainname_execution::ChainEvm<DB>,
    to: Address,
    data: Vec<u8>,
) -> Bytes {
    // `transact` rather than `transact_commit`: the state change is discarded,
    // so VIEWER's nonce stays at 0 and every view call looks identical.
    let tx = base_tx(VIEWER, 0, TxKind::Call(to), Bytes::from(data), U256::ZERO);
    let out = evm.transact(tx).expect("view call executes");
    match out.result {
        ExecutionResult::Success { output: Output::Call(bytes), .. } => bytes,
        other => panic!("view call failed: {other:?}"),
    }
}

#[test]
fn solc_bytecode_is_current() {
    // Guards against the checked-in artifact drifting from Erc20.sol. Skips
    // when solc is unavailable so CI stays hermetic (BLOCKERS.md B-001).
    let Some(solc) = find_solc() else {
        eprintln!("skipping: solc not on PATH or at CHAINNAME_SOLC");
        return;
    };

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    let output = std::process::Command::new(&solc)
        .args(["--bin", "--optimize", "--optimize-runs", "200", "Erc20.sol"])
        .current_dir(dir)
        .output()
        .expect("solc runs");
    assert!(output.status.success(), "solc failed: {}", String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let recompiled = stdout
        .lines()
        .skip_while(|l| !l.starts_with("Binary:"))
        .nth(1)
        .expect("solc emitted a Binary: section")
        .trim();

    let checked_in = include_str!("../testdata/Erc20.bin").trim();
    assert_eq!(recompiled, checked_in, "testdata/Erc20.bin is stale; regenerate it from Erc20.sol");
}

fn find_solc() -> Option<String> {
    if let Ok(path) = std::env::var("CHAINNAME_SOLC")
        && std::path::Path::new(&path).exists()
    {
        return Some(path);
    }
    std::process::Command::new("solc")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| "solc".to_string())
}
