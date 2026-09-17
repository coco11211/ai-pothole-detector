//! Block validation.
//!
//! Split into checks that need no context (structure, proof of work) and
//! checks that need the chain (difficulty, timestamps), so a peer's garbage is
//! rejected at the cheapest possible point.

use alloy_primitives::{B256, U256};
use chainname_difficulty::{AsertParams, CompactError, CompactTarget, next_target};
use chainname_pow::PowHash;
use chainname_primitives::{Header, HeaderError};

/// How far into the future a block's timestamp may be, in milliseconds.
///
/// Two minutes. A block claiming a future timestamp makes the chain look
/// behind schedule, which ASERT answers by *lowering* difficulty — so an
/// unbounded future timestamp is a difficulty attack, not a cosmetic problem.
/// Two minutes is far above any plausible clock skew between honest nodes and
/// far below the ASERT half-life, so honest blocks are never rejected and the
/// attack is bounded to a negligible nudge.
pub const MAX_FUTURE_DRIFT_MS: u64 = 2 * 60 * 1_000;

/// Everything validation needs to know about a block's position in the chain.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    /// Height of this block, counted from genesis.
    pub height: u64,
    /// Genesis timestamp, in milliseconds. The ASERT anchor.
    pub anchor_timestamp_ms: u64,
    /// Genesis target. The ASERT anchor.
    pub anchor_target: U256,
    /// Timestamp of the selected parent, in milliseconds.
    ///
    /// Blocks must not move time backwards relative to their selected parent.
    pub parent_timestamp_ms: u64,
    /// Current wall-clock time, in milliseconds, for the future-drift check.
    pub now_ms: u64,
    /// ASERT parameters.
    pub asert: AsertParams,
}

/// Validates a header completely.
pub fn validate_header(
    header: &Header,
    ctx: &ValidationContext,
    pow: &dyn PowHash,
) -> Result<(), ValidationError> {
    header.validate_structure()?;

    if header.timestamp_ms > ctx.now_ms.saturating_add(MAX_FUTURE_DRIFT_MS) {
        return Err(ValidationError::TimestampTooFarInFuture {
            timestamp_ms: header.timestamp_ms,
            limit_ms: ctx.now_ms.saturating_add(MAX_FUTURE_DRIFT_MS),
        });
    }

    if header.timestamp_ms < ctx.parent_timestamp_ms {
        return Err(ValidationError::TimestampWentBackwards {
            timestamp_ms: header.timestamp_ms,
            parent_timestamp_ms: ctx.parent_timestamp_ms,
        });
    }

    let expected = expected_bits(ctx, header.timestamp_ms);
    if header.bits != expected.to_u32() {
        return Err(ValidationError::WrongDifficulty {
            found: header.bits,
            expected: expected.to_u32(),
        });
    }

    check_pow(header, pow)?;
    Ok(())
}

/// The difficulty a block at this position, with this timestamp, must carry.
///
/// Difficulty is a function of the block's own timestamp, so it cannot be
/// chosen by the miner independently of the time it claims.
pub fn expected_bits(ctx: &ValidationContext, timestamp_ms: u64) -> CompactTarget {
    let elapsed_secs = (i128::from(timestamp_ms) - i128::from(ctx.anchor_timestamp_ms)) / 1_000;
    next_target(&ctx.asert, ctx.anchor_target, ctx.height, elapsed_secs)
}

/// Checks that the header's proof of work meets its declared target.
pub fn check_pow(header: &Header, pow: &dyn PowHash) -> Result<(), ValidationError> {
    let target = CompactTarget(header.bits).to_target()?;
    let hash = pow.hash_header(header);
    if U256::from_be_bytes(hash.0) > target {
        return Err(ValidationError::InsufficientWork { hash, bits: header.bits });
    }
    Ok(())
}

/// Reasons a block is invalid.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    /// The header is structurally malformed.
    #[error(transparent)]
    Structure(#[from] HeaderError),
    /// The declared target could not be decoded.
    #[error(transparent)]
    Target(#[from] CompactError),
    /// The proof-of-work hash does not meet the declared target.
    #[error("insufficient work: hash {hash} does not meet target {bits:#010x}")]
    InsufficientWork {
        /// The proof-of-work hash produced.
        hash: B256,
        /// The declared compact target.
        bits: u32,
    },
    /// The declared difficulty is not the one the retarget rule requires.
    #[error("wrong difficulty: header says {found:#010x}, rule requires {expected:#010x}")]
    WrongDifficulty {
        /// What the header declared.
        found: u32,
        /// What the retarget rule computed.
        expected: u32,
    },
    /// The timestamp is further ahead than clock skew can explain.
    #[error("timestamp {timestamp_ms}ms is beyond the accepted limit {limit_ms}ms")]
    TimestampTooFarInFuture {
        /// The header's timestamp.
        timestamp_ms: u64,
        /// The latest accepted timestamp.
        limit_ms: u64,
    },
    /// The timestamp precedes the selected parent's.
    #[error("timestamp {timestamp_ms}ms precedes parent's {parent_timestamp_ms}ms")]
    TimestampWentBackwards {
        /// The header's timestamp.
        timestamp_ms: u64,
        /// The selected parent's timestamp.
        parent_timestamp_ms: u64,
    },
}
