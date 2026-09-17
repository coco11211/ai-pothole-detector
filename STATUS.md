# STATUS

**Read this first. Written for an amnesiac session with zero memory.**

Project: CHAINNAME — a proof-of-work L1 with EVM execution (reth/revm as a
library) and GHOSTDAG blockDAG consensus. No validator set, no stake, no
attestation, no voting. Testnet only.

---

## Current milestone

**M1: SKELETON** — PASSED. **M2: EXECUTION** — IN PROGRESS

## State right now

M0 complete. ARCHITECTURE.md, DECISIONS.md, OPEN-PROBLEMS.md, BLOCKERS.md all
written from source that was actually read. Cargo workspace root exists,
toolchain pinned to 1.98.1, dependency graph resolves (`cargo fetch` green).

Key facts a fresh session needs and should not re-derive:
- reth pinned at v2.6.0, commit 73a3a00862a8f14f89e30da8de001456f18cfae0.
  Clone lives at $SCRATCH/reth if still present; re-clone blobless if not.
- We depend on `alloy-evm` 0.39.0 + `revm` 43.0.2 DIRECTLY, not on reth crates.
  Reasoning and proof in DECISIONS.md C-001. Do not "fix" this back to reth.
- Container shipped Rust 1.94.1 which cannot build reth. 1.98.1 installed via
  rustup and pinned in rust-toolchain.toml.
- `ContextTr::set_block` exists, so per-transaction beneficiary works. This is
  load-bearing for merge-set coinbase. DECISIONS.md D-007.
- solc and foundry are NOT installed. BLOCKERS.md B-001. Needed at M2 and M7.

## What has passed

- **M0 GATE PASSED.** ARCHITECTURE.md exists; every reth/alloy/revm
  integration point cites a file path and symbol read from pinned source.
- **M1 GATE PASSED.** `cargo test --workspace` green (28 tests),
  `cargo clippy --all-targets -- -D warnings` clean, `cargo fmt --check`
  clean, and `chainname-node --check` boots, initialises storage, logs the
  resolved consensus parameters, and exits 0.

Crates that exist and what they hold:
- `crates/primitives` — `Header`, `Block`, `BlockHash`, `ChainParams`. All
  chain constants with derivation comments. 14 tests.
- `crates/storage` — `BlockStore` trait + `RedbStore`. Schema versioned;
  mismatch is a hard error, never a silent migration. 5 tests.
- `crates/node` — `NodeConfig`, `Network` presets, `Node::boot`. 9 tests.
- `bin/chainname-node` — CLI with `--check` for the boot gate.

## In flight

M2 execution: embed revm, execute a hardcoded transaction against genesis
state, then deploy and call a real Solidity ERC-20.

## Exact next action

1. Resolve BLOCKERS.md B-001 (solc/foundry absent) — try installing solc via
   the static linux release binary; if the fetch is blocked, fall back to
   checked-in ERC-20 bytecode with the source and solc version recorded.
2. Create `crates/execution`: a `revm::Database` implementation over
   `chainname-storage`, a genesis state loader, and a single-transaction
   execution path using `EthEvmFactory`
   (registry:alloy-evm-0.39.0/src/eth/mod.rs:268).
3. Gate: an ERC-20 deploys and `transfer` moves balances correctly.

## Milestone ledger

| ID  | Name               | State       |
|-----|--------------------|-------------|
| M0  | Architecture       | PASSED      |
| M1  | Skeleton           | PASSED      |
| M2  | Execution          | IN PROGRESS |
| M3  | PoW and blocks     | not started |
| M4  | GHOSTDAG           | not started |
| M5  | Networking         | not started |
| M6  | The seam           | not started |
| M7  | RPC                | not started |
| M8  | Hardening          | not started |
| M9  | Raise the rate     | not started |
| M10 | Parallel execution | not started |

## Protocol reminders

- Read STATUS.md, ARCHITECTURE.md, DECISIONS.md, BLOCKERS.md at session start.
- Never ask questions. Choose cheapest-to-reverse, log OPEN in DECISIONS.md.
- Commit after every passing test.
- `CHAINNAME` is a literal placeholder string. Do not pick a name.
