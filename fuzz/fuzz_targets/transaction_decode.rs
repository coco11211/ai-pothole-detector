//! Fuzzes transaction decoding.
//!
//! Transactions arrive inside block bodies and from `eth_sendRawTransaction`,
//! both of which are reachable by anyone.

#![no_main]

use alloy_eips::eip2718::{Decodable2718, Encodable2718};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut slice = data;
    if let Ok(envelope) = alloy_consensus::TxEnvelope::decode_2718(&mut slice) {
        // Hashing and sender recovery must be total: both run on transactions
        // that have not been validated yet.
        let _ = envelope.hash();
        {
            use alloy_consensus::transaction::SignerRecoverable;
            let _ = envelope.recover_signer();
        }

        // A decoded transaction must re-encode to something that decodes back
        // to the same transaction, or its hash is not stable and neither is
        // deduplication in the merge set.
        let re_encoded = envelope.encoded_2718();
        let mut round = re_encoded.as_slice();
        let decoded = alloy_consensus::TxEnvelope::decode_2718(&mut round)
            .expect("a transaction we just encoded must decode");
        assert_eq!(envelope.hash(), decoded.hash(), "transaction hash is not stable");
    }
});
