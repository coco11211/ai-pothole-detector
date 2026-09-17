# CHAINNAME — Architecture

A proof-of-work L1. EVM execution via `revm`. GHOSTDAG blockDAG consensus.
No validator set, no stake, no attestation, no voting. Testnet only.

Every reth/alloy/revm claim below cites a file path and symbol that was read
directly from the pinned source. Paths under `reth:` are relative to
`paradigmxyz/reth` at tag `v2.6.0` (commit `73a3a00862a8f14f89e30da8de001456f18cfae0`).
Paths under `registry:` are relative to `~/.cargo/registry/src/index.crates.io-*/`.

---

## 1. Base and pinning

| Component | Pin | Why |
|---|---|---|
| reth | `v2.6.0` (version anchor, not a dependency of most crates) | latest release tag at time of writing |
| revm | `43.0.2` | reth v2.6.0's workspace pin (`reth:Cargo.toml:430`) |
| alloy-evm | `0.39.0` | `reth:Cargo.toml:442` |
| alloy-consensus / -eips / -genesis / -rpc-types-eth | `2.4.2` | `reth:Cargo.toml:448,450,451,463` |
| alloy-primitives / -sol-types | `1.6.1` | `reth:Cargo.toml:436,437` |
| alloy-trie | `0.9.4` | `reth:Cargo.toml:444` |
| Rust toolchain | `1.98.1` | reth v2.6.0 declares `rust-version = "1.95"` (`reth:Cargo.toml` `[workspace.package]`); container shipped 1.94.1 |

We match reth's exact dependency pins rather than depending on most `reth-*`
crates directly. See §2 for why, and DECISIONS.md D-002 / C-001.

`upstream` git remote points at `paradigmxyz/reth` for cherry-picking
security fixes into any vendored code.

---

## 2. The execution/consensus boundary (what we take from reth, what we don't)

### 2.1 What we take

The reusable part of reth is not reth — it is the `alloy-evm` + `revm`
execution layer that reth itself re-exports. `reth-evm`'s entire public EVM
surface is a re-export:

```rust
// reth:crates/evm/evm/src/lib.rs:57
pub use alloy_evm::{
    block::{state_changes, system_calls, OnStateHook},
    *,
};
```

and `reth-revm` is likewise a re-export shell:

```rust
// reth:crates/revm/src/lib.rs
pub use revm::{self, database::State, *};
```

So we depend on `alloy-evm` and `revm` at reth's pinned versions and get
byte-identical EVM semantics without inheriting reth's node stack.

Concrete symbols we build on:

| Symbol | Location | Use |
|---|---|---|
| `EthEvmFactory` | `registry:alloy-evm-0.39.0/src/eth/mod.rs:268` | constructs a configured EVM |
| `EvmFactory` trait | `registry:alloy-evm-0.39.0/src/evm.rs:259` | our factory wrapper implements/uses this |
| `Evm` trait | `registry:alloy-evm-0.39.0/src/evm.rs:27` | per-transaction execution |
| `BlockEnv` | `registry:revm-context-43.0.2/src/block.rs:8` | `beneficiary`, `prevrandao`, `basefee`, `gas_limit` |
| `ContextTr::set_block` | `registry:revm-context-interface-43.0.1/src/context.rs:283` | **mutate beneficiary between transactions** — this is what makes per-merged-block coinbase attribution possible |
| `SpecId` | `registry:revm-primitives-43.0.0/src/hardfork.rs:15` | hardfork selection; we pin `OSAKA` |
| `EthereumTxEnvelope<Eip4844>` | `registry:alloy-consensus-2.4.2/src/transaction/envelope.rs:486` | transaction type; generic over the 4844 variant |
| `alloy_trie::root::state_root` | `registry:alloy-trie-0.9.5/src/root.rs:156` | state root from a hashed state iterator |
| `alloy_trie::root::ordered_trie_root` | `registry:alloy-trie-0.9.5/src/root.rs:19` | transactions root, receipts root |

### 2.2 What we do NOT take, and why

**`reth-provider` / `reth-storage-api`.** Keyed on a linear canonical chain:
`BlockNumReader` (`reth:crates/storage/storage-api/src/block_id.rs:12`) and
`CanonChainTracker` (`reth:crates/storage/storage-api/src/chain_info.rs:5`)
assume a total order `BlockNumber -> BlockHash` with one canonical block per
height. In a blockDAG many blocks share a height and most are never on the
selected parent chain. Bending these traits to a DAG would mean lying to every
consumer of them.

**`reth-consensus`.** `HeaderValidator::validate_header_against_parent`
(`reth:crates/consensus/consensus/src/lib.rs:152`) takes exactly one parent.
GHOSTDAG headers have many. The trait shape is wrong, not just the logic.

**`reth-eth-wire` / devp2p.** The `eth` protocol is chain-shaped:
`GetBlockHeaders`, `GetBlockBodies` (`reth:crates/net/eth-wire-types/src/message.rs:337-339`)
are range queries over block numbers. DAG sync is a different query shape
(by hash, by blue score, by anti-past). We write our own wire protocol.

**`reth-node-builder` / engine.** Built around the Engine API and a beacon
consensus client. We have neither.

This is a deliberate override of the brief's "embed reth's execution
components"; see DECISIONS.md C-001. The spirit — do not fork reth, reuse its
execution — is preserved exactly. The letter, "use `reth-ethereum` crates," is
narrowed to the subset that is not chain-shaped.

---

## 3. Crate layout

```
crates/
  primitives/   Header, Block, chain parameters
  pow/          PowHash trait + DoubleKeccak256
  difficulty/   compact target codec, ASERT, integer cubic 2^x
  ghostdag/     DAG store, blue set, blue score, selected parent chain,
                merge set, the canonical ordering rule (§6), k derivation
  consensus/    genesis, header validation, miner
  storage/      redb-backed block store
  execution/    revm driver, world state, state root
  chain/        THE SEAM (§7): reorg, undo journal, bodies, chain-block
                executor, parallel execution
  pool/         mempool with fee-rate eviction
  net/          wire protocol, peer scoring, sync state machine, TCP transport
  rpc/          backend, eth_* namespace, chainname_* DAG namespace, server
  node/         config, dev chain, wiring
  testkit/      deterministic multi-node simulation harness
bin/
  chainname-node
fuzz/           cargo-fuzz targets (excluded from the workspace)
```

Dependency direction is strictly downward. `ghostdag` does not depend on
`execution`; `execution` does not depend on `ghostdag`. **The seam lives in
`chain`**, which is the only crate that sees both.

That is a change from this document's first draft, which put the seam in
`node`. Keeping it in its own crate means it can be tested directly — reorg
equivalence, undo-to-genesis, parallel-versus-sequential — without booting a
node, and `node` stays what it should be: wiring.

---

## 4. Block and header

A DAG block is a **transaction batch with no state commitment**.

```
Header {
    version:            u16
    parents:            Vec<BlockHash>   // >= 1, ordered, deduplicated
    timestamp_ms:       u64
    bits:               u32              // compact difficulty target
    nonce:              u64
    miner:              Address          // who is paid for THIS block
    txs_root:           B256             // ordered_trie_root over this block's txs
    // --- deferred, see §5 ---
    deferred_height:    u64              // = chain_height - D, or 0
    deferred_state_root:B256
    deferred_receipts_root: B256
    deferred_gas_used:  u64
}
```

`parents[0]` is not privileged. The selected parent is derived by GHOSTDAG,
not declared, so a miner cannot lie about it.

There is **no** `state_root` for this block's own transactions, no
`ommers_hash`, no `withdrawals_root`, no blob fields.

---

## 5. Deferred state root

A DAG block cannot commit to the state after its own execution: its
transactions' effect depends on which merge set eventually contains them, which
is not known when the block is mined.

**Design.** Header at selected-chain height `N` carries the state root of
chain height `N - D`.

`D = DEFERRED_STATE_ROOT_LAG_SECONDS * blocks_per_second`, with
`DEFERRED_STATE_ROOT_LAG_SECONDS = 20`.

- at 1 bps (M3–M8): `D = 20`
- at 10 bps (M9): `D = 200`

Justification for 20 seconds: it must exceed the worst-case execution lag so a
miner always has the root on hand without stalling, and it must be short enough
that a light client gets a state commitment promptly. Twenty seconds is ~4x the
expected propagation delay bound at 1 bps and leaves execution three orders of
magnitude of slack against measured revm throughput. It is a named constant,
changeable in one place.

Consequence: a node syncing headers only is trusting state up to `D` blocks
behind the tip. Documented in OPEN-PROBLEMS.md P-003.

---

## 6. The merge-set ordering rule

This is the consensus-critical core of the seam. It must be a pure,
deterministic function of DAG topology and block contents alone.

### 6.1 Merge set and base sequence

For selected-chain block `N`:

1. `mergeset(N) = past(N) \ past(selected_parent(N))`, plus `N` itself.
2. Order `mergeset(N)` **topologically**, ties broken by
   `(blue_work ASC, block_hash ASC)`. Deterministic because block hashes are
   unique, so the comparator is total and never ties.

   Blue *work*, not blue score: counting blocks would let a miner on an easy
   target outweigh one on a hard target. Implemented as
   `DagStore::compare_blocks` in `crates/ghostdag/src/dag.rs`.
3. Concatenate each block's transaction list in that block order. Red blocks
   are included: folding orphans into the ledger rather than discarding them
   is the point of the DAG. Redness costs a block its contribution to blue
   score, not its transactions.
4. **Deduplicate**, first occurrence wins. A transaction included in several
   parallel blocks executes once. The block of first occurrence owns it for
   coinbase/COINBASE purposes (§8).

Call the result the **base sequence** `T_0 .. T_{n-1}`.

### 6.2 Static access key set

The EVM's true access set is dynamic and cannot be known before execution. We
approximate with what the transaction statically declares:

```
S(tx) = { sender }
      ∪ { tx.to }                     if tx.to is Some
      ∪ { a : a in tx.access_list }   EIP-2930, if present
```

`sender` is always in `S`, which is what makes §6.4 hold.

### 6.3 The rule: longest-path layering of the conflict DAG

Single forward pass over the base sequence. `last_touch: Map<Address, i64>`
initialised empty.

```
for i in 0..n:
    round[i] = 0
    for a in S(T_i):
        if a in last_touch:
            round[i] = max(round[i], last_touch[a] + 1)
    for a in S(T_i):
        last_touch[a] = round[i]

canonical_order = stable_sort_by_key(0..n, |i| (round[i], i))
```

O(n · |S|), integer-only, no allocation beyond the map. The canonical order is
rounds concatenated, each round in base-sequence order.

Transactions inside one round have pairwise-disjoint static access sets, so a
batching scheduler can execute a whole round in parallel (M10) and a sequential
executor just walks the list (M2–M9). The two must produce identical state.

### 6.4 Properties (each gets a property test at M4)

- **P1 Permutation.** `canonical_order` is a permutation of the deduplicated
  base sequence. Nothing added, nothing lost.
- **P2 Sender order preserved.** For any two transactions with the same
  sender, relative order in `canonical_order` equals relative order in the
  base sequence. Follows from `sender ∈ S(tx)`: same-sender transactions
  always conflict, so their rounds are strictly increasing. This is what keeps
  nonces monotonic.
- **P3 Determinism.** Two independently-built DAG instances containing the same
  blocks produce identical `canonical_order`, regardless of insertion order.
- **P4 Intra-round disjointness.** Within a round, static access sets are
  pairwise disjoint.

### 6.5 What this does not solve

An adversary who controls transaction submission can construct a merge set
where every transaction touches one hot address, collapsing every round to
size one and serialising execution completely. This is not solved here and we
do not claim to solve it. OPEN-PROBLEMS.md P-001.

---

## 7. The seam: GHOSTDAG ordering into revm

Execution is driven along the **selected parent chain only**. DAG blocks off
the chain are never independently executed; they contribute their transactions
to whichever chain block merges them.

```
  DAG (ghostdag crate)                      Execution (execution crate)
  ─────────────────────                     ──────────────────────────
  new block arrives
    → insert into DagStore
    → compute blue set / blue score
    → recompute virtual selected parent chain
    → emit ChainReorg { removed: Vec<ChainBlock>,
                        added:   Vec<ChainBlock> }
                                     │
                                     ▼   node crate: the seam
                        for b in removed.rev(): state.rollback(b)
                        for b in added:
                            base  = mergeset_base_sequence(b)   §6.1
                            order = layer_and_sort(base)        §6.3
                            state.execute_chain_block(b, order) §7.1
```

### 7.1 `execute_chain_block`

```rust
// crates/execution/src/executor.rs
fn execute_chain_block(
    &mut self,
    ctx: &ChainBlockCtx,          // height, timestamp, basefee, gas limit, prevrandao
    ordered: &[(SourceBlock, Recovered<TxEnvelope>)],
) -> Result<ChainBlockOutcome>
```

1. Build `BlockEnv` from `ctx`. `beneficiary` is a placeholder.
2. Create the EVM once via `EthEvmFactory`
   (`registry:alloy-evm-0.39.0/src/eth/mod.rs:268`) over
   `revm::database::State<OurDb>`.
3. For each `(source_block, tx)` in `ordered`:
   - `ctx.set_block(block_env.with_beneficiary(source_block.miner))` —
     `ContextTr::set_block`, `registry:revm-context-interface-43.0.1/src/context.rs:283`.
     This is what credits the priority fee and sets `COINBASE` to the miner of
     the DAG block that actually carried the transaction, not the chain block.
   - execute, collect receipt, accumulate cumulative gas.
   - transactions that exceed the remaining block gas budget are **skipped,
     not failed**, and stay eligible for a later chain block.
4. Post-execution: credit subsidy to every merged block's miner (§8).
5. Compute `state_root` over the resulting hashed state
   (`alloy_trie::root::state_root`) and `receipts_root`
   (`ordered_trie_root`). Store them keyed by height for a header `D` blocks
   later to carry (§5).

The unit of state transition is the chain block. There is exactly one bank-like
state per selected-chain height, never one per DAG block.

### 7.2 Reorg

State rollback is by **journal**, not by re-execution from genesis. Each
executed chain block writes an undo record: the pre-value of every account and
storage slot it touched. Rolling back `k` chain blocks replays `k` undo records
in reverse. The journal is pruned beyond the finality window (§9).

---

## 8. Issuance, fees, gas

### 8.1 Coinbase pays the whole merge set

Every block in `mergeset(N)` is credited a subsidy to the `miner` address in
**its own header**. Paying only the selected chain would reward withholding and
defeat the DAG.

Subsidy is computed from the **chain height of the block that merged it**, so
two nodes always agree.

### 8.2 Emission

```
subsidy(h) = TAIL + (INITIAL - TAIL) * 2^(-h / H)
INITIAL = 50e18 wei, TAIL = 0.5e18 wei
H       = one year of blocks at the current rate
        = 31_536_000 at 1 bps, 315_360_000 at 10 bps
```

`2^(-x)` uses the **same integer cubic approximation as ASERT** (§10), so there
is exactly one approximation of `2^x` in the codebase and it is tested once.
Smooth decay, no halving cliffs.

### 8.3 Fees

EIP-1559 intact. Base fee **burned**. Priority fee to the merged block's own
miner via `BlockEnv.beneficiary` (§7.1). Base fee is recomputed per selected-
chain block from the parent chain block's gas usage, standard 1559 formula.

### 8.4 Gas limit

```
TARGET_GAS_PER_SECOND = 30_000_000
BLOCK_GAS_LIMIT       = TARGET_GAS_PER_SECOND / blocks_per_second
```

- 1 bps: `30_000_000` per block (≈ Ethereum L1's familiar figure)
- 10 bps: `3_000_000` per block

Derivation: in GHOSTDAG every block is merged and executed, orphans included,
so sustained throughput is `λ × BLOCK_GAS_LIMIT` independent of DAG width.
30M gas/s is ~8x Ethereum L1's current ~3.75M gas/s and sits inside sequential
revm's sustained capability with headroom for trie updates.

The 10 bps figure creates a real problem — a 3M per-block limit cannot fit a
large contract deployment. Recorded in OPEN-PROBLEMS.md P-002; M9 must resolve
it before the rate is raised.

---

## 9. GHOSTDAG parameters

| Constant | 1 bps | 10 bps | Source |
|---|---|---|---|
| `TARGET_BLOCK_INTERVAL_MS` | 1000 | 100 | brief |
| `GHOSTDAG_K` | 18 | recompute at M9 | Kaspa-proven for 1 bps |
| `FINALITY_DEPTH_BLOCKS` | 86_400 | 864_000 | 24h, Kaspa convention |
| `DEFERRED_STATE_ROOT_LAG` (D) | 20 | 200 | §5 |
| `MERGESET_SIZE_LIMIT` | 180 | 180 | 10 × k, DoS bound |

`k` at 10 bps is recomputed from the PHANTOM paper's formula against the
*measured* propagation delay bound from the M5 harness, not guessed. M9 gate.

---

## 10. Difficulty — ASERT

Per block, integer-only, Bitcoin Cash specification's cubic approximation to
`2^x`.

```
next_target = anchor_target * 2^((t_delta - ideal_delta) / HALFLIFE)
HALFLIFE = 7200 seconds (2 hours)
```

The exponent is clamped before the shift so the fixed-point intermediate cannot
overflow. `#[deny(clippy::float_arithmetic)]` is set workspace-wide
(`Cargo.toml` `[workspace.lints.clippy]`) so no consensus path can regress into
floating point.

---

## 11. Proof of work

Two-round Keccak-f[1600] over the serialised header with the nonce, truncated
to 256 bits. **Testnet placeholder.** Behind a `PowHash` trait so it is
swappable without touching consensus logic. The definition site carries the
required warning comment.

---

## 12. EVM compatibility surface

- **Chain ID `7717`.**
- **Spec level `SpecId::OSAKA`** (`registry:revm-primitives-43.0.0/src/hardfork.rs:68`).
  `AMSTERDAM` exists but is marked "Activated at block TBD" and is not final.
- **No EIP-4844.** Type-`0x03` transactions are rejected at decode, at mempool
  admission, and at block validation. `blob_excess_gas_and_price` is set to
  `Some(zero)` rather than `None` so `BLOBHASH`/`BLOBBASEFEE` return defined
  values instead of risking a panic; no blob can ever be present.
- **`PREVRANDAO`** returns the chain block's own PoW hash (the `mix_hash`
  analogue). Miner-grindable. The RPC docs and the constant's comment both say,
  in terms: *this is not a randomness beacon, do not use it for security.*
  A PoW chain cannot offer an unbiasable beacon; pretending otherwise would be
  worse than stating it.
- **`eth_*` JSON-RPC unchanged in shape.** MetaMask, ethers, viem, Foundry work
  with no patches.
- **DAG data in a new `chainname_*` namespace.** Nothing is bolted onto
  existing `eth_*` response shapes.
- **`eth_getTransactionReceipt`** reports `confirmations` as blue-score depth,
  which is the field's natural meaning here and keeps existing tooling honest.
- `eth_getBlockByNumber` addresses **selected-chain blocks**. Off-chain DAG
  blocks are reachable only via `chainname_*`. A "block number" is a
  selected-chain height.

---

## 13. Storage

`redb` for block/header/DAG indexes and the undo journal; a flat hashed-state
table for accounts and storage. Chosen over MDBX because we need neither
reth's table schema nor its static-file layout, and redb is pure Rust with no
C build dependency. DECISIONS.md D-008.

State root is recomputed from the hashed state via
`alloy_trie::root::state_root` at M2–M8 scale. This is O(state) per chain block
and will not survive a large state; replacing it with an incremental trie is
OPEN-PROBLEMS.md P-004 and is a known, scheduled debt, not an oversight.


---

## 14. What changed after this document was first written

This file was written at M0, before any code existed, and most of it survived.
These are the places where contact with the source moved it, each recorded in
full in DECISIONS.md:

* **The seam lives in `crates/chain`, not `node`** (§3 above). Testability.
* **The undo journal is built from revm's state diff, not from the static
  access set.** The EVM exceeds the declared access set via `CALL`, `CREATE`
  and `SELFDESTRUCT`; a journal built from the declaration would corrupt state
  on reorg, and only on reorg. D-029.
* **Merge-set ordering ties break on blue *work*, not blue score.** Counting
  blocks lets an easy-target miner outweigh a hard-target one. D-020.
* **Block bodies travel with headers.** A header in the DAG is immediately
  executable, so a body arriving second is an indefensible race. D-030.
* **The block gas *ceiling* and the EIP-1559 *target* are separate numbers.**
  Dividing a throughput target by a high block rate produced a ceiling below
  EIP-7825's per-transaction cap, which would have made large contract
  deployments impossible at any price. D-042.
* **`k` is measured, not assumed** — 18 at 1 bps, 151 at 10 bps, from measured
  propagation bounds fed through the PHANTOM formula. D-041.
* **The proof-of-work function is double Keccak-256**, reading the brief's
  "two-round Keccak-f[1600]" as two full applications rather than a
  reduced-round permutation, which would have been trivially invertible.
  D-016.

The deferred state root (§5), the ordering rule (§6) and the seam design (§7)
are as first written.
