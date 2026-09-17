//! The seam: GHOSTDAG ordering feeding revm.
//!
//! This is the crate the whole design turns on. ARCHITECTURE.md §6 and §7
//! describe it; this implements it.
//!
//! # The shape of the problem
//!
//! Sealevel-style parallel execution — and any execution at all — needs a
//! deterministic transaction order. GHOSTDAG produces order by walking a DAG.
//! The two have to meet somewhere, and the meeting point has three
//! requirements that pull against each other:
//!
//! 1. **Determinism.** Every node must derive the same order from the same
//!    blocks, in any arrival sequence.
//! 2. **Completeness.** Every block's transactions execute exactly once,
//!    including blocks that lost the fork-choice race. Discarding orphans
//!    would make the DAG pointless.
//! 3. **Attribution.** Each transaction's fee and `COINBASE` must resolve to
//!    the miner of the DAG block that carried it, not the chain block that
//!    merged it.
//!
//! # How they are met
//!
//! * Determinism: [`ghostdag::ordering`] is a pure function of topology and
//!   block contents. Its properties are property-tested there.
//! * Completeness: execution runs along the selected parent chain only, and
//!   chain block N consumes its whole merge set — blues and reds alike.
//! * Attribution: the EVM's beneficiary is reset *per transaction* via
//!   `ContextSetters::set_block`, which is why this crate drives revm directly
//!   instead of using `alloy-evm`'s block executor (DECISIONS.md C-004).
//!
//! # What lags
//!
//! A DAG block cannot commit to the state after its own execution, because its
//! effect depends on which merge set eventually contains it. Headers therefore
//! carry the state root of a block `D` heights back. See [`executor`].

pub mod bodies;
pub mod executor;
pub mod journal;
pub mod reorg;

pub use bodies::{BodyStore, ChainTx};
pub use executor::{ChainBlockOutcome, ChainExecutor, ExecutionError};
pub use journal::UndoRecord;
pub use reorg::{ChainReorg, compute_reorg};
