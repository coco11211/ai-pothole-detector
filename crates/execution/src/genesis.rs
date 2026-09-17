//! Genesis state loading.
//!
//! Accepts `alloy_genesis::Genesis`, the standard `genesis.json` shape, so
//! existing Ethereum tooling can produce a CHAINNAME genesis unmodified.

use alloy_genesis::Genesis;
use alloy_primitives::{KECCAK256_EMPTY, U256};
use revm::{bytecode::Bytecode, primitives::StorageKey, state::AccountInfo};

use crate::state::{AccountState, WorldState};

/// Builds a [`WorldState`] from a genesis allocation.
pub fn load_genesis(genesis: &Genesis) -> Result<WorldState, GenesisError> {
    let mut state = WorldState::new();

    for (address, account) in &genesis.alloc {
        let (code_hash, code) = match &account.code {
            Some(bytes) if !bytes.is_empty() => {
                let bytecode = Bytecode::new_raw(bytes.clone());
                (bytecode.hash_slow(), Some(bytecode))
            }
            _ => (KECCAK256_EMPTY, None),
        };

        if let Some(code) = code {
            state.insert_code(code);
        }

        let mut storage = std::collections::BTreeMap::new();
        if let Some(slots) = &account.storage {
            for (slot, value) in slots {
                let value = U256::from_be_bytes(value.0);
                // Zero slots are not stored: an unset slot and a slot set to
                // zero must be indistinguishable in the state root.
                if !value.is_zero() {
                    storage.insert(StorageKey::from_be_bytes(slot.0), value);
                }
            }
        }

        state.insert_account(
            *address,
            AccountState {
                info: AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce.unwrap_or_default(),
                    code_hash,
                    code: None,
                    ..Default::default()
                },
                storage,
            },
        );
    }

    Ok(state)
}

/// Reasons genesis could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    /// Genesis JSON was malformed.
    #[error("invalid genesis: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_genesis::GenesisAccount;
    use alloy_primitives::Address;

    #[test]
    fn loads_balances() {
        let addr = Address::repeat_byte(1);
        let mut genesis = Genesis::default();
        genesis
            .alloc
            .insert(addr, GenesisAccount { balance: U256::from(1_000u64), ..Default::default() });

        let state = load_genesis(&genesis).unwrap();
        assert_eq!(state.balance(addr), U256::from(1_000u64));
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn empty_genesis_gives_empty_state() {
        assert!(load_genesis(&Genesis::default()).unwrap().is_empty());
    }

    #[test]
    fn genesis_state_root_is_deterministic() {
        let mut genesis = Genesis::default();
        for i in 1..=5u8 {
            genesis.alloc.insert(
                Address::repeat_byte(i),
                GenesisAccount { balance: U256::from(i), ..Default::default() },
            );
        }
        let a = load_genesis(&genesis).unwrap().state_root();
        let b = load_genesis(&genesis).unwrap().state_root();
        assert_eq!(a, b);
    }
}
