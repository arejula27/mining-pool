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
| `just int` | Start full environment, run all integration tests (`tests/`), stop |
| `just int-rpc` | Start bitcoin-node only, run `tests/rpc.rs`, stop |
| `just int-tdp` | Start full environment, run `tests/template_client.rs`, stop |
| `just int-mine` | Start full environment, run `tests/mine_block.rs`, stop |
| `just int-sv1` | Start full environment, run `tests/sv1_miner.rs`, stop |

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

### sv2-tp (Template Provider)

| Command | What it does |
|---|---|
| `just start-sv2-tp` | Start sv2-tp in the background (requires bitcoin-node) |
| `just stop-sv2-tp` | Stop sv2-tp |

### Translator (SV1 → SV2)

| Command | What it does |
|---|---|
| `just start-translator` | Start translator_sv2 (requires pool running on :3333) |
| `just stop-translator` | Stop the translator |

### Combined

| Command | What it does |
|---|---|
| `just start-all` | Start bitcoin-node + sv2-tp |
| `just stop-all` | Stop sv2-tp + bitcoin-node |
| `just kill-all` | Force-kill translator, sv2-tp, and bitcoin-node |

To run a single integration test manually:
```
just start-all
cargo test --manifest-path pool/Cargo.toml --test <suite> <test_name> -- --nocapture
just stop-all
```

### What just manages vs what tests spawn

`just start-all` starts the persistent shared infrastructure (bitcoin-node, sv2-tp).
Integration tests that need the pool server (`sv2_server.rs`, `sv1_miner.rs`) spawn it
in-process via `tokio::spawn`. `sv1_miner.rs` also spawns `translator_sv2` as a subprocess
because it generates an ephemeral keypair per run and must configure the translator with
that same key; using `just start-translator` (which reads `.env`) would break test isolation.

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

- **`config`** — reads `STRATUM_PORT`, `POOL_ADDRESS`, `POOL_AUTHORITY_PUBLIC_KEY`, `POOL_AUTHORITY_PRIVATE_KEY`, `TP_ADDRESS` from environment variables (sourced from `.env` via `just run`).

- **`jobs`** — Protocol-agnostic coinbase and merkle construction. `build_sv2_coinbase_from_tdp` builds the segwit coinbase from TDP data; `build_merkle_branch` computes sibling hashes. `pool/tests/fixtures/block_250000.json` is a real-block fixture for unit tests.

- **`template_client`** — SV2 Template Distribution Protocol client. Connects to sv2-tp (default `127.0.0.1:18447`), completes the Noise initiator handshake, sends `SetupConnection` + `CoinbaseOutputConstraints`, and receives `NewTemplate` + `SetNewPrevHash`. Broadcasts templates over a `tokio::sync::watch` channel and accepts `SubmitSolution` via an `mpsc::Sender`.

- **`stratum_sv2`** — SV2 Mining Protocol server. TCP listener with Noise NX responder handshake per connection. Handles `SetupConnection`, `OpenExtendedMiningChannel`, and `SubmitShares`. Sends `NewExtendedMiningJob` and `SetNewPrevHash` to connected channels when a new template arrives.

- **`noise_connection`** — Thin helpers around `noise_sv2` and `codec_sv2`: `connect_noise` completes either initiator or responder handshake and returns typed read/write halves.

- **`rpc`** — `RpcClient` (JSON-RPC over HTTP via `reqwest`/`rustls`). Used by integration tests to call `generatetoaddress`, `getblockcount`, etc. against the local regtest node.
