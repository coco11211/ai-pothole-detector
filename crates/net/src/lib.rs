//! CHAINNAME peer-to-peer networking.
//!
//! Flood relay over a DAG-shaped wire protocol, with no stake weighting
//! anywhere. Peers earn standing by behaving, not by holding anything.
//!
//! The protocol logic is a **pure state machine**: [`sync::DagSync`] takes a
//! message and returns a list of [`sync::Action`]s. It performs no I/O, owns no
//! clock, and holds no sockets. That is what lets the multi-node harness drive
//! five nodes through thirty simulated minutes deterministically, and it is
//! what makes a divergence reproducible from a seed instead of unrepeatable.

pub mod message;
pub mod p2p;
pub mod peer;
pub mod sync;
pub mod transport;

pub use message::{
    BlockPayload, CodecError, MAX_BLOCK_BATCH, MAX_INV_ENTRIES, MAX_TXS_PER_BLOCK, Message,
    PROTOCOL_VERSION,
};
pub use p2p::{P2pConfig, P2pNode, announce_local_block};
pub use peer::{Misbehaviour, PeerId, PeerState};
pub use sync::{Action, DagSync, SyncConfig};
pub use transport::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
