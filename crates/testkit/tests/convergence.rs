//! M5 gate: five nodes converge on identical DAG state from a cold start,
//! over thirty minutes, under simulated packet loss and latency.
//!
//! Virtual time, seeded randomness, no sleeping. Every assertion failure here
//! reproduces exactly from the seed printed in the message.

use chainname_testkit::{LinkQuality, SimConfig, Simulation};

/// Thirty minutes of simulated time, the milestone's figure.
const THIRTY_MINUTES_MS: u64 = 30 * 60 * 1_000;

/// Lets the network settle, then asserts every node agrees.
///
/// Convergence is a property of a settled network. Asserting it mid-flight
/// would fail on a block mined moments ago that simply has not arrived, which
/// is correct behaviour rather than divergence.
fn assert_converged(sim: &mut Simulation, context: &str) {
    let settled = sim.quiesce(SETTLE_BUDGET_MS);
    assert!(settled, "{context}: network did not settle within the budget");
    if let Some(divergence) = sim.divergence() {
        panic!("{context}: {divergence}");
    }
}

/// How long the network is given to drain before convergence is asserted.
///
/// Generous: several request-timeout cycles, so a run that needs the retry
/// path still settles. A run that needs longer than this is genuinely stuck.
const SETTLE_BUDGET_MS: u64 = 300_000;

#[test]
fn five_nodes_converge_from_a_cold_start() {
    let config = SimConfig { nodes: 5, seed: 1, ..Default::default() };
    let mut sim = Simulation::new(config.clone());

    sim.run_for(60_000);

    assert_converged(&mut sim, &format!("seed {}", config.seed));
    assert!(sim.dag_size() > 1, "no blocks were produced at all");
}

#[test]
fn five_nodes_stay_converged_for_thirty_minutes() {
    let config = SimConfig { nodes: 5, seed: 7, ..Default::default() };
    let mut sim = Simulation::new(config.clone());

    // Check convergence periodically, not just at the end: a run that diverges
    // and then accidentally reconverges must still fail.
    for minute in 1..=30 {
        sim.run_for(60_000);
        assert_converged(&mut sim, &format!("seed {} at minute {minute}", config.seed));
    }

    // Quiescence advances the clock past the nominal thirty minutes, which is
    // expected: settling takes time too.
    assert!(sim.now_ms() - 1_700_000_000_000 >= THIRTY_MINUTES_MS);
    assert!(sim.mined > 100, "only {} blocks mined in 30 minutes", sim.mined);
    assert!(sim.dropped > 0, "packet loss never fired; the test is not testing loss");
    for node in 0..config.nodes {
        assert_eq!(sim.orphan_count(node), 0, "node {node} still holds orphans at the end");
    }
}

#[test]
fn convergence_survives_severe_packet_loss() {
    // 25% loss. Far worse than any real link; this exercises the retry path in
    // `DagSync::on_tick` rather than modelling a plausible network.
    let config = SimConfig {
        nodes: 5,
        seed: 13,
        link: LinkQuality { min_latency_ms: 50, max_latency_ms: 250, loss_permille: 250 },
        ..Default::default()
    };
    let mut sim = Simulation::new(config.clone());

    sim.run_for(10 * 60_000);

    assert_converged(&mut sim, &format!("seed {} under 25% loss", config.seed));
    assert!(sim.dropped > 100, "expected heavy loss, saw {}", sim.dropped);
}

#[test]
fn convergence_survives_high_latency() {
    // Latency well above the block interval, so the DAG is genuinely wide and
    // concurrent blocks are the norm rather than the exception.
    let config = SimConfig {
        nodes: 5,
        seed: 23,
        link: LinkQuality { min_latency_ms: 800, max_latency_ms: 2_500, loss_permille: 20 },
        ..Default::default()
    };
    let mut sim = Simulation::new(config.clone());

    sim.run_for(10 * 60_000);

    assert_converged(&mut sim, &format!("seed {} under high latency", config.seed));
}

#[test]
fn the_simulation_is_reproducible() {
    // The property the whole harness rests on. Two runs with the same seed
    // must produce byte-identical DAGs, or no divergence found here can be
    // debugged.
    let config = SimConfig { nodes: 5, seed: 99, ..Default::default() };

    let mut a = Simulation::new(config.clone());
    let mut b = Simulation::new(config);
    a.run_for(5 * 60_000);
    b.run_for(5 * 60_000);

    assert_eq!(a.mined, b.mined, "different block counts from the same seed");
    assert_eq!(a.dropped, b.dropped, "different loss patterns from the same seed");
    assert_eq!(a.delivered, b.delivered, "different delivery counts from the same seed");
    for node in 0..5 {
        assert_eq!(
            a.block_set(node),
            b.block_set(node),
            "node {node} built a different DAG from the same seed"
        );
    }
}

#[test]
fn different_seeds_produce_different_runs() {
    // Guards against the seed being ignored, which would make every
    // "reproducible" claim above vacuous.
    let mut a = Simulation::new(SimConfig { seed: 1, ..Default::default() });
    let mut b = Simulation::new(SimConfig { seed: 2, ..Default::default() });
    a.run_for(60_000);
    b.run_for(60_000);
    assert_ne!(a.block_set(0), b.block_set(0), "the seed had no effect");
}

#[test]
fn a_larger_network_still_converges() {
    let config = SimConfig { nodes: 9, seed: 31, ..Default::default() };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(5 * 60_000);
    assert_converged(&mut sim, &format!("seed {} with 9 nodes", config.seed));
}

#[test]
fn convergence_holds_across_many_seeds() {
    // One lucky seed proves nothing. Ten short runs across different seeds
    // catch ordering-dependent bugs that a single run would miss.
    for seed in 1..=10u64 {
        let mut sim = Simulation::new(SimConfig { nodes: 5, seed, ..Default::default() });
        sim.run_for(2 * 60_000);
        assert_converged(&mut sim, &format!("seed {seed}"));
    }
}
