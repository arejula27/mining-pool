//! SV2 Mining Protocol server (Extended Channel mode).
//!
//! Listens for SV2 connections from the translator (or any SV2 client), performs
//! the Noise NX handshake as the responder, and handles the Mining Protocol messages
//! required for extended channels.
//!
//! Current implementation (Paso 4c):
//!   - Noise handshake (responder)
//!   - `SetupConnection` → `SetupConnectionSuccess`
//!   - `OpenExtendedMiningChannel` → `OpenExtendedMiningChannelSuccess`
//!     + `SetNewPrevHash` + `NewExtendedMiningJob`
//!   - Template-change broadcast: sends new `SetNewPrevHash` + `NewExtendedMiningJob`
//!     to all open channels when the block template changes

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use binary_sv2::{B032, B064K, Seq0255, Sv2Option, U256};
use codec_sv2::HandshakeRole;
use common_messages_sv2::{
    Protocol, SetupConnection, SetupConnectionError, SetupConnectionSuccess,
    MESSAGE_TYPE_SETUP_CONNECTION, MESSAGE_TYPE_SETUP_CONNECTION_ERROR,
    MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use framing_sv2::framing::{Frame, Sv2Frame};
use mining_sv2::{
    NewExtendedMiningJob, OpenExtendedMiningChannel, OpenExtendedMiningChannelSuccess,
    SetNewPrevHash, MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH,
    MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL, MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
};
use noise_sv2::Responder;
use tokio::{net::TcpListener, sync::watch};
use tracing::{error, info, warn};

use crate::{
    jobs::{
        build_merkle_branch, build_sv2_coinbase_parts, script_from_address,
        witness_commitment_script, SV2_EXTRANONCE2_SIZE,
    },
    noise_connection::{accept_noise, EitherFrame, NoiseWriteHalf},
    rpc::BlockTemplate,
};

// ── Pool authority keypair ────────────────────────────────────────────────────

/// Static authority keypair used for the Noise NX handshake.
pub struct AuthorityKeypair {
    pub public: [u8; 32],
    pub private: [u8; 32],
}

impl AuthorityKeypair {
    /// Certificate is valid for ~1 year.
    const CERT_VALIDITY: Duration = Duration::from_secs(365 * 24 * 60 * 60);

    pub fn to_responder(&self) -> Result<Box<Responder>> {
        Responder::from_authority_kp(&self.public, &self.private, Self::CERT_VALIDITY)
            .map_err(|e| anyhow::anyhow!("invalid authority keypair: {e:?}"))
    }
}

// ── Per-connection state ──────────────────────────────────────────────────────

/// State for a single open extended channel.
struct ChannelInfo {
    channel_id: u32,
    /// First 4 bytes of extranonce_prefix; used in Paso 5 to reconstruct the coinbase.
    #[allow(dead_code)]
    extranonce1: [u8; 4],
    /// Bitcoin address for this miner's coinbase output.
    miner_address: String,
}

/// Mutable state for one TCP connection (multiple channels can be multiplexed).
struct ConnectionState {
    next_channel_id: u32,
    next_job_id: u32,
    channels: HashMap<u32, ChannelInfo>,
    /// Fallback payout address when the miner does not supply a valid BTC address.
    pool_address: String,
}

impl ConnectionState {
    fn new(pool_address: String) -> Self {
        Self {
            next_channel_id: 0,
            next_job_id: 1,
            channels: HashMap::new(),
            pool_address,
        }
    }

    fn alloc_channel_id(&mut self) -> u32 {
        let id = self.next_channel_id;
        self.next_channel_id += 1;
        id
    }

    fn alloc_job_id(&mut self) -> u32 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        id
    }
}

// ── Server ────────────────────────────────────────────────────────────────────

pub struct Sv2Server {
    keypair: Arc<AuthorityKeypair>,
    listen_addr: SocketAddr,
    template_rx: watch::Receiver<BlockTemplate>,
    pool_address: String,
}

impl Sv2Server {
    pub fn new(
        keypair: AuthorityKeypair,
        listen_addr: SocketAddr,
        template_rx: watch::Receiver<BlockTemplate>,
        pool_address: String,
    ) -> Self {
        Self {
            keypair: Arc::new(keypair),
            listen_addr,
            template_rx,
            pool_address,
        }
    }

    /// Starts the TCP listener and accepts connections indefinitely.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(self.listen_addr)
            .await
            .with_context(|| format!("failed to bind SV2 server on {}", self.listen_addr))?;

        info!("SV2 Mining Protocol server listening on {}", self.listen_addr);

        loop {
            let (stream, peer_addr) = listener.accept().await.context("accept error")?;
            info!(%peer_addr, "New SV2 connection");

            let keypair = Arc::clone(&self.keypair);
            let template_rx = self.template_rx.clone();
            let pool_address = self.pool_address.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    handle_connection(stream, peer_addr, &keypair, template_rx, pool_address).await
                {
                    warn!(%peer_addr, "Connection error: {e:#}");
                }
            });
        }
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    keypair: &AuthorityKeypair,
    mut template_rx: watch::Receiver<BlockTemplate>,
    pool_address: String,
) -> Result<()> {
    let responder = keypair.to_responder()?;
    let role = HandshakeRole::Responder(responder);

    let (mut reader, mut writer) = accept_noise(stream, role)
        .await
        .with_context(|| format!("Noise handshake failed with {peer_addr}"))?;

    info!(%peer_addr, "Noise handshake complete");

    // First message must always be SetupConnection.
    let frame = reader.read_frame().await.context("read SetupConnection")?;
    handle_setup_connection(frame, &mut writer, peer_addr).await?;

    let mut state = ConnectionState::new(pool_address);

    loop {
        tokio::select! {
            result = reader.read_frame() => {
                let frame = match result {
                    Ok(f) => f,
                    Err(e) => {
                        info!(%peer_addr, "Connection closed: {e}");
                        return Ok(());
                    }
                };
                let template = template_rx.borrow().clone();
                if let Err(e) = dispatch_message(frame, &mut writer, &mut state, &template, peer_addr).await {
                    error!(%peer_addr, "Message error: {e:#}");
                    return Err(e);
                }
            }
            Ok(()) = template_rx.changed() => {
                let template = template_rx.borrow_and_update().clone();
                info!(%peer_addr, height = template.height, "Template updated, sending new jobs");
                if let Err(e) = send_jobs_to_all_channels(&mut writer, &state, &template).await {
                    error!(%peer_addr, "Error broadcasting jobs: {e:#}");
                }
            }
        }
    }
}

// ── SetupConnection ───────────────────────────────────────────────────────────

/// Parses a `SetupConnection` frame and responds with Success or Error.
pub async fn handle_setup_connection(
    frame: EitherFrame,
    writer: &mut NoiseWriteHalf,
    peer_addr: SocketAddr,
) -> Result<()> {
    let mut sv2_frame = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame after Noise transport"),
    };

    let header = sv2_frame.get_header().context("frame missing header")?;
    if header.msg_type() != MESSAGE_TYPE_SETUP_CONNECTION {
        let err = SetupConnectionError {
            flags: 0,
            error_code: "unsupported-message"
                .to_string()
                .try_into()
                .expect("valid error code"),
        };
        writer
            .write_sv2_message(err, MESSAGE_TYPE_SETUP_CONNECTION_ERROR, false)
            .await?;
        bail!(
            "expected SetupConnection (0x{:02x}), got 0x{:02x}",
            MESSAGE_TYPE_SETUP_CONNECTION,
            header.msg_type()
        );
    }

    let msg: SetupConnection<'_> = binary_sv2::from_bytes(sv2_frame.payload())
        .map_err(|e| anyhow::anyhow!("SetupConnection parse error: {e:?}"))?;

    info!(
        %peer_addr,
        protocol = ?msg.protocol,
        min_ver = msg.min_version,
        max_ver = msg.max_version,
        "SetupConnection"
    );

    if msg.protocol != Protocol::MiningProtocol {
        let err = SetupConnectionError {
            flags: 0,
            error_code: "unsupported-protocol"
                .to_string()
                .try_into()
                .expect("valid error code"),
        };
        writer
            .write_sv2_message(err, MESSAGE_TYPE_SETUP_CONNECTION_ERROR, false)
            .await?;
        bail!("unsupported protocol: {:?}", msg.protocol);
    }

    if msg.min_version > 2 || msg.max_version < 2 {
        let err = SetupConnectionError {
            flags: 0,
            error_code: "protocol-version-mismatch"
                .to_string()
                .try_into()
                .expect("valid error code"),
        };
        writer
            .write_sv2_message(err, MESSAGE_TYPE_SETUP_CONNECTION_ERROR, false)
            .await?;
        bail!(
            "version mismatch: client wants [{}, {}], we support 2",
            msg.min_version,
            msg.max_version
        );
    }

    let success = SetupConnectionSuccess {
        used_version: 2,
        // REQUIRES_EXTENDED_CHANNELS (bit 1): we only accept extended channels.
        flags: 0x02,
    };

    writer
        .write_sv2_message(success, MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS, false)
        .await
        .context("send SetupConnectionSuccess")?;

    info!(%peer_addr, "SetupConnection.Success sent");
    Ok(())
}

// ── Message dispatcher ────────────────────────────────────────────────────────

async fn dispatch_message(
    frame: EitherFrame,
    writer: &mut NoiseWriteHalf,
    state: &mut ConnectionState,
    template: &BlockTemplate,
    peer_addr: SocketAddr,
) -> Result<()> {
    let sv2_frame = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame in transport mode"),
    };

    let header = sv2_frame.get_header().context("frame missing header")?;

    match header.msg_type() {
        MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL => {
            handle_open_extended_mining_channel(sv2_frame, writer, state, template, peer_addr).await
        }
        other => {
            warn!(
                %peer_addr,
                msg_type = format!("0x{other:02x}"),
                "Unhandled SV2 message"
            );
            Ok(())
        }
    }
}

// ── OpenExtendedMiningChannel ─────────────────────────────────────────────────

async fn handle_open_extended_mining_channel(
    mut frame: Sv2Frame<u32, Vec<u8>>,
    writer: &mut NoiseWriteHalf,
    state: &mut ConnectionState,
    template: &BlockTemplate,
    peer_addr: SocketAddr,
) -> Result<()> {
    let msg: OpenExtendedMiningChannel<'_> =
        binary_sv2::from_bytes(frame.payload())
            .map_err(|e| anyhow::anyhow!("OpenExtendedMiningChannel parse error: {e:?}"))?;

    let request_id = msg.request_id;
    let user_identity = msg.user_identity.as_utf8_or_hex().to_string();

    // Extract miner BTC address from user_identity (e.g. "bc1q....worker1" or just the address).
    let miner_address = parse_miner_address(&user_identity, &state.pool_address);

    let channel_id = state.alloc_channel_id();
    let extranonce1 = channel_id.to_le_bytes();

    info!(
        %peer_addr,
        channel_id,
        user_identity = %user_identity,
        miner_address = %miner_address,
        "OpenExtendedMiningChannel"
    );

    // Build extranonce_prefix: 4 bytes extranonce1 padded to 32 bytes.
    let mut prefix_bytes = [0u8; 32];
    prefix_bytes[..4].copy_from_slice(&extranonce1);
    let extranonce_prefix: B032<'static> = B032::try_from(prefix_bytes.to_vec())
        .expect("prefix_bytes <= 32 bytes, within B032 max");

    // Use the network target from the current template as the initial channel target.
    let target = template_target(template)?;

    let success = OpenExtendedMiningChannelSuccess {
        request_id,
        channel_id,
        target,
        extranonce_size: SV2_EXTRANONCE2_SIZE as u16,
        extranonce_prefix,
        group_channel_id: 0,
    };

    writer
        .write_sv2_message(success, MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS, false)
        .await
        .context("send OpenExtendedMiningChannelSuccess")?;

    info!(%peer_addr, channel_id, "OpenExtendedMiningChannelSuccess sent");

    // Register the channel.
    let channel = ChannelInfo { channel_id, extranonce1, miner_address };
    state.channels.insert(channel_id, channel);

    // Send the first job immediately.
    let job_id = state.alloc_job_id();
    let channel = state.channels.get(&channel_id).unwrap();
    let (prev_hash, job) = build_job_messages(channel, job_id, template)?;

    writer
        .write_sv2_message(prev_hash, MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, true)
        .await
        .context("send SetNewPrevHash")?;
    writer
        .write_sv2_message(job, MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, true)
        .await
        .context("send NewExtendedMiningJob")?;

    info!(%peer_addr, channel_id, job_id, "Initial job sent");
    Ok(())
}

// ── Job broadcast ─────────────────────────────────────────────────────────────

/// Send new SetNewPrevHash + NewExtendedMiningJob to every open channel.
async fn send_jobs_to_all_channels(
    writer: &mut NoiseWriteHalf,
    state: &ConnectionState,
    template: &BlockTemplate,
) -> Result<()> {
    for channel in state.channels.values() {
        // Note: job_id monotonicity is not strictly required here since we don't
        // have access to &mut state; we use channel_id XOR template height as a
        // stable per-template identifier until we refactor state ownership.
        let job_id = (template.height << 8) | (channel.channel_id & 0xff);
        let (prev_hash, job) = build_job_messages(channel, job_id, template)?;

        writer
            .write_sv2_message(prev_hash, MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, true)
            .await
            .context("send SetNewPrevHash on template update")?;
        writer
            .write_sv2_message(job, MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, true)
            .await
            .context("send NewExtendedMiningJob on template update")?;
    }
    Ok(())
}

// ── Job construction ──────────────────────────────────────────────────────────

/// Build `SetNewPrevHash` + `NewExtendedMiningJob` for a single channel.
fn build_job_messages(
    channel: &ChannelInfo,
    job_id: u32,
    template: &BlockTemplate,
) -> Result<(SetNewPrevHash<'static>, NewExtendedMiningJob<'static>)> {
    // prevhash: getblocktemplate gives display format (reversed). SV2 wants internal byte order.
    let mut prev_hash_bytes =
        hex::decode(&template.previousblockhash).context("decode previousblockhash")?;
    prev_hash_bytes.reverse();
    let prev_hash = U256::try_from(prev_hash_bytes)
        .map_err(|e| anyhow::anyhow!("prev_hash conversion: {e:?}"))?;

    let nbits = u32::from_str_radix(&template.bits, 16)
        .with_context(|| format!("parse bits: {}", template.bits))?;

    let set_prev_hash = SetNewPrevHash {
        channel_id: channel.channel_id,
        job_id,
        prev_hash,
        min_ntime: template.curtime,
        nbits,
    };

    // Build per-channel coinbase using the miner's address.
    let miner_script = script_from_address(&channel.miner_address)
        .with_context(|| format!("invalid miner address: {}", channel.miner_address))?;
    let wc_script = template
        .default_witness_commitment
        .as_deref()
        .map(witness_commitment_script);

    let parts = build_sv2_coinbase_parts(
        template.height,
        template.coinbasevalue,
        miner_script,
        wc_script,
    );

    let coinbase_tx_prefix = B064K::try_from(parts.coinb1)
        .map_err(|e| anyhow::anyhow!("coinb1 too large: {e:?}"))?;
    let coinbase_tx_suffix = B064K::try_from(parts.coinb2)
        .map_err(|e| anyhow::anyhow!("coinb2 too large: {e:?}"))?;

    // Merkle path: branch hashes as U256 values.
    let branch = build_merkle_branch(&template.transactions);
    let path_hashes: Vec<U256<'static>> = branch
        .into_iter()
        .map(U256::from)
        .collect();
    let merkle_path =
        Seq0255::new(path_hashes).map_err(|e| anyhow::anyhow!("merkle path: {e:?}"))?;

    let job = NewExtendedMiningJob {
        channel_id: channel.channel_id,
        job_id,
        min_ntime: Sv2Option::new(Some(template.curtime)),
        version: template.version,
        version_rolling_allowed: true,
        merkle_path,
        coinbase_tx_prefix,
        coinbase_tx_suffix,
    };

    Ok((set_prev_hash, job))
}

/// Parse the network target from the template's hex `target` field into a U256.
fn template_target(template: &BlockTemplate) -> Result<U256<'static>> {
    let bytes = hex::decode(&template.target).context("decode target")?;
    U256::try_from(bytes).map_err(|e| anyhow::anyhow!("target conversion: {e:?}"))
}

/// Try to extract a Bitcoin address from a Stratum user_identity string.
///
/// Accepts "address.worker" or just "address". Falls back to `pool_address`
/// if the string cannot be parsed as a valid address.
fn parse_miner_address(user_identity: &str, pool_address: &str) -> String {
    // Strip optional ".worker" suffix.
    let candidate = user_identity.split('.').next().unwrap_or(user_identity);

    if script_from_address(candidate).is_ok() {
        candidate.to_string()
    } else {
        warn!(
            user_identity,
            "cannot parse as BTC address, falling back to pool address"
        );
        pool_address.to_string()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use binary_sv2::{GetSize, Str0255};
    use common_messages_sv2::{Protocol, SetupConnection};
    use framing_sv2::framing::Sv2Frame;

    /// Build a raw SetupConnection frame (header + payload) as byte vector.
    fn make_setup_connection_frame(protocol: Protocol, min_ver: u16, max_ver: u16) -> Vec<u8> {
        let msg = SetupConnection {
            protocol,
            min_version: min_ver,
            max_version: max_ver,
            flags: 0,
            endpoint_host: Str0255::try_from(b"localhost".to_vec()).unwrap(),
            endpoint_port: 3333,
            vendor: Str0255::try_from(b"test-vendor".to_vec()).unwrap(),
            hardware_version: Str0255::try_from(b"1.0".to_vec()).unwrap(),
            firmware: Str0255::try_from(b"1.0".to_vec()).unwrap(),
            device_id: Str0255::try_from(b"test-device".to_vec()).unwrap(),
        };

        let payload_len = msg.get_size();
        let total = framing_sv2::header::Header::SIZE + payload_len;
        let mut dst = vec![0u8; total];

        let frame = Sv2Frame::<_, Vec<u8>>::from_message(
            msg,
            MESSAGE_TYPE_SETUP_CONNECTION,
            0,
            false,
        )
        .expect("valid frame");
        frame.serialize(&mut dst).expect("serialize ok");
        dst
    }

    /// Decode a serialized SV2 frame bytes into `EitherFrame`.
    fn frame_from_bytes(bytes: Vec<u8>) -> EitherFrame {
        let sv2 = Sv2Frame::<u32, Vec<u8>>::from_bytes_unchecked(bytes);
        Frame::Sv2(sv2)
    }

    #[test]
    fn parse_setup_connection_mining_v2() {
        let raw = make_setup_connection_frame(Protocol::MiningProtocol, 2, 2);
        let frame = frame_from_bytes(raw);

        let sv2_frame = match frame {
            Frame::Sv2(f) => f,
            _ => panic!("expected Sv2 frame"),
        };
        let header = sv2_frame.get_header().expect("header present");
        assert_eq!(header.msg_type(), MESSAGE_TYPE_SETUP_CONNECTION);
    }

    #[test]
    fn parse_setup_connection_payload() {
        let mut raw = make_setup_connection_frame(Protocol::MiningProtocol, 2, 2);

        let header_size = framing_sv2::header::Header::SIZE;
        let payload = &mut raw[header_size..];
        let msg: SetupConnection<'_> =
            binary_sv2::from_bytes(payload).expect("deserialize SetupConnection");

        assert_eq!(msg.protocol, Protocol::MiningProtocol);
        assert_eq!(msg.min_version, 2);
        assert_eq!(msg.max_version, 2);
    }

    #[test]
    fn setup_connection_success_serializes() {
        let success = SetupConnectionSuccess {
            used_version: 2,
            flags: 0x02,
        };
        let bytes = binary_sv2::to_bytes(success).expect("serialize SetupConnectionSuccess");
        assert_eq!(bytes.len(), 6);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 2);
        assert_eq!(u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]), 0x02);
    }

    #[test]
    fn parse_miner_address_valid() {
        let pool = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        let miner = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.worker1";
        assert_eq!(parse_miner_address(miner, pool), "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq");
    }

    #[test]
    fn parse_miner_address_fallback() {
        let pool = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
        assert_eq!(parse_miner_address("not-a-valid-address", pool), pool);
    }
}
