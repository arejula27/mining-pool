//! Noise-encrypted TCP connection for the SV2 Mining Protocol (responder side).
//!
//! After a successful Noise NX handshake, all SV2 frames are encrypted and decrypted
//! transparently. Callers use [`NoiseReadHalf::read_frame`] and
//! [`NoiseWriteHalf::write_sv2_message`] to exchange frames without dealing with the
//! encryption layer directly.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use codec_sv2::{HandshakeRole, NoiseEncoder, StandardEitherFrame, StandardNoiseDecoder, State};
use framing_sv2::framing::{Frame, HandShakeFrame, Sv2Frame};
use noise_sv2::{ELLSWIFT_ENCODING_SIZE, INITIATOR_EXPECTED_HANDSHAKE_MESSAGE_SIZE};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
};

// ── Type aliases ──────────────────────────────────────────────────────────────
//
// `StandardNoiseDecoder<T>` and `NoiseEncoder<T>` require `T: Serialize + GetSize +
// Deserialize`. We never actually instantiate a value of T from the network; the
// codec produces frames with `serialized: Some(bytes)` and `payload: None`, so the
// phantom type parameter is irrelevant for actual I/O. Using `u32` satisfies all
// bounds without any custom derive.

type Decoder = StandardNoiseDecoder<u32>;
type Encoder = NoiseEncoder<u32>;
pub type EitherFrame = StandardEitherFrame<u32>;

// ── Noise read half ───────────────────────────────────────────────────────────

/// Reading half of a Noise-secured SV2 TCP connection.
pub struct NoiseReadHalf {
    reader: OwnedReadHalf,
    decoder: Decoder,
    state: State,
    buf: Vec<u8>,
    bytes_read: usize,
}

impl NoiseReadHalf {
    /// Reads and returns the next complete SV2 frame (decrypted).
    pub async fn read_frame(&mut self) -> Result<EitherFrame> {
        loop {
            let expected = self.decoder.writable_len();
            if self.buf.len() != expected {
                self.buf.resize(expected, 0);
                self.bytes_read = 0;
            }

            while self.bytes_read < expected {
                let n = self
                    .reader
                    .read(&mut self.buf[self.bytes_read..])
                    .await
                    .context("read error")?;
                if n == 0 {
                    bail!("connection closed");
                }
                self.bytes_read += n;
            }

            self.decoder.writable().copy_from_slice(&self.buf);
            self.bytes_read = 0;

            match self.decoder.next_frame(&mut self.state) {
                Ok(frame) => return Ok(frame),
                Err(codec_sv2::Error::MissingBytes(_)) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(e) => bail!("frame decode error: {e:?}"),
            }
        }
    }
}

// ── Noise write half ──────────────────────────────────────────────────────────

/// Writing half of a Noise-secured SV2 TCP connection.
pub struct NoiseWriteHalf {
    writer: OwnedWriteHalf,
    encoder: Encoder,
    state: State,
}

impl NoiseWriteHalf {
    /// Serializes `msg` into an SV2 frame and sends it encrypted.
    ///
    /// `msg_type` must be one of the `MESSAGE_TYPE_*` constants from `common_messages_sv2`
    /// or `mining_sv2`. Set `is_channel_msg` to `true` for messages that carry a `channel_id`
    /// in the first four bytes of the payload (i.e. when the `channel_msg` bit in
    /// `extension_type` should be set).
    pub async fn write_sv2_message<T>(
        &mut self,
        msg: T,
        msg_type: u8,
        is_channel_msg: bool,
    ) -> Result<()>
    where
        T: binary_sv2::Serialize + binary_sv2::GetSize + 'static,
    {
        use framing_sv2::header::Header;

        let payload_len = msg.get_size();
        let total_len = Header::SIZE + payload_len;
        let mut dst = vec![0u8; total_len];

        // Serialize header + payload.  `B = Vec<u8>` is never written into since
        // `serialized` is `None`; only `payload` is used by `serialize`.
        let frame = Sv2Frame::<T, Vec<u8>>::from_message(msg, msg_type, 0, is_channel_msg)
            .context("payload too large for SV2 frame")?;
        frame
            .serialize(&mut dst)
            .map_err(|e| anyhow::anyhow!("SV2 frame serialize error: {e:?}"))?;

        // Wrap the serialized bytes in a Sv2Frame so the Noise encoder can encrypt them.
        // StandardEitherFrame<u32> = Frame<u32, Vec<u8>> (buffer_sv2 2.x Slice = Vec<u8>).
        let sv2_frame = Sv2Frame::<u32, Vec<u8>>::from_bytes_unchecked(dst);
        let either_frame: EitherFrame = Frame::Sv2(sv2_frame);

        let encoded = self
            .encoder
            .encode(either_frame, &mut self.state)
            .map_err(|e| anyhow::anyhow!("Noise encode error: {e:?}"))?;

        self.writer
            .write_all(encoded.as_ref())
            .await
            .context("write error")?;

        Ok(())
    }

    /// Sends an already-serialized SV2 frame (header + payload bytes) encrypted.
    ///
    /// Use this when you have raw SV2 frame bytes (e.g. from `Sv2Frame::serialize`)
    /// and want to send them without an additional `T: 'static` constraint.
    pub async fn write_raw_frame(&mut self, bytes: Vec<u8>) -> Result<()> {
        let sv2_frame = Sv2Frame::<u32, Vec<u8>>::from_bytes_unchecked(bytes);
        let either_frame: EitherFrame = Frame::Sv2(sv2_frame);
        let encoded = self
            .encoder
            .encode(either_frame, &mut self.state)
            .map_err(|e| anyhow::anyhow!("Noise encode error: {e:?}"))?;
        self.writer
            .write_all(encoded.as_ref())
            .await
            .context("write error")?;
        Ok(())
    }
}

// ── Handshake ─────────────────────────────────────────────────────────────────

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Performs the Noise NX handshake as the **responder** (pool side).
///
/// Returns `(read_half, write_half)` ready for SV2 message exchange.
pub async fn accept_noise(stream: TcpStream, role: HandshakeRole) -> Result<(NoiseReadHalf, NoiseWriteHalf)> {
    accept_noise_with_timeout(stream, role, HANDSHAKE_TIMEOUT).await
}

pub async fn accept_noise_with_timeout(
    stream: TcpStream,
    role: HandshakeRole,
    timeout: Duration,
) -> Result<(NoiseReadHalf, NoiseWriteHalf)> {
    let (mut reader, mut writer) = stream.into_split();

    let mut decoder = Decoder::new();
    let mut encoder = Encoder::new();

    // `state` is used for the active handshake role (Responder).
    let mut state = State::initialized(role.clone());
    // `handshake_reader_state` tells the decoder how many bytes to expect for
    // the *incoming* handshake message.  For Responder that is 64 bytes
    // (ELLSWIFT_ENCODING_SIZE: the initiator's ephemeral ElligatorSwift key).
    let mut reader_state = State::not_initialized(&role);

    // --- Read the initiator's first handshake message (64 bytes) ---
    // The decoder starts with missing_noise_b = 0.  The first call to next_frame
    // returns MissingBytes and sets the expected byte count; the second call (after
    // reading the bytes) returns the HandShakeFrame.
    let handshake_frame: HandShakeFrame = loop {
        let expected = decoder.writable_len();
        if expected > 0 {
            let mut buf = vec![0u8; expected];
            tokio::time::timeout(timeout, reader.read_exact(&mut buf))
                .await
                .context("handshake timeout")?
                .context("handshake read")?;
            decoder.writable().copy_from_slice(&buf);
        }
        match decoder.next_frame(&mut reader_state) {
            Ok(Frame::HandShake(f)) => break f,
            Ok(Frame::Sv2(_)) => bail!("expected HandShake frame, got Sv2 frame"),
            Err(codec_sv2::Error::MissingBytes(_)) => continue,
            Err(e) => bail!("handshake frame decode: {e:?}"),
        }
    };

    let payload: [u8; ELLSWIFT_ENCODING_SIZE] = handshake_frame
        .get_payload_when_handshaking()
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrong handshake payload size"))?;

    // --- Compute and send the responder's reply ---
    let (reply_frame, transport_state) = state
        .step_1(payload)
        .map_err(|e| anyhow::anyhow!("Noise step_1 error: {e:?}"))?;

    let encoded = encoder
        .encode(reply_frame.into(), &mut state)
        .map_err(|e| anyhow::anyhow!("Noise encode handshake reply: {e:?}"))?;
    writer
        .write_all(encoded.as_ref())
        .await
        .context("write handshake reply")?;

    state = transport_state;

    Ok((
        NoiseReadHalf {
            reader,
            decoder: Decoder::new(),
            state: state.clone(),
            buf: Vec::new(),
            bytes_read: 0,
        },
        NoiseWriteHalf {
            writer,
            encoder,
            state,
        },
    ))
}

// ── Initiator (client) handshake ──────────────────────────────────────────────

/// Performs the Noise NX handshake as the **initiator** (client side).
///
/// `role` must be `HandshakeRole::Initiator(...)`.
/// Returns `(read_half, write_half)` ready for SV2 message exchange.
pub async fn connect_noise(
    stream: TcpStream,
    role: HandshakeRole,
) -> Result<(NoiseReadHalf, NoiseWriteHalf)> {
    let (mut reader, mut writer) = stream.into_split();

    let mut decoder = Decoder::new();
    let mut encoder = Encoder::new();

    let mut state = State::initialized(role.clone());
    // reader_state for initiator: expects INITIATOR_EXPECTED_HANDSHAKE_MESSAGE_SIZE bytes
    let mut reader_state = State::not_initialized(&role);

    // --- Step 0: send the initiator's ephemeral key (64 bytes) ---
    let step0_frame = state
        .step_0()
        .map_err(|e| anyhow::anyhow!("Noise step_0 error: {e:?}"))?;
    let encoded = encoder
        .encode(Frame::HandShake(step0_frame), &mut state)
        .map_err(|e| anyhow::anyhow!("Noise encode step0: {e:?}"))?;
    writer
        .write_all(encoded.as_ref())
        .await
        .context("write Noise step0")?;

    // --- Step 2: read the responder's reply and complete the handshake ---
    let responder_frame: HandShakeFrame = loop {
        let expected = decoder.writable_len();
        if expected > 0 {
            let mut buf = vec![0u8; expected];
            reader
                .read_exact(&mut buf)
                .await
                .context("read Noise handshake reply")?;
            decoder.writable().copy_from_slice(&buf);
        }
        match decoder.next_frame(&mut reader_state) {
            Ok(Frame::HandShake(f)) => break f,
            Ok(Frame::Sv2(_)) => bail!("expected HandShake frame, got Sv2 frame"),
            Err(codec_sv2::Error::MissingBytes(_)) => continue,
            Err(e) => bail!("handshake reply decode: {e:?}"),
        }
    };

    let payload: [u8; INITIATOR_EXPECTED_HANDSHAKE_MESSAGE_SIZE] = responder_frame
        .get_payload_when_handshaking()
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrong handshake reply size"))?;

    let transport_state = state
        .step_2(payload)
        .map_err(|e| anyhow::anyhow!("Noise step_2 error: {e:?}"))?;

    state = transport_state;

    Ok((
        NoiseReadHalf {
            reader,
            decoder: Decoder::new(),
            state: state.clone(),
            buf: Vec::new(),
            bytes_read: 0,
        },
        NoiseWriteHalf {
            writer,
            encoder,
            state,
        },
    ))
}
