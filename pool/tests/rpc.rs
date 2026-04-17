use pool::rpc::RpcClient;

fn regtest_client() -> RpcClient {
    RpcClient::new(
        "http://127.0.0.1:18443",
        "pool",
        "poolpass",
    )
}

#[tokio::test]
async fn get_best_block_hash_returns_hex() {
    let client = regtest_client();
    let hash = client.get_best_block_hash().await.expect("RPC call failed");

    assert_eq!(hash.len(), 64, "block hash must be 64 hex chars");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "block hash must be hex: {hash}"
    );
}

#[tokio::test]
async fn get_block_template_fields_are_valid() {
    let client = regtest_client();
    let tmpl = client.get_block_template().await.expect("RPC call failed");

    assert!(tmpl.version > 0, "version must be positive");
    assert_eq!(tmpl.previousblockhash.len(), 64, "prevhash must be 64 hex chars");
    assert!(
        !tmpl.bits.is_empty(),
        "bits must not be empty"
    );
    assert_eq!(tmpl.target.len(), 64, "target must be 64 hex chars");
    assert!(tmpl.coinbasevalue > 0, "coinbasevalue must be positive");
    assert!(tmpl.curtime > 0, "curtime must be positive");
}

#[tokio::test]
async fn wrong_credentials_returns_error() {
    let client = RpcClient::new("http://127.0.0.1:18443", "wrong", "wrong");
    let result = client.get_best_block_hash().await;

    assert!(result.is_err(), "wrong credentials must return an error");
}

#[tokio::test]
async fn block_template_height_is_next_block() {
    let client = regtest_client();

    // Fetch both without mining so other concurrent tests don't skew the result.
    // getblocktemplate always targets the next block (tip + 1); allow for the
    // small race window where another test mines a block between the two calls.
    let blocks = client.get_block_count().await.expect("getblockchaininfo failed");
    let tmpl = client.get_block_template().await.expect("getblocktemplate failed");

    assert!(tmpl.height >= blocks + 1, "template height must be at least tip + 1");
}

#[tokio::test]
async fn block_template_transaction_fields_are_valid() {
    let client = regtest_client();
    let addr = client.get_new_address().await.expect("getnewaddress failed");

    // Mine 101 blocks so the first coinbase matures and we have spendable funds.
    client.generate_to_address(101, &addr).await.expect("generatetoaddress failed");

    // Send a transaction so the mempool is non-empty.
    client.send_to_address(&addr, 0.001).await.expect("sendtoaddress failed");

    let tmpl = client.get_block_template().await.expect("getblocktemplate failed");

    assert!(!tmpl.transactions.is_empty(), "template must contain the mempool tx");

    for tx in &tmpl.transactions {
        assert_eq!(tx.txid.len(), 64, "txid must be 64 hex chars");
        assert_eq!(tx.hash.len(), 64, "hash must be 64 hex chars");
        assert!(!tx.data.is_empty(), "tx data must not be empty");
        assert!(tx.fee >= 0, "fee must be non-negative");
        assert!(tx.weight > 0, "weight must be positive");
    }
}
