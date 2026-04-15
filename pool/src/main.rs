use pool::{config::Config, rpc::RpcClient};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::from_env()?;
    let client = RpcClient::new(&cfg.rpc_url, &cfg.rpc_user, &cfg.rpc_pass);

    let hash = client.get_best_block_hash().await?;
    tracing::info!("Connected to node, best block: {hash}");

    Ok(())
}
