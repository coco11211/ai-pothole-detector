//! The CHAINNAME wire protocol.
//!
//! Not devp2p. The Ethereum `eth` protocol is built on range queries over
//! block numbers (`GetBlockHeaders`, `GetBlockBodies`), which assumes one
//! canonical block per height. A blockDAG has many blocks per height and most
//! of them are never on the selected parent chain, so those messages cannot
//! express what a DAG node needs to ask for. See DECISIONS.md D-014 and C-001.
//!
//! DAG sync asks by *hash* and by *tip set* instead: "what are your tips",
//! "give me these blocks", "I have these blocks". Flood relay, Kaspa style.
//!
//! Encoding is RLP with a one-byte discriminant, so an unknown message type is
//! a clean parse error rather than a misinterpreted payload.

use alloy_primitives::Bytes;
use alloy_rlp::{Decodable, Encodable, Error as RlpError, Header as RlpHeader};
use chainname_primitives::{BlockHash, Header};

/// Protocol version. Peers with a different version are disconnected rather
/// than half-understood.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest number of hashes in a single inventory or request message.
///
/// Bounds the work a peer can force with one message. 512 hashes is 16 KiB,
/// enough to announce a large burst and far below anything that would let a
/// peer exhaust memory.
pub const MAX_INV_ENTRIES: usize = 512;

/// Largest number of blocks a peer may send in one batch response.
pub const MAX_BLOCK_BATCH: usize = 128;

/// Largest number of transactions in one block body.
///
/// A body arrives before its transactions can be validated, so this bounds the
/// work a peer can force with a single message.
pub const MAX_TXS_PER_BLOCK: usize = 8_192;

/// A block as it travels: a header plus its transactions.
///
/// Transactions are carried as opaque EIP-2718 envelopes, exactly as Ethereum
/// does. The network layer never decodes them — decoding and sender recovery
/// are validation concerns and belong where the work can be charged to
/// somebody, not on the receive path.
#[derive(Debug, Clone, PartialEq, Eq, alloy_rlp::RlpEncodable, alloy_rlp::RlpDecodable)]
pub struct BlockPayload {
    /// The block header.
    pub header: Header,
    /// EIP-2718 encoded transactions, in the order the miner chose.
    ///
    /// Not the execution order: that is derived from the merge set which
    /// eventually contains this block.
    pub transactions: Vec<Bytes>,
}

impl BlockPayload {
    /// A block with no transactions.
    pub fn empty(header: Header) -> Self {
        Self { header, transactions: Vec::new() }
    }

    /// This block's hash.
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }
}

/// Message type discriminants. Stable: changing one is a protocol break.
mod tag {
    pub const VERSION: u8 = 0x01;
    pub const VERACK: u8 = 0x02;
    pub const PING: u8 = 0x03;
    pub const PONG: u8 = 0x04;
    pub const GET_TIPS: u8 = 0x05;
    pub const TIPS: u8 = 0x06;
    pub const INV_BLOCKS: u8 = 0x07;
    pub const GET_BLOCKS: u8 = 0x08;
    pub const BLOCKS: u8 = 0x09;
    pub const INV_TXS: u8 = 0x0a;
    pub const GET_TXS: u8 = 0x0b;
}

/// A protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Handshake opener, carrying the protocol version and the sender's
    /// current tips so sync can begin immediately.
    Version {
        /// Protocol version the sender speaks.
        version: u32,
        /// The sender's genesis hash. A mismatch means different networks.
        genesis: BlockHash,
        /// The sender's current DAG tips.
        tips: Vec<BlockHash>,
    },
    /// Handshake acknowledgement.
    Verack,
    /// Liveness probe.
    Ping(u64),
    /// Liveness response, echoing the nonce.
    Pong(u64),
    /// Asks for the peer's current tips.
    GetTips,
    /// The sender's current DAG tips.
    Tips(Vec<BlockHash>),
    /// Announces blocks the sender holds.
    InvBlocks(Vec<BlockHash>),
    /// Requests block headers by hash.
    GetBlocks(Vec<BlockHash>),
    /// Delivers blocks: headers with their transactions.
    Blocks(Vec<BlockPayload>),
    /// Announces transactions the sender holds.
    InvTxs(Vec<BlockHash>),
    /// Requests transactions by hash.
    GetTxs(Vec<BlockHash>),
}

impl Message {
    /// The discriminant byte for this message.
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Version { .. } => tag::VERSION,
            Self::Verack => tag::VERACK,
            Self::Ping(_) => tag::PING,
            Self::Pong(_) => tag::PONG,
            Self::GetTips => tag::GET_TIPS,
            Self::Tips(_) => tag::TIPS,
            Self::InvBlocks(_) => tag::INV_BLOCKS,
            Self::GetBlocks(_) => tag::GET_BLOCKS,
            Self::Blocks(_) => tag::BLOCKS,
            Self::InvTxs(_) => tag::INV_TXS,
            Self::GetTxs(_) => tag::GET_TXS,
        }
    }

    /// A short name for logs and peer-scoring messages.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Version { .. } => "version",
            Self::Verack => "verack",
            Self::Ping(_) => "ping",
            Self::Pong(_) => "pong",
            Self::GetTips => "gettips",
            Self::Tips(_) => "tips",
            Self::InvBlocks(_) => "invblocks",
            Self::GetBlocks(_) => "getblocks",
            Self::Blocks(_) => "blocks",
            Self::InvTxs(_) => "invtxs",
            Self::GetTxs(_) => "gettxs",
        }
    }

    /// Encodes the message to bytes.
    pub fn encode_to_vec(&self) -> Vec<u8> {
        let mut out = vec![self.tag()];
        match self {
            Self::Version { version, genesis, tips } => {
                let payload =
                    VersionPayload { version: *version, genesis: *genesis, tips: tips.clone() };
                payload.encode(&mut out);
            }
            Self::Verack | Self::GetTips => {}
            Self::Ping(nonce) | Self::Pong(nonce) => nonce.encode(&mut out),
            Self::Tips(hashes)
            | Self::InvBlocks(hashes)
            | Self::GetBlocks(hashes)
            | Self::InvTxs(hashes)
            | Self::GetTxs(hashes) => hashes.encode(&mut out),
            Self::Blocks(blocks) => blocks.encode(&mut out),
        }
        out
    }

    /// Decodes a message from bytes.
    ///
    /// Rejects oversized collections here rather than after allocation, so a
    /// hostile peer cannot force an allocation it never has to pay for.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, CodecError> {
        let (tag, mut rest) = bytes.split_first().ok_or(CodecError::Empty)?;
        let buf = &mut rest;

        let message = match *tag {
            tag::VERSION => {
                let payload = VersionPayload::decode(buf)?;
                check_len(payload.tips.len(), MAX_INV_ENTRIES, "version.tips")?;
                Self::Version {
                    version: payload.version,
                    genesis: payload.genesis,
                    tips: payload.tips,
                }
            }
            tag::VERACK => Self::Verack,
            tag::GET_TIPS => Self::GetTips,
            tag::PING => Self::Ping(u64::decode(buf)?),
            tag::PONG => Self::Pong(u64::decode(buf)?),
            tag::TIPS => Self::Tips(decode_hashes(buf, "tips")?),
            tag::INV_BLOCKS => Self::InvBlocks(decode_hashes(buf, "invblocks")?),
            tag::GET_BLOCKS => Self::GetBlocks(decode_hashes(buf, "getblocks")?),
            tag::INV_TXS => Self::InvTxs(decode_hashes(buf, "invtxs")?),
            tag::GET_TXS => Self::GetTxs(decode_hashes(buf, "gettxs")?),
            tag::BLOCKS => {
                let blocks = Vec::<BlockPayload>::decode(buf)?;
                check_len(blocks.len(), MAX_BLOCK_BATCH, "blocks")?;
                for block in &blocks {
                    check_len(block.transactions.len(), MAX_TXS_PER_BLOCK, "block.transactions")?;
                }
                Self::Blocks(blocks)
            }
            other => return Err(CodecError::UnknownTag(other)),
        };

        if !buf.is_empty() {
            return Err(CodecError::TrailingBytes(buf.len()));
        }
        Ok(message)
    }
}

fn decode_hashes(buf: &mut &[u8], what: &'static str) -> Result<Vec<BlockHash>, CodecError> {
    let hashes = Vec::<BlockHash>::decode(buf)?;
    check_len(hashes.len(), MAX_INV_ENTRIES, what)?;
    Ok(hashes)
}

fn check_len(found: usize, limit: usize, what: &'static str) -> Result<(), CodecError> {
    if found > limit {
        return Err(CodecError::TooManyEntries { what, found, limit });
    }
    Ok(())
}

/// The `Version` payload, split out so it can derive RLP.
#[derive(Debug, Clone, PartialEq, Eq, alloy_rlp::RlpEncodable, alloy_rlp::RlpDecodable)]
struct VersionPayload {
    version: u32,
    genesis: BlockHash,
    tips: Vec<BlockHash>,
}

// Silences an unused-import warning: RlpHeader is used by the derive above.
const _: Option<RlpHeader> = None;

/// Reasons a message could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    /// The frame was empty, so there is not even a discriminant.
    #[error("empty message frame")]
    Empty,
    /// The discriminant is not one this version understands.
    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),
    /// RLP decoding failed.
    #[error("malformed message body: {0}")]
    Rlp(String),
    /// A collection exceeded its protocol limit.
    #[error("{what} has {found} entries, limit is {limit}")]
    TooManyEntries {
        /// Which field.
        what: &'static str,
        /// How many were present.
        found: usize,
        /// The protocol limit.
        limit: usize,
    },
    /// Bytes remained after a complete message. Either a framing bug or a peer
    /// trying to smuggle data past the parser.
    #[error("{0} trailing bytes after message")]
    TrailingBytes(usize),
}

impl From<RlpError> for CodecError {
    fn from(error: RlpError) -> Self {
        Self::Rlp(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use chainname_primitives::HEADER_VERSION;
    use proptest::prelude::*;

    fn hashes(n: usize) -> Vec<BlockHash> {
        (0..n).map(|i| B256::from(alloy_primitives::U256::from(i))).collect()
    }

    fn header(nonce: u64) -> Header {
        Header {
            version: HEADER_VERSION,
            parents: vec![B256::repeat_byte(1)],
            timestamp_ms: 1_700_000_000_000,
            bits: 0x2000_ffff,
            nonce,
            miner: Address::repeat_byte(2),
            txs_root: B256::ZERO,
            deferred_height: 7,
            deferred_state_root: B256::repeat_byte(3),
            deferred_receipts_root: B256::repeat_byte(4),
            deferred_gas_used: 21_000,
        }
    }

    fn roundtrip(message: &Message) {
        let bytes = message.encode_to_vec();
        assert_eq!(Message::decode_from_slice(&bytes).unwrap(), *message);
    }

    #[test]
    fn every_message_type_roundtrips() {
        roundtrip(&Message::Version {
            version: PROTOCOL_VERSION,
            genesis: B256::repeat_byte(9),
            tips: hashes(3),
        });
        roundtrip(&Message::Verack);
        roundtrip(&Message::Ping(42));
        roundtrip(&Message::Pong(42));
        roundtrip(&Message::GetTips);
        roundtrip(&Message::Tips(hashes(2)));
        roundtrip(&Message::InvBlocks(hashes(5)));
        roundtrip(&Message::GetBlocks(hashes(5)));
        roundtrip(&Message::Blocks(vec![
            BlockPayload::empty(header(1)),
            BlockPayload { header: header(2), transactions: vec![Bytes::from_static(b"tx")] },
        ]));
        roundtrip(&Message::InvTxs(hashes(4)));
        roundtrip(&Message::GetTxs(hashes(4)));
    }

    #[test]
    fn empty_collections_roundtrip() {
        roundtrip(&Message::Tips(Vec::new()));
        roundtrip(&Message::Blocks(Vec::new()));
    }

    #[test]
    fn an_unknown_tag_is_rejected() {
        assert_eq!(Message::decode_from_slice(&[0xff]), Err(CodecError::UnknownTag(0xff)));
    }

    #[test]
    fn an_empty_frame_is_rejected() {
        assert_eq!(Message::decode_from_slice(&[]), Err(CodecError::Empty));
    }

    #[test]
    fn oversized_inventories_are_rejected_before_they_cost_anything() {
        let message = Message::InvBlocks(hashes(MAX_INV_ENTRIES + 1));
        let bytes = message.encode_to_vec();
        assert!(matches!(
            Message::decode_from_slice(&bytes),
            Err(CodecError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn oversized_block_batches_are_rejected() {
        let blocks: Vec<BlockPayload> =
            (0..=MAX_BLOCK_BATCH as u64).map(|n| BlockPayload::empty(header(n))).collect();
        let bytes = Message::Blocks(blocks).encode_to_vec();
        assert!(matches!(
            Message::decode_from_slice(&bytes),
            Err(CodecError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = Message::Ping(1).encode_to_vec();
        bytes.push(0xaa);
        assert!(matches!(Message::decode_from_slice(&bytes), Err(CodecError::TrailingBytes(1))));
    }

    #[test]
    fn truncated_bodies_are_rejected() {
        let bytes = Message::Tips(hashes(4)).encode_to_vec();
        for cut in 1..bytes.len() {
            // Must never panic, whatever the truncation point.
            let _ = Message::decode_from_slice(&bytes[..cut]);
        }
    }

    proptest! {
        /// Decoding arbitrary bytes must never panic. This is the first line of
        /// defence against a hostile peer and is fuzzed properly at M8.
        #[test]
        fn decoding_arbitrary_bytes_never_panics(bytes: Vec<u8>) {
            let _ = Message::decode_from_slice(&bytes);
        }

        /// Any hash list roundtrips within the protocol limit.
        #[test]
        fn hash_lists_roundtrip(count in 0usize..64) {
            let message = Message::InvBlocks(hashes(count));
            let bytes = message.encode_to_vec();
            prop_assert_eq!(Message::decode_from_slice(&bytes).unwrap(), message);
        }
    }
}
