//! End-to-end block mining test via the Template Distribution Protocol.
//!
//! Flow:
//!   sv2-tp ──NewTemplate/SetNewPrevHash──▶ pool (template_client)
//!   pool builds coinbase, mines nonce
//!   pool ──SubmitSolution──▶ sv2-tp ──submitblock──▶ bitcoin-core
//!   assert block height increased
//!
//! Requires `just start-all` (bitcoind + sv2-tp) to be running.
//! Run with: `just int-mine`

use std::time::Duration;

use bitcoin::{
    block::{Header, Version as BlockVersion},
    consensus::deserialize,
    hashes::{sha256d, Hash},
    CompactTarget, TxMerkleNode,
};
use pool::{
    jobs::{build_sv2_coinbase_from_tdp, script_from_address, SV2_EXTRANONCE_TOTAL},
    rpc::RpcClient,
    template_client::{self, SubmitSolutionData},
};
use template_distribution_sv2::{NewTemplate, SetNewPrevHash};

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

/// Convert compact bits to a 32-byte big-endian target as a lowercase hex string,
/// matching the format of `header.block_hash().to_string()` for lexicographic comparison.
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

#[tokio::test]
async fn mine_block_from_sv2_template() {
    let client = regtest_client();

    // ── Connect to sv2-tp ────────────────────────────────────────────────────

    let pub_key = template_client::read_authority_pubkey(&datadir())
        .expect("read sv2_authority_key — run `just start-all` first");

    let (template_rx, solution_tx) = template_client::start(
        "127.0.0.1:18447".parse().unwrap(),
        pub_key,
        100,
    )
    .await
    .expect("connect to sv2-tp");

    let raw = template_rx.borrow().clone();

    // ── Parse TDP messages ───────────────────────────────────────────────────

    let mut nt_bytes = raw.new_template.clone();
    let mut snph_bytes = raw.set_new_prev_hash.clone();

    let nt: NewTemplate<'_> = binary_sv2::from_bytes(&mut nt_bytes)
        .unwrap_or_else(|e| panic!("parse NewTemplate: {e:?}"));
    let snph: SetNewPrevHash<'_> = binary_sv2::from_bytes(&mut snph_bytes)
        .unwrap_or_else(|e| panic!("parse SetNewPrevHash: {e:?}"));

    // ── Build coinbase from TDP data ─────────────────────────────────────────

    let pool_addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
    let miner_script = script_from_address(pool_addr).unwrap();
    let use_segwit = nt.coinbase_tx_outputs_count > 0;

    let parts = build_sv2_coinbase_from_tdp(
        nt.coinbase_prefix.inner_as_ref(),
        nt.coinbase_tx_version,
        nt.coinbase_tx_input_sequence,
        nt.coinbase_tx_value_remaining,
        nt.coinbase_tx_outputs_count,
        nt.coinbase_tx_outputs.inner_as_ref(),
        nt.coinbase_tx_locktime,
        miner_script,
        use_segwit,
    );

    // Full coinbase bytes with zero extranonce.
    let mut coinbase_bytes = parts.coinb1.clone();
    coinbase_bytes.extend_from_slice(&[0u8; SV2_EXTRANONCE_TOTAL]);
    coinbase_bytes.extend_from_slice(&parts.coinb2);

    // Verify it parses as a valid transaction.
    let coinbase_tx: bitcoin::Transaction = deserialize(&coinbase_bytes)
        .expect("coinbase must be a valid Bitcoin transaction");
    assert!(coinbase_tx.input[0].previous_output.is_null());

    // ── Compute merkle root ──────────────────────────────────────────────────
    // Block header merkle root uses TXIDs (non-witness hash), not wTXIDs.

    let mut hash: [u8; 32] = coinbase_tx.compute_txid().to_byte_array();
    for sibling in nt.merkle_path.inner_as_ref() {
        let mut data = [0u8; 64];
        data[..32].copy_from_slice(&hash);
        data[32..].copy_from_slice(sibling);
        hash = sha256d::Hash::hash(&data).to_byte_array();
    }
    let merkle_root = TxMerkleNode::from_byte_array(hash);

    // ── Build and mine block header ──────────────────────────────────────────

    let prev_bytes: [u8; 32] = snph.prev_hash.inner_as_ref()
        .try_into()
        .expect("prev_hash must be 32 bytes");
    let prev_blockhash = bitcoin::BlockHash::from_byte_array(prev_bytes);
    let target_hex = compact_to_target_hex(snph.n_bits);

    let mut header = Header {
        version: BlockVersion::from_consensus(nt.version as i32),
        prev_blockhash,
        merkle_root,
        time: snph.header_timestamp,
        bits: CompactTarget::from_consensus(snph.n_bits),
        nonce: 0,
    };

    for nonce in 0..=u32::MAX {
        header.nonce = nonce;
        if header.block_hash().to_string() <= target_hex {
            break;
        }
    }

    // ── Submit via SubmitSolution → sv2-tp reconstructs and submits block ────

    let height_before = client.get_block_count().await.unwrap();

    solution_tx
        .send(SubmitSolutionData {
            template_id: nt.template_id,
            version: header.version.to_consensus() as u32,
            header_timestamp: header.time,
            header_nonce: header.nonce,
            coinbase_tx: coinbase_bytes,
        })
        .await
        .expect("send SubmitSolution");

    // Give sv2-tp time to reconstruct and submit the block to bitcoin-core.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let height_after = client.get_block_count().await.unwrap();
    assert!(
        height_after > height_before,
        "block count must increase (before={height_before}, after={height_after})"
    );
}
