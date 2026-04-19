use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use binary_sv2::{B0255, B064K, Seq0255, U256};
use bitcoin::{
    Amount, Target, Transaction,
    amount::CheckedSum,
    block::Header,
    consensus::{deserialize, serialize},
    hashes::Hash,
};
use bitcoin_capnp_types::{
    init_capnp::init::Client as InitClient,
    mining_capnp::{block_template::Client as BlockTemplateClient, mining::Client as MiningClient},
    proxy_capnp::{thread::Client as ThreadClient, thread_map::Client as ThreadMapClient},
};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use template_distribution_sv2::{NewTemplate, SetNewPrevHash};
use tokio::{net::UnixStream, sync::{mpsc, oneshot, watch}};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::info;

const WEIGHT_FACTOR: u32 = 4;
const MIN_BLOCK_RESERVED_WEIGHT: u64 = 2000;
const FEE_THRESHOLD_SATS: i64 = 1000;
const WAIT_NEXT_TIMEOUT_MS: f64 = 10_000.0;

#[derive(Debug, Clone)]
pub struct RawTemplate {
    pub new_template: NewTemplate<'static>,
    pub set_new_prev_hash: SetNewPrevHash<'static>,
}

pub struct SubmitSolutionData {
    pub template_id: u64,
    pub version: u32,
    pub header_timestamp: u32,
    pub header_nonce: u32,
    pub coinbase_tx: Vec<u8>,
}

/// Connect directly to Bitcoin Core via IPC.
///
/// Returns a `watch::Receiver` updated on every new template and an
/// `mpsc::Sender` to submit valid block solutions back to the node.
/// Blocks the calling task until the first template is ready.
pub async fn start(
    socket_path: &Path,
    coinbase_output_max_size: u32,
) -> Result<(watch::Receiver<RawTemplate>, mpsc::Sender<SubmitSolutionData>)> {
    let (solution_tx, solution_rx) = mpsc::channel::<SubmitSolutionData>(8);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<watch::Receiver<RawTemplate>>>();

    let path = socket_path.to_path_buf();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime for node_ipc");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            if let Err(e) = ipc_main(path, coinbase_output_max_size, solution_rx, ready_tx).await {
                tracing::error!("node_ipc: {e:#}");
            }
        });
    });

    let template_rx = ready_rx
        .await
        .context("node_ipc thread exited before first template")??;

    Ok((template_rx, solution_tx))
}

async fn ipc_main(
    socket_path: PathBuf,
    coinbase_output_max_size: u32,
    mut solution_rx: mpsc::Receiver<SubmitSolutionData>,
    ready_tx: oneshot::Sender<Result<watch::Receiver<RawTemplate>>>,
) -> Result<()> {
    let stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("connect Bitcoin Core IPC: {}", socket_path.display()))?;

    let (reader, writer) = stream.into_split();
    let rpc_network = Box::new(twoparty::VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        rpc_twoparty_capnp::Side::Client,
        Default::default(),
    ));
    let mut rpc = RpcSystem::new(rpc_network, None);
    let init: InitClient = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
    tokio::task::spawn_local(rpc);

    let thread_map: ThreadMapClient = init.construct_request()
        .send().promise.await?
        .get()?.get_thread_map()?;

    let thread: ThreadClient = new_thread(&thread_map).await?;

    let mut mining_req = init.make_mining_request();
    mining_req.get().get_context()?.set_thread(thread.clone());
    let mining: MiningClient = mining_req.send().promise.await?.get()?.get_result()?;

    let mut block = new_block_client(&mining, coinbase_output_max_size).await?;

    let mut template_id = 0u64;
    let first = fetch_template(&block, &thread, template_id).await?;
    template_id += 1;
    info!("node_ipc: connected, first template ready");

    let (template_tx, template_rx) = watch::channel(first);
    let _ = ready_tx.send(Ok(template_rx));

    loop {
        let wait_thread = new_thread(&thread_map).await?;
        tokio::select! {
            result = wait_next(&block, &wait_thread) => {
                match result? {
                    Some(new_block) => {
                        destroy_block_client(&block, &thread).await;
                        block = new_block;
                        let raw = fetch_template(&block, &thread, template_id).await?;
                        template_id += 1;
                        info!("node_ipc: new template (id={})", raw.new_template.template_id);
                        if template_tx.send(raw).is_err() { break; }
                    }
                    None => {} // waitNext timed out, retry
                }
            }
            Some(sol) = solution_rx.recv() => {
                if let Err(e) = submit_solution(&block, &thread, sol).await {
                    tracing::error!("submit_solution: {e:#}");
                }
            }
        }
    }
    // Signal Bitcoin Core we are done before closing the socket.
    // Required on Bitcoin Core < v31 to avoid the node treating the disconnect as a crash.
    destroy_block_client(&block, &thread).await;
    Ok(())
}

async fn destroy_block_client(block: &BlockTemplateClient, thread: &ThreadClient) {
    let mut req = block.destroy_request();
    match req.get().get_context() {
        Ok(mut ctx) => ctx.set_thread(thread.clone()),
        Err(_) => return,
    }
    let _ = req.send().promise.await;
}

async fn new_thread(thread_map: &ThreadMapClient) -> Result<ThreadClient> {
    Ok(thread_map.make_thread_request()
        .send().promise.await?
        .get()?.get_result()?)
}

async fn new_block_client(
    mining: &MiningClient,
    coinbase_output_max_size: u32,
) -> Result<BlockTemplateClient> {
    let mut req = mining.create_new_block_request();
    let mut opts = req.get().get_options()?;
    let reserved = ((coinbase_output_max_size * WEIGHT_FACTOR) as u64).max(MIN_BLOCK_RESERVED_WEIGHT);
    opts.set_block_reserved_weight(reserved);
    opts.set_coinbase_output_max_additional_sigops(400);
    opts.set_use_mempool(true);
    Ok(req.send().promise.await?.get()?.get_result()?)
}

async fn fetch_template(
    block: &BlockTemplateClient,
    thread: &ThreadClient,
    template_id: u64,
) -> Result<RawTemplate> {
    let mut req = block.get_block_header_request();
    req.get().get_context()?.set_thread(thread.clone());
    let header_bytes = req.send().promise.await?.get()?.get_result()?.to_vec();
    let header: Header = deserialize(&header_bytes).context("deserialize block header")?;

    let mut req = block.get_coinbase_tx_request();
    req.get().get_context()?.set_thread(thread.clone());
    let cb_bytes = req.send().promise.await?.get()?.get_result()?.to_vec();
    let cb: Transaction = deserialize(&cb_bytes).context("deserialize coinbase tx")?;

    let mut req = block.get_coinbase_merkle_path_request();
    req.get().get_context()?.set_thread(thread.clone());
    let path: Vec<Vec<u8>> = req.send().promise.await?.get()?.get_result()?
        .iter()
        .map(|x| x.map(|s| s.to_vec()))
        .collect::<Result<Vec<_>, _>>()?;

    build_raw_template(template_id, &header, &cb, &path)
}

fn build_raw_template(
    template_id: u64,
    header: &Header,
    cb: &Transaction,
    path: &[Vec<u8>],
) -> Result<RawTemplate> {
    let empty_outputs: Vec<_> = cb.output.iter()
        .filter(|o| o.value == Amount::from_sat(0))
        .collect();
    let mut serialized_outputs = Vec::new();
    for o in &empty_outputs {
        serialized_outputs.extend_from_slice(&serialize(*o));
    }

    let value_remaining = cb.output.iter()
        .map(|o| o.value)
        .checked_sum()
        .context("coinbase output value overflow")?
        .to_sat();

    let coinbase_prefix = B0255::try_from(cb.input[0].script_sig.to_bytes())
        .map_err(|e| anyhow::anyhow!("coinbase_prefix: {e:?}"))?;
    let coinbase_tx_outputs = B064K::try_from(serialized_outputs)
        .map_err(|e| anyhow::anyhow!("coinbase_tx_outputs: {e:?}"))?;

    let path_u256: Vec<U256<'static>> = path.iter()
        .map(|h| U256::try_from(h.clone()).map_err(|e| anyhow::anyhow!("merkle hash: {e:?}")))
        .collect::<Result<_>>()?;
    let merkle_path = Seq0255::new(path_u256)
        .map_err(|e| anyhow::anyhow!("merkle_path: {e:?}"))?;

    let target_bytes: [u8; 32] = Target::from(header.bits).to_le_bytes();
    let prev_bytes: [u8; 32] = header.prev_blockhash.to_byte_array();

    let new_template = NewTemplate {
        template_id,
        future_template: true,
        version: header.version.to_consensus() as u32,
        coinbase_tx_version: cb.version.0 as u32,
        coinbase_prefix,
        coinbase_tx_input_sequence: cb.input[0].sequence.to_consensus_u32(),
        coinbase_tx_value_remaining: value_remaining,
        coinbase_tx_outputs_count: empty_outputs.len() as u32,
        coinbase_tx_outputs,
        coinbase_tx_locktime: cb.lock_time.to_consensus_u32(),
        merkle_path,
    };

    let set_new_prev_hash = SetNewPrevHash {
        template_id,
        prev_hash: prev_bytes.into(),
        header_timestamp: header.time,
        n_bits: header.bits.to_consensus(),
        target: target_bytes.into(),
    };

    Ok(RawTemplate {
        new_template: new_template.into_static(),
        set_new_prev_hash: set_new_prev_hash.into_static(),
    })
}

async fn wait_next(
    block: &BlockTemplateClient,
    thread: &ThreadClient,
) -> Result<Option<BlockTemplateClient>> {
    let mut req = block.wait_next_request();
    match req.get().get_context() {
        Ok(mut ctx) => ctx.set_thread(thread.clone()),
        Err(e) => return Err(e.into()),
    }
    let mut opts = req.get().get_options()?;
    opts.set_fee_threshold(FEE_THRESHOLD_SATS);
    opts.set_timeout(WAIT_NEXT_TIMEOUT_MS);

    let resp = req.send().promise.await?;
    match resp.get()?.get_result() {
        Ok(client) => Ok(Some(client)),
        Err(e) if e.kind == capnp::ErrorKind::MessageContainsNullCapabilityPointer => Ok(None),
        Err(e) => Err(anyhow::anyhow!("waitNext: {e}")),
    }
}

async fn submit_solution(
    block: &BlockTemplateClient,
    thread: &ThreadClient,
    sol: SubmitSolutionData,
) -> Result<()> {
    let mut req = block.submit_solution_request();
    let mut params = req.get();
    params.set_version(sol.version);
    params.set_timestamp(sol.header_timestamp);
    params.set_nonce(sol.header_nonce);
    params.set_coinbase(&sol.coinbase_tx);
    params.get_context()?.set_thread(thread.clone());

    let resp = req.send().promise.await?;
    if !resp.get()?.get_result() {
        anyhow::bail!("Bitcoin Core rejected solution for template {}", sol.template_id);
    }
    info!("node_ipc: solution accepted (template {})", sol.template_id);
    Ok(())
}
