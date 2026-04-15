# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Environment

All development happens inside the Nix dev shell (`nix develop`). The shell provides Rust 1.90, `bitcoind`, `just`, and defines a `bcli` helper function that wraps `bitcoin-cli` with the correct `-datadir` flag pointing to the local `.bitcoin-data/` directory.

Do not use `cargo` directly. Use the `just` recipes below instead.

## Commands

| Command | What it does |
|---|---|
| `just check` | `cargo check` including test targets |
| `just unit` | Run unit tests (no bitcoind required) |
| `just clean` | Remove build artifacts |
| `just int` | Start regtest, run all integration tests, stop regtest |
| `just int-rpc` | Same but only the `tests/rpc.rs` suite |
| `just start` | Start bitcoind in regtest (background daemon) |
| `just stop` | Stop bitcoind via RPC |
| `just kill` | Force-kill bitcoind (when RPC is unavailable) |
| `just mine [n]` | Mine `n` blocks to a throwaway address |
| `just node-check` | Verify bitcoind RPC is responding |
| `bcli <args>` | Run `bitcoin-cli` against the local regtest node |

To run a single integration test:
```
just start
cargo test --manifest-path pool/Cargo.toml --test rpc <test_name> -- --nocapture
just stop
```

## Architecture

Single Rust crate (`pool/`) with both a library target (`src/lib.rs`) and a binary target (`src/main.rs`). The library exposes all logic; the binary is a thin entry point. Integration tests live in `pool/tests/`.

Bitcoin node config is in `bitcoin/bitcoin.conf` (tracked in git, regtest). Blockchain data goes to `.bitcoin-data/` (gitignored). `just start` copies the config into the data dir before launching bitcoind so the node never reads `~/.bitcoin`.

### Modules (current)

- **`rpc`** — `RpcClient` (JSON-RPC over HTTP via `reqwest`/`rustls`) and `TemplatePoller`. The poller uses Bitcoin Core's long-polling (`longpollid`) to detect new blocks and broadcasts updated `BlockTemplate`s over a `tokio::sync::watch` channel. Consumers call `TemplatePoller::subscribe()` to get a receiver.

- **`config`** — reads `RPC_URL`, `RPC_USER`, `RPC_PASS`, `STRATUM_PORT`, `POOL_ADDRESS` from environment variables.

- **`jobs`** — Stratum V1 job construction. `build_stratum_job` builds a `StratumJob` from a `BlockTemplate` and a miner address. Internally builds `CoinbaseParts` (coinb1/coinb2 split around extranonce placeholder) and `build_merkle_branch` (sibling hashes in internal byte order). `pool/tests/fixtures/block_250000.json` is a real-block fixture used in unit tests to verify the merkle computation.

### Planned modules (see ACTION_PLAN.md)

`stratum` (TCP Stratum V1 server), `shares` (validation), `db` (SQLite).
