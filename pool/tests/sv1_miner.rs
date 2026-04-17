//! End-to-end SV1 mining test through the full stack.
//!
//! Flow:
//!   sv1_miner (this test) ──SV1──▶ translator_sv2 ──SV2──▶ our pool (Sv2Server)
//!   pool ──SubmitSolution──▶ sv2-tp ──submitblock──▶ bitcoin-core
//!   assert block height increased
//!
//! Requires `just start-all` (bitcoin-node + sv2-tp) before running.
//! Pool and translator are spawned in-process / as subprocess by this test.
//! Run with: `just int-sv1`

use std::{
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    process::{Child, Command, Stdio},
    time::Duration,
};

use bitcoin::hashes::{sha256d, Hash};
use pool::{
    rpc::RpcClient,
    stratum_sv2::{AuthorityKeypair, Sv2Server},
    template_client,
};
use secp256k1::{rand::thread_rng, Keypair, Secp256k1};
use serde_json::{json, Value};
use tokio::time::sleep;

// Port for our pool in this test (different from sv2_server.rs which uses 13334).
const POOL_PORT: u16 = 13335;
// Downstream port where translator listens for SV1 miners.
const SV1_PORT: u16 = 34255;

fn datadir() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".bitcoin-data")
        .to_string_lossy()
        .into_owned()
}

fn regtest_client() -> RpcClient {
    RpcClient::new("http://127.0.0.1:18443", "pool", "poolpass")
}

/// Generate a fresh authority keypair.
fn generate_keypair() -> ([u8; 32], [u8; 32]) {
    let secp = Secp256k1::new();
    let kp = Keypair::new(&secp, &mut thread_rng());
    let (xonly, _) = kp.x_only_public_key();
    (xonly.serialize(), kp.secret_key().secret_bytes())
}

/// Encode 32-byte hex key as base58 (no checksum).
fn hex_to_b58(hex: &str) -> String {
    const ALPHA: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let bytes = hex::decode(hex).expect("hex_to_b58: invalid hex");
    let mut digits = bytes;
    let mut result = Vec::<u8>::new();

    while !digits.iter().all(|&d| d == 0) {
        let mut carry = 0u32;
        for d in digits.iter_mut() {
            let val = carry * 256 + *d as u32;
            *d = (val / 58) as u8;
            carry = val % 58;
        }
        result.push(ALPHA[carry as usize]);
    }

    result.reverse();
    String::from_utf8(result).unwrap()
}

/// Write a temporary translator config and spawn translator_sv2 as a subprocess.
fn start_translator(pool_port: u16, authority_pubkey_hex: &str) -> Child {
    let pubkey_b58 = hex_to_b58(authority_pubkey_hex);

    let conf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".translator-test.toml");

    let conf = format!(
        r#"downstream_address = "0.0.0.0"
downstream_port = {SV1_PORT}
max_supported_version = 2
min_supported_version = 2
downstream_extranonce2_size = 4
user_identity = "test-miner"
aggregate_channels = false

[[upstreams]]
address = "127.0.0.1"
port = {pool_port}
authority_pubkey = "{pubkey_b58}"
"#
    );

    std::fs::write(&conf_path, &conf).expect("write translator config");

    Command::new("translator_sv2")
        .arg("-c")
        .arg(&conf_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn translator_sv2 — run from inside nix develop")
}

/// Wait until a TCP port is listening (poll every 200 ms, up to 15 s).
fn wait_for_port(port: u16) {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..75 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("port {port} did not become available within 15 s");
}

/// Send a JSON-RPC line and return the raw response line.
fn send_line(stream: &mut std::net::TcpStream, msg: &Value) -> String {
    let line = serde_json::to_string(msg).unwrap() + "\n";
    stream.write_all(line.as_bytes()).expect("SV1 write");
    stream.flush().expect("SV1 flush");
    String::new() // response read separately via BufReader
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// Full SV1→translator→pool→sv2-tp→bitcoin-core mining flow.
///
/// Verifies that a block mined by an SV1 client propagates through the entire
/// stack and is accepted by Bitcoin Core (block height increases).
#[tokio::test]
async fn sv1_mine_block_through_translator() {
    let rpc = regtest_client();

    // ── Start our pool ───────────────────────────────────────────────────────

    let (pub_key, priv_key) = generate_keypair();
    let authority = AuthorityKeypair { public: pub_key, private: priv_key };
    let pubkey_hex = hex::encode(pub_key);

    let tp_pubkey = template_client::read_authority_pubkey(&datadir())
        .expect("read sv2_authority_key — run `just start-all` first");
    let (template_rx, solution_tx) = template_client::start(
        "127.0.0.1:18447".parse().unwrap(),
        tp_pubkey,
        100,
    )
    .await
    .expect("connect to sv2-tp");

    let pool_addr: SocketAddr = format!("127.0.0.1:{POOL_PORT}").parse().unwrap();
    let pool_address = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq".to_string();
    let server = Sv2Server::new(authority, pool_addr, template_rx, pool_address, solution_tx);

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            eprintln!("SV2 server error: {e:#}");
        }
    });

    sleep(Duration::from_millis(200)).await;

    // ── Start translator ─────────────────────────────────────────────────────

    let mut translator = start_translator(POOL_PORT, &pubkey_hex);

    wait_for_port(SV1_PORT);

    // ── SV1 handshake ────────────────────────────────────────────────────────

    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{SV1_PORT}"))
        .expect("connect to translator SV1 port");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());

    // mining.subscribe
    send_line(
        &mut stream,
        &json!({"id": 1, "method": "mining.subscribe", "params": []}),
    );

    let mut sub_response = String::new();
    reader.read_line(&mut sub_response).expect("read subscribe response");
    let sub: Value = serde_json::from_str(sub_response.trim()).expect("parse subscribe response");
    assert_eq!(sub["id"], 1, "subscribe id mismatch");
    assert!(sub["error"].is_null(), "subscribe error: {}", sub["error"]);

    let extranonce1_hex = sub["result"][1].as_str().expect("extranonce1 missing").to_string();
    let extranonce2_size = sub["result"][2].as_u64().expect("extranonce2_size missing") as usize;
    assert_eq!(extranonce2_size, 4, "expected extranonce2_size=4");

    // mining.authorize
    send_line(
        &mut stream,
        &json!({"id": 2, "method": "mining.authorize", "params": ["test-miner", ""]}),
    );

    // Drain lines until we have an authorize response and a mining.notify.
    let mut auth_ok = false;
    let mut notify: Option<Value> = None;
    let mut difficulty: Option<f64> = None;

    for _ in 0..20 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if msg["id"] == 2 {
            auth_ok = msg["result"].as_bool().unwrap_or(false);
        } else if msg["method"] == "mining.set_difficulty" {
            difficulty = msg["params"][0].as_f64();
        } else if msg["method"] == "mining.notify" {
            notify = Some(msg);
            if auth_ok {
                break;
            }
        }
    }

    assert!(auth_ok, "mining.authorize failed");
    let notify = notify.expect("no mining.notify received");
    let _difficulty = difficulty.unwrap_or(1.0);

    // ── Parse mining.notify ──────────────────────────────────────────────────

    let params = &notify["params"];
    let job_id = params[0].as_str().expect("job_id").to_string();
    let prevhash_hex = params[1].as_str().expect("prevhash");
    let coinb1_hex = params[2].as_str().expect("coinb1");
    let coinb2_hex = params[3].as_str().expect("coinb2");
    let branches: Vec<String> = params[4]
        .as_array()
        .expect("merkle_branch")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let version_hex = params[5].as_str().expect("version");
    let nbits_hex = params[6].as_str().expect("nbits");
    let ntime_hex = params[7].as_str().expect("ntime");

    // Parse uint32 fields (big-endian hex from translator).
    let version = u32::from_str_radix(version_hex, 16).expect("version");
    let nbits   = u32::from_str_radix(nbits_hex,   16).expect("nbits");
    let ntime   = u32::from_str_radix(ntime_hex,   16).expect("ntime");

    // In Stratum v1 the prevhash bytes are swapped in 4-byte groups relative to
    // Bitcoin internal byte order.  Reverse each group to recover internal order.
    let prevhash_bytes = hex::decode(prevhash_hex).expect("prevhash hex");
    assert_eq!(prevhash_bytes.len(), 32, "prevhash must be 32 bytes");
    let mut prev_internal = [0u8; 32];
    for i in 0..8 {
        let g = &prevhash_bytes[i * 4..(i + 1) * 4];
        prev_internal[i * 4]     = g[3];
        prev_internal[i * 4 + 1] = g[2];
        prev_internal[i * 4 + 2] = g[1];
        prev_internal[i * 4 + 3] = g[0];
    }

    // ── Build coinbase ───────────────────────────────────────────────────────

    let extranonce1 = hex::decode(&extranonce1_hex).expect("extranonce1 hex");
    let extranonce2 = vec![0u8; extranonce2_size]; // all-zero extranonce2

    let mut coinbase = hex::decode(coinb1_hex).expect("coinb1 hex");
    coinbase.extend_from_slice(&extranonce1);
    coinbase.extend_from_slice(&extranonce2);
    coinbase.extend_from_slice(&hex::decode(coinb2_hex).expect("coinb2 hex"));

    let coinbase_tx: bitcoin::Transaction =
        bitcoin::consensus::deserialize(&coinbase).expect("coinbase must deserialize");
    assert!(coinbase_tx.input[0].previous_output.is_null(), "must be coinbase");

    // ── Compute merkle root ──────────────────────────────────────────────────

    let mut hash: [u8; 32] = coinbase_tx.compute_txid().to_byte_array();
    for branch_hex in &branches {
        let sibling = hex::decode(branch_hex).expect("branch hex");
        let mut data = [0u8; 64];
        data[..32].copy_from_slice(&hash);
        data[32..].copy_from_slice(&sibling);
        hash = sha256d::Hash::hash(&data).to_byte_array();
    }
    let merkle_root = bitcoin::TxMerkleNode::from_byte_array(hash);

    // ── Mine nonce ───────────────────────────────────────────────────────────

    let target_hex = compact_to_target_hex(nbits);
    let prev_blockhash = bitcoin::BlockHash::from_byte_array(prev_internal);

    let mut header = bitcoin::block::Header {
        version: bitcoin::block::Version::from_consensus(version as i32),
        prev_blockhash,
        merkle_root,
        time: ntime,
        bits: bitcoin::CompactTarget::from_consensus(nbits),
        nonce: 0,
    };

    let mut found = false;
    for nonce in 0..=u32::MAX {
        header.nonce = nonce;
        if header.block_hash().to_string() <= target_hex {
            found = true;
            break;
        }
    }
    assert!(found, "failed to find valid nonce (should be instant in regtest)");

    // ── Submit via SV1 mining.submit ─────────────────────────────────────────

    let height_before = rpc.get_block_count().await.unwrap();

    let ntime_submit = format!("{:08x}", header.time);
    let nonce_submit = format!("{:08x}", header.nonce);
    let extranonce2_hex = hex::encode(&extranonce2);

    send_line(
        &mut stream,
        &json!({
            "id": 4,
            "method": "mining.submit",
            "params": ["test-miner", job_id, extranonce2_hex, ntime_submit, nonce_submit]
        }),
    );

    // Give the full stack time to process.
    sleep(Duration::from_secs(4)).await;

    let height_after = rpc.get_block_count().await.unwrap();
    assert!(
        height_after > height_before,
        "block count must increase (before={height_before}, after={height_after})"
    );

    // ── Cleanup ──────────────────────────────────────────────────────────────

    translator.kill().ok();
}

/// Convert compact bits to a 32-byte big-endian target as a lowercase hex string.
fn compact_to_target_hex(bits: u32) -> String {
    let exp = (bits >> 24) as usize;
    let mantissa = bits & 0x007f_ffff;
    let mut be = [0u8; 32];
    if exp >= 3 && exp <= 32 {
        let i = 32 - exp;
        if i < 32     { be[i]   = ((mantissa >> 16) & 0xff) as u8; }
        if i + 1 < 32 { be[i+1] = ((mantissa >> 8)  & 0xff) as u8; }
        if i + 2 < 32 { be[i+2] = (mantissa          & 0xff) as u8; }
    }
    hex::encode(be)
}
