//! CHAINNAME consensus rules: block validation and mining.
//!
//! At M3 this covers a single node building and validating its own chain, with
//! one parent per block. GHOSTDAG and multi-parent blocks arrive at M4; the
//! rules here are the ones that do not depend on DAG structure, so they carry
//! forward unchanged.

pub mod genesis;
pub mod miner;
pub mod validate;

pub use genesis::{GenesisConfig, genesis_header};
pub use miner::{MineOutcome, Miner};
pub use validate::{ValidationError, check_pow, validate_header};
