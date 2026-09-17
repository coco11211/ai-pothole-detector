//! Choosing GHOSTDAG's `k`.
//!
//! `k` bounds how many blocks may be created inside the network's propagation
//! window before honest miners start building on blocks they have not yet
//! seen. Set it too low and honest blocks are coloured red; too high and an
//! attacker gets a wider window to build a competing blue set.
//!
//! The PHANTOM paper picks it from a Poisson tail. Blocks arrive at rate
//! `lambda`; within a window of `2 * delay` seconds the expected count is
//! `mu = 2 * delay * lambda`; `k` is the smallest value whose upper tail is at
//! most the failure probability `delta`:
//!
//! ```text
//! k = min { k : P[X > k] <= delta },  X ~ Poisson(2 * delay * lambda)
//! ```
//!
//! # This is not consensus code
//!
//! This computes a *parameter*, once, at design time. The result is an integer
//! written into [`crate::ChainParams`]; that integer is what consensus uses.
//! Nothing here runs on a node. The floating-point arithmetic below is
//! therefore outside the no-floats rule, and is marked as such at the one
//! function that uses it rather than by relaxing the lint.
//!
//! # Why it must be measured
//!
//! `delay` is a property of a real network, not something to assume. Kaspa's
//! `k = 18` comes from `delay = 5s`, `lambda = 1`, `delta = 0.01`; feeding
//! those in reproduces it exactly, which is what
//! [`tests::reproduces_kaspas_published_parameters`] checks. Raising the block
//! rate without re-measuring `delay` would keep a `k` that no longer holds.

/// Failure probability GHOSTDAG is parameterised against.
///
/// One in a hundred, the figure Kaspa uses. It is the probability that more
/// than `k` blocks appear inside a propagation window, which is when the
/// k-cluster property can be violated by honest behaviour alone.
pub const DEFAULT_DELTA: f64 = 0.01;

/// Largest `k` this will return.
///
/// `k` is a `u16` in the header-validation path and a merge set is bounded by
/// a multiple of it, so an enormous `k` would be a denial-of-service vector in
/// its own right. If the formula wants more than this, the block rate is too
/// high for the measured delay and the answer is to lower the rate, not to
/// raise the bound.
pub const MAX_K: u16 = 1_024;

/// Computes `k` from a measured propagation delay and a block rate.
///
/// * `delay_ms` — the propagation delay bound, in milliseconds. Use a high
///   percentile of measured full-propagation times, not the mean.
/// * `blocks_per_second` — the target block rate.
/// * `delta` — acceptable failure probability.
///
/// Returns `None` if the required `k` exceeds [`MAX_K`], which means the rate
/// is too aggressive for the network being measured.
#[allow(
    clippy::float_arithmetic,
    reason = "design-time parameter derivation, not consensus; the result is an \
              integer constant and nothing here runs on a node"
)]
pub fn calculate_k(delay_ms: u64, blocks_per_second: u64, delta: f64) -> Option<u16> {
    if blocks_per_second == 0 || delta <= 0.0 || delta >= 1.0 {
        return None;
    }

    let delay_secs = delay_ms as f64 / 1_000.0;
    let mu = 2.0 * delay_secs * blocks_per_second as f64;

    // Accumulate the Poisson CDF term by term until the remaining tail is at
    // most delta. Terms are computed by recurrence (t *= mu / j) rather than
    // from factorials, which overflow long before the tail gets small.
    let mut term = (-mu).exp();
    let mut cdf = term;

    for k in 0..=MAX_K {
        if 1.0 - cdf <= delta {
            return Some(k);
        }
        let next = u32::from(k) + 1;
        term *= mu / f64::from(next);
        cdf += term;
    }
    None
}

/// Computes `k` at the default failure probability.
pub fn calculate_k_default(delay_ms: u64, blocks_per_second: u64) -> Option<u16> {
    calculate_k(delay_ms, blocks_per_second, DEFAULT_DELTA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduces_kaspas_published_parameters() {
        // Kaspa runs k = 18 at one block per second. Its stated delay bound is
        // 5 seconds and its delta is 0.01. If this formula is right, those
        // three inputs must produce exactly 18 -- and if they do not, every
        // other number this module produces is suspect.
        assert_eq!(calculate_k(5_000, 1, 0.01), Some(18));
    }

    #[test]
    fn k_grows_with_the_block_rate() {
        // Ten times the rate needs a substantially larger k for the same
        // delay: more blocks fit inside the same propagation window.
        let slow = calculate_k(5_000, 1, 0.01).unwrap();
        let fast = calculate_k(5_000, 10, 0.01).unwrap();
        assert!(fast > slow, "k must grow with the block rate: {slow} -> {fast}");
    }

    #[test]
    fn k_grows_with_the_delay() {
        let quick = calculate_k(500, 10, 0.01).unwrap();
        let slow = calculate_k(5_000, 10, 0.01).unwrap();
        assert!(slow > quick, "k must grow with propagation delay: {quick} -> {slow}");
    }

    #[test]
    fn a_stricter_delta_needs_a_larger_k() {
        let lax = calculate_k(5_000, 1, 0.05).unwrap();
        let strict = calculate_k(5_000, 1, 0.0001).unwrap();
        assert!(strict > lax, "a smaller failure probability needs a larger k");
    }

    #[test]
    fn a_fast_network_needs_very_little() {
        // 100ms propagation at 1 bps: mu = 0.2, so a handful of blocks covers
        // the tail.
        let k = calculate_k(100, 1, 0.01).unwrap();
        assert!(k <= 3, "expected a small k for a fast network, got {k}");
    }

    #[test]
    fn an_impossible_combination_is_refused_not_clamped() {
        // An absurd delay at a high rate needs a k beyond the bound. Returning
        // a clamped value would silently ship a parameter that does not hold.
        assert_eq!(calculate_k(3_600_000, 100, 0.01), None);
    }

    #[test]
    fn nonsense_inputs_are_refused() {
        assert_eq!(calculate_k(1_000, 0, 0.01), None, "a zero block rate is meaningless");
        assert_eq!(calculate_k(1_000, 1, 0.0), None, "delta must be a probability");
        assert_eq!(calculate_k(1_000, 1, 1.0), None);
    }

    #[test]
    fn the_result_actually_satisfies_the_tail_bound() {
        // Independent check of the property, rather than of the implementation:
        // whatever k comes back, the Poisson tail beyond it must be under
        // delta, and the tail beyond k-1 must not be -- otherwise it is not
        // minimal.
        for (delay_ms, rate) in [(5_000u64, 1u64), (1_000, 10), (500, 10), (2_000, 5)] {
            let k = calculate_k(delay_ms, rate, 0.01).expect("a workable k exists");
            assert!(tail_above(delay_ms, rate, k) <= 0.01, "tail too fat at k={k}");
            if k > 0 {
                assert!(
                    tail_above(delay_ms, rate, k - 1) > 0.01,
                    "k={k} is not minimal for delay {delay_ms}ms at {rate} bps"
                );
            }
        }
    }

    /// `P[X > k]` for `X ~ Poisson(2 * delay * rate)`, computed independently
    /// of [`calculate_k`].
    #[allow(clippy::float_arithmetic, reason = "test-only reference computation")]
    fn tail_above(delay_ms: u64, rate: u64, k: u16) -> f64 {
        let mu = 2.0 * (delay_ms as f64 / 1_000.0) * rate as f64;
        let mut term = (-mu).exp();
        let mut cdf = term;
        for j in 1..=u32::from(k) {
            term *= mu / f64::from(j);
            cdf += term;
        }
        1.0 - cdf
    }
}
