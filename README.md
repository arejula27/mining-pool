# mining-pool
[![CI](https://github.com/arejula27/mining-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/arejula27/mining-pool/actions/workflows/ci.yml)
A lottery-style Bitcoin mining pool implementing the Stratum V2 protocol. Each miner that connects sets their own Bitcoin address as the coinbase recipient: if they find a block, the full subsidy and fees go directly to them. On top of that, participants who pay a monthly fee enter an epoch competition where the miner with the best share difficulty wins the entire pot.

The pool is designed for small home miners using Bitaxe and NerdAxe devices. SV1 devices connect through the official `translator_sv2` proxy from the sv2-apps project; the pool itself only speaks SV2.

## Architecture

```
Bitaxe/NerdAxe (SV1)
       |  SV1
       v
  translator_sv2         <- sv2-apps binary, unmodified
       |  SV2 Mining Protocol + Noise
       v
  pool                   <- this binary
       |  SV2 Template Distribution Protocol
       v
  sv2-tp                 <- sv2-apps binary, unmodified
       |
       v
  Bitcoin Core (regtest / mainnet)
```

## Deployment

All commands must be run inside the Nix dev shell:

```
nix develop
```

### Step 0: generate the pool authority keypair

On first run, generate the Noise keypair that authenticates the pool toward the translator:

```
just keygen
```

This appends `POOL_AUTHORITY_PUBLIC_KEY` and `POOL_AUTHORITY_PRIVATE_KEY` to `.env`.

---

### SV2 miners (native)

Miners that support Stratum V2 natively connect directly to the pool.

**Required binaries** (all provided by the Nix shell):

| Binary | Source | Role |
|---|---|---|
| `bitcoin-node` | Bitcoin Core with SV2 patch | Full node + block validation |
| `sv2-tp` | sv2-apps | Template Distribution Protocol provider |
| `pool` | this repository | SV2 Mining Protocol server |

**Start:**

```
just start-all   # starts bitcoin-node and sv2-tp
just run         # starts the pool (port 3333)
```

Miners connect to `<host>:3333` using the SV2 Mining Protocol with Noise encryption. They must be configured with the pool authority public key (`POOL_AUTHORITY_PUBLIC_KEY` from `.env`, encoded as base58).

**Stop:**

```
just stop-all
```

---

### SV1 miners (Bitaxe / NerdAxe via translator)

Devices that only speak Stratum V1 (such as Bitaxe and NerdAxe running ESP-Miner) connect to the `translator_sv2` proxy, which converts SV1 to SV2 and forwards to the pool.

**Required binaries** (all provided by the Nix shell):

| Binary | Source | Role |
|---|---|---|
| `bitcoin-node` | Bitcoin Core with SV2 patch | Full node + block validation |
| `sv2-tp` | sv2-apps | Template Distribution Protocol provider |
| `pool` | this repository | SV2 Mining Protocol server |
| `translator_sv2` | sv2-apps | SV1 to SV2 translation proxy |

**Start:**

```
just start-all        # starts bitcoin-node and sv2-tp
just run &            # starts the pool (port 3333)
just start-translator # starts the translator (port 34255)
```

Miners connect to `<host>:34255` using standard Stratum V1. The username must be a valid Bitcoin address; the pool uses it to build the coinbase output.

**Stop:**

```
just stop-translator
just stop-all
```

---

## Development

```
just check      # compile-check including test targets
just unit       # run unit tests from src/ (no node required)
just int        # start full environment, run all integration tests, stop
just int-rpc    # bitcoin-node only, run tests/rpc.rs
just int-mine   # full environment, run tests/mine_block.rs
just int-sv1    # full environment, run tests/sv1_miner.rs
```
