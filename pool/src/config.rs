use anyhow::{Context, Result};
use std::{env, net::SocketAddr};

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub rpc_user: String,
    pub rpc_pass: String,
    /// Address where the SV2 Mining Protocol server listens (default 0.0.0.0:3333).
    pub sv2_listen_addr: SocketAddr,
    pub pool_address: String,
    /// Pool authority X-only public key for the Noise NX handshake (32 bytes, hex).
    pub pool_authority_public_key: [u8; 32],
    /// Pool authority private key for the Noise NX handshake (32 bytes, hex).
    pub pool_authority_private_key: [u8; 32],
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Config {
            rpc_url: env::var("RPC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18443".to_string()),
            rpc_user: env::var("RPC_USER").context("RPC_USER not set")?,
            rpc_pass: env::var("RPC_PASS").context("RPC_PASS not set")?,
            sv2_listen_addr: env::var("SV2_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:3333".to_string())
                .parse()
                .context("SV2_LISTEN_ADDR must be a valid socket address")?,
            pool_address: env::var("POOL_ADDRESS").context("POOL_ADDRESS not set")?,
            pool_authority_public_key: parse_key32("POOL_AUTHORITY_PUBLIC_KEY")?,
            pool_authority_private_key: parse_key32("POOL_AUTHORITY_PRIVATE_KEY")?,
        })
    }
}

fn parse_key32(var: &str) -> Result<[u8; 32]> {
    let hex = env::var(var).with_context(|| format!("{var} not set"))?;
    let bytes = hex::decode(&hex).with_context(|| format!("{var} is not valid hex"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{var} must be exactly 32 bytes (64 hex chars)"))
}
