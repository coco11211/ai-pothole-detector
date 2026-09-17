# STATUS

**Read this first. Written for an amnesiac session with zero memory.**

Project: CHAINNAME — a proof-of-work L1 with EVM execution (reth/revm as a
library) and GHOSTDAG blockDAG consensus. No validator set, no stake, no
attestation, no voting. Testnet only.

---

## Current milestone

**M2: EXECUTION** — PASSED. **M3: POW AND BLOCKS** — IN PROGRESS

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
- **M2 GATE PASSED.** An ERC-20 compiled by solc 0.8.30 deploys through revm
  and `transfer` moves balances correctly
  (`crates/execution/tests/evm_execution.rs`). Also proven there: base fee is
  burned, priority fee reaches the carrying block's miner, execution is
  deterministic, and the checked-in bytecode matches its Solidity source.
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
- `crates/execution` — `WorldState` (revm `Database` + `DatabaseCommit` +
  `DatabaseRef`, state root via alloy-trie), genesis loading from
  `alloy_genesis::Genesis`, and the EVM driver. 16 tests.

More facts not to re-derive:
- `alloy_evm::Evm` has NO `set_block`. The setter is `ContextSetters::set_block`
  reached via `EthEvm::ctx_mut()`. `chainname_execution::set_beneficiary` wraps
  this; it is how merge-set coinbase attribution works.
- `ExecutionResult::gas_used()` is deprecated in revm 43 after the EIP-8037
  state gas split. Use `tx_gas_used()`.
- `make_evm` returns `ChainEvm<DB>`, an alias for the factory's associated
  type. Spelling `EthEvm<DB, NoOpInspector>` by hand does not match, because
  the precompile parameter differs.
- solc lives at `$SCRATCH/tools/solc` (0.8.30). Pass it as `CHAINNAME_SOLC`.

## In flight

M3 PoW and blocks: block structure, PoW hash, ASERT retargeting, block
validation, single-node mining loop.

## Exact next action

1. `crates/pow`: `PowHash` trait + `KeccakF1600x2` (two-round Keccak-f[1600]
   over the RLP header, truncated to 256 bits). Testnet placeholder — the
   definition site must carry the cryptanalytic-review warning.
2. `crates/difficulty`: compact `bits` <-> target conversion, then ASERT with
   the Bitcoin Cash cubic approximation to 2^x. Integer only; the workspace
   already denies `clippy::float_arithmetic`. Tests BEFORE implementation, per
   the engineering standards.
3. Block validation + a single-node mining loop.
4. Gate: mine 10,000 blocks; difficulty must track a simulated 10x hashrate
   step up and a 90% step down without stalling or oscillating. That
   simulation IS the test.

## Milestone ledger

| ID  | Name               | State       |
|-----|--------------------|-------------|
| M0  | Architecture       | PASSED      |
| M1  | Skeleton           | PASSED      |
| M2  | Execution          | PASSED      |
| M3  | PoW and blocks     | IN PROGRESS |
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
