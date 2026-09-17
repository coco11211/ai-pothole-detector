//! A deterministic pseudo-random generator for simulations.
//!
//! Integer-only and seeded. Nothing about a simulation run may depend on the
//! host, the thread schedule, or the wall clock: a divergence that cannot be
//! reproduced from a seed cannot be debugged.

/// A 64-bit linear congruential generator.
///
/// Not cryptographic, and not trying to be. It exists so that "packet loss" and
/// "latency jitter" are reproducible, which is the only property that matters
/// here.
#[derive(Debug, Clone)]
pub struct Lcg {
    state: u64,
}

impl Lcg {
    /// Multiplier and increment from Knuth's MMIX.
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    const INCREMENT: u64 = 1_442_695_040_888_963_407;

    /// Creates a generator from a seed.
    pub const fn new(seed: u64) -> Self {
        // A zero seed would make the first few outputs unusually structured;
        // mixing in a constant avoids that without affecting reproducibility.
        Self { state: seed ^ 0x9E37_79B9_7F4A_7C15 }
    }

    /// Next raw value.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(Self::MULTIPLIER).wrapping_add(Self::INCREMENT);
        // Return the high bits: the low bits of an LCG have short periods.
        self.state >> 16
    }

    /// Uniform value in `[0, bound)`. Returns 0 when `bound` is 0.
    pub const fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        self.next_u64() % bound
    }

    /// Uniform value in `[low, high]`.
    pub const fn between(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        low + self.below(high - low + 1)
    }

    /// True with probability `permille / 1000`.
    pub const fn chance_permille(&mut self, permille: u64) -> bool {
        self.below(1_000) < permille
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_sequence() {
        let mut a = Lcg::new(42);
        let mut b = Lcg::new(42);
        for _ in 0..1_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Lcg::new(1);
        let mut b = Lcg::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn below_respects_its_bound() {
        let mut rng = Lcg::new(7);
        for _ in 0..10_000 {
            assert!(rng.below(10) < 10);
        }
    }

    #[test]
    fn below_zero_is_zero_not_a_panic() {
        assert_eq!(Lcg::new(1).below(0), 0);
    }

    #[test]
    fn between_stays_within_range() {
        let mut rng = Lcg::new(9);
        for _ in 0..10_000 {
            let value = rng.between(5, 15);
            assert!((5..=15).contains(&value));
        }
    }

    #[test]
    fn between_handles_a_degenerate_range() {
        assert_eq!(Lcg::new(1).between(7, 7), 7);
        assert_eq!(Lcg::new(1).between(9, 3), 9);
    }

    #[test]
    fn chance_is_roughly_calibrated() {
        let mut rng = Lcg::new(11);
        let hits = (0..100_000).filter(|_| rng.chance_permille(250)).count();
        // 25% of 100,000, with generous slack: this checks the generator is
        // not obviously broken, not that it is a good RNG.
        assert!((23_000..27_000).contains(&hits), "got {hits} hits");
    }

    #[test]
    fn certainty_and_impossibility_are_exact() {
        let mut rng = Lcg::new(3);
        for _ in 0..1_000 {
            assert!(rng.chance_permille(1_000));
            assert!(!rng.chance_permille(0));
        }
    }
}
