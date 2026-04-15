use std::time::Duration;

use pool::rpc::{RpcClient, TemplatePoller};

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
async fn template_poller_updates_on_new_block() {
    let client = regtest_client();
    let poller = TemplatePoller::start(client.clone()).await.expect("poller failed to start");
    let mut rx = poller.subscribe();

    let height_before = rx.borrow().height;

    // Mine one block to trigger a new template.
    let addr = client.get_new_address().await.expect("getnewaddress failed");
    client.generate_to_address(1, &addr).await.expect("generatetoaddress failed");

    // Wait for the poller to deliver the updated template (long-poll timeout is 120s,
    // but on regtest a new block triggers it immediately).
    tokio::time::timeout(Duration::from_secs(10), rx.changed())
        .await
        .expect("timed out waiting for template update")
        .expect("watch channel closed");

    let height_after = rx.borrow().height;
    assert_eq!(height_after, height_before + 1, "height must increment by 1");
}
