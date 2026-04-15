use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub rpc_user: String,
    pub rpc_pass: String,
    pub stratum_port: u16,
    pub pool_address: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            rpc_url: env::var("RPC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18443".to_string()),
            rpc_user: env::var("RPC_USER").context("RPC_USER not set")?,
            rpc_pass: env::var("RPC_PASS").context("RPC_PASS not set")?,
            stratum_port: env::var("STRATUM_PORT")
                .unwrap_or_else(|_| "3333".to_string())
                .parse()
                .context("STRATUM_PORT must be a valid port number")?,
            pool_address: env::var("POOL_ADDRESS").context("POOL_ADDRESS not set")?,
        })
    }
}
