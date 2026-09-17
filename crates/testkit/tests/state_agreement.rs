//! M6 gate: five nodes, one hour, with reorgs, agreeing on the state root at
//! every chain height.
//!
//! This is the seam under load. DAG agreement alone proves nothing here — two
//! nodes can hold identical blocks and still disagree on state if the ordering
//! rule, the fee attribution, or the reorg rollback is wrong. Only matching
//! state roots at matching heights prove the seam is correct.

use chainname_testkit::{LinkQuality, SimConfig, Simulation};

/// One hour of simulated time, the milestone's figure.
const ONE_HOUR_MS: u64 = 60 * 60 * 1_000;
/// Budget for the network to settle before assertions.
const SETTLE_BUDGET_MS: u64 = 300_000;

fn assert_agreed(sim: &mut Simulation, context: &str) {
    let settled = sim.quiesce(SETTLE_BUDGET_MS);
    assert!(settled, "{context}: network did not settle\n{}", sim.diagnostics());

    if let Some(divergence) = sim.divergence() {
        panic!("{context}: DAG divergence: {divergence}\n{}", sim.diagnostics());
    }
    if let Some(divergence) = sim.state_divergence() {
        panic!("{context}: STATE divergence: {divergence}\n{}", sim.diagnostics());
    }
}

#[test]
fn nodes_agree_on_state_after_executing_transactions() {
    let config = SimConfig { nodes: 5, seed: 2, ..Default::default() };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(3 * 60_000);
    assert_agreed(&mut sim, &format!("seed {}", config.seed));

    assert!(sim.executed_height(0) > 10, "barely any chain was executed");
    for node in 0..config.nodes {
        assert_eq!(
            sim.state_root(node),
            sim.state_root(0),
            "node {node} finished on a different state root"
        );
    }
}

#[test]
fn nodes_agree_on_state_for_a_full_hour_with_reorgs() {
    // The gate. Checked every five minutes rather than only at the end, so a
    // run that diverges and later reconverges still fails.
    let config = SimConfig { nodes: 5, seed: 11, ..Default::default() };
    let mut sim = Simulation::new(config.clone());

    for segment in 1..=12 {
        sim.run_for(5 * 60_000);
        assert_agreed(&mut sim, &format!("seed {} at segment {segment}", config.seed));
    }

    assert!(sim.now_ms() - 1_700_000_000_000 >= ONE_HOUR_MS);
    assert!(sim.mined > 500, "only {} blocks mined in an hour", sim.mined);
    assert!(sim.reorgs > 0, "no reorgs occurred, so the rollback path was never exercised");
    assert!(sim.executed_height(0) > 100, "chain barely advanced");
}

#[test]
fn reorgs_are_induced_by_latency_and_state_still_agrees() {
    // Latency well above the block interval makes concurrent blocks the norm
    // and forces frequent selected-parent changes, which is what drives the
    // undo journal.
    let config = SimConfig {
        nodes: 5,
        seed: 17,
        link: LinkQuality { min_latency_ms: 900, max_latency_ms: 3_000, loss_permille: 30 },
        ..Default::default()
    };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(15 * 60_000);
    assert_agreed(&mut sim, &format!("seed {} under heavy latency", config.seed));

    assert!(
        sim.reorgs > 0,
        "high latency produced no reorgs, which means the test is not testing reorgs"
    );
}

#[test]
fn state_agreement_survives_severe_packet_loss() {
    let config = SimConfig {
        nodes: 5,
        seed: 19,
        link: LinkQuality { min_latency_ms: 50, max_latency_ms: 250, loss_permille: 250 },
        ..Default::default()
    };
    let mut sim = Simulation::new(config.clone());
    sim.run_for(10 * 60_000);
    assert_agreed(&mut sim, &format!("seed {} under 25% loss", config.seed));
}

#[test]
fn state_agreement_holds_across_many_seeds() {
    for seed in 1..=8u64 {
        let mut sim = Simulation::new(SimConfig { nodes: 5, seed, ..Default::default() });
        sim.run_for(2 * 60_000);
        assert_agreed(&mut sim, &format!("seed {seed}"));
    }
}

#[test]
fn execution_is_reproducible_from_the_seed() {
    // If two runs of the same seed reach different state roots, no divergence
    // found by any other test in this file can be debugged.
    let config = SimConfig { nodes: 5, seed: 23, ..Default::default() };
    let mut a = Simulation::new(config.clone());
    let mut b = Simulation::new(config);
    a.run_for(3 * 60_000);
    b.run_for(3 * 60_000);
    a.quiesce(SETTLE_BUDGET_MS);
    b.quiesce(SETTLE_BUDGET_MS);

    assert_eq!(a.executed_height(0), b.executed_height(0));
    assert_eq!(a.state_root(0), b.state_root(0), "same seed, different state");
}

#[test]
fn transactions_actually_moved_value() {
    // Guards against the whole suite passing vacuously because no transaction
    // ever executed: every node would then agree on the genesis root forever.
    let mut sim = Simulation::new(SimConfig { nodes: 5, seed: 29, ..Default::default() });
    let genesis_root = sim.state_root_at(0, 0).expect("genesis root recorded");
    sim.run_for(3 * 60_000);
    sim.quiesce(SETTLE_BUDGET_MS);

    assert_ne!(
        sim.state_root(0),
        genesis_root,
        "the state never changed, so agreement proves nothing"
    );
}
