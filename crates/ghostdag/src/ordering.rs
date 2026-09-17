//! The merge-set ordering rule: how DAG topology becomes an execution order.
//!
//! This is the consensus-critical seam described in ARCHITECTURE.md §6. It
//! must be a pure, deterministic function of DAG topology and block contents
//! alone — two nodes that hold the same blocks must derive the same order, in
//! any insertion sequence, on any machine.
//!
//! # Two stages
//!
//! 1. **Base sequence** ([`base_sequence`]). Walk the merge set in its
//!    deterministic topological order, concatenate each block's transactions,
//!    then append the chain block's own. Deduplicate, first occurrence wins.
//! 2. **Layering** ([`layer_and_sort`]). Assign each transaction a round equal
//!    to one more than the highest round of any earlier transaction it
//!    conflicts with, then sort by `(round, original index)`.
//!
//! # Why layering rather than plain concatenation
//!
//! Concatenation gives a batching scheduler nothing to work with. Layering
//! groups transactions with pairwise-disjoint static access sets into the same
//! round, so a parallel executor (M10) can run a whole round at once and a
//! sequential one just walks the list. The two must agree exactly.
//!
//! # What it cannot do
//!
//! The EVM's real access set is dynamic. `S(tx)` below is a static
//! approximation from what the transaction declares. Adversarially constructed
//! merge sets defeat it entirely — OPEN-PROBLEMS.md P-001.

use std::collections::{HashMap, HashSet};

use alloy_primitives::Address;
use chainname_primitives::BlockHash;

use crate::dag::DagStore;

/// A transaction's statically declared access set.
///
/// The sender is always a member. That single fact is what gives the ordering
/// its nonce-safety property: two transactions from one sender always conflict,
/// so they always land in different rounds and never swap places.
pub trait AccessSet {
    /// Addresses this transaction statically declares it will touch: the
    /// sender, the destination if any, and any EIP-2930 access list entries.
    fn access_keys(&self) -> impl Iterator<Item = Address>;
}

/// The merge set of `chain_block`, in execution order, followed by the chain
/// block itself.
///
/// The selected parent is excluded: it is the previous chain block and its
/// transactions have already been executed.
pub fn base_sequence(dag: &DagStore, chain_block: BlockHash) -> Vec<BlockHash> {
    let Some(data) = dag.data(chain_block) else { return vec![chain_block] };
    let mut sequence = data.mergeset_ordered.clone();
    sequence.push(chain_block);
    sequence
}

/// Computes the canonical execution order.
///
/// Returns indices into `transactions`, in the order they must execute.
///
/// Single forward pass, O(n · |S|), integer only, no allocation beyond one map.
pub fn layer_and_sort<T: AccessSet>(transactions: &[T]) -> Vec<usize> {
    // Round of the last transaction to touch each address.
    let mut last_touch: HashMap<Address, u32> = HashMap::new();
    let mut rounds: Vec<u32> = Vec::with_capacity(transactions.len());

    for transaction in transactions {
        let mut round = 0u32;
        for key in transaction.access_keys() {
            if let Some(previous) = last_touch.get(&key) {
                round = round.max(previous.saturating_add(1));
            }
        }
        for key in transaction.access_keys() {
            last_touch.insert(key, round);
        }
        rounds.push(round);
    }

    let mut order: Vec<usize> = (0..transactions.len()).collect();
    // `(round, index)` is a total order — index is unique — so this needs no
    // stability guarantee from the sort and cannot differ between runs.
    order.sort_unstable_by_key(|&i| (rounds[i], i));
    order
}

/// The round each transaction was assigned. Exposed for tests and for the
/// parallel executor, which schedules a round at a time.
pub fn rounds<T: AccessSet>(transactions: &[T]) -> Vec<u32> {
    let mut last_touch: HashMap<Address, u32> = HashMap::new();
    let mut out = Vec::with_capacity(transactions.len());
    for transaction in transactions {
        let mut round = 0u32;
        for key in transaction.access_keys() {
            if let Some(previous) = last_touch.get(&key) {
                round = round.max(previous.saturating_add(1));
            }
        }
        for key in transaction.access_keys() {
            last_touch.insert(key, round);
        }
        out.push(round);
    }
    out
}

/// Removes duplicates, keeping the first occurrence.
///
/// A transaction included in several parallel blocks executes once. The block
/// of first occurrence owns it, which is what decides who is paid its priority
/// fee and what `COINBASE` returns during its execution.
pub fn deduplicate<T, K: std::hash::Hash + Eq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut seen: HashSet<K> = HashSet::new();
    items.into_iter().filter(|item| seen.insert(key(item))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A test transaction: just an access set.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Tx {
        sender: Address,
        touches: Vec<Address>,
    }

    impl AccessSet for Tx {
        fn access_keys(&self) -> impl Iterator<Item = Address> {
            std::iter::once(self.sender).chain(self.touches.iter().copied())
        }
    }

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn tx(sender: u8, touches: &[u8]) -> Tx {
        Tx { sender: addr(sender), touches: touches.iter().copied().map(addr).collect() }
    }

    #[test]
    fn disjoint_transactions_all_land_in_round_zero() {
        let txs = vec![tx(1, &[10]), tx(2, &[20]), tx(3, &[30])];
        assert_eq!(rounds(&txs), vec![0, 0, 0]);
        assert_eq!(layer_and_sort(&txs), vec![0, 1, 2]);
    }

    #[test]
    fn same_sender_transactions_are_strictly_ordered() {
        // P2 in ARCHITECTURE.md §6.4. This is what keeps nonces monotonic.
        let txs = vec![tx(1, &[10]), tx(1, &[20]), tx(1, &[30])];
        assert_eq!(rounds(&txs), vec![0, 1, 2]);
        assert_eq!(layer_and_sort(&txs), vec![0, 1, 2]);
    }

    #[test]
    fn a_hot_address_serialises_everything_touching_it() {
        // OPEN-PROBLEMS.md P-001, demonstrated. Every transaction touches
        // address 99, so every round holds exactly one transaction.
        let txs = vec![tx(1, &[99]), tx(2, &[99]), tx(3, &[99]), tx(4, &[99])];
        assert_eq!(rounds(&txs), vec![0, 1, 2, 3]);
    }

    #[test]
    fn independent_work_interleaves_with_a_hot_address() {
        // Transactions 0 and 2 contend on 99; 1 and 3 are independent and
        // should join round 0 rather than waiting behind the contention.
        let txs = vec![tx(1, &[99]), tx(2, &[20]), tx(3, &[99]), tx(4, &[40])];
        assert_eq!(rounds(&txs), vec![0, 0, 1, 0]);
        // Round 0 first, in base order, then round 1.
        assert_eq!(layer_and_sort(&txs), vec![0, 1, 3, 2]);
    }

    #[test]
    fn ordering_is_a_permutation() {
        // P1 in ARCHITECTURE.md §6.4.
        let txs = vec![tx(1, &[10]), tx(1, &[20]), tx(2, &[10]), tx(3, &[])];
        let order = layer_and_sort(&txs);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..txs.len()).collect::<Vec<_>>());
    }

    #[test]
    fn deduplicate_keeps_the_first_occurrence() {
        let items = vec![("a", 1), ("b", 2), ("a", 3), ("c", 4), ("b", 5)];
        let kept = deduplicate(items, |(k, _)| *k);
        assert_eq!(kept, vec![("a", 1), ("b", 2), ("c", 4)]);
    }

    /// Builds transactions from a compact spec for property testing.
    fn build(spec: &[(u8, Vec<u8>)]) -> Vec<Tx> {
        spec.iter().map(|(s, t)| tx(*s, t)).collect()
    }

    proptest! {
        /// P1: the order is always a permutation of the input.
        #[test]
        fn always_a_permutation(spec in prop::collection::vec((0u8..8, prop::collection::vec(0u8..8, 0..3)), 0..40)) {
            let txs = build(&spec);
            let order = layer_and_sort(&txs);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            prop_assert_eq!(sorted, (0..txs.len()).collect::<Vec<_>>());
        }

        /// P2: transactions from the same sender never change relative order.
        #[test]
        fn sender_order_is_preserved(spec in prop::collection::vec((0u8..6, prop::collection::vec(0u8..8, 0..3)), 0..40)) {
            let txs = build(&spec);
            let order = layer_and_sort(&txs);
            let position: HashMap<usize, usize> =
                order.iter().enumerate().map(|(pos, &i)| (i, pos)).collect();

            for i in 0..txs.len() {
                for j in (i + 1)..txs.len() {
                    if txs[i].sender == txs[j].sender {
                        prop_assert!(
                            position[&i] < position[&j],
                            "sender order inverted between {i} and {j}"
                        );
                    }
                }
            }
        }

        /// P3: the rule is a pure function. Running it twice gives the same
        /// answer, regardless of anything ambient.
        #[test]
        fn is_deterministic(spec in prop::collection::vec((0u8..8, prop::collection::vec(0u8..8, 0..3)), 0..40)) {
            let txs = build(&spec);
            prop_assert_eq!(layer_and_sort(&txs), layer_and_sort(&txs));
        }

        /// P4: within a round, static access sets are pairwise disjoint.
        #[test]
        fn rounds_are_internally_conflict_free(spec in prop::collection::vec((0u8..8, prop::collection::vec(0u8..8, 0..3)), 0..40)) {
            let txs = build(&spec);
            let rounds = rounds(&txs);

            let mut by_round: HashMap<u32, Vec<usize>> = HashMap::new();
            for (i, r) in rounds.iter().enumerate() {
                by_round.entry(*r).or_default().push(i);
            }

            for members in by_round.values() {
                for (a_pos, &a) in members.iter().enumerate() {
                    for &b in &members[(a_pos + 1)..] {
                        let keys_a: HashSet<Address> = txs[a].access_keys().collect();
                        let keys_b: HashSet<Address> = txs[b].access_keys().collect();
                        prop_assert!(
                            keys_a.is_disjoint(&keys_b),
                            "transactions {a} and {b} share a key but are in the same round"
                        );
                    }
                }
            }
        }

        /// Every transaction's round is strictly greater than the round of any
        /// earlier transaction it conflicts with. This is the invariant that
        /// makes round-at-a-time parallel execution equivalent to sequential.
        #[test]
        fn conflicts_always_advance_the_round(spec in prop::collection::vec((0u8..6, prop::collection::vec(0u8..6, 0..3)), 0..30)) {
            let txs = build(&spec);
            let rounds = rounds(&txs);
            for j in 0..txs.len() {
                let keys_j: HashSet<Address> = txs[j].access_keys().collect();
                for i in 0..j {
                    let keys_i: HashSet<Address> = txs[i].access_keys().collect();
                    if !keys_i.is_disjoint(&keys_j) {
                        prop_assert!(
                            rounds[j] > rounds[i],
                            "conflicting {i} (round {}) and {j} (round {})",
                            rounds[i], rounds[j]
                        );
                    }
                }
            }
        }
    }
}
