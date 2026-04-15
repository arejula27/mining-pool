# Action Plan — Fase 1: Plug & Play

Objetivo: servicio Rust que distribuye trabajos a Bitaxe/NerdAxe via Stratum V1
y los miners envían shares de vuelta. Comportamiento idéntico a public-pool.

## Paso 1 — Entorno y estructura del proyecto
- [x] `flake.nix` con Rust 1.90 (fenix) + bitcoind
- [x] `cargo init` con workspace
- [x] `bitcoin.conf` para regtest local
- [x] Levantar bitcoind en regtest y verificar RPC

## Paso 2 — Cliente RPC de Bitcoin Core
- [x] Conectar al nodo via JSON-RPC (usuario/contraseña desde config)
- [ ] Implementar llamada a `getblocktemplate`
- [ ] Parsear la respuesta: transacciones, prevhash, bits, height, coinbasevalue
- [ ] Poll de nuevos bloques (detectar cuando llega bloque nuevo → `clearJobs`)

## Paso 3 — Constructor de jobs
- [ ] Construir la transacción coinbase con la dirección del miner
- [ ] Calcular el witness commitment (SegWit)
- [ ] Construir el merkle tree a partir de las transacciones
- [ ] Generar `extranonce1` único por conexión y `extranonce2` de tamaño fijo
- [ ] Serializar el job en formato `mining.notify`

## Paso 4 — Servidor Stratum V1 (TCP + JSON-RPC)
- [ ] TCP listener en el puerto configurado
- [ ] Una tarea async por conexión de miner
- [ ] Parsear mensajes JSON-RPC entrantes
- [ ] `mining.subscribe` → responder con extranonce1 + extranonce2_size
- [ ] `mining.authorize` → extraer dirección BTC del username
- [ ] `mining.notify` → enviar job al miner
- [ ] `mining.set_difficulty` → ajustar dificultad del share
- [ ] Timeout de conexiones inactivas

## Paso 5 — Validación de shares
- [ ] Recibir `mining.submit` (jobid, nonce, ntime, extranonce2)
- [ ] Reconstruir el block header
- [ ] Calcular el hash y verificar que cumple la dificultad del share
- [ ] Verificar dificultad de red → si sí, enviar bloque a Core (`submitblock`)
- [ ] Responder `true`/`false` al miner

## Paso 6 — Persistencia básica
- [ ] SQLite con sqlx
- [ ] Tabla `miners` (dirección BTC, primera conexión)
- [ ] Tabla `shares` (miner, dificultad, timestamp, epoch)
- [ ] Tabla `epoch_best` (mejor share por miner por epoch)

## Paso 7 — Integración end-to-end
- [ ] Conectar Bitaxe real (o simulado) al servidor
- [ ] Verificar que recibe jobs y envía shares válidos
- [ ] Verificar que si se encuentra un bloque (regtest) se propaga a Core
