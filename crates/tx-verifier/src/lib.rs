//! Transaction verification for Canton interactive submissions
//!
//! Verifies `PreparedTransaction` content and recomputes its hash before signing.
//!
//! Current behavior (debugging mode):
//! - Inspector accepts all transactions (stub)
//! - Hasher computes the full 3-layer SHA-256 hash
//! - On hash MATCH: return computed hash (caller signs it)
//! - On hash MISMATCH: WARN + return sentinel (caller falls back to server hash)
//! - Always `accepted: true` — never blocks transactions during debugging

pub mod decode;
pub mod hasher;
pub mod inspector;
pub mod types;

pub use types::{InspectionResult, OperationExpectation, VerificationResult};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use tracing::debug;

/// Verify a prepared transaction and compute its hash.
///
/// This is the main entry point called by agents before signing.
///
/// # Arguments
/// - `prepared_transaction_bytes`: Raw `PreparedTransaction` protobuf from server
/// - `server_hash_base64`: Server-provided hash (for comparison only, NEVER trusted)
/// - `hashing_scheme_version`: `"V2"` or `"V3"`
/// - `expectation`: What the agent expects this transaction to do
///
/// # Returns
/// `VerificationResult` with the self-computed hash to sign.
///
/// # Behavior
/// - Inspector accepts all transactions (stub — Phase A)
/// - Hasher computes the full 3-layer SHA-256 hash
/// - On hash match: returns computed hash (caller signs it — identical to server hash)
/// - On hash mismatch: returns `[0u8; 32]` sentinel (caller falls back to server hash)
/// - Always `accepted: true` — debugging mode, never blocks
pub fn verify_and_hash(
    prepared_transaction_bytes: &[u8],
    server_hash_base64: &str,
    hashing_scheme_version: &str,
    expectation: &OperationExpectation,
    verbose: bool,
) -> Result<VerificationResult> {
    let mut warnings = Vec::new();

    // Step 1: Inspect transaction fields against expectation
    let inspection = inspector::inspect(prepared_transaction_bytes, expectation, verbose)?;
    if !inspection.accepted {
        return Ok(VerificationResult {
            accepted: false,
            computed_hash: [0u8; 32],
            summary: inspection.summary,
            warnings: inspection.warnings,
            rejection_reason: inspection.rejection_reason,
        });
    }
    warnings.extend(inspection.warnings);

    // Step 2: Compute hash from PreparedTransaction
    let computed_hash = hasher::compute_hash(prepared_transaction_bytes, hashing_scheme_version)?;

    // Step 3: Compare with server hash (debugging mode — always accept)
    if computed_hash != [0u8; 32] {
        let server_hash_bytes = BASE64
            .decode(server_hash_base64)
            .map_err(|e| anyhow::anyhow!("Failed to decode server hash: {}", e))?;

        if computed_hash.as_slice() == server_hash_bytes.as_slice() {
            debug!("TX HASH MATCH: {}", hex::encode(computed_hash));
            return Ok(VerificationResult {
                accepted: true,
                computed_hash,
                summary: format!("{}\nHash verified — MATCH.", inspection.summary),
                warnings,
                rejection_reason: None,
            });
        } else {
            debug!(
                "TX HASH MISMATCH: server={}, computed={} — using server hash (debug mode)",
                hex::encode(&server_hash_bytes),
                hex::encode(computed_hash),
            );
            warnings.push(format!(
                "Hash mismatch: server={}, computed={}",
                hex::encode(&server_hash_bytes),
                hex::encode(computed_hash),
            ));
            // Return sentinel so caller falls back to server hash (safe)
            return Ok(VerificationResult {
                accepted: true,
                computed_hash: [0u8; 32],
                summary: format!("{}\nHash MISMATCH (debug mode — using server hash).", inspection.summary),
                warnings,
                rejection_reason: None,
            });
        }
    }

    // Hasher returned sentinel (computation failed) — fall back to server hash
    Ok(VerificationResult {
        accepted: true,
        computed_hash,
        summary: format!("{}\nHash computation failed — using server hash.", inspection.summary),
        warnings,
        rejection_reason: None,
    })
}
