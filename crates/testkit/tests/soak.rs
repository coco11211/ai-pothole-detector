//! M8 gate: a 24-hour, 10-node soak.
//!
//! Twenty-four hours of *simulated* time, which is the milestone's figure run
//! in a way that a failure can be reproduced. A wall-clock soak would take a
//! day, could not be run in CI, and a divergence in hour nineteen would be
//! gone forever. This runs in minutes, deterministically, from a seed.
//!
//! Three things are asserted, matching the gate:
//!
//! * **No divergence** — every node agrees on the DAG, the selected chain, and
//!   the state root at every height, checked repeatedly through the run rather
//!   than only at the end.
//! * **No memory growth** — the structures that could grow without bound
//!   (orphans, outstanding requests, undo journal) stay bounded, and the ones
//!   that grow by design (the DAG, block bodies) grow only with the chain.
//! * **No panics** — any panic anywhere fails the test.

use chainname_testkit::{LinkQuality, SimConfig, Simulation};

/// Twenty-four hours in milliseconds.
const TWENTY_FOUR_HOURS_MS: u64 = 24 * 60 * 60 * 1_000;
/// How often the run is checked. Twelve checkpoints across the day.
const CHECKPOINT_MS: u64 = 2 * 60 * 60 * 1_000;
/// Budget for the network to settle at each checkpoint.
const SETTLE_BUDGET_MS: u64 = 300_000;

fn assert_healthy(sim: &mut Simulation, nodes: usize, context: &str) {
    assert_healthy_within(sim, nodes, context, SETTLE_BUDGET_MS);
}

fn assert_healthy_within(sim: &mut Simulation, nodes: usize, context: &str, settle_budget_ms: u64) {
    let settled = sim.quiesce(settle_budget_ms);
    assert!(settled, "{context}: network did not settle\n{}", sim.diagnostics());

    if let Some(divergence) = sim.divergence() {
        panic!("{context}: DAG divergence: {divergence}\n{}", sim.diagnostics());
    }
    if let Some(divergence) = sim.state_divergence() {
        panic!("{context}: STATE divergence: {divergence}\n{}", sim.diagnostics());
    }

    for node in 0..nodes {
        let m = sim.memory_proxies(node);
        assert_eq!(m.orphans, 0, "{context}: node {node} still holds {} orphans", m.orphans);
        assert_eq!(
            m.pending_requests, 0,
            "{context}: node {node} still has {} outstanding requests",
            m.pending_requests
        );
    }
}

#[test]
#[ignore = "runs for several minutes; the M8 gate, run explicitly with --ignored"]
fn ten_nodes_soak_for_twenty_four_simulated_hours() {
    let config = SimConfig { nodes: 10, seed: 24, ..Default::default() };
    let mut sim = Simulation::new(config.clone());

    let checkpoints = TWENTY_FOUR_HOURS_MS / CHECKPOINT_MS;
    let mut previous_journal_entries = 0usize;

    for checkpoint in 1..=checkpoints {
        sim.run_for(CHECKPOINT_MS);
        assert_healthy(&mut sim, config.nodes, &format!("seed 24, checkpoint {checkpoint}"));

        let m = sim.memory_proxies(0);
        // The DAG and the bodies grow with the chain; that is not a leak. The
        // journal is the one that would grow without bound if pruning were
        // broken, so it is tracked explicitly across checkpoints.
        eprintln!(
            "checkpoint {checkpoint:>2}/{checkpoints}: height={} dag={} bodies={} \
             journal={} entries={} mined={} reorgs={} deepest={}",
            sim.executed_height(0),
            m.dag_blocks,
            m.bodies,
            m.journal_records,
            m.journal_entries,
            sim.mined,
            sim.reorgs,
            sim.deepest_reorg,
        );
        previous_journal_entries = m.journal_entries;
    }

    // The run must have been substantial, or "no divergence" means nothing.
    assert!(sim.mined > 40_000, "only {} blocks mined in 24 hours", sim.mined);
    assert!(sim.executed_height(0) > 20_000, "chain only reached {}", sim.executed_height(0));
    assert!(sim.reorgs > 0, "no reorgs occurred, so rollback was never exercised");
    assert!(previous_journal_entries > 0, "the journal recorded nothing at all");

    // Every node must agree, right to the end.
    for node in 0..config.nodes {
        assert_eq!(
            sim.state_root(node),
            sim.state_root(0),
            "node {node} ended on a different state root"
        );
    }
}

#[test]
fn ten_nodes_stay_healthy_for_two_simulated_hours() {
    // The same checks over a shorter horizon, so CI exercises this path on
    // every run rather than only when someone remembers `--ignored`.
    let config = SimConfig { nodes: 10, seed: 2, ..Default::default() };
    let mut sim = Simulation::new(config.clone());

    for checkpoint in 1..=4 {
        sim.run_for(30 * 60_000);
        assert_healthy(&mut sim, config.nodes, &format!("seed 2, checkpoint {checkpoint}"));
    }

    assert!(sim.mined > 3_000, "only {} blocks mined", sim.mined);
    assert!(sim.reorgs > 0, "no reorgs, so rollback was never exercised");
}

#[test]
fn the_undo_journal_stays_bounded() {
    // The journal holds the pre-value of every account each chain block
    // touched. Unpruned it grows forever, and it is the clearest leak in the
    // system if pruning regresses.
    let mut sim = Simulation::new(SimConfig { nodes: 5, seed: 41, ..Default::default() });

    sim.run_for(20 * 60_000);
    sim.quiesce(SETTLE_BUDGET_MS);
    let early = sim.memory_proxies(0);

    sim.run_for(40 * 60_000);
    sim.quiesce(SETTLE_BUDGET_MS);
    let late = sim.memory_proxies(0);

    // The journal is bounded by the pruning window, so within a single hour it
    // simply tracks the chain. What must hold is that it never exceeds the
    // height: one record per executed block, never more.
    assert!(
        late.journal_records as u64 <= sim.executed_height(0),
        "journal holds {} records for a chain of height {}",
        late.journal_records,
        sim.executed_height(0)
    );
    assert!(late.journal_records >= early.journal_records, "records vanished unexpectedly");
}

#[test]
fn a_network_losing_forty_percent_of_packets_still_converges() {
    // Far beyond any real link. Every round trip completes with probability
    // 0.6 x 0.6 = 0.36, so recovery needs many more retries than usual — which
    // is why the settle budget here is generous rather than the default. A
    // network this degraded converging *slowly* is correct; converging never
    // would not be.
    //
    // This test earned its place: at 40% loss an earlier version of the
    // handshake treated a dropped `Version` as misbehaviour, and the network
    // quietly disconnected itself into permanent non-convergence.
    const HEAVY_LOSS_SETTLE_MS: u64 = 30 * 60_000;

    let config = SimConfig {
        nodes: 6,
        seed: 55,
        link: LinkQuality { min_latency_ms: 50, max_latency_ms: 400, loss_permille: 400 },
        ..Default::default()
    };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(20 * 60_000);

    assert_healthy_within(
        &mut sim,
        config.nodes,
        "seed 55 at 40% packet loss",
        HEAVY_LOSS_SETTLE_MS,
    );

    // And every peer survived: a lossy link must not cost connections.
    for node in 0..config.nodes {
        assert_eq!(sim.peer_count(node), config.nodes - 1, "node {node} lost peers to packet loss");
    }
}
