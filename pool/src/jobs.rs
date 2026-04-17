//! Job construction for Stratum V1.
//!
//! Unlike pools that use a single pool address for all miners, this pool builds
//! the coinbase output with each miner's own Bitcoin address (provided as the
//! `mining.authorize` username). `coinb2` — which contains the payout output —
//! is therefore different for every connected miner.
//!
//! The merkle branch depends only on the block template and is shared across
//! all miners for the same template.
//!
//! # Coinbase split
//! Stratum V1 splits the coinbase around the extranonce:
//!
//!   full_coinbase = coinb1 || extranonce1 || extranonce2 || coinb2
//!
//! We build the full serialized coinbase with `bitcoin::Transaction`, locate
//! the extranonce boundary, and slice.

use anyhow::{Context, Result};
use bitcoin::{
    address::NetworkUnchecked,
    blockdata::script::{Builder, PushBytesBuf},
    consensus::serialize,
    hashes::{sha256d, Hash},
    transaction::Version,
    Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
};
use std::str::FromStr;

use crate::rpc::{BlockTemplate, TemplateTransaction};

// ── Constants ──────────────────────────────────────────────────────────────────

pub const EXTRANONCE1_SIZE: usize = 4;
pub const EXTRANONCE2_SIZE: usize = 4;

const POOL_TAG: &[u8] = b"/lottery-pool/";

// Placeholder bytes written into the coinbase script where extranonce will go.
// Must be distinct from any valid script data so we can locate them to split.
const EXTRANONCE_PLACEHOLDER: [u8; EXTRANONCE1_SIZE + EXTRANONCE2_SIZE] =
    [0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];

// ── Public types ───────────────────────────────────────────────────────────────

/// Coinbase transaction split at the extranonce position.
///
/// Full coinbase bytes = `coinb1 || extranonce1 || extranonce2 || coinb2`
#[derive(Debug, Clone)]
pub struct CoinbaseParts {
    pub coinb1: Vec<u8>,
    pub coinb2: Vec<u8>,
}

/// A job ready to be sent to a miner via `mining.notify`.
#[derive(Debug, Clone)]
pub struct StratumJob {
    pub job_id: String,
    /// Previous block hash: full 32-byte reversal of the RPC display format.
    pub prevhash: String,
    pub coinb1: String,
    pub coinb2: String,
    /// Sibling hashes at each merkle level, as lowercase hex.
    pub merkle_branch: Vec<String>,
    /// Block version as big-endian hex (8 chars).
    pub version: String,
    /// Compact target bits as big-endian hex (8 chars).
    pub nbits: String,
    /// Current time as big-endian hex (8 chars).
    pub ntime: String,
    pub clean_jobs: bool,
}

// ── Coinbase construction ──────────────────────────────────────────────────────

/// Build `coinb1` and `coinb2` for a specific miner using `bitcoin::Transaction`.
///
/// We serialize the full transaction with a placeholder where extranonce goes,
/// locate the placeholder in the bytes, and split there.
pub fn build_coinbase_parts(
    height: u32,
    coinbase_value: i64,
    miner_script: ScriptBuf,
    witness_commitment_script: Option<ScriptBuf>,
) -> CoinbaseParts {
    let tx = build_coinbase_tx(height, coinbase_value, miner_script, witness_commitment_script);
    let raw = serialize(&tx);
    split_at_extranonce(raw)
}

fn build_coinbase_tx(
    height: u32,
    coinbase_value: i64,
    miner_script: ScriptBuf,
    witness_commitment_script: Option<ScriptBuf>,
) -> Transaction {
    // Coinbase script: BIP34 height + pool tag + extranonce placeholder
    let coinbase_script = build_coinbase_script(height);

    let input = TxIn {
        previous_output: OutPoint::null(), // all zeros — marks this as coinbase
        script_sig: coinbase_script,
        sequence: Sequence::MAX,
        witness: Witness::default(),
    };

    let miner_output = TxOut {
        value: Amount::from_sat(coinbase_value as u64),
        script_pubkey: miner_script,
    };

    let mut outputs = vec![miner_output];

    if let Some(wc_script) = witness_commitment_script {
        outputs.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: wc_script,
        });
    }

    Transaction {
        version: Version::ONE,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![input],
        output: outputs,
    }
}

fn build_coinbase_script(height: u32) -> ScriptBuf {
    let pool_tag = PushBytesBuf::try_from(POOL_TAG.to_vec())
        .expect("pool tag is within push size limit");
    let placeholder = PushBytesBuf::try_from(EXTRANONCE_PLACEHOLDER.to_vec())
        .expect("extranonce placeholder is within push size limit");

    Builder::new()
        .push_int(height as i64)    // BIP34 height
        .push_slice(pool_tag.as_ref())    // pool identifier
        .push_slice(placeholder.as_ref()) // extranonce1 + extranonce2 will go here
        .into_script()
}

/// Find the extranonce placeholder in the serialized coinbase and split.
fn split_at_extranonce(raw: Vec<u8>) -> CoinbaseParts {
    let pos = raw
        .windows(EXTRANONCE_PLACEHOLDER.len())
        .position(|w| w == EXTRANONCE_PLACEHOLDER)
        .expect("extranonce placeholder not found in serialized coinbase");

    CoinbaseParts {
        coinb1: raw[..pos].to_vec(),
        coinb2: raw[pos + EXTRANONCE_PLACEHOLDER.len()..].to_vec(),
    }
}

// ── Merkle branch ──────────────────────────────────────────────────────────────

/// Build the sibling hashes at each merkle level needed to compute the merkle
/// root from the coinbase hash.
///
/// The coinbase is NOT included — it is computed per-miner from coinb1/coinb2.
pub fn build_merkle_branch(transactions: &[TemplateTransaction]) -> Vec<[u8; 32]> {
    if transactions.is_empty() {
        return vec![];
    }

    // getblocktemplate txids are in display format (byte-reversed from internal byte order).
    // The merkle tree is built on internal byte order — reverse each txid.
    let mut level: Vec<[u8; 32]> = transactions
        .iter()
        .map(|tx| {
            let mut h = [0u8; 32];
            let mut b = hex::decode(&tx.txid).expect("txid is not valid hex");
            b.reverse();
            h.copy_from_slice(&b);
            h
        })
        .collect();

    let mut branch: Vec<[u8; 32]> = Vec::new();

    // At each iteration, level[0] is the sibling of the coinbase path.
    // The next level is built by merging level[1..] in pairs (not level[0..]),
    // because level[0] was consumed as the sibling and the coinbase path moves
    // one step up by merging with level[0].
    loop {
        branch.push(level[0]);
        if level.len() == 1 {
            break;
        }
        let mut next = Vec::new();
        let mut i = 1; // start at 1: level[0] was the sibling, skip it
        while i < level.len() {
            let a = &level[i];
            let b = if i + 1 < level.len() { &level[i + 1] } else { a };
            next.push(sha256d_pair(a, b));
            i += 2;
        }
        level = next;
    }

    branch
}

fn sha256d_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(a);
    data[32..].copy_from_slice(b);
    *sha256d::Hash::hash(&data).as_byte_array()
}

// ── Address helpers ────────────────────────────────────────────────────────────

/// Parse any Bitcoin address and return its `scriptPubKey`.
pub fn script_from_address(address: &str) -> Result<ScriptBuf> {
    let addr: Address<NetworkUnchecked> = Address::from_str(address)
        .with_context(|| format!("invalid bitcoin address: {address}"))?;
    Ok(addr.assume_checked().script_pubkey())
}

/// Parse the witness commitment hex from `getblocktemplate` into a scriptPubKey.
pub fn witness_commitment_script(hex: &str) -> ScriptBuf {
    let bytes = hex::decode(hex).expect("witness commitment is not valid hex");
    ScriptBuf::from_bytes(bytes)
}

// ── SV2 extranonce constants ───────────────────────────────────────────────────

/// Total bytes occupied by the extranonce area in an SV2 extended-channel coinbase.
///
/// SV2 layout:  coinbase_tx_prefix || extranonce_prefix(32) || extranonce2(4) || coinbase_tx_suffix
pub const SV2_EXTRANONCE_PREFIX_SIZE: usize = 32;
pub const SV2_EXTRANONCE2_SIZE: usize = 4;
pub const SV2_EXTRANONCE_TOTAL: usize = SV2_EXTRANONCE_PREFIX_SIZE + SV2_EXTRANONCE2_SIZE;

const SV2_EXTRANONCE_PLACEHOLDER: [u8; SV2_EXTRANONCE_TOTAL] = {
    let mut b = [0u8; SV2_EXTRANONCE_TOTAL];
    // Recognisable byte pattern so we can locate the placeholder after serialization.
    let mut i = 0;
    while i < SV2_EXTRANONCE_TOTAL {
        b[i] = 0xcc;
        i += 1;
    }
    b
};

/// Build coinbase parts for an SV2 extended channel.
///
/// The extranonce area is 36 bytes (32-byte prefix + 4-byte miner extranonce2).
/// Full coinbase = `coinbase_tx_prefix || extranonce_prefix(32) || extranonce2(4) || coinbase_tx_suffix`
pub fn build_sv2_coinbase_parts(
    height: u32,
    coinbase_value: i64,
    miner_script: ScriptBuf,
    witness_commitment_script: Option<ScriptBuf>,
) -> CoinbaseParts {
    let tx = build_coinbase_tx_sv2(height, coinbase_value, miner_script, witness_commitment_script);
    let raw = serialize(&tx);
    split_at_sv2_extranonce(raw)
}

fn build_coinbase_tx_sv2(
    height: u32,
    coinbase_value: i64,
    miner_script: ScriptBuf,
    witness_commitment_script: Option<ScriptBuf>,
) -> Transaction {
    let placeholder = PushBytesBuf::try_from(SV2_EXTRANONCE_PLACEHOLDER.to_vec())
        .expect("SV2 extranonce placeholder is within push size limit");
    let pool_tag = PushBytesBuf::try_from(POOL_TAG.to_vec())
        .expect("pool tag is within push size limit");

    let coinbase_script = Builder::new()
        .push_int(height as i64)
        .push_slice(pool_tag.as_ref())
        .push_slice(placeholder.as_ref())
        .into_script();

    let input = TxIn {
        previous_output: OutPoint::null(),
        script_sig: coinbase_script,
        sequence: Sequence::MAX,
        witness: Witness::default(),
    };

    let miner_output = TxOut {
        value: Amount::from_sat(coinbase_value as u64),
        script_pubkey: miner_script,
    };

    let mut outputs = vec![miner_output];
    if let Some(wc) = witness_commitment_script {
        outputs.push(TxOut { value: Amount::ZERO, script_pubkey: wc });
    }

    Transaction {
        version: Version::ONE,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![input],
        output: outputs,
    }
}

fn split_at_sv2_extranonce(raw: Vec<u8>) -> CoinbaseParts {
    let pos = raw
        .windows(SV2_EXTRANONCE_PLACEHOLDER.len())
        .position(|w| w == SV2_EXTRANONCE_PLACEHOLDER)
        .expect("SV2 extranonce placeholder not found in serialized coinbase");
    CoinbaseParts {
        coinb1: raw[..pos].to_vec(),
        coinb2: raw[pos + SV2_EXTRANONCE_PLACEHOLDER.len()..].to_vec(),
    }
}

// ── TDP coinbase construction ──────────────────────────────────────────────────

/// Build `coinbase_tx_prefix` / `coinbase_tx_suffix` for `NewExtendedMiningJob`
/// from the fields of a `template_distribution_sv2::NewTemplate`.
///
/// Layout of the assembled coinbase transaction:
/// ```text
/// coinbase_tx_prefix
///     tx_version (4 B LE)  |  varint(1)  |  outpoint (36 B zero)
///     varint(script_len)   |  coinbase_script_prefix
/// ── extranonce goes here (SV2_EXTRANONCE_TOTAL bytes) ──
/// coinbase_tx_suffix
///     input_sequence (4 B LE)
///     varint(1 + coinbase_tx_outputs_count)
///     pool payout output (bitcoin serialization)
///     coinbase_tx_outputs_bytes  (witness commitment, etc. from sv2-tp)
///     locktime (4 B LE)
/// ```
pub fn build_sv2_coinbase_from_tdp(
    coinbase_script_prefix: &[u8],
    coinbase_tx_version: u32,
    coinbase_tx_input_sequence: u32,
    coinbase_tx_value_remaining: u64,
    coinbase_tx_outputs_count: u32,
    coinbase_tx_outputs_bytes: &[u8],
    coinbase_tx_locktime: u32,
    miner_script: ScriptBuf,
) -> CoinbaseParts {
    let script_total_len = coinbase_script_prefix.len() + SV2_EXTRANONCE_TOTAL;

    // ── prefix: everything up to (but not including) the extranonce ──
    let mut prefix = Vec::new();
    prefix.extend_from_slice(&coinbase_tx_version.to_le_bytes());
    prefix.push(1u8); // vin count
    prefix.extend_from_slice(&[0u8; 32]); // prevout hash (all zero = coinbase)
    prefix.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // prevout index
    write_varint(&mut prefix, script_total_len as u64);
    prefix.extend_from_slice(coinbase_script_prefix);

    // ── suffix: input sequence, outputs, locktime ──
    let mut suffix = Vec::new();
    suffix.extend_from_slice(&coinbase_tx_input_sequence.to_le_bytes());

    // output count: 1 (pool payout) + however many sv2-tp added
    write_varint(&mut suffix, 1 + coinbase_tx_outputs_count as u64);

    // pool payout output serialized with bitcoin crate
    let payout = TxOut {
        value: Amount::from_sat(coinbase_tx_value_remaining),
        script_pubkey: miner_script,
    };
    suffix.extend_from_slice(&serialize(&payout));

    // sv2-tp's additional outputs (e.g. witness commitment OP_RETURN)
    suffix.extend_from_slice(coinbase_tx_outputs_bytes);
    suffix.extend_from_slice(&coinbase_tx_locktime.to_le_bytes());

    CoinbaseParts { coinb1: prefix, coinb2: suffix }
}

fn write_varint(buf: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        buf.push(n as u8);
    } else if n <= 0xffff {
        buf.push(0xfd);
        buf.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        buf.push(0xfe);
        buf.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        buf.push(0xff);
        buf.extend_from_slice(&n.to_le_bytes());
    }
}

// ── StratumJob assembly ────────────────────────────────────────────────────────

pub fn build_stratum_job(
    template: &BlockTemplate,
    miner_address: &str,
    job_id: &str,
    clean_jobs: bool,
) -> Result<StratumJob> {
    let miner_script = script_from_address(miner_address)?;
    let wc_script = template
        .default_witness_commitment
        .as_deref()
        .map(witness_commitment_script);

    let parts = build_coinbase_parts(
        template.height,
        template.coinbasevalue,
        miner_script,
        wc_script,
    );

    let branch = build_merkle_branch(&template.transactions);

    // Stratum V1 prevhash: split the display-format hash into 4-byte words and
    // reverse the bytes within each word (little-endian 32-bit groups).
    // This differs from a full 32-byte reversal (which gives internal byte order).
    let mut prevhash_bytes = hex::decode(&template.previousblockhash)
        .expect("previousblockhash is not valid hex");
    for chunk in prevhash_bytes.chunks_mut(4) {
        chunk.reverse();
    }
    let prevhash = hex::encode(&prevhash_bytes);

    Ok(StratumJob {
        job_id: job_id.to_string(),
        prevhash,
        coinb1: hex::encode(&parts.coinb1),
        coinb2: hex::encode(&parts.coinb2),
        merkle_branch: branch.iter().map(hex::encode).collect(),
        version: format!("{:08x}", template.version),
        nbits: template.bits.clone(),
        ntime: format!("{:08x}", template.curtime),
        clean_jobs,
    })
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Valid mainnet bech32 address; assume_checked() skips network validation.
    const REGTEST_ADDR: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";

    fn dummy_script() -> ScriptBuf {
        script_from_address(REGTEST_ADDR).unwrap()
    }

    // ── coinbase split ────────────────────────────────────────────────────────

    #[test]
    fn coinbase_splits_at_extranonce() {
        let parts = build_coinbase_parts(100, 5_000_000_000, dummy_script(), None);

        // Reassembled coinbase must not contain the placeholder
        let extranonce1 = [0xaau8; EXTRANONCE1_SIZE];
        let extranonce2 = [0xbbu8; EXTRANONCE2_SIZE];
        let mut full = parts.coinb1.clone();
        full.extend_from_slice(&extranonce1);
        full.extend_from_slice(&extranonce2);
        full.extend_from_slice(&parts.coinb2);

        // Placeholder must be gone
        assert!(!full.windows(EXTRANONCE_PLACEHOLDER.len())
            .any(|w| w == EXTRANONCE_PLACEHOLDER));
    }

    #[test]
    fn coinbase_roundtrip_deserializes() {
        let parts = build_coinbase_parts(1, 5_000_000_000, dummy_script(), None);

        let extranonce1 = [0x00u8; EXTRANONCE1_SIZE];
        let extranonce2 = [0x00u8; EXTRANONCE2_SIZE];
        let mut full = parts.coinb1.clone();
        full.extend_from_slice(&extranonce1);
        full.extend_from_slice(&extranonce2);
        full.extend_from_slice(&parts.coinb2);

        // Must deserialize as a valid Bitcoin transaction
        let tx: Transaction =
            bitcoin::consensus::deserialize(&full).expect("coinbase must be a valid transaction");

        assert_eq!(tx.input.len(), 1);
        assert!(tx.input[0].previous_output.is_null());
    }

    #[test]
    fn coinbase_with_witness_commitment_has_two_outputs() {
        let wc_hex = "6a24aa21a9ed".to_string() + &"00".repeat(32);
        let wc = witness_commitment_script(&wc_hex);
        let parts = build_coinbase_parts(1, 5_000_000_000, dummy_script(), Some(wc));

        let extranonce = [0u8; EXTRANONCE1_SIZE + EXTRANONCE2_SIZE];
        let mut full = parts.coinb1.clone();
        full.extend_from_slice(&extranonce);
        full.extend_from_slice(&parts.coinb2);

        let tx: Transaction = bitcoin::consensus::deserialize(&full).unwrap();
        assert_eq!(tx.output.len(), 2);
        assert_eq!(tx.output[1].value, Amount::ZERO);
    }

    // ── merkle branch ─────────────────────────────────────────────────────────

    fn fake_tx(byte: u8) -> TemplateTransaction {
        TemplateTransaction {
            txid: format!("{byte:02x}{:0<62}", ""),
            hash: format!("{byte:02x}{:0<62}", ""),
            data: "00".to_string(),
            fee: 0,
            weight: 400,
        }
    }

    #[test]
    fn merkle_branch_empty() {
        assert!(build_merkle_branch(&[]).is_empty());
    }

    #[test]
    fn merkle_branch_one_tx() {
        let branch = build_merkle_branch(&[fake_tx(0xaa)]);
        assert_eq!(branch.len(), 1);
        // txid "aa00...00" is reversed to internal byte order → 0xaa is at position 31
        assert_eq!(branch[0][31], 0xaa);
    }

    #[test]
    fn merkle_branch_two_txs_has_two_levels() {
        let branch = build_merkle_branch(&[fake_tx(0x01), fake_tx(0x02)]);
        assert_eq!(branch.len(), 2);
    }

    /// Mainnet block 250000 (156 transactions). Fixture from mempool.space.
    /// Verifies that build_merkle_branch + branch application reproduces the
    /// header's merkle root for a real block with many transactions.
    #[test]
    fn merkle_branch_matches_block_250000() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            merkle_root: String,
            coinbase_txid: String,
            txids: Vec<String>, // non-coinbase txids, as getblocktemplate returns
        }

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/block_250000.json");
        let fixture: Fixture =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

        let transactions: Vec<TemplateTransaction> = fixture
            .txids
            .iter()
            .map(|txid| TemplateTransaction {
                txid: txid.clone(),
                hash: txid.clone(),
                data: "00".to_string(),
                fee: 0,
                weight: 0,
            })
            .collect();

        let branch = build_merkle_branch(&transactions);

        // Start from coinbase hash in internal byte order (display format reversed).
        let mut hash = {
            let mut b = hex::decode(&fixture.coinbase_txid).unwrap();
            b.reverse();
            let mut h = [0u8; 32];
            h.copy_from_slice(&b);
            h
        };

        // Apply branch: merkle_root = sha256d(current || sibling) at each level.
        for sibling in &branch {
            let mut data = [0u8; 64];
            data[..32].copy_from_slice(&hash);
            data[32..].copy_from_slice(sibling);
            hash = sha256d::Hash::hash(&data).to_byte_array();
        }

        // Reverse to display format for comparison against the known merkle root.
        let root_display: String = hash.iter().rev().map(|b| format!("{b:02x}")).collect();
        assert_eq!(root_display, fixture.merkle_root);
    }
}
