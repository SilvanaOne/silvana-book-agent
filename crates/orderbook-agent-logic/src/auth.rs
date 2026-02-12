//! JWT authentication for orderbook agent
//!
//! Generates self-describing Ed25519-signed JWT tokens for authenticating with the orderbook service.
//! The public key is embedded in the JWT header's `jwk` field per RFC 8037.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD}, Engine};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a self-describing JWT token signed with Ed25519 private key
///
/// The JWT includes the public key in the header's `jwk` field per RFC 8037.
/// This allows the server to verify the token without needing the public key
/// to be sent separately.
///
/// # Arguments
/// * `party_id` - The Canton party ID (will be in `sub` claim)
/// * `role` - The party role (trader, operator, etc.)
/// * `private_key_bytes` - The Ed25519 private key as 32 bytes
/// * `ttl_secs` - Time to live in seconds
pub fn generate_jwt(
    party_id: &str,
    role: &str,
    private_key_bytes: &[u8; 32],
    ttl_secs: u64,
    node_name: Option<&str>,
) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System time error")?
        .as_secs();

    // Minimal claims for external auth
    let mut claims = serde_json::json!({
        "sub": party_id,
        "exp": now + ttl_secs,
        "iat": now,
        "role": role,
    });
    if let Some(nn) = node_name {
        claims["node_name"] = serde_json::Value::String(nn.to_string());
    }

    let signing_key = SigningKey::from_bytes(private_key_bytes);
    let public_key = signing_key.verifying_key();

    // Create JWT header with embedded JWK (RFC 8037 format)
    let header = serde_json::json!({
        "typ": "JWT",
        "alg": "EdDSA",
        "jwk": {
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(public_key.as_bytes())
        }
    });

    // Encode header and claims
    let header_json = serde_json::to_string(&header).context("Failed to serialize header")?;
    let claims_json = serde_json::to_string(&claims).context("Failed to serialize claims")?;

    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let claims_b64 = URL_SAFE_NO_PAD.encode(claims_json.as_bytes());

    // Create signing input
    let signing_input = format!("{}.{}", header_b64, claims_b64);

    // Sign with Ed25519
    let signature = signing_key.sign(signing_input.as_bytes());
    let signature_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

    // Combine into JWT
    let jwt = format!("{}.{}.{}", header_b64, claims_b64, signature_b64);

    Ok(jwt)
}

/// Decode Base58 Solana-style Ed25519 private key to 32-byte seed
///
/// Solana keys can be:
/// - 32 bytes: seed only
/// - 64 bytes: seed + public key (first 32 bytes are the seed)
pub fn decode_base58_private_key(base58_key: &str) -> Result<[u8; 32]> {
    let key_bytes = bs58::decode(base58_key.trim())
        .into_vec()
        .map_err(|e| anyhow!("Failed to decode Base58 private key: {}", e))?;

    if key_bytes.len() < 32 {
        return Err(anyhow!(
            "Private key too short: expected at least 32 bytes, got {}",
            key_bytes.len()
        ));
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&key_bytes[..32]);
    Ok(arr)
}

/// Get public key hex string from private key bytes
///
/// Derives the Ed25519 public key from the 32-byte seed and returns it as hex.
pub fn get_public_key_hex(private_key_bytes: &[u8; 32]) -> String {
    let signing_key = SigningKey::from_bytes(private_key_bytes);
    hex::encode(signing_key.verifying_key().as_bytes())
}

/// Extract user ID (sub claim) from a JWT token
pub fn extract_user_id_from_jwt(jwt: &str) -> Result<String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(anyhow!(
            "Invalid JWT format: expected 3 parts separated by '.', got {}",
            parts.len()
        ));
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| anyhow!("Failed to decode JWT payload: {}", e))?;

    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| anyhow!("Failed to parse JWT claims: {}", e))?;

    claims
        .get("sub")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("Missing 'sub' claim in JWT"))
}

/// Sign order data with Ed25519, returning base64-encoded signature
pub fn sign_order_data(private_key_bytes: &[u8; 32], signed_data: &[u8]) -> String {
    let signing_key = SigningKey::from_bytes(private_key_bytes);
    let signature = signing_key.sign(signed_data);
    STANDARD.encode(signature.to_bytes())
}

/// Verify order signature using the public key derived from private key bytes
pub fn verify_order_signature(
    private_key_bytes: &[u8; 32],
    signed_data: &[u8],
    signature_b64: &str,
) -> bool {
    let signing_key = SigningKey::from_bytes(private_key_bytes);
    let verifying_key = signing_key.verifying_key();
    verify_order_signature_with_public_key(&verifying_key, signed_data, signature_b64)
}

/// Verify order signature using a public key directly
pub fn verify_order_signature_with_public_key(
    public_key: &VerifyingKey,
    signed_data: &[u8],
    signature_b64: &str,
) -> bool {
    let sig_bytes = match STANDARD.decode(signature_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig_array: [u8; 64] = match sig_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
    public_key.verify(signed_data, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_jwt() {
        // Test with raw bytes (simulating decoded base58 key)
        let private_key_bytes: [u8; 32] = [
            0x0f, 0xe6, 0x65, 0xf7, 0xed, 0xb1, 0x93, 0xdb,
            0x35, 0xcc, 0x37, 0xd7, 0xd7, 0x03, 0xe1, 0x2a,
            0xe9, 0x4e, 0x9e, 0x1c, 0x5f, 0x5b, 0x88, 0x57,
            0xae, 0x1b, 0x6a, 0xca, 0x00, 0x5d, 0xf1, 0x5b,
        ];

        let jwt = generate_jwt(
            "test_party",
            "trader",
            &private_key_bytes,
            3600,
            None,
        )
        .expect("Failed to generate JWT");

        assert!(jwt.contains('.'));
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
    }
}
