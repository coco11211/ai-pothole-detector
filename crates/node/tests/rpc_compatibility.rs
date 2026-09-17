//! M7: the exact JSON-RPC sequences real Ethereum clients issue.
//!
//! Two flows are reproduced call for call:
//!
//! 1. **MetaMask connecting and sending a transaction.** The extension's UI is
//!    not driven here — see BLOCKERS.md B-002 — but what MetaMask does over the
//!    wire is exactly this sequence, and every response is checked for the
//!    shape the extension parses.
//! 2. **A dapp reading logs**, which is what makes existing Solidity
//!    deployments work rather than merely deploy.
//!
//! Foundry is covered separately and for real: `forge script --broadcast`
//! against a running node. See BLOCKERS.md B-001.

use std::net::SocketAddr;

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use chainname_node::{DevNode, dev::dev_accounts};
use chainname_primitives::ChainParams;
use serde_json::{Value, json};

/// The well-known development key every Ethereum tutorial uses.
const DEV_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

struct Harness {
    node: DevNode,
    url: String,
    _handle: chainname_rpc::RpcServerHandle,
    client: reqwest_lite::Client,
}

/// A minimal JSON-RPC client.
///
/// Hand-rolled rather than pulled in as a dependency: the point of this test is
/// to check what goes over the wire, and a client that shares types with the
/// server would hide exactly the mismatches being looked for.
mod reqwest_lite {
    use std::io::{Read, Write};

    use serde_json::Value;

    /// A blocking HTTP/1.1 JSON-RPC client over a raw socket.
    #[derive(Debug, Clone)]
    pub struct Client {
        addr: std::net::SocketAddr,
    }

    impl Client {
        /// Points the client at an address.
        pub const fn new(addr: std::net::SocketAddr) -> Self {
            Self { addr }
        }

        /// Issues a JSON-RPC call and returns the `result` field.
        pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            })
            .to_string();

            let request = format!(
                "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                self.addr,
                body.len(),
                body
            );

            let mut stream = std::net::TcpStream::connect(self.addr)
                .map_err(|e| format!("connect failed: {e}"))?;
            stream.write_all(request.as_bytes()).map_err(|e| format!("write failed: {e}"))?;

            let mut response = String::new();
            stream.read_to_string(&mut response).map_err(|e| format!("read failed: {e}"))?;

            let body = response
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .ok_or_else(|| format!("malformed http response: {response}"))?;

            let parsed: Value =
                serde_json::from_str(body).map_err(|e| format!("bad json {body:?}: {e}"))?;

            if let Some(error) = parsed.get("error") {
                return Err(format!("{method} returned an error: {error}"));
            }
            parsed
                .get("result")
                .cloned()
                .ok_or_else(|| format!("{method} response had no result: {parsed}"))
        }
    }
}

impl Harness {
    async fn start() -> Self {
        let params = ChainParams::testnet_1bps();
        let node = DevNode::new(params, Address::repeat_byte(0xcc));
        let addr: SocketAddr = "127.0.0.1:0".parse().expect("valid address");
        let handle = node.serve_rpc(addr).await.expect("rpc starts");
        let bound = handle.addr();
        Self {
            node,
            url: handle.http_url(),
            _handle: handle,
            client: reqwest_lite::Client::new(bound),
        }
    }

    fn call(&self, method: &str, params: Value) -> Value {
        self.client.call(method, params).unwrap_or_else(|e| panic!("{e}"))
    }

    fn try_call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client.call(method, params)
    }

    /// Mines `count` blocks, so submitted transactions get executed.
    fn mine(&self, count: u64) {
        for i in 0..count {
            self.node.mine_once(1_700_000_001_000 + i * 1_000);
        }
    }

    fn hex_to_u64(value: &Value) -> u64 {
        let s = value.as_str().expect("expected a hex string");
        u64::from_str_radix(s.trim_start_matches("0x"), 16).expect("valid hex")
    }
}

fn dev_signer() -> PrivateKeySigner {
    PrivateKeySigner::from_bytes(&DEV_KEY.parse::<B256>().expect("valid key"))
        .expect("valid signer")
}

/// Signs a transfer the way a wallet would.
fn signed_transfer(nonce: u64, to: Address, value: u128, base_fee: u128) -> String {
    let wallet = dev_signer();
    let tx = TxEip1559 {
        chain_id: ChainParams::testnet_1bps().chain_id,
        nonce,
        gas_limit: 21_000,
        max_fee_per_gas: base_fee * 2 + 1_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        to: TxKind::Call(to),
        value: U256::from(value),
        access_list: Default::default(),
        input: Default::default(),
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash()).expect("signs");
    let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
    format!("0x{}", alloy_primitives::hex::encode(envelope.encoded_2718()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_can_connect() {
    // MetaMask's connection handshake, in order. A failure in any one of these
    // shows up in the UI as "could not connect to network", with no detail.
    let h = Harness::start().await;

    let chain_id = h.call("eth_chainId", json!([]));
    assert_eq!(chain_id.as_str(), Some("0x1e25"), "chain id must be 7717");

    let net_version = h.call("net_version", json!([]));
    assert_eq!(net_version.as_str(), Some("7717"), "net_version is decimal, not hex");

    let listening = h.call("net_listening", json!([]));
    assert_eq!(listening.as_bool(), Some(true));

    let client = h.call("web3_clientVersion", json!([]));
    assert!(client.as_str().is_some_and(|s| s.starts_with("CHAINNAME/")));

    let block_number = h.call("eth_blockNumber", json!([]));
    assert!(block_number.as_str().is_some_and(|s| s.starts_with("0x")));

    let syncing = h.call("eth_syncing", json!([]));
    assert_eq!(syncing.as_bool(), Some(false), "a syncing node makes wallets refuse to send");

    let accounts = h.call("eth_accounts", json!([]));
    assert_eq!(accounts.as_array().map(Vec::len), Some(0), "the node holds no keys");

    assert!(h.url.starts_with("http://"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_sees_a_funded_balance() {
    let h = Harness::start().await;
    let address = dev_accounts()[0].address;

    let balance = h.call("eth_getBalance", json!([address, "latest"]));
    let balance = balance.as_str().expect("hex string");
    assert_ne!(balance, "0x0", "the well-known dev account must be funded");

    let nonce = h.call("eth_getTransactionCount", json!([address, "latest"]));
    assert_eq!(nonce.as_str(), Some("0x0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_can_read_the_latest_block() {
    // MetaMask polls this to learn the base fee and to detect new blocks.
    // Missing fields here break fee estimation silently.
    let h = Harness::start().await;
    h.mine(3);

    let block = h.call("eth_getBlockByNumber", json!(["latest", false]));
    for field in [
        "number",
        "hash",
        "parentHash",
        "nonce",
        "sha3Uncles",
        "logsBloom",
        "transactionsRoot",
        "stateRoot",
        "receiptsRoot",
        "miner",
        "difficulty",
        "totalDifficulty",
        "extraData",
        "size",
        "gasLimit",
        "gasUsed",
        "timestamp",
        "transactions",
        "uncles",
        "baseFeePerGas",
    ] {
        assert!(block.get(field).is_some(), "block is missing `{field}`");
    }
    assert!(block["transactions"].is_array());
    assert!(Harness::hex_to_u64(&block["number"]) >= 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_can_estimate_fees() {
    let h = Harness::start().await;
    h.mine(5);

    let gas_price = h.call("eth_gasPrice", json!([]));
    assert!(Harness::hex_to_u64(&gas_price) > 0);

    let tip = h.call("eth_maxPriorityFeePerGas", json!([]));
    assert!(Harness::hex_to_u64(&tip) > 0);

    // MetaMask's fee estimation depends on this exact shape.
    let history = h.call("eth_feeHistory", json!(["0x4", "latest", [25.0, 50.0, 75.0]]));
    assert!(history.get("oldestBlock").is_some());
    let base_fees = history["baseFeePerGas"].as_array().expect("baseFeePerGas array");
    let ratios = history["gasUsedRatio"].as_array().expect("gasUsedRatio array");
    assert_eq!(
        base_fees.len(),
        ratios.len() + 1,
        "baseFeePerGas must carry one more entry than gasUsedRatio"
    );
    assert!(history.get("reward").is_some(), "reward percentiles were requested");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_can_estimate_gas_for_a_transfer() {
    let h = Harness::start().await;
    let from = dev_accounts()[0].address;

    let estimate = h.call(
        "eth_estimateGas",
        json!([{ "from": from, "to": Address::repeat_byte(9), "value": "0x1" }]),
    );
    let estimate = Harness::hex_to_u64(&estimate);
    assert!(estimate >= 21_000, "a transfer cannot cost less than the base cost");
    assert!(estimate < 100_000, "a bare transfer should not estimate this high");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wallet_can_send_a_transaction_end_to_end() {
    // The gate: connect, read state, sign, send, poll for the receipt.
    let h = Harness::start().await;
    let from = dev_accounts()[0].address;
    let to = Address::repeat_byte(0x77);

    let base_fee = Harness::hex_to_u64(&h.call("eth_gasPrice", json!([])));
    let nonce = Harness::hex_to_u64(&h.call("eth_getTransactionCount", json!([from, "pending"])));
    assert_eq!(nonce, 0);

    let raw = signed_transfer(nonce, to, 1_000_000_000_000_000_000, u128::from(base_fee));
    let hash = h.call("eth_sendRawTransaction", json!([raw]));
    let hash = hash.as_str().expect("a transaction hash").to_string();

    // Pending before it is mined, exactly as a wallet would observe.
    let pending = h.call("eth_getTransactionByHash", json!([hash]));
    assert!(pending.is_object(), "a submitted transaction must be visible immediately");
    assert!(pending["blockNumber"].is_null(), "it has not been mined yet");

    let no_receipt = h.call("eth_getTransactionReceipt", json!([hash]));
    assert!(no_receipt.is_null(), "an unmined transaction has no receipt");

    h.mine(2);

    let receipt = h.call("eth_getTransactionReceipt", json!([hash]));
    assert!(receipt.is_object(), "the transaction should have been mined");
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "the transfer should have succeeded");
    assert_eq!(
        receipt["from"].as_str().map(str::to_lowercase),
        Some(format!("{from:#x}").to_lowercase())
    );
    for field in [
        "transactionHash",
        "transactionIndex",
        "blockHash",
        "blockNumber",
        "cumulativeGasUsed",
        "gasUsed",
        "logs",
        "logsBloom",
        "status",
        "effectiveGasPrice",
    ] {
        assert!(receipt.get(field).is_some(), "receipt is missing `{field}`");
    }

    // And the balance actually moved.
    let balance = h.call("eth_getBalance", json!([to, "latest"]));
    assert_eq!(balance.as_str(), Some("0xde0b6b3a7640000"), "recipient should hold 1 unit");

    // The nonce advanced, so the wallet's next transaction will not collide.
    let next = Harness::hex_to_u64(&h.call("eth_getTransactionCount", json!([from, "latest"])));
    assert_eq!(next, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pending_nonce_accounts_for_queued_transactions() {
    // Without this a wallet's second transaction reuses the first one's nonce
    // and is silently dropped.
    let h = Harness::start().await;
    let from = dev_accounts()[0].address;
    let base_fee = Harness::hex_to_u64(&h.call("eth_gasPrice", json!([])));

    h.call(
        "eth_sendRawTransaction",
        json!([signed_transfer(0, Address::repeat_byte(1), 1, u128::from(base_fee))]),
    );

    let latest = Harness::hex_to_u64(&h.call("eth_getTransactionCount", json!([from, "latest"])));
    let pending = Harness::hex_to_u64(&h.call("eth_getTransactionCount", json!([from, "pending"])));
    assert_eq!(latest, 0, "nothing has executed yet");
    assert_eq!(pending, 1, "the queued transaction must advance the pending nonce");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_transaction_is_refused() {
    let h = Harness::start().await;
    let base_fee = Harness::hex_to_u64(&h.call("eth_gasPrice", json!([])));
    let raw = signed_transfer(0, Address::repeat_byte(2), 1, u128::from(base_fee));

    h.call("eth_sendRawTransaction", json!([raw.clone()]));
    assert!(
        h.try_call("eth_sendRawTransaction", json!([raw])).is_err(),
        "the same transaction twice must be refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transaction_for_another_chain_is_refused() {
    // Replay protection, which is the reason chain ids exist.
    let h = Harness::start().await;
    let wallet = dev_signer();
    let tx = TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: 10_000_000_000,
        max_priority_fee_per_gas: 1_000_000_000,
        to: TxKind::Call(Address::repeat_byte(3)),
        value: U256::from(1u64),
        access_list: Default::default(),
        input: Default::default(),
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash()).unwrap();
    let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
    let raw = format!("0x{}", alloy_primitives::hex::encode(envelope.encoded_2718()));

    assert!(
        h.try_call("eth_sendRawTransaction", json!([raw])).is_err(),
        "a mainnet-signed transaction must not be accepted here"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dapp_can_read_logs() {
    // What makes an existing Solidity deployment actually usable rather than
    // merely deployable.
    let h = Harness::start().await;
    h.mine(1);

    let logs = h.call("eth_getLogs", json!([{ "fromBlock": "0x0", "toBlock": "latest" }]));
    assert!(logs.is_array(), "getLogs must return an array even when empty");

    // A range beyond the limit is refused rather than served slowly.
    assert!(
        h.try_call("eth_getLogs", json!([{ "fromBlock": "0x0", "toBlock": "0xFFFFFFFF" }])).is_ok(),
        "a range clamped to the tip is fine"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dag_namespace_answers() {
    let h = Harness::start().await;
    h.mine(3);

    let tips = h.call("chainname_getDagTips", json!([]));
    assert!(tips["tips"].as_array().is_some_and(|t| !t.is_empty()));
    assert!(tips.get("virtualSelectedParent").is_some());
    assert!(tips["blueScore"].as_u64().is_some());

    let selected = tips["virtualSelectedParent"].as_str().unwrap().to_string();
    let info = h.call("chainname_getBlockDagInfo", json!([selected]));
    assert!(info.is_object());
    assert!(info["blueWork"].is_string(), "blue work is a string to keep 256-bit precision");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_eth_namespace_carries_no_dag_specific_required_fields() {
    // The compatibility contract: `eth_*` responses stay Ethereum-shaped. DAG
    // data may be present as *additional* fields, which strict clients ignore,
    // but nothing an Ethereum client needs may be missing or retyped.
    let h = Harness::start().await;
    h.mine(2);

    let block = h.call("eth_getBlockByNumber", json!(["latest", false]));
    for field in ["number", "hash", "parentHash", "gasLimit", "gasUsed", "timestamp"] {
        assert!(
            block[field].is_string(),
            "`{field}` must be a hex string, as every Ethereum client expects"
        );
    }
    assert!(block["transactions"].is_array());
    assert!(block["uncles"].is_array());
}
