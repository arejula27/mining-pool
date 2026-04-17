//! Integration tests for the Template Distribution Protocol client.
//!
//! Requires sv2-tp running on 127.0.0.1:18447 (regtest).
//! Use `just int-tdp` which starts and stops the full environment automatically,
//! or run `just start-all` manually before `cargo test --test template_client`.

use std::{net::SocketAddr, time::Duration};

use anyhow::{bail, Context, Result};
use binary_sv2::Str0255;
use codec_sv2::HandshakeRole;
use common_messages_sv2::{
    Protocol, SetupConnection, MESSAGE_TYPE_SETUP_CONNECTION,
    MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
};
use framing_sv2::framing::Frame;
use noise_sv2::Initiator;
use template_distribution_sv2::{
    CoinbaseOutputConstraints, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_NEW_TEMPLATE,
    MESSAGE_TYPE_SET_NEW_PREV_HASH,
};
use tokio::net::TcpStream;

use pool::{
    noise_connection::{connect_noise, NoiseReadHalf, NoiseWriteHalf},
    template_client::read_authority_pubkey,
};

const TP_ADDR: &str = "127.0.0.1:18447";

fn datadir() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".bitcoin-data")
        .to_string_lossy()
        .into_owned()
}

/// Read the next SV2 frame and return (msg_type, payload_bytes).
async fn next_msg(reader: &mut NoiseReadHalf) -> Result<(u8, Vec<u8>)> {
    let frame = reader.read_frame().await.context("read frame")?;
    let mut sv2 = match frame {
        Frame::Sv2(f) => f,
        Frame::HandShake(_) => bail!("unexpected HandShake frame after transport"),
    };
    let msg_type = sv2.get_header().context("frame has no header")?.msg_type();
    Ok((msg_type, sv2.payload().to_vec()))
}

async fn setup(
    writer: &mut NoiseWriteHalf,
    reader: &mut NoiseReadHalf,
) -> Result<()> {
    let setup_conn = SetupConnection {
        protocol: Protocol::TemplateDistributionProtocol,
        min_version: 2,
        max_version: 2,
        flags: 0,
        endpoint_host: Str0255::try_from(b"127.0.0.1".to_vec()).unwrap(),
        endpoint_port: 18447,
        vendor: Str0255::try_from(b"test".to_vec()).unwrap(),
        hardware_version: Str0255::try_from(b"0.1".to_vec()).unwrap(),
        firmware: Str0255::try_from(b"0.1".to_vec()).unwrap(),
        device_id: Str0255::try_from(b"test".to_vec()).unwrap(),
    };
    writer
        .write_sv2_message(setup_conn, MESSAGE_TYPE_SETUP_CONNECTION, false)
        .await
        .context("send SetupConnection")?;

    let (msg_type, _) = next_msg(reader).await?;
    if msg_type != MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS {
        bail!(
            "expected SetupConnectionSuccess (0x{MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS:02x}), \
             got 0x{msg_type:02x}"
        );
    }

    let constraints = CoinbaseOutputConstraints {
        coinbase_output_max_additional_size: 100,
        coinbase_output_max_additional_sigops: 400,
    };
    writer
        .write_sv2_message(constraints, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, false)
        .await
        .context("send CoinbaseOutputConstraints")?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Full TDP flow: Noise handshake → SetupConnection → CoinbaseOutputConstraints
/// → receive NewTemplate + SetNewPrevHash.
///
/// Verifies that:
/// - The Noise NX handshake completes against sv2-tp.
/// - SetupConnectionSuccess is returned.
/// - sv2-tp pushes a NewTemplate with a positive coinbase_tx_value_remaining.
/// - sv2-tp pushes a SetNewPrevHash with positive n_bits and a non-zero prev_hash.
#[tokio::test]
async fn tdp_receives_template_pair() {
    let authority_pubkey = read_authority_pubkey(&datadir())
        .expect("read sv2_authority_key — make sure sv2-tp has run at least once");

    let tp_addr: SocketAddr = TP_ADDR.parse().unwrap();
    let stream = TcpStream::connect(tp_addr)
        .await
        .expect("TCP connect to sv2-tp on :18447 — run `just start-all` first");

    let initiator =
        Initiator::from_raw_k(authority_pubkey).expect("create Noise initiator");
    let (mut reader, mut writer) =
        connect_noise(stream, HandshakeRole::Initiator(initiator))
            .await
            .expect("Noise NX handshake with sv2-tp");

    setup(&mut writer, &mut reader)
        .await
        .expect("SetupConnection + CoinbaseOutputConstraints");

    let mut got_new_template = false;
    let mut got_set_new_prev_hash = false;

    // sv2-tp should push both messages immediately after CoinbaseOutputConstraints.
    // Give it 10 s to account for slow CI; normally arrives in <1 s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while !got_new_template || !got_set_new_prev_hash {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for NewTemplate + SetNewPrevHash from sv2-tp"
        );

        let (msg_type, mut payload) =
            tokio::time::timeout(Duration::from_secs(10), next_msg(&mut reader))
                .await
                .expect("read timeout")
                .expect("read frame");

        match msg_type {
            MESSAGE_TYPE_NEW_TEMPLATE => {
                let tmpl: template_distribution_sv2::NewTemplate =
                    binary_sv2::from_bytes(&mut payload).expect("parse NewTemplate");
                assert!(
                    tmpl.coinbase_tx_value_remaining > 0,
                    "coinbase_tx_value_remaining must be positive (got {})",
                    tmpl.coinbase_tx_value_remaining
                );
                got_new_template = true;
            }
            MESSAGE_TYPE_SET_NEW_PREV_HASH => {
                let snph: template_distribution_sv2::SetNewPrevHash =
                    binary_sv2::from_bytes(&mut payload).expect("parse SetNewPrevHash");
                assert!(snph.n_bits > 0, "n_bits must be positive");
                assert!(
                    snph.prev_hash.inner_as_ref().iter().any(|&b| b != 0),
                    "prev_hash must not be all-zero"
                );
                got_set_new_prev_hash = true;
            }
            other => {
                eprintln!("sv2-tp: ignoring message type 0x{other:02x}");
            }
        }
    }
}
