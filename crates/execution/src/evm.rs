//! Building and driving the EVM.
//!
//! We construct EVMs through [`EthEvmFactory`]
//! (registry:alloy-evm-0.39.0/src/eth/mod.rs:268) and drive transactions
//! ourselves rather than going through `alloy-evm`'s `EthBlockExecutor`.
//!
//! The reason is structural, not preference: `BlockEnv` carries a single
//! `beneficiary` (registry:revm-context-43.0.2/src/block.rs:14) and
//! `EthBlockExecutor::apply_post_execution_changes` pays exactly that one
//! address (registry:alloy-evm-0.39.0/src/eth/block.rs:308). CHAINNAME pays
//! *every block in the merge set* from its own header, so the beneficiary
//! changes between transactions inside one chain block. See DECISIONS.md
//! C-004 and D-007.

use alloy_evm::{
    EvmEnv, EvmFactory,
    eth::{EthEvm, EthEvmFactory},
};
use alloy_primitives::{Address, B256, U256};
use chainname_primitives::ChainParams;
use revm::{
    context::{BlockEnv, CfgEnv},
    context_interface::block::BlobExcessGasAndPrice,
    context_interface::{ContextSetters, ContextTr},
    primitives::hardfork::SpecId,
};

/// Excess blob gas, permanently zero: CHAINNAME has no blob transactions.
const NO_EXCESS_BLOB_GAS: u64 = 0;

/// Blob base fee update fraction.
///
/// Carried only so `BLOBBASEFEE` has a defined value. With
/// [`NO_EXCESS_BLOB_GAS`] fixed at zero the resulting blob base fee is the
/// protocol minimum regardless of this figure, so the exact value is inert;
/// we use Ethereum's Prague constant rather than inventing one.
const BLOB_BASE_FEE_UPDATE_FRACTION: u64 = 5_007_716;

/// EVM specification level.
///
/// `AMSTERDAM` exists in revm 43 but is documented "Activated at block TBD"
/// (registry:revm-primitives-43.0.0/src/hardfork.rs:73) and is not final.
/// OSAKA is the latest finalised fork. DECISIONS.md D-010.
pub const SPEC_ID: SpecId = SpecId::OSAKA;

/// Everything the EVM needs to know about the selected-chain block being
/// executed, other than the transactions themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBlockCtx {
    /// Selected-chain height.
    pub height: u64,
    /// Block timestamp, seconds since the Unix epoch.
    ///
    /// Headers carry milliseconds (needed to order blocks at 10 bps), but the
    /// `TIMESTAMP` opcode is specified in seconds and contracts depend on that.
    pub timestamp_secs: u64,
    /// EIP-1559 base fee for this chain block, in wei per gas.
    pub base_fee_per_gas: u64,
    /// Gas limit for this chain block.
    pub gas_limit: u64,
    /// Value returned by the `PREVRANDAO` opcode.
    ///
    /// This is the chain block's own proof-of-work hash. It is **miner
    /// grindable and is not a randomness beacon** — a proof-of-work chain
    /// cannot provide one. Contracts using it as randomness are insecure here,
    /// exactly as they were on pre-merge Ethereum. DECISIONS.md D-012,
    /// OPEN-PROBLEMS.md P-005.
    pub prevrandao: B256,
}

/// Builds the EVM environment for a chain block.
///
/// `beneficiary` is left zero here. It is set per transaction, to the miner of
/// the DAG block that actually carried that transaction, via
/// `ContextTr::set_block` (registry:revm-context-interface-43.0.1/src/context.rs:283).
pub fn evm_env(params: &ChainParams, ctx: &ChainBlockCtx) -> EvmEnv<SpecId, BlockEnv> {
    let cfg_env = CfgEnv::new_with_spec(SPEC_ID).with_chain_id(params.chain_id);

    let block_env = BlockEnv {
        number: U256::from(ctx.height),
        beneficiary: Address::ZERO,
        timestamp: U256::from(ctx.timestamp_secs),
        gas_limit: ctx.gas_limit,
        basefee: ctx.base_fee_per_gas,
        // Unused after Paris; `prevrandao` replaces it. Kept at 1 rather than 0
        // because some legacy contracts branch on `DIFFICULTY != 0` to detect
        // the merge, and we are not pre-merge Ethereum.
        difficulty: U256::from(1u64),
        prevrandao: Some(ctx.prevrandao),
        // EIP-4844 is cut (DECISIONS.md D-011). We publish a present-but-zero
        // value rather than `None` so `BLOBHASH` and `BLOBBASEFEE` return
        // defined results at a >= Cancun spec level. No blob can ever exist:
        // type-0x03 transactions are rejected at decode, mempool, and block
        // validation.
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new(
            NO_EXCESS_BLOB_GAS,
            BLOB_BASE_FEE_UPDATE_FRACTION,
        )),
        slot_num: 0,
    };

    EvmEnv { cfg_env, block_env }
}

/// The concrete EVM type this crate builds.
///
/// Named so callers do not have to spell the factory's associated type.
pub type ChainEvm<DB> = <EthEvmFactory as EvmFactory>::Evm<DB, revm::inspector::NoOpInspector>;

/// Creates an EVM over `db` for the given chain block.
pub fn make_evm<DB>(db: DB, params: &ChainParams, ctx: &ChainBlockCtx) -> ChainEvm<DB>
where
    DB: alloy_evm::Database,
{
    EthEvmFactory::default().create_evm(db, evm_env(params, ctx))
}

/// Points the EVM's beneficiary at `miner` for the next transaction.
///
/// Called before each transaction in a merge set so the priority fee and the
/// `COINBASE` opcode both resolve to the miner of the DAG block that carried
/// it, rather than to the miner of the chain block that merged it.
pub fn set_beneficiary<DB, I, P>(evm: &mut EthEvm<DB, I, P>, miner: Address)
where
    DB: alloy_evm::Database,
{
    let mut block = evm.ctx().block().clone();
    block.beneficiary = miner;
    evm.ctx_mut().set_block(block);
}
