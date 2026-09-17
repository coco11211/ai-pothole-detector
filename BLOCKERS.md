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

**State:** deferred until M2. Not blocking M0 or M1.
