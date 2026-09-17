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

## P-002 — per-block gas limit at 10 bps is too small for large deploys — RESOLVED

`BLOCK_GAS_LIMIT = 30_000_000 / 10 = 3_000_000` at the M9 rate. Large contract
deployments exceed this. A transaction cannot span blocks.

**Resolved at M9**, by separating two numbers that were wrongly the same one.

`block_gas_target()` is the amortised budget — the throughput target divided by
the block rate — and is what EIP-1559 steers towards. `block_gas_limit()` is
the hard ceiling, and is the larger of that target and EIP-7825's
per-transaction cap of 16,777,216.

At 1 bps nothing changes: target 30,000,000, ceiling 30,000,000. At 10 bps the
target is 3,000,000 and the ceiling is 16,777,216, so a maximum-size
transaction still fits in a block, while sustained use above 3,000,000 per
block drives the base fee up exponentially. The fee market bounds sustained
throughput; the ceiling only bounds what can be included at all.

The option deliberately *not* taken was raising `TARGET_GAS_PER_SECOND` to
167,000,000 so the division came out above the cap. A microbenchmark of bare
value transfers sustains billions of gas per second
(`crates/chain/tests/throughput.rs`), but that is unrepresentative — no
contract execution, a tiny account set, and a state root computation that is
only cheap because the state is trivial (P-004). Advertising a throughput
figure on that evidence is exactly the "absurd capacity" failure this problem
was raised to avoid.

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

## P-008 — `k` at 10 bps is not yet known — RESOLVED

**Resolved at M9, by measurement.**

`chainname_ghostdag::calculate_k` implements the PHANTOM tail bound:
`k = min { k : P[X > k] <= delta }` for `X ~ Poisson(2 * delay * rate)`. Fed
Kaspa's own inputs — a 5-second bound, 1 bps, delta 0.01 — it returns exactly
18, which is Kaspa's published value and an independent check that the formula
is right.

`crates/testkit/tests/measure_k.rs` then measures full-propagation time across
ten nodes and takes the 99th percentile as the bound:

| rate | measured p99 delay | derived k |
|---|---|---|
| 1 bps | ~5.0 s | 18 |
| 10 bps | ~6.2 s | 151 |

The 1 bps row landing on 18 — Kaspa's value, from our own measurement rather
than from copying it — is the strongest evidence available that the 10 bps row
is trustworthy too.

Getting there required fixing something else first. The request timeout was 10
seconds, roughly twenty times the round trip, and since an in-flight request
suppresses duplicates nothing could shortcut it. A block reaching ten peers
involves about thirty messages, so at 2% loss something on the critical path
was dropped for around 40% of blocks, and measured propagation was 10.8s at the
median against 0.6s with no loss at all. That fed straight into `k` and made no
workable value exist at 10 bps. At a 2-second timeout the same measurement
gives 5.0s and 6.2s.

## P-009 — reachability is O(|past|), not O(1) — MITIGATED

`DagStore::is_ancestor_of` is a memoised breadth-first search over parent
edges. Correct, simple, and adequate for tests and moderate DAGs. It is called
inside the k-cluster colouring loop, so its cost multiplies.

Kaspa solves this with interval-labelled reachability, which answers ancestry
in O(1) by assigning each block an interval in a tree traversal and testing
containment. That is the known fix, deliberately deferred.

**Mitigated at M8.** The search is now pruned by *topological height* — the
longest path from genesis, which strictly increases along ancestry — so a
branch is abandoned as soon as it reaches a block no deeper than the one being
looked for. Blue score would not work here: it counts blocks while
selected-parent choice compares work, so the two disagree whenever difficulty
varies, and a prune based on it would return wrong answers on exactly the
chains where difficulty moved.

Two other quadratics were removed at the same time: `DagStore::tips()` scanned
every header on every call (now maintained incrementally), and `compute_reorg`
walked both selected-parent chains to genesis on every block (now walks in
lockstep to the common ancestor, so it costs the reorg's depth rather than the
chain's length).

Together these took a 24-hour ten-node simulated soak from an estimated half a
day of real time to under eight minutes, and made cost linear in chain length
rather than quadratic.

Still open: this is not Kaspa's interval-labelled reachability, which answers
in O(1). The prune makes the current cost affordable, not free.

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

## P-012 — there is no real transport yet — RESOLVED

M5 is built and gated on a **deterministic simulation**: virtual time, seeded
randomness, the real `DagSync` state machine and the real `DagStore`, but
in-memory message passing rather than sockets.

That is the right way round — the engineering standards call for deterministic
seeded simulation precisely because nondeterministic divergence cannot be
debugged, and this harness found four real bugs that a socket-based test would
have shown only as intermittent flakiness (see the M5 commit). But it does mean
no TCP framing, handshake, or backpressure code has been written or tested.

**Resolved.** `crates/net/transport` implements length-prefixed framing and
`crates/net/p2p` the connection runtime: accept loop, dialer with redial, one
task per connection, a bounded outbound queue per peer, and a ticker.

`crates/net/tests/tcp_convergence.rs` runs it over real sockets: two nodes
handshake, a 25-block chain syncs in full with matching tips, and three nodes
in a line relay a block transitively from A to C through B — flood relay
actually relaying rather than two peers exchanging directly. Garbage on the
wire and a frame claiming four gigabytes both get the peer dropped without
taking the node with it.

The deterministic simulation remains the place long multi-node behaviour is
tested, because a wall-clock TCP test that fails once in twenty runs tells
nobody anything.

## P-013 — deferred execution means blocks can carry unexecutable transactions

A miner includes transactions before anyone knows which merge set will contain
them, so it cannot know whether a transaction will still be valid when it
executes: the nonce may have been consumed, or the sender drained, by a
transaction in a block it had never seen.

A block carrying such a transaction is therefore **not invalid**. The executor
skips the transaction and continues. This is the only workable rule — rejecting
the block would let anyone invalidate a competitor's block by front-running one
of its transactions.

The consequence is that transaction inclusion is not free to the network: a
miner earns nothing from a transaction that turns out to be unexecutable, but
the block still had to be propagated and validated. There is no fee for
inclusion, only for execution, so spam that is cheap to produce and expensive
to relay has no economic brake. Kaspa does not have this problem, because a
UTXO transaction's validity is checkable against a known set at inclusion time.

Bounding this needs either a small inclusion fee charged regardless of outcome,
or validation against the selected parent's state at mining time (which
reintroduces some of the coupling deferred execution was meant to remove).
Neither is chosen. It must be resolved before the chain carries anything worth
spamming.

## P-014 — the undo journal is unbounded in principle — PARTIALLY RESOLVED

`ChainExecutor` keeps one undo record per executed chain block and prunes only
when told to. The record holds the pre-value of every account a block touched,
so a block touching a large contract's storage produces a large record.

**Partially resolved at M8.** `ChainExecutor` now prunes automatically after
every block, keeping records only within the pruning window. The 24-hour soak
confirms the journal tracks chain height rather than growing without bound.

Still open: nothing bounds a *single* record's size, so one block touching a
very large contract's storage produces a very large record. And a reorg deeper
than the pruning window still cannot be undone — the node would have to resync,
and that path does not exist.

## P-015 - `eth_getLogs` scans blocks instead of using an index

Log queries walk every block in the requested range and filter in memory. The
range is capped at 10,000 blocks so a single query cannot be unbounded, but at
one block per second that cap is under three hours of history, and the scan is
linear in the range whether or not anything matches.

A real implementation keeps an address-and-topic index. Dapps with event-heavy
front ends will feel this before anything else does.

## P-016 - the dev node does not yet join the peer-to-peer network

A TCP transport now exists and is tested (P-012, resolved), but
`chainname-node --dev` does not use it: it still runs a standalone chain with
RPC and mining and no peers.

So all three parts work — RPC against real clients, convergence over real
TCP, convergence in simulation — but the binary wires only two of them
together. What remains is plumbing, not design: give the dev node a
`P2pNode`, route mined blocks through `announce_local_block`, and feed
accepted blocks into the backend.
