//! Selected-parent-chain reorganisation.
//!
//! Execution runs along the selected parent chain. When GHOSTDAG picks a new
//! virtual selected parent, some chain blocks may leave the chain and others
//! join it. This computes exactly which, so execution can undo and redo the
//! minimum.

use std::collections::HashSet;

use chainname_ghostdag::DagStore;
use chainname_primitives::BlockHash;

/// The difference between two selected parent chains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChainReorg {
    /// Blocks leaving the chain, **tip first**: the order they must be undone.
    pub removed: Vec<BlockHash>,
    /// Blocks joining the chain, **oldest first**: the order they must execute.
    pub added: Vec<BlockHash>,
}

impl ChainReorg {
    /// True if nothing changed.
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }

    /// True if this only extends the chain, undoing nothing. The common case,
    /// and the cheap one.
    pub fn is_extension(&self) -> bool {
        self.removed.is_empty() && !self.added.is_empty()
    }

    /// How deep the reorg goes, in chain blocks undone.
    pub fn depth(&self) -> usize {
        self.removed.len()
    }
}

/// Computes the reorg from `old_tip` to `new_tip`.
///
/// Walks both selected parent chains back to their common ancestor. Genesis is
/// always common, so the walk always terminates.
pub fn compute_reorg(dag: &DagStore, old_tip: BlockHash, new_tip: BlockHash) -> ChainReorg {
    if old_tip == new_tip {
        return ChainReorg::default();
    }

    // Tip-first chains, both ending at genesis.
    let old_chain = dag.selected_parent_chain(old_tip);
    let new_chain = dag.selected_parent_chain(new_tip);

    let old_set: HashSet<BlockHash> = old_chain.iter().copied().collect();

    // The first block of the new chain that the old chain also had is the fork
    // point. Walking the new chain tip-first finds the *deepest* such block,
    // which is what we want: everything above it on the old chain is undone.
    let fork_point = new_chain.iter().position(|hash| old_set.contains(hash));

    let Some(fork_index) = fork_point else {
        // No common block at all. Only possible if the two tips belong to
        // different DAGs, which callers must not do.
        return ChainReorg { removed: old_chain, added: new_chain.into_iter().rev().collect() };
    };

    let fork_hash = new_chain[fork_index];

    // Old chain above the fork point, tip-first: the undo order.
    let removed: Vec<BlockHash> =
        old_chain.into_iter().take_while(|hash| *hash != fork_hash).collect();

    // New chain above the fork point, oldest-first: the execution order.
    let added: Vec<BlockHash> = new_chain.into_iter().take(fork_index).rev().collect();

    ChainReorg { removed, added }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use chainname_primitives::{HEADER_VERSION, Header};

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

    fn child(parents: &[BlockHash], nonce: u64) -> Header {
        let mut parents = parents.to_vec();
        parents.sort_unstable();
        Header { parents, nonce, ..genesis() }
    }

    fn store() -> DagStore {
        DagStore::new(genesis(), 18, 180)
    }

    #[test]
    fn no_change_is_an_empty_reorg() {
        let dag = store();
        let g = dag.genesis();
        assert!(compute_reorg(&dag, g, g).is_empty());
    }

    #[test]
    fn extending_the_chain_removes_nothing() {
        let mut dag = store();
        let g = dag.genesis();
        let a = dag.add_block(child(&[g], 1)).unwrap();
        let b = dag.add_block(child(&[a], 2)).unwrap();

        let reorg = compute_reorg(&dag, g, b);
        assert!(reorg.removed.is_empty());
        assert_eq!(reorg.added, vec![a, b], "added must be oldest-first");
        assert!(reorg.is_extension());
        assert_eq!(reorg.depth(), 0);
    }

    #[test]
    fn switching_branches_undoes_tip_first_and_redoes_oldest_first() {
        let mut dag = store();
        let g = dag.genesis();

        // Branch one: g -> a1 -> a2
        let a1 = dag.add_block(child(&[g], 1)).unwrap();
        let a2 = dag.add_block(child(&[a1], 2)).unwrap();
        // Branch two: g -> b1 -> b2 -> b3
        let b1 = dag.add_block(child(&[g], 3)).unwrap();
        let b2 = dag.add_block(child(&[b1], 4)).unwrap();
        let b3 = dag.add_block(child(&[b2], 5)).unwrap();

        let reorg = compute_reorg(&dag, a2, b3);
        assert_eq!(reorg.removed, vec![a2, a1], "undo order is tip-first");
        assert_eq!(reorg.added, vec![b1, b2, b3], "execution order is oldest-first");
        assert_eq!(reorg.depth(), 2);
        assert!(!reorg.is_extension());
    }

    #[test]
    fn a_reorg_and_its_inverse_are_mirror_images() {
        let mut dag = store();
        let g = dag.genesis();
        let a1 = dag.add_block(child(&[g], 1)).unwrap();
        let a2 = dag.add_block(child(&[a1], 2)).unwrap();
        let b1 = dag.add_block(child(&[g], 3)).unwrap();

        let forward = compute_reorg(&dag, a2, b1);
        let backward = compute_reorg(&dag, b1, a2);

        assert_eq!(forward.removed, backward.added.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(forward.added, backward.removed.iter().rev().copied().collect::<Vec<_>>());
    }

    #[test]
    fn the_fork_point_itself_is_never_undone() {
        let mut dag = store();
        let g = dag.genesis();
        let shared = dag.add_block(child(&[g], 1)).unwrap();
        let a = dag.add_block(child(&[shared], 2)).unwrap();
        let b = dag.add_block(child(&[shared], 3)).unwrap();

        let reorg = compute_reorg(&dag, a, b);
        assert_eq!(reorg.removed, vec![a]);
        assert_eq!(reorg.added, vec![b]);
        assert!(!reorg.removed.contains(&shared), "the fork point stays executed");
    }

    #[test]
    fn genesis_is_never_undone() {
        let mut dag = store();
        let g = dag.genesis();
        let a = dag.add_block(child(&[g], 1)).unwrap();
        let b = dag.add_block(child(&[g], 2)).unwrap();
        let reorg = compute_reorg(&dag, a, b);
        assert!(!reorg.removed.contains(&g));
        assert!(!reorg.added.contains(&g));
    }
}
