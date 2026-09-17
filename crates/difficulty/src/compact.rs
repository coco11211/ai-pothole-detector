//! Compact difficulty target encoding (`bits`).
//!
//! Bitcoin's nBits format: one exponent byte and a 24-bit mantissa, giving
//! `target = mantissa * 256^(exponent - 3)`. Reused unchanged because it is
//! well understood, fits a header field in four bytes, and its rounding
//! behaviour is already specified down to the last bit.
//!
//! Integer only. No floating point appears anywhere in this crate.

use alloy_primitives::U256;

/// Number of mantissa bytes in the compact encoding.
const MANTISSA_BYTES: u32 = 3;
/// Mantissa values at or above this have their top bit set, which the encoding
/// reserves as a sign bit. Targets are never negative, so such a mantissa is
/// renormalised by shifting down one byte and raising the exponent.
const MANTISSA_SIGN_BIT: u32 = 0x0080_0000;
/// Mantissa mask.
const MANTISSA_MASK: u32 = 0x007f_ffff;

/// A difficulty target in compact form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CompactTarget(pub u32);

impl CompactTarget {
    /// The raw `bits` value.
    pub const fn to_u32(self) -> u32 {
        self.0
    }

    /// Decodes to a full 256-bit target.
    ///
    /// Returns an error rather than a silent zero for encodings that are
    /// negative, overflowing, or zero: an invalid target must never be
    /// interpreted as "everything passes" or "nothing passes".
    pub fn to_target(self) -> Result<U256, CompactError> {
        let exponent = self.0 >> 24;
        let mantissa = self.0 & 0x00ff_ffff;

        if mantissa & MANTISSA_SIGN_BIT != 0 {
            return Err(CompactError::NegativeTarget(self.0));
        }
        if mantissa == 0 {
            return Err(CompactError::ZeroTarget(self.0));
        }

        if exponent <= MANTISSA_BYTES {
            // Mantissa is shifted *down*: small targets.
            let shift = 8 * (MANTISSA_BYTES - exponent);
            // The shift can annihilate a non-zero mantissa, e.g. 0x01000001
            // decodes 1 >> 16 == 0. A zero target is unsatisfiable by any hash,
            // so it must be an error here rather than an `Ok(0)` that callers
            // would compare against and always reject. Found by
            // `decodable_values_are_fixed_points`.
            let shifted = mantissa >> shift;
            if shifted == 0 {
                return Err(CompactError::ZeroTarget(self.0));
            }
            return Ok(U256::from(shifted));
        }

        let shift = 8 * (exponent - MANTISSA_BYTES);
        if shift >= 256 {
            return Err(CompactError::Overflow(self.0));
        }
        let target = U256::from(mantissa);
        // Check the shift cannot discard significant bits off the top.
        if target.leading_zeros() < shift as usize {
            return Err(CompactError::Overflow(self.0));
        }
        Ok(target << shift)
    }

    /// Encodes a full target into compact form, rounding *down* so the encoded
    /// target is never easier than the one requested.
    pub fn from_target(target: U256) -> Self {
        if target.is_zero() {
            return Self(0);
        }

        // Number of bytes needed to represent the target.
        let bits = 256 - target.leading_zeros();
        // `bits` is at most 256, so the byte count is at most 32. The cast is
        // exact; it is only a cast because `leading_zeros` returns `usize`.
        let mut exponent = u32::try_from(bits.div_ceil(8)).expect("byte count fits in u32");

        let mut mantissa: u32 = if exponent <= MANTISSA_BYTES {
            let shift = 8 * (MANTISSA_BYTES - exponent);
            // Fits in 24 bits by construction.
            (target << shift).to::<u32>() & 0x00ff_ffff
        } else {
            let shift = 8 * (exponent - MANTISSA_BYTES);
            (target >> shift).to::<u32>() & 0x00ff_ffff
        };

        // The top mantissa bit is the encoding's sign bit and must stay clear.
        if mantissa & MANTISSA_SIGN_BIT != 0 {
            mantissa >>= 8;
            exponent += 1;
        }

        Self((exponent << 24) | (mantissa & MANTISSA_MASK))
    }
}

/// Reasons a compact target could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompactError {
    /// The mantissa's sign bit is set. Targets are unsigned.
    #[error("compact target {0:#010x} has the sign bit set")]
    NegativeTarget(u32),
    /// A zero mantissa encodes a zero target, which no hash can satisfy.
    #[error("compact target {0:#010x} encodes zero")]
    ZeroTarget(u32),
    /// The exponent shifts the mantissa beyond 256 bits.
    #[error("compact target {0:#010x} overflows 256 bits")]
    Overflow(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn decodes_bitcoin_genesis_bits() {
        // 0x1d00ffff is Bitcoin's genesis difficulty; its target is a value
        // every implementation agrees on, which makes it a good fixed point.
        let target = CompactTarget(0x1d00_ffff).to_target().unwrap();
        assert_eq!(target, U256::from(0xffffu64) << (8 * (0x1d - 3)));
    }

    #[test]
    fn rejects_negative_mantissa() {
        assert_eq!(
            CompactTarget(0x1d80_0000).to_target(),
            Err(CompactError::NegativeTarget(0x1d80_0000))
        );
    }

    #[test]
    fn rejects_encodings_whose_shift_annihilates_the_mantissa() {
        // 0x01000001: exponent 1, mantissa 1, shifted down 16 bits -> zero.
        // A zero target can never be met, so decoding must fail loudly rather
        // than hand back a target that rejects every hash.
        assert_eq!(
            CompactTarget(0x0100_0001).to_target(),
            Err(CompactError::ZeroTarget(0x0100_0001))
        );
    }

    #[test]
    fn rejects_zero_mantissa() {
        assert_eq!(
            CompactTarget(0x1d00_0000).to_target(),
            Err(CompactError::ZeroTarget(0x1d00_0000))
        );
    }

    #[test]
    fn rejects_overflowing_exponent() {
        assert!(matches!(CompactTarget(0xff00_ffff).to_target(), Err(CompactError::Overflow(_))));
    }

    #[test]
    fn small_targets_roundtrip_exactly() {
        // Exact only while the mantissa's top bit stays clear: 0x7fffff is the
        // largest value representable without renormalisation.
        for value in [1u64, 2, 0xff, 0x1234, 0x7fffff] {
            let compact = CompactTarget::from_target(U256::from(value));
            assert_eq!(compact.to_target().unwrap(), U256::from(value), "value {value}");
        }
    }

    #[test]
    fn mantissa_sign_bit_forces_renormalisation() {
        // 0xffffff cannot be encoded directly: its top mantissa bit is the
        // encoding's sign bit. It must renormalise to 0x04ffff00, losing the
        // low byte *downwards* so the target never gets easier.
        let compact = CompactTarget::from_target(U256::from(0xffffffu64));
        assert_eq!(compact, CompactTarget(0x0400_ffff));
        assert_eq!(compact.to_target().unwrap(), U256::from(0xffff00u64));
    }

    #[test]
    fn encoding_never_rounds_up() {
        // Rounding up would make the target *easier* than requested, silently
        // lowering difficulty. It must always round down.
        for value in [0x1234_5678u64, 0xdead_beef_cafe, u64::MAX] {
            let original = U256::from(value);
            let decoded = CompactTarget::from_target(original).to_target().unwrap();
            assert!(decoded <= original, "value {value:#x} rounded up");
        }
    }

    proptest! {
        /// Encoding then decoding never produces an easier target.
        #[test]
        fn roundtrip_never_increases_target(hi: u128, lo: u128) {
            let target: U256 = (U256::from(hi) << 128usize) | U256::from(lo);
            prop_assume!(!target.is_zero());
            let compact = CompactTarget::from_target(target);
            if let Ok(decoded) = compact.to_target() {
                prop_assert!(decoded <= target);
            }
        }

        /// Decoding is the inverse of encoding for values that are already
        /// representable, i.e. encoding is idempotent.
        #[test]
        fn encoding_is_idempotent(hi: u128, lo: u128) {
            let target: U256 = (U256::from(hi) << 128usize) | U256::from(lo);
            prop_assume!(!target.is_zero());
            let once = CompactTarget::from_target(target);
            if let Ok(decoded) = once.to_target() {
                prop_assert_eq!(CompactTarget::from_target(decoded), once);
            }
        }

        /// Any decodable compact value encodes back to itself.
        #[test]
        fn decodable_values_are_fixed_points(raw: u32) {
            if let Ok(target) = CompactTarget(raw).to_target() {
                prop_assert_eq!(CompactTarget::from_target(target).to_target().unwrap(), target);
            }
        }
    }
}
