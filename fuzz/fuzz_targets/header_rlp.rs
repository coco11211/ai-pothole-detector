//! Fuzzes block header RLP decoding.
//!
//! Headers arrive from peers before anything about them is trusted, so the
//! decoder is inside the attack surface.

#![no_main]

use alloy_rlp::{Decodable, Encodable};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut slice = data;
    if let Ok(header) = chainname_primitives::Header::decode(&mut slice) {
        // Hashing must not panic on any decodable header.
        let _ = header.hash();
        // Structural validation must be total.
        let _ = header.validate_structure();

        // Round trip: encoding and decoding must be the identity. If it is
        // not, two nodes can disagree on a block's hash while holding the
        // same bytes.
        let mut buffer = Vec::new();
        header.encode(&mut buffer);
        let mut round = buffer.as_slice();
        let decoded = chainname_primitives::Header::decode(&mut round)
            .expect("a header we just encoded must decode");
        assert_eq!(header, decoded, "header did not survive a round trip");
        assert_eq!(header.hash(), decoded.hash(), "hash changed across a round trip");
    }
});
