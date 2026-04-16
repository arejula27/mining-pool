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

> Este módulo se sustituirá en Paso 4b por el Template Distribution Protocol.
> Hasta entonces sirve como fuente de templates durante el desarrollo.

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
- [ ] New module `pool/src/template_client.rs`
- [ ] TCP client that connects to `bitcoin-core-sv2` (configurable address, default port 8442)
- [ ] Noise **initiator** handshake toward the template provider (opposite role to Paso 4c)
- [ ] Send `SetupConnection` (protocol = Template Distribution, flags = 0)
- [ ] Receive `NewTemplate` + `SetNewPrevHash`; broadcast via `tokio::sync::watch` channel (same interface as current `TemplatePoller`)
- [ ] Send `SubmitSolution` to the template provider when a block is found
- [ ] Add `bitcoin-core-sv2` binary to `flake.nix` and `just` recipe to launch it
- [ ] Write `config/bitcoin-core-sv2.toml` (RPC connection to bitcoind, listen port 8442)

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
- [ ] Añadir receta `just translator` para arrancarlo
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
