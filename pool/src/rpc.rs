use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct RpcClient {
    client: Client,
    url: String,
    user: String,
    pass: String,
}

impl RpcClient {
    pub fn new(url: &str, user: &str, pass: &str) -> Self {
        RpcClient {
            client: Client::new(),
            url: url.to_string(),
            user: user.to_string(),
            pass: pass.to_string(),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.do_call(&self.client, method, params).await
    }

    async fn do_call(&self, client: &Client, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "1.0",
            "id": "pool",
            "method": method,
            "params": params
        });

        let resp = client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&body)
            .send()
            .await
            .context("Failed to connect to Bitcoin node")?;

        let json: Value = resp.json().await.context("RPC response is not valid JSON")?;

        if let Some(err) = json.get("error").filter(|e| !e.is_null()) {
            bail!("RPC error in {method}: {err}");
        }

        json.get("result")
            .cloned()
            .context("RPC response missing 'result' field")
    }

    pub async fn get_block_template(&self) -> Result<BlockTemplate> {
        let params = json!([{ "rules": ["segwit"] }]);
        let result = self.call("getblocktemplate", params).await?;
        let tmpl: BlockTemplate =
            serde_json::from_value(result).context("Failed to parse BlockTemplate")?;
        tmpl.assert_invariants();
        Ok(tmpl)
    }

    /// Returns null on success, or an error string if the block was rejected.
    pub async fn submit_block(&self, block_hex: &str) -> Result<()> {
        let params = json!([block_hex]);
        let result = self.call("submitblock", params).await?;
        if result.is_null() {
            Ok(())
        } else {
            bail!("submitblock rejected: {result}");
        }
    }

    pub async fn get_best_block_hash(&self) -> Result<String> {
        let result = self.call("getbestblockhash", json!([])).await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .context("getbestblockhash did not return a string")
    }

    /// Mine `n` blocks to `address`.  Regtest only — used in integration tests.
    pub async fn generate_to_address(&self, n: u32, address: &str) -> Result<Vec<String>> {
        let params = json!([n, address]);
        let result = self.call("generatetoaddress", params).await?;
        serde_json::from_value(result).context("Failed to parse generatetoaddress response")
    }

    /// Get a new bech32 address from the node wallet.  Regtest only.
    pub async fn get_new_address(&self) -> Result<String> {
        let result = self.call("getnewaddress", json!([])).await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .context("getnewaddress did not return a string")
    }

    /// Send `amount_btc` to `address` and return the txid.  Regtest only.
    pub async fn send_to_address(&self, address: &str, amount_btc: f64) -> Result<String> {
        let result = self.call("sendtoaddress", json!([address, amount_btc])).await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .context("sendtoaddress did not return a txid")
    }

    /// Return the current `blocks` count from `getblockchaininfo`.
    pub async fn get_block_count(&self) -> Result<u32> {
        let result = self.call("getblockchaininfo", json!([])).await?;
        result
            .get("blocks")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .context("getblockchaininfo missing 'blocks' field")
    }
}

// ── getblocktemplate response types ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct BlockTemplate {
    pub version: u32,
    pub previousblockhash: String,
    pub transactions: Vec<TemplateTransaction>,
    /// Total value available for the coinbase output (subsidy + fees), in satoshis.
    pub coinbasevalue: i64,
    /// Compact target bits (e.g. "207fffff" on regtest).
    pub bits: String,
    pub height: u32,
    pub curtime: u32,
    /// Full 256-bit target in hex.
    pub target: String,
    #[serde(default)]
    pub default_witness_commitment: Option<String>,
    /// Used for long-polling: resend getblocktemplate when this changes.
    pub longpollid: String,
}

impl BlockTemplate {
    fn assert_invariants(&self) {
        debug_assert_eq!(self.previousblockhash.len(), 64, "prevhash must be 64 hex chars");
        debug_assert_eq!(self.target.len(), 64, "target must be 64 hex chars");
        debug_assert!(!self.bits.is_empty(), "bits must not be empty");
        debug_assert!(!self.longpollid.is_empty(), "longpollid must not be empty");
        debug_assert!(self.coinbasevalue > 0, "coinbasevalue must be positive");
        debug_assert!(self.curtime > 0, "curtime must be positive");
        debug_assert!(self.version > 0, "version must be positive");
        for tx in &self.transactions {
            tx.assert_invariants();
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateTransaction {
    /// Raw serialized transaction in hex.
    pub data: String,
    pub txid: String,
    /// Witness txid (equals txid when there is no witness data).
    pub hash: String,
    pub fee: i64,
    pub weight: u32,
}

impl TemplateTransaction {
    fn assert_invariants(&self) {
        debug_assert_eq!(self.txid.len(), 64, "txid must be 64 hex chars");
        debug_assert_eq!(self.hash.len(), 64, "hash must be 64 hex chars");
        debug_assert!(!self.data.is_empty(), "tx data must not be empty");
        debug_assert!(self.fee >= 0, "fee must be non-negative");
        debug_assert!(self.weight > 0, "weight must be positive");
    }
}
