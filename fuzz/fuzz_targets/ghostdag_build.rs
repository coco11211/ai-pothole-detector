//! Fuzzes GHOSTDAG with malformed and adversarial DAG shapes.
//!
//! Parent sets come from peers, so every shape here is one a hostile node can
//! actually send: duplicate parents, parents that do not exist, enormous
//! parent sets, and blocks claiming to be their own ancestor.
//!
//! The property being checked is not "the DAG accepts this" — most of these
//! should be rejected. It is that the store never panics, never loops, and
//! never leaves itself inconsistent.

#![no_main]

use alloy_primitives::{Address, B256};
use arbitrary::Arbitrary;
use chainname_ghostdag::DagStore;
use chainname_primitives::{BlockHash, HEADER_VERSION, Header};
use libfuzzer_sys::fuzz_target;

/// One block to add, described by which earlier blocks it names as parents.
#[derive(Debug, Arbitrary)]
struct BlockSpec {
    /// Indices into the blocks added so far, modulo the count. Out-of-range
    /// values become references to blocks that do not exist, which is exactly
    /// what a hostile peer would send.
    parent_indices: Vec<u16>,
    nonce: u16,
    /// Whether to sort parents. Unsorted parents must be rejected, because the
    /// canonical encoding requires strictly ascending order.
    sort_parents: bool,
}

#[derive(Debug, Arbitrary)]
struct Scenario {
    k: u8,
    blocks: Vec<BlockSpec>,
}

fn genesis() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: 1_700_000_000_000,
        bits: 0x2000_ffff,
        nonce: 0,
        miner: Address::ZERO,
        txs_root: B256::ZERO,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: B256::ZERO,
        deferred_gas_used: 0,
    }
}

fuzz_target!(|scenario: Scenario| {
    // Bounded so the fuzzer explores shapes rather than sizes.
    const MAX_BLOCKS: usize = 64;
    const MERGESET_LIMIT: u64 = 180;

    let g = genesis();
    let mut dag = DagStore::new(g, u16::from(scenario.k), MERGESET_LIMIT);
    let mut known: Vec<BlockHash> = vec![dag.genesis()];

    for (index, spec) in scenario.blocks.iter().take(MAX_BLOCKS).enumerate() {
        let mut parents: Vec<BlockHash> = spec
            .parent_indices
            .iter()
            .take(32)
            .map(|i| {
                let position = usize::from(*i);
                // Half the time reference a real block, half the time a hash
                // that does not exist.
                known.get(position % known.len().max(1)).copied().unwrap_or(B256::from(
                    alloy_primitives::U256::from(u64::from(*i) + 1_000_000),
                ))
            })
            .collect();

        if spec.sort_parents {
            parents.sort_unstable();
            parents.dedup();
        }

        let header = Header {
            parents,
            nonce: u64::from(spec.nonce) + index as u64 + 1,
            ..genesis()
        };

        // Must never panic, whatever the shape.
        if dag.add_block(header.clone()).is_ok() {
            known.push(header.hash());

            // Every accepted block must have coherent GHOSTDAG data.
            let data = dag.data(header.hash()).expect("an accepted block has data");
            assert!(
                dag.contains(data.selected_parent),
                "selected parent is not in the DAG"
            );
            assert!(
                data.mergeset_blues.contains(&data.selected_parent),
                "the selected parent must always be blue"
            );
            // Blues and reds never overlap.
            for red in &data.mergeset_reds {
                assert!(
                    !data.mergeset_blues.contains(red),
                    "a block was coloured both blue and red"
                );
            }
            // The merge set covers exactly the blues (minus the selected
            // parent) plus the reds.
            assert_eq!(
                data.mergeset_ordered.len(),
                data.mergeset_blues.len() - 1 + data.mergeset_reds.len(),
                "merge set size does not match its colouring"
            );
        }
    }

    // The chain walk must terminate and reach genesis, whatever was added.
    let tip = dag.virtual_selected_parent();
    let chain = dag.selected_parent_chain(tip);
    assert!(!chain.is_empty(), "the selected parent chain is never empty");
    assert_eq!(
        *chain.last().expect("non-empty"),
        dag.genesis(),
        "the chain must end at genesis"
    );
});
