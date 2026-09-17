//! CHAINNAME difficulty retargeting.
//!
//! ASERT, per block, integer only. **No floating point appears in this crate**
//! and the workspace denies `clippy::float_arithmetic` so it cannot creep in.

pub mod asert;
pub mod compact;

pub use asert::{AsertParams, next_target};
pub use compact::{CompactError, CompactTarget};
