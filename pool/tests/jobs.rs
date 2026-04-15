use bitcoin::{
    block::{Header, Version as BlockVersion},
    consensus::{deserialize, serialize},
    hashes::{sha256d, Hash},
    CompactTarget, Transaction, TxMerkleNode,
};
use pool::{
    jobs::{build_stratum_job, EXTRANONCE1_SIZE, EXTRANONCE2_SIZE},
    rpc::RpcClient,
};

fn regtest_client() -> RpcClient {
    RpcClient::new("http://127.0.0.1:18443", "pool", "poolpass")
}

/// Reconstruct full coinbase bytes from the Stratum coinb1/coinb2 split.
fn assemble_coinbase(coinb1: &str, coinb2: &str) -> Vec<u8> {
    let mut coinbase = hex::decode(coinb1).unwrap();
    coinbase.extend_from_slice(&[0x00u8; EXTRANONCE1_SIZE]);
    coinbase.extend_from_slice(&[0x00u8; EXTRANONCE2_SIZE]);
    coinbase.extend_from_slice(&hex::decode(coinb2).unwrap());
    coinbase
}

/// Apply the Stratum merkle branch to a coinbase hash to get the merkle root.
fn apply_branch(coinbase_bytes: &[u8], branch: &[String]) -> TxMerkleNode {
    let mut hash: [u8; 32] = sha256d::Hash::hash(coinbase_bytes).to_byte_array();
    for sibling_hex in branch {
        let mut data = [0u8; 64];
        data[..32].copy_from_slice(&hash);
        data[32..].copy_from_slice(&hex::decode(sibling_hex).unwrap());
        hash = sha256d::Hash::hash(&data).to_byte_array();
    }
    TxMerkleNode::from_byte_array(hash)
}

/// Mine a valid nonce for the given header in regtest (near-zero difficulty).
fn mine_nonce(header: &mut Header, target: &str) -> u32 {
    for nonce in 0..=u32::MAX {
        header.nonce = nonce;
        // block_hash().to_string() gives display-format hex (big-endian).
        // template.target is also display-format hex — lexicographic comparison
        // on same-length lowercase hex strings is equivalent to numeric comparison.
        if header.block_hash().to_string().as_str() <= target {
            return nonce;
        }
    }
    panic!("no valid nonce found — is difficulty really set for regtest?");
}

#[tokio::test]
async fn job_produces_valid_block() {
    let client = regtest_client();
    let template = client.get_block_template().await.unwrap();

    // Build a job using a wallet address so the coinbase output is spendable.
    let miner_addr = client.get_new_address().await.unwrap();
    let job = build_stratum_job(&template, &miner_addr, "job-0", true).unwrap();

    // ── Reconstruct coinbase ──────────────────────────────────────────────────
    let coinbase_bytes = assemble_coinbase(&job.coinb1, &job.coinb2);

    // Verify it deserializes as a valid Bitcoin transaction before going further.
    let coinbase_tx: Transaction = deserialize(&coinbase_bytes)
        .expect("coinbase must be a valid Bitcoin transaction");

    assert!(
        coinbase_tx.input[0].previous_output.is_null(),
        "first input prevout must be null (coinbase marker)"
    );

    // ── Compute merkle root ───────────────────────────────────────────────────
    let merkle_root = apply_branch(&coinbase_bytes, &job.merkle_branch);

    // ── Mine ──────────────────────────────────────────────────────────────────
    let bits = u32::from_str_radix(&template.bits, 16)
        .expect("bits must be valid hex");

    let prevhash = template.previousblockhash.parse()
        .expect("previousblockhash must be a valid block hash");

    let mut header = Header {
        version: BlockVersion::from_consensus(template.version as i32),
        prev_blockhash: prevhash,
        merkle_root,
        time: template.curtime,
        bits: CompactTarget::from_consensus(bits),
        nonce: 0,
    };

    mine_nonce(&mut header, &template.target);

    // ── Assemble block ────────────────────────────────────────────────────────
    let mut txdata = vec![coinbase_tx];
    for tmpl_tx in &template.transactions {
        let tx: Transaction = deserialize(&hex::decode(&tmpl_tx.data).unwrap())
            .expect("template transaction must be valid");
        txdata.push(tx);
    }

    let block = bitcoin::Block { header, txdata };

    // ── Submit and verify ─────────────────────────────────────────────────────
    let height_before = template.height;
    client
        .submit_block(&hex::encode(serialize(&block)))
        .await
        .expect("submit_block must succeed");

    let tip = client.get_block_count().await.unwrap();
    assert_eq!(tip, height_before, "new block must be at the expected height");
}
