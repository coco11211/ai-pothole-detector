//! Measures how a simulated network's memory scales with chain length.
//!
//! The 24-hour soak at 10 bps does not fit in this container, and "it OOMed"
//! is not a useful finding on its own. This measures the actual cost per block
//! per node, so the limit can be stated as a number rather than an anecdote.

use chainname_testkit::{SimConfig, Simulation};

/// Resident set size of this process, in bytes.
///
/// Read from `/proc/self/statm`, whose second field is resident pages.
/// Linux-only, which is fine: it is a measurement aid, not shipped behaviour.
fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // 4 KiB pages on every platform this runs on.
    Some(resident_pages * 4096)
}

#[test]
fn report_memory_per_block_per_node() {
    let Some(baseline) = rss_bytes() else {
        eprintln!("skipping: /proc/self/statm unavailable");
        return;
    };

    for (label, config, minutes) in [
        ("1 bps, 10 nodes", SimConfig::at_one_bps(10, 11), 20u64),
        ("10 bps, 10 nodes", SimConfig::at_ten_bps(10, 11), 5),
    ] {
        let before = rss_bytes().unwrap_or(baseline);
        let mut sim = Simulation::new(config.clone());
        sim.run_for(minutes * 60_000);
        sim.quiesce(120_000);

        let after = rss_bytes().unwrap_or(before);
        let used = after.saturating_sub(before);
        let blocks = sim.dag_size() as u64;
        let per_block_per_node = used / blocks.max(1) / config.nodes as u64;

        eprintln!(
            "{label:>17}: {} blocks/node, {:>6} MiB resident, \
             ~{per_block_per_node} bytes per block per node",
            blocks,
            used / (1024 * 1024),
        );

        // What a 24-hour run would need at this rate.
        let day_blocks = 24 * 60 * 60 * (1_000 / config.block_interval_ms);
        let projected_gib =
            per_block_per_node.saturating_mul(day_blocks).saturating_mul(config.nodes as u64)
                / (1024 * 1024 * 1024);
        eprintln!("{:>17}  projected for 24h x {} nodes: ~{projected_gib} GiB", "", config.nodes);

        drop(sim);
    }
}
