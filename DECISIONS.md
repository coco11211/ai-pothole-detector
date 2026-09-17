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
