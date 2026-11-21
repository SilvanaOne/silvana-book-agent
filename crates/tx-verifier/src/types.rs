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
        expected_delivery_amount: String,
        expected_payment_amount: String,
        expected_delivery_instrument_id: String,
        expected_delivery_instrument_admin: String,
        expected_payment_instrument_id: String,
        expected_payment_instrument_admin: String,
    },
    Allocate {
        party: String,
        proposal_id: String,
        dvp_cid: String,
    },
    /// Direct DvpProposal_Cancel (controller proposer) — pure archive of an
    /// expired proposal, must touch nothing else.
    CancelDvpProposal {
        party: String,
        dvp_proposal_cid: String,
    },
    /// Direct DvpProposal_Reject (controller counterparty) — archives the
    /// proposal, creating a RejectedDvpProposal.
    RejectDvpProposal {
        party: String,
        dvp_proposal_cid: String,
    },
    TransferCc {
        sender_party: String,
        receiver_party: String,
        amount: String,
        command_id: String,
    },
    /// Off-chain prepaid traffic top-up — server-resolved receiver
    /// (PARTY_PREPAID_TRAFFIC). Inspected as a stub today since the
    /// cloud-agent doesn't carry the receiver party id locally.
    PrepayTraffic {
        sender_party: String,
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
    ExecuteMulticall {
        party: String,
        op_count: usize,
    },
    LockHoldings {
        party: String,
        amount: String,
        instrument_id: String,
        context: String,
    },
    ProcessLockUnlockRequests {
        party: String,
        request_count: usize,
    },
    ResizeLock {
        party: String,
        new_amount: String,
    },
    TerminateLock {
        party: String,
    },
    /// RFQ V2 (AtomicDVP) single-transaction settle — the user's own funds are
    /// at risk, so `user_input_holding_cids` must match the prepared exercise
    /// exactly (Phase-B inspector). Until Phase B, the H14 envelope pre-check
    /// in the agent is the effective guard.
    AtomicDvpSettle {
        user_party: String,
        venue_cid: String,
        template_id: String, // "#atomic-dvp-v2:AtomicDVP:AtomicDVP"
        quote_id: String,
        ticket_id: String,
        ticket_cid: Option<String>,
        side: String,
        base_amount: String,
        quote_amount: String,
        lp_party: String,
        base_instrument_id: String,
        base_instrument_admin: String,
        quote_instrument_id: String,
        quote_instrument_admin: String,
        valid_until_micros: i64,
        lp_input_holding_cids: Vec<String>,
        user_input_holding_cids: Vec<String>,
    },
    IssueTickets {
        lp_party: String,
        ticket_count: usize,
    },
    SplitHoldings {
        lp_party: String,
        instrument_id: String,
        split_count: usize,
        input_cids: Vec<String>,
    },
    CreateAtomicDvpVenue {
        lp_party: String,
        pair_name: String,
        quote_public_key_spki_hex: String,
    },
    UpdateVenueKey {
        lp_party: String,
        venue_cid: String,
        new_key_spki_hex: String,
    },
    RetireVenue {
        lp_party: String,
        venue_cid: String,
    },
    CancelTickets {
        lp_party: String,
        ticket_count: usize,
    },
    /// USDCx bridge: create the party's BridgeUserAgreementRequest. Every
    /// `Usdcx*` expectation names its network (`mainnet` or `testnet`), whose
    /// pinned parties and synchronizer the inspection checks.
    UsdcxRequestAgreement {
        party: String,
        network: String,
        pre_approval: bool,
        /// `usdcx::onboard_command_id(party, request_id)`.
        command_id: String,
    },
    /// USDCx bridge: mint a DepositAttestation of the party.
    UsdcxMint {
        party: String,
        network: String,
        deposit_attestation_cid: String,
        /// `usdcx::mint_command_id(deposit_attestation_cid)`.
        command_id: String,
    },
    /// USDCx bridge: burn USDCx for a release of USDC on Ethereum.
    UsdcxBurn {
        party: String,
        network: String,
        /// Decimal, compared exactly.
        amount: String,
        /// `0x` + 40 lowercase hex.
        destination_recipient: String,
        request_id: String,
        /// `usdcx::burn_command_id(request_id)`.
        command_id: String,
    },
    /// USDCx bridge: take back the USDCx of a revoked withdrawal.
    UsdcxReclaimHolding {
        party: String,
        network: String,
        revoked_withdraw_attestation_cid: String,
        /// `usdcx::reclaim_command_id(revoked_withdraw_attestation_cid)`.
        command_id: String,
    },
    /// USDCx bridge: switch direct minting of deposits on or off. Its choice
    /// consumes the agreement, so it needs no deterministic command id.
    UsdcxUpdatePreApproval {
        party: String,
        network: String,
        pre_approval: bool,
    },
    /// USDCx bridge: end the party's agreement (consuming, like pre-approval).
    UsdcxTerminateAgreement {
        party: String,
        network: String,
        reason: String,
    },
}

impl OperationExpectation {
    /// A USDCx bridge operation.
    pub fn is_usdcx(&self) -> bool {
        matches!(
            self,
            Self::UsdcxRequestAgreement { .. }
                | Self::UsdcxMint { .. }
                | Self::UsdcxBurn { .. }
                | Self::UsdcxReclaimHolding { .. }
                | Self::UsdcxUpdatePreApproval { .. }
                | Self::UsdcxTerminateAgreement { .. }
        )
    }

    /// Whether only a self-computed hash equal to the server's may be signed (USDCx and AcceptDvp):
    /// a mismatch or an uncomputable hash rejects, and callers must not override a rejection.
    pub fn requires_hash_match(&self) -> bool {
        self.is_usdcx() || matches!(self, Self::AcceptDvp { .. })
    }
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
    /// The holdings a USDCx burn spends, as inspected (empty otherwise): a
    /// caller that guards against signing two burns for one withdrawal
    /// compares them across signatures.
    pub burn_holding_cids: Vec<String>,
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
