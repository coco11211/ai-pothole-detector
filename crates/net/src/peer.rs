//! Peer state and scoring.
//!
//! **No stake weighting.** There is no stake on this chain, and there is no
//! substitute for it here either — no allowlist, no reputation carried between
//! sessions, no identity that costs anything to create. A peer's standing comes
//! only from what it has done on this connection.
//!
//! That means scoring must be robust to Sybils: a misbehaving peer is cheap to
//! replace, so the score exists to bound the damage one connection can do, not
//! to identify bad actors durably.

use std::fmt;

/// A connection identifier, assigned locally.
///
/// Deliberately not a public key or an address: identity on this network is
/// per-connection and costs nothing to mint, so treating it as meaningful
/// would be a mistake waiting to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub u64);

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peer{}", self.0)
    }
}

/// Starting score for a new peer.
pub const INITIAL_SCORE: i32 = 100;
/// At or below this, the peer is disconnected.
pub const BAN_THRESHOLD: i32 = 0;
/// Ceiling on score, so a long-lived peer cannot bank credit and then spend it
/// on a burst of abuse.
pub const MAX_SCORE: i32 = 200;

/// Things a peer can do wrong, and what each costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misbehaviour {
    /// Sent bytes that are not a valid message.
    MalformedMessage,
    /// Spoke a protocol version we do not.
    WrongVersion,
    /// Claimed a different genesis: a different network entirely.
    WrongGenesis,
    /// Sent a message before completing the handshake.
    OutOfOrder,
    /// Sent a block that fails validation.
    InvalidBlock,
    /// Sent a block nobody asked for and which was already known.
    ///
    /// Retained for completeness but **not applied to duplicate blocks**:
    /// under flood relay a duplicate is the expected case, and penalising it
    /// partitions an honest network. See `DagSync::on_blocks`.
    UnsolicitedBlock,
    /// Answered a request with data that does not match it.
    IrrelevantResponse,
}

impl Misbehaviour {
    /// Score penalty.
    ///
    /// Penalties are graded by what the behaviour proves. A malformed message
    /// or a wrong genesis proves the peer is useless to us, so it is
    /// immediately fatal. An invalid block proves dishonesty or a serious bug
    /// and is nearly fatal. An unsolicited duplicate is more likely a race
    /// than an attack, so it is cheap.
    pub const fn penalty(self) -> i32 {
        match self {
            Self::MalformedMessage | Self::WrongVersion | Self::WrongGenesis => INITIAL_SCORE,
            Self::InvalidBlock => 50,
            Self::OutOfOrder => 25,
            Self::IrrelevantResponse => 10,
            Self::UnsolicitedBlock => 1,
        }
    }
}

/// How far a peer has got through the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Handshake {
    /// Connected, nothing exchanged.
    #[default]
    New,
    /// We have seen their `Version` and sent ours.
    VersionExchanged,
    /// Fully established.
    Ready,
}

/// Everything known about one connected peer.
#[derive(Debug, Clone)]
pub struct PeerState {
    /// Local connection identifier.
    pub id: PeerId,
    /// Handshake progress.
    pub handshake: Handshake,
    /// Behavioural score.
    pub score: i32,
    /// Blocks this peer is known to have, so we do not re-announce them.
    pub known_blocks: std::collections::HashSet<chainname_primitives::BlockHash>,
    /// Blocks we have requested from this peer and are still waiting for.
    pub in_flight: std::collections::HashSet<chainname_primitives::BlockHash>,
    /// Valid blocks this peer has delivered. Used only for diagnostics.
    pub blocks_delivered: u64,
}

impl PeerState {
    /// A freshly connected peer.
    pub fn new(id: PeerId) -> Self {
        Self {
            id,
            handshake: Handshake::New,
            score: INITIAL_SCORE,
            known_blocks: std::collections::HashSet::new(),
            in_flight: std::collections::HashSet::new(),
            blocks_delivered: 0,
        }
    }

    /// True once the handshake has completed.
    pub const fn is_ready(&self) -> bool {
        matches!(self.handshake, Handshake::Ready)
    }

    /// Applies a penalty. Returns true if the peer should now be disconnected.
    pub fn penalise(&mut self, what: Misbehaviour) -> bool {
        self.score = self.score.saturating_sub(what.penalty());
        self.score <= BAN_THRESHOLD
    }

    /// Credits the peer for useful work, up to [`MAX_SCORE`].
    pub fn reward(&mut self, amount: i32) {
        self.score = (self.score + amount).min(MAX_SCORE);
    }

    /// Records that the peer is known to hold a block.
    ///
    /// The set is bounded: past the cap the oldest knowledge is dropped
    /// wholesale. Re-announcing a block a peer already has is wasteful but
    /// harmless, whereas an unbounded set is a memory leak a peer controls.
    pub fn note_known(&mut self, hash: chainname_primitives::BlockHash) {
        const KNOWN_BLOCKS_CAP: usize = 8_192;
        if self.known_blocks.len() >= KNOWN_BLOCKS_CAP {
            self.known_blocks.clear();
        }
        self.known_blocks.insert(hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn a_new_peer_starts_unbanned_and_not_ready() {
        let peer = PeerState::new(PeerId(1));
        assert_eq!(peer.score, INITIAL_SCORE);
        assert!(!peer.is_ready());
    }

    #[test]
    fn a_malformed_message_is_immediately_fatal() {
        let mut peer = PeerState::new(PeerId(1));
        assert!(peer.penalise(Misbehaviour::MalformedMessage));
    }

    #[test]
    fn a_wrong_genesis_is_immediately_fatal() {
        let mut peer = PeerState::new(PeerId(1));
        assert!(peer.penalise(Misbehaviour::WrongGenesis));
    }

    #[test]
    fn two_invalid_blocks_are_fatal() {
        let mut peer = PeerState::new(PeerId(1));
        assert!(!peer.penalise(Misbehaviour::InvalidBlock));
        assert!(peer.penalise(Misbehaviour::InvalidBlock));
    }

    #[test]
    fn unsolicited_duplicates_are_tolerated_for_a_long_time() {
        // These are much more likely a relay race than an attack.
        let mut peer = PeerState::new(PeerId(1));
        for _ in 0..(INITIAL_SCORE - 1) {
            assert!(!peer.penalise(Misbehaviour::UnsolicitedBlock));
        }
        assert!(peer.penalise(Misbehaviour::UnsolicitedBlock));
    }

    #[test]
    fn score_cannot_be_banked_above_the_ceiling() {
        let mut peer = PeerState::new(PeerId(1));
        peer.reward(10_000);
        assert_eq!(peer.score, MAX_SCORE);
    }

    #[test]
    fn known_blocks_are_bounded() {
        let mut peer = PeerState::new(PeerId(1));
        for i in 0..20_000u32 {
            peer.note_known(B256::from(alloy_primitives::U256::from(i)));
        }
        assert!(peer.known_blocks.len() <= 8_192, "known set must stay bounded");
    }
}
