//! DAG synchronisation: a pure state machine.
//!
//! [`DagSync`] takes messages in and returns [`Action`]s out. It does no I/O,
//! owns no clock, and holds no sockets — time arrives as a parameter. That is
//! deliberate: it is what lets the multi-node harness run five nodes through
//! thirty simulated minutes deterministically, and what turns a divergence
//! into something reproducible from a seed rather than a heisenbug.
//!
//! # How a block propagates
//!
//! A node that accepts a block announces it (`InvBlocks`) to every ready peer
//! not already known to have it. A peer that does not have it asks
//! (`GetBlocks`) and receives it (`Blocks`). Flood relay, Kaspa style: no
//! structured overlay, no stake-weighted tree, because there is no stake to
//! weight by.
//!
//! # Orphans
//!
//! A block whose parents have not arrived cannot join the DAG. It is held in a
//! bounded pool and its missing parents are requested in a single batch, so
//! sync walks backwards one round trip per DAG level rather than one per
//! block.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use chainname_ghostdag::{DagError, DagStore};
use chainname_primitives::{BlockHash, Header};
use tracing::{debug, trace};

use crate::{
    message::{BlockPayload, MAX_BLOCK_BATCH, MAX_INV_ENTRIES, Message, PROTOCOL_VERSION},
    peer::{Misbehaviour, PeerId, PeerState},
};

/// A cheap, context-free header check: structure and proof of work.
///
/// Contextual validation — that the difficulty is the one the retarget rule
/// requires — needs chain state and happens in the node, not here. Keeping
/// this narrow is what stops `chainname-net` depending on consensus.
pub trait HeaderGate {
    /// Returns an error describing why the header is unacceptable.
    fn check(&self, header: &Header) -> Result<(), String>;
}

/// A gate that accepts everything. For tests that are not about validation.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptAll;

impl HeaderGate for AcceptAll {
    fn check(&self, _header: &Header) -> Result<(), String> {
        Ok(())
    }
}

/// Something the caller must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a message to one peer.
    Send(PeerId, Message),
    /// Drop a peer, with the reason for the log.
    Disconnect(PeerId, &'static str),
}

/// Tunable limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// Largest number of orphans held at once.
    ///
    /// Orphans are attacker-controlled: anyone can send blocks referencing
    /// parents that do not exist. The pool is bounded and evicts oldest-first.
    pub max_orphans: usize,
    /// How long to wait for a requested block before asking someone else.
    pub request_timeout_ms: u64,
    /// Our genesis hash. A peer claiming a different one is on another network.
    pub genesis: BlockHash,
}

impl SyncConfig {
    /// Defaults for a node on the given network.
    pub fn new(genesis: BlockHash) -> Self {
        Self {
            // ~16 MiB of headers at worst. Large enough to absorb a deep sync,
            // small enough that a hostile peer cannot exhaust memory.
            max_orphans: 4_096,
            // Well above any plausible round trip, so a slow honest peer is not
            // treated as unresponsive, and low enough that a stalled request
            // does not hold up sync for long.
            request_timeout_ms: 10_000,
            genesis,
        }
    }
}

/// A block waiting for its parents.
#[derive(Debug, Clone)]
struct Orphan {
    header: Header,
    /// Which peer supplied it, so a peer that floods junk orphans can be
    /// penalised when they turn out to be unresolvable.
    from: PeerId,
}

/// The synchronisation state machine.
#[derive(Debug)]
pub struct DagSync<G> {
    dag: DagStore,
    gate: G,
    config: SyncConfig,
    /// `BTreeMap`, not `HashMap`: iteration order feeds directly into the
    /// actions we emit, and `HashMap` order varies per process. That made the
    /// multi-node simulation non-reproducible, which defeats the point of
    /// seeding it. Determinism here is a requirement, not a nicety.
    peers: BTreeMap<PeerId, PeerState>,
    /// Orphans by hash.
    orphans: HashMap<BlockHash, Orphan>,
    /// Insertion order, for oldest-first eviction.
    orphan_order: VecDeque<BlockHash>,
    /// Missing parent hash -> orphans waiting on it.
    /// Ordered, for the same reason as `peers`.
    waiting_on: HashMap<BlockHash, BTreeSet<BlockHash>>,
    /// Hashes requested from some peer, with the time of the request.
    requested: HashMap<BlockHash, (PeerId, u64)>,
    /// Block bodies, as opaque EIP-2718 transaction envelopes.
    ///
    /// Held here rather than in the DAG because the DAG is about topology and
    /// a header is two hundred bytes while a body can be megabytes. Decoding
    /// and sender recovery happen above this layer, where the cost can be
    /// charged to somebody.
    bodies: HashMap<BlockHash, Vec<alloy_primitives::Bytes>>,
    /// Blocks accepted since the last drain, for diagnostics and tests.
    accepted: Vec<BlockHash>,
    /// Rotating offset for spreading retries across peers.
    retry_cursor: u64,
}

impl<G: HeaderGate> DagSync<G> {
    /// Creates a state machine over an existing DAG.
    pub fn new(dag: DagStore, gate: G, config: SyncConfig) -> Self {
        Self {
            dag,
            gate,
            config,
            peers: BTreeMap::new(),
            orphans: HashMap::new(),
            orphan_order: VecDeque::new(),
            waiting_on: HashMap::new(),
            requested: HashMap::new(),
            bodies: HashMap::new(),
            accepted: Vec::new(),
            retry_cursor: 0,
        }
    }

    /// The DAG this node has built.
    pub const fn dag(&self) -> &DagStore {
        &self.dag
    }

    /// Mutable access, for the node to add locally mined blocks.
    pub const fn dag_mut(&mut self) -> &mut DagStore {
        &mut self.dag
    }

    /// Number of orphans currently held.
    pub fn orphan_count(&self) -> usize {
        self.orphans.len()
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Blocks requested from a peer and not yet received.
    ///
    /// Used by the simulation harness to tell "still syncing" from "settled".
    pub fn pending_request_count(&self) -> usize {
        self.requested.len()
    }

    /// A block's transactions, as opaque EIP-2718 envelopes.
    pub fn body(&self, hash: BlockHash) -> &[alloy_primitives::Bytes] {
        self.bodies.get(&hash).map_or(&[], Vec::as_slice)
    }

    /// Hashes accepted since the last call, clearing the buffer.
    pub fn drain_accepted(&mut self) -> Vec<BlockHash> {
        std::mem::take(&mut self.accepted)
    }

    /// Registers a new connection and opens the handshake.
    pub fn on_connect(&mut self, peer: PeerId) -> Vec<Action> {
        self.peers.insert(peer, PeerState::new(peer));
        vec![Action::Send(
            peer,
            Message::Version {
                version: PROTOCOL_VERSION,
                genesis: self.config.genesis,
                tips: self.dag.tips(),
            },
        )]
    }

    /// Forgets a peer and releases anything it owed us, so those blocks can be
    /// requested from someone else.
    pub fn on_disconnect(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        self.requested.retain(|_, (owner, _)| *owner != peer);
    }

    /// Announces a locally mined block.
    pub fn on_local_block(&mut self, block: BlockPayload) -> Vec<Action> {
        let hash = block.hash();
        self.bodies.insert(hash, block.transactions);
        if self.dag.add_block(block.header).is_ok() {
            self.accepted.push(hash);
            let mut actions = self.announce(hash, None);
            // A locally mined block can be the parent an orphan was waiting
            // for — rare, but leaving it out would strand that orphan
            // permanently.
            actions.extend(self.resolve_orphans_of(hash, 0));
            return actions;
        }
        Vec::new()
    }

    /// Handles a decoded message.
    pub fn on_message(&mut self, peer: PeerId, message: Message, now_ms: u64) -> Vec<Action> {
        if !self.peers.contains_key(&peer) {
            return vec![Action::Disconnect(peer, "message from unknown peer")];
        }

        // A message from a peer we have not finished handshaking with is not
        // misbehaviour: it means our `Version` was lost in flight and the peer
        // believes the connection is up. Re-sending ours repairs it.
        //
        // An earlier version penalised this and disconnected after four such
        // messages. Under heavy packet loss that turned a single dropped
        // handshake into a permanently broken connection, and a six-node
        // network at 40% loss slowly disconnected itself until it could no
        // longer converge. Handshake loss must be recoverable, because on a
        // lossy link it is not rare, it is expected.
        let ready = self.peers[&peer].is_ready();
        if !ready && !matches!(message, Message::Version { .. } | Message::Verack) {
            trace!(%peer, "message before handshake completed; resending version");
            return vec![Action::Send(
                peer,
                Message::Version {
                    version: PROTOCOL_VERSION,
                    genesis: self.config.genesis,
                    tips: self.dag.tips(),
                },
            )];
        }

        match message {
            Message::Version { version, genesis, tips } => {
                self.on_version(peer, version, genesis, tips, now_ms)
            }
            Message::Verack => {
                if let Some(state) = self.peers.get_mut(&peer) {
                    state.handshake = crate::peer::Handshake::Ready;
                }
                Vec::new()
            }
            Message::Ping(nonce) => vec![Action::Send(peer, Message::Pong(nonce))],
            Message::Pong(_) => Vec::new(),
            Message::GetTips => vec![Action::Send(peer, Message::Tips(self.dag.tips()))],
            Message::Tips(tips) => self.request_unknown(peer, tips, now_ms),
            Message::InvBlocks(hashes) => self.on_inv_blocks(peer, hashes, now_ms),
            Message::GetBlocks(hashes) => self.on_get_blocks(peer, hashes),
            Message::Blocks(blocks) => self.on_blocks(peer, blocks, now_ms),
            // Transaction relay is wired at M7 with the mempool. Until then the
            // messages parse and are ignored rather than being a protocol error,
            // so a newer peer talking to an older one is not disconnected.
            Message::InvTxs(_) | Message::GetTxs(_) => Vec::new(),
        }
    }

    /// Periodic work: re-request timed-out blocks, and reconcile tips.
    ///
    /// Tip reconciliation is what makes convergence *eventual* rather than
    /// merely likely. Flood relay announces a block once; if that announcement
    /// is lost, nothing re-sends it. Usually a later block rescues the gap,
    /// because its parents get requested and the missing ancestor is pulled in
    /// as an orphan resolves — but the newest blocks have no descendants yet,
    /// so a lost announcement for a tip is never recovered. Asking peers for
    /// their tips on a timer closes that hole for good.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<Action> {
        let timeout = self.config.request_timeout_ms;
        let mut stale: Vec<BlockHash> = self
            .requested
            .iter()
            .filter(|(_, (_, asked_at))| now_ms.saturating_sub(*asked_at) > timeout)
            .map(|(hash, _)| *hash)
            .collect();
        // Sorted: `requested` is a HashMap and its order must not reach the
        // wire.
        stale.sort_unstable();

        for hash in &stale {
            self.requested.remove(hash);
        }

        let ready: Vec<PeerId> =
            self.peers.values().filter(|p| p.is_ready()).map(|p| p.id).collect();
        let unready: Vec<PeerId> =
            self.peers.values().filter(|p| !p.is_ready()).map(|p| p.id).collect();

        let mut actions = Vec::new();

        // Retry the handshake with anyone it has not completed with. On a
        // lossy link the opening `Version` is simply lost sometimes, and
        // without this the connection stays half-open forever.
        for peer in unready {
            actions.push(Action::Send(
                peer,
                Message::Version {
                    version: PROTOCOL_VERSION,
                    genesis: self.config.genesis,
                    tips: self.dag.tips(),
                },
            ));
        }

        if !stale.is_empty() && !ready.is_empty() {
            debug!(count = stale.len(), "re-requesting timed-out blocks");

            // Spread retries across *different* peers each round, rotating the
            // starting point. Re-asking whoever already failed to answer is a
            // deadlock: a peer that does not have a block never will, and
            // `request_from` suppresses duplicate in-flight requests, so the
            // hash stays stuck forever and every later attempt is filtered
            // out. That stalled the five-node network permanently, with a
            // handful of blocks nobody could ever obtain.
            self.retry_cursor = self.retry_cursor.wrapping_add(1);
            // `% ready.len()` first, so the value is always in range before
            // it is narrowed.
            let start = usize::try_from(self.retry_cursor % ready.len() as u64)
                .expect("value is already below ready.len()");

            let mut buckets: Vec<Vec<BlockHash>> = vec![Vec::new(); ready.len()];
            for (i, hash) in stale.iter().enumerate() {
                buckets[(start + i) % ready.len()].push(*hash);
            }
            for (i, bucket) in buckets.into_iter().enumerate() {
                if !bucket.is_empty() {
                    actions.extend(self.request_from(ready[i], bucket, now_ms));
                }
            }
        }

        for peer in ready {
            actions.push(Action::Send(peer, Message::GetTips));
        }

        actions
    }

    fn on_version(
        &mut self,
        peer: PeerId,
        version: u32,
        genesis: BlockHash,
        tips: Vec<BlockHash>,
        now_ms: u64,
    ) -> Vec<Action> {
        if version != PROTOCOL_VERSION {
            return self.penalise(peer, Misbehaviour::WrongVersion, "protocol version mismatch");
        }
        if genesis != self.config.genesis {
            return self.penalise(peer, Misbehaviour::WrongGenesis, "different network");
        }

        let Some(state) = self.peers.get_mut(&peer) else { return Vec::new() };
        state.handshake = crate::peer::Handshake::Ready;
        for tip in &tips {
            state.note_known(*tip);
        }

        let mut actions = vec![Action::Send(peer, Message::Verack)];
        actions.extend(self.request_unknown(peer, tips, now_ms));
        actions
    }

    fn on_inv_blocks(&mut self, peer: PeerId, hashes: Vec<BlockHash>, now_ms: u64) -> Vec<Action> {
        if let Some(state) = self.peers.get_mut(&peer) {
            for hash in &hashes {
                state.note_known(*hash);
            }
        }
        self.request_unknown(peer, hashes, now_ms)
    }

    fn on_get_blocks(&mut self, peer: PeerId, hashes: Vec<BlockHash>) -> Vec<Action> {
        let blocks: Vec<BlockPayload> = hashes
            .iter()
            .take(MAX_BLOCK_BATCH)
            .filter_map(|hash| {
                self.dag.header(*hash).cloned().map(|header| BlockPayload {
                    header,
                    transactions: self.bodies.get(hash).cloned().unwrap_or_default(),
                })
            })
            .collect();

        if blocks.is_empty() {
            return Vec::new();
        }
        if let Some(state) = self.peers.get_mut(&peer) {
            for block in &blocks {
                state.note_known(block.hash());
            }
        }
        vec![Action::Send(peer, Message::Blocks(blocks))]
    }

    fn on_blocks(&mut self, peer: PeerId, blocks: Vec<BlockPayload>, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();

        for block in blocks {
            let BlockPayload { header, transactions } = block;
            let hash = header.hash();
            self.requested.remove(&hash);
            if let Some(state) = self.peers.get_mut(&peer) {
                state.note_known(hash);
            }

            if self.dag.contains(hash) || self.orphans.contains_key(&hash) {
                // Already have it. This is *normal* under flood relay, not
                // misbehaviour: every peer announces every block, and two
                // responses racing is the expected case, not an attack.
                //
                // An earlier version penalised this. Over thirty simulated
                // minutes the penalties accumulated until honest nodes
                // disconnected each other and the network partitioned. Sending
                // redundant data is a bandwidth concern, and bandwidth is
                // bounded by rate limiting, not by reputation.
                // OPEN-PROBLEMS.md P-011.
                trace!(%peer, %hash, "duplicate block, ignoring");
                continue;
            }

            if let Err(reason) = self.gate.check(&header) {
                debug!(%peer, %hash, reason, "rejecting block");
                actions.extend(self.penalise(peer, Misbehaviour::InvalidBlock, "invalid block"));
                if actions.iter().any(|a| matches!(a, Action::Disconnect(..))) {
                    return actions;
                }
                continue;
            }

            // Store the body before admitting: once the header is in the DAG
            // the block is executable, and a body arriving second would be a
            // race no caller can defend against.
            self.bodies.insert(hash, transactions);
            actions.extend(self.admit(header, peer, now_ms));
        }

        actions
    }

    /// Adds a validated header to the DAG, or holds it as an orphan.
    fn admit(&mut self, header: Header, from: PeerId, now_ms: u64) -> Vec<Action> {
        let hash = header.hash();

        match self.dag.add_block(header.clone()) {
            Ok(_) => {
                trace!(%hash, "block accepted");
                self.accepted.push(hash);
                if let Some(state) = self.peers.get_mut(&from) {
                    state.blocks_delivered += 1;
                    state.reward(1);
                }
                let mut actions = self.announce(hash, Some(from));
                actions.extend(self.resolve_orphans_of(hash, now_ms));
                actions
            }
            Err(DagError::MissingParent { .. }) => {
                let missing: Vec<BlockHash> = header
                    .parents
                    .iter()
                    .filter(|p| !self.dag.contains(**p) && !self.orphans.contains_key(*p))
                    .copied()
                    .collect();

                self.store_orphan(hash, header, from);
                // Request every missing parent in one batch: sync then costs
                // one round trip per DAG level rather than per block.
                self.request_from(from, missing, now_ms)
            }
            Err(error) => {
                debug!(%hash, %error, "block not added");
                Vec::new()
            }
        }
    }

    fn store_orphan(&mut self, hash: BlockHash, header: Header, from: PeerId) {
        // Evict oldest-first before inserting, so the pool never exceeds its
        // bound even momentarily.
        while self.orphans.len() >= self.config.max_orphans {
            let Some(oldest) = self.orphan_order.pop_front() else { break };
            if let Some(evicted) = self.orphans.remove(&oldest) {
                for parent in &evicted.header.parents {
                    if let Some(set) = self.waiting_on.get_mut(parent) {
                        set.remove(&oldest);
                        if set.is_empty() {
                            self.waiting_on.remove(parent);
                        }
                    }
                }
            }
        }

        for parent in &header.parents {
            if !self.dag.contains(*parent) {
                self.waiting_on.entry(*parent).or_default().insert(hash);
            }
        }
        self.orphans.insert(hash, Orphan { header, from });
        self.orphan_order.push_back(hash);
    }

    /// After `hash` joins the DAG, admits any orphans that were waiting on it.
    ///
    /// Iterative rather than recursive: a long orphan chain would otherwise be
    /// a stack-overflow vector a peer controls.
    fn resolve_orphans_of(&mut self, hash: BlockHash, now_ms: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut queue = VecDeque::from([hash]);

        while let Some(parent) = queue.pop_front() {
            let Some(children) = self.waiting_on.remove(&parent) else { continue };
            // `BTreeSet`, so this iterates in hash order rather than whatever
            // the allocator happened to produce.
            for child in children {
                let Some(orphan) = self.orphans.get(&child) else { continue };
                // Only try once every parent is present.
                if !orphan.header.parents.iter().all(|p| self.dag.contains(*p)) {
                    continue;
                }
                let Some(orphan) = self.orphans.remove(&child) else { continue };
                self.orphan_order.retain(|h| *h != child);

                if self.dag.add_block(orphan.header).is_ok() {
                    self.accepted.push(child);
                    actions.extend(self.announce(child, Some(orphan.from)));
                    queue.push_back(child);
                }
            }
        }

        let _ = now_ms;
        actions
    }

    /// Tells every ready peer, except `exclude`, that we have `hash`.
    fn announce(&mut self, hash: BlockHash, exclude: Option<PeerId>) -> Vec<Action> {
        let targets: Vec<PeerId> = self
            .peers
            .values()
            .filter(|p| p.is_ready() && Some(p.id) != exclude && !p.known_blocks.contains(&hash))
            .map(|p| p.id)
            .collect();

        for target in &targets {
            if let Some(state) = self.peers.get_mut(target) {
                state.note_known(hash);
            }
        }

        targets.into_iter().map(|peer| Action::Send(peer, Message::InvBlocks(vec![hash]))).collect()
    }

    /// Requests the subset of `hashes` we neither have nor have already asked for.
    fn request_unknown(
        &mut self,
        peer: PeerId,
        hashes: Vec<BlockHash>,
        now_ms: u64,
    ) -> Vec<Action> {
        let wanted: Vec<BlockHash> = hashes
            .into_iter()
            .filter(|h| !self.dag.contains(*h) && !self.orphans.contains_key(h))
            .collect();
        self.request_from(peer, wanted, now_ms)
    }

    /// Issues a `GetBlocks`, skipping anything already in flight.
    fn request_from(&mut self, peer: PeerId, hashes: Vec<BlockHash>, now_ms: u64) -> Vec<Action> {
        let wanted: Vec<BlockHash> = hashes
            .into_iter()
            .filter(|h| !self.requested.contains_key(h))
            .take(MAX_INV_ENTRIES)
            .collect();

        if wanted.is_empty() {
            return Vec::new();
        }
        for hash in &wanted {
            self.requested.insert(*hash, (peer, now_ms));
        }
        vec![Action::Send(peer, Message::GetBlocks(wanted))]
    }

    fn penalise(&mut self, peer: PeerId, what: Misbehaviour, reason: &'static str) -> Vec<Action> {
        let banned = self.peers.get_mut(&peer).is_some_and(|state| state.penalise(what));
        if banned { vec![Action::Disconnect(peer, reason)] } else { Vec::new() }
    }
}
