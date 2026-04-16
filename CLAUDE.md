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

## Target architecture

```
Bitaxe/NerdAxe (SV1)
       │  SV1
       ▼
  translator             ← sv2-apps binary, unmodified
       │  SV2 Mining Protocol + Noise
       ▼
  our pool               ← what we write
       │  SV2 Template Distribution Protocol
       ▼
  bitcoin-core-sv2       ← sv2-apps binary, unmodified
       │
       ▼
  Bitcoin Core (regtest / mainnet)
```

SV1 miners connect through the official `translator` binary from sv2-apps. We only implement the SV2 pool side. We do NOT implement a direct SV1 server.

## Codebase structure

Single Rust crate (`pool/`) with both a library target (`src/lib.rs`) and a binary target (`src/main.rs`). The library exposes all logic; the binary is a thin entry point. Integration tests live in `pool/tests/`.

Bitcoin node config is in `bitcoin/bitcoin.conf` (tracked in git, regtest). Blockchain data goes to `.bitcoin-data/` (gitignored). `just start` copies the config into the data dir before launching bitcoind so the node never reads `~/.bitcoin`.

### Modules (current)

- **`rpc`** — `RpcClient` (JSON-RPC over HTTP via `reqwest`/`rustls`) and `TemplatePoller`. The poller uses Bitcoin Core's long-polling (`longpollid`) to detect new blocks and broadcasts updated `BlockTemplate`s over a `tokio::sync::watch` channel. **Temporary**: will be replaced by the SV2 Template Distribution Protocol client (Paso 4b).

- **`config`** — reads `RPC_URL`, `RPC_USER`, `RPC_PASS`, `STRATUM_PORT`, `POOL_ADDRESS` from environment variables.

- **`jobs`** — Protocol-agnostic coinbase and merkle construction. `build_coinbase_parts` builds the coinbase split at the extranonce placeholder; `build_merkle_branch` computes sibling hashes in internal byte order. `pool/tests/fixtures/block_250000.json` is a real-block fixture for unit tests. Will gain a `build_sv2_job` function alongside the existing SV1 one.

### Planned modules (see ACTION_PLAN.md)

`stratum_sv2` (SV2 Mining Protocol server using `roles_logic_sv2` + `mining_sv2` + `noise_sv2`), `template_client` (SV2 Template Distribution Protocol, replacing `rpc`), `shares` (validation), `db` (SQLite).
