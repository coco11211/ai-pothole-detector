# DECISIONS

Append only. Never rewrite history. Tags: SETTLED, OPEN, OPEN-RISK, CORRECTIONS.

---

## D-001 — reth pinned at v2.6.0 — SETTLED

Latest release tag. Commit `73a3a00862a8f14f89e30da8de001456f18cfae0`.
We pin reth's *dependency versions* (revm 43.0.2, alloy-evm 0.39.0, alloy-*
2.4.2 / 1.6.1, alloy-trie 0.9.4) rather than tracking a branch, per the brief.

## D-002 — depend on `alloy-evm` + `revm` directly, not `reth-*` — SETTLED

See CORRECTIONS C-001. `reth-evm` is a re-export of `alloy-evm`
(`reth:crates/evm/evm/src/lib.rs:57`) and `reth-revm` a re-export of `revm`
(`reth:crates/revm/src/lib.rs`), so depending on the underlying crates at
reth's exact pins gives identical EVM semantics with a far smaller graph.

## D-003 — Rust toolchain 1.98.1 — SETTLED

reth v2.6.0 declares `rust-version = "1.95"`. The container shipped 1.94.1,
which cannot compile it. Installed 1.98.1 (latest stable) and pinned it in
`rust-toolchain.toml` so CI and local builds agree.

## D-004 — chain block is the unit of state transition — SETTLED

One state per selected-parent-chain height. DAG blocks off the chain are never
independently executed. Expensive to reverse, but it is forced: a DAG block's
state effect is undefined until a merge set contains it.

## D-005 — deferred state root lag D = 20 seconds of blocks — SETTLED

D = 20 at 1 bps, 200 at 10 bps. Defined in time, not blocks, so raising the
block rate at M9 does not silently shrink the execution slack. Cheap to
reverse: one named constant.

## D-006 — merge-set ordering by longest-path layering — SETTLED

ARCHITECTURE.md §6.3. Chosen over (a) plain concatenation, which gives a
batching scheduler nothing, and (b) full conflict-graph colouring, which is
NP-hard and would need a tie-breaking rule anyway. Layering is a single
forward pass, integer-only, and yields the sender-order property (P2) for
free, which is required for nonce correctness.

## D-007 — `COINBASE` and priority fee follow the *merged* block's miner — SETTLED

Not the chain block's miner. Enabled by `ContextTr::set_block`
(`registry:revm-context-interface-43.0.1/src/context.rs:283`), verified to
exist before committing to the design. Consistent with the brief's "each
merged block's miner is credited from its own header," and makes `COINBASE`
mean something defensible inside a merge set.

## D-008 — `redb` for storage, not MDBX / reth-db — OPEN

We need neither reth's table schema nor its static-file layout, and redb is
pure Rust with no C toolchain dependency. Reversible: storage sits behind a
trait in `crates/storage`. Marked OPEN because no benchmark has been run yet;
if redb write amplification hurts at M8 soak, swapping to MDBX is a
crate-local change.

## D-009 — no floating point in consensus, enforced by lint — SETTLED

`clippy::float_arithmetic = "deny"` at the workspace level. `2^x` appears
twice in the design (ASERT retargeting, emission decay) and both use the same
integer cubic approximation so there is one implementation to test.

## D-010 — `SpecId::OSAKA` as the EVM spec level — SETTLED

`AMSTERDAM` exists in revm 43 but is documented "Activated at block TBD"
(`registry:revm-primitives-43.0.0/src/hardfork.rs:73-75`) and is not final.
OSAKA is the latest finalised fork. Raising it later is a one-line change
plus a fork-activation rule.

## D-011 — blob fields present-but-zero rather than absent — SETTLED

EIP-4844 is cut per the brief. But `BlockEnv.blob_excess_gas_and_price` is set
to `Some(zero)` rather than `None`, because the spec level is >= CANCUN and
`BLOBHASH` / `BLOBBASEFEE` must return defined values. Type-`0x03`
transactions are rejected at decode, mempool, and block validation. Cheapest
reversible option: no blob can exist, but no opcode can trap.

## D-012 — `PREVRANDAO` = the chain block's own PoW hash — OPEN-RISK

A PoW chain cannot provide an unbiasable randomness beacon. Any choice here is
grindable by the miner of whichever block supplies it. Options considered:
zero (breaks contracts that branch on it), a hash from D blocks back (still
grindable, just earlier, and adds coupling), the block's own PoW hash
(grindable now, but matches pre-merge Ethereum `mixHash` semantics that
tooling already expects).

Chose the block's own PoW hash as the most conventional and least surprising.
Tagged OPEN-RISK, not SETTLED, because contracts *will* misuse it. The
definition site and the RPC documentation both state plainly that it is not a
randomness source. Not solvable at this layer; recorded in OPEN-PROBLEMS.md
P-005.

## D-013 — target 30M gas/second — OPEN-RISK

`BLOCK_GAS_LIMIT = 30_000_000 / blocks_per_second`. At 1 bps this is a
comfortable, familiar 30M. At 10 bps it is 3M per block, which is too small
for large contract deploys. Recorded as OPEN-PROBLEMS.md P-002 and gating M9.
Chose to keep the derivation honest and surface the problem rather than pick a
gas target that flatters the block rate.

## D-014 — own wire protocol, not devp2p — SETTLED

`reth-eth-wire`'s `eth` protocol is range-queries over block numbers
(`reth:crates/net/eth-wire-types/src/message.rs:337-339`). DAG sync queries by
hash and by anti-past. Reusing devp2p framing while replacing every message
would buy only the ECIES handshake.

## D-015 — staged block rate, 1 bps first — SETTLED

Per the brief, and independently correct: `k=18` is only proven for 1 bps, and
recomputing it needs a *measured* propagation bound that does not exist until
the M5 harness runs.

## D-016 — "two-round Keccak-f[1600]" read as double-Keccak256 — OPEN-RISK

The brief said "two-round Keccak-f\[1600\] over the block header, truncated
to 256 bits". That admits two readings:

1. Two applications of the *hash* — `keccak256(keccak256(header))`, i.e.
   Bitcoin's double-SHA-256 with Keccak substituted. Each application runs the
   full 24-round permutation.
2. A **reduced-round** permutation running 2 of Keccak-f's 24 rounds.

Reading 2 produces a function that is trivially invertible. Two-round Keccak-f
has been broken in practice; a miner could compute preimages directly rather
than searching nonces, which does not weaken the proof of work, it removes it
entirely. Implemented reading 1.

Tagged OPEN-RISK rather than SETTLED because it is an interpretation of an
ambiguous instruction, not a verified requirement. Reversing it is a one-line
change behind the `PowHash` trait. The function is a testnet placeholder either
way and still requires cryptanalytic review — OPEN-PROBLEMS.md P-006.

## D-017 — genesis is the ASERT anchor — SETTLED

Absolute ASERT: every block's difficulty is computed from genesis directly,
never from its parent. Retarget error cannot accumulate, and any block's
difficulty is independently verifiable from the anchor plus its own height and
timestamp — no need to walk the chain. This is the Bitcoin Cash `aserti3-2d`
form.

## D-018 — block timestamps are milliseconds — SETTLED

Headers carry `timestamp_ms`. At 10 blocks/second a second-resolution
timestamp cannot order blocks or drive ASERT. The EVM's `TIMESTAMP` opcode is
still fed seconds, because contracts depend on that unit.

## D-019 — future-timestamp drift bounded at 2 minutes — SETTLED

A block claiming a future timestamp makes the chain look *behind* schedule,
which ASERT answers by lowering difficulty. An unbounded future timestamp is
therefore a difficulty attack, not a cosmetic problem. Two minutes is far above
any plausible honest clock skew and far below the 2-hour half-life, so honest
blocks are never rejected and the attack is bounded to a negligible nudge.

## D-020 — selected parent chosen by blue work, not blue score — SETTLED

Counting blue *blocks* would let a miner on an easy target outweigh one on a
hard target, which is a difficulty-manipulation attack on fork choice. Blue
work sums each blue block's expected hashes (`crates/ghostdag/src/work.rs`).
Ties break on block hash, so the comparator is total and every node resolves
them identically.

## D-021 — genesis is its own selected parent — SETTLED

Genesis is the one block with no parent. Making its selected parent
self-referential means chain walks terminate on a value rather than on an
`Option`, so no caller has to special-case the root. The walk stops on the
`cursor == genesis` check, not on a null.

## D-022 — merge set excludes the selected parent — SETTLED

`mergeset(N) = past(N) \ past(selected_parent(N))` does not contain the
selected parent itself. The selected parent is the *previous chain block*; its
transactions were executed when it was the chain tip. Including it would
execute them twice.

Consequence worth remembering: a block with ten parents has a merge set of
nine, not ten.

## D-023 — red blocks' transactions are executed — SETTLED

Colouring is about blue score and fork choice, not about inclusion. Red blocks
contribute their transactions to the execution order exactly like blue ones.
Discarding them would make the DAG pointless — folding orphans into the ledger
instead of throwing them away is the reason for using one.

The separate question of whether red blocks should be *paid* is
OPEN-PROBLEMS.md P-010.

## D-024 — the sync layer is a pure state machine — SETTLED

`DagSync` takes a message and returns a list of actions. It does no I/O, owns
no clock (time is a parameter), and holds no sockets.

This is the single most useful structural decision in the networking layer. It
is what lets five nodes run thirty simulated minutes deterministically, and it
turned four real bugs from "intermittent flakiness" into "reproducible from
seed 7". Transport code wraps it; it never wraps transport.

## D-025 — peers are ordered, not hashed — SETTLED

`DagSync::peers` is a `BTreeMap` and `waiting_on`'s children are a `BTreeSet`.
Iteration order over these feeds directly into the actions emitted, and
`HashMap` ordering varies per process, which made the seeded simulation
non-reproducible. Determinism in anything that reaches the wire is a
requirement here, not a preference.

## D-026 — duplicate blocks are not misbehaviour — SETTLED

Under flood relay every peer announces every block and responses race. An
earlier version charged a small penalty per duplicate; over thirty simulated
minutes the penalties accumulated until honest nodes disconnected each other
and the network partitioned permanently.

Redundant data is a *bandwidth* problem, bounded by rate limiting, not a
*trust* problem bounded by reputation. OPEN-PROBLEMS.md P-011.

## D-027 — timed-out requests rotate across peers — SETTLED

Re-asking the peer that already failed to answer is a deadlock, not merely
inefficient: a peer that does not have a block never will, and duplicate-request
suppression means the hash stays in `requested` and every later attempt is
filtered out. A handful of blocks became permanently unobtainable and the
five-node network never converged.

Retries now spread across all ready peers from a rotating offset.

## D-028 — periodic tip reconciliation — SETTLED

Flood relay announces a block once. A lost announcement is usually rescued by a
later block, whose parents get requested and pull the missing ancestor in as an
orphan resolves. But the newest blocks have no descendants yet, so a lost
announcement for a *tip* is never recovered by that mechanism.

Asking every peer for its tips on a timer closes the hole, and is what makes
convergence eventual rather than merely probable.

## D-029 — the undo record is built from revm's state diff, not the access set — SETTLED

`ChainExecutor` calls `Evm::transact` rather than `transact_commit`, notes the
pre-value of every address in the returned diff, and only then commits.

An earlier version built the undo record from the transaction's *statically
declared* access set. That set is precisely the one the EVM is free to exceed:
a `CALL` to a computed address, a `CREATE`, a `SELFDESTRUCT` all touch accounts
nothing declared. Rolling back from that record would have left those accounts
at their post-execution values, and the divergence would have surfaced only
after a reorg, as a state root mismatch with no proximate cause.

The static access set is for **ordering** only. The revm diff is authoritative
for **state**. Conflating the two is a subtle and expensive mistake.

## D-030 — block bodies travel with their headers — SETTLED

`Message::Blocks` carries `BlockPayload { header, transactions }`, with
transactions as opaque EIP-2718 envelopes.

The alternative — announcing headers and fetching bodies separately — is what
Ethereum does and is better under bandwidth pressure. It is rejected here
because a header in the DAG is immediately executable: the moment a block joins
the DAG it may enter a merge set, and a body arriving second would be a race
that no caller could defend against. Bodies arriving with headers makes "in the
DAG" and "executable" the same condition.

Revisit if block sizes make this expensive; it is a protocol change, so not
cheap to reverse.

## D-031 — the network layer never decodes transactions — SETTLED

`DagSync` stores bodies as opaque bytes. Decoding and sender recovery — which
is an elliptic curve operation per transaction, by far the most expensive thing
on the receive path — happen above it.

Keeping that cost out of the network layer means a peer cannot force
unbounded signature verification simply by sending a large block; the work
happens once, after the block has been accepted into the DAG, where it can be
attributed and bounded.

## D-032 — an unexecutable transaction is skipped, not fatal — SETTLED

See OPEN-PROBLEMS.md P-013. A miner cannot know a transaction's validity when
it includes it, so a block carrying one must still execute. Rejecting the block
would let anyone invalidate a competitor's block by front-running one of its
transactions.

## D-033 - `eth_*` addresses selected-chain blocks - SETTLED

`eth_getBlockByNumber` and every `blockNumber` field mean **selected-chain
height**. It is the only sequence in a blockDAG with one block per height,
which is what every Ethereum client assumes. DAG blocks off the selected chain
are reachable only through `chainname_*`.

The alternative - inventing a numbering covering every DAG block - would give
two blocks the same number and break clients in ways they cannot detect.

## D-034 - DAG data is additive in `eth_*`, never substitutive - SETTLED

Receipts carry `blueScoreDepth` and blocks carry `blueScore`, as *extra*
fields. No Ethereum field is removed, retyped, or repurposed. Strict clients
ignore what they do not recognise; DAG-aware ones get the number that matters
without a second round trip.

The rule this encodes: adding a field is safe, changing one is not.

## D-035 - `safe` and `finalized` resolve to the tip - OPEN-RISK

There is no finality on this chain (OPEN-PROBLEMS.md P-007), so these tags
cannot mean what they mean on Ethereum. Three options: error, resolve to the
tip, or resolve to some depth and call it final.

Chose the tip. Erroring breaks clients that request `finalized` routinely;
picking a depth and calling it final is a lie, and it is exactly the lie that
costs an exchange money. Resolving to the tip is at least transparent: callers
needing settlement confidence read blue-score depth from `chainname_*` and pick
their own threshold.

Tagged OPEN-RISK because a client that trusts `finalized` will be wrong here,
and nothing in the response tells it so.

## D-036 - the node holds no keys - SETTLED

`eth_accounts` returns an empty list and there is no `eth_sendTransaction`.
Signing belongs in a wallet. A node that signs is a custody service with a
JSON-RPC port, and dev-mode convenience is not worth building that habit into
the client. The `--dev` chain pre-funds well-known *published* keys instead,
and warns about it at startup.

## D-037 - the float rule is absolute in consensus, explicit elsewhere - SETTLED

`eth_feeHistory` must emit `gasUsedRatio` as a JSON float; the schema leaves no
choice. Rather than relax the workspace lint, the exception is a single
function carrying `#[allow(clippy::float_arithmetic, reason = ...)]`, computed
from already-final integers and never fed back into state.

The rule stays absolute where it matters, and every exception is visible at its
site rather than invisible in a config file.

## D-038 - reachability prunes on topological height, never blue score - SETTLED

Topological height (longest path from genesis) strictly increases along
ancestry by construction, so it is a sound bound for abandoning a branch during
a reachability search.

Blue score is *not* sound for this and the distinction matters. Blue score
counts blocks; selected-parent choice compares accumulated work. They agree
only while difficulty is constant, which is exactly the condition a test
network satisfies and a real one does not. A prune on blue score would pass
every test here and return wrong answers in production on any chain where
difficulty moved - the worst possible failure mode.

## D-039 - the soak runs in simulated time - SETTLED

The M8 gate asks for a 24-hour ten-node soak. It runs as 24 hours of
*simulated* time, deterministically, from a seed, in under eight minutes.

A wall-clock soak would take a day, could not run in CI, and a divergence in
hour nineteen would be unreproducible and therefore undebuggable. The
simulation runs the real state machine, the real DAG, real proof of work and
real signed transactions; only the clock and the sockets are substituted. What
it does not cover - TCP framing, handshake, backpressure - is covered
separately by `crates/net/tests/tcp_convergence.rs` over real sockets.

## D-040 - an incomplete handshake is repaired, not punished - SETTLED

A message from a peer we have not finished handshaking with means our
`Version` was lost, not that the peer is hostile. The response is to re-send
it, and the periodic tick retries any handshake still outstanding.

An earlier version charged a penalty and disconnected after four such
messages. At 40% packet loss that turned one dropped handshake into a
permanently broken connection, and a six-node network quietly disconnected
itself into permanent non-convergence. On a lossy link a dropped handshake is
not rare, it is expected, so it has to be recoverable.

## D-041 - `k` is measured, never assumed - SETTLED

`GHOSTDAG_K_AT_10_BPS = 151` comes from a measured 99th-percentile propagation
bound of ~6.2 seconds fed through the PHANTOM tail formula, not from scaling
the 1 bps value or copying another chain's.

The formula is validated against Kaspa's published parameters: 5 seconds, 1
bps, delta 0.01 returns exactly 18. Our own measurement at 1 bps also returns
18. Two independent routes to the same number is what makes the 10 bps figure
believable.

## D-042 - the block gas ceiling and the 1559 target are separate numbers - SETTLED

`block_gas_target()` is the amortised budget EIP-1559 steers towards.
`block_gas_limit()` is the hard ceiling, and is `max(target, EIP-7825 tx cap)`.

They were the same number and that was the bug. Dividing a throughput target by
a high block rate produces a ceiling below the per-transaction cap, which means
a large contract deployment cannot be included *at any price* - breaking the
promise that existing Solidity deploys work unchanged. Separating them lets a
block carry one large transaction while the fee market, not an arbitrary
ceiling, bounds sustained throughput. OPEN-PROBLEMS.md P-002.

## D-043 - the request timeout is recovery latency, and must be sized as such - SETTLED

Two seconds, not ten.

The original ten seconds looked harmless: it is a timeout, and timeouts only
fire when something has gone wrong. But an in-flight request suppresses
duplicates, so nothing - not even periodic tip reconciliation - can shortcut
it, and a block reaching ten peers involves about thirty messages. At 2% loss
something on the critical path is dropped for roughly 40% of blocks, so the
timeout was not an edge case: it was the *median* path, and measured
propagation delay was 10.8s against 0.6s on a clean link.

Propagation delay is the input to `k`. A timeout chosen carelessly therefore
sets a consensus parameter, which is not a connection any of this made obvious
until it was measured.

---

# CORRECTIONS

Places where the brief was wrong against the actual source. Each cites proof.

## C-001 — "reth as a library, use reth-ethereum crates" — narrowed

The brief says to embed reth's execution components. Most of reth is shaped
around a linear canonical chain and the Engine API and cannot be embedded by a
DAG node:

- `BlockNumReader` — `reth:crates/storage/storage-api/src/block_id.rs:12` —
  assumes one canonical block per height.
- `CanonChainTracker` — `reth:crates/storage/storage-api/src/chain_info.rs:5` —
  same.
- `HeaderValidator::validate_header_against_parent` —
  `reth:crates/consensus/consensus/src/lib.rs:152` — takes exactly one parent.
  GHOSTDAG headers have many.
- `EthMessage::GetBlockHeaders` / `GetBlockBodies` —
  `reth:crates/net/eth-wire-types/src/message.rs:337-339` — range queries over
  block numbers.

The genuinely reusable layer is `alloy-evm` + `revm`, which is exactly what
`reth-evm` and `reth-revm` re-export (`reth:crates/evm/evm/src/lib.rs:57`,
`reth:crates/revm/src/lib.rs`). We take that, at reth's pins, and write our
own everything-else. The brief's intent — do not fork reth, reuse its
execution — is met. Its letter is not.

## C-002 — Rust 1.94.1 in the container cannot build reth v2.6.0

`reth:Cargo.toml` `[workspace.package]` declares `rust-version = "1.95"`.
Installed 1.98.1 and pinned it. Would have been a hard stop at M1 otherwise.

## C-003 — `BlockExecutor` lives in `alloy-evm`, not reth

The brief implies reth owns the block execution abstraction. It does not, as
of v2.6.0: `BlockExecutor` is
`registry:alloy-evm-0.39.0/src/block/mod.rs:199` and reth re-exports it.

## C-004 — `alloy-evm`'s block executor cannot express merge-set coinbase

`BlockEnv` has a single `beneficiary`
(`registry:revm-context-43.0.2/src/block.rs:14`) and
`EthBlockExecutor::apply_post_execution_changes` pays one beneficiary plus
ommers (`registry:alloy-evm-0.39.0/src/eth/block.rs:308`). Paying every block
in a merge set from its own header is not expressible through that trait, so
we drive `revm` directly and write our own block executor. `ContextTr::set_block`
(`registry:revm-context-interface-43.0.1/src/context.rs:283`) makes the
per-transaction beneficiary switch possible; this was verified before the
design was fixed.

## C-005 — "sort transactions to group non-conflicting access sets" is only possible statically

The brief asks for ordering by access set. EVM access sets are *dynamic* — a
call's storage touches are not knowable before execution, and EIP-2930 access
lists are optional and advisory. The ordering rule can therefore only use
statically declared keys (sender, `to`, declared access list), and the real
conflict detection has to happen at execution time via speculation (M10
Block-STM). ARCHITECTURE.md §6.2 states the approximation explicitly rather
than implying the sort is exact.

## C-006 — 10,000 blocks cannot demonstrate difficulty convergence

The M3 gate asked for 10,000 blocks. The arithmetic says that is not enough.

Adapting to a 10x hashrate change requires the chain to run `log2(10) ≈ 3.32`
half-lives ahead of schedule. At the specified 2-hour half-life that is ~23,900
seconds of accumulated lead. With blocks arriving 10x too fast, lead builds at
9 seconds per second, so the adjustment completes after ~2,650 seconds of real
time — by which point roughly 26,000 blocks have been produced. A 10,000-block
run can only show the chain moving in the right direction, never arriving.

Both are therefore tested in `crates/difficulty/tests/retarget_simulation.rs`:

- `difficulty_converges_after_hashrate_steps` runs 150,000 blocks and asserts
  actual convergence (block time back within 5% of target, difficulty within
  10% of 10x, no stall above 15s, settled spread under 10% of the mean).
- `ten_thousand_blocks_track_in_the_right_direction` runs the literal 10,000
  and asserts what that horizon can actually show.

This is not a relaxation of the gate. It is a stronger test plus the original.

## C-007 — ASERT's 256-bit intermediate silently overflowed (found, fixed)

Not a correction to the brief — a bug in our own first implementation, recorded
because the failure mode is instructive and could easily be reintroduced.

`next_target` computed `anchor_target * factor` in `U256` with
`saturating_mul`. `anchor * factor` needs up to 256 + 17 bits and the
subsequent left shift adds up to 16 more, so any anchor near the top of the
range overflowed. Saturating did **not** contain it: the `>> RADIX_BITS`
afterwards pulled the saturated value back under the pow limit, so the clamp
never fired and a wrong target was returned as though it were correct.

Effect: an anchor of `U256::MAX >> 8` retargeted **256x harder** at zero drift.
The unit tests missed it because they used `U256::MAX >> 40`, which does not
overflow. It surfaced as the M3 single-node chain test taking 16 minutes
instead of 3 seconds — a performance symptom of a correctness bug.

Fixed by computing the intermediate in `U512`
(`crates/difficulty/src/asert.rs`). Three regression tests added:
`near_maximum_anchor_is_unchanged_at_zero_drift`,
`near_maximum_anchor_still_halves_correctly`, `maximum_anchor_does_not_overflow`.

Lesson carried forward: consensus arithmetic tests must include inputs at the
extremes of the type, not just comfortable mid-range values.

## C-008 — "5 nodes on one machine" is met by deterministic simulation

The M5 gate says five nodes on one machine, sustained thirty minutes, under
simulated packet loss and latency. That is implemented as a **deterministic
simulation**: virtual time, a seeded integer LCG, the real `DagSync` state
machine over the real `DagStore`, real proof of work at an easy target, and
in-memory message passing with modelled loss and latency.

Not a shortcut — the engineering standards ask for exactly this ("Deterministic
seeded simulation where possible — you cannot debug nondeterministic
divergence"). It earned its keep immediately, finding four real bugs that a
socket-based test would have surfaced only as intermittent flakiness:

1. `HashMap` iteration order reaching the wire, making runs irreproducible.
2. Duplicate-block penalties partitioning an honest network over ~15 minutes.
3. Timed-out requests re-asked to the same unhelpful peer, deadlocking forever.
4. Lost tip announcements never recovered, because a tip has no descendant to
   rescue it.

Each of those is a bug that only appears at multi-node scale over time.

What it does *not* cover is TCP framing, handshake, and backpressure, because
no socket code exists yet. Recorded honestly as OPEN-PROBLEMS.md P-012, to land
with the RPC server at M7.
