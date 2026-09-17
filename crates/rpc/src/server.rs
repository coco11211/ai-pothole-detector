//! The JSON-RPC server.

use std::net::SocketAddr;

use jsonrpsee::{RpcModule, server::ServerBuilder};
use tracing::info;

use crate::{
    backend::Backend,
    dag_api::{DagApiImpl, DagApiServer},
    eth::{CLIENT_VERSION, EthApiImpl, EthApiServer},
};

/// A running server. Dropping this stops it.
#[derive(Debug)]
pub struct RpcServerHandle {
    addr: SocketAddr,
    handle: jsonrpsee::server::ServerHandle,
}

impl RpcServerHandle {
    /// The address actually bound, which matters when port 0 was requested.
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The HTTP endpoint, as a client would be configured with it.
    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stops the server.
    pub fn stop(self) {
        let _ = self.handle.stop();
    }
}

/// Starts the RPC server on `addr`.
pub async fn serve(backend: Backend, addr: SocketAddr) -> std::io::Result<RpcServerHandle> {
    let server = ServerBuilder::default().build(addr).await?;
    let addr = server.local_addr()?;

    let mut module = RpcModule::new(());
    module
        .merge(EthApiImpl::new(backend.clone()).into_rpc())
        .expect("eth namespace has no name collisions");
    module
        .merge(DagApiImpl::new(backend.clone()).into_rpc())
        .expect("chainname namespace has no name collisions");

    // `web3_*` and `net_*` are tiny but not optional: wallets call them during
    // connection setup and treat a failure as the node being unreachable.
    module
        .register_method("web3_clientVersion", |_, _, _| CLIENT_VERSION.to_string())
        .expect("unique");
    let chain_id = backend.params().chain_id;
    module.register_method("net_version", move |_, _, _| chain_id.to_string()).expect("unique");
    module.register_method("net_listening", |_, _, _| true).expect("unique");
    module.register_method("net_peerCount", |_, _, _| "0x0".to_string()).expect("unique");

    let handle = server.start(module);
    info!(%addr, "CHAINNAME JSON-RPC listening");

    Ok(RpcServerHandle { addr, handle })
}
