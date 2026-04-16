use pool::{
    config::Config,
    rpc::{RpcClient, TemplatePoller},
    stratum_sv2::{AuthorityKeypair, Sv2Server},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::from_env()?;
    let client = RpcClient::new(&cfg.rpc_url, &cfg.rpc_user, &cfg.rpc_pass);

    let hash = client.get_best_block_hash().await?;
    tracing::info!("Connected to node, best block: {hash}");

    // Start the template poller (long-polls Bitcoin Core for new block templates).
    // TODO Paso 4b: replace with SV2 Template Distribution Protocol client.
    let poller = TemplatePoller::start(client).await?;
    let template_rx = poller.subscribe();

    // Build the Noise NX authority keypair from config.
    let keypair = AuthorityKeypair {
        public: cfg.pool_authority_public_key,
        private: cfg.pool_authority_private_key,
    };

    // Start the SV2 Mining Protocol server.
    let server = Sv2Server::new(keypair, cfg.sv2_listen_addr, template_rx, cfg.pool_address);
    server.run().await?;

    Ok(())
}
