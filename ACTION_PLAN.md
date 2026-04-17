# Action Plan — Fase 1: Plug & Play

Objetivo: pool de minería lottery que los Bitaxe/NerdAxe puedan usar a través
del translator oficial de sv2-apps. La pool habla SV2 internamente; el translator
convierte SV1↔SV2 sin que nosotros tengamos que implementarlo.

## Arquitectura objetivo

```
Bitaxe/NerdAxe  (SV1)
       │  SV1
       ▼
  translator             ← binario de sv2-apps, sin modificar
       │  SV2 Mining Protocol + Noise
       ▼
  nuestra pool           ← única pieza que escribimos
       │  SV2 Template Distribution Protocol
       ▼
  bitcoin-core-sv2       ← binario de sv2-apps, sin modificar
       │
       ▼
  Bitcoin Core (regtest / mainnet)
```

---

## Paso 1 — Entorno y estructura del proyecto
- [x] `flake.nix` con Rust 1.90 (fenix) + bitcoind
- [x] `cargo init` con workspace
- [x] `bitcoin.conf` para regtest local
- [x] Levantar bitcoind en regtest y verificar RPC

## Paso 2 — Cliente RPC de Bitcoin Core
- [x] Conectar al nodo via JSON-RPC (usuario/contraseña desde config)
- [x] Implementar llamada a `getblocktemplate`
- [x] Parsear la respuesta: transacciones, prevhash, bits, height, coinbasevalue
- [x] Poll de nuevos bloques (detectar cuando llega bloque nuevo → `clearJobs`)

> `TemplatePoller` ya fue eliminado en Paso 4b. `RpcClient` se mantiene para tests
> de integración (generatetoaddress, submitblock, etc.) y para el futuro Paso 5.

## Paso 3 — Constructor de jobs
- [x] Construir la transacción coinbase con la dirección del miner
- [x] Calcular el witness commitment (SegWit)
- [x] Construir el merkle tree a partir de las transacciones
- [x] Generar extranonce1 único por conexión y extranonce2 de tamaño fijo
- [x] Serializar coinbase parts (coinb1/coinb2) y merkle branch

> `jobs.rs` es agnóstico del protocolo de red y se reutiliza sin cambios en SV2.
> Solo se añadirá `build_sv2_job` que produce los tipos de `mining_sv2` en lugar
> de los de `sv1_api`.

## Paso 4a — Retirar el servidor SV1 directo
El servidor SV1 que escribimos en `stratum.rs` lo reemplaza el translator de sv2-apps.
- [x] Borrar `pool/src/stratum.rs`
- [x] Retirar `sv1_api` de `Cargo.toml`

## Paso 4b — Cliente de Template Distribution Protocol
Reemplaza el RPC poller (`rpc::TemplatePoller`) con una conexión real al Template Provider.
Crates ya en `Cargo.toml`: `template_distribution_sv2 = "5.0.0"`. Añadir: `stratum-apps = "0.3.0"` (feature `network_helpers`) para `NoiseTcpStream` / `accept_noise_connection`.
- [x] New module `pool/src/template_client.rs`
- [x] TCP client that connects to sv2-tp (configurable address, default port 18447 on regtest)
- [x] Noise **initiator** handshake toward the template provider (reads authority pubkey from `sv2_authority_key` file in bitcoind datadir)
- [x] Send `SetupConnection` + `CoinbaseOutputConstraints`; receive `NewTemplate` + `SetNewPrevHash`
- [x] Broadcast via `tokio::sync::watch<RawTemplate>` channel; replaces `TemplatePoller`
- [x] Remove `TemplatePoller` and dead SV1 code (`StratumJob`, `build_stratum_job`) from codebase
- [x] Add `sv2-tp` binary to `flake.nix`; `just start-all` / `just stop-all` recipes
- [x] Write `bitcoin/sv2-tp.conf`; integration test `tests/template_client.rs`
- [x] Fix `build_sv2_coinbase_from_tdp` for segwit: add `use_segwit: bool` param (true when `coinbase_tx_outputs_count > 0`); insert marker/flag bytes in prefix; append 32-byte witness nonce before locktime in suffix
- [x] Write `pool/tests/mine_block.rs`: receive `RawTemplate` → build segwit coinbase → mine nonce → send `SubmitSolution` to sv2-tp → sv2-tp reconstructs block and calls `submitblock` → assert height increased
- [x] Expose `mpsc::Sender<SubmitSolutionData>` from `template_client::start`; background `io_loop` owns `NoiseWriteHalf` and sends `SubmitSolution` on demand

## Paso 4c — Servidor SV2 Mining Protocol
La pool habla SV2 Extended Channel con el translator. Los Bitaxes se conectan al translator.
Crates ya en `Cargo.toml`: `mining_sv2 = "8.0.0"`, `noise_sv2 = "1.4.2"`, `codec_sv2 = "5.0.0"`, `common_messages_sv2 = "7.0.0"`.
Key design: use **extended channels** (not standard) — the official translator opens extended channels toward the pool.
- [x] New module `pool/src/stratum_sv2.rs`
- [x] Add `POOL_AUTHORITY_PUBLIC_KEY` and `POOL_AUTHORITY_PRIVATE_KEY` (32-byte hex) to `config.rs` / env vars; `just keygen` genera el par y `just run` carga `.env`
- [x] TCP listener; per-connection task with Noise **responder** handshake
- [x] Handle `SetupConnection` (protocol = Mining, flags = `REQUIRES_EXTENDED_CHANNELS`) → respond `SetupConnectionSuccess`
- [x] Handle `OpenExtendedMiningChannel` → assign `channel_id`, extranonce size; respond `OpenExtendedMiningChannelSuccess`
- [x] Extract miner BTC address from `user_identity` field; fall back to pool address if invalid
- [x] On open channel and on every template change: send `SetNewPrevHash` + `NewExtendedMiningJob` (per-channel coinbase with miner address)
- [ ] Spawn reader/writer io-task pair per connection (actualmente una sola tarea por conexión)
- [ ] Inactive channel timeout (configurable, default 10 min)

## Paso 4d — Integrar y configurar el translator
- [ ] Añadir el binario `translator` de sv2-apps al `flake.nix`
- [ ] Escribir `translator.toml`: upstream apunta a nuestro puerto SV2, downstream expone puerto SV1
- [ ] Añadir receta `just translator` para arrancarlo, tambien añadirlo al test de integracion 
- [ ] Verificar handshake SV1 completo (subscribe + authorize + notify) a través del translator

## Paso 5 — Validación de shares
Using extended channels, miners (via translator) send `SubmitSharesExtended`.
- [ ] Recibir `SubmitSharesExtended` (channel_id, sequence_number, job_id, nonce, ntime, version, extranonce)
- [ ] Look up the job by channel_id + job_id; reconstruct coinbase using channel's coinbase_prefix + extranonce + coinbase_suffix
- [ ] Compute coinbase hash → merkle root (applying stored merkle_path) → block header hash
- [ ] Verify hash meets share difficulty for the channel
- [ ] Si cumple dificultad de red → serializar bloque completo y enviarlo a Core vía `submitblock` (RPC); enviar `SubmitSolution` al template provider
- [ ] Responder `SubmitSharesSuccess` / `SubmitSharesError` con el sequence_number correcto

## Paso 6 — Persistencia básica
- [ ] SQLite con sqlx
- [ ] Tabla `miners` (dirección BTC, primera conexión)
- [ ] Tabla `shares` (miner, dificultad, timestamp, epoch)
- [ ] Tabla `epoch_best` (mejor share por miner por epoch)

## Paso 7 — Integración end-to-end
- [ ] Stack completo en local: bitcoind + bitcoin-core-sv2 + nuestra pool + translator
- [ ] Conectar Bitaxe real (o simulado) al translator
- [ ] Verificar que recibe jobs y envía shares válidos a través del stack completo
- [ ] Verificar que un bloque encontrado en regtest se propaga a Core
