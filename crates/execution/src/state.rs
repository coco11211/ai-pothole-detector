//! The world state: accounts, storage, code.
//!
//! Implements revm's [`Database`] and [`DatabaseCommit`] directly, so an EVM
//! can be built straight over it and `transact_commit` writes through.
//!
//! State root computation here walks the entire state
//! (`alloy_trie::root::state_root_unhashed`). That is correct and fine at test
//! scale but is O(state) per chain block and will not survive a state of any
//! real size. Replacing it with an incremental trie is OPEN-PROBLEMS.md P-004:
//! scheduled debt, not an oversight.

use std::{collections::BTreeMap, convert::Infallible};

use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256, map::AddressMap};
use alloy_trie::{EMPTY_ROOT_HASH, TrieAccount};
use revm::{
    bytecode::Bytecode,
    database_interface::{Database, DatabaseCommit, DatabaseRef},
    primitives::{StorageKey, StorageValue},
    state::{Account as RevmAccount, AccountInfo},
};

/// One account's persisted form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountState {
    /// Nonce, balance, code hash.
    pub info: AccountInfo,
    /// Storage slots. `BTreeMap` so iteration order is deterministic, which
    /// matters because state roots must not depend on insertion order.
    pub storage: BTreeMap<StorageKey, StorageValue>,
}

/// The full world state.
#[derive(Debug, Clone, Default)]
pub struct WorldState {
    accounts: BTreeMap<Address, AccountState>,
    /// Contract code, keyed by code hash. Shared across accounts, as in
    /// Ethereum: two accounts with identical code share one entry.
    code: BTreeMap<B256, Bytecode>,
    /// Recent block hashes, for the `BLOCKHASH` opcode.
    block_hashes: BTreeMap<u64, B256>,
}

impl WorldState {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of accounts.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// True if no accounts exist.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Reads an account, if it exists.
    pub fn account(&self, address: Address) -> Option<&AccountState> {
        self.accounts.get(&address)
    }

    /// Reads a balance. Absent accounts have zero balance, as in Ethereum.
    pub fn balance(&self, address: Address) -> U256 {
        self.accounts.get(&address).map_or(U256::ZERO, |a| a.info.balance)
    }

    /// Reads a nonce. Absent accounts have nonce zero.
    pub fn nonce(&self, address: Address) -> u64 {
        self.accounts.get(&address).map_or(0, |a| a.info.nonce)
    }

    /// Reads a storage slot. Unset slots read as zero.
    pub fn storage_slot(&self, address: Address, slot: StorageKey) -> StorageValue {
        self.accounts
            .get(&address)
            .and_then(|a| a.storage.get(&slot))
            .copied()
            .unwrap_or(StorageValue::ZERO)
    }

    /// Reads an account's code, if any.
    pub fn code(&self, address: Address) -> Option<&Bytecode> {
        let hash = self.accounts.get(&address)?.info.code_hash;
        if hash == KECCAK256_EMPTY { None } else { self.code.get(&hash) }
    }

    /// Inserts or replaces an account wholesale. Used by genesis loading and
    /// by tests; normal execution goes through [`DatabaseCommit::commit`].
    pub fn insert_account(&mut self, address: Address, account: AccountState) {
        self.accounts.insert(address, account);
    }

    /// Registers contract code so it can be resolved by hash.
    pub fn insert_code(&mut self, code: Bytecode) -> B256 {
        let hash = code.hash_slow();
        self.code.insert(hash, code);
        hash
    }

    /// Records a block hash for the `BLOCKHASH` opcode.
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }

    /// Iterates accounts in address order.
    pub fn iter_accounts(&self) -> impl Iterator<Item = (&Address, &AccountState)> {
        self.accounts.iter()
    }

    /// Computes the state root over the whole state.
    ///
    /// O(state). See the module docs and OPEN-PROBLEMS.md P-004.
    pub fn state_root(&self) -> B256 {
        let entries = self.accounts.iter().map(|(address, account)| {
            let storage_root = if account.storage.is_empty() {
                EMPTY_ROOT_HASH
            } else {
                alloy_trie::root::storage_root_unhashed(
                    account
                        .storage
                        .iter()
                        // Zero slots are not part of the trie: an unset slot and
                        // a slot explicitly set to zero must hash identically.
                        .filter(|(_, value)| !value.is_zero())
                        .map(|(slot, value)| (B256::from(*slot), *value)),
                )
            };
            let trie_account = TrieAccount {
                nonce: account.info.nonce,
                balance: account.info.balance,
                storage_root,
                code_hash: account.info.code_hash,
            };
            (*address, trie_account)
        });
        alloy_trie::root::state_root_unhashed(entries)
    }
}

impl DatabaseRef for WorldState {
    type Error = Infallible;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(&address).map(|a| a.info.clone()))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        Ok(self.code.get(&code_hash).cloned().unwrap_or_default())
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        Ok(self.storage_slot(address, index))
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self.block_hashes.get(&number).copied().unwrap_or_default())
    }
}

impl Database for WorldState {
    type Error = Infallible;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.basic_ref(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.code_by_hash_ref(code_hash)
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.storage_ref(address, index)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hash_ref(number)
    }
}

impl DatabaseCommit for WorldState {
    fn commit(&mut self, changes: AddressMap<RevmAccount>) {
        for (address, account) in changes {
            if !account.is_touched() {
                continue;
            }

            if account.is_selfdestructed() {
                self.accounts.remove(&address);
                continue;
            }

            // An account that is empty after execution is removed outright
            // (EIP-161). Leaving it behind would change the state root.
            if account.is_empty() && account.is_loaded_as_not_existing() {
                self.accounts.remove(&address);
                continue;
            }

            if let Some(code) = &account.info.code
                && !code.is_empty()
            {
                self.code.insert(account.info.code_hash, code.clone());
            }

            let entry = self.accounts.entry(address).or_default();
            entry.info = account.info.clone();
            // A freshly created account starts with empty storage, even if an
            // account previously existed at this address (selfdestruct then
            // recreate). Not clearing here would resurrect dead slots.
            if account.is_created() {
                entry.storage.clear();
            }
            for (slot, value) in &account.storage {
                if value.present_value().is_zero() {
                    entry.storage.remove(slot);
                } else {
                    entry.storage.insert(*slot, value.present_value());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_root_is_the_empty_trie_root() {
        assert_eq!(WorldState::new().state_root(), EMPTY_ROOT_HASH);
    }

    #[test]
    fn state_root_is_insertion_order_independent() {
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let mk = |info: AccountInfo| AccountState { info, storage: BTreeMap::new() };

        let mut one = WorldState::new();
        one.insert_account(a, mk(AccountInfo { balance: U256::from(10), ..Default::default() }));
        one.insert_account(b, mk(AccountInfo { balance: U256::from(20), ..Default::default() }));

        let mut two = WorldState::new();
        two.insert_account(b, mk(AccountInfo { balance: U256::from(20), ..Default::default() }));
        two.insert_account(a, mk(AccountInfo { balance: U256::from(10), ..Default::default() }));

        assert_eq!(one.state_root(), two.state_root());
    }

    #[test]
    fn zero_storage_slot_does_not_change_the_root() {
        let addr = Address::repeat_byte(3);
        let info = AccountInfo { balance: U256::from(1), ..Default::default() };

        let mut without = WorldState::new();
        without.insert_account(addr, AccountState { info: info.clone(), storage: BTreeMap::new() });

        let mut with_zero = WorldState::new();
        let mut storage = BTreeMap::new();
        storage.insert(StorageKey::from(7u64), StorageValue::ZERO);
        with_zero.insert_account(addr, AccountState { info, storage });

        assert_eq!(without.state_root(), with_zero.state_root());
    }

    #[test]
    fn balance_change_changes_the_root() {
        let addr = Address::repeat_byte(4);
        let mut one = WorldState::new();
        one.insert_account(
            addr,
            AccountState {
                info: AccountInfo { balance: U256::from(1), ..Default::default() },
                storage: BTreeMap::new(),
            },
        );
        let mut two = WorldState::new();
        two.insert_account(
            addr,
            AccountState {
                info: AccountInfo { balance: U256::from(2), ..Default::default() },
                storage: BTreeMap::new(),
            },
        );
        assert_ne!(one.state_root(), two.state_root());
    }

    #[test]
    fn missing_account_reads_as_zero() {
        let state = WorldState::new();
        let addr = Address::repeat_byte(5);
        assert_eq!(state.balance(addr), U256::ZERO);
        assert_eq!(state.nonce(addr), 0);
        assert_eq!(state.storage_slot(addr, StorageKey::from(1u64)), StorageValue::ZERO);
    }
}
