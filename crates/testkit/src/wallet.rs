//! Deterministic test wallets.
//!
//! Real secp256k1 keys and real signatures, so the simulation exercises the
//! whole path a wallet does: sign, encode as EIP-2718, gossip, decode, recover,
//! execute. A shortcut here would hide exactly the bugs the harness exists to
//! find.

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Bytes, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;

/// A funded test account.
#[derive(Debug, Clone)]
pub struct Wallet {
    signer: PrivateKeySigner,
    address: Address,
}

impl Wallet {
    /// Derives a wallet from an index.
    ///
    /// Keys are a deterministic function of the index, so every node in the
    /// simulation and every rerun agree on who is who.
    pub fn from_index(index: u64) -> Self {
        // A non-zero, well-distributed key from the index. Zero and values at
        // or above the curve order are invalid secp256k1 keys, so the index is
        // hashed rather than used directly.
        let key = alloy_primitives::keccak256(index.to_be_bytes());
        let signer = PrivateKeySigner::from_bytes(&key).expect("hashed index is a valid key");
        let address = signer.address();
        Self { signer, address }
    }

    /// This wallet's address.
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Signs an EIP-1559 transfer and returns it EIP-2718 encoded, ready for
    /// the wire.
    pub fn signed_transfer(
        &self,
        chain_id: u64,
        nonce: u64,
        to: Address,
        value: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Bytes {
        let tx = TxEip1559 {
            chain_id,
            nonce,
            // A plain transfer costs 21,000. The extra headroom means a
            // transfer to a contract account would still fit.
            gas_limit: 100_000,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to: TxKind::Call(to),
            value: U256::from(value),
            access_list: Default::default(),
            input: Default::default(),
        };
        let signature = self
            .signer
            .sign_hash_sync(&tx.signature_hash())
            .expect("signing a well-formed transaction cannot fail");
        let envelope = TxEnvelope::Eip1559(tx.into_signed(signature));
        Bytes::from(envelope.encoded_2718())
    }

    /// The signing key, for tests that need to re-derive it.
    pub fn key(index: u64) -> B256 {
        alloy_primitives::keccak256(index.to_be_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::transaction::SignerRecoverable;
    use alloy_eips::eip2718::Decodable2718;

    #[test]
    fn wallets_are_deterministic() {
        assert_eq!(Wallet::from_index(7).address(), Wallet::from_index(7).address());
    }

    #[test]
    fn different_indices_are_different_wallets() {
        assert_ne!(Wallet::from_index(1).address(), Wallet::from_index(2).address());
    }

    #[test]
    fn a_signed_transfer_recovers_to_its_sender() {
        // The whole point: sign, encode, decode, recover, and get the same
        // address back. If this breaks, every transaction in the simulation is
        // attributed to the wrong account.
        let wallet = Wallet::from_index(3);
        let encoded =
            wallet.signed_transfer(7717, 0, Address::repeat_byte(9), 100, 1_000_000_000, 0);

        let envelope = TxEnvelope::decode_2718(&mut encoded.as_ref()).expect("decodes");
        assert_eq!(envelope.recover_signer().expect("recovers"), wallet.address());
    }

    #[test]
    fn distinct_nonces_give_distinct_transaction_hashes() {
        let wallet = Wallet::from_index(4);
        let to = Address::repeat_byte(9);
        let a = wallet.signed_transfer(7717, 0, to, 1, 1_000_000_000, 0);
        let b = wallet.signed_transfer(7717, 1, to, 1, 1_000_000_000, 0);
        assert_ne!(a, b);
    }
}
