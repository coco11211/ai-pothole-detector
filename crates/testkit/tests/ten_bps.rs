//! M9 gate: the M5, M6 and M8 gates re-run at 10 blocks per second.
//!
//! The rate is only raised once `k` has been *measured* for it
//! (`measure_k.rs`) and the gas limit problem has been resolved
//! (OPEN-PROBLEMS.md P-002). Both are done; this checks that everything which
//! held at 1 bps still holds at ten times the rate.

use chainname_primitives::ChainParams;
use chainname_testkit::{LinkQuality, SimConfig, Simulation};

const SETTLE_BUDGET_MS: u64 = 300_000;

fn assert_healthy(sim: &mut Simulation, nodes: usize, context: &str) {
    let settled = sim.quiesce(SETTLE_BUDGET_MS);
    assert!(settled, "{context}: network did not settle\n{}", sim.diagnostics());
    if let Some(divergence) = sim.divergence() {
        panic!("{context}: DAG divergence: {divergence}\n{}", sim.diagnostics());
    }
    if let Some(divergence) = sim.state_divergence() {
        panic!("{context}: STATE divergence: {divergence}\n{}", sim.diagnostics());
    }
    for node in 0..nodes {
        let m = sim.memory_proxies(node);
        assert_eq!(m.orphans, 0, "{context}: node {node} still holds orphans");
        assert_eq!(m.pending_requests, 0, "{context}: node {node} has outstanding requests");
    }
}

#[test]
fn the_ten_bps_parameters_are_self_consistent() {
    let p = ChainParams::testnet_10bps();
    p.validate().expect("the 10 bps preset must validate");

    assert_eq!(p.blocks_per_second(), 10);
    assert_eq!(p.ghostdag_k, 151, "k must be the value measured at this rate");
    assert_ne!(
        p.ghostdag_k,
        ChainParams::testnet_1bps().ghostdag_k,
        "carrying the 1 bps k over would be the dangerous shortcut P-008 warns about"
    );

    // Derived quantities follow the rate.
    assert_eq!(p.deferred_state_root_lag(), 200, "20 seconds of blocks");
    assert_eq!(p.pruning_window_blocks(), 864_000, "24 hours of blocks");
    assert_eq!(p.emission_halflife_blocks(), 315_360_000, "one year of blocks");
    assert_eq!(p.mergeset_size_limit(), 1_510, "10 x k");
}

// --- M5 gate at 10 bps --------------------------------------------------

#[test]
fn m5_five_nodes_converge_at_ten_bps() {
    let config = SimConfig::at_ten_bps(5, 91);
    let mut sim = Simulation::new(config.clone());
    sim.run_for(5 * 60_000);
    assert_healthy(&mut sim, config.nodes, "M5 at 10 bps");
    assert!(sim.mined > 2_000, "only {} blocks in 5 minutes at 10 bps", sim.mined);
}

#[test]
fn m5_convergence_at_ten_bps_survives_packet_loss() {
    let mut config = SimConfig::at_ten_bps(5, 93);
    config.link = LinkQuality { min_latency_ms: 50, max_latency_ms: 250, loss_permille: 250 };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(5 * 60_000);
    assert_healthy(&mut sim, config.nodes, "M5 at 10 bps under 25% loss");
}

#[test]
fn m5_convergence_at_ten_bps_survives_high_latency() {
    // At 10 bps a one-second link is ten block intervals, so the DAG is very
    // wide and the k-cluster rule is doing real work.
    let mut config = SimConfig::at_ten_bps(5, 95);
    config.link = LinkQuality { min_latency_ms: 800, max_latency_ms: 2_500, loss_permille: 20 };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(5 * 60_000);
    assert_healthy(&mut sim, config.nodes, "M5 at 10 bps under high latency");
}

// --- M6 gate at 10 bps --------------------------------------------------

#[test]
fn m6_state_agrees_at_ten_bps_with_reorgs() {
    let config = SimConfig::at_ten_bps(5, 97);
    let mut sim = Simulation::new(config.clone());

    for segment in 1..=6 {
        sim.run_for(5 * 60_000);
        assert_healthy(&mut sim, config.nodes, &format!("M6 at 10 bps, segment {segment}"));
    }

    assert!(sim.reorgs > 0, "no reorgs, so rollback was never exercised at this rate");
    assert!(sim.executed_height(0) > 100, "chain barely advanced");
    for node in 0..config.nodes {
        assert_eq!(
            sim.state_root(node),
            sim.state_root(0),
            "node {node} ended on a different state root at 10 bps"
        );
    }
}

#[test]
fn m6_execution_is_reproducible_at_ten_bps() {
    let config = SimConfig::at_ten_bps(5, 99);
    let mut a = Simulation::new(config.clone());
    let mut b = Simulation::new(config);
    a.run_for(3 * 60_000);
    b.run_for(3 * 60_000);
    a.quiesce(SETTLE_BUDGET_MS);
    b.quiesce(SETTLE_BUDGET_MS);
    assert_eq!(a.state_root(0), b.state_root(0), "same seed, different state at 10 bps");
}

// --- M8 gate at 10 bps --------------------------------------------------

#[test]
fn m8_ten_nodes_stay_healthy_at_ten_bps() {
    let config = SimConfig::at_ten_bps(10, 101);
    let mut sim = Simulation::new(config.clone());
    for checkpoint in 1..=4 {
        sim.run_for(10 * 60_000);
        assert_healthy(&mut sim, config.nodes, &format!("M8 at 10 bps, checkpoint {checkpoint}"));
    }
    assert!(sim.mined > 10_000, "only {} blocks mined", sim.mined);
}

/// The longest 10 bps soak that fits in memory here.
///
/// The 1 bps soak covers a full 24 hours. At 10 bps the same wall-clock span
/// is ten times the blocks, and the harness holds ten complete nodes in one
/// process: measured at ~5.9 KiB per block per node
/// (`crates/testkit/tests/memory_scale.rs`), 24 hours across ten nodes would
/// need roughly 46 GiB. That is a limit of simulating ten nodes in one
/// address space, not of the chain — but it is real, so the soak is scoped to
/// what fits and the arithmetic is stated rather than the horizon quietly
/// shortened. See OPEN-PROBLEMS.md P-017.
const TEN_BPS_SOAK_HOURS: u64 = 6;

#[test]
#[ignore = "runs for several minutes; the M9 soak, run with --ignored"]
fn m8_long_soak_at_ten_bps() {
    let config = SimConfig::at_ten_bps(10, 103);
    let mut sim = Simulation::new(config.clone());

    for checkpoint in 1..=TEN_BPS_SOAK_HOURS {
        sim.run_for(60 * 60_000);
        assert_healthy(&mut sim, config.nodes, &format!("M9 soak, hour {checkpoint}"));
        let m = sim.memory_proxies(0);
        eprintln!(
            "hour {checkpoint:>2}/{TEN_BPS_SOAK_HOURS}: height={} dag={} journal={} \
             entries={} mined={} reorgs={} deepest={}",
            sim.executed_height(0),
            m.dag_blocks,
            m.journal_records,
            m.journal_entries,
            sim.mined,
            sim.reorgs,
            sim.deepest_reorg,
        );
    }

    assert!(
        sim.mined > 100_000,
        "only {} blocks in {TEN_BPS_SOAK_HOURS} hours at 10 bps",
        sim.mined
    );
    for node in 0..config.nodes {
        assert_eq!(sim.state_root(node), sim.state_root(0), "node {node} diverged");
    }
}
