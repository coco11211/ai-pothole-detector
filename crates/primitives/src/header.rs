//! Block header and body.
//!
//! A CHAINNAME block is a transaction batch with **no state commitment for its
//! own transactions**. See ARCHITECTURE.md §4 and §5: a DAG block cannot commit
//! to the state after its own execution, because its effect depends on which
//! merge set eventually contains it, which is unknown at mining time.

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, B256, keccak256};
use alloy_rlp::{Encodable, RlpDecodable, RlpEncodable};

/// Identity of a block: `keccak256(rlp(header))`.
///
/// Deliberately *not* the proof-of-work hash. Keeping identity independent of
/// the PoW function means the PoW function stays swappable (it is a testnet
/// placeholder — see OPEN-PROBLEMS.md P-006) without changing how blocks are
/// named, stored, or referenced by their children.
pub type BlockHash = B256;

/// Current header version. Bumped only by a consensus change.
pub const HEADER_VERSION: u16 = 1;

/// A block header.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Header {
    /// Header format version.
    pub version: u16,
    /// Parent block hashes. Non-empty, strictly ascending, no duplicates.
    ///
    /// No parent is privileged. The *selected* parent is derived by GHOSTDAG
    /// from blue work, never declared, so a miner cannot lie about it.
    /// Requiring strict ascending order removes header malleability: a given
    /// parent set has exactly one valid encoding.
    pub parents: Vec<BlockHash>,
    /// Block timestamp, milliseconds since the Unix epoch.
    ///
    /// Milliseconds rather than seconds because at 10 blocks/second a
    /// second-resolution timestamp cannot order blocks or drive ASERT.
    pub timestamp_ms: u64,
    /// Compact difficulty target for this block.
    pub bits: u32,
    /// Proof-of-work nonce.
    pub nonce: u64,
    /// Address credited with this block's subsidy and with the priority fees
    /// of the transactions this block contributed to a merge set.
    pub miner: Address,
    /// Merkle root over this block's own transaction list.
    pub txs_root: B256,
    /// Selected-chain height whose execution results the three `deferred_*`
    /// fields below describe. Zero in the genesis block and in any block whose
    /// chain height is below the lag `D`.
    pub deferred_height: u64,
    /// State root after executing selected-chain block [`Self::deferred_height`].
    pub deferred_state_root: B256,
    /// Receipts root of selected-chain block [`Self::deferred_height`].
    pub deferred_receipts_root: B256,
    /// Gas used by selected-chain block [`Self::deferred_height`].
    pub deferred_gas_used: u64,
}

impl Header {
    /// Computes this header's identity hash.
    pub fn hash(&self) -> BlockHash {
        let mut buf = Vec::with_capacity(self.length());
        self.encode(&mut buf);
        keccak256(&buf)
    }

    /// RLP encoding of the header, which is also the proof-of-work preimage.
    pub fn encoded(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.length());
        self.encode(&mut buf);
        buf
    }

    /// The selected parent *candidate* set. GHOSTDAG picks one of these; this
    /// is just the declared set in canonical order.
    pub fn parents(&self) -> &[BlockHash] {
        &self.parents
    }

    /// Structural validation that needs no DAG context and no state.
    ///
    /// Everything checkable from the header alone, so a peer's garbage is
    /// rejected before it costs a DAG lookup.
    pub fn validate_structure(&self) -> Result<(), HeaderError> {
        if self.version != HEADER_VERSION {
            return Err(HeaderError::UnknownVersion(self.version));
        }
        if self.parents.is_empty() {
            return Err(HeaderError::NoParents);
        }
        for pair in self.parents.windows(2) {
            if pair[0] >= pair[1] {
                return Err(HeaderError::ParentsNotStrictlyAscending);
            }
        }
        Ok(())
    }
}

/// A block: a header plus the transactions it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The header.
    pub header: Header,
    /// Transactions, in the order this block's miner chose.
    ///
    /// This order is *not* the execution order. Execution order is derived
    /// from the merge set that eventually contains this block; see
    /// ARCHITECTURE.md §6.
    pub transactions: Vec<TxEnvelope>,
}

impl Block {
    /// This block's identity.
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }
}

/// Reasons a header is structurally invalid.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderError {
    /// Version is not [`HEADER_VERSION`].
    #[error("unknown header version {0}")]
    UnknownVersion(u16),
    /// Parent list is empty. Only genesis has no parents, and genesis is never
    /// validated through this path.
    #[error("header has no parents")]
    NoParents,
    /// Parents are not strictly ascending, so the encoding is malleable.
    #[error("parents must be strictly ascending with no duplicates")]
    ParentsNotStrictlyAscending,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;

    fn header_with_parents(parents: Vec<BlockHash>) -> Header {
        Header {
            version: HEADER_VERSION,
            parents,
            timestamp_ms: 1_700_000_000_000,
            bits: 0x1d00_ffff,
            nonce: 0,
            miner: Address::ZERO,
            txs_root: B256::ZERO,
            deferred_height: 0,
            deferred_state_root: B256::ZERO,
            deferred_receipts_root: B256::ZERO,
            deferred_gas_used: 0,
        }
    }

    const A: B256 = b256!("0000000000000000000000000000000000000000000000000000000000000001");
    const B: B256 = b256!("0000000000000000000000000000000000000000000000000000000000000002");

    #[test]
    fn accepts_ascending_parents() {
        header_with_parents(vec![A, B]).validate_structure().unwrap();
    }

    #[test]
    fn rejects_descending_parents() {
        assert_eq!(
            header_with_parents(vec![B, A]).validate_structure(),
            Err(HeaderError::ParentsNotStrictlyAscending)
        );
    }

    #[test]
    fn rejects_duplicate_parents() {
        assert_eq!(
            header_with_parents(vec![A, A]).validate_structure(),
            Err(HeaderError::ParentsNotStrictlyAscending)
        );
    }

    #[test]
    fn rejects_empty_parents() {
        assert_eq!(header_with_parents(vec![]).validate_structure(), Err(HeaderError::NoParents));
    }

    #[test]
    fn rejects_unknown_version() {
        let mut h = header_with_parents(vec![A]);
        h.version = 99;
        assert_eq!(h.validate_structure(), Err(HeaderError::UnknownVersion(99)));
    }

    #[test]
    fn nonce_changes_identity() {
        let h1 = header_with_parents(vec![A]);
        let mut h2 = h1.clone();
        h2.nonce = 1;
        assert_ne!(h1.hash(), h2.hash());
    }

    #[test]
    fn rlp_roundtrips() {
        use alloy_rlp::Decodable;
        let h = header_with_parents(vec![A, B]);
        let encoded = h.encoded();
        let decoded = Header::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(h, decoded);
    }
}
