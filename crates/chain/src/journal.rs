//! The undo journal.
//!
//! Reorgs roll state back by replaying undo records in reverse, not by
//! re-executing from genesis. At one block per second, re-execution would make
//! even a shallow reorg unaffordable.
//!
//! Each record holds the **pre-value** of everything a chain block touched.
//! Restoring it is idempotent and needs no knowledge of what the block did.

use std::collections::BTreeMap;

use alloy_primitives::Address;
use chainname_execution::{AccountState, WorldState};

/// Everything needed to undo one chain block's execution.
#[derive(Debug, Clone, Default)]
pub struct UndoRecord {
    /// Pre-execution value of each touched account. `None` means the account
    /// did not exist, so undoing removes it.
    ///
    /// `BTreeMap` so a record is deterministic and comparable, which matters
    /// when diagnosing a divergence between two nodes.
    prior: BTreeMap<Address, Option<AccountState>>,
}

impl UndoRecord {
    /// An empty record.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an account's value before it is modified.
    ///
    /// Only the *first* call for an address has any effect: the pre-execution
    /// value is the one before the block ran, not before the most recent
    /// transaction in it.
    pub fn note(&mut self, address: Address, state: &WorldState) {
        self.prior.entry(address).or_insert_with(|| state.account(address).cloned());
    }

    /// How many accounts this record covers.
    pub fn len(&self) -> usize {
        self.prior.len()
    }

    /// True if the block touched nothing.
    pub fn is_empty(&self) -> bool {
        self.prior.is_empty()
    }

    /// Restores the recorded pre-values.
    pub fn apply(&self, state: &mut WorldState) {
        for (address, prior) in &self.prior {
            match prior {
                Some(account) => state.insert_account(*address, account.clone()),
                None => state.remove_account(*address),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use revm::state::AccountInfo;

    fn account(balance: u64) -> AccountState {
        AccountState {
            info: AccountInfo { balance: U256::from(balance), ..Default::default() },
            storage: Default::default(),
        }
    }

    #[test]
    fn undoing_restores_a_modified_account() {
        let addr = Address::repeat_byte(1);
        let mut state = WorldState::new();
        state.insert_account(addr, account(100));

        let mut record = UndoRecord::new();
        record.note(addr, &state);

        state.insert_account(addr, account(999));
        record.apply(&mut state);

        assert_eq!(state.balance(addr), U256::from(100u64));
    }

    #[test]
    fn undoing_removes_an_account_that_did_not_exist() {
        let addr = Address::repeat_byte(2);
        let mut state = WorldState::new();

        let mut record = UndoRecord::new();
        record.note(addr, &state);

        state.insert_account(addr, account(50));
        record.apply(&mut state);

        assert!(state.account(addr).is_none(), "an account created by the block must be removed");
    }

    #[test]
    fn only_the_first_note_counts() {
        // The pre-value is the one before the *block* ran, not before the most
        // recent transaction in it.
        let addr = Address::repeat_byte(3);
        let mut state = WorldState::new();
        state.insert_account(addr, account(1));

        let mut record = UndoRecord::new();
        record.note(addr, &state);
        state.insert_account(addr, account(2));
        record.note(addr, &state);
        state.insert_account(addr, account(3));

        record.apply(&mut state);
        assert_eq!(state.balance(addr), U256::from(1u64));
    }

    #[test]
    fn applying_twice_is_idempotent() {
        let addr = Address::repeat_byte(4);
        let mut state = WorldState::new();
        state.insert_account(addr, account(7));

        let mut record = UndoRecord::new();
        record.note(addr, &state);
        state.insert_account(addr, account(8));

        record.apply(&mut state);
        let root_once = state.state_root();
        record.apply(&mut state);
        assert_eq!(state.state_root(), root_once);
    }

    #[test]
    fn an_empty_record_changes_nothing() {
        let mut state = WorldState::new();
        state.insert_account(Address::repeat_byte(5), account(1));
        let before = state.state_root();
        UndoRecord::new().apply(&mut state);
        assert_eq!(state.state_root(), before);
    }
}
