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

pub fn canonical_params_prepay_traffic(
    amount: &str,
    description: Option<&str>,
    command_id: &str,
) -> String {
    let mut s = format!("param_type=PrepayTraffic\namount={}\n", amount);
    if let Some(d) = description {
        s.push_str(&format!("description={}\n", d));
    }
    s.push_str(&format!("command_id={}\n", command_id));
    s
}

pub fn canonical_params_request_preapproval(instrument_admin: &str) -> String {
    format!("param_type=RequestPreapproval\ninstrument_admin={}\n", instrument_admin)
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

pub fn canonical_params_execute_multicall(op_count: usize) -> String {
    format!(
        "param_type=ExecuteMultiCall\nop_count={}\n",
        op_count
    )
}

pub fn canonical_params_lock_holdings(lock_service_cid: &str, amount: &str, context: &str) -> String {
    format!(
        "param_type=LockHoldings\nlock_service_cid={}\namount={}\ncontext={}\n",
        lock_service_cid, amount, context
    )
}

pub fn canonical_params_process_lock_unlock_requests(lock_controller_cid: &str, request_count: usize) -> String {
    format!(
        "param_type=ProcessLockUnlockRequests\nlock_controller_cid={}\nrequest_count={}\n",
        lock_controller_cid, request_count
    )
}

pub fn canonical_params_resize_lock(lock_controller_cid: &str, new_amount: &str) -> String {
    format!(
        "param_type=ResizeLock\nlock_controller_cid={}\nnew_amount={}\n",
        lock_controller_cid, new_amount
    )
}

pub fn canonical_params_terminate_lock(lock_controller_cid: &str) -> String {
    format!(
        "param_type=TerminateLock\nlock_controller_cid={}\n",
        lock_controller_cid
    )
}

pub fn canonical_params_faucet(token_name: &str, token_admin: &str, ticket: &str, dry_run: bool) -> String {
    format!(
        "param_type=Faucet\ntoken_name={}\ntoken_admin={}\nticket={}\ndry_run={}\n",
        token_name, token_admin, ticket, dry_run
    )
}

// ============================================================================
// Canonical payload builders — RFQ V2 (AtomicDVP), PrepareAtomicTransactionRequest
//
// V2 operation identity is the params oneof ARM (the v1 TransactionOperation
// enum is not used), so the wrapper carries the param_type-bearing params
// canonical directly. Disclosure blobs are deliberately EXCLUDED from the
// canonicals: blob <-> contract-id integrity is enforced by Canton at exercise
// time; only the cids are covered.
// ============================================================================

/// Canonical payload for PrepareAtomicTransactionRequest.
pub fn canonical_prepare_atomic_request(params_canonical: &str) -> Vec<u8> {
    format!(
        "msg_type=PrepareAtomicTransactionRequest\n{}",
        params_canonical
    )
    .into_bytes()
}

/// One SIGNED LP-required fee as it rides the settle request canonical
/// (mirrors the rfqv2 `AtomicFeeSpec` / the v4 canonical fee block —
/// design §14 D18/D21).
pub struct AtomicLpFee<'a> {
    pub receiver: &'a str,
    pub instrument_admin: &'a str,
    pub instrument_id: &'a str,
    /// canonical DAML-show decimal string
    pub amount: &'a str,
}

#[allow(clippy::too_many_arguments)]
pub fn canonical_params_atomic_dvp_settle(
    venue_cid: &str,
    quote_id: &str,
    ticket_id: &str,
    user: &str,
    side: &str,
    base_amount: &str,
    quote_amount: &str,
    created_at_micros: i64,
    valid_until_micros: i64,
    quote_signature_der_hex: &str,
    ticket_cid: Option<&str>,
    lp_input_holding_cids: &[String],
    user_input_holding_cids: &[String],
    lp_fees: &[AtomicLpFee<'_>],
) -> String {
    let mut s = format!(
        "param_type=AtomicDvpSettle\nvenue_cid={}\nquote_id={}\nticket_id={}\nuser={}\nside={}\nbase_amount={}\nquote_amount={}\ncreated_at_micros={}\nvalid_until_micros={}\nquote_signature={}\n",
        venue_cid,
        quote_id,
        ticket_id,
        user,
        side,
        base_amount,
        quote_amount,
        created_at_micros,
        valid_until_micros,
        quote_signature_der_hex
    );
    if let Some(t) = ticket_cid {
        s.push_str(&format!("ticket_cid={}\n", t));
    }
    s.push_str(&format!("lp_input_holding_cids={}\n", lp_input_holding_cids.join(",")));
    s.push_str(&format!("user_input_holding_cids={}\n", user_input_holding_cids.join(",")));
    // Fee block, mirroring the v4 canonical-message layout; omitted entirely
    // when there are no fees (byte-compat with fee-less settles — design §14 D21).
    if !lp_fees.is_empty() {
        s.push_str(&format!("fee_count={}\n", lp_fees.len()));
        for (i, f) in lp_fees.iter().enumerate() {
            s.push_str(&format!("fee_{}_receiver={}\n", i, f.receiver));
            s.push_str(&format!("fee_{}_admin={}\n", i, f.instrument_admin));
            s.push_str(&format!("fee_{}_id={}\n", i, f.instrument_id));
            s.push_str(&format!("fee_{}_amount={}\n", i, f.amount));
        }
    }
    s
}

pub fn canonical_params_issue_tickets(ticket_ids: &[String]) -> String {
    format!(
        "param_type=IssueTickets\nticket_ids={}\n",
        ticket_ids.join(",")
    )
}

pub fn canonical_params_split_holdings(
    instrument_id: &str,
    instrument_admin: &str,
    splits: &[(String, u32)],
    input_holding_cids: &[String],
) -> String {
    let splits_joined = splits
        .iter()
        .map(|(amount, count)| format!("{}x{}", amount, count))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "param_type=SplitHoldings\ninstrument_id={}\ninstrument_admin={}\nsplits={}\ninput_holding_cids={}\n",
        instrument_id,
        instrument_admin,
        splits_joined,
        input_holding_cids.join(",")
    )
}

pub fn canonical_params_create_atomic_dvp_venue(
    pair_name: &str,
    base_instrument_id: &str,
    base_instrument_admin: &str,
    quote_instrument_id: &str,
    quote_instrument_admin: &str,
    quote_public_key_spki_hex: &str,
) -> String {
    // atomic-dvp-v2: the venue's provider is fixed by the AtomicDVPService
    // singleton — no operator_party line (design §13 D11).
    format!(
        "param_type=CreateAtomicDvpVenue\npair_name={}\nbase_instrument_id={}\nbase_instrument_admin={}\nquote_instrument_id={}\nquote_instrument_admin={}\nquote_public_key={}\n",
        pair_name,
        base_instrument_id,
        base_instrument_admin,
        quote_instrument_id,
        quote_instrument_admin,
        quote_public_key_spki_hex
    )
}

pub fn canonical_params_update_venue_key(venue_cid: &str, new_quote_public_key_spki_hex: &str) -> String {
    format!(
        "param_type=UpdateVenueKey\nvenue_cid={}\nnew_quote_public_key={}\n",
        venue_cid, new_quote_public_key_spki_hex
    )
}

pub fn canonical_params_retire_venue(venue_cid: &str) -> String {
    format!("param_type=RetireVenue\nvenue_cid={}\n", venue_cid)
}

pub fn canonical_params_cancel_tickets(ticket_cids: &[String]) -> String {
    format!(
        "param_type=CancelTickets\nticket_cids={}\n",
        ticket_cids.join(",")
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
// Canonical payload builders — PreparePayFee / ExecutePayFee
//
// Off-chain processing-fee payment flow. The two-signature model mirrors
// PrepareTransactionResponse / ExecuteTransactionRequest:
//   - request_signature / response_signature covers the FULL canonical
//     (proposal_id, fee_type, fees_json).
//   - fees_signature / fees_authorization covers ONLY the fees_json (via
//     `canonical_fees_authorization`), kept independent so future Canton
//     payloads can ride alongside without entanglement.
// ============================================================================

pub fn canonical_prepare_pay_fee_request(proposal_id: &str, fee_type: &str) -> Vec<u8> {
    format!(
        "msg_type=PreparePayFeeRequest\nproposal_id={}\nfee_type={}\n",
        proposal_id, fee_type
    )
    .into_bytes()
}

pub fn canonical_prepare_pay_fee_response(
    proposal_id: &str,
    fee_type: &str,
    fees_json: &str,
    session_id: &str,
) -> Vec<u8> {
    format!(
        "msg_type=PreparePayFeeResponse\nproposal_id={}\nfee_type={}\nfees_json={}\nsession_id={}\n",
        proposal_id, fee_type, fees_json, session_id
    )
    .into_bytes()
}

pub fn canonical_execute_pay_fee_request(
    proposal_id: &str,
    fee_type: &str,
    fees_json: &str,
    session_id: &str,
) -> Vec<u8> {
    format!(
        "msg_type=ExecutePayFeeRequest\nproposal_id={}\nfee_type={}\nfees_json={}\nsession_id={}\n",
        proposal_id, fee_type, fees_json, session_id
    )
    .into_bytes()
}

pub fn canonical_execute_pay_fee_response(success: bool, error_message: Option<&str>) -> Vec<u8> {
    let mut s = format!(
        "msg_type=ExecutePayFeeResponse\nsuccess={}\n",
        success
    );
    if let Some(e) = error_message {
        s.push_str(&format!("error_message={}\n", e));
    }
    s.into_bytes()
}

// ============================================================================
// Canonical payload builders — Fees Authorization (context-bound)
//
// Independent from the Canton-prepared payload signature so they can be
// added without disturbing the multihash Canton verifies. Each
// authorization is bound to:
//   - the `party_id` it applies to (prevents cross-party signature reuse),
//   - a server-issued single-use session id (prevents replay; the server
//     consumes the session atomically at execute time, so a replayed
//     signature lands on a missing session and is rejected),
//   - the `fees_json` schedule (prevents schedule substitution),
//   - the operation context (transaction_id for tx fees; proposal_id +
//     fee_type for pay-fee).
//
// `canonical_tx_fees_authorization` covers `PrepareTransactionResponse.
// fees_signature` and `ExecuteTransactionRequest.fees_authorization`.
// `canonical_pay_fee_authorization` covers `PreparePayFeeResponse.
// fees_signature` and `ExecutePayFeeRequest.fees_authorization`.
// ============================================================================

/// Bind a tx-time fees signature to the party + the existing
/// `transaction_id` session. The session is server-issued, party-scoped,
/// and single-use — `SessionStore::take` consumes it on execute, so any
/// replayed signature lands on a missing session and is rejected.
pub fn canonical_tx_fees_authorization(
    party_id: &str,
    transaction_id: &str,
    fees_json: &str,
) -> Vec<u8> {
    format!(
        "msg_type=TxFeesAuthorization\nparty_id={}\ntransaction_id={}\nfees_json={}\n",
        party_id, transaction_id, fees_json,
    )
    .into_bytes()
}

/// Bind a pay-fee authorization to the party + a server-issued
/// `session_id` + the operation's contextual identifiers. Domain-separated
/// from `canonical_tx_fees_authorization` by `msg_type`.
pub fn canonical_pay_fee_authorization(
    party_id: &str,
    session_id: &str,
    proposal_id: &str,
    fee_type: &str,
    fees_json: &str,
) -> Vec<u8> {
    format!(
        "msg_type=PayFeeAuthorization\nparty_id={}\nsession_id={}\nproposal_id={}\nfee_type={}\nfees_json={}\n",
        party_id, session_id, proposal_id, fee_type, fees_json,
    )
    .into_bytes()
}

/// Bind an off-chain debit authorization to (party, nonce, source_kind,
/// external_id, fees_json). Domain-separated from the canton-tx and
/// pay-fee canonicals by `msg_type`. Replay across sources is prevented
/// by including `source_kind` + `external_id` in the canonical bytes
/// (a signature for `subscription:sub-1` will not verify against
/// `ai_agent:sub-1`).
pub fn canonical_off_chain_authorization(
    party_id: &str,
    nonce: &str,
    source_kind: &str,
    external_id: &str,
    fees_json: &str,
) -> Vec<u8> {
    format!(
        "msg_type=OffChainAuthorization\nparty_id={}\nnonce={}\nsource_kind={}\nexternal_id={}\nfees_json={}\n",
        party_id, nonce, source_kind, external_id, fees_json,
    )
    .into_bytes()
}

/// Persisted-row view used by [`verify_persisted_fee_authorization`] to
/// reconstruct the canonical and re-verify a stored signature offline.
/// Fields are owned `String`s so this can be built directly from a SeaORM
/// model or a JSON dump without lifetime gymnastics.
///
/// Maps 1:1 onto the columns of `prepaid_traffic_authorization`. Pre-046
/// rows have `nonce = ""` and are intentionally rejected by
/// `verify_persisted_fee_authorization` — they predate the binding-evidence
/// being persisted, so they cannot be reconstructed.
#[derive(Debug, Clone)]
pub struct PersistedFeeAuthorization {
    /// UUIDv7 from `SessionStore` — the value the canonical was bound to.
    pub nonce: String,
    /// Canonical discriminator: "TxFeesAuthorization" | "PayFeeAuthorization".
    pub msg_type: String,
    /// Pay-fee canonical inputs (proposal_id + fee_type). None for tx-time.
    pub context_json: Option<serde_json::Value>,
    pub party_id: String,
    pub fees_json: String,
    pub signature_b64: String,
    pub public_key_b64url: String,
    pub signing_scheme: String,
}

/// Reconstruct the canonical bytes from a persisted auth row and verify
/// the stored signature offline. Branches on `msg_type` to pick the right
/// `canonical_*` builder.
///
/// Returns `Err` if:
/// - `nonce` is empty (pre-046 historical row — refuses to certify what
///   it cannot reconstruct).
/// - `msg_type` is unknown.
/// - `msg_type == "PayFeeAuthorization"` but `context_json` is missing
///   the `proposal_id`/`fee_type` fields.
/// - The signature does not verify against the rebuilt canonical.
pub fn verify_persisted_fee_authorization(row: &PersistedFeeAuthorization) -> Result<()> {
    if row.nonce.is_empty() {
        return Err(anyhow!(
            "Cannot verify pre-046 auth row: `nonce` is empty (the binding evidence was not persisted)"
        ));
    }

    let canonical = match row.msg_type.as_str() {
        "TxFeesAuthorization" => {
            canonical_tx_fees_authorization(&row.party_id, &row.nonce, &row.fees_json)
        }
        "PayFeeAuthorization" => {
            let ctx = row.context_json.as_ref().ok_or_else(|| {
                anyhow!("PayFeeAuthorization row missing context_json (need proposal_id + fee_type)")
            })?;
            let proposal_id = ctx
                .get("proposal_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("context_json.proposal_id missing or not a string"))?;
            let fee_type = ctx
                .get("fee_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("context_json.fee_type missing or not a string"))?;
            canonical_pay_fee_authorization(
                &row.party_id,
                &row.nonce,
                proposal_id,
                fee_type,
                &row.fees_json,
            )
        }
        "OffChainAuthorization" => {
            let ctx = row.context_json.as_ref().ok_or_else(|| {
                anyhow!("OffChainAuthorization row missing context_json (need source_kind + external_id)")
            })?;
            let source_kind = ctx
                .get("source_kind")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("context_json.source_kind missing or not a string"))?;
            let external_id = ctx
                .get("external_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("context_json.external_id missing or not a string"))?;
            canonical_off_chain_authorization(
                &row.party_id,
                &row.nonce,
                source_kind,
                external_id,
                &row.fees_json,
            )
        }
        other => return Err(anyhow!("Unknown msg_type '{}'", other)),
    };

    let public_key_bytes = parse_public_key(&row.public_key_b64url)
        .map_err(|e| anyhow!("Invalid public_key: {}", e))?;

    verify_canonical(
        &public_key_bytes,
        &canonical,
        &row.signature_b64,
        &row.signing_scheme,
    )
}

/// Deterministic JSON serialization of a fee schedule so server and client
/// agree byte-for-byte on what gets signed. Sorted by `entry_type` (stable
/// for ties), no whitespace, decimal strings preserved verbatim.
///
/// Each entry is `{"type":"<entry_type>","desc":"<description>","amount":"<amount_cc>"}`.
/// Field names are deliberately short to keep the payload small; both sides
/// must use this exact serializer.
pub fn serialize_fees_canonical<I, F>(fees: I) -> String
where
    I: IntoIterator<Item = F>,
    F: AsCanonicalFee,
{
    let mut entries: Vec<(String, String, String)> = fees
        .into_iter()
        .map(|f| (f.entry_type().to_string(), f.description().to_string(), f.amount_cc().to_string()))
        .collect();
    // Stable sort by (entry_type, description, amount_cc) — preserves order
    // among same-type fees while being deterministic across implementations.
    entries.sort();

    let mut out = String::from("[");
    for (i, (et, desc, amt)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"type":""#);
        json_escape_into(et, &mut out);
        out.push_str(r#"","desc":""#);
        json_escape_into(desc, &mut out);
        out.push_str(r#"","amount":""#);
        json_escape_into(amt, &mut out);
        out.push_str(r#""}"#);
    }
    out.push(']');
    out
}

/// Trait so `serialize_fees_canonical` can take any concrete `Fee` type
/// (the proto-generated one, or a domain struct) without taking a hard
/// dependency on the proto module here.
pub trait AsCanonicalFee {
    fn entry_type(&self) -> &str;
    fn description(&self) -> &str;
    fn amount_cc(&self) -> &str;
}

#[cfg(test)]
mod fees_canonical_tests {
    use super::*;

    struct TestFee {
        et: &'static str,
        d: &'static str,
        a: &'static str,
    }
    impl AsCanonicalFee for TestFee {
        fn entry_type(&self) -> &str { self.et }
        fn description(&self) -> &str { self.d }
        fn amount_cc(&self) -> &str { self.a }
    }

    #[test]
    fn serialize_fees_is_deterministic_and_sorted() {
        let a = vec![
            TestFee { et: "traffic", d: "Traffic fee", a: "0.001" },
            TestFee { et: "agent", d: "Agent fee (accept)", a: "0.035" },
        ];
        let b = vec![
            TestFee { et: "agent", d: "Agent fee (accept)", a: "0.035" },
            TestFee { et: "traffic", d: "Traffic fee", a: "0.001" },
        ];
        let s_a = serialize_fees_canonical(a);
        let s_b = serialize_fees_canonical(b);
        assert_eq!(s_a, s_b, "different input order must produce same canonical bytes");
        // Sorted: agent first
        assert!(s_a.starts_with(r#"[{"type":"agent""#));
    }

    #[test]
    fn serialize_fees_escapes_quotes_and_backslashes() {
        let f = vec![TestFee {
            et: "agent",
            d: r#"weird "desc" with \backslash"#,
            a: "1.0",
        }];
        let s = serialize_fees_canonical(f);
        assert!(s.contains(r#"\"desc\""#), "{}", s);
        assert!(s.contains(r"\\backslash"), "{}", s);
    }

    #[test]
    fn canonical_tx_fees_authorization_binds_party_and_transaction_id() {
        let bytes = canonical_tx_fees_authorization(
            "alice::1",
            "txn_abc",
            r#"[{"type":"agent","desc":"x","amount":"1"}]"#,
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("msg_type=TxFeesAuthorization\n"));
        assert!(s.contains("party_id=alice::1\n"));
        assert!(s.contains("transaction_id=txn_abc\n"));
        assert!(s.contains("fees_json="));
    }

    #[test]
    fn canonical_pay_fee_authorization_binds_full_context() {
        let bytes = canonical_pay_fee_authorization(
            "alice::1",
            "sess_uuid",
            "prop_xyz",
            "dvp",
            r#"[{"type":"dvp_processing","desc":"x","amount":"0.25"}]"#,
        );
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("msg_type=PayFeeAuthorization\n"));
        assert!(s.contains("party_id=alice::1\n"));
        assert!(s.contains("session_id=sess_uuid\n"));
        assert!(s.contains("proposal_id=prop_xyz\n"));
        assert!(s.contains("fee_type=dvp\n"));
        assert!(s.contains("fees_json="));
    }

    #[test]
    fn canonical_pay_fee_and_tx_are_domain_separated() {
        let json = r#"[]"#;
        let tx = canonical_tx_fees_authorization("p", "id", json);
        let pf = canonical_pay_fee_authorization("p", "id", "id", "id", json);
        assert_ne!(tx, pf, "different msg_types must produce different bytes");
    }

    #[test]
    fn empty_fees_serializes_to_empty_array() {
        let empty: Vec<TestFee> = Vec::new();
        assert_eq!(serialize_fees_canonical(empty), "[]");
    }
}

fn json_escape_into(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
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
    fn atomic_settle_canonical_fee_block() {
        let base = |fees: &[AtomicLpFee<'_>]| {
            canonical_params_atomic_dvp_settle(
                "00venue", "q-1", "t-1", "user::1", "Buy", "10.0", "25.5",
                1_000, 2_000, "30deadbeef", Some("00tkt"),
                &["00lp1".into()], &["00u1".into()], fees,
            )
        };
        // fee-less: byte-identical to the pre-fee layout (no fee_count line)
        let no_fees = base(&[]);
        assert!(!no_fees.contains("fee_count="));
        assert!(no_fees.ends_with("user_input_holding_cids=00u1\n"));
        // one fee: v4-shaped block appended after the cid lines
        let one = base(&[AtomicLpFee {
            receiver: "fee::1",
            instrument_admin: "dso::1",
            instrument_id: "Amulet",
            amount: "1.2",
        }]);
        assert!(one.starts_with(&no_fees));
        assert!(one.ends_with(
            "fee_count=1\nfee_0_receiver=fee::1\nfee_0_admin=dso::1\nfee_0_id=Amulet\nfee_0_amount=1.2\n"
        ));
    }

    #[test]
    fn verify_persisted_tx_fees_authorization_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key_b64url =
            URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

        let party_id = "party::abc";
        let nonce = "01958d4f-0000-7000-8000-000000000001"; // shape of UUIDv7
        let fees_json = r#"[{"type":"traffic","desc":"6105 bytes","amount":"0.349330902099609375"}]"#;

        let canonical = canonical_tx_fees_authorization(party_id, nonce, fees_json);
        let sig = sign_canonical(&private_key, &canonical);

        let row = PersistedFeeAuthorization {
            nonce: nonce.to_string(),
            msg_type: "TxFeesAuthorization".to_string(),
            context_json: None,
            party_id: party_id.to_string(),
            fees_json: fees_json.to_string(),
            signature_b64: sig.signature_b64.clone(),
            public_key_b64url: public_key_b64url.clone(),
            signing_scheme: SIGNING_SCHEME.to_string(),
        };
        verify_persisted_fee_authorization(&row).expect("Verification should succeed");

        // Tamper fees_json — must fail.
        let mut tampered = row.clone();
        tampered.fees_json = r#"[{"type":"traffic","desc":"6105 bytes","amount":"99999"}]"#.to_string();
        verify_persisted_fee_authorization(&tampered)
            .expect_err("Tampered fees_json must fail verification");

        // Tamper nonce — must fail.
        let mut tampered = row.clone();
        tampered.nonce = "01958d4f-0000-7000-8000-000000000002".to_string();
        verify_persisted_fee_authorization(&tampered)
            .expect_err("Tampered nonce must fail verification");
    }

    #[test]
    fn verify_persisted_pay_fee_authorization_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key_b64url =
            URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

        let party_id = "party::xyz";
        let nonce = "01958d50-0000-7000-8000-000000000001";
        let proposal_id = "prop-42";
        let fee_type = "dvp";
        let fees_json = r#"[{"type":"dvp_processing","desc":"DVP fee","amount":"0.5"}]"#;

        let canonical =
            canonical_pay_fee_authorization(party_id, nonce, proposal_id, fee_type, fees_json);
        let sig = sign_canonical(&private_key, &canonical);

        let row = PersistedFeeAuthorization {
            nonce: nonce.to_string(),
            msg_type: "PayFeeAuthorization".to_string(),
            context_json: Some(serde_json::json!({
                "proposal_id": proposal_id,
                "fee_type": fee_type,
            })),
            party_id: party_id.to_string(),
            fees_json: fees_json.to_string(),
            signature_b64: sig.signature_b64.clone(),
            public_key_b64url,
            signing_scheme: SIGNING_SCHEME.to_string(),
        };
        verify_persisted_fee_authorization(&row).expect("Verification should succeed");

        // Tamper proposal_id in context — must fail.
        let mut tampered = row.clone();
        tampered.context_json = Some(serde_json::json!({
            "proposal_id": "different-prop",
            "fee_type": fee_type,
        }));
        verify_persisted_fee_authorization(&tampered)
            .expect_err("Tampered proposal_id must fail verification");
    }

    #[test]
    fn verify_persisted_off_chain_authorization_roundtrip() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key_b64url =
            URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());

        let party_id = "party::sub";
        let nonce = "01958d51-0000-7000-8000-000000000001";
        let source_kind = "subscription";
        let external_id = "sub-12345";
        let fees_json = r#"[{"type":"subscription","desc":"Monthly fee","amount":"10.0"}]"#;

        let canonical = canonical_off_chain_authorization(
            party_id,
            nonce,
            source_kind,
            external_id,
            fees_json,
        );
        let sig = sign_canonical(&private_key, &canonical);

        let row = PersistedFeeAuthorization {
            nonce: nonce.to_string(),
            msg_type: "OffChainAuthorization".to_string(),
            context_json: Some(serde_json::json!({
                "source_kind": source_kind,
                "external_id": external_id,
            })),
            party_id: party_id.to_string(),
            fees_json: fees_json.to_string(),
            signature_b64: sig.signature_b64.clone(),
            public_key_b64url,
            signing_scheme: SIGNING_SCHEME.to_string(),
        };
        verify_persisted_fee_authorization(&row).expect("Verification should succeed");

        // Cross-source replay: same nonce + signature, different source_kind → must fail.
        let mut tampered = row.clone();
        tampered.context_json = Some(serde_json::json!({
            "source_kind": "ai_agent",
            "external_id": external_id,
        }));
        verify_persisted_fee_authorization(&tampered)
            .expect_err("Tampered source_kind must fail verification");

        // Tamper external_id — must fail.
        let mut tampered = row.clone();
        tampered.context_json = Some(serde_json::json!({
            "source_kind": source_kind,
            "external_id": "sub-99999",
        }));
        verify_persisted_fee_authorization(&tampered)
            .expect_err("Tampered external_id must fail verification");
    }

    #[test]
    fn canonical_off_chain_is_domain_separated_from_tx_and_pay_fee() {
        let off = canonical_off_chain_authorization(
            "p", "01958d52-0000-7000-8000-000000000001", "subscription", "sub-1",
            r#"[{"type":"x","desc":"y","amount":"1"}]"#,
        );
        let tx = canonical_tx_fees_authorization(
            "p", "01958d52-0000-7000-8000-000000000001",
            r#"[{"type":"x","desc":"y","amount":"1"}]"#,
        );
        let pay = canonical_pay_fee_authorization(
            "p", "01958d52-0000-7000-8000-000000000001", "sub-1", "subscription",
            r#"[{"type":"x","desc":"y","amount":"1"}]"#,
        );
        assert_ne!(off, tx);
        assert_ne!(off, pay);
        assert!(String::from_utf8_lossy(&off).starts_with("msg_type=OffChainAuthorization\n"));
    }

    #[test]
    fn verify_persisted_rejects_empty_nonce() {
        let row = PersistedFeeAuthorization {
            nonce: String::new(),
            msg_type: "TxFeesAuthorization".to_string(),
            context_json: None,
            party_id: "p".to_string(),
            fees_json: "[]".to_string(),
            signature_b64: String::new(),
            public_key_b64url: String::new(),
            signing_scheme: SIGNING_SCHEME.to_string(),
        };
        let err = verify_persisted_fee_authorization(&row).unwrap_err();
        assert!(err.to_string().contains("pre-046"), "{}", err);
    }

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
        assert!(!canonical_params_request_preapproval("DSO::1220abc").is_empty());
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
