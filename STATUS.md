# STATUS

**Read this first. Written for an amnesiac session with zero memory.**

Project: CHAINNAME — a proof-of-work L1 with EVM execution (reth/revm as a
library) and GHOSTDAG blockDAG consensus. No validator set, no stake, no
attestation, no voting. Testnet only.

---

## Current milestone

**M3: POW AND BLOCKS** — PASSED. **M4: GHOSTDAG** — IN PROGRESS

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
- **M3 GATE PASSED.** `crates/difficulty/tests/retarget_simulation.rs`
  simulates 150,000 blocks through a 10x hashrate step up and a 90% step down:
  block time returns to within 5% of the 1s target in every regime, difficulty
  reaches ~10x and returns to ~1x, the worst block took 9.7s (no stall), and
  settled block times span under 10% of their mean (no oscillation). The
  literal 10,000-block horizon is tested separately — see DECISIONS.md C-006
  for why 10,000 cannot show convergence.
  `crates/consensus/tests/single_node_chain.rs` mines and validates a real
  200-block chain with real proof of work in 2.8s.
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
- `crates/pow` — `PowHash` trait + `DoubleKeccak256`. 7 tests.
- `crates/difficulty` — compact `bits` codec + ASERT. 25 unit + 3 simulation.
- `crates/consensus` — genesis, header validation, miner. 13 tests.

More facts not to re-derive:
- **ASERT must compute its intermediate in U512.** `anchor * factor` needs up
  to 256+17 bits and the shift adds 16 more. A saturating U256 multiply does
  NOT contain this: the later `>> 16` pulls the saturated value back under the
  pow limit so the clamp never fires and a silently wrong target ships. See
  DECISIONS.md C-007. Three regression tests guard it. Do not "simplify" this
  back to U256.
- Consensus arithmetic tests must use inputs at the extremes of the type. The
  original ASERT tests used `U256::MAX >> 40`, which does not overflow, and
  missed the bug entirely.
- `CompactTarget::to_target` must reject encodings whose shift annihilates the
  mantissa (e.g. `0x01000001` -> 0). A zero target is unsatisfiable, so it is
  an error, not an `Ok(0)`. Found by a property test.
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

M4 GHOSTDAG: DAG storage, blue set, blue score, selected parent chain, merge
set ordering, and the deterministic intra-merge-set sort.

## Exact next action

1. `crates/ghostdag`: `DagStore` holding headers plus per-block GHOSTDAG data
   (selected parent, blue set, blue score, blue work, mergeset blues/reds).
2. Implement the GHOSTDAG ordering algorithm from the PHANTOM paper:
   selected parent = max blue work among parents; k-cluster check to colour
   the mergeset blue or red; blue score = parent's blue score + blue mergeset
   size.
3. Implement the merge-set base sequence and the layering sort exactly as
   ARCHITECTURE.md §6.1 and §6.3 specify.
4. Gate: property tests for P1-P4 in ARCHITECTURE.md §6.4, plus adversarial
   DAGs asserting ordering is identical across independently-built instances.

## Milestone ledger

| ID  | Name               | State       |
|-----|--------------------|-------------|
| M0  | Architecture       | PASSED      |
| M1  | Skeleton           | PASSED      |
| M2  | Execution          | PASSED      |
| M3  | PoW and blocks     | PASSED      |
| M4  | GHOSTDAG           | IN PROGRESS |
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
