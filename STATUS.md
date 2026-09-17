# STATUS

**Read this first. Written for an amnesiac session with zero memory.**

Project: CHAINNAME — a proof-of-work L1 with EVM execution (reth/revm as a
library) and GHOSTDAG blockDAG consensus. No validator set, no stake, no
attestation, no voting. Testnet only.

---

## Current milestone

**ALL TEN MILESTONES BUILT.** M9 and M10 both passed with caveats recorded
below and in OPEN-PROBLEMS.md. 301 tests green; clippy and fmt clean.

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
- **M10 GATE: correctness PASSED, speedup measured and NEGATIVE.**
  11 tests compare parallel against sequential on the same input and require
  identical state roots, heights, per-block gas and receipt counts — across
  independent transfers, shared senders, hot recipients, a shared beneficiary,
  a transaction paying the miner directly, real ERC-20 deployment and calls,
  an unexecutable transaction, and 20 blocks across 4 miners. All identical.
  Speedup is 0.56x on value transfers and 0.74x on ERC-20 calls: parallel is
  *slower*. Diagnostics rule out the easy explanations (zero fallbacks, 32
  transactions per round across 4 threads). Per-transaction speculation
  overhead is simply comparable to executing a ~4us transaction. Left off by
  default. OPEN-PROBLEMS.md P-018 has the numbers and what would change them.
- **M9: `k` and the gas limit resolved with evidence; gates re-run at 10 bps.**
  `k = 151` at 10 bps, derived from a measured 99th-percentile propagation
  bound of ~6.2s via the PHANTOM formula. The same method at 1 bps yields
  18 — Kaspa's published value, reached independently, which is the best
  available evidence the 10 bps figure is trustworthy. P-002 resolved by
  separating the block gas *ceiling* from the 1559 *target*. M5, M6 and M8
  gates all re-run and pass at 10 bps, including under 25% loss and
  800-2500ms latency. The 10 bps soak ran 6 simulated hours across 10 nodes:
  196,342 blocks, chain height 37,326, 252,007 reorgs (deepest 20), zero
  divergence at every hourly checkpoint, 21 minutes wall time. The full 24
  hours does NOT fit in this container — ~46 GiB for ten nodes — so it is
  scoped, with the arithmetic recorded in OPEN-PROBLEMS.md P-017.
- **M8 GATE PASSED.** 24 simulated hours, 10 nodes, checked at twelve
  checkpoints: 78,443 blocks mined, chain height 42,947, 79,267 reorgs
  (deepest 13), and at every checkpoint identical DAGs, identical chains,
  identical state roots at every height, zero orphans, zero outstanding
  requests, and no panics. Runs in under eight minutes.
  Five cargo-fuzz targets (message decode, header RLP, compact target,
  transaction decode, GHOSTDAG construction) ran ~70 million iterations
  without a crash. Nine resource-exhaustion tests bound orphan floods,
  oversized inventories, batches and bodies, and repeated reconnects.
  Also closed here: P-012 (real TCP transport) and the three quadratics that
  made a long soak infeasible.
- **M7 GATE PASSED**, with one half PARTIAL.
  `forge script --broadcast` really ran against a live node and printed
  "ONCHAIN EXECUTION COMPLETE & SUCCESSFUL", deploying an ERC-20 and
  transferring tokens, both verified afterwards with `cast call`. `forge
  create` and `cast send` also work end to end.
  MetaMask's exact RPC sequence is verified call for call in
  `crates/node/tests/rpc_compatibility.rs`, but the extension itself was not
  driven in a browser. Recorded honestly as BLOCKERS.md B-002 PARTIAL, not as
  a pass.
- **M6 GATE PASSED.** Five nodes, one simulated hour, checked every five
  minutes: identical DAGs, identical selected parent chains, and identical
  state roots at every chain height. Reorgs genuinely occurred (asserted, not
  assumed) and the undo journal was exercised. Also green under 25% packet
  loss, under 900-3000ms latency, and across eight seeds. Execution is
  reproducible from the seed, and a separate test asserts the state actually
  changed so agreement is not vacuous.
- **M5 GATE PASSED.** Five nodes converge on identical DAG state from a cold
  start and stay converged across thirty simulated minutes, checked every
  minute, under 2% packet loss and 50-250ms latency. Also green: 25% packet
  loss, 800-2500ms latency, nine nodes, and ten different seeds. The run is
  reproducible from its seed (asserted), and different seeds produce different
  runs (also asserted, so the first claim is not vacuous).
  Transport is simulated, not TCP — DECISIONS.md C-008, OPEN-PROBLEMS.md P-012.
- **M4 GATE PASSED.** GHOSTDAG colouring, blue score, blue work, selected
  parent chain, merge set, and the layering sort. 34 tests including five
  property tests for ARCHITECTURE.md §6.4's P1-P4 and the round-advance
  invariant, plus adversarial DAGs: a withheld branch released late, a merge
  set ten wide against k=3, and k=0. `identical_dags_built_in_different_orders_agree_exactly`
  builds the same eight-block DAG in four insertion orders and asserts every
  block's GHOSTDAG data, the chain tip, and the whole selected parent chain
  match exactly.
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
- `crates/ghostdag` — `work` (PoW accumulation), `dag` (`DagStore`,
  reachability, colouring), `ordering` (base sequence + layering sort).
  34 tests.
- `crates/net` — wire protocol (`message`), peer scoring with no stake
  weighting (`peer`), and `DagSync`, a pure state machine. 34 tests.
- `crates/pool` — mempool with fee-rate eviction, per-sender nonce ordering,
  replacement pricing. 16 tests.
- `crates/rpc` — `Backend` (one lock over DAG + executor + pool + indexes),
  the `eth_*` namespace, the `chainname_*` DAG namespace, jsonrpsee server.
- `crates/node/dev` — `DevNode`: a complete single-node chain with mining and
  RPC. `chainname-node --dev` runs it. 12 RPC compatibility tests.
- `crates/chain` — THE SEAM. `reorg` (chain diff), `journal` (undo records),
  `bodies` (transactions + static access sets), `executor` (chain-block
  execution, deferred state root, EIP-1559 base fee). 25 tests.
- `crates/testkit` — seeded LCG, real secp256k1 wallets, the deterministic
  multi-node simulation with execution, propagation measurement, and memory
  scaling measurement.
- `crates/net/transport` + `p2p` — real TCP: length-prefixed framing, accept
  loop, redialling, bounded per-peer queues.
- `crates/chain/parallel` — Block-STM style speculation over rounds.
- `crates/ghostdag/k_parameter` — the PHANTOM formula for deriving `k`.
- `fuzz/` — five cargo-fuzz targets, excluded from the workspace.

More facts not to re-derive:
- `k` is MEASURED: 18 at 1 bps, 151 at 10 bps. The formula is validated by
  reproducing Kaspa's published 18 from its own stated inputs. Never carry a
  `k` across block rates. D-041.
- The block gas CEILING and the 1559 TARGET are different numbers. Conflating
  them made large contract deploys impossible at 10 bps. D-042.
- The DAG is entirely in memory and never shrinks: ~5.9 KiB per block per
  node, so ~4.6 GiB per day at 10 bps. This is the largest unaddressed scaling
  problem. P-017.
- Parallel execution is correct but slower on 4 cores; it is OFF by default
  and should stay off until P-018's numbers change.
- Reachability prunes on TOPOLOGICAL HEIGHT, never blue score. Blue score
  counts blocks while fork choice compares work; they diverge as soon as
  difficulty varies, so a blue-score prune passes every test here and is wrong
  in production. DECISIONS.md D-038.
- Three quadratics were removed at M8 and must not come back: reachability
  scanning the whole past, `tips()` scanning every header, and `compute_reorg`
  walking both chains to genesis. They took the soak from ~12 hours to 8 min.
- An incomplete handshake must be REPAIRED (resend Version), never penalised.
  Penalising it partitions a lossy network permanently. D-040.
- Fuzzing: `cargo +nightly fuzz run <target>`. Targets live in `fuzz/`, which
  is excluded from the workspace.
- Foundry lives at $SCRATCH/tools/{forge,cast,anvil} (1.5.1). solc is there too.
  Pass solc via `--use $SOLC` and add `--offline` so forge does not try to
  fetch a compiler.
- A forge script needs `vm.startBroadcast()` or it broadcasts nothing. The
  cheatcode interface can be declared inline (address
  0x7109709ECfa91a80626fF3989D68f67F5b1DD12D) to avoid needing forge-std.
- EIP-7825 caps a single transaction at 2^24 gas in Osaka, BELOW the 30M block
  limit. `eth_estimateGas` must default `gas` to min(block_limit, cap) or the
  EVM rejects the call before running it.
- `#[tokio::test]` is single-threaded. A blocking socket read in one deadlocks
  against an in-process server. Use
  `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`.
- `DagStore`'s reachability cache is a `Mutex`, not a `RefCell`, because the
  RPC shares the node across threads and a `RefCell` makes it `!Sync`.
- The undo record MUST come from revm's state diff (`Evm::transact`, then note
  every address in the returned diff, then commit). NOT from the static access
  set — the EVM exceeds it via CALL/CREATE/SELFDESTRUCT, and the resulting
  corruption only shows up after a reorg. DECISIONS.md D-029.
- The static access set is for ORDERING ONLY. Never for state.
- Block bodies travel with headers (`BlockPayload`), because a header in the
  DAG is immediately executable. D-030.
- The net layer never decodes transactions; recovery happens above it. D-031.
- An unexecutable transaction is skipped, never fatal. D-032, P-013.
- `DagSync` is a PURE STATE MACHINE. No I/O, no clock (time is a parameter),
  no sockets. Do not "helpfully" give it a tokio runtime. This is what makes
  the simulation deterministic and the bugs reproducible. DECISIONS.md D-024.
- Anything whose ITERATION ORDER reaches the wire must be ordered
  (`BTreeMap`/`BTreeSet`), never hashed. D-025.
- Duplicate blocks are NOT misbehaviour under flood relay. Penalising them
  partitions honest networks. D-026.
- Timed-out requests must rotate across peers. Re-asking the same peer is a
  deadlock, not just slow. D-027.
- Convergence must be asserted on a SETTLED network. `Simulation::quiesce`
  stops mining and waits for no outstanding work plus a quiet propagation
  window. "No traffic in flight" is NOT the criterion — tip reconciliation is
  perpetual chatter by design.
- The merge set EXCLUDES the selected parent. A block with ten parents has a
  merge set of nine. The selected parent is the previous chain block and its
  transactions are already executed. DECISIONS.md D-022.
- `blue_work(B) = blue_work(selected_parent) + sum(work of mergeset_blues
  except the selected parent) + work(B)`. The block's own work IS included.
  Without it, blue work would not grow along a plain chain.
- `work_for_target` must handle `target == U256::MAX` explicitly: `target + 1`
  overflows into a divide-by-zero. Third extreme-value arithmetic bug in this
  codebase; test the ends of the range.
- Red blocks' transactions ARE executed. Colouring affects blue score and fork
  choice, not inclusion. DECISIONS.md D-023.
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

Nothing. All ten milestones are built and their gates have run.

## Exact next action

All ten milestones are built. The highest-value remaining work, in order:

0. **P-019: validate declared difficulty on receipt.** Found while auditing the
   wiring at the end of M10, and it is the most serious item here. Nothing
   checks that a block's `bits` is the value ASERT requires — only that its
   proof of work meets the target *it declares for itself*. A peer can declare
   an easy target, do trivial work, and be accepted. The validation code
   exists and is tested; it is called from no acceptance path, because it
   needs the block's height and that is not known until its parents arrive.
   P-019 sets out where the fix belongs.
1. **P-017: persist and prune the DAG.** It is held entirely in memory and
   never shrinks — ~4.6 GiB per day at 10 bps. This is the largest scaling
   problem in the system and it blocks any long-running node.
2. **P-004: incremental state root.** Currently O(state) per chain block,
   which is only affordable because test states are tiny.
3. **P-009: interval-labelled reachability.** Now pruned by topological
   height, which made it affordable, not free.
4. **B-002: drive MetaMask itself**, or have one person connect it by hand
   once. The wire protocol it speaks is verified; the extension is not.
5. **P-013: the inclusion-spam vector.** A miner earns nothing from a
   transaction that turns out to be unexecutable, but the network still paid
   to relay it. Needs an inclusion fee or mining-time validation.

Read OPEN-PROBLEMS.md before starting any of these; several interact.

## Milestone ledger

| ID  | Name               | State       |
|-----|--------------------|-------------|
| M0  | Architecture       | PASSED      |
| M1  | Skeleton           | PASSED      |
| M2  | Execution          | PASSED      |
| M3  | PoW and blocks     | PASSED      |
| M4  | GHOSTDAG           | PASSED      |
| M5  | Networking         | PASSED      |
| M6  | The seam           | PASSED      |
| M7  | RPC                | PASSED*     |
| M8  | Hardening          | PASSED      |
| M9  | Raise the rate     | PASSED*     |
| M10 | Parallel execution | PASSED*     |

## Protocol reminders

- Read STATUS.md, ARCHITECTURE.md, DECISIONS.md, BLOCKERS.md at session start.
- Never ask questions. Choose cheapest-to-reverse, log OPEN in DECISIONS.md.
- Commit after every passing test.
- `CHAINNAME` is a literal placeholder string. Do not pick a name.
