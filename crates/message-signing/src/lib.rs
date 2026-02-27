//! Message-level Ed25519 signing for gRPC request/response integrity
//!
//! Provides canonical payload construction, signing, and verification for the
//! PrepareTransaction/ExecuteTransaction RPC messages.
//!
//! ## Canonical Format
//!
//! Each message is serialized as `key=value\n` lines in a fixed order per message type.
//! Optional fields are omitted when absent. Binary fields are base64-encoded.
//! The SHA-256 hash of this canonical payload is signed with Ed25519.
//!
//! ## Signing Scheme
//!
//! `ed25519-sha256-v1`: Sign(Ed25519, SHA-256(canonical_payload))

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub const SIGNING_SCHEME: &str = "ed25519-sha256-v1";

/// Result of signing a canonical payload
pub struct MessageSignatureData {
    /// Base64-encoded Ed25519 signature (64 bytes → 88 chars)
    pub signature_b64: String,
    /// Base64url-encoded Ed25519 public key (32 bytes → 43 chars)
    pub public_key_b64url: String,
    /// Signing scheme identifier
    pub signing_scheme: String,
}

// ============================================================================
// Core sign/verify
// ============================================================================

/// Sign a canonical payload: SHA-256 hash then Ed25519 sign
pub fn sign_canonical(private_key: &[u8; 32], canonical: &[u8]) -> MessageSignatureData {
    let hash = Sha256::digest(canonical);
    let signing_key = SigningKey::from_bytes(private_key);
    let signature = signing_key.sign(&hash);
    let public_key = signing_key.verifying_key();

    MessageSignatureData {
        signature_b64: BASE64.encode(signature.to_bytes()),
        public_key_b64url: URL_SAFE_NO_PAD.encode(public_key.as_bytes()),
        signing_scheme: SIGNING_SCHEME.to_string(),
    }
}

/// Verify a message signature against a canonical payload
pub fn verify_canonical(
    public_key_bytes: &[u8; 32],
    canonical: &[u8],
    signature_b64: &str,
    signing_scheme: &str,
) -> Result<()> {
    if signing_scheme != SIGNING_SCHEME {
        return Err(anyhow!(
            "Unsupported signing scheme '{}', expected '{}'",
            signing_scheme,
            SIGNING_SCHEME
        ));
    }

    let hash = Sha256::digest(canonical);

    let verifying_key = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| anyhow!("Invalid public key: {}", e))?;

    let sig_bytes = BASE64
        .decode(signature_b64)
        .map_err(|e| anyhow!("Invalid signature base64: {}", e))?;

    if sig_bytes.len() != 64 {
        return Err(anyhow!(
            "Invalid signature length: expected 64 bytes, got {}",
            sig_bytes.len()
        ));
    }

    let sig_array: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow!("Failed to convert signature to array"))?;

    let signature = Signature::from_bytes(&sig_array);

    verifying_key
        .verify(&hash, &signature)
        .map_err(|_| anyhow!("Message signature verification failed"))
}

/// Parse a base64url-encoded public key to bytes
pub fn parse_public_key(public_key_b64url: &str) -> Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(public_key_b64url)
        .map_err(|e| anyhow!("Invalid public key base64url: {}", e))?;

    if bytes.len() != 32 {
        return Err(anyhow!(
            "Invalid public key length: expected 32 bytes, got {}",
            bytes.len()
        ));
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

// ============================================================================
// Canonical payload builders — PrepareTransactionRequest
// ============================================================================

/// Canonical payload for PrepareTransactionRequest.
///
/// The `params_canonical` is built by the caller using one of the
/// `canonical_params_*` functions below.
pub fn canonical_prepare_request(operation: i32, params_canonical: &str) -> Vec<u8> {
    format!(
        "msg_type=PrepareTransactionRequest\noperation={}\n{}",
        operation, params_canonical
    )
    .into_bytes()
}

pub fn canonical_params_pay_fee(proposal_id: &str, fee_type: &str) -> String {
    format!(
        "param_type=PayFee\nproposal_id={}\nfee_type={}\n",
        proposal_id, fee_type
    )
}

pub fn canonical_params_propose_dvp(proposal_id: &str) -> String {
    format!("param_type=ProposeDvp\nproposal_id={}\n", proposal_id)
}

pub fn canonical_params_accept_dvp(proposal_id: &str, dvp_proposal_cid: &str) -> String {
    format!(
        "param_type=AcceptDvp\nproposal_id={}\ndvp_proposal_cid={}\n",
        proposal_id, dvp_proposal_cid
    )
}

pub fn canonical_params_allocate(proposal_id: &str, dvp_cid: &str) -> String {
    format!(
        "param_type=Allocate\nproposal_id={}\ndvp_cid={}\n",
        proposal_id, dvp_cid
    )
}

pub fn canonical_params_transfer_cc(
    receiver_party: &str,
    amount: &str,
    description: Option<&str>,
    command_id: &str,
    settlement_proposal_id: Option<&str>,
) -> String {
    let mut s = format!(
        "param_type=TransferCc\nreceiver_party={}\namount={}\n",
        receiver_party, amount
    );
    if let Some(d) = description {
        s.push_str(&format!("description={}\n", d));
    }
    s.push_str(&format!("command_id={}\n", command_id));
    if let Some(sid) = settlement_proposal_id {
        s.push_str(&format!("settlement_proposal_id={}\n", sid));
    }
    s
}

pub fn canonical_params_request_preapproval() -> String {
    "param_type=RequestPreapproval\n".to_string()
}

pub fn canonical_params_request_recurring_prepaid(
    app_party: &str,
    amount: &str,
    locked_amount: &str,
    lock_days: u32,
    description: Option<&str>,
    reference: Option<&str>,
) -> String {
    let mut s = format!(
        "param_type=RequestRecurringPrepaid\napp_party={}\namount={}\nlocked_amount={}\nlock_days={}\n",
        app_party, amount, locked_amount, lock_days
    );
    if let Some(d) = description {
        s.push_str(&format!("description={}\n", d));
    }
    if let Some(r) = reference {
        s.push_str(&format!("reference={}\n", r));
    }
    s
}

pub fn canonical_params_request_recurring_payasyougo(
    app_party: &str,
    amount: &str,
    description: Option<&str>,
    reference: Option<&str>,
) -> String {
    let mut s = format!(
        "param_type=RequestRecurringPayasyougo\napp_party={}\namount={}\n",
        app_party, amount
    );
    if let Some(d) = description {
        s.push_str(&format!("description={}\n", d));
    }
    if let Some(r) = reference {
        s.push_str(&format!("reference={}\n", r));
    }
    s
}

pub fn canonical_params_request_user_service(
    reference_id: Option<&str>,
    party_name: Option<&str>,
) -> String {
    let mut s = "param_type=RequestUserService\n".to_string();
    if let Some(r) = reference_id {
        s.push_str(&format!("reference_id={}\n", r));
    }
    if let Some(p) = party_name {
        s.push_str(&format!("party_name={}\n", p));
    }
    s
}

pub fn canonical_params_transfer_cip56(
    instrument_id: &str,
    instrument_admin: &str,
    receiver_party: &str,
    amount: &str,
    reference: Option<&str>,
) -> String {
    let mut s = format!(
        "param_type=TransferCip56\ninstrument_id={}\ninstrument_admin={}\nreceiver_party={}\namount={}\n",
        instrument_id, instrument_admin, receiver_party, amount
    );
    if let Some(r) = reference {
        s.push_str(&format!("reference={}\n", r));
    }
    s
}

pub fn canonical_params_accept_cip56(contract_id: &str) -> String {
    format!("param_type=AcceptCip56\ncontract_id={}\n", contract_id)
}

pub fn canonical_params_split_cc(output_amounts: &[String]) -> String {
    format!(
        "param_type=SplitCc\noutput_amounts={}\n",
        output_amounts.join(",")
    )
}

// ============================================================================
// Canonical payload builders — Onboarding RPCs
//
// Used by RegisterAgent, GetOnboardingStatus, and SubmitOnboardingSignature.
// These are standalone messages (not PrepareTransactionRequest), so they use
// their own msg_type and include public_key as the caller identity.
// ============================================================================

/// Canonical payload for RegisterAgent request.
pub fn canonical_register_agent(public_key: &str, invite_code: Option<&str>) -> Vec<u8> {
    let mut s = format!(
        "msg_type=RegisterAgent\npublic_key={}\n",
        public_key
    );
    if let Some(code) = invite_code {
        s.push_str(&format!("invite_code={}\n", code));
    }
    s.into_bytes()
}

/// Canonical payload for GetOnboardingStatus request.
pub fn canonical_get_onboarding_status(public_key: &str) -> Vec<u8> {
    format!(
        "msg_type=GetOnboardingStatus\npublic_key={}\n",
        public_key
    )
    .into_bytes()
}

/// Canonical payload for SubmitOnboardingSignature request.
pub fn canonical_submit_onboarding_signature(
    public_key: &str,
    multihash_signature: &str,
) -> Vec<u8> {
    format!(
        "msg_type=SubmitOnboardingSignature\npublic_key={}\nmultihash_signature={}\n",
        public_key, multihash_signature
    )
    .into_bytes()
}

// ============================================================================
// Canonical payload builders — PrepareTransactionResponse
// ============================================================================

pub fn canonical_prepare_response(
    transaction_id: &str,
    prepared_transaction_hash: &str,
    command_id: &str,
    prepared_transaction: &[u8],
    hashing_scheme_version: &str,
) -> Vec<u8> {
    format!(
        "msg_type=PrepareTransactionResponse\ntransaction_id={}\nprepared_transaction_hash={}\ncommand_id={}\nprepared_transaction={}\nhashing_scheme_version={}\n",
        transaction_id,
        prepared_transaction_hash,
        BASE64.encode(prepared_transaction),
        command_id,
        hashing_scheme_version,
    )
    .into_bytes()
}

// ============================================================================
// Canonical payload builders — ExecuteTransactionRequest
// ============================================================================

pub fn canonical_execute_request(transaction_id: &str, signature: &str) -> Vec<u8> {
    format!(
        "msg_type=ExecuteTransactionRequest\ntransaction_id={}\nsignature={}\n",
        transaction_id, signature
    )
    .into_bytes()
}

// ============================================================================
// Canonical payload builders — ExecuteTransactionResponse
// ============================================================================

pub fn canonical_execute_response(
    success: bool,
    update_id: &str,
    contract_id: Option<&str>,
    error_message: Option<&str>,
    rewards_amount: Option<&str>,
    rewards_round: Option<u64>,
) -> Vec<u8> {
    let mut s = format!(
        "msg_type=ExecuteTransactionResponse\nsuccess={}\nupdate_id={}\n",
        success, update_id
    );
    if let Some(cid) = contract_id {
        s.push_str(&format!("contract_id={}\n", cid));
    }
    if let Some(err) = error_message {
        s.push_str(&format!("error_message={}\n", err));
    }
    if let Some(ra) = rewards_amount {
        s.push_str(&format!("rewards_amount={}\n", ra));
    }
    if let Some(rr) = rewards_round {
        s.push_str(&format!("rewards_round={}\n", rr));
    }
    s.into_bytes()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn test_sign_verify_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key = signing_key.verifying_key().to_bytes();

        let canonical = canonical_prepare_request(
            6,
            &canonical_params_transfer_cc("receiver123", "100.0", Some("test"), "cmd-1", None),
        );

        let sig = sign_canonical(&private_key, &canonical);
        assert_eq!(sig.signing_scheme, SIGNING_SCHEME);

        // Verify succeeds
        verify_canonical(&public_key, &canonical, &sig.signature_b64, &sig.signing_scheme)
            .expect("Verification should succeed");
    }

    #[test]
    fn test_tampered_payload_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key = signing_key.verifying_key().to_bytes();

        let canonical = canonical_prepare_request(
            6,
            &canonical_params_transfer_cc("receiver123", "100.0", None, "cmd-1", None),
        );

        let sig = sign_canonical(&private_key, &canonical);

        // Tamper with payload
        let tampered = canonical_prepare_request(
            6,
            &canonical_params_transfer_cc("attacker", "100.0", None, "cmd-1", None),
        );

        let result =
            verify_canonical(&public_key, &tampered, &sig.signature_b64, &sig.signing_scheme);
        assert!(result.is_err(), "Tampered payload should fail verification");
    }

    #[test]
    fn test_wrong_key_fails() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();

        let other_key = SigningKey::generate(&mut OsRng);
        let other_public = other_key.verifying_key().to_bytes();

        let canonical = canonical_execute_request("tx-123", "sig-abc");
        let sig = sign_canonical(&private_key, &canonical);

        let result =
            verify_canonical(&other_public, &canonical, &sig.signature_b64, &sig.signing_scheme);
        assert!(result.is_err(), "Wrong key should fail verification");
    }

    #[test]
    fn test_canonical_determinism() {
        let c1 = canonical_params_transfer_cc("recv", "50", Some("desc"), "cmd", Some("prop-1"));
        let c2 = canonical_params_transfer_cc("recv", "50", Some("desc"), "cmd", Some("prop-1"));
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_canonical_optional_fields() {
        let with = canonical_params_transfer_cc("recv", "50", Some("desc"), "cmd", None);
        let without = canonical_params_transfer_cc("recv", "50", None, "cmd", None);
        assert!(with.contains("description=desc"));
        assert!(!without.contains("description="));
    }

    #[test]
    fn test_parse_public_key_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = signing_key.verifying_key().to_bytes();
        let encoded = URL_SAFE_NO_PAD.encode(public_key);
        let decoded = parse_public_key(&encoded).expect("Should decode");
        assert_eq!(decoded, public_key);
    }

    #[test]
    fn test_all_param_types_produce_output() {
        assert!(!canonical_params_pay_fee("p1", "dvp").is_empty());
        assert!(!canonical_params_propose_dvp("p1").is_empty());
        assert!(!canonical_params_accept_dvp("p1", "cid").is_empty());
        assert!(!canonical_params_allocate("p1", "cid").is_empty());
        assert!(!canonical_params_request_preapproval().is_empty());
        assert!(!canonical_params_request_recurring_prepaid("app", "10", "5", 30, None, None).is_empty());
        assert!(!canonical_params_request_recurring_payasyougo("app", "10", None, None).is_empty());
        assert!(!canonical_params_request_user_service(None, None).is_empty());
        assert!(!canonical_params_transfer_cip56("USDC", "admin", "recv", "100", None).is_empty());
        assert!(!canonical_params_accept_cip56("cid").is_empty());
    }

    #[test]
    fn test_execute_response_canonical() {
        let c = canonical_execute_response(true, "update-1", Some("cid-1"), None, Some("1.5"), Some(42));
        let s = String::from_utf8(c).unwrap();
        assert!(s.contains("success=true"));
        assert!(s.contains("update_id=update-1"));
        assert!(s.contains("contract_id=cid-1"));
        assert!(!s.contains("error_message="));
        assert!(s.contains("rewards_amount=1.5"));
        assert!(s.contains("rewards_round=42"));
    }

    #[test]
    fn test_unsupported_scheme_rejected() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key = signing_key.verifying_key().to_bytes();
        let canonical = b"test";
        let sig = sign_canonical(&private_key, canonical);

        let result = verify_canonical(&public_key, canonical, &sig.signature_b64, "rsa-sha256-v1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported signing scheme"));
    }
}
