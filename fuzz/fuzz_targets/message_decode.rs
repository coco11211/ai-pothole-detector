//! Fuzzes the wire-message decoder.
//!
//! This is the first thing a hostile peer touches. It must never panic, never
//! allocate unboundedly, and never loop, whatever bytes arrive.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Decoding must be total: any byte string is either a message or an error.
    if let Ok(message) = chainname_net::Message::decode_from_slice(data) {
        // A decoded message must re-encode and decode back to itself.
        // Anything else means the same bytes could mean two things to two
        // nodes, which is a consensus split waiting to happen.
        let re_encoded = message.encode_to_vec();
        let decoded = chainname_net::Message::decode_from_slice(&re_encoded)
            .expect("a message we just encoded must decode");
        assert_eq!(message, decoded, "message did not survive a re-encode");
    }
});
