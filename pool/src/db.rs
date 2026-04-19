//! SQLite persistence layer.
//!
//! All writes go through a dedicated `std::thread` (the DB worker) that owns the
//! `Connection`. The hot path (share validation, ACK) sends a `DbEvent` over an
//! unbounded channel and returns immediately — the worker batches inserts and
//! flushes every 60 seconds in a single SQLite transaction.

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
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

// ── Worker ────────────────────────────────────────────────────────────────────

const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Owns the SQLite connection and processes `DbEvent`s in a background thread.
///
/// Clone the sender (via [`DbWorker::sender`]) to emit events from the hot path.
pub struct DbWorker {
    tx: mpsc::Sender<DbEvent>,
}

impl DbWorker {
    /// Open (or create) the database at `path` and start the background worker.
    pub fn start(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
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

// ── Schema ────────────────────────────────────────────────────────────────────

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

        -- One row per miner: updated on each accepted share.
        -- epoch_number and competition logic added in Paso 8.
        CREATE TABLE IF NOT EXISTS epoch_stats (
            miner_address    TEXT PRIMARY KEY,
            shares_count     INTEGER NOT NULL DEFAULT 0,
            total_difficulty REAL    NOT NULL DEFAULT 0.0,
            best_share_hash  BLOB
        );

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
    let mut last_flush = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ev) => push_event(ev, &mut share_batch, &mut miner_batch),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                flush(&conn, &mut share_batch, &mut miner_batch);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        loop {
            match rx.try_recv() {
                Ok(ev) => push_event(ev, &mut share_batch, &mut miner_batch),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    flush(&conn, &mut share_batch, &mut miner_batch);
                    return;
                }
            }
        }

        if last_flush.elapsed() >= FLUSH_INTERVAL || share_batch.len() >= 1000 {
            flush(&conn, &mut share_batch, &mut miner_batch);
            last_flush = Instant::now();
        }
    }
}

fn push_event(ev: DbEvent, shares: &mut Vec<ShareEvent>, miners: &mut Vec<(String, i64)>) {
    match ev {
        DbEvent::Share(s) => shares.push(s),
        DbEvent::MinerConnected { address, timestamp } => miners.push((address, timestamp)),
    }
}

fn flush(conn: &Connection, shares: &mut Vec<ShareEvent>, miners: &mut Vec<(String, i64)>) {
    if shares.is_empty() && miners.is_empty() {
        return;
    }
    match flush_inner(conn, shares, miners) {
        Ok(()) => {
            info!(shares = shares.len(), miners = miners.len(), "DB flush");
            shares.clear();
            miners.clear();
        }
        Err(e) => error!("DB flush failed: {e:#}"),
    }
}

fn flush_inner(
    conn: &Connection,
    shares: &[ShareEvent],
    miners: &[(String, i64)],
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

        // Only update best_share_hash when this share beats the current best.
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

    tx.commit()?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Approximate share difficulty from the block hash (big-endian).
///
/// Uses the upper 8 bytes for an f64 approximation sufficient for hashrate
/// estimation. For competition winner determination, compare `block_hash_be`
/// bytes directly (lower = harder).
pub fn hash_to_difficulty(hash_be: &[u8; 32]) -> f64 {
    // pdiff1 upper 8 bytes (BE): 0x00000000_FFFF0000
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_decreases_with_larger_hash() {
        let easy = [0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let hard = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(hash_to_difficulty(&hard) > hash_to_difficulty(&easy));
    }

    #[test]
    fn schema_creates_without_error() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap(); // idempotent
    }

    #[test]
    fn flush_inserts_share_and_upserts_epoch_stats() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        let share = ShareEvent {
            miner_address: "bc1qtest".into(),
            difficulty: 42.0,
            block_hash_be: [0xab; 32],
            timestamp: 1_700_000_000,
        };
        flush_inner(&conn, &[share], &[]).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM shares", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (shares_count, best): (i64, Vec<u8>) = conn
            .query_row(
                "SELECT shares_count, best_share_hash FROM epoch_stats
                 WHERE miner_address = 'bc1qtest'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(shares_count, 1);
        assert_eq!(best, vec![0xab; 32]);
    }

    #[test]
    fn best_share_hash_keeps_lowest() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        let make = |hash: [u8; 32], diff: f64| ShareEvent {
            miner_address: "addr".into(),
            difficulty: diff,
            block_hash_be: hash,
            timestamp: 0,
        };

        let high = [0xff; 32];
        let low = {
            let mut h = [0u8; 32];
            h[3] = 0x01;
            h
        };
        flush_inner(&conn, &[make(high, 1.0), make(low, 100.0), make([0x88; 32], 2.0)], &[]).unwrap();

        let best: Vec<u8> = conn
            .query_row(
                "SELECT best_share_hash FROM epoch_stats WHERE miner_address = 'addr'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(best, low.to_vec());
    }
}
