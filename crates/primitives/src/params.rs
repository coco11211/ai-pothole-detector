//! Chain parameters.
//!
//! Every magic number in this file has a derivation comment. Nothing here may
//! use floating point: these values feed consensus directly.

/// EVM chain id. Fixed by the brief.
pub const CHAIN_ID: u64 = 7717;

/// Milliseconds in a second. Named so the block-rate derivations below read as
/// arithmetic rather than as magic.
pub const MS_PER_SECOND: u64 = 1_000;

/// Seconds in a year, used to derive the emission half-life from the block
/// rate. 365 days exactly; we do not model leap years because the emission
/// curve is smooth and a 0.27% error in the half-life is not observable.
pub const SECONDS_PER_YEAR: u64 = 365 * 24 * 60 * 60;

/// ASERT difficulty half-life, in seconds.
///
/// Two hours. Long enough that ordinary hashrate noise does not cause the
/// target to oscillate, short enough that a 10x step change is absorbed within
/// a few hours rather than a few days. This is the Bitcoin Cash `aserti3-2d`
/// parameter scaled down from 2 days to match a sub-second block interval:
/// what matters is the half-life measured in *blocks*, and at 1 bps two hours
/// is 7200 blocks, comfortably above the ~1000-block floor below which ASERT
/// becomes jumpy.
pub const ASERT_HALFLIFE_SECONDS: u64 = 2 * 60 * 60;

/// Sustained gas throughput target, in gas per second.
///
/// In GHOSTDAG every block is merged and executed, orphans included, so
/// sustained throughput is `blocks_per_second * block_gas_limit` and is
/// independent of DAG width. 30M gas/s is roughly 8x Ethereum L1's current
/// ~3.75M gas/s and sits inside sequential revm's sustained capability with
/// headroom for trie updates.
///
/// See OPEN-PROBLEMS.md P-002: at 10 bps this yields a 3M per-block limit,
/// which is too small for large contract deployments. M9 must resolve that
/// before raising the rate.
pub const TARGET_GAS_PER_SECOND: u64 = 30_000_000;

/// Deferred state root lag, expressed in seconds rather than blocks.
///
/// A header at selected-chain height N carries the state root of height N - D.
/// Defining D in time means raising the block rate at M9 does not silently
/// shrink the execution slack. Twenty seconds is ~4x the expected propagation
/// delay bound at 1 bps.
pub const DEFERRED_STATE_ROOT_LAG_SECONDS: u64 = 20;

/// Finality / pruning window, in seconds. 24 hours, the Kaspa convention.
///
/// This is a *pruning* horizon, not finality in the BFT sense. There is no
/// finality on this chain; see OPEN-PROBLEMS.md P-007.
pub const PRUNING_WINDOW_SECONDS: u64 = 24 * 60 * 60;

/// Initial block subsidy, in wei. 50 units, Bitcoin's opening figure.
pub const INITIAL_SUBSIDY_WEI: u128 = 50_000_000_000_000_000_000;

/// Permanent tail subsidy, in wei. 0.5 units.
///
/// A non-zero tail means security does not depend on fee revenue alone once
/// the exponential term has decayed.
pub const TAIL_SUBSIDY_WEI: u128 = 500_000_000_000_000_000;

/// GHOSTDAG `k` for a 1 block/second rate.
///
/// Kaspa-proven at this rate. `k` bounds the number of blocks that may be
/// created within the propagation delay window; exceeding it lets an attacker
/// build a competing blue set. It must be *recomputed* from the PHANTOM
/// paper's formula against a measured propagation bound before the rate is
/// raised — see OPEN-PROBLEMS.md P-008. Do not guess it.
pub const GHOSTDAG_K_AT_1_BPS: u16 = 18;

/// Upper bound on merge set size, as a multiple of `k`.
///
/// Without a bound, a block declaring a pathological parent set forces
/// unbounded work on every validator. 10x `k` is generous for honest operation
/// (a merge set larger than a few `k` means the network is badly partitioned)
/// while keeping validation cost linear and bounded.
pub const MERGESET_SIZE_LIMIT_K_MULTIPLE: u64 = 10;

/// Consensus parameters for one network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainParams {
    /// EVM chain id.
    pub chain_id: u64,
    /// Target interval between blocks, in milliseconds.
    pub target_block_interval_ms: u64,
    /// GHOSTDAG `k`.
    pub ghostdag_k: u16,
    /// ASERT half-life, in seconds.
    pub asert_halflife_seconds: u64,
    /// Sustained gas throughput target, in gas per second.
    pub target_gas_per_second: u64,
}

impl ChainParams {
    /// The 1 block/second staged testnet. This is the configuration for M3
    /// through M8; M9 moves to [`Self::testnet_10bps`].
    pub const fn testnet_1bps() -> Self {
        Self {
            chain_id: CHAIN_ID,
            target_block_interval_ms: MS_PER_SECOND,
            ghostdag_k: GHOSTDAG_K_AT_1_BPS,
            asert_halflife_seconds: ASERT_HALFLIFE_SECONDS,
            target_gas_per_second: TARGET_GAS_PER_SECOND,
        }
    }

    /// The 10 block/second configuration. **Not usable until M9**: `ghostdag_k`
    /// here is a placeholder copy of the 1 bps value and is wrong at this rate.
    /// See OPEN-PROBLEMS.md P-008.
    pub const fn testnet_10bps() -> Self {
        Self {
            chain_id: CHAIN_ID,
            target_block_interval_ms: MS_PER_SECOND / 10,
            ghostdag_k: GHOSTDAG_K_AT_1_BPS,
            asert_halflife_seconds: ASERT_HALFLIFE_SECONDS,
            target_gas_per_second: TARGET_GAS_PER_SECOND,
        }
    }

    /// Blocks per second, derived from the interval.
    ///
    /// Integer division. Intervals that do not divide a second evenly are
    /// rejected by [`Self::validate`], so this is exact wherever it is used.
    pub const fn blocks_per_second(&self) -> u64 {
        MS_PER_SECOND / self.target_block_interval_ms
    }

    /// Per-block gas limit, derived from the throughput target and the rate.
    pub const fn block_gas_limit(&self) -> u64 {
        self.target_gas_per_second / self.blocks_per_second()
    }

    /// Deferred state root lag `D`, in blocks.
    pub const fn deferred_state_root_lag(&self) -> u64 {
        DEFERRED_STATE_ROOT_LAG_SECONDS * self.blocks_per_second()
    }

    /// Pruning window, in blocks.
    pub const fn pruning_window_blocks(&self) -> u64 {
        PRUNING_WINDOW_SECONDS * self.blocks_per_second()
    }

    /// Emission half-life `H`, in blocks: one year at the current rate.
    pub const fn emission_halflife_blocks(&self) -> u64 {
        SECONDS_PER_YEAR * self.blocks_per_second()
    }

    /// Maximum number of blocks in a single merge set.
    pub const fn mergeset_size_limit(&self) -> u64 {
        MERGESET_SIZE_LIMIT_K_MULTIPLE * self.ghostdag_k as u64
    }

    /// Rejects parameter sets whose derivations would not be exact.
    pub fn validate(&self) -> Result<(), ParamsError> {
        if self.target_block_interval_ms == 0 {
            return Err(ParamsError::ZeroBlockInterval);
        }
        if !MS_PER_SECOND.is_multiple_of(self.target_block_interval_ms) {
            return Err(ParamsError::IntervalNotDivisor(self.target_block_interval_ms));
        }
        if !self.target_gas_per_second.is_multiple_of(self.blocks_per_second()) {
            return Err(ParamsError::GasTargetNotDivisible {
                target: self.target_gas_per_second,
                bps: self.blocks_per_second(),
            });
        }
        if self.ghostdag_k == 0 {
            return Err(ParamsError::ZeroK);
        }
        Ok(())
    }
}

/// Reasons a [`ChainParams`] is not usable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParamsError {
    /// Block interval of zero.
    #[error("target block interval must be non-zero")]
    ZeroBlockInterval,
    /// Interval does not divide one second, so `blocks_per_second` would be lossy.
    #[error("target block interval {0}ms does not divide 1000ms evenly")]
    IntervalNotDivisor(u64),
    /// Gas target does not divide by the block rate, so the per-block limit would be lossy.
    #[error("gas target {target} does not divide evenly by {bps} blocks/second")]
    GasTargetNotDivisible {
        /// The configured gas-per-second target.
        target: u64,
        /// The derived block rate.
        bps: u64,
    },
    /// GHOSTDAG k of zero.
    #[error("ghostdag k must be non-zero")]
    ZeroK,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_validate() {
        ChainParams::testnet_1bps().validate().unwrap();
        ChainParams::testnet_10bps().validate().unwrap();
    }

    #[test]
    fn one_bps_derivations() {
        let p = ChainParams::testnet_1bps();
        assert_eq!(p.blocks_per_second(), 1);
        assert_eq!(p.block_gas_limit(), 30_000_000);
        assert_eq!(p.deferred_state_root_lag(), 20);
        assert_eq!(p.pruning_window_blocks(), 86_400);
        assert_eq!(p.emission_halflife_blocks(), 31_536_000);
        assert_eq!(p.mergeset_size_limit(), 180);
    }

    #[test]
    fn ten_bps_derivations() {
        let p = ChainParams::testnet_10bps();
        assert_eq!(p.blocks_per_second(), 10);
        // OPEN-PROBLEMS.md P-002: this is the figure that is too small.
        assert_eq!(p.block_gas_limit(), 3_000_000);
        assert_eq!(p.deferred_state_root_lag(), 200);
        assert_eq!(p.pruning_window_blocks(), 864_000);
        assert_eq!(p.emission_halflife_blocks(), 315_360_000);
    }

    #[test]
    fn rejects_inexact_interval() {
        let p = ChainParams { target_block_interval_ms: 300, ..ChainParams::testnet_1bps() };
        assert_eq!(p.validate(), Err(ParamsError::IntervalNotDivisor(300)));
    }

    #[test]
    fn rejects_zero_interval() {
        let p = ChainParams { target_block_interval_ms: 0, ..ChainParams::testnet_1bps() };
        assert_eq!(p.validate(), Err(ParamsError::ZeroBlockInterval));
    }

    #[test]
    fn rejects_zero_k() {
        let p = ChainParams { ghostdag_k: 0, ..ChainParams::testnet_1bps() };
        assert_eq!(p.validate(), Err(ParamsError::ZeroK));
    }

    #[test]
    fn tail_is_below_initial() {
        const { assert!(TAIL_SUBSIDY_WEI < INITIAL_SUBSIDY_WEI) };
    }
}
