//! Types for transaction verification
//!
//! `OperationExpectation` describes what the agent expects a transaction to do.
//! `VerificationResult` is the output of `verify_and_hash()`.

/// What the agent expects the prepared transaction to do.
/// Built from the `PrepareTransactionRequest` params + local config.
#[derive(Debug, Clone, serde::Serialize)]
pub enum OperationExpectation {
    PayFee {
        sender_party: String,
        fee_party: String,
        proposal_id: String,
        fee_type: String, // "dvp" or "allocate"
    },
    ProposeDvp {
        buyer_party: String,
        seller_party: String,
        proposal_id: String,
        synchronizer_id: String,
    },
    AcceptDvp {
        seller_party: String,
        proposal_id: String,
        dvp_proposal_cid: String,
    },
    Allocate {
        party: String,
        proposal_id: String,
        dvp_cid: String,
    },
    TransferCc {
        sender_party: String,
        receiver_party: String,
        amount: String,
        command_id: String,
    },
    RequestPreapproval {
        party: String,
    },
    RequestRecurringPrepaid {
        party: String,
        app_party: String,
        amount: String,
    },
    RequestRecurringPayasyougo {
        party: String,
        app_party: String,
        amount: String,
    },
    RequestUserService {
        party: String,
    },
    TransferCip56 {
        sender_party: String,
        receiver_party: String,
        instrument_id: String,
        instrument_admin: String,
        amount: String,
    },
    AcceptCip56 {
        receiver_party: String,
        contract_id: String,
    },
    SplitCc {
        party: String,
        output_amounts: Vec<String>,
    },
}

/// Result of transaction verification
#[derive(Debug)]
pub struct VerificationResult {
    /// Whether all checks passed
    pub accepted: bool,
    /// Computed hash (32 bytes) — sign THIS, not the server hash.
    /// `[0u8; 32]` is a sentinel meaning "Phase A stub, use server hash".
    pub computed_hash: [u8; 32],
    /// Human-readable summary of what was verified
    pub summary: String,
    /// Any warnings (non-fatal)
    pub warnings: Vec<String>,
    /// Rejection reason if accepted=false
    pub rejection_reason: Option<String>,
}

/// Result of transaction field inspection (before hashing)
#[derive(Debug)]
pub struct InspectionResult {
    /// Whether the transaction fields match expectations
    pub accepted: bool,
    /// Human-readable summary
    pub summary: String,
    /// Non-fatal warnings
    pub warnings: Vec<String>,
    /// Rejection reason if accepted=false
    pub rejection_reason: Option<String>,
}
