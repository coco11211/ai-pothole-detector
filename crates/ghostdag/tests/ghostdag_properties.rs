//! M4 gate: GHOSTDAG properties, adversarial DAGs, and cross-instance
//! determinism.
//!
//! Every DAG here is built from an explicit shape, then rebuilt in a different
//! insertion order, and the two instances must agree on every block's
//! GHOSTDAG data. That is the property the whole design rests on: nodes learn
//! about blocks in different orders and must still compute the same chain.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, B256};
use chainname_ghostdag::{DagError, DagStore};
use chainname_primitives::{BlockHash, HEADER_VERSION, Header};

/// GHOSTDAG k for these tests. Small enough that the k-cluster rule actually
/// fires on DAGs we can write down by hand.
const TEST_K: u16 = 3;
/// Generous merge-set bound; the limit has its own dedicated test.
const MERGESET_LIMIT: u64 = 10_000;
/// Every block uses this target, so work per block is uniform and blue work
/// differences come only from topology.
const TEST_BITS: u32 = 0x2000_ffff;

fn genesis_header() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: 1_700_000_000_000,
        bits: TEST_BITS,
        nonce: 0,
        miner: Address::ZERO,
        txs_root: B256::ZERO,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: B256::ZERO,
        deferred_gas_used: 0,
    }
}

/// Builds a header with the given parents and a distinguishing nonce.
///
/// Parents are sorted, matching the canonical encoding rule that removes
/// header malleability.
fn block(parents: &[BlockHash], nonce: u64) -> Header {
    let mut parents = parents.to_vec();
    parents.sort_unstable();
    parents.dedup();
    Header {
        version: HEADER_VERSION,
        parents,
        timestamp_ms: 1_700_000_000_000 + nonce * 1_000,
        bits: TEST_BITS,
        nonce,
        miner: Address::ZERO,
        txs_root: B256::ZERO,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: B256::ZERO,
        deferred_gas_used: 0,
    }
}

/// A DAG shape: names, and each block's parents by name.
///
/// Declared once, then instantiated in several insertion orders. `"G"` is
/// genesis and is implicit.
struct Shape {
    blocks: Vec<(&'static str, Vec<&'static str>)>,
}

impl Shape {
    /// Instantiates the shape, adding blocks in the order given by `order`
    /// (indices into `self.blocks`). Every order must be topologically valid.
    fn build(&self, order: &[usize], k: u16) -> (DagStore, HashMap<&'static str, BlockHash>) {
        let mut dag = DagStore::new(genesis_header(), k, MERGESET_LIMIT);
        let mut names: HashMap<&'static str, BlockHash> = HashMap::new();
        names.insert("G", dag.genesis());

        // Nonces are keyed to the block's position in `self.blocks`, not to
        // the insertion order, so a block's hash does not depend on when it
        // was added. Without this the test would be comparing different DAGs.
        for &i in order {
            let (name, parent_names) = &self.blocks[i];
            let parents: Vec<BlockHash> = parent_names.iter().map(|p| names[p]).collect();
            let header = block(&parents, i as u64 + 1);
            let hash = dag.add_block(header).expect("block should be addable");
            names.insert(name, hash);
        }
        (dag, names)
    }

    /// A topologically valid insertion order: the declaration order.
    fn declaration_order(&self) -> Vec<usize> {
        (0..self.blocks.len()).collect()
    }
}

/// A simple chain: G <- A <- B <- C.
fn chain_shape() -> Shape {
    Shape { blocks: vec![("A", vec!["G"]), ("B", vec!["A"]), ("C", vec!["B"])] }
}

/// A diamond: two parallel blocks merged by a third.
fn diamond_shape() -> Shape {
    Shape { blocks: vec![("A", vec!["G"]), ("B", vec!["G"]), ("M", vec!["A", "B"])] }
}

/// A wide fan: `width` parallel blocks off genesis, merged by one block.
fn wide_shape(width: usize) -> Shape {
    let names: Vec<&'static str> = vec!["P0", "P1", "P2", "P3", "P4", "P5", "P6", "P7", "P8", "P9"];
    let mut blocks: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for name in names.iter().take(width) {
        blocks.push((name, vec!["G"]));
    }
    blocks.push(("M", names[..width].to_vec()));
    Shape { blocks }
}

#[test]
fn genesis_is_its_own_selected_parent() {
    let dag = DagStore::new(genesis_header(), TEST_K, MERGESET_LIMIT);
    let data = dag.data(dag.genesis()).unwrap();
    assert_eq!(data.selected_parent, dag.genesis());
    assert_eq!(data.blue_score, 0);
    assert!(data.mergeset_ordered.is_empty());
}

#[test]
fn a_chain_increments_blue_score_by_one() {
    let shape = chain_shape();
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    for (name, expected) in [("A", 1u64), ("B", 2), ("C", 3)] {
        assert_eq!(dag.data(names[name]).unwrap().blue_score, expected, "block {name}");
    }
}

#[test]
fn a_chain_accumulates_blue_work() {
    let shape = chain_shape();
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    let g = dag.data(dag.genesis()).unwrap().blue_work;
    let a = dag.data(names["A"]).unwrap().blue_work;
    let c = dag.data(names["C"]).unwrap().blue_work;
    assert!(a > g, "blue work must grow along a chain");
    assert!(c > a, "blue work must keep growing");
}

#[test]
fn a_diamond_merges_both_branches() {
    let shape = diamond_shape();
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    let merge = dag.data(names["M"]).unwrap();

    // One branch is the selected parent, the other is merged.
    let selected = merge.selected_parent;
    assert!(selected == names["A"] || selected == names["B"]);
    let other = if selected == names["A"] { names["B"] } else { names["A"] };

    assert_eq!(merge.mergeset_ordered, vec![other], "the other branch must be merged");
    assert!(merge.mergeset_blues.contains(&other), "with k=3 it must be blue");
    // Genesis, both branches, and M itself.
    assert_eq!(merge.blue_score, 3);
}

#[test]
fn the_selected_parent_is_the_one_with_more_blue_work() {
    // A long branch and a short one. The long branch must win, regardless of
    // hash ordering.
    let shape = Shape {
        blocks: vec![
            ("L1", vec!["G"]),
            ("L2", vec!["L1"]),
            ("L3", vec!["L2"]),
            ("S1", vec!["G"]),
            ("M", vec!["L3", "S1"]),
        ],
    };
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    assert_eq!(dag.data(names["M"]).unwrap().selected_parent, names["L3"]);
}

#[test]
fn k_zero_colours_everything_off_the_chain_red() {
    // With k=0, no block may have any blue in its anticone, so only the
    // selected parent chain stays blue. This is the degenerate case the
    // k-cluster rule reduces to, and it must not panic or mis-colour.
    let shape = diamond_shape();
    let (dag, names) = shape.build(&shape.declaration_order(), 0);
    let merge = dag.data(names["M"]).unwrap();
    assert_eq!(merge.mergeset_blues.len(), 1, "only the selected parent may be blue at k=0");
    assert_eq!(merge.mergeset_reds.len(), 1);
    assert_eq!(merge.blue_score, 2, "genesis and M itself");
}

#[test]
fn a_merge_set_wider_than_k_produces_red_blocks() {
    // Ten parallel blocks with k=3: at most k+1 = 4 entries in
    // `mergeset_blues` (the selected parent plus three), so of the nine
    // merged blocks at least six must be red.
    let shape = wide_shape(10);
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    let merge = dag.data(names["M"]).unwrap();

    // Nine, not ten: one of the ten parents becomes the selected parent, and
    // the merge set is by definition `past(M) \ past(selected_parent)`, which
    // excludes it. The selected parent is the previous chain block; its
    // transactions are already executed.
    assert_eq!(merge.mergeset_ordered.len(), 9, "the nine non-selected parents are merged");
    assert!(
        merge.mergeset_blues.len() <= usize::from(TEST_K) + 1,
        "at most k+1 blues, got {}",
        merge.mergeset_blues.len()
    );
    assert!(!merge.mergeset_reds.is_empty(), "the excess must be red");
    assert_eq!(
        merge.mergeset_blues.len() - 1 + merge.mergeset_reds.len(),
        9,
        "every merged block is coloured exactly once"
    );
}

#[test]
fn red_blocks_are_still_merged_and_still_ordered() {
    // The point of a DAG: orphans fold into the ledger rather than being
    // discarded. Redness costs a block its blue score, not its transactions.
    let shape = wide_shape(10);
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    let merge = dag.data(names["M"]).unwrap();

    let ordered: HashSet<BlockHash> = merge.mergeset_ordered.iter().copied().collect();
    for red in &merge.mergeset_reds {
        assert!(ordered.contains(red), "a red block must still appear in the execution order");
    }
}

#[test]
fn the_merge_set_is_topologically_ordered() {
    let shape = Shape {
        blocks: vec![
            ("A", vec!["G"]),
            ("B", vec!["A"]),
            ("C", vec!["G"]),
            ("D", vec!["C"]),
            ("M", vec!["B", "D"]),
        ],
    };
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);
    let merge = dag.data(names["M"]).unwrap();

    let position: HashMap<BlockHash, usize> =
        merge.mergeset_ordered.iter().enumerate().map(|(i, h)| (*h, i)).collect();

    for (hash, pos) in &position {
        for parent in &dag.header(*hash).unwrap().parents {
            if let Some(parent_pos) = position.get(parent) {
                assert!(parent_pos < pos, "a parent appeared after its child in the merge set");
            }
        }
    }
}

#[test]
fn every_block_is_merged_exactly_once_by_the_chain() {
    // The core DAG guarantee. Walking the selected parent chain and taking
    // each block's merge set must visit every block in the DAG exactly once,
    // with nothing lost and nothing counted twice.
    let shape = Shape {
        blocks: vec![
            ("A", vec!["G"]),
            ("B", vec!["G"]),
            ("C", vec!["A"]),
            ("D", vec!["B"]),
            ("E", vec!["A", "B"]),
            ("M", vec!["C", "D", "E"]),
            ("N", vec!["M"]),
        ],
    };
    let (dag, _names) = shape.build(&shape.declaration_order(), TEST_K);

    let tip = dag.virtual_selected_parent();
    let chain = dag.selected_parent_chain(tip);

    let mut visited: Vec<BlockHash> = Vec::new();
    for chain_block in &chain {
        if *chain_block == dag.genesis() {
            continue;
        }
        visited.extend(dag.data(*chain_block).unwrap().mergeset_ordered.iter().copied());
        visited.push(*chain_block);
    }
    visited.push(dag.genesis());

    let unique: HashSet<BlockHash> = visited.iter().copied().collect();
    assert_eq!(unique.len(), visited.len(), "a block was merged twice");
    assert_eq!(unique.len(), dag.len(), "a block was never merged");
}

#[test]
fn identical_dags_built_in_different_orders_agree_exactly() {
    // The M4 gate proper. Two nodes learning the same blocks in different
    // orders must compute identical GHOSTDAG data for every block.
    let shape = Shape {
        blocks: vec![
            ("A", vec!["G"]),
            ("B", vec!["G"]),
            ("C", vec!["G"]),
            ("D", vec!["A", "B"]),
            ("E", vec!["B", "C"]),
            ("F", vec!["A", "C"]),
            ("M", vec!["D", "E", "F"]),
            ("N", vec!["M"]),
        ],
    };

    // Several topologically valid orders of the same shape.
    let orders: Vec<Vec<usize>> = vec![
        vec![0, 1, 2, 3, 4, 5, 6, 7],
        vec![2, 1, 0, 4, 3, 5, 6, 7],
        vec![1, 0, 2, 5, 3, 4, 6, 7],
        vec![0, 2, 1, 5, 4, 3, 6, 7],
    ];

    let (reference, reference_names) = shape.build(&orders[0], TEST_K);

    for order in &orders[1..] {
        let (other, other_names) = shape.build(order, TEST_K);

        assert_eq!(other.len(), reference.len());
        for (name, hash) in &reference_names {
            assert_eq!(other_names[name], *hash, "block {name} got a different hash");
            assert_eq!(
                other.data(*hash),
                reference.data(*hash),
                "block {name} got different GHOSTDAG data under a different insertion order"
            );
        }

        assert_eq!(
            other.virtual_selected_parent(),
            reference.virtual_selected_parent(),
            "the two instances chose different chain tips"
        );
        assert_eq!(
            other.selected_parent_chain(other.virtual_selected_parent()),
            reference.selected_parent_chain(reference.virtual_selected_parent()),
            "the two instances derived different selected parent chains"
        );
    }
}

#[test]
fn an_adversarial_withholding_pattern_is_still_deterministic() {
    // A miner withholds a long private branch and releases it late, so honest
    // nodes see it in a very different order from the blocks it competes with.
    // Ordering must still be identical everywhere.
    let shape = Shape {
        blocks: vec![
            ("H1", vec!["G"]),
            ("H2", vec!["H1"]),
            ("H3", vec!["H2"]),
            ("W1", vec!["G"]),
            ("W2", vec!["W1"]),
            ("W3", vec!["W2"]),
            ("W4", vec!["W3"]),
            ("M", vec!["H3", "W4"]),
        ],
    };

    // Honest-first, then withheld-first: the two views a partition produces.
    let honest_first = vec![0, 1, 2, 3, 4, 5, 6, 7];
    let withheld_first = vec![3, 4, 5, 6, 0, 1, 2, 7];

    let (a, names_a) = shape.build(&honest_first, TEST_K);
    let (b, names_b) = shape.build(&withheld_first, TEST_K);

    for (name, hash) in &names_a {
        assert_eq!(names_b[name], *hash);
        assert_eq!(a.data(*hash), b.data(*hash), "block {name} diverged");
    }

    // The longer withheld branch has more work, so it wins the selected
    // parent. That is the correct, if uncomfortable, Nakamoto answer.
    assert_eq!(a.data(names_a["M"]).unwrap().selected_parent, names_a["W4"]);
}

#[test]
fn a_missing_parent_is_reported_not_silently_accepted() {
    let mut dag = DagStore::new(genesis_header(), TEST_K, MERGESET_LIMIT);
    let orphan = block(&[B256::repeat_byte(0xaa)], 1);
    let hash = orphan.hash();
    assert_eq!(
        dag.add_block(orphan),
        Err(DagError::MissingParent { block: hash, parent: B256::repeat_byte(0xaa) })
    );
}

#[test]
fn adding_the_same_block_twice_is_rejected() {
    let mut dag = DagStore::new(genesis_header(), TEST_K, MERGESET_LIMIT);
    let header = block(&[dag.genesis()], 1);
    let hash = dag.add_block(header.clone()).unwrap();
    assert_eq!(dag.add_block(header), Err(DagError::AlreadyPresent(hash)));
}

#[test]
fn an_oversized_merge_set_is_rejected() {
    // A denial-of-service guard: a block declaring a pathological parent set
    // must be refused rather than forcing unbounded work on every validator.
    let mut dag = DagStore::new(genesis_header(), TEST_K, 3);
    let mut parents = Vec::new();
    for nonce in 1..=6 {
        let hash = dag.add_block(block(&[dag.genesis()], nonce)).unwrap();
        parents.push(hash);
    }
    assert!(matches!(dag.add_block(block(&parents, 100)), Err(DagError::MergesetTooLarge { .. })));
}

#[test]
fn tips_are_reported_in_a_canonical_order() {
    let shape = wide_shape(5);
    // Build without the merging block so the parallel blocks stay tips.
    let order: Vec<usize> = (0..5).collect();
    let (a, _) = shape.build(&order, TEST_K);
    let (b, _) = shape.build(&[4, 3, 2, 1, 0], TEST_K);
    assert_eq!(a.tips(), b.tips(), "tip order must not depend on insertion order");
}

#[test]
fn reachability_is_correct_in_both_directions() {
    let shape = diamond_shape();
    let (dag, names) = shape.build(&shape.declaration_order(), TEST_K);

    assert!(dag.is_ancestor_of(dag.genesis(), names["M"]));
    assert!(dag.is_ancestor_of(names["A"], names["M"]));
    assert!(dag.is_ancestor_of(names["B"], names["M"]));
    assert!(dag.is_ancestor_of(names["M"], names["M"]), "a block is its own ancestor");

    assert!(!dag.is_ancestor_of(names["M"], names["A"]));
    assert!(!dag.is_ancestor_of(names["A"], names["B"]), "siblings are concurrent");
    assert!(!dag.is_ancestor_of(names["B"], names["A"]));
}
