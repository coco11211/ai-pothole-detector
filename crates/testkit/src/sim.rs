//! The simulation itself.
//!
//! An event queue over virtual time drives N nodes, each running the real
//! [`DagSync`] state machine over the real [`DagStore`], mining real proof of
//! work at a trivially easy target.
//!
//! Events are ordered by `(time, sequence)`. The sequence number breaks ties,
//! so two events scheduled for the same millisecond always fire in the same
//! order — which is what makes the whole run reproducible.

use std::collections::{BinaryHeap, HashSet};

use alloy_primitives::{Address, B256, U256};
use chainname_difficulty::CompactTarget;
use chainname_ghostdag::DagStore;
use chainname_net::{Action, DagSync, Message, PeerId, SyncConfig, sync::HeaderGate};
use chainname_pow::{DoubleKeccak256, PowHash};
use chainname_primitives::{BlockHash, HEADER_VERSION, Header};

use crate::rng::Lcg;

/// Genesis timestamp shared by every simulated network.
const GENESIS_MS: u64 = 1_700_000_000_000;

/// Mining target for simulations: about 16 hashes per block.
///
/// Real proof of work, so the mining and validation paths are genuinely
/// exercised, but cheap enough that the simulation is bound by DAG work rather
/// than by hashing.
fn sim_target() -> U256 {
    U256::MAX >> 4
}

fn sim_bits() -> u32 {
    CompactTarget::from_target(sim_target()).to_u32()
}

/// Header checks a simulated node applies: structure and proof of work.
///
/// Contextual difficulty validation is the node's job, not the network layer's.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureAndPow;

impl HeaderGate for StructureAndPow {
    fn check(&self, header: &Header) -> Result<(), String> {
        header.validate_structure().map_err(|e| e.to_string())?;
        let target = CompactTarget(header.bits).to_target().map_err(|e| e.to_string())?;
        let hash = DoubleKeccak256.hash_header(header);
        if U256::from_be_bytes(hash.0) > target {
            return Err("insufficient work".to_string());
        }
        Ok(())
    }
}

/// Quality of the link between two nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkQuality {
    /// Minimum one-way delay, in milliseconds.
    pub min_latency_ms: u64,
    /// Maximum one-way delay, in milliseconds.
    pub max_latency_ms: u64,
    /// Probability a message is dropped, in parts per thousand.
    pub loss_permille: u64,
}

impl Default for LinkQuality {
    fn default() -> Self {
        // A plausible wide-area link: 50-250ms, 2% loss. The loss figure is
        // deliberately far worse than a real connection, because the point is
        // to exercise the retry path, not to model the internet.
        Self { min_latency_ms: 50, max_latency_ms: 250, loss_permille: 20 }
    }
}

/// Simulation parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimConfig {
    /// Number of nodes.
    pub nodes: usize,
    /// Seed. The whole run is a function of this.
    pub seed: u64,
    /// GHOSTDAG k.
    pub k: u16,
    /// Merge-set size limit.
    pub mergeset_limit: u64,
    /// Mean interval between blocks, network-wide, in milliseconds.
    pub block_interval_ms: u64,
    /// Link quality between every pair of nodes.
    pub link: LinkQuality,
    /// How often each node runs its periodic work.
    pub tick_interval_ms: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            nodes: 5,
            seed: 1,
            k: 18,
            mergeset_limit: 180,
            block_interval_ms: 1_000,
            link: LinkQuality::default(),
            tick_interval_ms: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EventKind {
    Deliver { to: usize, from: usize, message: Message },
    Mine { node: usize },
    Tick { node: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    at_ms: u64,
    sequence: u64,
    kind: EventKind,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap; reverse so the earliest event pops first.
        // The sequence number is the tie-break, which is what makes two runs
        // with the same seed identical rather than merely similar.
        other.at_ms.cmp(&self.at_ms).then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One simulated node.
struct SimNode {
    sync: DagSync<StructureAndPow>,
    miner: Address,
}

/// A running simulation.
pub struct Simulation {
    config: SimConfig,
    nodes: Vec<SimNode>,
    queue: BinaryHeap<Event>,
    rng: Lcg,
    now_ms: u64,
    sequence: u64,
    /// Messages dropped by simulated packet loss.
    pub dropped: u64,
    /// Messages delivered.
    pub delivered: u64,
    /// Blocks mined across all nodes.
    pub mined: u64,
    /// When false, `Mine` events are dropped and not rescheduled. Used by
    /// [`Simulation::quiesce`].
    mining_enabled: bool,
    /// Virtual time at which any node last accepted a block.
    last_change_ms: u64,
}

impl std::fmt::Debug for Simulation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulation")
            .field("nodes", &self.nodes.len())
            .field("now_ms", &self.now_ms)
            .field("mined", &self.mined)
            .field("delivered", &self.delivered)
            .field("dropped", &self.dropped)
            .finish()
    }
}

/// Maps a peer id to a node index, rejecting anything out of range.
///
/// Peer ids are node indices in this harness, but they arrive from the state
/// machine as `u64`, so the conversion is checked rather than assumed.
fn node_index(peer: u64, node_count: usize) -> Option<usize> {
    let index = usize::try_from(peer).ok()?;
    (index < node_count).then_some(index)
}

/// The genesis header every simulated node starts from.
pub fn sim_genesis() -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: GENESIS_MS,
        bits: sim_bits(),
        nonce: 0,
        miner: Address::ZERO,
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: B256::ZERO,
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    }
}

impl Simulation {
    /// Builds a simulation with every node connected to every other.
    pub fn new(config: SimConfig) -> Self {
        let genesis = sim_genesis();
        let genesis_hash = genesis.hash();

        let nodes: Vec<SimNode> = (0..config.nodes)
            .map(|i| SimNode {
                sync: DagSync::new(
                    DagStore::new(genesis.clone(), config.k, config.mergeset_limit),
                    StructureAndPow,
                    SyncConfig::new(genesis_hash),
                ),
                // Distinct miner addresses, so two nodes mining on the same
                // tips at the same instant still produce different blocks --
                // as they would in reality.
                miner: Address::repeat_byte(u8::try_from(i % 255).expect("fits") + 1),
            })
            .collect();

        let mut sim = Self {
            rng: Lcg::new(config.seed),
            config,
            nodes,
            queue: BinaryHeap::new(),
            now_ms: GENESIS_MS,
            sequence: 0,
            dropped: 0,
            delivered: 0,
            mined: 0,
            mining_enabled: true,
            last_change_ms: GENESIS_MS,
        };

        sim.connect_all();
        sim.schedule_initial_events();
        sim
    }

    /// Current virtual time.
    pub const fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// A node's DAG.
    pub fn dag(&self, node: usize) -> &DagStore {
        self.nodes[node].sync.dag()
    }

    /// How many orphans a node is holding.
    pub fn orphan_count(&self, node: usize) -> usize {
        self.nodes[node].sync.orphan_count()
    }

    /// How many block requests a node is still waiting on.
    pub fn pending_request_count(&self, node: usize) -> usize {
        self.nodes[node].sync.pending_request_count()
    }

    /// How many peers a node still considers connected.
    pub fn peer_count(&self, node: usize) -> usize {
        self.nodes[node].sync.peer_count()
    }

    /// A one-line-per-node summary, for diagnosing a stalled run.
    pub fn diagnostics(&self) -> String {
        let mut out = format!(
            "t={}ms mined={} delivered={} dropped={} last_change={}ms ago\n",
            self.now_ms - GENESIS_MS,
            self.mined,
            self.delivered,
            self.dropped,
            self.now_ms.saturating_sub(self.last_change_ms),
        );
        for (i, node) in self.nodes.iter().enumerate() {
            out.push_str(&format!(
                "  node {i}: blocks={} orphans={} pending={} peers={} tip={}\n",
                node.sync.dag().len(),
                node.sync.orphan_count(),
                node.sync.pending_request_count(),
                node.sync.peer_count(),
                node.sync.dag().virtual_selected_parent(),
            ));
        }
        out
    }

    fn connect_all(&mut self) {
        // Peer ids are node indices, so routing an action is a direct lookup.
        for i in 0..self.nodes.len() {
            for j in 0..self.nodes.len() {
                if i == j {
                    continue;
                }
                let actions = self.nodes[i].sync.on_connect(PeerId(j as u64));
                self.dispatch(i, actions);
            }
        }
    }

    fn schedule_initial_events(&mut self) {
        for node in 0..self.nodes.len() {
            self.schedule_next_mine(node);
            let at = self.now_ms + self.config.tick_interval_ms;
            self.push(at, EventKind::Tick { node });
        }
    }

    fn push(&mut self, at_ms: u64, kind: EventKind) {
        self.sequence += 1;
        self.queue.push(Event { at_ms, sequence: self.sequence, kind });
    }

    /// Schedules a node's next block.
    ///
    /// Each node mines at `block_interval_ms * node_count`, so the network as a
    /// whole produces one block per interval. The interval is jittered over
    /// `[0.2x, 2.0x]` so blocks do not arrive in lockstep, which would hide the
    /// concurrent-block behaviour the DAG exists to handle.
    fn schedule_next_mine(&mut self, node: usize) {
        let mean = self.config.block_interval_ms * self.nodes.len() as u64;
        let delay = self.rng.between(mean / 5, mean * 2);
        let at = self.now_ms + delay.max(1);
        self.push(at, EventKind::Mine { node });
    }

    /// Runs until virtual time reaches `until_ms` after genesis.
    pub fn run_for(&mut self, duration_ms: u64) {
        let deadline = self.now_ms + duration_ms;
        while let Some(event) = self.queue.pop() {
            if event.at_ms > deadline {
                // Put it back so a later `run_for` resumes correctly.
                self.queue.push(event);
                break;
            }
            self.now_ms = event.at_ms;
            self.handle(event.kind);
        }
        self.now_ms = deadline;
    }

    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::Deliver { to, from, message } => {
                self.delivered += 1;
                let now = self.now_ms;
                let actions = self.nodes[to].sync.on_message(PeerId(from as u64), message, now);
                self.dispatch(to, actions);
                self.note_changes(to);
            }
            EventKind::Mine { node } => {
                if self.mining_enabled {
                    self.mine_one(node);
                }
                // Always reschedule, even while mining is suspended. Dropping
                // the event would mean a node never mines again after the
                // first `quiesce`, which silently turned a thirty-minute run
                // into a one-minute one.
                self.schedule_next_mine(node);
            }
            EventKind::Tick { node } => {
                let now = self.now_ms;
                let actions = self.nodes[node].sync.on_tick(now);
                self.dispatch(node, actions);
                let at = now + self.config.tick_interval_ms;
                self.push(at, EventKind::Tick { node });
            }
        }
    }

    /// Mines one block on a node's current tips.
    fn mine_one(&mut self, node: usize) {
        let mut parents = self.nodes[node].sync.dag().tips();
        // A block may name at most this many parents; take the highest-work
        // tips, which is what a real miner would do.
        parents.truncate(16);
        parents.sort_unstable();
        parents.dedup();
        if parents.is_empty() {
            return;
        }

        let target = sim_target();
        let mut header = Header {
            version: HEADER_VERSION,
            parents,
            timestamp_ms: self.now_ms,
            bits: sim_bits(),
            nonce: 0,
            miner: self.nodes[node].miner,
            txs_root: alloy_trie::EMPTY_ROOT_HASH,
            deferred_height: 0,
            deferred_state_root: B256::ZERO,
            deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
            deferred_gas_used: 0,
        };

        // Real proof of work at an easy target.
        let mut found = false;
        for nonce in 0..100_000u64 {
            header.nonce = nonce;
            if U256::from_be_bytes(DoubleKeccak256.hash_header(&header).0) <= target {
                found = true;
                break;
            }
        }
        if !found {
            return;
        }

        self.mined += 1;
        let actions = self.nodes[node].sync.on_local_block(header);
        self.dispatch(node, actions);
        self.note_changes(node);
    }

    /// Records that a node's DAG changed, for quiescence detection.
    fn note_changes(&mut self, node: usize) {
        if !self.nodes[node].sync.drain_accepted().is_empty() {
            self.last_change_ms = self.now_ms;
        }
    }

    /// How long the network must be quiet before it counts as settled.
    ///
    /// One tick interval (so periodic tip reconciliation has had a chance to
    /// run and find nothing) plus three maximum latencies (so anything that
    /// reconciliation *did* find has had time to travel and be answered).
    const fn settle_window_ms(&self) -> u64 {
        self.config.tick_interval_ms + 3 * self.config.link.max_latency_ms
    }

    /// Turns a node's actions into scheduled deliveries, applying loss and
    /// latency.
    fn dispatch(&mut self, from: usize, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send(PeerId(to), message) => {
                    let Some(to) = node_index(to, self.nodes.len()) else { continue };
                    if self.rng.chance_permille(self.config.link.loss_permille) {
                        self.dropped += 1;
                        continue;
                    }
                    let latency = self
                        .rng
                        .between(self.config.link.min_latency_ms, self.config.link.max_latency_ms);
                    let at = self.now_ms + latency;
                    self.push(at, EventKind::Deliver { to, from, message });
                }
                Action::Disconnect(PeerId(to), reason) => {
                    tracing::warn!(from, to, reason, "simulated disconnect");
                    // Symmetric, as a real socket close would be. Modelling it
                    // one-sided would let the simulation drift into states no
                    // real network can reach.
                    self.nodes[from].sync.on_disconnect(PeerId(to));
                    if let Some(to) = node_index(to, self.nodes.len()) {
                        self.nodes[to].sync.on_disconnect(PeerId(from as u64));
                    }
                }
            }
        }
    }

    /// Stops mining and runs until the network has nothing left in flight.
    ///
    /// Convergence is a statement about a *settled* network. Checking it while
    /// messages are still in flight would fail for a block mined a hundred
    /// milliseconds ago that has simply not arrived yet — which is correct
    /// behaviour, not divergence. This drains the network first so the
    /// assertion means what it says.
    ///
    /// Returns false if the network had not settled within `max_ms`, which is
    /// itself a failure worth reporting.
    pub fn quiesce(&mut self, max_ms: u64) -> bool {
        self.mining_enabled = false;
        let deadline = self.now_ms + max_ms;

        // Note: "no traffic in flight" is NOT the criterion. Periodic tip
        // reconciliation means there is always chatter, by design. What
        // settles is DAG *state*: nothing outstanding, and no node has
        // accepted a block for a full propagation window.
        let settled = |sim: &Self| {
            let unresolved = sim
                .nodes
                .iter()
                .any(|n| n.sync.orphan_count() > 0 || n.sync.pending_request_count() > 0);
            !unresolved && sim.now_ms.saturating_sub(sim.last_change_ms) >= sim.settle_window_ms()
        };

        while self.now_ms < deadline {
            if settled(self) {
                self.mining_enabled = true;
                return true;
            }
            let Some(event) = self.queue.pop() else { break };
            if event.at_ms > deadline {
                self.queue.push(event);
                break;
            }
            self.now_ms = event.at_ms;
            self.handle(event.kind);
        }

        let ok = settled(self);
        self.mining_enabled = true;
        ok
    }

    /// The set of block hashes a node holds.
    pub fn block_set(&self, node: usize) -> HashSet<BlockHash> {
        let dag = self.nodes[node].sync.dag();
        // Reconstruct by walking every block reachable from the tips.
        let mut seen = HashSet::new();
        let mut stack: Vec<BlockHash> = dag.tips();
        while let Some(hash) = stack.pop() {
            if !seen.insert(hash) {
                continue;
            }
            if let Some(header) = dag.header(hash) {
                stack.extend(header.parents.iter().copied());
            }
        }
        seen
    }

    /// Checks every node agrees on the DAG and on the chain it selects.
    ///
    /// Returns a description of the first disagreement, or `None`.
    pub fn divergence(&self) -> Option<String> {
        let reference_blocks = self.block_set(0);
        let reference_tip = self.nodes[0].sync.dag().virtual_selected_parent();
        let reference_chain = self.nodes[0].sync.dag().selected_parent_chain(reference_tip);

        for node in 1..self.nodes.len() {
            let blocks = self.block_set(node);
            if blocks != reference_blocks {
                let missing = reference_blocks.difference(&blocks).count();
                let extra = blocks.difference(&reference_blocks).count();
                return Some(format!(
                    "node {node} block set differs from node 0: \
                     {missing} missing, {extra} extra \
                     (node 0 has {}, node {node} has {})",
                    reference_blocks.len(),
                    blocks.len()
                ));
            }

            let dag = self.nodes[node].sync.dag();
            let tip = dag.virtual_selected_parent();
            if tip != reference_tip {
                return Some(format!(
                    "node {node} selected a different chain tip: {tip} vs {reference_tip}"
                ));
            }
            if dag.selected_parent_chain(tip) != reference_chain {
                return Some(format!("node {node} derived a different selected parent chain"));
            }
        }
        None
    }

    /// Number of blocks node 0 holds.
    pub fn dag_size(&self) -> usize {
        self.nodes[0].sync.dag().len()
    }
}
