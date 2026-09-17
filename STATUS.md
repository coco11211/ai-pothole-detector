# STATUS

**Read this first. Written for an amnesiac session with zero memory.**

Project: CHAINNAME — a proof-of-work L1 with EVM execution (reth/revm as a
library) and GHOSTDAG blockDAG consensus. No validator set, no stake, no
attestation, no voting. Testnet only.

---

## Current milestone

**M0: ARCHITECTURE** — IN PROGRESS

## State right now

Repo is empty apart from these state files. Nothing built yet.

## What has passed

Nothing yet.

## In flight

- Cloning reth to read actual source and verify the spec's claims.
- Producing ARCHITECTURE.md and OPEN-PROBLEMS.md.

## Exact next action

Clone `paradigmxyz/reth`, pick an exact release tag, and read the real
integration points (block executor, state provider, RPC assembly, storage)
before writing a single line of ARCHITECTURE.md.

## Milestone ledger

| ID  | Name               | State       |
|-----|--------------------|-------------|
| M0  | Architecture       | IN PROGRESS |
| M1  | Skeleton           | not started |
| M2  | Execution          | not started |
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
