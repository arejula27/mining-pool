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
       │  Cap'n Proto IPC (UNIX socket)
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
~~Reemplaza el RPC poller con conexión TCP+Noise a sv2-tp.~~ Completado, pero sustituido por Paso 4b-IPC.
- [x] (histórico) `template_client.rs`, sv2-tp en flake, `bitcoin/sv2-tp.conf`, tests `template_client.rs` y `mine_block.rs`

## Paso 4b-IPC — IPC directo a Bitcoin Core (elimina sv2-tp)
El equipo de sv2 confirmó que sv2-tp solo es necesario si pool y nodo corren en máquinas distintas.
Conectamos directamente al nodo via Cap'n Proto sobre UNIX socket usando los mismos crates que usa sv2-tp internamente.

**Módulo objetivo: `pool/src/node_ipc.rs` (~150 líneas)**

**Crates a añadir:** `bitcoin-capnp-types = "0.1.0"`, `capnp`, `capnp-rpc`, `tokio-util` (feature `compat`)

**Config:** sustituir `TP_ADDRESS` por `BITCOIN_IPC_SOCKET` (path al UNIX socket, e.g. `.bitcoin-data/regtest/node.sock`)

**Interfaz pública idéntica a la actual:**
`start(socket_path, coinbase_output_max_size)` → `(watch::Receiver<RawTemplate>, mpsc::Sender<SubmitSolutionData>)`

**Internos** (todo en un único archivo):
- Thread dedicado: `std::thread::spawn` + `Runtime::new()` + `LocalSet::block_on` (capnp-rpc es `!Send`)
- Bootstrap Cap'n Proto: `UnixStream` → `RpcSystem` → `InitIpcClient` → `MiningIpcClient` → `BlockTemplateIpcClient`
- Fetch template: `get_block_header` + `get_coinbase_tx` + `get_coinbase_merkle_path` → construir `NewTemplate` + `SetNewPrevHash`
- `waitNext` loop (timeout 10 s): si cambia `prev_hash` → chain tip nuevo; si no → fee mempool → emitir al `watch` channel
- Submit solution: `template_ipc_client.submit_solution_request()` con version/timestamp/nonce/coinbase

**`RawTemplate` pasa de bytes crudos a tipos estructurados** (`NewTemplate<'static>` + `SetNewPrevHash<'static>`) — elimina la serialización/deserialización redundante.

**Eliminar:**
- [ ] `pool/src/template_client.rs` → reemplazar por `node_ipc.rs`
- [ ] sv2-tp de `flake.nix`, `bitcoin/sv2-tp.conf`, recetas `just start/stop-sv2-tp`
- [ ] `read_authority_pubkey` y lectura de `sv2_authority_key`
- [ ] `TP_ADDRESS` de `config.rs`; añadir `BITCOIN_IPC_SOCKET`
- [ ] Actualizar `stratum_sv2.rs` para consumir `NewTemplate`/`SetNewPrevHash` directamente (sin `binary_sv2::from_bytes`)
- [ ] Actualizar tests `mine_block.rs` y `sv1_miner.rs` para el nuevo arranque (sin sv2-tp subprocess)

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
- [x] SQLite con `rusqlite` (bundled, sin Redis ni sqlx)
- [x] Tabla `miners` (dirección BTC, primera conexión)
- [x] Tabla `shares` (miner_address, difficulty, block_hash_be, timestamp)
- [x] Tabla `epoch_stats` (miner_address, shares_count, total_difficulty, best_share_hash) — epoch_number se añade en Paso 8
- [x] Tabla `competition_entries` (miner_address, entry_paid_sats, entry_timestamp, ark_address) — epoch_number se añade en Paso 8
- [x] En el hot path: ACK al miner primero, `Sender::send()` no bloqueante tras el write — DB nunca bloquea el ACK
- [x] `DbWorker` acumula en `Vec` en memoria; flush cada 60 s con una sola transacción SQLite (batch INSERT + upsert epoch_stats)
- [x] `best_share_hash` se actualiza sólo cuando el nuevo hash es menor (mejor) que el actual — comparación BLOB en SQLite
- [x] `tracing::debug!(ack_us = ...)` en el hot path de share validation
- [x] Test `share_ack_does_not_block_on_db` en `tests/sv1_miner.rs`: DB artificial de 100 ms/evento, assert ACK < 500 ms

## Paso 7 — Medición de hashrate
- [x] En `stratum_sv2.rs`: al aceptar share válido, enviar `DbEvent::Share(miner_address, difficulty, timestamp)` — ya fluye por el canal del Paso 6
- [x] En `DbWorker`: acumular shares por ventanas de 1 minuto por address (`HashMap<String, f64>`)
- [x] Cada minuto: `epoch_stats.active_minutes++` + insertar en `minute_hashrates` en el flush batch; pruning de rows > 24 h
- [x] `hashrate = Σ(share_difficulty) * 2^32 / window_seconds` (misma fórmula que blitzpool)
- [x] API interna (`DbReader`): `hashrate_for_address(address, lookback_minutes)`, `pool_hashrate(lookback_minutes)` — WAL mode, read-only connection

## Paso 8 — Grupos colaborativos

Dos o más miners pueden asociarse para compartir la coinbase: si cualquiera del grupo
encuentra un bloque, el subsidio se reparte exactamente según los pesos acordados.
El modelo sigue siendo lottery (cada bloque encontrado va a la coinbase del grupo, no hay
redistribución externa), pero la coinbase ya tiene múltiples outputs desde el principio.

**Decisión de diseño:** los miners en solitario son grupos de uno (weight = 1, un solo miembro).
Así el código de `stratum_sv2` y de construcción de coinbase no tiene bifurcaciones —
siempre opera con `Vec<(address, weight)>`. El coste es un INSERT de grupo en la primera
conexión del miner, lo cual es insignificante.

### Modelo de datos
- [ ] Nueva tabla `groups` (`group_id UUID PK`, `name TEXT`, `auto_split BOOLEAN`)
- [ ] Nueva tabla `group_members` (`group_id`, `miner_address`, `weight INTEGER`, `joined_at`)
  — `weight` es un entero; la distribución se calcula como `weight / Σweights`
- [ ] Al conectar un miner por primera vez sin grupo declarado: crear automáticamente
  un grupo solo (`auto_split = false`, `weight = 1`) con su dirección

### Coinbase multi-output
- [ ] Extender `build_sv2_coinbase_from_tdp` para aceptar `Vec<(address, weight)>`
  en lugar de una sola dirección
- [ ] Los outputs se ordenan por weight desc para reproducibilidad (misma serialización
  independientemente del orden de inserción en BD)
- [ ] El `coinbase_output_max_size` enviado en `CoinbaseOutputConstraints` debe
  actualizarse para reflejar el peor caso (N outputs × ~34 bytes cada uno)

### Integración en stratum_sv2
- [ ] Al recibir `OpenExtendedMiningChannel`, consultar BD para obtener el grupo del miner
  y construir la coinbase con todos sus miembros y pesos
- [ ] Todos los miembros activos del mismo grupo reciben exactamente el mismo job
  (mismo `job_id`, mismo coinbase) — comparten trabajo, no lo duplican
- [ ] Al cambiar template, reemitir el job del grupo con el nuevo template base
  (los pesos no cambian entre templates)

### Modo auto-split
- [ ] Si `groups.auto_split = true`, recalcular los pesos de cada miembro antes de
  emitir cada nuevo job, usando `DbReader::hashrate_for_address(address, 60)`
- [ ] Los pesos se redondean a enteros proporcionales (ej. hashrates [3 TH, 1 TH] → pesos [3, 1])
- [ ] Si un miembro lleva más de N minutos sin shares (sin hashrate medible), su peso
  cae a 0 temporalmente hasta que vuelva a minar

### Gestión de grupos (API HTTP)
- [ ] `POST /groups` — crear grupo (`name`, `auto_split`)
- [ ] `POST /groups/:id/members` — añadir miembro (`miner_address`, `weight`)
- [ ] `DELETE /groups/:id/members/:address` — salir del grupo
- [ ] `GET /groups/:id` — ver grupo, miembros y pesos actuales

### Tests
- [ ] Unit: `build_sv2_coinbase_from_tdp` con múltiples outputs — verificar que la suma
  de outputs = coinbase_value y que los scripts son correctos
- [ ] Unit: miner que conecta sin grupo → se crea grupo solo automáticamente
- [ ] Integration: dos miners del mismo grupo reciben el mismo `job_id`; un miner
  en solitario recibe job diferente en la misma ronda
- [ ] Integration: auto-split recalcula pesos correctamente al llegar nuevo template

## Paso 9 — Plantillas custom (Fase 2)

Miners avanzados con nodo Bitcoin propio pueden declarar su propia template vía
Job Declaration Protocol (JDC). La pool actúa como árbitro de shares: valida que
el trabajo enviado es válido pero no construye la coinbase.

### Arquitectura
```
[Bitcoin Node del miner] → [JDC client] → [Job Declarator Server (JDS)] → nuestra pool
[Miner SV2]  ─────────────────────────────────────────────────────────→  (shares)
```
El JDS es el binario oficial de sv2-apps, sin modificar. La pool recibe shares
referenciados a jobs declarados externamente.

### Verificación de coinbase para grupos
Cuando un miner pertenece a un grupo y usa template propia, la pool **debe verificar**
que los outputs de la coinbase declarada respetan la distribución acordada:
- Extraer outputs del coinbase del job declarado
- Comprobar que cada `(address, amount)` coincide con los pesos del grupo aplicados
  al `coinbase_value` del template (tolerancia ±1 sat por redondeo)
- Si no coincide → rechazar el job con `DeclareMiningJobError`
- Sin esta verificación, un miembro podría declarar una coinbase que se lleva el 100%

### Tablas adicionales
- `declared_jobs` (`job_id`, `miner_address`, `template_hash`, `coinbase_tx_hex`,
  `declared_at`) — para auditoría y resolución de disputas

### Lo que queda fuera de este paso
- [ ] Integración completa con bark para pago al ganador de grupos en competición
- [ ] UI para gestión de grupos

## Paso 10 — Competición por época (Bitaxe League)

> Aplazado respecto al plan original para dar prioridad a grupos colaborativos (Paso 8).
> Los grupos pueden participar en la competición: el grupo como unidad tiene su
> `best_difficulty` y su `active_minutes` agregados.

### Modelo de negocio
- Mineros **free**: minan normal, coinbase a su dirección (o grupo), sin acceso a competición
- Mineros **competitor**: pagan 5 000 sats de entrada → compiten en la **siguiente** época
- Un grupo completo puede inscribirse: todos los miembros pagan la cuota, el premio
  se reparte según los pesos del grupo
- Premio: suma de todas las entradas menos comisión arbitraria de la pool
- Pago: ARK (almacenar dirección ARK del ganador; lógica de pago posterior)

### Épocas
- Época = 2016 bloques de Bitcoin (ajuste de dificultad estándar)
- Número de época derivado de `block_height / 2016`
- Al inicio de cada época: los competitors que pagaron en la anterior quedan activados

### Reglas de elegibilidad para ganar
- Activo ≥ 90 % de la época: `active_minutes >= 0.9 * epoch_duration_minutes`
  (anti-cheat: no vale minar con dificultad altísima 5 min y apagarse)
- Hashrate dentro del rango Bitaxe/NerdAxe: cap superior ~equivalente a un OctaXe
  (el número exacto se fija después; la lógica de comprobación ya debe existir)
- Hashrate mínimo: debe superar un umbral mínimo de actividad real

### Determinación del ganador
- Ganador = competitor elegible con mayor `best_difficulty` en la época
  (`best_difficulty` = dificultad del mejor share individual enviado)
- En caso de empate exacto: primer share en tiempo gana

### Tablas adicionales
- `competition_entries`: registra la inscripción (epoch_number, miner_address, ark_address, paid_sats)
- `epoch_stats` ya cubre el tracking de actividad y best_difficulty por miner por época

### Lo que queda fuera de este paso (futuro)
- [ ] Cobro real de los 5 000 sats (Lightning / ARK on-chain)
- [ ] Pago automático al ganador via ARK
- [ ] Frontend/API pública con leaderboard

## Paso 11 — Integración end-to-end
- [ ] Stack completo en local: bitcoind + nuestra pool (IPC directo) + translator
- [ ] Conectar Bitaxe real (o simulado) al translator
- [ ] Verificar que recibe jobs y envía shares válidos a través del stack completo
- [ ] Verificar que un bloque encontrado en regtest se propaga a Core
- [ ] Simular dos miners en grupo colaborativo: verificar que la coinbase del bloque
  encontrado tiene los dos outputs correctos
- [ ] Simular una época completa con varios miners (free + competitor) y verificar ganador

---

## Mejoras identificadas (análisis de blitzpool)

Puntos extraídos del análisis del fork JS de public-pool. No son pasos bloqueantes, aplicar cuando
corresponda según la fase en curso.

### Latencia de respuesta a shares ✓ implementado en Paso 6
`SubmitSharesSuccess` sale al wire antes de cualquier operación de DB. El evento de share
se envía por `std::sync::mpsc::Sender::send()` (no bloqueante) tras el ACK. Test de regresión:
`share_ack_does_not_block_on_db` en `sv1_miner.rs`.

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

### Batch de inserts en DB ✓ implementado en Paso 6
`DbWorker` acumula eventos en `Vec` en memoria y hace un único `BEGIN/COMMIT` cada 60 s.
Nunca hay INSERT síncrono en el hot path.
