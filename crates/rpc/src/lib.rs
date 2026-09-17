//! JSON-RPC for CHAINNAME.
//!
//! Two namespaces, and the split between them is deliberate:
//!
//! * **`eth_*` is unchanged in shape.** Every response is the field set an
//!   Ethereum client already expects, so MetaMask, ethers, viem and Foundry
//!   work with no patches. Where a DAG concept has no Ethereum equivalent it is
//!   mapped onto the nearest honest one — `blockNumber` is selected-chain
//!   height, `confirmations` is blue-score depth — rather than bolted onto the
//!   response as an extra field that would break strict clients.
//!
//! * **`chainname_*` carries everything DAG-shaped**: tips, blue score, merge
//!   sets, the deferred state root. Nothing here leaks into `eth_*`.
//!
//! What a "block" means is the one place the mapping is not free. `eth_*`
//! addresses **selected-chain blocks**, because that is the only sequence with
//! one block per height, which is what every Ethereum client assumes. DAG
//! blocks off the selected chain are reachable only through `chainname_*`.

pub mod backend;
pub mod dag_api;
pub mod eth;
pub mod server;

pub use backend::{Backend, BackendError};
pub use server::{RpcServerHandle, serve};
