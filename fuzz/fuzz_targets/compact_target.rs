//! Fuzzes the compact difficulty encoding.
//!
//! A bug here is a consensus bug: two nodes disagreeing on what a header's
//! `bits` means disagree on whether its proof of work is valid. An earlier
//! version of this code returned `Ok(0)` for some encodings, which would have
//! made every hash fail against a target that should never have decoded.

#![no_main]

use chainname_difficulty::CompactTarget;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|raw: u32| {
    let compact = CompactTarget(raw);
    if let Ok(target) = compact.to_target() {
        // A decodable target is never zero: a zero target can be met by no
        // hash at all, so it must be an error rather than a value.
        assert!(!target.is_zero(), "{raw:#010x} decoded to a zero target");

        // Re-encoding a decoded target must be a fixed point.
        let re_encoded = CompactTarget::from_target(target);
        let round = re_encoded.to_target().expect("a re-encoded target must decode");
        assert_eq!(round, target, "{raw:#010x} did not survive a round trip");
    }
});
