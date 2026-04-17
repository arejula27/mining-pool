use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use binary_sv2::Str0255;
use codec_sv2::HandshakeRole;
use common_messages_sv2::{
    Protocol, SetupConnection, MESSAGE_TYPE_SETUP_CONNECTION,
    MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use framing_sv2::framing::Frame;
use noise_sv2::Initiator;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use template_distribution_sv2::{
    CoinbaseOutputConstraints, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_NEW_TEMPLATE,
    MESSAGE_TYPE_SET_NEW_PREV_HASH,
};
use tokio::{net::TcpStream, sync::watch};
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
/// Mirrors `TemplatePoller::start`: blocks until the first `NewTemplate` + `SetNewPrevHash`
/// pair is received, then returns a `watch::Receiver` and spawns a background task.
pub async fn start(
    tp_addr: SocketAddr,
    authority_pubkey: [u8; 32],
    coinbase_output_max_size: u32,
) -> Result<watch::Receiver<RawTemplate>> {
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

    let (tx, rx) = watch::channel(first);

    tokio::spawn(async move {
        if let Err(e) = recv_loop(reader, tx).await {
            tracing::error!("sv2-tp template stream ended: {e:#}");
        }
    });

    Ok(rx)
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

/// Background loop: continues pairing NewTemplate + SetNewPrevHash and sending on `tx`.
async fn recv_loop(mut reader: NoiseReadHalf, tx: watch::Sender<RawTemplate>) -> Result<()> {
    let mut pending: Option<Vec<u8>> = None;

    loop {
        let (msg_type, payload) = next_msg(&mut reader).await?;
        match msg_type {
            MESSAGE_TYPE_NEW_TEMPLATE => {
                info!("sv2-tp: NewTemplate");
                pending = Some(payload);
            }
            MESSAGE_TYPE_SET_NEW_PREV_HASH => {
                info!("sv2-tp: SetNewPrevHash");
                if let Some(new_template) = pending.take() {
                    if tx.send(RawTemplate { new_template, set_new_prev_hash: payload }).is_err() {
                        break; // all receivers dropped, pool shutting down
                    }
                } else {
                    warn!("sv2-tp: SetNewPrevHash without pending NewTemplate — ignoring");
                }
            }
            other => warn!(msg_type = other, "sv2-tp: unexpected message"),
        }
    }
    Ok(())
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
