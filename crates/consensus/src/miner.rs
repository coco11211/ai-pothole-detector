//! The mining loop.
//!
//! Searches nonces for a header whose proof-of-work hash meets its target.
//! Deliberately simple: correctness first, and the PoW function is a testnet
//! placeholder (OPEN-PROBLEMS.md P-006) so optimising the search would be
//! optimising something that is going to be replaced.

use alloy_primitives::U256;
use chainname_difficulty::CompactTarget;
use chainname_pow::PowHash;
use chainname_primitives::Header;

/// Result of a mining attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MineOutcome {
    /// A nonce satisfying the target was found.
    Found {
        /// The sealed header.
        header: Header,
        /// How many nonces were tried.
        attempts: u64,
    },
    /// The attempt budget was exhausted without a solution.
    ///
    /// Not an error: the caller re-templates (new timestamp, new transactions)
    /// and tries again, which is what a real miner does.
    Exhausted {
        /// How many nonces were tried.
        attempts: u64,
    },
}

/// A single-threaded nonce searcher.
#[derive(Debug)]
pub struct Miner<P> {
    pow: P,
}

impl<P: PowHash> Miner<P> {
    /// Creates a miner using the given proof-of-work function.
    pub const fn new(pow: P) -> Self {
        Self { pow }
    }

    /// The proof-of-work function in use.
    pub const fn pow(&self) -> &P {
        &self.pow
    }

    /// Searches for a nonce, starting at `start_nonce` and trying at most
    /// `max_attempts`.
    ///
    /// `start_nonce` exists so concurrent miners can partition the search
    /// space without duplicating work.
    pub fn mine(
        &self,
        template: Header,
        start_nonce: u64,
        max_attempts: u64,
    ) -> Result<MineOutcome, chainname_difficulty::CompactError> {
        let target = CompactTarget(template.bits).to_target()?;
        let mut header = template;

        for attempt in 0..max_attempts {
            header.nonce = start_nonce.wrapping_add(attempt);
            let hash = self.pow.hash_header(&header);
            if U256::from_be_bytes(hash.0) <= target {
                return Ok(MineOutcome::Found { header, attempts: attempt + 1 });
            }
        }

        Ok(MineOutcome::Exhausted { attempts: max_attempts })
    }
}
