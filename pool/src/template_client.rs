use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use binary_sv2::{B064K, Str0255};
use codec_sv2::HandshakeRole;
use common_messages_sv2::{
    Protocol, SetupConnection, MESSAGE_TYPE_SETUP_CONNECTION,
    MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use framing_sv2::framing::Frame;
use noise_sv2::Initiator;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use template_distribution_sv2::{
    CoinbaseOutputConstraints, SubmitSolution, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS,
    MESSAGE_TYPE_NEW_TEMPLATE, MESSAGE_TYPE_SET_NEW_PREV_HASH, MESSAGE_TYPE_SUBMIT_SOLUTION,
};
use tokio::{net::TcpStream, sync::{mpsc, watch}};
use tracing::{info, warn};

use crate::noise_connection::{connect_noise, NoiseReadHalf, NoiseWriteHalf};

/// sv2-tp listens on 18447 in regtest (8442 mainnet).
pub const TP_REGTEST_PORT: u16 = 18447;

/// Raw payload bytes for a paired NewTemplate + SetNewPrevHash.
///
/// Consumers deserialize with `binary_sv2::from_bytes` as needed.
#[derive(Debug, Clone)]
pub struct RawTemplate {
    pub new_template: Vec<u8>,
    pub set_new_prev_hash: Vec<u8>,
}

/// Data needed to send a `SubmitSolution` to sv2-tp after mining a valid block.
pub struct SubmitSolutionData {
    pub template_id: u64,
    pub version: u32,
    pub header_timestamp: u32,
    pub header_nonce: u32,
    /// Full serialized coinbase transaction with the actual extranonce filled in.
    pub coinbase_tx: Vec<u8>,
}

/// Read the sv2-tp authority public key from the bitcoin data dir.
///
/// sv2-tp writes `<datadir>/regtest/sv2_authority_key` (33 bytes: 1-byte format prefix +
/// 32-byte private key). The public key is derived via secp256k1.
pub fn read_authority_pubkey(datadir: &str) -> Result<[u8; 32]> {
    let path = format!("{datadir}/regtest/sv2_authority_key");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("read sv2 authority key from {path}"))?;
    if bytes.len() != 33 {
        bail!("expected 33-byte authority key file, got {} bytes", bytes.len());
    }
    // bytes[0] = format prefix (0x01); bytes[1..33] = private key.
    let secret_key = SecretKey::from_slice(&bytes[1..])
        .context("parse sv2 authority secret key")?;
    let secp = Secp256k1::new();
    let (xonly, _) = Keypair::from_secret_key(&secp, &secret_key).x_only_public_key();
    Ok(xonly.serialize())
}

/// Connect to sv2-tp, receive the first template pair, then stream further updates.
///
/// Returns a `watch::Receiver` that is updated on each new template pair, and an
/// `mpsc::Sender` to send `SubmitSolution` messages back to sv2-tp when a block is found.
pub async fn start(
    tp_addr: SocketAddr,
    authority_pubkey: [u8; 32],
    coinbase_output_max_size: u32,
) -> Result<(watch::Receiver<RawTemplate>, mpsc::Sender<SubmitSolutionData>)> {
    let stream = TcpStream::connect(tp_addr)
        .await
        .context("TCP connect to sv2-tp")?;

    let initiator = Initiator::from_raw_k(authority_pubkey)
        .map_err(|e| anyhow::anyhow!("create Noise initiator: {e:?}"))?;
    let (mut reader, mut writer) = connect_noise(stream, HandshakeRole::Initiator(initiator))
        .await
        .context("Noise NX handshake with sv2-tp")?;

    setup_connection(&mut writer, tp_addr.port()).await?;
    expect_setup_connection_success(&mut reader).await?;
    coinbase_output_constraints(&mut writer, coinbase_output_max_size).await?;
    info!("sv2-tp: connected, waiting for first template pair");

    let first = recv_until_pair(&mut reader).await?;
    info!("sv2-tp: first template pair received");

    let (template_tx, template_rx) = watch::channel(first);
    let (solution_tx, solution_rx) = mpsc::channel::<SubmitSolutionData>(8);

    tokio::spawn(async move {
        if let Err(e) = io_loop(reader, writer, template_tx, solution_rx).await {
            tracing::error!("sv2-tp io loop ended: {e:#}");
        }
    });

    Ok((template_rx, solution_tx))
}

/// Background task: receives template updates and sends SubmitSolution messages.
async fn io_loop(
    mut reader: NoiseReadHalf,
    mut writer: NoiseWriteHalf,
    template_tx: watch::Sender<RawTemplate>,
    mut solution_rx: mpsc::Receiver<SubmitSolutionData>,
) -> Result<()> {
    let mut pending: Option<Vec<u8>> = None;

    loop {
        tokio::select! {
            result = next_msg(&mut reader) => {
                let (msg_type, payload) = result?;
                match msg_type {
                    MESSAGE_TYPE_NEW_TEMPLATE => {
                        info!("sv2-tp: NewTemplate");
                        pending = Some(payload);
                    }
                    MESSAGE_TYPE_SET_NEW_PREV_HASH => {
                        info!("sv2-tp: SetNewPrevHash");
                        if let Some(new_template) = pending.take() {
                            if template_tx.send(RawTemplate {
                                new_template,
                                set_new_prev_hash: payload,
                            }).is_err() {
                                break; // all receivers dropped
                            }
                        } else {
                            warn!("sv2-tp: SetNewPrevHash without pending NewTemplate — ignoring");
                        }
                    }
                    other => warn!(msg_type = other, "sv2-tp: unexpected message"),
                }
            }
            Some(sol) = solution_rx.recv() => {
                send_submit_solution(&mut writer, sol).await?;
            }
        }
    }
    Ok(())
}

async fn send_submit_solution(writer: &mut NoiseWriteHalf, data: SubmitSolutionData) -> Result<()> {
    let coinbase_tx = B064K::try_from(data.coinbase_tx)
        .map_err(|e| anyhow::anyhow!("coinbase_tx for SubmitSolution: {e:?}"))?;
    let msg = SubmitSolution {
        template_id: data.template_id,
        version: data.version,
        header_timestamp: data.header_timestamp,
        header_nonce: data.header_nonce,
        coinbase_tx,
    };
    info!(template_id = msg.template_id, header_nonce = msg.header_nonce, "sv2-tp: SubmitSolution");
    writer.write_sv2_message(msg, MESSAGE_TYPE_SUBMIT_SOLUTION, false).await
}

/// Receive messages until one complete NewTemplate + SetNewPrevHash pair is available.
async fn recv_until_pair(reader: &mut NoiseReadHalf) -> Result<RawTemplate> {
    let mut pending: Option<Vec<u8>> = None;

    loop {
        let (msg_type, payload) = next_msg(reader).await?;
        match msg_type {
            MESSAGE_TYPE_NEW_TEMPLATE => {
                pending = Some(payload);
            }
            MESSAGE_TYPE_SET_NEW_PREV_HASH => {
                if let Some(new_template) = pending.take() {
                    return Ok(RawTemplate { new_template, set_new_prev_hash: payload });
                }
                warn!("sv2-tp: SetNewPrevHash without pending NewTemplate — ignoring");
            }
            other => warn!(msg_type = other, "sv2-tp: unexpected message during setup"),
        }
    }
}

async fn next_msg(reader: &mut NoiseReadHalf) -> Result<(u8, Vec<u8>)> {
    let frame = reader.read_frame().await.context("read sv2-tp frame")?;
    let mut sv2 = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame after transport"),
    };
    let msg_type = sv2.get_header().context("frame has no header")?.msg_type();
    Ok((msg_type, sv2.payload().to_vec()))
}

async fn setup_connection(writer: &mut NoiseWriteHalf, port: u16) -> Result<()> {
    let msg = SetupConnection {
        protocol: Protocol::TemplateDistributionProtocol,
        min_version: 2,
        max_version: 2,
        flags: 0,
        endpoint_host: Str0255::try_from(b"127.0.0.1".to_vec()).unwrap(),
        endpoint_port: port,
        vendor: Str0255::try_from(b"lottery-pool".to_vec()).unwrap(),
        hardware_version: Str0255::try_from(b"0.1".to_vec()).unwrap(),
        firmware: Str0255::try_from(b"0.1".to_vec()).unwrap(),
        device_id: Str0255::try_from(b"pool".to_vec()).unwrap(),
    };
    writer
        .write_sv2_message(msg, MESSAGE_TYPE_SETUP_CONNECTION, false)
        .await
}

async fn expect_setup_connection_success(reader: &mut NoiseReadHalf) -> Result<()> {
    let frame = reader.read_frame().await.context("read SetupConnectionSuccess")?;
    let sv2 = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame"),
    };
    let msg_type = sv2.get_header().context("no header")?.msg_type();
    if msg_type != MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS {
        bail!(
            "expected SetupConnectionSuccess (0x{MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS:02x}), \
             got 0x{msg_type:02x}"
        );
    }
    Ok(())
}

async fn coinbase_output_constraints(
    writer: &mut NoiseWriteHalf,
    max_additional_size: u32,
) -> Result<()> {
    let msg = CoinbaseOutputConstraints {
        coinbase_output_max_additional_size: max_additional_size,
        coinbase_output_max_additional_sigops: 400u16,
    };
    writer
        .write_sv2_message(msg, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, false)
        .await
}
