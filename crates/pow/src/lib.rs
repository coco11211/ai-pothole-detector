//! CHAINNAME proof of work.
//!
//! The hash function is pluggable behind [`PowHash`] so it can be replaced
//! without touching consensus logic.

use alloy_primitives::B256;
use chainname_primitives::Header;
use tiny_keccak::{Hasher, Keccak};

/// A proof-of-work hash function.
///
/// Implementors take the RLP-encoded header and return a 256-bit digest that
/// is compared against the difficulty target.
pub trait PowHash: Send + Sync + std::fmt::Debug {
    /// Human-readable name, recorded in logs and in the genesis config so a
    /// chain cannot silently change its PoW function.
    fn name(&self) -> &'static str;

    /// Hashes a PoW preimage.
    fn hash(&self, preimage: &[u8]) -> B256;

    /// Hashes a header.
    fn hash_header(&self, header: &Header) -> B256 {
        self.hash(&header.encoded())
    }
}

/// ============================================================================
/// TESTNET PLACEHOLDER — REQUIRES INDEPENDENT CRYPTANALYTIC REVIEW
/// ============================================================================
///
/// **This function must not secure a network carrying value until it has been
/// independently reviewed by cryptanalysts.** It was chosen for simplicity and
/// for having a well-studied permutation underneath, not because its security
/// margin, ASIC-resistance, or resistance to shortcut attacks has been
/// analysed. Nothing here has been analysed. See OPEN-PROBLEMS.md P-006.
///
/// # What this computes
///
/// `keccak256(keccak256(preimage))` — two sequential applications of the full
/// 24-round Keccak-f[1600] sponge, truncated to 256 bits. Structurally this is
/// Bitcoin's double-SHA-256 with Keccak substituted.
///
/// # Why this reading of "two-round Keccak-f[1600]"
///
/// The specification said "two-round Keccak-f\[1600\], truncated to 256 bits",
/// which admits two readings: two applications of the *hash*, or a
/// **reduced-round** permutation running only 2 of Keccak's 24 rounds. The
/// second reading produces a function that is trivially invertible — 2-round
/// Keccak-f has been broken in practice, and a miner could compute preimages
/// directly instead of searching nonces, which does not merely weaken the
/// proof of work, it removes it. The first reading is implemented here.
/// DECISIONS.md D-016 records this as a deliberate interpretation.
#[derive(Debug, Clone, Copy, Default)]
pub struct DoubleKeccak256;

impl DoubleKeccak256 {
    /// Name reported by [`PowHash::name`].
    pub const NAME: &'static str = "double-keccak256";
}

impl PowHash for DoubleKeccak256 {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn hash(&self, preimage: &[u8]) -> B256 {
        B256::from(keccak(&keccak(preimage)))
    }
}

fn keccak(input: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut out = [0u8; 32];
    hasher.update(input);
    hasher.finalize(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, b256};
    use chainname_primitives::HEADER_VERSION;
    use proptest::prelude::*;

    fn header(nonce: u64) -> Header {
        Header {
            version: HEADER_VERSION,
            parents: vec![b256!(
                "0000000000000000000000000000000000000000000000000000000000000001"
            )],
            timestamp_ms: 1_700_000_000_000,
            bits: 0x1d00_ffff,
            nonce,
            miner: Address::ZERO,
            txs_root: B256::ZERO,
            deferred_height: 0,
            deferred_state_root: B256::ZERO,
            deferred_receipts_root: B256::ZERO,
            deferred_gas_used: 0,
        }
    }

    #[test]
    fn matches_double_keccak_of_the_empty_string() {
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        // The outer hash is over those 32 bytes. Pinned so an accidental change
        // to the construction is caught rather than silently forking the chain.
        let inner = keccak(b"");
        assert_eq!(
            hex::encode(inner),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
        assert_eq!(DoubleKeccak256.hash(b""), B256::from(keccak(&inner)));
    }

    #[test]
    fn is_not_a_single_keccak() {
        // Guards against someone "simplifying" the implementation into one round.
        assert_ne!(DoubleKeccak256.hash(b"abc"), B256::from(keccak(b"abc")));
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(DoubleKeccak256.hash(b"abc"), DoubleKeccak256.hash(b"abc"));
    }

    #[test]
    fn nonce_changes_the_pow_hash() {
        assert_ne!(
            DoubleKeccak256.hash_header(&header(0)),
            DoubleKeccak256.hash_header(&header(1))
        );
    }

    #[test]
    fn name_is_stable() {
        assert_eq!(DoubleKeccak256.name(), "double-keccak256");
    }

    proptest! {
        /// Distinct preimages essentially never collide. A failure here means
        /// the construction is degenerate, not that we found a real collision.
        #[test]
        fn distinct_inputs_give_distinct_digests(a: Vec<u8>, b: Vec<u8>) {
            prop_assume!(a != b);
            prop_assert_ne!(DoubleKeccak256.hash(&a), DoubleKeccak256.hash(&b));
        }

        /// Hashing is a pure function of its input.
        #[test]
        fn hashing_is_pure(input: Vec<u8>) {
            prop_assert_eq!(DoubleKeccak256.hash(&input), DoubleKeccak256.hash(&input));
        }
    }
}
