# BLOCKERS

Things that stopped work, with full reproduction detail. Empty is good.

Format: ID, milestone, symptom, reproduction, three attempts made, current state.

---

## B-001 — `solc` and `foundry` are not installed — OPEN, not yet blocking

**Milestone:** M2 gate (ERC-20 deploy) and M7 gate (`forge script`).

**Symptom:** `solc`, `forge`, `cast`, `foundryup` are all absent from the
container. `jq`, `cmake`, `clang`, `gcc` are present.

**Reproduction:** `command -v solc forge cast foundryup` → all MISSING.

**Impact:** The M2 gate requires "an ERC-20 compiled by solc". The M7 gate
requires `forge script` to actually run.

**Plan (not yet attempted, recorded so it is not forgotten):**
1. Install `solc` via the static linux binary from the solidity releases, or
   via `svm-rs`.
2. Install Foundry via `foundryup`.
3. If outbound fetch of either is blocked, fall back for M2 to checked-in
   ERC-20 *bytecode* compiled elsewhere, with the source and the exact solc
   version recorded next to it, and keep the gate honest by asserting real
   `transfer` semantics rather than just a successful deploy. M7's `forge`
   requirement has no such fallback and would become a genuine blocker.

**State (updated at M2):** PARTIALLY RESOLVED.

- `solc` 0.8.30 was fetched successfully from the solidity releases page and
  lives at `$SCRATCH/tools/solc`. The M2 gate ran against it: an ERC-20
  compiled by solc deploys and `transfer` moves balances
  (`crates/execution/tests/evm_execution.rs::erc20_deploys_and_transfers`).
- The compiled bytecode is checked in at `crates/execution/testdata/Erc20.bin`
  so CI needs no solc to run the gate. A separate test,
  `solc_bytecode_is_current`, recompiles and compares whenever solc is
  available (`CHAINNAME_SOLC` env var or `solc` on PATH), so the artifact
  cannot silently drift from `Erc20.sol`. CI installs solc for exactly this.
- **Foundry is still absent.** The M7 gate requires `forge script` to really
  run. Not yet attempted. If `foundryup` cannot reach its release host from
  this environment, M7 becomes a genuine blocker and will be recorded here as
  a new entry rather than quietly downgraded.
