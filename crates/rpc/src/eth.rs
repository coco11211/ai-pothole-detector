//! The `eth_*` namespace.
//!
//! Response *shapes* are Ethereum's, unchanged, so existing tooling works with
//! no patches. Where a DAG concept has no Ethereum equivalent it is mapped onto
//! the nearest honest one rather than added as an extra field:
//!
//! * `blockNumber` is **selected-chain height**. It is the only sequence with
//!   one block per height, which is what every Ethereum client assumes.
//! * A receipt's `confirmations`-equivalent depth is **blue-score depth**, the
//!   natural meaning of "how buried is this" in a DAG.
//! * `difficulty` and `mixHash` carry real values, because this is a
//!   proof-of-work chain and pre-merge semantics are the ones that fit.

use alloy_consensus::{Transaction as _, TxEnvelope, Typed2718 as _};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{Address, B256, Bloom, Bytes, TxKind, U256};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use chainname_difficulty::CompactTarget;
use chainname_execution::{ChainBlockCtx, make_evm, set_beneficiary};
use chainname_ghostdag::work_for_target;
use jsonrpsee::{
    core::RpcResult,
    proc_macros::rpc,
    types::{ErrorObject, ErrorObjectOwned},
};
use revm::context::{
    TxEnv,
    result::{ExecutionResult, Output},
};
use serde_json::{Value, json};

use crate::backend::Backend;

/// Client version string, reported by `web3_clientVersion`.
pub const CLIENT_VERSION: &str = concat!("CHAINNAME/v", env!("CARGO_PKG_VERSION"), "/rust");

/// JSON-RPC error code for a rejected transaction.
const TRANSACTION_REJECTED: i32 = -32003;
/// JSON-RPC error code for invalid parameters.
const INVALID_PARAMS: i32 = -32602;
/// JSON-RPC error code for a resource that does not exist.
const RESOURCE_NOT_FOUND: i32 = -32001;

/// The subset of `eth_*` that wallets and Foundry actually use.
///
/// Deliberately not the whole historical surface: a method that would have to
/// lie is worse than one that is absent, because a client can detect absence.
#[rpc(server, namespace = "eth")]
pub trait EthApi {
    /// The chain id.
    #[method(name = "chainId")]
    fn chain_id(&self) -> RpcResult<String>;

    /// Selected-chain height of the tip.
    #[method(name = "blockNumber")]
    fn block_number(&self) -> RpcResult<String>;

    /// Account balance.
    #[method(name = "getBalance")]
    fn get_balance(&self, address: Address, block: Option<Value>) -> RpcResult<String>;

    /// Account nonce. `pending` includes queued transactions.
    #[method(name = "getTransactionCount")]
    fn get_transaction_count(&self, address: Address, block: Option<Value>) -> RpcResult<String>;

    /// Contract code.
    #[method(name = "getCode")]
    fn get_code(&self, address: Address, block: Option<Value>) -> RpcResult<String>;

    /// A storage slot.
    #[method(name = "getStorageAt")]
    fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block: Option<Value>,
    ) -> RpcResult<String>;

    /// Executes a call without committing.
    #[method(name = "call")]
    fn call(&self, request: TransactionRequest, block: Option<Value>) -> RpcResult<String>;

    /// Estimates gas for a call.
    #[method(name = "estimateGas")]
    fn estimate_gas(&self, request: TransactionRequest, block: Option<Value>) -> RpcResult<String>;

    /// A legacy gas price: base fee plus a suggested tip.
    #[method(name = "gasPrice")]
    fn gas_price(&self) -> RpcResult<String>;

    /// Suggested priority fee.
    #[method(name = "maxPriorityFeePerGas")]
    fn max_priority_fee_per_gas(&self) -> RpcResult<String>;

    /// Historical base fees and gas ratios.
    #[method(name = "feeHistory")]
    fn fee_history(
        &self,
        count: Value,
        newest: Value,
        reward_percentiles: Option<Vec<f64>>,
    ) -> RpcResult<Value>;

    /// Submits a signed transaction.
    #[method(name = "sendRawTransaction")]
    fn send_raw_transaction(&self, raw: Bytes) -> RpcResult<String>;

    /// A transaction receipt, or null if it has not executed.
    #[method(name = "getTransactionReceipt")]
    fn get_transaction_receipt(&self, hash: B256) -> RpcResult<Option<Value>>;

    /// A transaction by hash, mined or pending.
    #[method(name = "getTransactionByHash")]
    fn get_transaction_by_hash(&self, hash: B256) -> RpcResult<Option<Value>>;

    /// A selected-chain block by number.
    #[method(name = "getBlockByNumber")]
    fn get_block_by_number(&self, block: Value, full: Option<bool>) -> RpcResult<Option<Value>>;

    /// A selected-chain block by hash.
    #[method(name = "getBlockByHash")]
    fn get_block_by_hash(&self, hash: B256, full: Option<bool>) -> RpcResult<Option<Value>>;

    /// Always empty: this node holds no keys.
    #[method(name = "accounts")]
    fn accounts(&self) -> RpcResult<Vec<Address>>;

    /// Always false: there is no separate sync phase to report.
    #[method(name = "syncing")]
    fn syncing(&self) -> RpcResult<bool>;

    /// Logs matching a filter. Dapps depend on this far more than wallets do.
    #[method(name = "getLogs")]
    fn get_logs(&self, filter: Value) -> RpcResult<Vec<Value>>;
}

/// Implements [`EthApiServer`] over a [`Backend`].
#[derive(Debug, Clone)]
pub struct EthApiImpl {
    backend: Backend,
}

impl EthApiImpl {
    /// Wraps a backend.
    pub const fn new(backend: Backend) -> Self {
        Self { backend }
    }

    fn hex_u64(value: u64) -> String {
        format!("{value:#x}")
    }

    fn hex_u256(value: U256) -> String {
        format!("{value:#x}")
    }

    fn invalid_params(message: impl Into<String>) -> ErrorObjectOwned {
        ErrorObject::owned(INVALID_PARAMS, message.into(), None::<()>)
    }

    fn rejected(message: impl Into<String>) -> ErrorObjectOwned {
        ErrorObject::owned(TRANSACTION_REJECTED, message.into(), None::<()>)
    }

    /// Builds a `TxEnv` from an `eth_call` style request.
    fn call_env(&self, request: &TransactionRequest) -> TxEnv {
        let base_fee = self.backend.next_base_fee();
        let params = self.backend.params();
        TxEnv {
            tx_type: 2,
            caller: request.from.unwrap_or(Address::ZERO),
            gas_limit: request
                .gas
                .unwrap_or_else(|| params.block_gas_limit().min(TX_GAS_LIMIT_CAP)),
            // A call is not paid for, so the fee ceiling is set to the base fee
            // rather than to whatever the caller guessed. Otherwise a caller
            // who omits `gasPrice` gets "gas price below base fee" instead of
            // an answer.
            gas_price: u128::from(base_fee),
            gas_priority_fee: Some(0),
            kind: request.to.unwrap_or(TxKind::Create),
            value: request.value.unwrap_or_default(),
            data: request.input.input().cloned().unwrap_or_default(),
            nonce: self.backend.nonce(request.from.unwrap_or(Address::ZERO)),
            chain_id: Some(params.chain_id),
            ..Default::default()
        }
    }

    /// Runs a call against the executed tip without committing.
    fn simulate(&self, request: &TransactionRequest) -> Result<ExecutionResult, ErrorObjectOwned> {
        let params = self.backend.params();
        let base_fee = self.backend.next_base_fee();
        let height = self.backend.height();

        let tx = self.call_env(request);
        // Zero the caller's fee obligations for a simulation: a call has no
        // sender who has agreed to pay, and requiring a funded `from` would
        // make `eth_call` useless from an unfunded address, which is how
        // wallets use it.
        let mut tx = tx;
        tx.gas_price = 0;
        tx.gas_priority_fee = Some(0);

        self.backend.read(|state| {
            let ctx = ChainBlockCtx {
                height: height + 1,
                timestamp_secs: state
                    .executor
                    .result_at(height)
                    .map_or(0, |r| r.timestamp_secs)
                    .saturating_add(1),
                // Zero, matching the zeroed transaction fee fields above.
                base_fee_per_gas: 0,
                gas_limit: params.block_gas_limit(),
                prevrandao: state.executor.tip(),
            };
            let _ = base_fee;

            let db = state.executor.state().clone();
            let mut evm = make_evm(db, &params, &ctx);
            set_beneficiary(&mut evm, Address::ZERO);
            use alloy_evm::Evm;
            evm.transact(tx)
                .map(|out| out.result)
                .map_err(|e| Self::rejected(format!("call failed: {e}")))
        })
    }

    /// Renders a selected-chain block in Ethereum's shape.
    fn render_block(&self, height: u64, full: bool) -> Option<Value> {
        self.backend.read(|state| {
            let outcome = state.executor.result_at(height)?.clone();
            let header = state.dag.header(outcome.hash).cloned();

            let (parent, difficulty, nonce, blue_score) = match &header {
                Some(h) => {
                    let target = CompactTarget(h.bits).to_target().unwrap_or(U256::MAX);
                    let data = state.dag.data(outcome.hash);
                    (
                        data.map_or(B256::ZERO, |d| d.selected_parent),
                        work_for_target(target),
                        h.nonce,
                        data.map_or(0, |d| d.blue_score),
                    )
                }
                None => (B256::ZERO, U256::ZERO, 0, 0),
            };

            let transactions: Value = if full {
                Value::Array(
                    outcome
                        .transaction_hashes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, hash)| {
                            let bytes = state.tx_bytes.get(hash)?;
                            let envelope = TxEnvelope::decode_2718(&mut bytes.as_ref()).ok()?;
                            Some(render_transaction(
                                &envelope,
                                Some(outcome.hash),
                                Some(height),
                                Some(index),
                            ))
                        })
                        .collect(),
                )
            } else {
                Value::Array(
                    outcome
                        .transaction_hashes
                        .iter()
                        .map(|h| Value::String(format!("{h:#x}")))
                        .collect(),
                )
            };

            let logs_bloom: Bloom =
                outcome.receipts.iter().fold(Bloom::ZERO, |acc, r| acc | r.logs_bloom);

            Some(json!({
                "number": Self::hex_u64(height),
                "hash": format!("{:#x}", outcome.hash),
                "parentHash": format!("{parent:#x}"),
                "nonce": format!("{:#018x}", nonce),
                "sha3Uncles": format!("{:#x}", alloy_trie::EMPTY_ROOT_HASH),
                "logsBloom": format!("{logs_bloom:#x}"),
                "transactionsRoot": format!("{:#x}",
                    header.as_ref().map_or(alloy_trie::EMPTY_ROOT_HASH, |h| h.txs_root)),
                "stateRoot": format!("{:#x}", outcome.state_root),
                "receiptsRoot": format!("{:#x}", outcome.receipts_root),
                "miner": format!("{:#x}", outcome.miner),
                // A proof-of-work chain has a real difficulty, so it is
                // reported rather than zeroed as a post-merge client would.
                "difficulty": Self::hex_u256(difficulty),
                "totalDifficulty": Self::hex_u256(
                    state.dag.data(outcome.hash).map_or(U256::ZERO, |d| d.blue_work)
                ),
                // `mixHash` is where pre-merge Ethereum put the PoW hash, and
                // it is what PREVRANDAO reads. See DECISIONS.md D-012: this is
                // NOT a randomness beacon.
                "mixHash": format!("{:#x}", outcome.hash),
                "extraData": "0x",
                "size": Self::hex_u64(0),
                "gasLimit": Self::hex_u64(self.backend.params().block_gas_limit()),
                "gasUsed": Self::hex_u64(outcome.gas_used),
                "timestamp": Self::hex_u64(outcome.timestamp_secs),
                "baseFeePerGas": Self::hex_u64(outcome.base_fee_per_gas),
                "transactions": transactions,
                "uncles": Value::Array(Vec::new()),
                // Blue score is not an Ethereum field, but omitting it from a
                // block would force callers into a second round trip for the
                // one number that means "how buried is this" here. It is
                // additive, so strict clients ignore it.
                "blueScore": Self::hex_u64(blue_score),
            }))
        })
    }

    /// Resolves a block tag or number to a selected-chain height.
    fn resolve_height(&self, block: Option<&Value>) -> Result<u64, ErrorObjectOwned> {
        let Some(value) = block else { return Ok(self.backend.height()) };
        match value {
            Value::Null => Ok(self.backend.height()),
            Value::String(tag) => match tag.as_str() {
                // There is no finality on this chain, so `finalized` and `safe`
                // cannot mean what they do on Ethereum. They resolve to the
                // tip rather than to a lie about irreversibility; callers that
                // need settlement confidence read blue-score depth from the
                // `chainname_*` namespace. OPEN-PROBLEMS.md P-007.
                "latest" | "pending" | "safe" | "finalized" => Ok(self.backend.height()),
                "earliest" => Ok(0),
                hex => u64::from_str_radix(hex.trim_start_matches("0x"), 16)
                    .map_err(|_| Self::invalid_params(format!("bad block tag {hex}"))),
            },
            Value::Number(n) => {
                n.as_u64().ok_or_else(|| Self::invalid_params("block number out of range"))
            }
            Value::Object(_) => {
                // `{"blockNumber": "0x..."}` / `{"blockHash": "0x..."}` form.
                if let Some(Value::String(n)) = value.get("blockNumber") {
                    return u64::from_str_radix(n.trim_start_matches("0x"), 16)
                        .map_err(|_| Self::invalid_params("bad blockNumber"));
                }
                Ok(self.backend.height())
            }
            other => Err(Self::invalid_params(format!("unsupported block parameter {other}"))),
        }
    }

    /// True if the request asks about pending rather than executed state.
    fn is_pending(block: Option<&Value>) -> bool {
        matches!(block, Some(Value::String(tag)) if tag == "pending")
    }
}

/// Renders a transaction in Ethereum's shape.
fn render_transaction(
    envelope: &TxEnvelope,
    block_hash: Option<B256>,
    block_number: Option<u64>,
    index: Option<usize>,
) -> Value {
    use alloy_consensus::transaction::SignerRecoverable;

    let from = envelope.recover_signer().unwrap_or(Address::ZERO);
    let to = match envelope.kind() {
        TxKind::Call(address) => Value::String(format!("{address:#x}")),
        TxKind::Create => Value::Null,
    };

    json!({
        "hash": format!("{:#x}", envelope.hash()),
        "nonce": format!("{:#x}", envelope.nonce()),
        "blockHash": block_hash.map_or(Value::Null, |h| Value::String(format!("{h:#x}"))),
        "blockNumber": block_number.map_or(Value::Null, |n| Value::String(format!("{n:#x}"))),
        "transactionIndex": index.map_or(Value::Null, |i| Value::String(format!("{i:#x}"))),
        "from": format!("{from:#x}"),
        "to": to,
        "value": format!("{:#x}", envelope.value()),
        "gas": format!("{:#x}", envelope.gas_limit()),
        "gasPrice": format!("{:#x}", envelope.max_fee_per_gas()),
        "maxFeePerGas": format!("{:#x}", envelope.max_fee_per_gas()),
        "maxPriorityFeePerGas": format!("{:#x}",
            envelope.max_priority_fee_per_gas().unwrap_or(0)),
        "input": format!("0x{}", alloy_primitives::hex::encode(envelope.input())),
        "chainId": envelope.chain_id().map_or(Value::Null, |c| Value::String(format!("{c:#x}"))),
        "type": format!("{:#x}", envelope.ty()),
        "accessList": Value::Array(Vec::new()),
        "v": "0x0",
        "r": "0x0",
        "s": "0x0",
    })
}

impl EthApiServer for EthApiImpl {
    fn chain_id(&self) -> RpcResult<String> {
        Ok(Self::hex_u64(self.backend.params().chain_id))
    }

    fn block_number(&self) -> RpcResult<String> {
        Ok(Self::hex_u64(self.backend.height()))
    }

    fn get_balance(&self, address: Address, _block: Option<Value>) -> RpcResult<String> {
        Ok(Self::hex_u256(self.backend.balance(address)))
    }

    fn get_transaction_count(&self, address: Address, block: Option<Value>) -> RpcResult<String> {
        let nonce = if Self::is_pending(block.as_ref()) {
            self.backend.pending_nonce(address)
        } else {
            self.backend.nonce(address)
        };
        Ok(Self::hex_u64(nonce))
    }

    fn get_code(&self, address: Address, _block: Option<Value>) -> RpcResult<String> {
        let code = self.backend.read(|state| {
            state
                .executor
                .state()
                .code(address)
                .map(|c| c.original_bytes().to_vec())
                .unwrap_or_default()
        });
        Ok(format!("0x{}", alloy_primitives::hex::encode(code)))
    }

    fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        _block: Option<Value>,
    ) -> RpcResult<String> {
        let value = self.backend.read(|state| state.executor.state().storage_slot(address, slot));
        Ok(format!("{:#066x}", value))
    }

    fn call(&self, request: TransactionRequest, _block: Option<Value>) -> RpcResult<String> {
        match self.simulate(&request)? {
            ExecutionResult::Success { output, .. } => {
                Ok(format!("0x{}", alloy_primitives::hex::encode(output.data())))
            }
            ExecutionResult::Revert { output, .. } => Err(ErrorObject::owned(
                TRANSACTION_REJECTED,
                "execution reverted",
                Some(format!("0x{}", alloy_primitives::hex::encode(&output))),
            )),
            ExecutionResult::Halt { reason, .. } => {
                Err(Self::rejected(format!("execution halted: {reason:?}")))
            }
        }
    }

    fn estimate_gas(
        &self,
        request: TransactionRequest,
        _block: Option<Value>,
    ) -> RpcResult<String> {
        // Execute once at the block limit and report what was used, with a
        // margin. A binary search would be tighter, but this never
        // under-reports, and under-reporting is the failure that actually
        // costs users money.
        const MARGIN_PERCENT: u64 = 25;
        if let ExecutionResult::Halt { reason, .. } = self.simulate(&request)? {
            return Err(Self::rejected(format!("execution halted: {reason:?}")));
        }
        let used = self.simulate(&request)?.tx_gas_used();
        let with_margin = used.saturating_mul(100 + MARGIN_PERCENT) / 100;
        Ok(Self::hex_u64(with_margin.max(21_000)))
    }

    fn gas_price(&self) -> RpcResult<String> {
        // Base fee plus the suggested tip, which is what a legacy caller needs
        // to actually get included.
        let base = self.backend.next_base_fee();
        Ok(Self::hex_u64(base.saturating_add(SUGGESTED_TIP_WEI)))
    }

    fn max_priority_fee_per_gas(&self) -> RpcResult<String> {
        Ok(Self::hex_u64(SUGGESTED_TIP_WEI))
    }

    fn fee_history(
        &self,
        count: Value,
        _newest: Value,
        reward_percentiles: Option<Vec<f64>>,
    ) -> RpcResult<Value> {
        let requested = match &count {
            Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(1),
            Value::Number(n) => n.as_u64().unwrap_or(1),
            _ => 1,
        }
        .clamp(1, 1_024);

        let tip = self.backend.height();
        let oldest = tip.saturating_sub(requested.saturating_sub(1));

        let mut base_fees = Vec::new();
        let mut ratios = Vec::new();
        let gas_limit = self.backend.params().block_gas_limit();

        self.backend.read(|state| {
            for height in oldest..=tip {
                let (base, used) = state
                    .executor
                    .result_at(height)
                    .map_or((self.backend.next_base_fee(), 0), |r| {
                        (r.base_fee_per_gas, r.gas_used)
                    });
                base_fees.push(format!("{base:#x}"));
                ratios.push(gas_used_ratio(used, gas_limit));
            }
        });

        // One extra base fee: the schema wants `count + 1` entries, the last
        // being the fee for the block after the newest.
        base_fees.push(format!("{:#x}", self.backend.next_base_fee()));

        let rewards = reward_percentiles.map(|percentiles| {
            Value::Array(
                (oldest..=tip)
                    .map(|_| {
                        Value::Array(
                            percentiles
                                .iter()
                                .map(|_| Value::String(format!("{SUGGESTED_TIP_WEI:#x}")))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        });

        let mut out = json!({
            "oldestBlock": format!("{oldest:#x}"),
            "baseFeePerGas": base_fees,
            "gasUsedRatio": ratios,
        });
        if let Some(rewards) = rewards {
            out["reward"] = rewards;
        }
        Ok(out)
    }

    fn send_raw_transaction(&self, raw: Bytes) -> RpcResult<String> {
        self.backend
            .submit_raw(raw)
            .map(|hash| format!("{hash:#x}"))
            .map_err(|e| Self::rejected(e.to_string()))
    }

    fn get_transaction_receipt(&self, hash: B256) -> RpcResult<Option<Value>> {
        let Some((location, receipt, outcome)) = self.backend.receipt(hash) else {
            return Ok(None);
        };

        let (from, to, contract_address) = self.backend.read(|state| {
            let bytes = state.tx_bytes.get(&hash).cloned();
            let Some(bytes) = bytes else { return (Address::ZERO, Value::Null, Value::Null) };
            let Ok(envelope) = TxEnvelope::decode_2718(&mut bytes.as_ref()) else {
                return (Address::ZERO, Value::Null, Value::Null);
            };
            use alloy_consensus::transaction::SignerRecoverable;
            let from = envelope.recover_signer().unwrap_or(Address::ZERO);
            match envelope.kind() {
                TxKind::Call(address) => {
                    (from, Value::String(format!("{address:#x}")), Value::Null)
                }
                TxKind::Create => {
                    // The created address is deterministic from sender and
                    // nonce, which is how a deployer finds its contract.
                    let created = from.create(envelope.nonce());
                    (from, Value::Null, Value::String(format!("{created:#x}")))
                }
            }
        });

        // Gas used by *this* transaction: the difference between its cumulative
        // total and the previous one's.
        let previous_cumulative = if location.index == 0 {
            0
        } else {
            outcome.receipts.get(location.index - 1).map_or(0, |r| r.receipt.cumulative_gas_used)
        };
        let gas_used = receipt.receipt.cumulative_gas_used.saturating_sub(previous_cumulative);

        let effective_gas_price = outcome.base_fee_per_gas;
        let blue_depth = self.backend.read(|state| {
            let tip = state.dag.virtual_selected_parent();
            let tip_score = state.dag.data(tip).map_or(0, |d| d.blue_score);
            let block_score = state.dag.data(outcome.hash).map_or(0, |d| d.blue_score);
            tip_score.saturating_sub(block_score)
        });

        let logs: Vec<Value> = receipt
            .receipt
            .logs
            .iter()
            .enumerate()
            .map(|(log_index, log)| {
                json!({
                    "address": format!("{:#x}", log.address),
                    "topics": log.topics().iter().map(|t| format!("{t:#x}")).collect::<Vec<_>>(),
                    "data": format!("0x{}", alloy_primitives::hex::encode(&log.data.data)),
                    "blockHash": format!("{:#x}", outcome.hash),
                    "blockNumber": format!("{:#x}", location.height),
                    "transactionHash": format!("{hash:#x}"),
                    "transactionIndex": format!("{:#x}", location.index),
                    "logIndex": format!("{log_index:#x}"),
                    "removed": false,
                })
            })
            .collect();

        Ok(Some(json!({
            "transactionHash": format!("{hash:#x}"),
            "transactionIndex": format!("{:#x}", location.index),
            "blockHash": format!("{:#x}", outcome.hash),
            "blockNumber": format!("{:#x}", location.height),
            "from": format!("{from:#x}"),
            "to": to,
            "cumulativeGasUsed": format!("{:#x}", receipt.receipt.cumulative_gas_used),
            "gasUsed": format!("{gas_used:#x}"),
            "contractAddress": contract_address,
            "logs": logs,
            "logsBloom": format!("{:#x}", receipt.logs_bloom),
            "status": if receipt.receipt.status.coerce_status() { "0x1" } else { "0x0" },
            "effectiveGasPrice": format!("{effective_gas_price:#x}"),
            "type": "0x2",
            // Blue-score depth: the DAG's answer to "how buried is this".
            // Additive, so clients that do not know about it ignore it.
            "blueScoreDepth": format!("{blue_depth:#x}"),
        })))
    }

    fn get_transaction_by_hash(&self, hash: B256) -> RpcResult<Option<Value>> {
        self.backend.read(|state| {
            let Some(bytes) = state.tx_bytes.get(&hash) else { return Ok(None) };
            let Ok(envelope) = TxEnvelope::decode_2718(&mut bytes.as_ref()) else {
                return Ok(None);
            };
            let location = state.tx_index.get(&hash);
            Ok(Some(render_transaction(
                &envelope,
                location.map(|l| l.chain_block),
                location.map(|l| l.height),
                location.map(|l| l.index),
            )))
        })
    }

    fn get_block_by_number(&self, block: Value, full: Option<bool>) -> RpcResult<Option<Value>> {
        let height = self.resolve_height(Some(&block))?;
        Ok(self.render_block(height, full.unwrap_or(false)))
    }

    fn get_block_by_hash(&self, hash: B256, full: Option<bool>) -> RpcResult<Option<Value>> {
        let height = self.backend.read(|state| {
            (0..=state.executor.height())
                .find(|h| state.executor.result_at(*h).is_some_and(|r| r.hash == hash))
        });
        Ok(height.and_then(|h| self.render_block(h, full.unwrap_or(false))))
    }

    fn accounts(&self) -> RpcResult<Vec<Address>> {
        // This node holds no keys. Returning an empty list is the honest
        // answer and is what every client expects from a non-custodial node.
        Ok(Vec::new())
    }

    fn syncing(&self) -> RpcResult<bool> {
        Ok(false)
    }

    fn get_logs(&self, filter: Value) -> RpcResult<Vec<Value>> {
        let tip = self.backend.height();
        let from = self.resolve_height(filter.get("fromBlock"))?.min(tip);
        let to = self.resolve_height(filter.get("toBlock"))?.min(tip);
        if from > to {
            return Err(Self::invalid_params("fromBlock is after toBlock"));
        }

        // Scanning every block in the range is O(range). Adequate for a
        // testnet and for the ranges wallets actually request; a log index is
        // the obvious improvement and is not built. OPEN-PROBLEMS.md P-015.
        const MAX_RANGE: u64 = 10_000;
        if to - from > MAX_RANGE {
            return Err(Self::invalid_params(format!(
                "block range {} exceeds the {MAX_RANGE} block limit",
                to - from
            )));
        }

        let wanted_addresses = parse_addresses(filter.get("address"));
        let wanted_topics = parse_topics(filter.get("topics"));

        let mut out = Vec::new();
        self.backend.read(|state| {
            for height in from..=to {
                let Some(outcome) = state.executor.result_at(height) else { continue };
                let mut log_index = 0usize;
                for (tx_index, receipt) in outcome.receipts.iter().enumerate() {
                    let tx_hash =
                        outcome.transaction_hashes.get(tx_index).copied().unwrap_or(B256::ZERO);
                    for log in &receipt.receipt.logs {
                        let this_index = log_index;
                        log_index += 1;

                        if !wanted_addresses.is_empty() && !wanted_addresses.contains(&log.address)
                        {
                            continue;
                        }
                        if !topics_match(&wanted_topics, log.topics()) {
                            continue;
                        }

                        out.push(json!({
                            "address": format!("{:#x}", log.address),
                            "topics": log.topics()
                                .iter()
                                .map(|t| format!("{t:#x}"))
                                .collect::<Vec<_>>(),
                            "data": format!("0x{}",
                                alloy_primitives::hex::encode(&log.data.data)),
                            "blockHash": format!("{:#x}", outcome.hash),
                            "blockNumber": format!("{height:#x}"),
                            "transactionHash": format!("{tx_hash:#x}"),
                            "transactionIndex": format!("{tx_index:#x}"),
                            "logIndex": format!("{this_index:#x}"),
                            "removed": false,
                        }));
                    }
                }
            }
        });
        Ok(out)
    }
}

/// Parses a filter's `address` field, which may be absent, one address, or a
/// list.
fn parse_addresses(value: Option<&Value>) -> Vec<Address> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(one)) => one.parse().into_iter().collect(),
        Some(Value::Array(many)) => {
            many.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect()
        }
        _ => Vec::new(),
    }
}

/// Parses a filter's `topics` field into per-position constraints.
///
/// `None` at a position means "any". A position may list alternatives.
fn parse_topics(value: Option<&Value>) -> Vec<Option<Vec<B256>>> {
    let Some(Value::Array(positions)) = value else { return Vec::new() };
    positions
        .iter()
        .map(|position| match position {
            Value::Null => None,
            Value::String(one) => one.parse().ok().map(|t| vec![t]),
            Value::Array(many) => {
                Some(many.iter().filter_map(|v| v.as_str().and_then(|s| s.parse().ok())).collect())
            }
            _ => None,
        })
        .collect()
}

/// True if a log's topics satisfy the filter's per-position constraints.
fn topics_match(wanted: &[Option<Vec<B256>>], actual: &[B256]) -> bool {
    for (position, constraint) in wanted.iter().enumerate() {
        let Some(allowed) = constraint else { continue };
        let Some(topic) = actual.get(position) else { return false };
        if !allowed.contains(topic) {
            return false;
        }
    }
    true
}

/// The largest gas limit a single transaction may declare.
///
/// EIP-7825, activated in Osaka: 2^24 gas. Lower than the block gas limit, so
/// a caller who omits `gas` must be given this rather than the block limit, or
/// the EVM rejects the transaction before it runs.
pub const TX_GAS_LIMIT_CAP: u64 = alloy_eips::eip7825::MAX_TX_GAS_LIMIT_OSAKA;

/// `gasUsedRatio` for one block, as `eth_feeHistory` reports it.
///
/// The JSON-RPC schema defines this field as a float, so producing one is not
/// optional. The workspace denies floating-point arithmetic because no
/// consensus value may depend on it; this is **presentation only** — it is
/// computed from already-final integers, is never fed back into state, and
/// changing it could not fork the chain. The exception is scoped to this one
/// function rather than relaxed at the lint level, so the rule stays absolute
/// everywhere it matters.
#[allow(
    clippy::float_arithmetic,
    reason = "eth_feeHistory's gasUsedRatio is a float in the JSON-RPC schema; \
              this is RPC presentation and never reaches consensus"
)]
fn gas_used_ratio(gas_used: u64, gas_limit: u64) -> Value {
    if gas_limit == 0 {
        return Value::from(0);
    }
    serde_json::Number::from_f64(gas_used as f64 / gas_limit as f64)
        .map_or(Value::from(0), Value::Number)
}

/// Suggested priority fee, in wei per gas.
///
/// One gwei. High enough to be included promptly on an uncongested chain, low
/// enough not to overpay. A real implementation would derive this from recent
/// blocks; this is a testnet constant and says so.
pub const SUGGESTED_TIP_WEI: u64 = 1_000_000_000;

/// Guard against an unused-import warning for the error code constant.
const _: i32 = RESOURCE_NOT_FOUND;

/// Guard against an unused-import warning for `Output`.
const _: Option<Output> = None;

/// Guard for `BlockNumberOrTag`, re-exported for callers that build requests.
pub use alloy_rpc_types_eth::BlockNumberOrTag as BlockTag;
const _: Option<BlockNumberOrTag> = None;
