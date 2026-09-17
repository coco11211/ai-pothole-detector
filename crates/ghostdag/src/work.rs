//! Proof-of-work accumulation.
//!
//! A block's *work* is the expected number of hashes needed to produce it:
//! `2^256 / (target + 1)`. Bitcoin's formula, computed without a 257-bit
//! intermediate as `(!target) / (target + 1) + 1`.
//!
//! Work, not block count, is what GHOSTDAG compares when choosing a selected
//! parent. Counting blocks would let a miner on an easy target outvote one on
//! a hard target.

use alloy_primitives::U256;

/// Expected hashes required to produce a block at `target`.
///
/// A zero target is unachievable; it yields the maximum work rather than
/// dividing by zero, which keeps the function total.
pub fn work_for_target(target: U256) -> U256 {
    if target.is_zero() {
        return U256::MAX;
    }
    // `target + 1` overflows exactly when target is U256::MAX, where every
    // possible hash satisfies the target: one hash of work. Handled explicitly
    // rather than left to wrap into a division by zero.
    let Some(denominator) = target.checked_add(U256::from(1u8)) else {
        return U256::from(1u8);
    };
    // (2^256 - target - 1) / (target + 1) + 1, i.e. floor(2^256 / (target+1)).
    (!target) / denominator + U256::from(1u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easier_targets_are_less_work() {
        let easy = work_for_target(U256::MAX >> 1);
        let hard = work_for_target(U256::MAX >> 8);
        assert!(hard > easy, "a smaller target must mean more work");
    }

    #[test]
    fn halving_the_target_doubles_the_work() {
        let a = work_for_target(U256::MAX >> 16);
        let b = work_for_target(U256::MAX >> 17);
        // Within one unit of exactly double, allowing for integer rounding.
        assert!(b.abs_diff(a * U256::from(2u8)) <= U256::from(2u8));
    }

    #[test]
    fn maximum_target_is_minimum_work() {
        assert_eq!(work_for_target(U256::MAX), U256::from(1u8));
    }

    #[test]
    fn zero_target_is_total_not_a_panic() {
        assert_eq!(work_for_target(U256::ZERO), U256::MAX);
    }

    #[test]
    fn work_is_monotonic_across_the_whole_range() {
        // Extreme inputs, because that is where the last two arithmetic bugs
        // in this codebase lived. See DECISIONS.md C-007.
        let mut previous = work_for_target(U256::MAX);
        for shift in 1..=255usize {
            let work = work_for_target(U256::MAX >> shift);
            assert!(work >= previous, "work decreased at shift {shift}");
            previous = work;
        }
    }

    #[test]
    fn target_of_one_needs_half_the_space() {
        // target = 1 accepts hashes 0 and 1 out of 2^256, so 2^255 work.
        assert_eq!(work_for_target(U256::from(1u8)), U256::from(1u8) << 255);
    }
}
