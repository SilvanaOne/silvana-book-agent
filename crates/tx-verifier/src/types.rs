//! Types for transaction verification
//!
//! `OperationExpectation` describes what the agent expects a transaction to do.
//! `VerificationResult` is the output of `verify_and_hash()`.

/// An instrument and an exact decimal amount.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedQuantity {
    pub instrument_admin: String,
    pub instrument_id: String,
    /// Decimal text, compared exactly.
    pub amount: String,
}

/// The DvP terms the agent recorded for a proposal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedDvpTerms {
    pub proposal_id: String,
    pub buyer: String,
    pub seller: String,
    pub delivery: ExpectedQuantity,
    pub payment: ExpectedQuantity,
}

/// A fee the request makes the agent pay.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedFee {
    pub receiver: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    pub amount: String,
}

/// One receiver of a CC batch payment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BatchPayTarget {
    pub receiver: String,
    pub amount: String,
}

/// What the agent expects the prepared transaction to do.
/// Built from the `PrepareTransactionRequest` params + local config.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OperationExpectation {
    PayFee {
        sender_party: String,
        fee_party: String,
        proposal_id: String,
        fee_type: String, // "dvp" or "allocate"
    },
    ProposeDvp {
        #[serde(alias = "buyer_party")]
        proposer: String,
        #[serde(alias = "seller_party")]
        counterparty: String,
        proposal_id: String,
        synchronizer_id: String,
        #[serde(default)]
        terms: Option<ExpectedDvpTerms>,
    },
    AcceptDvp {
        #[serde(alias = "seller_party")]
        acceptor: String,
        proposal_id: String,
        dvp_proposal_cid: String,
        expected_delivery_amount: String,
        expected_payment_amount: String,
        expected_delivery_instrument_id: String,
        expected_delivery_instrument_admin: String,
        expected_payment_instrument_id: String,
        expected_payment_instrument_admin: String,
        #[serde(default)]
        terms: Option<ExpectedDvpTerms>,
    },
    Allocate {
        party: String,
        proposal_id: String,
        dvp_cid: String,
        #[serde(default)]
        terms: Option<ExpectedDvpTerms>,
        #[serde(default)]
        settlement_operator: Option<String>,
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
        #[serde(default)]
        dso_party: Option<String>,
    },
    /// Off-chain prepaid traffic top-up — server-resolved receiver
    /// (PARTY_PREPAID_TRAFFIC). Inspected as a stub today since the
    /// cloud-agent doesn't carry the receiver party id locally.
    PrepayTraffic {
        sender_party: String,
        amount: String,
        command_id: String,
        #[serde(default)]
        receiver_party: Option<String>,
        #[serde(default)]
        dso_party: Option<String>,
    },
    RequestPreapproval {
        party: String,
        #[serde(default)]
        operator: Option<String>,
        #[serde(default)]
        instrument_admin: Option<String>,
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
        #[serde(default)]
        command_id: Option<String>,
        #[serde(default)]
        input_holding_cids: Vec<String>,
    },
    AcceptCip56 {
        receiver_party: String,
        contract_id: String,
    },
    SplitCc {
        party: String,
        output_amounts: Vec<String>,
        #[serde(default)]
        amulet_cids: Vec<String>,
        #[serde(default)]
        dso_party: Option<String>,
    },
    ExecuteMulticall {
        party: String,
        op_count: usize,
        /// The DvP terms the agent recorded for the proposal its operations accept and allocate.
        #[serde(default)]
        terms: Option<ExpectedDvpTerms>,
    },
    /// CC payments to several receivers in one multicall.
    BatchPayCc {
        party: String,
        targets: Vec<BatchPayTarget>,
        holding_cids: Vec<String>,
        #[serde(default)]
        dso_party: Option<String>,
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
        #[serde(default)]
        expected_fees: Vec<ExpectedFee>,
        #[serde(default)]
        fee_party: Option<String>,
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

    /// How this operation's newer checks act for an agent that is not linked, under
    /// `[verify] mode = "default"`: one entry per operation, moved to `Enforce` one by one.
    pub fn default_check_mode(&self) -> CheckMode {
        match self {
            Self::AcceptDvp { .. }
            | Self::UsdcxRequestAgreement { .. }
            | Self::UsdcxMint { .. }
            | Self::UsdcxBurn { .. }
            | Self::UsdcxReclaimHolding { .. }
            | Self::UsdcxUpdatePreApproval { .. }
            | Self::UsdcxTerminateAgreement { .. } => CheckMode::Enforce,
            Self::PayFee { .. }
            | Self::ProposeDvp { .. }
            | Self::Allocate { .. }
            | Self::CancelDvpProposal { .. }
            | Self::RejectDvpProposal { .. }
            | Self::TransferCc { .. }
            | Self::PrepayTraffic { .. }
            | Self::RequestPreapproval { .. }
            | Self::RequestRecurringPrepaid { .. }
            | Self::RequestRecurringPayasyougo { .. }
            | Self::RequestUserService { .. }
            | Self::TransferCip56 { .. }
            | Self::AcceptCip56 { .. }
            | Self::SplitCc { .. }
            | Self::ExecuteMulticall { .. }
            | Self::BatchPayCc { .. }
            | Self::LockHoldings { .. }
            | Self::ProcessLockUnlockRequests { .. }
            | Self::ResizeLock { .. }
            | Self::TerminateLock { .. }
            | Self::AtomicDvpSettle { .. }
            | Self::IssueTickets { .. }
            | Self::SplitHoldings { .. }
            | Self::CreateAtomicDvpVenue { .. }
            | Self::UpdateVenueKey { .. }
            | Self::RetireVenue { .. }
            | Self::CancelTickets { .. } => CheckMode::Observe,
        }
    }

    /// Whether the inspector checks this operation's fields.
    pub fn coverage(&self) -> Coverage {
        match self {
            Self::PayFee { .. }
            | Self::ProposeDvp { .. }
            | Self::AcceptDvp { .. }
            | Self::Allocate { .. }
            | Self::CancelDvpProposal { .. }
            | Self::RejectDvpProposal { .. }
            | Self::TransferCc { .. }
            | Self::RequestPreapproval { .. }
            | Self::RequestRecurringPrepaid { .. }
            | Self::RequestRecurringPayasyougo { .. }
            | Self::RequestUserService { .. }
            | Self::AcceptCip56 { .. }
            | Self::AtomicDvpSettle { .. }
            | Self::UsdcxRequestAgreement { .. }
            | Self::UsdcxMint { .. }
            | Self::UsdcxBurn { .. }
            | Self::UsdcxReclaimHolding { .. }
            | Self::UsdcxUpdatePreApproval { .. }
            | Self::UsdcxTerminateAgreement { .. }
            | Self::PrepayTraffic { .. }
            | Self::TransferCip56 { .. }
            | Self::SplitCc { .. }
            | Self::BatchPayCc { .. } => Coverage::Fields,
            Self::ExecuteMulticall { .. }
            | Self::LockHoldings { .. }
            | Self::ProcessLockUnlockRequests { .. }
            | Self::ResizeLock { .. }
            | Self::TerminateLock { .. }
            | Self::IssueTickets { .. }
            | Self::SplitHoldings { .. }
            | Self::CreateAtomicDvpVenue { .. }
            | Self::UpdateVenueKey { .. }
            | Self::RetireVenue { .. }
            | Self::CancelTickets { .. } => Coverage::Unverified,
        }
    }
}

/// How the self-computed hash compared with the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashCheck {
    /// The server hash is the computed hash.
    Match,
    /// The server hash differs or is not 32 bytes; `server_hex` is empty when it is not base64.
    Mismatch {
        server_hex: String,
        computed_hex: String,
    },
    /// No hash was computed.
    Uncomputed { reason: String },
}

/// How the checks that are newer than an operation's signing behaviour act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// Logged and counted as "would refuse"; the transaction is signed as before.
    Observe,
    /// Refused.
    Enforce,
}

impl CheckMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// `[verify] mode` of an agent that is not linked; a linked agent always enforces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerifySetting {
    /// Each operation's entry in [`OperationExpectation::default_check_mode`].
    #[default]
    Default,
    /// Every operation observes, except those that always refuse a hash mismatch.
    Observe,
    /// Every operation enforces.
    Enforce,
}

impl VerifySetting {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// A check that refuses under [`CheckMode::Enforce`] and was only observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WouldRefuse {
    /// The hash did not match, or none was computed, so the server hash was signed.
    Hash(String),
    /// A field check of the inspector.
    Inspector(String),
}

/// Whether the inspector checked the operation's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// The fields were checked against the expectation.
    Fields,
    /// The fields were not checked.
    Unverified,
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
    /// The hash comparison; a refusal before hashing is `Uncomputed`.
    pub hash_check: HashCheck,
    /// Whether the operation's fields were checked.
    pub coverage: Coverage,
    /// What [`CheckMode::Enforce`] would have refused; only an observed operation has any.
    pub would_refuse: Vec<WouldRefuse>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn load(json: &str) -> OperationExpectation {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn expectations_written_before_the_renames_still_load() {
        let propose = load(r#"{"ProposeDvp":{"buyer_party":"a","seller_party":"b","proposal_id":"p","synchronizer_id":"s"}}"#);
        assert!(
            matches!(&propose, OperationExpectation::ProposeDvp { proposer, counterparty, terms: None, .. } if proposer == "a" && counterparty == "b"),
            "{propose:?}"
        );
        let accept = load(
            r#"{"AcceptDvp":{"seller_party":"a","proposal_id":"p","dvp_proposal_cid":"c","expected_delivery_amount":"1",
            "expected_payment_amount":"2","expected_delivery_instrument_id":"D","expected_delivery_instrument_admin":"d",
            "expected_payment_instrument_id":"Q","expected_payment_instrument_admin":"q"}}"#,
        );
        assert!(matches!(&accept, OperationExpectation::AcceptDvp { acceptor, terms: None, .. } if acceptor == "a"), "{accept:?}");
        let split = load(r#"{"SplitCc":{"party":"a","output_amounts":["1"]}}"#);
        assert!(
            matches!(&split, OperationExpectation::SplitCc { amulet_cids, dso_party: None, .. } if amulet_cids.is_empty()),
            "{split:?}"
        );
        let allocate = load(r#"{"Allocate":{"party":"a","proposal_id":"p","dvp_cid":"c"}}"#);
        assert!(
            matches!(&allocate, OperationExpectation::Allocate { terms: None, settlement_operator: None, .. }),
            "{allocate:?}"
        );
    }

    #[test]
    fn the_new_fields_survive_a_round_trip() {
        let quantity = |id: &str| ExpectedQuantity { instrument_admin: "admin".into(), instrument_id: id.into(), amount: "1.5".into() };
        let terms = ExpectedDvpTerms {
            proposal_id: "p".into(),
            buyer: "b".into(),
            seller: "s".into(),
            delivery: quantity("D"),
            payment: quantity("Q"),
        };
        for e in [
            OperationExpectation::Allocate {
                party: "b".into(),
                proposal_id: "p".into(),
                dvp_cid: "c".into(),
                terms: Some(terms.clone()),
                settlement_operator: Some("op".into()),
            },
            OperationExpectation::BatchPayCc {
                party: "a".into(),
                targets: vec![BatchPayTarget { receiver: "r".into(), amount: "2".into() }],
                holding_cids: vec!["h".into()],
                dso_party: Some("dso".into()),
            },
        ] {
            let back = load(&serde_json::to_string(&e).unwrap());
            assert_eq!(format!("{back:?}"), format!("{e:?}"));
        }
    }
}
