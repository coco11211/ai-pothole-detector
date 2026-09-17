//! Deterministic multi-node simulation for CHAINNAME.
//!
//! This is infrastructure, not a test helper. Every run is a pure function of
//! its seed and configuration: same seed, same event ordering, same DAGs, same
//! divergence if there is one. Nondeterministic consensus failures cannot be
//! debugged, so the harness refuses to produce any.
//!
//! Virtual time. Thirty simulated minutes run in seconds, and no test ever
//! sleeps.

pub mod rng;
pub mod sim;

pub use rng::Lcg;
pub use sim::{LinkQuality, SimConfig, Simulation};
