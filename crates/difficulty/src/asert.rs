//! ASERT difficulty retargeting.
//!
//! Absolutely-Scheduled Exponentially Rising Targets, in the "absolute" form
//! used by Bitcoin Cash's `aserti3-2d`: the target is computed from a fixed
//! anchor rather than from the previous block, so retargeting error cannot
//! accumulate and every block's difficulty is independently verifiable from
//! the anchor alone.
//!
//! ```text
//! next_target = anchor_target * 2^((t - t_anchor - ideal * (h - h_anchor)) / halflife)
//! ```
//!
//! `2^x` is evaluated with the Bitcoin Cash specification's integer cubic
//! approximation. The same approximation is the only one in the codebase; the
//! emission curve reuses it (ARCHITECTURE.md §8.2) so there is one function to
//! get right and one set of tests to trust.

use alloy_primitives::{U256, U512};

use crate::compact::CompactTarget;

/// Fixed-point fractional bits used by the `2^x` approximation.
const RADIX_BITS: u32 = 16;
/// `1 << RADIX_BITS`, the fixed-point unit.
const RADIX: i128 = 1 << RADIX_BITS;

// Cubic polynomial coefficients approximating `2^x - 1` for x in [0, 1),
// in 48-bit fixed point. Taken verbatim from the Bitcoin Cash aserti3-2d
// specification; the approximation error is under 0.013%, which is far below
// the granularity that matters for a difficulty target.
const CUBIC_C1: i128 = 195_766_423_245_049;
const CUBIC_C2: i128 = 971_821_376;
const CUBIC_C3: i128 = 5_127;
/// Rounding term: half of `1 << 48`.
const CUBIC_ROUND: i128 = 1 << 47;
/// Shift applied after the cubic polynomial.
const CUBIC_SHIFT: u32 = 48;

/// Bound on how far a single retarget may move the exponent, in half-lives.
///
/// Without this, a timestamp far in the future or past would shift the target
/// by an unbounded number of bits. The clamp is deliberately generous — 16
/// half-lives is a factor of 65536 in either direction, far beyond any honest
/// swing — so it only ever fires on malicious or broken input, and never
/// distorts normal retargeting.
const MAX_HALFLIVES: i128 = 16;

/// Parameters for [`next_target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsertParams {
    /// Target seconds between blocks.
    pub ideal_block_time_secs: i128,
    /// Half-life, in seconds.
    pub halflife_secs: i128,
    /// Easiest permitted target. Difficulty never falls below this.
    pub pow_limit: U256,
}

impl AsertParams {
    /// Parameters for a given block interval.
    ///
    /// `interval_ms` must divide evenly into seconds; `ChainParams::validate`
    /// already guarantees that for every shipped preset.
    pub fn new(ideal_block_time_secs: i128, halflife_secs: i128, pow_limit: U256) -> Self {
        Self { ideal_block_time_secs, halflife_secs, pow_limit }
    }
}

/// Computes the target for a block, from a fixed anchor.
///
/// * `anchor_target` — the anchor block's target.
/// * `height_from_anchor` — blocks since the anchor. Must be >= 1.
/// * `secs_from_anchor` — seconds between the anchor's timestamp and this
///   block's. May be negative if a timestamp moved backwards; the formula
///   handles that by raising difficulty.
///
/// The result is clamped to `[1, pow_limit]` and returned in compact form,
/// because the compact encoding is what the header carries and what every node
/// must agree on bit for bit. Returning a full `U256` here would let callers
/// disagree about rounding.
pub fn next_target(
    params: &AsertParams,
    anchor_target: U256,
    height_from_anchor: u64,
    secs_from_anchor: i128,
) -> CompactTarget {
    debug_assert!(height_from_anchor >= 1, "anchor is not its own successor");

    // How far ahead of (positive) or behind (negative) schedule we are.
    let scheduled = params.ideal_block_time_secs * i128::from(height_from_anchor);
    let drift = secs_from_anchor - scheduled;

    // Fixed-point exponent, in half-lives.
    let exponent = (drift * RADIX) / params.halflife_secs;

    // Split into whole half-lives (a bit shift) and a fraction (the cubic).
    // Arithmetic shift floors toward negative infinity, which is what keeps
    // `frac` in [0, RADIX) for negative exponents too.
    let clamped = exponent.clamp(-MAX_HALFLIVES * RADIX, MAX_HALFLIVES * RADIX);
    let shifts = clamped >> RADIX_BITS;
    let frac = clamped - (shifts << RADIX_BITS);
    debug_assert!((0..RADIX).contains(&frac));

    // 2^frac in 16-bit fixed point, via the cubic approximation.
    let factor = RADIX
        + ((CUBIC_C1 * frac
            + CUBIC_C2 * frac * frac
            + CUBIC_C3 * frac * frac * frac
            + CUBIC_ROUND)
            >> CUBIC_SHIFT);
    debug_assert!((RADIX..=2 * RADIX).contains(&factor));

    // next = anchor_target * factor * 2^shifts / RADIX.
    //
    // Computed in 512 bits. A 256-bit intermediate is not enough, and getting
    // this wrong is silent: `anchor * factor` needs up to 256 + 17 bits, and
    // the left shift adds up to MAX_HALFLIVES more. A *saturating* 256-bit
    // multiply does not rescue it either, because the `>> RADIX_BITS`
    // afterwards pulls the saturated value back under the pow limit, so the
    // clamp never fires and a wrong target is returned as though it were
    // right. That bug was real: a near-maximum anchor retargeted 256x harder
    // at zero drift. Regression test:
    // `near_maximum_anchor_is_unchanged_at_zero_drift`.
    //
    // 512 bits is comfortably sufficient: 256 + 17 + 16 = 289.
    let anchor = U512::from_limbs_slice(anchor_target.as_limbs());
    let mut next = anchor * U512::from(factor as u128);

    if shifts >= 0 {
        next <<= shifts as usize;
    } else {
        next >>= (-shifts) as usize;
    }

    next >>= RADIX_BITS as usize;

    // Back down to 256 bits. Anything that still does not fit is far easier
    // than any sane pow limit, so clamping is the correct answer for it.
    let limit = U512::from_limbs_slice(params.pow_limit.as_limbs());
    let next =
        if next > limit { params.pow_limit } else { U256::from_limbs_slice(next.as_limbs()) };

    let next = if next.is_zero() { U256::from(1u8) } else { next };

    CompactTarget::from_target(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> AsertParams {
        AsertParams::new(
            1,
            2 * 60 * 60,
            // A deliberately easy pow limit so tests are not clamped by it.
            U256::MAX >> 8,
        )
    }

    fn anchor() -> U256 {
        U256::MAX >> 40
    }

    #[test]
    fn on_schedule_leaves_the_target_unchanged() {
        // Exactly on schedule: drift is zero, so the factor is 1.
        let got = next_target(&params(), anchor(), 1000, 1000);
        assert_eq!(got, CompactTarget::from_target(anchor()));
    }

    #[test]
    fn blocks_arriving_too_fast_make_the_target_harder() {
        // 1000 blocks in 500 seconds: twice as fast as scheduled.
        let fast = next_target(&params(), anchor(), 1000, 500).to_target().unwrap();
        assert!(fast < anchor(), "target must shrink when blocks come too fast");
    }

    #[test]
    fn blocks_arriving_too_slow_make_the_target_easier() {
        let slow = next_target(&params(), anchor(), 1000, 2000).to_target().unwrap();
        assert!(slow > anchor(), "target must grow when blocks come too slowly");
    }

    #[test]
    fn one_halflife_behind_schedule_doubles_the_target() {
        let p = params();
        // Drift of exactly one half-life: the target must double.
        let drift = p.halflife_secs;
        let got = next_target(&p, anchor(), 1000, 1000 + drift).to_target().unwrap();
        let expected = anchor() * U256::from(2u8);
        let tolerance = expected / U256::from(1000u32);
        assert!(
            got.abs_diff(expected) <= tolerance,
            "expected ~{expected}, got {got}: cubic approximation is off by more than 0.1%"
        );
    }

    #[test]
    fn one_halflife_ahead_of_schedule_halves_the_target() {
        let p = params();
        let drift = p.halflife_secs;
        let got = next_target(&p, anchor(), 10_000, 10_000 - drift).to_target().unwrap();
        let expected = anchor() / U256::from(2u8);
        let tolerance = expected / U256::from(1000u32);
        assert!(got.abs_diff(expected) <= tolerance, "expected ~{expected}, got {got}");
    }

    #[test]
    fn extreme_future_timestamps_are_clamped_not_overflowing() {
        // A timestamp centuries ahead must not panic or wrap. MAX_HALFLIVES
        // caps the shift at 2^16, so the result is the anchor scaled by that
        // and nothing larger -- proving the clamp fired rather than the
        // arithmetic saturating or wrapping.
        let p = params();
        let got = next_target(&p, anchor(), 1, i128::from(u32::MAX)).to_target().unwrap();
        let capped = anchor() << (MAX_HALFLIVES as usize);
        let tolerance = capped / U256::from(1000u32);
        assert!(got.abs_diff(capped) <= tolerance, "expected ~{capped}, got {got}");
        assert!(got <= p.pow_limit);
    }

    #[test]
    fn the_pow_limit_still_binds_when_it_is_tighter_than_the_clamp() {
        // With a pow limit below the MAX_HALFLIVES ceiling, the limit wins.
        let p = AsertParams::new(1, 2 * 60 * 60, anchor() * U256::from(4u8));
        let got = next_target(&p, anchor(), 1, i128::from(u32::MAX)).to_target().unwrap();
        assert!(got <= p.pow_limit);
    }

    #[test]
    fn extreme_past_timestamps_clamp_to_maximum_difficulty() {
        // Enormously negative drift drives the target down, never below 1.
        let got = next_target(&params(), anchor(), 1, -i128::from(u32::MAX));
        let target = got.to_target().unwrap();
        assert!(target >= U256::from(1u8), "target must never reach zero");
        assert!(target < anchor());
    }

    #[test]
    fn target_never_exceeds_the_pow_limit() {
        let p = params();
        let got = next_target(&p, p.pow_limit, 1, 1_000_000).to_target().unwrap();
        assert!(got <= p.pow_limit);
    }

    #[test]
    fn target_never_reaches_zero() {
        let p = params();
        let got = next_target(&p, U256::from(1u8), 100_000, -1_000_000).to_target().unwrap();
        assert!(got >= U256::from(1u8));
    }

    #[test]
    fn near_maximum_anchor_is_unchanged_at_zero_drift() {
        // Regression: the 256-bit intermediate overflowed here and the
        // saturating multiply hid it. An anchor this close to the top of the
        // range must round-trip exactly when the chain is on schedule.
        let anchor = U256::MAX >> 8;
        let p = AsertParams::new(1, 2 * 60 * 60, U256::MAX);
        assert_eq!(next_target(&p, anchor, 1_000, 1_000), CompactTarget::from_target(anchor));
    }

    #[test]
    fn near_maximum_anchor_still_halves_correctly() {
        let anchor = U256::MAX >> 8;
        let p = AsertParams::new(1, 2 * 60 * 60, U256::MAX);
        let got = next_target(&p, anchor, 10_000, 10_000 - p.halflife_secs).to_target().unwrap();
        let expected = anchor / U256::from(2u8);
        let tolerance = expected / U256::from(1000u32);
        assert!(got.abs_diff(expected) <= tolerance, "expected ~{expected}, got {got}");
    }

    #[test]
    fn maximum_anchor_does_not_overflow() {
        // The most extreme input the type allows, with the largest upward
        // shift. Must clamp, never wrap.
        let p = AsertParams::new(1, 2 * 60 * 60, U256::MAX);
        let got = next_target(&p, U256::MAX, 1, i128::from(u32::MAX)).to_target().unwrap();
        assert!(got <= p.pow_limit);
    }

    #[test]
    fn is_deterministic() {
        let p = params();
        assert_eq!(
            next_target(&p, anchor(), 12_345, 11_000),
            next_target(&p, anchor(), 12_345, 11_000)
        );
    }
}
