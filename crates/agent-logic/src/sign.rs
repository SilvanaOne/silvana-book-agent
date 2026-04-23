//! Shared Ed25519 signing utilities for CLI sign subcommands
//!
//! Used by both `orderbook-agent` and `orderbook-cloud-agent`.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};

use crate::config::decode_private_key;

/// Resolve signing key: use override if provided, else config key bytes.
pub fn resolve_signing_key(
    config_key: &[u8; 32],
    private_key_override: Option<&str>,
) -> Result<SigningKey> {
    match private_key_override {
        Some(pk) => {
            let bytes = decode_private_key(pk)?;
            Ok(SigningKey::from_bytes(&bytes))
        }
        None => Ok(SigningKey::from_bytes(config_key)),
    }
}

/// Sign a 34-byte Canton multihash (base64-encoded input). Returns base64 signature.
pub fn sign_multihash(signing_key: &SigningKey, base64_input: &str) -> Result<String> {
    let bytes = BASE64
        .decode(base64_input.trim())
        .context("Invalid base64 encoding")?;
    anyhow::ensure!(
        bytes.len() == 34,
        "Multihash must be 34 bytes. Got {} bytes.",
        bytes.len()
    );
    let signature = signing_key.sign(&bytes);
    Ok(BASE64.encode(signature.to_bytes()))
}

/// Sign a text message (UTF-8 bytes). Returns base64 signature.
pub fn sign_message(signing_key: &SigningKey, message: &str) -> String {
    let signature = signing_key.sign(message.as_bytes());
    BASE64.encode(signature.to_bytes())
}

/// Sign binary data from hex string (with or without 0x prefix). Returns base64 signature.
pub fn sign_binary(signing_key: &SigningKey, hex_input: &str) -> Result<String> {
    let hex_str = hex_input
        .strip_prefix("0x")
        .or_else(|| hex_input.strip_prefix("0X"))
        .unwrap_or(hex_input);
    let bytes = hex::decode(hex_str).context("Invalid hex encoding")?;
    let signature = signing_key.sign(&bytes);
    Ok(BASE64.encode(signature.to_bytes()))
}

/// Generate a new Ed25519 keypair. Returns (private_key_base58, public_key_base58).
pub fn generate_keypair() -> (String, String) {
    use rand::rngs::OsRng;

    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    // Encode full 64-byte keypair (32-byte seed + 32-byte public key) as base58
    let mut keypair_bytes = [0u8; 64];
    keypair_bytes[..32].copy_from_slice(signing_key.as_bytes());
    keypair_bytes[32..].copy_from_slice(verifying_key.as_bytes());
    let private_key_b58 = bs58::encode(&keypair_bytes).into_string();

    let public_key_b58 = bs58::encode(verifying_key.as_bytes()).into_string();

    (private_key_b58, public_key_b58)
}
