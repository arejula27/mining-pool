# Pool SPEC

## Concepto

Pool de minería estilo lotería donde cada miner (o grupo colaborativo) controla su propia coinbase.
El pool no redistribuye recompensas de bloques — si un miner encuentra un bloque, el subsidio va
íntegramente a la coinbase que él (o su grupo) configuró. La competición winner-takes-all por epoch
es una capa opcional encima, financiada por cuotas de participación.

**Referencia de implementación**: [public-pool](https://github.com/benjamin-wilson/public-pool) (GPL-3.0).
El comportamiento base (conexión de miners, coinbase por dirección, modo sin cuota) es idéntico
al de public-pool. Los grupos colaborativos, la competición por epoch y el pago del bote son las
capas añadidas encima.

---

## Fases de implementación

### Fase 1 — Plug & Play ✓ completada
Miner se conecta, pone su dirección BTC como username, el pool construye la coinbase con esa
dirección y distribuye jobs via Stratum V2 (a través del translator oficial para Bitaxe/NerdAxe SV1).

### Fase 2 — Grupos colaborativos (en curso)
Dos o más miners pueden asociarse y compartir la coinbase con los pesos que acuerden.
Los miners en solitario son tratados internamente como grupos de uno — código uniforme sin bifurcaciones.
Soporte para distribución manual (pesos fijos) y automática (proporcional al hashrate aportado).

### Fase 3 — Plantillas custom
Miners avanzados con nodo Bitcoin propio declaran su propia template vía Job Declaration Protocol (SV2).
El pool actúa como árbitro de shares. Para grupos, verifica que la coinbase declarada respeta
la distribución acordada (impide que un miembro se asigne el 100%).

### Fase 4 — Competición por epoch (Bitaxe League)
Sistema winner-takes-all por epoch de dificultad (2016 bloques). Los grupos pueden participar
como unidad. Pago del bote via Ark.

---

## Modos de uso

**Modo libre (sin cuota)**
- Cualquiera puede minar sin pagar
- El pool construye la coinbase con la dirección del miner; si encuentra un bloque se lo queda íntegro
- No entra al concurso del bote

**Modo grupo**
- Dos o más miners se asocian y acuerdan una distribución de la coinbase (p.ej. 60 % Alice, 40 % Bob)
- Si cualquier miembro del grupo encuentra un bloque, la coinbase ya tiene los outputs configurados
- El pool no interviene en el reparto: la distribución está codificada en la propia coinbase
- Disponible con o sin cuota de competición
- **Auto-split**: la pool puede calcular los pesos automáticamente en función del hashrate aportado
  por cada miembro en la última hora

**Modo competición (con cuota)**
- **Cuota**: 5.000 sats/epoch por cuenta (o por grupo si compiten como unidad)
- Entra al ranking del epoch y puede ganar el bote
- Sujeto a las restricciones de dispositivos y hashrate descritas abajo

---

## Dispositivos y restricciones

- **Máquinas elegibles para competición**: Bitaxe / NerdAxe únicamente
  - Filtro por user-agent: `BitAxe/X.X.X` (ESP-Miner)
  - Cap de hashrate por dispositivo: por definir (orientado a home miners)
- **Máximo 5 dispositivos por cuenta**
- El pool mide el hashrate efectivo por worker y suspende los que superen el cap

---

## Coinbase y bloques encontrados

- Cada miner registra su dirección BTC al conectarse (username del canal SV2)
- Los miners en solitario tienen una coinbase de un solo output (su dirección)
- Los grupos tienen una coinbase multi-output: un output por miembro, proporcional a su peso
- Si alguien del grupo encuentra un bloque real: el subsidio + fees se reparten exactamente
  según la coinbase configurada — el pool no toca los fondos en ningún caso
- El evento de bloque encontrado no afecta al bote ni al ranking del epoch

---

## Competición por epoch

- Época = 2016 bloques de Bitcoin (~2 semanas)
- Ganador = participant elegible con mayor `best_difficulty` (dificultad del mejor share individual)
- En caso de empate: primer share en tiempo gana
- **Elegibilidad**: haber pagado cuota + activo ≥ 90 % de la época + hashrate dentro del rango Bitaxe
- **Bote**: 100 % de las cuotas recaudadas ese epoch, pagado vía Ark
- Al cierre de cada epoch: se determina el ganador, se paga el bote, se resetea el ranking
- Un grupo compite como unidad: su `best_difficulty` es el mejor share de cualquier miembro;
  el premio se reparte entre los miembros según sus pesos

---

## Pago del bote

- **Mecanismo**: Ark (bark)
- El participante registra su Ark address al inscribirse
- El pago se ejecuta automáticamente al cierre del epoch
- Fallback: onchain si el participante no tiene Ark address

---

## Transparencia

- Leaderboard público con el `best_difficulty` de cada miner/grupo
- Hashrates individuales y de grupo consultables
- Los shares publicados permiten verificación externa del ganador

---

## Arquitectura técnica

### Stack

- **Lenguaje**: Rust (servicio único)
- **Protocolo**: Stratum V2 (stratum-mining/stratum, Apache 2.0 / MIT)
- **Base de datos**: SQLite (`rusqlite`, bundled)
- **Pagos**: bark (cliente Ark de Second)
- **Nodo**: Bitcoin Core (conexión directa via Cap'n Proto IPC, sin sv2-tp)

### Componentes que hostea el operador

| Componente | Origen | Notas |
|---|---|---|
| Translation Proxy | sv2-apps (oficial) | Para Bitaxe/NerdAxe SV1 |
| SV2 Pool Server | Propio | Lógica de negocio completa |
| Job Declarator Server | sv2-apps (oficial) | Para miners avanzados (Fase 3) |
| Bitcoin Core | - | IPC socket + RPC para tests |
| bark daemon | Second / Ark Labs | Pagos Ark (Fase 4) |

### Componentes que hostea el miner (modo avanzado, Fase 3)

| Componente | Notas |
|---|---|
| Bitcoin Node | Para construir su propia template |
| Job Declarator Client (JDC) | Declara la template al JDS del pool |

### Diagrama: Plug & Play (Bitaxe/NerdAxe)

```
[Bitaxe SV1] ──→ [Translation Proxy] ──→ ┐
[NerdAxe SV1] ─→ (SV1→SV2)               └──→ [Pool Rust] ──→ [Bitcoin Core]
                                                     │           (IPC socket)
                                               share events
                                                     │
                                         ┌───────────┴───────────┐
                                         │  lógica de negocio    │
                                         │  - grupos / coinbase  │
                                         │  - best_difficulty    │
                                         │  - cuotas / epoch     │
                                         │  - BD (SQLite)        │
                                         └───────────┬───────────┘
                                                     │
                                             [bark daemon]
                                          payout al ganador
```

### Diagrama: Template propia (Fase 3, miner avanzado)

```
[Bitcoin Node]  ──→ [JDC client] ──→ [Job Declarator Server] ──→ ┐
(template propia,    (del miner)      (del operador)              ├──→ [Pool Rust]
 coinbase propia)                                                  ┘    verifica coinbase
[Miner SV2] ──────────────────────────────────────────────────────→    de grupo
```

---

## Modelo de datos

```sql
-- Miners registrados (creados al primera conexión)
CREATE TABLE miners (
    address      TEXT PRIMARY KEY,   -- dirección BTC
    first_seen   INTEGER NOT NULL    -- unix timestamp
);

-- Grupos (los miners en solitario son grupos de 1)
CREATE TABLE groups (
    group_id    TEXT PRIMARY KEY,    -- UUID
    name        TEXT,
    auto_split  BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE group_members (
    group_id        TEXT REFERENCES groups(group_id),
    miner_address   TEXT REFERENCES miners(address),
    weight          INTEGER NOT NULL DEFAULT 1,
    joined_at       INTEGER NOT NULL,
    PRIMARY KEY (group_id, miner_address)
);

-- Shares recibidos
CREATE TABLE shares (
    miner_address  TEXT NOT NULL,
    difficulty     REAL NOT NULL,
    block_hash_be  BLOB NOT NULL,
    timestamp      INTEGER NOT NULL
);

-- Estadísticas por epoch (mejor share + actividad)
CREATE TABLE epoch_stats (
    miner_address   TEXT NOT NULL,
    epoch_number    INTEGER NOT NULL,
    best_share_hash BLOB,
    active_minutes  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (miner_address, epoch_number)
);

-- Hashrates por ventana de 1 minuto (pruning a 24 h)
CREATE TABLE minute_hashrates (
    miner_address  TEXT NOT NULL,
    window_start   INTEGER NOT NULL,
    difficulty_sum REAL NOT NULL,
    PRIMARY KEY (miner_address, window_start)
);

-- Inscripciones a la competición
CREATE TABLE competition_entries (
    miner_address   TEXT NOT NULL,
    epoch_number    INTEGER NOT NULL,
    ark_address     TEXT,
    paid_sats       INTEGER NOT NULL,
    entry_timestamp INTEGER NOT NULL,
    PRIMARY KEY (miner_address, epoch_number)
);

-- Jobs declarados por miners avanzados (Fase 3)
CREATE TABLE declared_jobs (
    job_id          TEXT PRIMARY KEY,
    miner_address   TEXT NOT NULL,
    template_hash   TEXT NOT NULL,
    coinbase_tx_hex TEXT NOT NULL,
    declared_at     INTEGER NOT NULL
);
```

---

## Pendiente de definir

- [ ] Cap de hashrate exacto por dispositivo para competición
- [ ] Mínimo de shares por epoch para ser elegible
- [ ] Gestión de cuotas: Lightning / Ark on-chain
- [ ] Leaderboard: WebSocket para tiempo real o polling
- [ ] Qué pasa si nadie califica en un epoch (¿bote se acumula al siguiente?)
- [ ] Licencia del proyecto
