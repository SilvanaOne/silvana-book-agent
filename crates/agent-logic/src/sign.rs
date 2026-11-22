//! Shared Ed25519 signing utilities for CLI sign subcommands
//!
//! Used by both `orderbook-agent` and `orderbook-cloud-agent`.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use zeroize::Zeroize;

use crate::secret::Secret;
use crate::signer::{AgentSigner, ONBOARDING_MULTIHASH_LEN};

/// The signer for the override key if one is given, else for the configured key.
pub fn resolve_signer(config_key: Option<&Secret<32>>, private_key_override: Option<&str>) -> Result<AgentSigner> {
    match (private_key_override, config_key) {
        (Some(pk), _) => AgentSigner::from_base58(pk),
        (None, Some(key)) => crate::signer::resolve(key),
        (None, None) => Err(anyhow!("no signing key configured")),
    }
}

/// Decode a base64 Canton multihash, which must be 34 bytes.
pub fn decode_multihash(base64_input: &str) -> Result<Vec<u8>> {
    let bytes = BASE64
        .decode(base64_input.trim())
        .context("Invalid base64 encoding")?;
    anyhow::ensure!(
        bytes.len() == ONBOARDING_MULTIHASH_LEN,
        "Multihash must be 34 bytes. Got {} bytes.",
        bytes.len()
    );
    Ok(bytes)
}

/// Sign a 34-byte Canton multihash (base64-encoded input). Returns base64 signature.
pub fn sign_multihash(signer: &AgentSigner, base64_input: &str) -> Result<String> {
    signer.sign_raw_cli(&decode_multihash(base64_input)?)
}

/// Sign a text message (UTF-8 bytes). Returns base64 signature.
pub fn sign_message(signer: &AgentSigner, message: &str) -> Result<String> {
    signer.sign_raw_cli(message.as_bytes())
}

/// Sign binary data from hex string (with or without 0x prefix). Returns base64 signature.
pub fn sign_binary(signer: &AgentSigner, hex_input: &str) -> Result<String> {
    let hex_str = hex_input
        .strip_prefix("0x")
        .or_else(|| hex_input.strip_prefix("0X"))
        .unwrap_or(hex_input);
    let bytes = hex::decode(hex_str).context("Invalid hex encoding")?;
    signer.sign_raw_cli(&bytes)
}

/// Generate a new Ed25519 keypair. Returns (private_key_base58, public_key_base58).
pub fn generate_keypair() -> Result<(String, String)> {
    generate_keypair_with(&mut rand::rngs::OsRng)
}

fn generate_keypair_with<R: RngCore>(rng: &mut R) -> Result<(String, String)> {
    let mut seed = [0u8; 32];
    if let Err(e) = rng.try_fill_bytes(&mut seed) {
        seed.zeroize();
        return Err(anyhow!("random source failed: {e}"));
    }
    let signing_key = SigningKey::from_bytes(&seed);
    seed.zeroize();

    // Full 64-byte keypair (32-byte seed + 32-byte public key) as base58
    let mut keypair_bytes = signing_key.to_keypair_bytes();
    let private_key_b58 = bs58::encode(&keypair_bytes).into_string();
    keypair_bytes.zeroize();

    let public_key_b58 = bs58::encode(signing_key.verifying_key().as_bytes()).into_string();

    Ok((private_key_b58, public_key_b58))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingRng;

    impl RngCore for FailingRng {
        fn next_u32(&mut self) -> u32 {
            0
        }
        fn next_u64(&mut self) -> u64 {
            0
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.fill(0);
        }
        fn try_fill_bytes(&mut self, _: &mut [u8]) -> Result<(), rand::Error> {
            Err(rand::Error::new(std::io::Error::other("no entropy")))
        }
    }

    #[test]
    fn a_failing_random_source_is_an_error() {
        let err = generate_keypair_with(&mut FailingRng).unwrap_err();
        assert!(err.to_string().contains("random source failed"), "{err}");
    }

    #[test]
    fn the_sign_helpers_use_the_override_or_the_configured_key() {
        use ed25519_dalek::Signer;
        let seed = [11u8; 32];
        let configured = Secret::seal(&mut { seed }).unwrap();
        let expected = |msg: &[u8]| BASE64.encode(SigningKey::from_bytes(&seed).sign(msg).to_bytes());
        let signer = resolve_signer(Some(&configured), None).unwrap();
        assert_eq!(sign_message(&signer, "hi").unwrap(), expected(b"hi"));
        assert_eq!(sign_binary(&signer, "0x0102").unwrap(), expected(&[1, 2]));
        let multihash = BASE64.encode([0x12u8; 34]);
        assert_eq!(sign_multihash(&signer, &multihash).unwrap(), expected(&[0x12u8; 34]));
        let err = sign_multihash(&signer, &BASE64.encode([0x12u8; 33])).unwrap_err().to_string();
        assert!(err.contains("Multihash must be 34 bytes. Got 33 bytes."), "{err}");

        let other = bs58::encode([12u8; 32]).into_string();
        let overridden = resolve_signer(Some(&configured), Some(&other)).unwrap();
        assert_eq!(overridden.public_key(), SigningKey::from_bytes(&[12u8; 32]).verifying_key().to_bytes());
        assert!(resolve_signer(None, None).is_err());
    }

    #[test]
    fn a_generated_keypair_round_trips() {
        use crate::config::decode_private_key;
        let (private_b58, public_b58) = generate_keypair().unwrap();
        let seed = decode_private_key(&private_b58).unwrap();
        let derived = SigningKey::from_bytes(&seed).verifying_key();
        assert_eq!(bs58::encode(derived.as_bytes()).into_string(), public_b58);
        let full = bs58::decode(&private_b58).into_vec().unwrap();
        assert_eq!(full.get(32..), Some(derived.as_bytes().as_slice()));
        assert_ne!(generate_keypair().unwrap().0, private_b58);
    }
}
