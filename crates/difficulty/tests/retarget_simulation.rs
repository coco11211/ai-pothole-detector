//! M3 gate: difficulty must track a simulated hashrate through a 10x step up
//! and a 90% step down, without stalling and without oscillating.
//!
//! The simulation is integer-only, deterministic, and seeded where it is
//! random, because a nondeterministic consensus test cannot be debugged.
//!
//! # Model
//!
//! A block requires `work = 2^256 / target` hashes in expectation. A miner
//! with `hashrate` hashes per second therefore finds one every
//! `work * 1000 / hashrate` milliseconds. Difficulty is recomputed after every
//! block from the fixed anchor, which is what absolute ASERT does.
//!
//! # Why 150,000 blocks and not 10,000
//!
//! The milestone asked for 10,000 blocks. That is not enough to demonstrate
//! convergence, and the arithmetic says so. Adapting to a 10x hashrate change
//! requires the chain to run `log2(10) ≈ 3.32` half-lives ahead of schedule.
//! At a 2-hour half-life that is ~23,900 seconds of accumulated lead. With
//! blocks arriving 10x too fast, lead builds at 9 seconds per second, so the
//! adjustment completes after ~2,650 seconds of real time — but by then
//! roughly 26,000 blocks have been produced. A 10,000-block run can only show
//! the chain moving in the right direction, never arriving.
//!
//! So both are tested: `difficulty_converges_after_hashrate_steps` runs long
//! enough to actually converge, and `ten_thousand_blocks_track_in_the_right_direction`
//! asserts what the literal 10,000-block horizon can show. DECISIONS.md C-006.

use alloy_primitives::U256;
use chainname_difficulty::{AsertParams, next_target};

/// Baseline miner hashrate, in hashes per second.
const BASE_HASHRATE: u128 = 1_000_000;
/// Target seconds between blocks at the staged 1 bps rate.
const IDEAL_BLOCK_TIME_SECS: i128 = 1;
/// ASERT half-life: two hours.
const HALFLIFE_SECS: i128 = 2 * 60 * 60;
/// Expected block time in milliseconds when difficulty matches hashrate.
const IDEAL_BLOCK_TIME_MS: u128 = 1_000;

/// Blocks per regime. Sized from the convergence arithmetic in the module
/// docs: ~26,000 blocks to adapt to a 10x step, plus margin to settle.
const REGIME_BLOCKS: u64 = 30_000;
/// Blocks in the elevated-hashrate regime. Longer because a 10x hashrate
/// produces blocks 10x faster, so the same wall-clock adaptation spans far
/// more blocks.
const HIGH_REGIME_BLOCKS: u64 = 60_000;

/// Block heights are `u64` in consensus and `usize` when indexing samples.
/// One explicit conversion point rather than casts scattered through the test.
fn idx(height: u64) -> usize {
    usize::try_from(height).expect("simulation length fits in usize")
}

struct Sample {
    block_time_ms: u128,
    target: U256,
}

fn params() -> AsertParams {
    AsertParams::new(
        IDEAL_BLOCK_TIME_SECS,
        HALFLIFE_SECS,
        // Easy enough never to bind in this simulation; we are testing the
        // retarget response, not the clamp (which has its own unit tests).
        U256::MAX >> 1,
    )
}

/// The anchor target, chosen so one block takes exactly one second at
/// [`BASE_HASHRATE`]. This puts the simulation at equilibrium on block 1.
fn anchor_target() -> U256 {
    U256::MAX / U256::from(BASE_HASHRATE)
}

/// Runs the simulation. `hashrate_at` returns the hashrate in force at a given
/// height; `jitter_permille` scales each block time (1000 = no jitter).
fn simulate(
    total_blocks: u64,
    hashrate_at: impl Fn(u64) -> u128,
    mut jitter_permille: impl FnMut(u64) -> u128,
) -> Vec<Sample> {
    let params = params();
    let anchor = anchor_target();

    let mut elapsed_ms: i128 = 0;
    let mut target = anchor;
    let mut samples = Vec::with_capacity(idx(total_blocks));

    for height in 1..=total_blocks {
        // Expected hashes to find this block.
        let work = (U256::MAX / target).saturating_to::<u128>();
        let hashrate = hashrate_at(height);
        let block_time_ms =
            (work * IDEAL_BLOCK_TIME_MS / hashrate) * jitter_permille(height) / 1000;
        // A block cannot take zero time; clamp so the schedule stays monotone.
        let block_time_ms = block_time_ms.max(1);

        elapsed_ms += i128::try_from(block_time_ms).expect("block time fits in i128");
        samples.push(Sample { block_time_ms, target });

        target = next_target(&params, anchor, height, elapsed_ms / 1000)
            .to_target()
            .expect("retarget always produces a decodable target");
    }

    samples
}

/// Mean block time over a window.
fn mean_block_time_ms(samples: &[Sample], from: usize, to: usize) -> u128 {
    let window = &samples[from..to];
    let count = u128::try_from(window.len()).expect("window length fits");
    window.iter().map(|s| s.block_time_ms).sum::<u128>() / count
}

/// Difficulty relative to the anchor, in permille (1000 = anchor difficulty).
///
/// Difficulty is inversely proportional to target, so this is
/// `anchor / target`, scaled.
fn difficulty_permille(target: U256) -> u128 {
    (anchor_target() * U256::from(1000u32) / target).saturating_to::<u128>()
}

#[test]
fn difficulty_converges_after_hashrate_steps() {
    let step_up = REGIME_BLOCKS;
    let step_down = step_up + HIGH_REGIME_BLOCKS;
    let total = step_down + HIGH_REGIME_BLOCKS;

    let samples = simulate(
        total,
        |h| {
            if h <= step_up {
                BASE_HASHRATE
            } else if h <= step_down {
                // 10x step up.
                BASE_HASHRATE * 10
            } else {
                // 90% step down, back to baseline.
                BASE_HASHRATE
            }
        },
        |_| 1000,
    );

    let settled = |end: u64| {
        let end = idx(end);
        mean_block_time_ms(&samples, end - 5_000, end)
    };

    // --- each regime settles back to the one-second target ---
    let baseline = settled(step_up);
    let elevated = settled(step_down);
    let recovered = settled(total);

    for (label, mean) in [
        ("baseline", baseline),
        ("after 10x step up", elevated),
        ("after 90% step down", recovered),
    ] {
        assert!(
            (950..=1_050).contains(&mean),
            "{label}: mean block time {mean}ms is not within 5% of the 1000ms target"
        );
    }

    // --- difficulty actually moved to match the hashrate ---
    let elevated_difficulty = difficulty_permille(samples[idx(step_down - 1)].target);
    assert!(
        (9_000..=11_000).contains(&elevated_difficulty),
        "difficulty should be ~10x the anchor at the end of the 10x regime, was \
         {elevated_difficulty} permille"
    );

    let recovered_difficulty = difficulty_permille(samples[idx(total - 1)].target);
    assert!(
        (900..=1_100).contains(&recovered_difficulty),
        "difficulty should return to ~1x the anchor after the step down, was \
         {recovered_difficulty} permille"
    );

    // --- it never stalled ---
    let worst = samples.iter().map(|s| s.block_time_ms).max().unwrap();
    assert!(worst < 15_000, "a block took {worst}ms; a 90% hashrate drop must not stall the chain");

    // --- and it did not oscillate ---
    // In a settled window, block times should be tightly clustered. An
    // oscillating retarget shows up as a wide spread even after convergence.
    for (label, end) in [("baseline", step_up), ("elevated", step_down), ("recovered", total)] {
        let window = &samples[(idx(end) - 5_000)..idx(end)];
        let min = window.iter().map(|s| s.block_time_ms).min().unwrap();
        let max = window.iter().map(|s| s.block_time_ms).max().unwrap();
        let mean = mean_block_time_ms(&samples, idx(end) - 5_000, idx(end));
        assert!(
            (max - min) * 10 < mean,
            "{label}: settled block times span {min}..{max}ms around a {mean}ms mean, \
             which is oscillation, not noise"
        );
    }
}

#[test]
fn ten_thousand_blocks_track_in_the_right_direction() {
    // The literal milestone horizon. Ten thousand blocks cannot show
    // convergence (see the module docs), but they must show the retarget
    // pushing the right way and never stalling.
    const TOTAL: u64 = 10_000;
    const STEP_UP: u64 = 3_000;
    const STEP_DOWN: u64 = 6_000;

    let samples = simulate(
        TOTAL,
        |h| {
            if h <= STEP_UP {
                BASE_HASHRATE
            } else if h <= STEP_DOWN {
                BASE_HASHRATE * 10
            } else {
                BASE_HASHRATE
            }
        },
        |_| 1000,
    );

    let at = |h: u64| difficulty_permille(samples[idx(h - 1)].target);

    // Steady at the anchor before anything happens.
    assert!((990..=1_010).contains(&at(STEP_UP)), "baseline drifted: {}", at(STEP_UP));

    // Rising while the hashrate is elevated.
    assert!(
        at(STEP_DOWN) > at(STEP_UP),
        "difficulty must rise under 10x hashrate: {} -> {}",
        at(STEP_UP),
        at(STEP_DOWN)
    );

    // Falling again once the hashrate drops.
    assert!(
        at(TOTAL) < at(STEP_DOWN),
        "difficulty must fall after a 90% hashrate drop: {} -> {}",
        at(STEP_DOWN),
        at(TOTAL)
    );

    let worst = samples.iter().map(|s| s.block_time_ms).max().unwrap();
    assert!(worst < 30_000, "a block took {worst}ms within the 10,000 block horizon");
}

#[test]
fn converges_under_jittered_block_times() {
    // Real block times are exponentially distributed, not constant. This uses
    // a seeded integer LCG to scale each block time between 0.05x and ~4x,
    // which is a cruder distribution than a true exponential but has the
    // property that matters: heavy spread, including very long blocks.
    //
    // Integer-only and fixed-seed, so a failure here reproduces exactly.
    let mut lcg: u64 = 0x2545_F491_4F6C_DD1D;
    let mut jitter = move |_h: u64| -> u128 {
        // Numerical Recipes LCG constants.
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // Map the top bits to [50, 4000] permille.
        50 + u128::from(lcg >> 43) * 3950 / u128::from(u32::MAX >> 11)
    };

    let total = REGIME_BLOCKS * 2;
    let samples = simulate(total, |_| BASE_HASHRATE, &mut jitter);

    // The jitter's mean is ~2.0x, so equilibrium sits at roughly half the
    // anchor difficulty rather than at it. What matters is that the retarget
    // finds *an* equilibrium and holds there: block times average the target.
    let mean = mean_block_time_ms(&samples, idx(total) - 10_000, idx(total));
    assert!(
        (800..=1_250).contains(&mean),
        "jittered mean block time {mean}ms did not settle near the 1000ms target"
    );
}
