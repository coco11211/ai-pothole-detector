# CHAINNAME

A proof-of-work L1 with EVM execution and GHOSTDAG blockDAG consensus.

No validator set, no stake, no attestation, no voting. Pure Nakamoto
settlement. **Testnet only** — see [OPEN-PROBLEMS.md](OPEN-PROBLEMS.md) before
considering anything else.

`CHAINNAME` is a literal placeholder. It has not been named.

---

## What it is

| | |
|---|---|
| **Execution** | `revm`, EVM spec `OSAKA`, chain id `7717`. Existing Solidity deploys unchanged; Foundry and standard JSON-RPC tooling work with no patches. |
| **Consensus** | GHOSTDAG from the PHANTOM paper. Blocks that lose the fork-choice race still contribute their transactions instead of being discarded. |
| **Proof of work** | Double Keccak-256 over the header. A **testnet placeholder** pending cryptanalytic review — OPEN-PROBLEMS.md P-006. |
| **Difficulty** | Absolute ASERT, integer only. No floating point anywhere in consensus. |
| **Block rate** | 1 block/second, with a measured 10 block/second configuration. |
| **Finality** | None, by design. RPC reports blue-score depth so callers choose their own confidence. |

## Try it

```sh
cargo run --release --bin chainname-node -- --dev
```

Starts a single-node chain with JSON-RPC on `127.0.0.1:8545`, mining once a
second, with the well-known development accounts pre-funded. Those keys are
public; the node says so at startup.

```sh
cast chain-id --rpc-url http://127.0.0.1:8545
forge create --rpc-url http://127.0.0.1:8545 --private-key <dev key> \
  --broadcast src/Erc20.sol:Erc20 --constructor-args 1000000000000000000000000
```

## Reading the repo

Start here, in this order:

| File | What it is |
|---|---|
| **[STATUS.md](STATUS.md)** | Where the work is, written for a reader with no context. The most important file here. |
| [ARCHITECTURE.md](ARCHITECTURE.md) | The design, and every integration point with a file path and symbol. |
| [DECISIONS.md](DECISIONS.md) | Every architectural choice with its reasoning. Append-only. Includes CORRECTIONS, where the original brief turned out to be wrong against the source. |
| [OPEN-PROBLEMS.md](OPEN-PROBLEMS.md) | What is known to be unsolved. Read this before trusting anything. |
| [BLOCKERS.md](BLOCKERS.md) | What stopped work, with reproduction detail. |

## Crates

```
crates/
  primitives/   Header, Block, chain parameters
  pow/          PowHash trait + DoubleKeccak256
  difficulty/   compact target codec + ASERT
  ghostdag/     DAG store, blue set, ordering rule, k derivation
  consensus/    genesis, header validation, miner
  execution/    revm over the CHAINNAME world state
  chain/        THE SEAM: merge-set ordering feeding revm, reorgs, undo journal
  pool/         mempool with fee-rate eviction
  net/          wire protocol, peer scoring, sync state machine, TCP transport
  rpc/          eth_* and chainname_* namespaces
  node/         config, dev chain, wiring
  storage/      redb-backed block store
  testkit/      deterministic multi-node simulation
fuzz/           cargo-fuzz targets (excluded from the workspace)
```

## The two ideas worth knowing

**The sync layer is a pure state machine.** `DagSync` takes a message and
returns actions. No I/O, no clock, no sockets. That is what lets ten nodes run
twenty-four simulated hours deterministically from a seed, and it is why the
bugs that surfaced there were reproducible instead of intermittent.

**The ordering rule is a pure function of the DAG.** A chain block's
transactions are its merge set in topological order, deduplicated, then layered
so that transactions with disjoint declared access sets share a round. Because
a transaction's sender is always in its own access set, transactions from one
sender always conflict — and therefore never reorder, which is what keeps
nonces monotonic. That falls out of the rule rather than being bolted on.

## Testing

```sh
cargo test --workspace                    # ~270 tests
cargo test --workspace -- --ignored       # the long soaks, several minutes each
cargo +nightly fuzz run message_decode    # five fuzz targets in fuzz/
```

The multi-node harness is infrastructure, not a helper. Every run is a pure
function of its seed: same seed, same event order, same DAGs, same divergence
if there is one.

## Licence

MIT OR Apache-2.0.
