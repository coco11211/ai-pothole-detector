//! The peer-to-peer runtime.
//!
//! Turns [`DagSync`], which is a pure state machine, into a running node by
//! giving it sockets, a clock, and a place to send its actions. The state
//! machine stays ignorant of all three — that separation is what lets the same
//! logic be driven deterministically by the simulation harness and
//! nondeterministically by real TCP, and be the same logic in both.
//!
//! One task per connection reading frames, one shared registry of outbound
//! channels, one ticker. The `DagSync` itself is behind a mutex: it is a state
//! machine, so every message must be applied in some serial order anyway, and
//! pretending otherwise would only move the serialisation somewhere less
//! obvious.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
};
use tracing::{debug, info, warn};

use crate::{
    message::Message,
    peer::PeerId,
    sync::{Action, DagSync, HeaderGate},
    transport::{read_frame, write_frame},
};

/// How many messages may queue for a peer before it is considered stuck.
///
/// A slow peer must not be allowed to make us buffer without limit. When the
/// channel fills, the connection is dropped rather than the node's memory
/// growing: a peer that cannot keep up is not useful, and dropping it is the
/// backpressure.
const PEER_SEND_QUEUE: usize = 1_024;

/// Configuration for the peer-to-peer runtime.
#[derive(Debug, Clone)]
pub struct P2pConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Peers to dial at startup.
    pub dial: Vec<SocketAddr>,
    /// How often to run periodic work.
    pub tick_interval_ms: u64,
    /// How long to wait between redial attempts.
    pub redial_interval_ms: u64,
}

impl P2pConfig {
    /// Defaults for a listening node.
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            dial: Vec::new(),
            tick_interval_ms: 2_000,
            // Long enough not to hammer an unreachable peer, short enough that
            // a restarted peer is picked up promptly.
            redial_interval_ms: 5_000,
        }
    }
}

/// Outbound channels, one per connected peer.
type Registry = Arc<Mutex<HashMap<PeerId, mpsc::Sender<Message>>>>;

/// A running peer-to-peer node.
#[derive(Debug)]
pub struct P2pNode<G> {
    sync: Arc<Mutex<DagSync<G>>>,
    registry: Registry,
    next_peer_id: Arc<AtomicU64>,
    /// The address actually bound, which matters when port 0 was requested.
    local_addr: SocketAddr,
}

impl<G: HeaderGate + Send + 'static> P2pNode<G> {
    /// Starts listening and dialling. Returns once the listener is bound, with
    /// background tasks running.
    pub async fn start(sync: DagSync<G>, config: P2pConfig) -> std::io::Result<Self> {
        let listener = TcpListener::bind(config.listen).await?;
        let local_addr = listener.local_addr()?;

        let node = Self {
            sync: Arc::new(Mutex::new(sync)),
            registry: Arc::new(Mutex::new(HashMap::new())),
            next_peer_id: Arc::new(AtomicU64::new(0)),
            local_addr,
        };

        node.spawn_accept_loop(listener);
        node.spawn_ticker(config.tick_interval_ms);
        for address in config.dial {
            node.spawn_dialer(address, config.redial_interval_ms);
        }

        info!(%local_addr, "peer-to-peer listening");
        Ok(node)
    }

    /// The address this node is listening on.
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Shared access to the sync state machine, for mining and for tests.
    pub fn sync(&self) -> Arc<Mutex<DagSync<G>>> {
        Arc::clone(&self.sync)
    }

    /// How many peers are connected.
    pub async fn peer_count(&self) -> usize {
        self.registry.lock().await.len()
    }

    /// Applies a batch of actions produced by the state machine.
    async fn dispatch(registry: &Registry, actions: Vec<Action>) {
        let mut stuck: Vec<PeerId> = Vec::new();

        {
            let table = registry.lock().await;
            for action in actions {
                match action {
                    Action::Send(peer, message) => {
                        let Some(sender) = table.get(&peer) else { continue };
                        // `try_send`, not `send`: blocking here would let one
                        // slow peer stall every other peer's traffic, because
                        // the dispatch holds the registry lock.
                        if sender.try_send(message).is_err() {
                            stuck.push(peer);
                        }
                    }
                    Action::Disconnect(peer, reason) => {
                        warn!(%peer, reason, "disconnecting peer");
                        stuck.push(peer);
                    }
                }
            }
        }

        if !stuck.is_empty() {
            let mut table = registry.lock().await;
            for peer in stuck {
                table.remove(&peer);
            }
        }
    }

    fn spawn_accept_loop(&self, listener: TcpListener) {
        let sync = Arc::clone(&self.sync);
        let registry = Arc::clone(&self.registry);
        let next_id = Arc::clone(&self.next_peer_id);

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, address)) => {
                        let peer = PeerId(next_id.fetch_add(1, Ordering::Relaxed));
                        debug!(%peer, %address, "inbound connection");
                        spawn_connection(stream, peer, Arc::clone(&sync), Arc::clone(&registry));
                    }
                    Err(error) => {
                        // An accept failure is usually transient (fd limits).
                        // Logging and continuing beats tearing down the node.
                        warn!(%error, "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
    }

    fn spawn_dialer(&self, address: SocketAddr, redial_interval_ms: u64) {
        let sync = Arc::clone(&self.sync);
        let registry = Arc::clone(&self.registry);
        let next_id = Arc::clone(&self.next_peer_id);

        tokio::spawn(async move {
            loop {
                match TcpStream::connect(address).await {
                    Ok(stream) => {
                        let peer = PeerId(next_id.fetch_add(1, Ordering::Relaxed));
                        debug!(%peer, %address, "outbound connection");
                        let handle = spawn_connection(
                            stream,
                            peer,
                            Arc::clone(&sync),
                            Arc::clone(&registry),
                        );
                        // Redial once the connection ends, so a restarted peer
                        // is picked back up without operator action.
                        let _ = handle.await;
                        debug!(%address, "connection closed, will redial");
                    }
                    Err(error) => {
                        debug!(%address, %error, "dial failed, will retry");
                    }
                }
                tokio::time::sleep(Duration::from_millis(redial_interval_ms)).await;
            }
        });
    }

    fn spawn_ticker(&self, interval_ms: u64) {
        let sync = Arc::clone(&self.sync);
        let registry = Arc::clone(&self.registry);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms.max(1)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let actions = {
                    let mut guard = sync.lock().await;
                    guard.on_tick(now_ms())
                };
                Self::dispatch(&registry, actions).await;
            }
        });
    }
}

/// Runs one connection: a read loop and a write loop.
fn spawn_connection<G: HeaderGate + Send + 'static>(
    stream: TcpStream,
    peer: PeerId,
    sync: Arc<Mutex<DagSync<G>>>,
    registry: Registry,
) -> tokio::task::JoinHandle<()> {
    // Disable Nagle: consensus messages are small and latency-sensitive, and
    // coalescing them adds delay that directly widens the DAG.
    let _ = stream.set_nodelay(true);

    let (mut reader, mut writer) = stream.into_split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(PEER_SEND_QUEUE);

    // Write loop.
    tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if write_frame(&mut writer, &message.encode_to_vec()).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        {
            let mut table = registry.lock().await;
            table.insert(peer, outbound_tx);
        }

        // Opening handshake.
        let actions = {
            let mut guard = sync.lock().await;
            guard.on_connect(peer)
        };
        P2pNode::<G>::dispatch(&registry, actions).await;

        loop {
            match read_frame(&mut reader).await {
                Ok(Some(bytes)) => {
                    let actions = match Message::decode_from_slice(&bytes) {
                        Ok(message) => {
                            let mut guard = sync.lock().await;
                            guard.on_message(peer, message, now_ms())
                        }
                        Err(error) => {
                            // Malformed bytes are immediately fatal: a peer
                            // that cannot frame a message is of no use, and
                            // continuing to read from it is free work for an
                            // attacker.
                            warn!(%peer, %error, "malformed message");
                            break;
                        }
                    };
                    P2pNode::<G>::dispatch(&registry, actions).await;
                }
                Ok(None) => {
                    debug!(%peer, "peer disconnected");
                    break;
                }
                Err(error) => {
                    warn!(%peer, %error, "connection error");
                    break;
                }
            }
        }

        registry.lock().await.remove(&peer);
        // Release anything this peer owed us so it can be requested elsewhere.
        sync.lock().await.on_disconnect(peer);
    })
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// The state machine takes time as a parameter precisely so this function
/// exists in exactly one place and the simulation can supply its own.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Broadcasts a locally produced block to every connected peer.
pub async fn announce_local_block<G: HeaderGate + Send + 'static>(
    node: &P2pNode<G>,
    block: crate::message::BlockPayload,
) {
    let actions = {
        let mut guard = node.sync.lock().await;
        guard.on_local_block(block)
    };
    P2pNode::<G>::dispatch(&node.registry, actions).await;
}
