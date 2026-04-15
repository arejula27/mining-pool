# Pool SPEC

## Concepto

Pool de minería estilo lotería con sistema de competición winner-takes-all por epoch de dificultad.
El pool no redistribuye recompensas de bloques — cada miner cobra su propia coinbase si encuentra un bloque.
El bote es independiente, financiado exclusivamente por las cuotas mensuales de los participantes.

**Referencia de implementación**: [public-pool](https://github.com/benjamin-wilson/public-pool) (GPL-3.0).
El comportamiento base del pool (conexión de miners, coinbase por dirección, modo sin cuota) es idéntico
al de public-pool. La competición por epoch y el pago del bote son la capa añadida encima.

---

## Fases de implementación

### Fase 1 — Plug & Play (prioridad)
Comportamiento idéntico a public-pool: el miner se conecta, pone su dirección BTC como username,
el pool construye la coinbase con esa dirección y distribuye jobs via Stratum.
Sin configuración extra por parte del miner.

### Fase 2 — Custom templates (posterior)
Soporte para miners avanzados con nodo Bitcoin propio que quieran declarar su propia template
vía Job Declaration Protocol (SV2). El pool actúa solo como árbitro de shares.

---

## Reglas de participación

### Modos de uso

**Modo libre (sin cuota)**
- Cualquiera puede minar en el pool sin pagar
- El pool construye la coinbase con su dirección: si encuentran un bloque se lo quedan íntegro
- No entran al concurso del bote
- Idéntico al comportamiento de public-pool

**Modo competición (con cuota)**
- **Cuota**: 5.000 sats/mes por cuenta
- Entra al ranking del epoch y puede ganar el bote
- Sujeto a las restricciones de dispositivos y hashrate descritas abajo

### Dispositivos y cuentas

- **Máquinas elegibles**: Bitaxe / NerdAxe únicamente
  - Filtro por user-agent: `BitAxe/X.X.X` (ESP-Miner)
  - Cap de hashrate por dispositivo: por definir (orientado a home miners)
- **Máximo 5 dispositivos por cuenta**
- Si se quieren conectar más de 5 dispositivos, se deben abrir cuentas adicionales
  y pagar la cuota por cada una (cada cuenta tiene su propia dirección BTC y Ark)
- El pool mide el hashrate efectivo por worker y suspende los que superen el cap por dispositivo

### Elegibilidad para el bote

- Haber pagado la cuota del epoch en curso
- Haber alcanzado el mínimo de shares válidos durante el epoch (por definir)
- No superar el cap de hashrate por dispositivo

### Ganador

- El miner con el share de mayor dificultad al cierre del epoch entre los elegibles
- **Bote**: 100% de las cuotas recaudadas ese epoch, pagado vía Ark

---

## Epoch y periodo

- El concurso se resetea cada **difficulty epoch de Bitcoin** (cada 2016 bloques, ~2 semanas)
- El ranking es por bloques, no por tiempo
- Al cierre de cada epoch: se determina el ganador, se paga el bote, se resetea el ranking

---

## Coinbase y bloques encontrados

- Cada miner registra su dirección BTC al inscribirse
- El pool construye la coinbase con la dirección del miner (igual que public-pool)
- Si un miner encuentra un bloque real: el subsidio + fees van 100% al miner directamente
- El pool no toca los fondos del bloque en ningún caso
- El evento de bloque encontrado no afecta al bote ni al ranking del epoch

---

## Pago del bote

- **Mecanismo**: Ark (bark)
- El participante registra su Ark address al inscribirse
- El pago se ejecuta automáticamente al cierre del epoch desde el servicio
- Alternativa fallback: onchain si el participante no tiene Ark address

---

## Transparencia

- Leaderboard público en tiempo real con el `best_difficulty` de cada miner
- Cualquiera puede auditar el ranking a partir de los datos públicos del pool
- Los shares publicados permiten verificación externa del ganador

---

## Arquitectura técnica

### Stack

- **Lenguaje**: Rust (servicio único)
- **Protocolo**: Stratum V2 (stratum-mining/stratum, Apache 2.0 / MIT)
- **Base de datos**: PostgreSQL o SQLite (por definir)
- **Pagos**: bark (cliente Ark de Second)
- **Nodo**: Bitcoin Core

### Componentes que hostea el operador

| Componente | Origen | Notas |
|---|---|---|
| Translation Proxy | stratum-mining (oficial) | Para Bitaxe/NerdAxe SV1 |
| SV2 Pool Server | stratum-mining (oficial) | Con wrapper de lógica propia |
| Job Declarator Server | stratum-mining (oficial) | Para miners avanzados |
| Bitcoin Core | - | getblocktemplate + validación |
| bark daemon | Second / Ark Labs | Pagos Ark |
| Servicio principal Rust | Propio | Lógica de negocio completa |

### Componentes que hostea el miner (modo avanzado)

| Componente | Notas |
|---|---|
| Bitcoin Node | Para construir su propia template |
| Job Declarator Client (JDC) | Declara la template al JDS del pool |

### Diagrama: Plug & Play (Bitaxe/NerdAxe)

```
[Bitaxe SV1] ──→ [Translation Proxy] ──→ ┐
[NerdAxe SV1] ─→ (SV1→SV2)               └──→ [Pool Rust] ──→ [Bitcoin Core]
                                                     │
                                               share events
                                                     │
                                         ┌───────────┴───────────┐
                                         │  lógica de negocio    │
                                         │  - best_difficulty    │
                                         │  - cuotas             │
                                         │  - epoch tracking     │
                                         │  - BD                 │
                                         └───────────┬───────────┘
                                                     │
                                             [bark daemon]
                                          payout al ganador
```

### Diagrama: Template propia (miner avanzado)

```
[Bitcoin Node]  ──→ [JDC client] ──→ [Job Declarator Server] ──→ ┐
(template propia,    (del miner)      (del operador)              ├──→ [Pool Rust] ──→ [Bitcoin Core]
 coinbase propia)                                                  ┘         │
                                                                       share events
[Miner SV2] ───────────────────────────────────────────────────────→      │
                                                                   ┌───────┴───────┐
                                                                   │ lógica negocio│
                                                                   └───────┬───────┘
                                                                           │
                                                                   [bark daemon]
```

### Estructura del servicio Rust

```
pool/
├── src/
│   ├── main.rs
│   ├── stratum/         # wrapper sobre crates stratum-mining
│   │   ├── pool.rs      # SV2 Pool Server
│   │   ├── proxy.rs     # Translation Proxy (SV1→SV2)
│   │   └── jds.rs       # Job Declarator Server
│   ├── epoch/           # lógica de epoch y ranking
│   │   ├── tracker.rs   # best_difficulty por miner
│   │   └── reset.rs     # cierre y reset de epoch
│   ├── fees/            # gestión de cuotas
│   │   └── payment.rs
│   ├── payout/          # integración bark
│   │   └── ark.rs
│   ├── db/              # persistencia
│   │   └── schema.rs
│   └── api/             # leaderboard público (HTTP)
│       └── routes.rs
└── Cargo.toml
```

### Dependencias principales

```toml
[dependencies]
# Protocolo SV2
mining_sv2 = { git = "https://github.com/stratum-mining/stratum" }
job_declaration_sv2 = { git = "https://github.com/stratum-mining/stratum" }
template_distribution_sv2 = { git = "https://github.com/stratum-mining/stratum" }

# Async runtime
tokio = { version = "1", features = ["full"] }

# Base de datos
sqlx = { version = "0.7", features = ["postgres", "runtime-tokio"] }

# HTTP (leaderboard API)
axum = "0.7"

# bark / Ark (por definir según API disponible)
```

---

## Modelo de datos (borrador)

```sql
-- Participantes registrados
CREATE TABLE miners (
    id          UUID PRIMARY KEY,
    btc_address TEXT NOT NULL,      -- coinbase
    ark_address TEXT,               -- payout del bote
    registered_at TIMESTAMPTZ NOT NULL
);

-- Cuotas pagadas
CREATE TABLE fees (
    id         UUID PRIMARY KEY,
    miner_id   UUID REFERENCES miners(id),
    epoch      INTEGER NOT NULL,    -- número de epoch
    paid_at    TIMESTAMPTZ NOT NULL,
    amount_sat BIGINT NOT NULL
);

-- Shares por epoch
CREATE TABLE shares (
    id            UUID PRIMARY KEY,
    miner_id      UUID REFERENCES miners(id),
    epoch         INTEGER NOT NULL,
    difficulty    NUMERIC NOT NULL,
    submitted_at  TIMESTAMPTZ NOT NULL,
    job_id        TEXT NOT NULL
);

-- Ranking por epoch (mejor share por miner)
CREATE TABLE epoch_best (
    epoch       INTEGER NOT NULL,
    miner_id    UUID REFERENCES miners(id),
    best_diff   NUMERIC NOT NULL,
    PRIMARY KEY (epoch, miner_id)
);

-- Payouts ejecutados
CREATE TABLE payouts (
    id         UUID PRIMARY KEY,
    epoch      INTEGER NOT NULL,
    miner_id   UUID REFERENCES miners(id),
    amount_sat BIGINT NOT NULL,
    ark_txid   TEXT,
    paid_at    TIMESTAMPTZ NOT NULL
);
```

---

## Pendiente de definir

- [ ] Cap de hashrate exacto por cuenta
- [ ] Mínimo de shares por epoch para ser elegible
- [ ] PostgreSQL vs SQLite
- [ ] Gestión de cuotas: ¿Lightning para cobrarlas? ¿También Ark?
- [ ] Leaderboard: ¿WebSocket para tiempo real o polling?
- [ ] Manejo del caso: miner paga cuota pero no alcanza mínimo de shares
- [ ] Qué pasa si nadie califica en un epoch (bote se acumula al siguiente?)
- [ ] Licencia del proyecto
