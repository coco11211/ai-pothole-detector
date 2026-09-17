//! Measures the propagation delay bound and derives GHOSTDAG's `k` from it.
//!
//! `k` must never be guessed (OPEN-PROBLEMS.md P-008). This measures full
//! propagation time across the network at a given block rate, takes a high
//! percentile as the delay bound `D`, and feeds it to the PHANTOM formula.

use chainname_ghostdag::calculate_k_default;
use chainname_testkit::{SimConfig, Simulation};

/// Percentile of full-propagation times taken as the delay bound, in parts
/// per thousand.
///
/// The 99th. `D` is a *bound*, so a mean would understate it badly — the tail
/// is exactly the case `k` exists to survive. The 99.9th would be dominated by
/// single outliers in a run of this length.
const DELAY_PERCENTILE: u64 = 990;

fn measure(nodes: usize, block_interval_ms: u64, seed: u64, minutes: u64) -> (u64, u64, u64) {
    let config = SimConfig { nodes, seed, block_interval_ms, ..Default::default() };
    let mut sim = Simulation::new(config);
    sim.run_for(minutes * 60_000);
    sim.quiesce(120_000);

    let delays = sim.propagation_delays();
    let median = delays.get(delays.len() / 2).copied().unwrap_or(0);
    let p99 = sim.propagation_delay_percentile(DELAY_PERCENTILE);
    let max = delays.last().copied().unwrap_or(0);
    (median, p99, max)
}

#[test]
fn measure_propagation_and_derive_k() {
    for (label, interval_ms, bps) in [("1 bps", 1_000u64, 1u64), ("10 bps", 100, 10)] {
        let (median, p99, max) = measure(10, interval_ms, 7, 10);
        let k = calculate_k_default(p99, bps);
        eprintln!("{label:>7}: median={median}ms p99={p99}ms max={max}ms -> k={k:?}");
        assert!(p99 > 0, "{label}: no propagation was measured at all");
        assert!(k.is_some(), "{label}: no workable k for a {p99}ms delay bound");
    }
}

#[test]
fn propagation_is_measured_not_assumed() {
    // Guards against the measurement silently reporting nothing, which would
    // make every k derived from it meaningless.
    let mut sim = Simulation::new(SimConfig { nodes: 5, seed: 3, ..Default::default() });
    sim.run_for(2 * 60_000);
    sim.quiesce(120_000);

    let delays = sim.propagation_delays();
    assert!(delays.len() > 100, "only {} blocks fully propagated", delays.len());
    assert!(delays.iter().all(|d| *d > 0), "a block propagated in zero time");
}
