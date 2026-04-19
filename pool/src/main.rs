use pool::{
    config::Config,
    db::DbWorker,
    node_ipc,
    stratum_sv2::{AuthorityKeypair, Sv2Server},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::from_env()?;

    let db_worker = DbWorker::start("pool.db")?;

    let (template_rx, solution_tx) = node_ipc::start(&cfg.bitcoin_ipc_socket, 100).await?;
    tracing::info!("Connected to Bitcoin Core IPC at {}", cfg.bitcoin_ipc_socket.display());

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
