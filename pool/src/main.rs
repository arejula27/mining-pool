use pool::{
    config::Config,
    db::DbWorker,
    stratum_sv2::{AuthorityKeypair, Sv2Server},
    template_client,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::from_env()?;

    let db_worker = DbWorker::start("pool.db")?;

    let authority_pubkey = template_client::read_authority_pubkey(&cfg.bitcoin_datadir)?;
    let (template_rx, solution_tx) = template_client::start(cfg.tp_addr, authority_pubkey, 100).await?;
    tracing::info!("Connected to sv2-tp at {}", cfg.tp_addr);

    let keypair = AuthorityKeypair {
        public: cfg.pool_authority_public_key,
        private: cfg.pool_authority_private_key,
    };

    let server = Sv2Server::new(
        keypair,
        cfg.sv2_listen_addr,
        template_rx,
        cfg.pool_address,
        solution_tx,
        Some(db_worker.sender()),
    );
    server.run().await?;

    Ok(())
}
