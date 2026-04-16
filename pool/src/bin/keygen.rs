//! Generates a fresh secp256k1 authority keypair for the SV2 Noise NX handshake.
//!
//! Prints two lines ready to paste into the environment:
//!   POOL_AUTHORITY_PUBLIC_KEY=<64 hex chars>
//!   POOL_AUTHORITY_PRIVATE_KEY=<64 hex chars>
//!
//! Usage:  just keygen

use secp256k1::{rand::thread_rng, Keypair, Secp256k1};

fn main() {
    let secp = Secp256k1::new();
    let kp = Keypair::new(&secp, &mut thread_rng());

    // X-only public key: 32 bytes, even-parity representation used by noise_sv2.
    let (xonly, _parity) = kp.x_only_public_key();
    let pub_hex = hex::encode(xonly.serialize());

    // Raw private key bytes: 32 bytes.
    let priv_hex = hex::encode(kp.secret_key().secret_bytes());

    println!("POOL_AUTHORITY_PUBLIC_KEY={pub_hex}");
    println!("POOL_AUTHORITY_PRIVATE_KEY={priv_hex}");
}
