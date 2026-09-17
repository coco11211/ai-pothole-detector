//! The DAG store and the GHOSTDAG colouring algorithm.
//!
//! # The algorithm, in one paragraph
//!
//! Each block picks a **selected parent**: the parent with the most
//! accumulated blue work. Its **merge set** is everything in its past that is
//! not in the selected parent's past. Each merge-set block is then coloured
//! blue or red by the *k-cluster* rule: a block stays blue only if adding it
//! keeps every blue block's blue-anticone below `k`. **Blue score** counts
//! blue blocks; **blue work** sums their proof of work. The selected parent
//! chain — follow selected parents from the tip — is the canonical chain, and
//! it is what execution runs along.
//!
//! # Reachability
//!
//! `is_ancestor_of` is a memoised breadth-first search over parent edges. That
//! is correct and simple, and it is O(|past|) per query in the worst case.
//! Kaspa uses interval-labelled reachability to make this O(1); we do not, yet.
//! Recorded as OPEN-PROBLEMS.md P-009 — a performance ceiling, not a
//! correctness gap.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use alloy_primitives::U256;
use chainname_difficulty::CompactTarget;
use chainname_primitives::{BlockHash, Header};

use crate::work::work_for_target;

/// GHOSTDAG data derived for one block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostdagData {
    /// The parent with the most accumulated blue work.
    pub selected_parent: BlockHash,
    /// Number of blue blocks in this block's past, inclusive of itself.
    pub blue_score: u64,
    /// Accumulated proof of work over the blue set.
    pub blue_work: U256,
    /// Merge-set blocks coloured blue, selected parent first, then in
    /// topological order.
    pub mergeset_blues: Vec<BlockHash>,
    /// Merge-set blocks coloured red.
    pub mergeset_reds: Vec<BlockHash>,
    /// The whole merge set — blues and reds together — in one deterministic
    /// topological order, **excluding** the selected parent.
    ///
    /// This is what execution consumes. Red blocks' transactions are included:
    /// folding orphans into the ledger instead of discarding them is the point
    /// of using a DAG. Redness costs a block its contribution to blue score,
    /// not its transactions.
    pub mergeset_ordered: Vec<BlockHash>,
    /// Longest path from genesis, in blocks.
    ///
    /// Strictly increasing along ancestry by construction: every block is one
    /// deeper than its deepest parent. That makes it a *sound* bound for
    /// pruning reachability searches, which blue score is not — blue score
    /// counts blocks while selected-parent choice compares work, so the two
    /// can disagree whenever difficulty varies, and a prune based on it would
    /// silently return wrong answers.
    pub topological_height: u64,
    /// For each blue in `mergeset_blues`, how many blues sit in its anticone.
    /// Carried forward so descendants can extend the k-cluster check without
    /// recomputing it from scratch.
    pub blues_anticone_sizes: HashMap<BlockHash, u16>,
}

impl GhostdagData {
    /// The merge set in colouring order: blues first, then reds.
    ///
    /// Not the execution order — see [`crate::ordering`].
    pub fn mergeset(&self) -> impl Iterator<Item = &BlockHash> {
        self.mergeset_blues.iter().chain(self.mergeset_reds.iter())
    }
}

/// Outcome of testing one merge-set block against the k-cluster rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Colour {
    /// Keeps the k-cluster property: colour it blue.
    Blue,
    /// Violates it: colour it red.
    Red,
}

/// An in-memory block DAG with GHOSTDAG data.
#[derive(Debug)]
pub struct DagStore {
    k: u16,
    genesis: BlockHash,
    headers: HashMap<BlockHash, Header>,
    data: HashMap<BlockHash, GhostdagData>,
    /// Child edges.
    children: HashMap<BlockHash, Vec<BlockHash>>,
    /// Blocks with no children, maintained incrementally.
    ///
    /// Recomputing this by scanning every header was O(blocks) per call, and
    /// it is called on every mine, every announcement and every execution
    /// advance — which made simply building a DAG quadratic in its size.
    /// `BTreeSet` so iteration order never depends on hashing.
    tips: BTreeSet<BlockHash>,
    /// Memoised `(descendant, ancestor) -> bool`.
    ///
    /// A `Mutex` rather than a `RefCell`: the RPC layer shares a `DagStore`
    /// across threads, and a `RefCell` would make the whole node `!Sync`.
    /// Contention is negligible because queries are short and the common case
    /// is a cache hit.
    reachability: std::sync::Mutex<HashMap<(BlockHash, BlockHash), bool>>,
    /// Upper bound on merge-set size, a denial-of-service guard.
    mergeset_size_limit: u64,
}

impl DagStore {
    /// Creates a store rooted at `genesis`.
    pub fn new(genesis: Header, k: u16, mergeset_size_limit: u64) -> Self {
        let genesis_hash = genesis.hash();
        let genesis_work =
            work_for_target(CompactTarget(genesis.bits).to_target().unwrap_or(U256::MAX));

        let mut headers = HashMap::new();
        headers.insert(genesis_hash, genesis);

        let mut data = HashMap::new();
        data.insert(
            genesis_hash,
            GhostdagData {
                // Genesis is its own selected parent. It is the one block with
                // no parent, and making it self-referential means chain walks
                // terminate on a value rather than on an `Option`.
                selected_parent: genesis_hash,
                blue_score: 0,
                blue_work: genesis_work,
                mergeset_blues: Vec::new(),
                mergeset_reds: Vec::new(),
                mergeset_ordered: Vec::new(),
                topological_height: 0,
                blues_anticone_sizes: HashMap::new(),
            },
        );

        Self {
            k,
            genesis: genesis_hash,
            headers,
            data,
            children: HashMap::new(),
            tips: BTreeSet::from([genesis_hash]),
            reachability: std::sync::Mutex::new(HashMap::new()),
            mergeset_size_limit,
        }
    }

    /// The genesis block's hash.
    pub const fn genesis(&self) -> BlockHash {
        self.genesis
    }

    /// The GHOSTDAG `k` parameter.
    pub const fn k(&self) -> u16 {
        self.k
    }

    /// Number of blocks in the DAG.
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    /// True if the DAG holds only genesis.
    pub fn is_empty(&self) -> bool {
        self.headers.len() <= 1
    }

    /// True if the block is present.
    pub fn contains(&self, hash: BlockHash) -> bool {
        self.headers.contains_key(&hash)
    }

    /// A block's header.
    pub fn header(&self, hash: BlockHash) -> Option<&Header> {
        self.headers.get(&hash)
    }

    /// A block's GHOSTDAG data.
    pub fn data(&self, hash: BlockHash) -> Option<&GhostdagData> {
        self.data.get(&hash)
    }

    /// Blocks with no children: the DAG's tips.
    ///
    /// Sorted by `(blue_work, hash)` descending, so the result is a canonical
    /// order that two nodes with the same DAG always agree on. A miner takes
    /// its parents from the front of this list.
    pub fn tips(&self) -> Vec<BlockHash> {
        let mut tips: Vec<BlockHash> = self.tips.iter().copied().collect();
        tips.sort_by(|a, b| self.compare_blocks(*b, *a));
        tips
    }

    /// The tip with the most blue work: the head of the selected parent chain.
    pub fn virtual_selected_parent(&self) -> BlockHash {
        self.tips().first().copied().unwrap_or(self.genesis)
    }

    /// The selected parent chain from `tip` back to genesis, tip first.
    pub fn selected_parent_chain(&self, tip: BlockHash) -> Vec<BlockHash> {
        let mut chain = Vec::new();
        let mut cursor = tip;
        loop {
            chain.push(cursor);
            if cursor == self.genesis {
                break;
            }
            match self.data.get(&cursor) {
                Some(data) => cursor = data.selected_parent,
                None => break,
            }
        }
        chain
    }

    /// Adds a block, computing its GHOSTDAG data.
    ///
    /// Requires every parent to be present already. Callers hold orphans
    /// outside the DAG until their parents arrive.
    pub fn add_block(&mut self, header: Header) -> Result<BlockHash, DagError> {
        let hash = header.hash();
        if self.headers.contains_key(&hash) {
            return Err(DagError::AlreadyPresent(hash));
        }
        if header.parents.is_empty() {
            return Err(DagError::NoParents(hash));
        }
        for parent in &header.parents {
            if !self.headers.contains_key(parent) {
                return Err(DagError::MissingParent { block: hash, parent: *parent });
            }
        }

        let data = self.compute_ghostdag(&header)?;

        for parent in &header.parents {
            self.children.entry(*parent).or_default().push(hash);
            // A block with a child is no longer a tip.
            self.tips.remove(parent);
        }
        self.tips.insert(hash);
        self.headers.insert(hash, header);
        self.data.insert(hash, data);
        Ok(hash)
    }

    /// Computes GHOSTDAG data for a block that is not yet in the store.
    fn compute_ghostdag(&self, header: &Header) -> Result<GhostdagData, DagError> {
        let hash = header.hash();
        let selected_parent = self.select_parent(&header.parents);
        let selected_data = self
            .data
            .get(&selected_parent)
            .ok_or(DagError::MissingParent { block: hash, parent: selected_parent })?;

        let mergeset = self.ordered_mergeset(header, selected_parent)?;

        // The selected parent is always blue and always first.
        let mut mergeset_blues = vec![selected_parent];
        let mut mergeset_reds = Vec::new();
        let mut blues_anticone_sizes: HashMap<BlockHash, u16> = HashMap::new();
        blues_anticone_sizes.insert(selected_parent, 0);

        for candidate in &mergeset {
            let (colour, candidate_anticone, updates) = self.colour_candidate(
                *candidate,
                selected_parent,
                &mergeset_blues,
                &blues_anticone_sizes,
            );

            match colour {
                Colour::Blue => {
                    mergeset_blues.push(*candidate);
                    blues_anticone_sizes.insert(*candidate, candidate_anticone);
                    for (blue, size) in updates {
                        blues_anticone_sizes.insert(blue, size);
                    }
                }
                Colour::Red => mergeset_reds.push(*candidate),
            }
        }

        // Blue score counts the selected parent's blues plus this block's new
        // ones, excluding the selected parent itself (already counted).
        let blue_score = selected_data.blue_score + (mergeset_blues.len() as u64);

        let mut blue_work = selected_data.blue_work;
        for blue in mergeset_blues.iter().skip(1) {
            blue_work = blue_work.saturating_add(self.block_work(*blue));
        }
        blue_work = blue_work.saturating_add(self.header_work(header));

        let topological_height =
            header.parents.iter().map(|p| self.topological_height(*p)).max().unwrap_or(0) + 1;

        Ok(GhostdagData {
            selected_parent,
            blue_score,
            blue_work,
            topological_height,
            mergeset_blues,
            mergeset_reds,
            mergeset_ordered: mergeset,
            blues_anticone_sizes,
        })
    }

    /// Picks the parent with the most blue work, breaking ties by hash so the
    /// choice is total and every node makes it identically.
    fn select_parent(&self, parents: &[BlockHash]) -> BlockHash {
        let mut best = parents[0];
        for parent in &parents[1..] {
            if self.compare_blocks(*parent, best) == std::cmp::Ordering::Greater {
                best = *parent;
            }
        }
        best
    }

    /// Orders two blocks by `(blue_work, hash)`.
    ///
    /// A total order with no ties: block hashes are unique. This is what makes
    /// selected-parent choice deterministic across nodes.
    fn compare_blocks(&self, a: BlockHash, b: BlockHash) -> std::cmp::Ordering {
        let work_a = self.data.get(&a).map(|d| d.blue_work).unwrap_or_default();
        let work_b = self.data.get(&b).map(|d| d.blue_work).unwrap_or_default();
        work_a.cmp(&work_b).then_with(|| a.cmp(&b))
    }

    /// `past(block) \ past(selected_parent)`, in topological order.
    ///
    /// Ordered by `(blue_work, hash)` ascending among blocks whose parents have
    /// all been emitted, which is a deterministic topological sort.
    fn ordered_mergeset(
        &self,
        header: &Header,
        selected_parent: BlockHash,
    ) -> Result<Vec<BlockHash>, DagError> {
        let hash = header.hash();
        let mut mergeset = Vec::new();
        let mut seen: HashSet<BlockHash> = HashSet::new();
        let mut queue: VecDeque<BlockHash> = VecDeque::new();

        for parent in &header.parents {
            if *parent != selected_parent && seen.insert(*parent) {
                queue.push_back(*parent);
            }
        }

        while let Some(current) = queue.pop_front() {
            // Anything already in the selected parent's past is, by
            // definition, not in the merge set.
            if self.is_ancestor_of(current, selected_parent) {
                continue;
            }
            mergeset.push(current);
            if mergeset.len() as u64 > self.mergeset_size_limit {
                return Err(DagError::MergesetTooLarge {
                    block: hash,
                    limit: self.mergeset_size_limit,
                });
            }
            if let Some(parents) = self.headers.get(&current).map(|h| h.parents.clone()) {
                for parent in parents {
                    if parent != selected_parent && seen.insert(parent) {
                        queue.push_back(parent);
                    }
                }
            }
        }

        // Deterministic topological order: repeatedly emit the smallest
        // available block by (blue_work, hash) whose merge-set parents have
        // already been emitted.
        Ok(self.topological_sort(&mergeset))
    }

    /// Sorts a set of blocks into a deterministic topological order.
    fn topological_sort(&self, blocks: &[BlockHash]) -> Vec<BlockHash> {
        let members: HashSet<BlockHash> = blocks.iter().copied().collect();
        let mut emitted: HashSet<BlockHash> = HashSet::new();
        let mut remaining: Vec<BlockHash> = blocks.to_vec();
        // Ascending by (blue_work, hash): the canonical tie-break.
        remaining.sort_by(|a, b| self.compare_blocks(*a, *b));

        let mut out = Vec::with_capacity(blocks.len());
        while !remaining.is_empty() {
            let mut progressed = false;
            let mut next_remaining = Vec::with_capacity(remaining.len());

            for block in remaining {
                let ready = self
                    .headers
                    .get(&block)
                    .map(|h| h.parents.iter().all(|p| !members.contains(p) || emitted.contains(p)))
                    .unwrap_or(true);
                if ready {
                    emitted.insert(block);
                    out.push(block);
                    progressed = true;
                } else {
                    next_remaining.push(block);
                }
            }

            if !progressed {
                // Unreachable for a well-formed DAG: a cycle would be needed.
                // Emit deterministically rather than looping forever.
                out.extend(next_remaining);
                break;
            }
            remaining = next_remaining;
        }
        out
    }

    /// Applies the k-cluster rule to one merge-set candidate.
    ///
    /// Returns the colour, the candidate's blue-anticone size, and the updated
    /// anticone sizes of the existing blues it was found to be concurrent with.
    fn colour_candidate(
        &self,
        candidate: BlockHash,
        selected_parent: BlockHash,
        mergeset_blues: &[BlockHash],
        blues_anticone_sizes: &HashMap<BlockHash, u16>,
    ) -> (Colour, u16, Vec<(BlockHash, u16)>) {
        // `mergeset_blues` holds at most k+1 entries: the selected parent
        // plus k more. Written as `> k` rather than `>= k + 1` to keep clippy
        // quiet; the bound is the same.
        if mergeset_blues.len() as u64 > u64::from(self.k) {
            return (Colour::Red, 0, Vec::new());
        }

        let mut candidate_anticone: u16 = 0;
        let mut updates: Vec<(BlockHash, u16)> = Vec::new();

        // Blues introduced by this very block, other than the selected parent.
        for blue in mergeset_blues {
            if self.is_ancestor_of(*blue, candidate) {
                // In the candidate's past, so not in its anticone.
                continue;
            }
            candidate_anticone += 1;
            if candidate_anticone > self.k {
                return (Colour::Red, 0, Vec::new());
            }
            let existing = blues_anticone_sizes.get(blue).copied().unwrap_or(0);
            if existing >= self.k {
                // Colouring the candidate blue would push this blue's own
                // anticone past k, breaking the cluster for a block that is
                // already blue.
                return (Colour::Red, 0, Vec::new());
            }
            updates.push((*blue, existing + 1));
        }

        // Then the blues already established along the selected parent chain.
        let mut chain_block = selected_parent;
        loop {
            if self.is_ancestor_of(chain_block, candidate) {
                // Every remaining blue is in this chain block's past, hence in
                // the candidate's past. Its anticone cannot grow further.
                break;
            }

            let Some(chain_data) = self.data.get(&chain_block) else { break };
            for blue in &chain_data.mergeset_blues {
                if self.is_ancestor_of(*blue, candidate) {
                    continue;
                }
                candidate_anticone += 1;
                if candidate_anticone > self.k {
                    return (Colour::Red, 0, Vec::new());
                }
                let existing = chain_data.blues_anticone_sizes.get(blue).copied().unwrap_or(0);
                if existing >= self.k {
                    return (Colour::Red, 0, Vec::new());
                }
                updates.push((*blue, existing + 1));
            }

            if chain_block == self.genesis {
                break;
            }
            chain_block = chain_data.selected_parent;
        }

        (Colour::Blue, candidate_anticone, updates)
    }

    /// A block's longest path from genesis.
    pub fn topological_height(&self, hash: BlockHash) -> u64 {
        self.data.get(&hash).map_or(0, |d| d.topological_height)
    }

    /// True if `ancestor` is in `descendant`'s past, or is `descendant`.
    ///
    /// Breadth-first search over parent edges, pruned by topological height
    /// and memoised.
    ///
    /// The prune is what makes this usable. Topological height strictly
    /// increases along ancestry, so once the search reaches a block no deeper
    /// than `ancestor`, `ancestor` cannot lie in that block's past and the
    /// whole branch can be abandoned. Without it the search visits the entire
    /// past on every miss, which made building a DAG quadratic in its size: a
    /// 24-hour simulated soak would have taken about half a day of real time.
    ///
    /// Blue score would NOT be a sound bound here. It counts blocks while
    /// selected-parent choice compares work, so the two disagree whenever
    /// difficulty varies, and a prune based on it would silently return wrong
    /// answers on exactly the chains where difficulty moved.
    ///
    /// The prune is applied *after* comparing the parent against `ancestor`,
    /// so the target itself is never pruned away.
    ///
    /// Still not Kaspa's interval-labelled reachability, which answers in
    /// O(1). OPEN-PROBLEMS.md P-009 stays open; this makes it affordable, not
    /// free.
    pub fn is_ancestor_of(&self, ancestor: BlockHash, descendant: BlockHash) -> bool {
        if ancestor == descendant {
            return true;
        }
        if let Some(cached) = self
            .reachability
            .lock()
            .expect("reachability cache is never poisoned")
            .get(&(descendant, ancestor))
        {
            return *cached;
        }

        let ancestor_height = self.topological_height(ancestor);
        // A block no deeper than the one being looked for cannot contain it.
        if self.topological_height(descendant) <= ancestor_height {
            self.remember(descendant, ancestor, false);
            return false;
        }

        let mut seen: HashSet<BlockHash> = HashSet::new();
        let mut queue: VecDeque<BlockHash> = VecDeque::new();
        queue.push_back(descendant);
        seen.insert(descendant);

        let mut found = false;
        'search: while let Some(current) = queue.pop_front() {
            let Some(header) = self.headers.get(&current) else { continue };
            for parent in &header.parents {
                if *parent == ancestor {
                    found = true;
                    break 'search;
                }
                if self.topological_height(*parent) <= ancestor_height {
                    continue;
                }
                if seen.insert(*parent) {
                    queue.push_back(*parent);
                }
            }
        }

        self.remember(descendant, ancestor, found);
        found
    }

    /// Caches a reachability answer, bounding the cache.
    ///
    /// Queries are unbounded over a node's lifetime, so the cache needs a
    /// ceiling or a long-running node leaks. Clearing wholesale rather than
    /// evicting individually is crude, but it costs nothing to maintain and a
    /// cold cache is a slowdown rather than a bug.
    fn remember(&self, descendant: BlockHash, ancestor: BlockHash, answer: bool) {
        /// Entries held before the cache is dropped.
        const CACHE_CAPACITY: usize = 32_768;

        let mut cache = self.reachability.lock().expect("reachability cache is never poisoned");
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert((descendant, ancestor), answer);
    }

    fn block_work(&self, hash: BlockHash) -> U256 {
        self.headers.get(&hash).map(|h| self.header_work(h)).unwrap_or_default()
    }

    fn header_work(&self, header: &Header) -> U256 {
        work_for_target(CompactTarget(header.bits).to_target().unwrap_or(U256::MAX))
    }
}

/// Reasons a block could not join the DAG.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DagError {
    /// The block is already present.
    #[error("block {0} is already in the DAG")]
    AlreadyPresent(BlockHash),
    /// The block declares no parents. Only genesis may do that, and genesis is
    /// installed directly rather than added.
    #[error("block {0} has no parents")]
    NoParents(BlockHash),
    /// A declared parent is not in the DAG. The caller should hold the block
    /// as an orphan until it arrives.
    #[error("block {block} references missing parent {parent}")]
    MissingParent {
        /// The block being added.
        block: BlockHash,
        /// The parent that is absent.
        parent: BlockHash,
    },
    /// The merge set exceeds the configured limit.
    #[error("block {block} has a merge set larger than the limit of {limit}")]
    MergesetTooLarge {
        /// The block being added.
        block: BlockHash,
        /// The configured limit.
        limit: u64,
    },
}
