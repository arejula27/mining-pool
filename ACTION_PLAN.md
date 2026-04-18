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
- [x] Añadir el binario `translator` de sv2-apps al `flake.nix`
- [x] Escribir `translator.toml`: upstream apunta a nuestro puerto SV2, downstream expone puerto SV1
- [x] Añadir receta `just translator` para arrancarlo, tambien añadirlo al test de integracion 
- [x] Verificar handshake SV1 completo (subscribe + authorize + notify) a través del translator
- [x] Write `pool/tests/sv1_miner.rs`: SV1 client → translator → pool → sv2-tp → bitcoin-core e2e test

## Paso 5 — Validación de shares
Using extended channels, miners (via translator) send `SubmitSharesExtended`.
- [x] Recibir `SubmitSharesExtended` (channel_id, sequence_number, job_id, nonce, ntime, version, extranonce)
- [x] Look up the job by channel_id + job_id; reconstruct coinbase using channel's coinbase_prefix + extranonce + coinbase_suffix
- [x] Compute coinbase hash → merkle root (applying stored merkle_path) → block header hash
- [x] Si cumple dificultad de red → enviar `SubmitSolution` al template provider
- [x] Responder `SubmitSharesSuccess` con el sequence_number correcto
- [x] Per-channel difficulty target: computed from `nominal_hash_rate` (~1 share/30 s); sent as U256 in `OpenExtendedMiningChannelSuccess`
- [x] `SubmitSharesError` for stale job_id and shares below channel difficulty

## Paso 5b - comparar solo pool en JS
- [x] analizar https://github.com/warioishere/blitzpool, ver que podemos aprender de ella y aplicarlo

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

---

## Mejoras identificadas (análisis de blitzpool)

Puntos extraídos del análisis del fork JS de public-pool. No son pasos bloqueantes, aplicar cuando
corresponda según la fase en curso.

### Latencia de respuesta a shares
Enviar `SubmitSharesSuccess` antes de cualquier `await` (escrituras a DB, estadísticas, etc.).
blitzpool escribe el ACK al socket sin `await` y lanza el trabajo async de contabilidad después.
En `stratum_sv2.rs`, el `SubmitSharesSuccess` debe salir al wire antes de tocar SQLite (Paso 6).

### TCP_NODELAY explícito
blitzpool llama `socket.setNoDelay(true)` en el momento del accept, antes de detectar protocolo.
Verificar que nuestro `TcpStream` lo activa al aceptar la conexión del translator.

### Detección de shares duplicados por canal
Mantener un `HashSet` por channel con un hash de `(job_id, nonce, ntime, extranonce)`.
O(1), limpiar en cada `clearJobs` para evitar crecimiento ilimitado.
Responder `SubmitSharesError` con código `duplicate-share` si se detecta.

### Expiración de jobs y stale-share
blitzpool retiene jobs 90 s y los limpia en cada nueva emisión de template.
Actualmente no tenemos expiración de jobs en `stratum_sv2.rs`. Shares con `job_id` antiguo
deben responder `SubmitSharesError(stale-share)` en lugar de ser ignorados.

### Omitir notify redundante tras nuevo bloque
blitzpool tiene un flag `skipNext`: cuando llega un nuevo bloque el siguiente tick del intervalo
de refresco se descarta para no enviar un segundo notify inmediato.
En nuestro `watch` channel el último valor ya es idempotente, pero conviene no disparar un
`NewExtendedMiningJob` extra justo después del `SetNewPrevHash` del bloque nuevo.

### Cálculo de dificultad con aritmética entera
Para comparar `share_hash < target` usar aritmética u256 entera (`[u8; 32]` comparación LE),
no conversión a f64. blitzpool usa `big.js` precisamente para evitar pérdida de precisión
al convertir el hash a float de dificultad.

### Target mínimo por canal (Bitaxe)
Si calculamos `nominal_hash_rate` del Bitaxe desde `OpenExtendedMiningChannel`, saturar el
target a un mínimo sensato (p.ej. equivalente a ~1 share/min al hash rate declarado) para no
enviar una dificultad imposible a dispositivos de muy poca tasa hash.

### Vardiff (futuro canal estándar o SV1 directo)
Si en el futuro se abren canales estándar directos sin translator:
- Ventana deslizante: últimas 30 submissions dentro de los últimos 300 s.
- Recalcular solo si el delta supera ×2 respecto al objetivo.
- Cuantizar a potencias de 2 con paso ×1.5 intermedio (1, 1.5, 2, 3, 4, 6, 8, …).
- Enviar `mining.set_difficulty` + reenviar job actual con `clearJobs=true`.

### Gestión de desconexiones (crítico para Bitaxe/NerdAxe)
Los Bitaxes pierden WiFi, se resetean o sufren latencia alta con frecuencia. blitzpool maneja
esto con varias capas:

**Timeout en dos fases:**
- Al aceptar la conexión: 30 s para que llegue el primer byte (detecta conexiones fantasma).
  Si expira → `socket.destroy()`. Relevante cuando un Bitaxe arranca pero no termina el handshake.
- Tras el handshake: 5 min de inactividad. Si expira → `end()` + `destroy()`. Generoso a propósito
  porque con dificultad alta pueden pasar minutos entre shares.

**ECONNRESET silenciado:** cuando el Bitaxe pierde la WiFi el TCP se corta abruptamente sin FIN,
generando `ECONNRESET`. blitzpool lo filtra del log (no es un error real, es ruido). El
`socket.destroy()` dispara `close` igualmente y la limpieza se hace normal.

**Limpieza completa en disconnect:** al recibir `close`, hay que:
1. Cancelar la suscripción al `watch` channel de templates — evita escribir a un socket muerto.
2. Limpiar todos los timers del canal (vardiff, keepalive).
3. `drop` de todos los `Arc`/`Sender` que apunten al canal — libera memoria.
4. Borrar el canal del mapa de canales activos.
Actualmente en `stratum_sv2.rs` si el translator cae, la task del canal puede quedar huérfana
enviando jobs a un `TcpStream` muerto hasta que el write falle.

**Write seguro:** comprobar que el `TcpStream` no está cerrado antes de escribir. En Tokio,
un write a un socket cerrado devuelve `Err(BrokenPipe)` — capturarlo y trigger la limpieza
en lugar de propagar el error.

**Sesión nueva en cada reconexión:** al desconectarse, no guardar estado de sesión que la
reconexión deba recuperar. SV2 tampoco tiene concepto de sesión persistente: cuando el translator
reconecta, abre un `OpenExtendedMiningChannel` nuevo y recibe un `channel_id` fresco.

**TCP_NODELAY — Nagle's algorithm:** el kernel TCP agrupa paquetes pequeños esperando hasta
~200 ms antes de mandarlos (Nagle's algorithm), útil para bulk data pero destructivo en mining.
El ACK de un share (~50 bytes) puede quedarse en buffer 200 ms mientras el Bitaxe espera para
saber si puede seguir. Con `setNoDelay(true)` cada write sale al wire inmediatamente.
blitzpool lo activa en el `accept`, antes de detectar protocolo.
En Tokio: `TcpStream::set_nodelay(true)` justo tras `listener.accept().await`.

---

## Mejoras identificadas (análisis de public-pool)

public-pool es el upstream del que deriva blitzpool. Útil como referencia de lo que blitzpool
corrigió y como fuente de antipatrones a evitar.

### Antipatrón: handler de error vacío

`stratum-v1.service.ts:78`: el handler de `error` está completamente vacío `{}`.
En Node.js es obligatorio tener un listener para no crashear el proceso, pero no hace nada.
ECONNRESET, EPIPE, ETIMEDOUT — todos silenciados sin log ni limpieza. La limpieza depende
enteramente del evento `close` que dispara después.
En Rust/Tokio no aplica (los errores de IO son `Result`), pero la lección es: **toda ruta de
error de socket debe terminar en la misma limpieza que un `close` normal**.

### Antipatrón: sin cleanup si el cliente desconecta antes del handshake

`stratum-v1.service.ts:64`: el handler de `close` guarda la limpieza con
`if (client.extraNonceAndSessionId != null)`. Si el Bitaxe conecta y corta antes de mandar
`mining.subscribe`, no se cancela nada, no se llama `socket.removeAllListeners()`,
y los listeners quedan vivos con referencias al objeto cliente hasta que el GC los recoja.
En nuestra pool: si el translator abre TCP pero cae antes de `SetupConnection`, el `accept`
ya habrá creado estructuras — hay que limpiarlas igualmente.

### Antipatrón: write directo sin guard dentro de checkDifficulty

`StratumV1Client.ts:611`: dentro de `checkDifficulty()` usa `this.socket.write(data)` directo,
saltándose el helper `write()` que comprueba `destroyed` y `writableEnded`. Si el socket cierra
durante un ajuste de dificultad, esto lanza o falla silenciosamente.
En Rust: **nunca escribir al `WriteHalf` de un canal desde un timer o tarea secundaria sin pasar
por el mismo punto de salida que gestiona el cierre**.

### Antipatrón: sin TCP keepalive

public-pool no llama `socket.setKeepAlive()`. Depende únicamente del timeout de 5 min de
inactividad a nivel aplicación. Si la red intermedia descarta la conexión silenciosamente
(NAT timeout, WiFi AP que "olvida" la sesión), el servidor no lo sabe hasta que intenta escribir.
En Tokio: `socket.set_keepalive(Some(Duration::from_secs(60)))` activa `SO_KEEPALIVE` con
probes TCP del kernel — detecta conexiones muertas sin depender de que el miner envíe datos.

### Buena práctica confirmada: no confiar en la dificultad declarada

El commit `fda3f21` introduce un endpoint REST para recibir shares de otras instancias.
La validación (`external-share.controller.ts:39`) **recalcula la dificultad desde el header raw**
y descarta el valor que el cliente afirme. Principio aplicable en nuestra validación SV2:
el `SubmitSharesExtended` incluye campos informativos pero la dificultad real siempre se
recomputa del hash del header reconstruido.

### Buena práctica confirmada: ventana temporal en shares recibidos

El mismo commit aplica una ventana de ±10 minutos sobre el `block.timestamp` del header
(`external-share.controller.ts:46-51`) para rechazar shares replay de bloques antiguos.
En nuestra validación SV2: el `ntime` del `SubmitSharesExtended` debe estar dentro de un
rango razonable respecto al `prev_hash` del template activo. Ya lo tenemos implícito (job
desaparece tras 90 s), pero conviene validarlo explícitamente.

### Batch de inserts en DB y throttle de heartbeat

public-pool acumula inserts de clientes nuevos en cola y los persiste cada 5 s
(`client.service.ts:25`). Los updates de estadísticas sólo van a DB cada 60 s
(`StratumV1ClientStatistics.ts:93`), no en cada share.
Aplicar en Paso 6: nunca hacer un INSERT/UPDATE síncrono en el hot path de validación de share.
Usar un canal `mpsc` para mandar eventos al worker de DB y devolver el ACK al miner sin esperar.
