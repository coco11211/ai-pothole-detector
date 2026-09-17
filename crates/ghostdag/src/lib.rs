//! GHOSTDAG: blockDAG ordering without a validator set.
//!
//! Implements the colouring and ordering rules from the PHANTOM/GHOSTDAG
//! paper, in the form Kaspa uses. The algorithm is reimplemented rather than
//! ported: Kaspa's is UTXO-shaped and CHAINNAME is account-shaped, so only the
//! DAG logic transfers.
//!
//! The pieces:
//!
//! * [`work`] — proof-of-work accumulation. Selected-parent choice compares
//!   accumulated *work*, never block counts.
//! * [`dag`] — the DAG store, reachability, and the GHOSTDAG colouring.
//! * [`ordering`] — the merge-set base sequence and the deterministic
//!   intra-merge-set sort that feeds execution (ARCHITECTURE.md §6).

pub mod dag;
pub mod k_parameter;
pub mod ordering;
pub mod work;

pub use dag::{DagError, DagStore, GhostdagData};
pub use k_parameter::{calculate_k, calculate_k_default};
pub use ordering::{AccessSet, layer_and_sort};
pub use work::work_for_target;
