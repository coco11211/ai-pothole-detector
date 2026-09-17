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

## P-009 — reachability is O(|past|), not O(1)

`DagStore::is_ancestor_of` is a memoised breadth-first search over parent
edges. Correct, simple, and adequate for tests and moderate DAGs. It is called
inside the k-cluster colouring loop, so its cost multiplies.

Kaspa solves this with interval-labelled reachability, which answers ancestry
in O(1) by assigning each block an interval in a tree traversal and testing
containment. That is the known fix, deliberately deferred.

This is a performance ceiling, not a correctness gap, but it will bind before
M8's soak test is meaningful at scale. Scheduled debt.

## P-010 — paying red blocks weakens the k-cluster incentive

The brief specifies that the coinbase pays *every* block in the merge set, so
that mining on the DAG rather than withholding is always rewarded. Implemented
as specified.

Kaspa does not do this: it pays only blue blocks. The difference matters. If
red blocks are paid, a miner who violates the k-cluster property — for example
by withholding blocks and releasing them late — is still paid for the blocks
that get coloured red. The k-cluster rule then costs them blue score, which
affects fork choice, but costs them nothing in revenue.

Whether that is acceptable depends on whether blue score alone is a sufficient
deterrent. It is not obvious either way and no analysis has been done here. The
conservative alternative — pay blues only — is a one-line change in the block
executor if this turns out to be wrong. Recorded rather than silently resolved.

## P-011 — redundant data is bounded by nothing

Flood relay means a node receives the same block from several peers. That is
normal and is explicitly *not* treated as misbehaviour — an earlier version
penalised it and honest five-node networks partitioned themselves within
fifteen simulated minutes.

The consequence is that a hostile peer can send blocks we already hold, for
free, as often as it likes. Bandwidth is the cost, and bandwidth is properly
bounded by per-peer rate limiting, not by reputation scoring. No rate limiting
exists yet. It belongs with the M8 resource-exhaustion work.

## P-012 — there is no real transport yet

M5 is built and gated on a **deterministic simulation**: virtual time, seeded
randomness, the real `DagSync` state machine and the real `DagStore`, but
in-memory message passing rather than sockets.

That is the right way round — the engineering standards call for deterministic
seeded simulation precisely because nondeterministic divergence cannot be
debugged, and this harness found four real bugs that a socket-based test would
have shown only as intermittent flakiness (see the M5 commit). But it does mean
no TCP framing, handshake, or backpressure code has been written or tested.

That code must exist before M8's soak test means anything, and before M7 can
be exercised against a real client. Planned to land alongside the RPC server at
M7, which introduces an async runtime anyway.
