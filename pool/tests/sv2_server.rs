//! Integration tests for the SV2 Mining Protocol server.
//!
//! Requires the full environment (bitcoin-node + sv2-tp).
//! Use `just int` or `just int-tdp` which start and stop everything automatically.
//!
//! The tests spin up the SV2 server on a fixed local port (13334) and connect a
//! minimal in-process Noise initiator to exercise the full handshake + channel
//! opening flow without needing the external translator binary.

use std::{net::SocketAddr, time::Duration};

use anyhow::{bail, Context, Result};
use binary_sv2::{Str0255, U256};
use codec_sv2::HandshakeRole;
use common_messages_sv2::{
    Protocol, SetupConnection, SetupConnectionSuccess, MESSAGE_TYPE_SETUP_CONNECTION,
    MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use framing_sv2::{framing::{Frame, Sv2Frame}, header::Header};
use mining_sv2::{
    NewExtendedMiningJob, OpenExtendedMiningChannel, OpenExtendedMiningChannelSuccess,
    SetNewPrevHash, MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB,
    MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL, MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
};
use noise_sv2::Initiator;
use secp256k1::{rand::thread_rng, Keypair, Secp256k1};
use tokio::net::TcpStream;

use pool::{
    noise_connection::{connect_noise, NoiseReadHalf},
    stratum_sv2::{AuthorityKeypair, Sv2Server},
    template_client,
};

// Fixed port for SV2 server in tests.  Tests run with --test-threads=1 so no conflicts.
const SV2_TEST_PORT: u16 = 13334;

fn datadir() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".bitcoin-data")
        .to_string_lossy()
        .into_owned()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Generate a fresh authority keypair for the test.
fn generate_keypair() -> ([u8; 32], [u8; 32]) {
    let secp = Secp256k1::new();
    let kp = Keypair::new(&secp, &mut thread_rng());
    let (xonly, _) = kp.x_only_public_key();
    (xonly.serialize(), kp.secret_key().secret_bytes())
}

/// Serialize an SV2 message to wire bytes (SV2 header + payload).
fn serialize_sv2<T: binary_sv2::Serialize + binary_sv2::GetSize>(
    msg: T,
    msg_type: u8,
    is_channel_msg: bool,
) -> Vec<u8> {
    let payload_len = msg.get_size();
    let total = Header::SIZE + payload_len;
    let mut dst = vec![0u8; total];
    Sv2Frame::<_, Vec<u8>>::from_message(msg, msg_type, 0, is_channel_msg)
        .expect("valid SV2 frame")
        .serialize(&mut dst)
        .expect("serialize SV2 frame");
    dst
}

/// Read the next SV2 frame, check its message type, and return the payload bytes.
async fn read_expect(
    reader: &mut NoiseReadHalf,
    expected_msg_type: u8,
) -> Result<Vec<u8>> {
    let frame = reader.read_frame().await.context("read frame")?;
    let mut sv2 = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame after transport"),
    };
    let header = sv2.get_header().context("frame missing header")?;
    let got = header.msg_type();
    if got != expected_msg_type {
        bail!(
            "expected message type 0x{expected_msg_type:02x}, got 0x{got:02x}"
        );
    }
    Ok(sv2.payload().to_vec())
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// Full SV2 flow: Noise handshake → SetupConnection → OpenExtendedMiningChannel.
///
/// Verifies that:
/// - The pool completes the Noise NX handshake as the responder.
/// - `SetupConnectionSuccess` is returned with `used_version = 2` and the
///   `REQUIRES_EXTENDED_CHANNELS` flag set.
/// - `OpenExtendedMiningChannelSuccess` echoes the request_id and assigns a
///   channel_id with extranonce_size = 4.
/// - `SetNewPrevHash` and `NewExtendedMiningJob` are immediately sent for the
///   new channel, referencing the correct channel_id and containing non-empty
///   coinbase parts.
#[tokio::test]
async fn sv2_server_open_extended_channel() {
    // Generate test keypair and start the server.
    let (pub_key, priv_key) = generate_keypair();
    let authority = AuthorityKeypair { public: pub_key, private: priv_key };

    // Any parseable address works; assume_checked() skips network validation.
    let pool_addr = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq".to_string();

    let tp_authority_pubkey = template_client::read_authority_pubkey(&datadir())
        .expect("read sv2_authority_key — run `just start-all` first");
    let (template_rx, solution_tx) = template_client::start(
        "127.0.0.1:18447".parse().unwrap(),
        tp_authority_pubkey,
        100,
    )
    .await
    .expect("connect to sv2-tp");

    let listen_addr: SocketAddr = format!("127.0.0.1:{SV2_TEST_PORT}").parse().unwrap();
    let server = Sv2Server::new(authority, listen_addr, template_rx, pool_addr, solution_tx);

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            eprintln!("SV2 server error: {e:#}");
        }
    });

    // Give the server time to start listening.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Connect and complete the Noise NX handshake.
    let stream = TcpStream::connect(format!("127.0.0.1:{SV2_TEST_PORT}"))
        .await
        .expect("TCP connect to SV2 server");

    let initiator = Initiator::from_raw_k(pub_key).expect("create Noise initiator");
    let role = HandshakeRole::Initiator(initiator);

    let (mut reader, mut writer) = connect_noise(stream, role)
        .await
        .expect("Noise NX handshake");

    // ── SetupConnection ──────────────────────────────────────────────────────

    let setup = SetupConnection {
        protocol: Protocol::MiningProtocol,
        min_version: 2,
        max_version: 2,
        flags: 0x02, // REQUIRES_EXTENDED_CHANNELS
        endpoint_host: Str0255::try_from(b"127.0.0.1".to_vec()).unwrap(),
        endpoint_port: SV2_TEST_PORT,
        vendor: Str0255::try_from(b"test-client".to_vec()).unwrap(),
        hardware_version: Str0255::try_from(b"1.0".to_vec()).unwrap(),
        firmware: Str0255::try_from(b"1.0".to_vec()).unwrap(),
        device_id: Str0255::try_from(b"test-device".to_vec()).unwrap(),
    };
    let bytes = serialize_sv2(setup, MESSAGE_TYPE_SETUP_CONNECTION, false);
    writer.write_raw_frame(bytes).await.expect("send SetupConnection");

    let mut payload = read_expect(&mut reader, MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS)
        .await
        .expect("receive SetupConnectionSuccess");
    let sc_success: SetupConnectionSuccess =
        binary_sv2::from_bytes(&mut payload).expect("parse SetupConnectionSuccess");

    assert_eq!(sc_success.used_version, 2, "used_version must be 2");
    assert_eq!(
        sc_success.flags & 0x02, 0x02,
        "REQUIRES_EXTENDED_CHANNELS flag must be set"
    );

    // ── OpenExtendedMiningChannel ────────────────────────────────────────────

    // Use all-0xff max_target (lowest difficulty) so the pool accepts any share.
    let max_target = U256::try_from([0xff_u8; 32].to_vec()).expect("max_target");
    let open_ch = OpenExtendedMiningChannel {
        request_id: 42,
        user_identity: Str0255::try_from(b"test-miner".to_vec()).unwrap(),
        nominal_hash_rate: 1000.0_f32,
        max_target,
        min_extranonce_size: 4,
    };
    let bytes = serialize_sv2(open_ch, MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL, false);
    writer
        .write_raw_frame(bytes)
        .await
        .expect("send OpenExtendedMiningChannel");

    // Receive OpenExtendedMiningChannelSuccess.
    let mut payload =
        read_expect(&mut reader, MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS)
            .await
            .expect("receive OpenExtendedMiningChannelSuccess");
    let ch_success: OpenExtendedMiningChannelSuccess =
        binary_sv2::from_bytes(&mut payload).expect("parse OpenExtendedMiningChannelSuccess");

    assert_eq!(ch_success.request_id, 42, "request_id must match");
    assert_eq!(
        ch_success.extranonce_size, 4,
        "extranonce_size must equal min_extranonce_size"
    );
    let channel_id = ch_success.channel_id;

    // ── NewExtendedMiningJob ─────────────────────────────────────────────────

    let mut payload = read_expect(&mut reader, MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB)
        .await
        .expect("receive NewExtendedMiningJob");
    let job: NewExtendedMiningJob =
        binary_sv2::from_bytes(&mut payload).expect("parse NewExtendedMiningJob");

    assert_eq!(
        job.channel_id, channel_id,
        "NewExtendedMiningJob channel_id must match"
    );
    assert!(
        job.coinbase_tx_prefix.inner_as_ref().len() > 0,
        "coinbase_tx_prefix must not be empty"
    );
    assert!(
        job.coinbase_tx_suffix.inner_as_ref().len() > 0,
        "coinbase_tx_suffix must not be empty"
    );
    assert!(job.version > 0, "version must be positive");
    assert!(job.is_future(), "initial job must be a future job");

    // ── SetNewPrevHash ───────────────────────────────────────────────────────

    let mut payload = read_expect(&mut reader, MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH)
        .await
        .expect("receive SetNewPrevHash");
    let prev_hash: SetNewPrevHash =
        binary_sv2::from_bytes(&mut payload).expect("parse SetNewPrevHash");

    assert_eq!(
        prev_hash.channel_id, channel_id,
        "SetNewPrevHash channel_id must match"
    );
    assert!(prev_hash.nbits > 0, "nbits must be positive");
    assert_eq!(prev_hash.job_id, job.job_id, "SetNewPrevHash must reference the sent job");
}
