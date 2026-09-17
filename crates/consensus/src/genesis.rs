//! The genesis block.
//!
//! Genesis is also the **ASERT anchor**: every later block's difficulty is
//! computed from it directly, never from its parent. That makes each block's
//! difficulty independently verifiable and stops retarget error accumulating.

use alloy_primitives::{Address, B256};
use chainname_difficulty::CompactTarget;
use chainname_primitives::{HEADER_VERSION, Header};

/// Parameters that pin a network's genesis block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisConfig {
    /// Genesis timestamp, milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Starting difficulty, and the ASERT anchor target.
    pub bits: CompactTarget,
    /// State root of the genesis allocation.
    ///
    /// Genesis is the one block whose `deferred_*` fields describe its own
    /// height, because there is no earlier block to describe.
    pub state_root: B256,
}

/// Builds the genesis header.
///
/// Genesis has no parents. It is the only header for which
/// [`Header::validate_structure`] would fail, and it is never validated
/// through that path — it is pinned by configuration, not derived.
pub fn genesis_header(config: &GenesisConfig) -> Header {
    Header {
        version: HEADER_VERSION,
        parents: Vec::new(),
        timestamp_ms: config.timestamp_ms,
        bits: config.bits.to_u32(),
        nonce: 0,
        miner: Address::ZERO,
        txs_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_height: 0,
        deferred_state_root: config.state_root,
        deferred_receipts_root: alloy_trie::EMPTY_ROOT_HASH,
        deferred_gas_used: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> GenesisConfig {
        GenesisConfig {
            timestamp_ms: 1_700_000_000_000,
            bits: CompactTarget(0x1e00_ffff),
            state_root: B256::repeat_byte(0x11),
        }
    }

    #[test]
    fn genesis_has_no_parents() {
        assert!(genesis_header(&config()).parents.is_empty());
    }

    #[test]
    fn genesis_is_deterministic() {
        assert_eq!(genesis_header(&config()).hash(), genesis_header(&config()).hash());
    }

    #[test]
    fn different_configs_give_different_genesis() {
        let mut other = config();
        other.timestamp_ms += 1;
        assert_ne!(genesis_header(&config()).hash(), genesis_header(&other).hash());
    }
}
