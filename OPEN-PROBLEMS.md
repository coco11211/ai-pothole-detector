# OPEN PROBLEMS

Things known to be unsolved. Not bugs, not TODOs — genuine open questions that
the design does not answer.

## P-001 — adversarial merge sets defeat the ordering rule

The layering rule (ARCHITECTURE.md §6.3) recovers parallelism only when
transactions in a merge set touch disjoint static access sets. An adversary
who controls transaction submission can make every transaction touch one hot
address. Every round then has size one and execution serialises completely.

Unlike Solana, no leader chooses the order to optimise it — DAG topology
imposes it. There is no known solution. Mitigations that do *not* solve it:
per-account gas limits (bound the damage, do not remove it), fee pressure on
hot accounts (changes the economics, not the ordering).

Explicitly out of scope. Documented so no later work pretends it was handled.

## P-002 — per-block gas limit at 10 bps is too small for large deploys

`BLOCK_GAS_LIMIT = 30_000_000 / 10 = 3_000_000` at the M9 rate. Large contract
deployments exceed this. A transaction cannot span blocks.

Options, none chosen: raise `TARGET_GAS_PER_SECOND` (requires evidence that
execution keeps up); allow a per-block limit above the amortised target with a
rolling DAG-wide gas budget (complicates validation, needs a new consensus
rule); stay at 1 bps (abandons the M9 goal). **M9 cannot pass without picking
one.**

## P-003 — header-only sync trusts state up to D blocks behind

By construction (ARCHITECTURE.md §5) the tip carries no state commitment for
itself. A light client following headers has no state proof for the last `D`
blocks. This is inherent to deferred state roots, shared with every chain that
uses them. Noted so it is not rediscovered as a bug.

## P-004 — state root recomputation is O(state) per chain block

M2–M8 compute the state root by walking the full hashed state through
`alloy_trie::root::state_root`
(`registry:alloy-trie-0.9.5/src/root.rs:156`). Correct, and fine at test
scale. It will not survive a state of any real size at 1 block/second.

Replacement is an incremental/sparse trie that updates only touched paths.
Scheduled debt, not an oversight. Must be resolved before M8's 24-hour soak is
meaningful at realistic state sizes.

## P-005 — `PREVRANDAO` is miner-grindable

See DECISIONS.md D-012. A proof-of-work chain cannot supply an unbiasable
randomness beacon. Contracts that use `PREVRANDAO` as randomness are insecure
here, as they were on pre-merge Ethereum. Documented at the definition site
and in RPC docs. Not fixable at this layer.

## P-006 — PoW function is unreviewed

Two-round Keccak-f[1600] is a testnet placeholder chosen for simplicity, not
for ASIC resistance or any analysed security margin. It requires independent
cryptanalytic review before any launch with value. Behind a trait so it can be
swapped.

## P-007 — no finality, by design

Pure Nakamoto: probabilistic settlement only. `chainname_*` RPC exposes
blue-score depth so callers can choose their own confidence threshold. There
is no point at which a reorg becomes impossible, only increasingly unlikely.
Exchange-style integrations must pick a depth and own that choice.

## P-008 — `k` at 10 bps is not yet known

`k = 18` is Kaspa-proven for 1 bps. The PHANTOM formula needs a measured
propagation delay bound, which does not exist until the M5 harness runs.
Guessing it would be the single most dangerous shortcut available. M9 gate.
