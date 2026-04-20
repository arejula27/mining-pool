# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Environment

All development happens inside the Nix dev shell (`nix develop`). The shell provides Rust 1.90, `bitcoind`, `just`, and defines a `bcli` helper function that wraps `bitcoin-cli` with the correct `-datadir` flag pointing to the local `.bitcoin-data/` directory.

Do not use `cargo` directly. Use the `just` recipes below instead.

## Commands

### Build and test

| Command | What it does |
|---|---|
| `just check` | `cargo check` including test targets |
| `just unit` | Run unit tests from `src/` only (`--lib`, no node required) |
| `just clean` | Remove build artifacts |
| `just int` | Start node, run all integration test suites sequentially, stop |
| `just int-rpc` | Start bitcoin-node only, run `tests/rpc.rs`, stop |
| `just int-mine` | Start node, mine 1 block, run `tests/mine_block.rs`, stop |
| `just int-sv1` | Reset chain, start node, run `tests/sv1_miner.rs`, stop |

### Pool

| Command | What it does |
|---|---|
| `just keygen` | Generate a fresh SV2 authority keypair and append it to `.env` |
| `just run` | Run the pool binary (sources `.env` if it exists) |

### Bitcoin node

| Command | What it does |
|---|---|
| `just start` | Start bitcoin-node in regtest (background daemon) |
| `just stop` | Stop bitcoin-node via RPC |
| `just kill` | Force-kill bitcoin-node (when RPC is unavailable) |
| `just mine [n]` | Mine `n` blocks to a throwaway address |
| `just node-check` | Verify bitcoin-node RPC is responding |
| `just cli <args>` | Run `bitcoin-cli` against the local regtest node |
| `just reset-chain` | Wipe regtest data (run `stop` first) |

### Translator (SV1 → SV2)

| Command | What it does |
|---|---|
| `just start-translator` | Start translator_sv2 (requires pool running on :3333) |
| `just stop-translator` | Stop the translator |

To run a single integration test manually:
```
just start
just mine 1
cargo test --manifest-path pool/Cargo.toml --test <suite> <test_name> -- --nocapture
just stop
```

### What `just` manages vs what tests spawn

`just start` starts the bitcoin-node. Integration tests that need the pool server (`sv2_server.rs`, `sv1_miner.rs`) spawn it in-process via `tokio::spawn`. `sv1_miner.rs` also spawns `translator_sv2` as a subprocess because it generates an ephemeral keypair per run and must configure the translator with that key; using `just start-translator` would break test isolation.

**Important:** Bitcoin Core v30.2 shuts down when any IPC client disconnects (upstream bug, not yet fixed). `just int` therefore restarts the node before each test suite that uses IPC.

## Target architecture

```
Bitaxe/NerdAxe (SV1)
       │  SV1
       ▼
  translator             ← sv2-apps binary, unmodified
       │  SV2 Mining Protocol + Noise
       ▼
  our pool               ← what we write
       │  Cap'n Proto IPC (UNIX socket)
       ▼
  Bitcoin Core (regtest / mainnet)
```

SV1 miners connect through the official `translator` binary from sv2-apps. We only implement the SV2 pool side. We do NOT implement a direct SV1 server.

## Codebase structure

Single Rust crate (`pool/`) with both a library target (`src/lib.rs`) and a binary target (`src/main.rs`). The library exposes all logic; the binary is a thin entry point. Integration tests live in `pool/tests/`.

Bitcoin node config is in `bitcoin/bitcoin.conf` (tracked in git, regtest). Blockchain data goes to `.bitcoin-data/` (gitignored). `just start` copies the config into the data dir before launching bitcoind so the node never reads `~/.bitcoin`.

### Modules

- **`config`** — reads env vars: `SV2_LISTEN_ADDR` (default `0.0.0.0:3333`), `POOL_ADDRESS`, `POOL_AUTHORITY_PUBLIC_KEY`, `POOL_AUTHORITY_PRIVATE_KEY`, `BITCOIN_IPC_SOCKET` (default `.bitcoin-data/regtest/node.sock`), `RPC_URL`, `RPC_USER`, `RPC_PASS`.

- **`jobs`** — Protocol-agnostic coinbase and merkle construction. Each miner gets their own coinbase output pointing to their address (lottery model — if they find a block, they keep 100%). `build_sv2_coinbase_from_tdp` builds the segwit coinbase; `build_merkle_branch` computes sibling hashes. `pool/tests/fixtures/block_250000.json` is a real-block fixture for unit tests.

- **`node_ipc`** — Connects directly to Bitcoin Core via Cap'n Proto over a UNIX socket (bypasses sv2-tp entirely). Bootstraps `InitIpcClient` → `MiningIpcClient` → `BlockTemplateIpcClient`, runs a `waitNext` polling loop, and broadcasts templates over a `tokio::sync::watch` channel. Accepts `SubmitSolution` via an `mpsc::Sender`. Runs in a dedicated `std::thread` + single-threaded `tokio` runtime because `capnp-rpc` is `!Send`.

- **`stratum_sv2`** — SV2 Mining Protocol server. TCP listener with Noise NX responder handshake per connection. Handles `SetupConnection`, `OpenExtendedMiningChannel`, and `SubmitShares`. Sends `NewExtendedMiningJob` and `SetNewPrevHash` to connected channels when a new template arrives. Accepts `Option<std::sync::mpsc::Sender<DbEvent>>` to emit share and miner-connected events to the DB worker without blocking the ACK path.

- **`db`** — SQLite persistence (via `rusqlite`). `DbWorker::start(path)` opens the database and spawns a background `std::thread` that batches `DbEvent`s and flushes every 60 s in a single transaction. Tables: `miners`, `shares`, `epoch_stats` (best share hash + active_minutes per miner), `minute_hashrates` (difficulty sum per address per 1-min window, pruned after 24 h), `competition_entries`. The hot path never awaits the DB — it calls `Sender::send()` (non-blocking) after the ACK is already on the wire. `DbReader` provides a WAL-mode read-only connection with `hashrate_for_address(address, lookback_minutes)` and `pool_hashrate(lookback_minutes)` using `hashrate = Σ(difficulty) × 2^32 / window_seconds`.

- **`noise_connection`** — Thin helpers around `noise_sv2` and `codec_sv2`: `connect_noise` completes either initiator or responder handshake and returns typed read/write halves.

- **`rpc`** — `RpcClient` (JSON-RPC over HTTP via `reqwest`/`rustls`). Used by integration tests to call `generatetoaddress`, `getblockcount`, etc. against the local regtest node.
