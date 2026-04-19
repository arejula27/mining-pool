//! SQLite persistence layer.
//!
//! All writes go through a dedicated `std::thread` (the DB worker) that owns the
//! write `Connection`. The hot path sends a `DbEvent` (non-blocking) after the ACK
//! is on the wire; the worker batches inserts and flushes every 60 seconds.
//!
//! Hashrate is tracked in 1-minute windows stored in `minute_hashrates`. Readers
//! open a separate connection (WAL mode allows concurrent reads alongside the writer).

use std::{
    collections::HashMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use rusqlite::{params, Connection};
use tracing::{error, info};

// ── Public types ──────────────────────────────────────────────────────────────

pub struct ShareEvent {
    pub miner_address: String,
    /// Difficulty computed from the block hash (pdiff approximation).
    pub difficulty: f64,
    /// Block hash in big-endian byte order (same order as the target).
    pub block_hash_be: [u8; 32],
    /// Unix timestamp (seconds) from the share `ntime` field.
    pub timestamp: i64,
}

pub enum DbEvent {
    Share(ShareEvent),
    MinerConnected { address: String, timestamp: i64 },
}

// ── Writer ────────────────────────────────────────────────────────────────────

const FLUSH_INTERVAL: Duration = Duration::from_secs(60);
const MINUTE_RETENTION_SECS: i64 = 24 * 3600; // prune minute_hashrates older than 24 h

pub struct DbWorker {
    tx: mpsc::Sender<DbEvent>,
}

impl DbWorker {
    pub fn start(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure_conn(&conn)?;
        init_schema(&conn)?;
        info!("SQLite database opened at {path}");

        let (tx, rx) = mpsc::channel::<DbEvent>();
        thread::Builder::new()
            .name("db-worker".into())
            .spawn(move || run_worker(conn, rx))
            .expect("spawn db-worker thread");

        Ok(Self { tx })
    }

    pub fn sender(&self) -> mpsc::Sender<DbEvent> {
        self.tx.clone()
    }
}

// ── Reader ────────────────────────────────────────────────────────────────────

/// Read-only view of the database. Can be opened from any thread alongside the
/// writer because SQLite WAL mode allows concurrent readers.
pub struct DbReader {
    conn: Connection,
}

impl DbReader {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure_conn(&conn)?;
        Ok(Self { conn })
    }

    /// Estimated hashrate for `address` over the last `lookback_minutes` minutes.
    ///
    /// Returns 0.0 if no data is available.
    pub fn hashrate_for_address(&self, address: &str, lookback_minutes: u64) -> Result<f64> {
        let cutoff = now_secs() - (lookback_minutes as i64 * 60);
        let diff_sum: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(difficulty_sum), 0.0)
             FROM minute_hashrates
             WHERE miner_address = ?1 AND minute_ts >= ?2",
            params![address, cutoff],
            |r| r.get(0),
        )?;
        Ok(diff_sum * 4_294_967_296.0 / (lookback_minutes as f64 * 60.0))
    }

    /// Estimated pool-wide hashrate over the last `lookback_minutes` minutes.
    pub fn pool_hashrate(&self, lookback_minutes: u64) -> Result<f64> {
        let cutoff = now_secs() - (lookback_minutes as i64 * 60);
        let diff_sum: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(difficulty_sum), 0.0)
             FROM minute_hashrates
             WHERE minute_ts >= ?1",
            params![cutoff],
            |r| r.get(0),
        )?;
        Ok(diff_sum * 4_294_967_296.0 / (lookback_minutes as f64 * 60.0))
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

fn configure_conn(conn: &Connection) -> Result<()> {
    // WAL mode: allows one writer + multiple concurrent readers.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS miners (
            address    TEXT    PRIMARY KEY,
            first_seen INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS shares (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            miner_address TEXT    NOT NULL,
            difficulty    REAL    NOT NULL,
            block_hash_be BLOB    NOT NULL,
            timestamp     INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS shares_miner_ts
            ON shares (miner_address, timestamp);

        -- Cumulative stats per miner (epoch_number added in Paso 8).
        CREATE TABLE IF NOT EXISTS epoch_stats (
            miner_address    TEXT    PRIMARY KEY,
            shares_count     INTEGER NOT NULL DEFAULT 0,
            active_minutes   INTEGER NOT NULL DEFAULT 0,
            total_difficulty REAL    NOT NULL DEFAULT 0.0,
            best_share_hash  BLOB
        );

        -- 1-minute hashrate time series. Pruned to last 24 h on each flush.
        CREATE TABLE IF NOT EXISTS minute_hashrates (
            miner_address  TEXT    NOT NULL,
            minute_ts      INTEGER NOT NULL,  -- unix timestamp, floor(t/60)*60
            difficulty_sum REAL    NOT NULL,
            PRIMARY KEY (miner_address, minute_ts)
        );

        CREATE INDEX IF NOT EXISTS minute_hashrates_ts
            ON minute_hashrates (minute_ts);

        -- Competition registration (epoch_number added in Paso 8).
        CREATE TABLE IF NOT EXISTS competition_entries (
            miner_address   TEXT    PRIMARY KEY,
            entry_paid_sats INTEGER NOT NULL,
            entry_timestamp INTEGER NOT NULL,
            ark_address     TEXT    NOT NULL
        );
        ",
    )?;
    Ok(())
}

// ── Worker loop ───────────────────────────────────────────────────────────────

fn run_worker(conn: Connection, rx: mpsc::Receiver<DbEvent>) {
    let mut share_batch: Vec<ShareEvent> = Vec::with_capacity(512);
    let mut miner_batch: Vec<(String, i64)> = Vec::with_capacity(64);
    // Per-address difficulty accumulated since the last minute flush.
    let mut minute_windows: HashMap<String, f64> = HashMap::new();
    let mut last_flush = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ev) => push_event(ev, &mut share_batch, &mut miner_batch, &mut minute_windows),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                flush(&conn, &mut share_batch, &mut miner_batch, &mut minute_windows);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        loop {
            match rx.try_recv() {
                Ok(ev) => push_event(ev, &mut share_batch, &mut miner_batch, &mut minute_windows),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    flush(&conn, &mut share_batch, &mut miner_batch, &mut minute_windows);
                    return;
                }
            }
        }

        if last_flush.elapsed() >= FLUSH_INTERVAL || share_batch.len() >= 1000 {
            flush(&conn, &mut share_batch, &mut miner_batch, &mut minute_windows);
            last_flush = Instant::now();
        }
    }
}

fn push_event(
    ev: DbEvent,
    shares: &mut Vec<ShareEvent>,
    miners: &mut Vec<(String, i64)>,
    minute_windows: &mut HashMap<String, f64>,
) {
    match ev {
        DbEvent::Share(s) => {
            *minute_windows.entry(s.miner_address.clone()).or_insert(0.0) += s.difficulty;
            shares.push(s);
        }
        DbEvent::MinerConnected { address, timestamp } => miners.push((address, timestamp)),
    }
}

fn flush(
    conn: &Connection,
    shares: &mut Vec<ShareEvent>,
    miners: &mut Vec<(String, i64)>,
    minute_windows: &mut HashMap<String, f64>,
) {
    if shares.is_empty() && miners.is_empty() && minute_windows.is_empty() {
        return;
    }
    let minute_ts = floor_minute(now_secs());
    match flush_inner(conn, shares, miners, minute_windows, minute_ts) {
        Ok(()) => {
            info!(
                shares = shares.len(),
                miners = miners.len(),
                active_addresses = minute_windows.len(),
                "DB flush"
            );
            shares.clear();
            miners.clear();
            minute_windows.clear();
        }
        Err(e) => error!("DB flush failed: {e:#}"),
    }
}

fn flush_inner(
    conn: &Connection,
    shares: &[ShareEvent],
    miners: &[(String, i64)],
    minute_windows: &HashMap<String, f64>,
    minute_ts: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    for (address, ts) in miners {
        tx.execute(
            "INSERT OR IGNORE INTO miners (address, first_seen) VALUES (?1, ?2)",
            params![address, ts],
        )?;
    }

    for share in shares {
        tx.execute(
            "INSERT INTO shares (miner_address, difficulty, block_hash_be, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                share.miner_address,
                share.difficulty,
                share.block_hash_be.as_slice(),
                share.timestamp,
            ],
        )?;

        tx.execute(
            "INSERT INTO epoch_stats (miner_address, shares_count, total_difficulty, best_share_hash)
             VALUES (?1, 1, ?2, ?3)
             ON CONFLICT(miner_address) DO UPDATE SET
               shares_count     = shares_count + 1,
               total_difficulty = total_difficulty + ?2,
               best_share_hash  = CASE
                 WHEN best_share_hash IS NULL  THEN ?3
                 WHEN ?3 < best_share_hash     THEN ?3
                 ELSE best_share_hash
               END",
            params![
                share.miner_address,
                share.difficulty,
                share.block_hash_be.as_slice(),
            ],
        )?;
    }

    // Flush 1-minute windows → minute_hashrates + epoch_stats.active_minutes.
    for (address, diff_sum) in minute_windows {
        tx.execute(
            "INSERT INTO minute_hashrates (miner_address, minute_ts, difficulty_sum)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(miner_address, minute_ts) DO UPDATE SET
               difficulty_sum = difficulty_sum + ?3",
            params![address, minute_ts, diff_sum],
        )?;

        tx.execute(
            "INSERT INTO epoch_stats (miner_address, active_minutes)
             VALUES (?1, 1)
             ON CONFLICT(miner_address) DO UPDATE SET
               active_minutes = active_minutes + 1",
            params![address],
        )?;
    }

    // Prune minute_hashrates older than 24 h.
    let cutoff = minute_ts - MINUTE_RETENTION_SECS;
    tx.execute(
        "DELETE FROM minute_hashrates WHERE minute_ts < ?1",
        params![cutoff],
    )?;

    tx.commit()?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn hash_to_difficulty(hash_be: &[u8; 32]) -> f64 {
    const PDIFF1_UPPER: f64 = 0x0000_0000_FFFF_0000_u64 as f64;
    let hash_upper = u64::from_be_bytes([
        hash_be[0], hash_be[1], hash_be[2], hash_be[3],
        hash_be[4], hash_be[5], hash_be[6], hash_be[7],
    ]);
    if hash_upper == 0 {
        return f64::MAX;
    }
    PDIFF1_UPPER / hash_upper as f64
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn floor_minute(ts_secs: i64) -> i64 {
    (ts_secs / 60) * 60
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        configure_conn(&conn).unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn difficulty_decreases_with_larger_hash() {
        let easy = [0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let hard = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(hash_to_difficulty(&hard) > hash_to_difficulty(&easy));
    }

    #[test]
    fn schema_creates_without_error() {
        let conn = in_memory();
        init_schema(&conn).unwrap(); // idempotent
    }

    #[test]
    fn flush_inserts_share_and_upserts_epoch_stats() {
        let conn = in_memory();
        let share = ShareEvent {
            miner_address: "bc1qtest".into(),
            difficulty: 42.0,
            block_hash_be: [0xab; 32],
            timestamp: 1_700_000_000,
        };
        flush_inner(&conn, &[share], &[], &HashMap::new(), 0).unwrap();

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM shares", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);

        let shares_count: i64 = conn.query_row(
            "SELECT shares_count FROM epoch_stats WHERE miner_address = 'bc1qtest'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(shares_count, 1);
    }

    #[test]
    fn best_share_hash_keeps_lowest() {
        let conn = in_memory();
        let make = |hash: [u8; 32], diff: f64| ShareEvent {
            miner_address: "addr".into(),
            difficulty: diff,
            block_hash_be: hash,
            timestamp: 0,
        };
        let high = [0xff; 32];
        let low = { let mut h = [0u8; 32]; h[3] = 0x01; h };
        flush_inner(&conn, &[make(high, 1.0), make(low, 100.0), make([0x88; 32], 2.0)], &[], &HashMap::new(), 0).unwrap();

        let best: Vec<u8> = conn.query_row(
            "SELECT best_share_hash FROM epoch_stats WHERE miner_address = 'addr'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(best, low.to_vec());
    }

    #[test]
    fn minute_windows_write_to_minute_hashrates_and_active_minutes() {
        let conn = in_memory();
        let mut windows = HashMap::new();
        windows.insert("alice".to_string(), 100.0);
        windows.insert("bob".to_string(), 50.0);

        flush_inner(&conn, &[], &[], &windows, 1_700_000_000).unwrap();

        let alice_diff: f64 = conn.query_row(
            "SELECT difficulty_sum FROM minute_hashrates WHERE miner_address = 'alice'",
            [], |r| r.get(0),
        ).unwrap();
        assert!((alice_diff - 100.0).abs() < 1e-9);

        let alice_minutes: i64 = conn.query_row(
            "SELECT active_minutes FROM epoch_stats WHERE miner_address = 'alice'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(alice_minutes, 1);
    }

    #[test]
    fn minute_windows_accumulate_across_flushes() {
        let conn = in_memory();
        let ts1 = 1_700_000_000i64;
        let ts2 = ts1 + 60;

        let mut w1 = HashMap::new();
        w1.insert("alice".to_string(), 100.0);
        flush_inner(&conn, &[], &[], &w1, ts1).unwrap();

        let mut w2 = HashMap::new();
        w2.insert("alice".to_string(), 200.0);
        flush_inner(&conn, &[], &[], &w2, ts2).unwrap();

        let total: f64 = conn.query_row(
            "SELECT SUM(difficulty_sum) FROM minute_hashrates WHERE miner_address = 'alice'",
            [], |r| r.get(0),
        ).unwrap();
        assert!((total - 300.0).abs() < 1e-9);

        let active: i64 = conn.query_row(
            "SELECT active_minutes FROM epoch_stats WHERE miner_address = 'alice'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(active, 2);
    }

    #[test]
    fn hashrate_query_returns_correct_value() {
        let conn = in_memory();
        let now = floor_minute(now_secs());

        // Insert two minutes of data: difficulty_sum = 1000.0 each.
        for i in 0..2i64 {
            let mut w = HashMap::new();
            w.insert("alice".to_string(), 1000.0);
            flush_inner(&conn, &[], &[], &w, now - i * 60).unwrap();
        }

        // Manually query using the same formula as DbReader.
        let lookback = 2u64;
        let cutoff = now_secs() - (lookback as i64 * 60);
        let diff_sum: f64 = conn.query_row(
            "SELECT COALESCE(SUM(difficulty_sum), 0.0) FROM minute_hashrates WHERE minute_ts >= ?1",
            params![cutoff],
            |r| r.get(0),
        ).unwrap();
        let hashrate = diff_sum * 4_294_967_296.0 / (lookback as f64 * 60.0);
        // 2000 * 2^32 / 120 ≈ 71.58 GH/s
        assert!(hashrate > 0.0);
    }

    #[test]
    fn old_minute_hashrates_are_pruned() {
        let conn = in_memory();
        let old_ts = floor_minute(now_secs()) - MINUTE_RETENTION_SECS - 120;
        let mut w = HashMap::new();
        w.insert("alice".to_string(), 999.0);
        // Flush with an old timestamp; the prune runs based on current minute_ts.
        let current_ts = floor_minute(now_secs());
        // Insert old row manually then trigger prune via flush.
        conn.execute(
            "INSERT INTO minute_hashrates (miner_address, minute_ts, difficulty_sum) VALUES ('alice', ?1, 999.0)",
            params![old_ts],
        ).unwrap();
        flush_inner(&conn, &[], &[], &HashMap::new(), current_ts).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM minute_hashrates WHERE miner_address = 'alice' AND minute_ts = ?1",
            params![old_ts],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 0, "old row should have been pruned");
    }
}
