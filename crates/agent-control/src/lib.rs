//! Canonical encoding and verification of agent control documents.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    allow(renamed_and_removed_lints),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::unchecked_duration_subtraction,
        clippy::arithmetic_side_effects,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::dbg_macro,
        clippy::disallowed_methods
    ),
    warn(renamed_and_removed_lints)
)]

pub mod builder;
pub mod canon;
pub mod command;
pub mod controller;
pub mod entry;
pub mod error;
pub mod head;
pub mod json;
pub mod link;
pub mod log;
pub mod party;
pub mod policy;
pub mod proof;
pub mod read_session;
pub mod receipt;
pub mod sig;
pub mod signable;
pub mod state;

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod vectors;

pub use builder::{BuildError, EntryBuilder, PendingEntry};
pub use canon::codes::{format_invite_code, format_link_code, parse_link_code, split_invite_code};
pub use command::{Command, CommandOp, VerifiedCommand, command_matches, verify_command};
pub use head::HeadClaim;
pub use read_session::{
    READ_REQUEST_WINDOW_MS, ReadRequest, ReadSessionRequest, SessionId, VerifiedReadSession,
    verify_read_request, verify_read_session,
};
pub use receipt::{CodeSource, Receipt, ReceiptOutcome, ReceiptSubject};
pub use canon::time::UtcTime;
pub use canon::values::{
    Amount, Commitment, EthAddress, Hash32, InstrumentId, InstrumentRef, Label, MarketId, Network,
    Nonce16, Origin, OwnerAccount, RpId, Sig64, Text, Token, Uuid7,
};
pub use controller::{
    BackupPolicy, Capability, CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey,
    ControllerType, WebAuthnKey, controller_id_for, fingerprint,
};
pub use entry::{EntryBody, EntryKind, LogEntry, SignedEntry};
pub use error::{
    CanonError, CanonErrorKind, CodeError, CommandError, LinkError, LogError, LogErrorKind,
    PolicyError, ProofError, ReadSessionError, ReplayError, ReplayErrorKind, SignatureError,
    SignedDocError, ValueError, ValueKind, WebAuthnError,
};
pub use link::{LinkDescriptor, LinkPurpose, RpPolicy, commitment_of, verify_link_descriptor};
pub use log::{
    ForkEvidence, HeadCheck, HeadRef, LogComparison, MAX_LOG_ENTRIES, VerifiedLog, check_head,
    compare_logs, decode_entries, verify_extension, verify_log,
};
pub use party::{PartyId, namespace_for_key, verify_party_key};
pub use policy::{
    Coverage, DEFAULT_LINKED_RULES, Decision, EvalContext, Flow, Intent, MAX_POLICY_BYTES,
    OperationClass, OperationKind, PlatformParties, PolicyDocument, ReasonCode, Recipient, Rules,
    UsageBucket, UsageDelta, UsageRecord, UsageTotals, VerifiedPolicy, Widening, class_is_limited,
    describe_rules, evaluate, expected_effective_at, precheck, precheck_intent,
    required_delay_secs, verify_policy_submission, widening,
};
pub use proof::{ControllerProof, ProofEnvelope, ProofOutcome, ProofType, verify_controller_proof};
pub use sig::ed25519::{parse_ed25519_public_key, verify_ed25519_strict};
pub use sig::p256::{P256PublicKey, parse_p256_spki, verify_p256_p1363};
pub use sig::webauthn::{
    AssertionInfo, WebAuthnAssertion, WebAuthnParams, check_sign_count, verify_assertion,
};
pub use signable::{AgentSignable, SignablePurpose, sign_agent};
pub use state::{
    AcceptedPolicy, CONTROLLER_CHANGE_DELAY_SECS, ControlState, ControllerChange, ControllerRecord,
    EffectiveRules, MAX_AGENT_CONTROLLERS, MinimumRuleOutcome, PendingChange, PendingTarget,
    PolicyDocuments, force_confirmation_phrase, minimum_rule, replay,
};

/// First-line scheme of a control log entry.
pub const SCHEME_CONTROL: &str = "silvana.agent.control.v1";
/// First-line scheme of a link descriptor.
pub const SCHEME_LINK: &str = "silvana.agent.link.v1";
/// First-line scheme of a policy document.
pub const SCHEME_POLICY: &str = "silvana.agent.policy.v1";
/// First-line scheme of a command.
pub const SCHEME_COMMAND: &str = "silvana.agent.command.v1";
/// First-line scheme of a read-session request.
pub const SCHEME_READ_SESSION: &str = "silvana.agent.read-session.v1";
/// First-line scheme of a signed read request.
pub const SCHEME_READ_REQUEST: &str = "silvana.agent.read-request.v1";
/// First-line scheme of a receipt.
pub const SCHEME_RECEIPT: &str = "silvana.agent.receipt.v1";
/// First-line scheme of a head claim.
pub const SCHEME_HEAD: &str = "silvana.agent.control-head.v1";
/// First-line scheme of a controller proof envelope.
pub const SCHEME_PROOF: &str = "silvana.agent.proof.v1";
/// Domain prefix hashed in front of a controller's type and key.
pub const CONTROLLER_ID_DOMAIN: &str = "silvana.agent.controller-id.v1\n";

/// Smallest encoded document, in bytes.
pub const MIN_DOCUMENT_BYTES: usize = 64;
/// Largest link descriptor, in bytes.
pub const MAX_LINK_BYTES: usize = 3584;
/// Largest proof envelope, in bytes.
pub const MAX_PROOF_BYTES: usize = 4096;
/// Largest control log entry, in bytes.
pub const MAX_ENTRY_BYTES: usize = 4096;

/// Most log entries in one upload request.
pub const MAX_APPEND_ENTRIES: usize = 64;
/// Most policy documents in one upload request.
pub const MAX_APPEND_DOCUMENTS: usize = 16;
/// JWT audience prefix of control-service tokens; the network name follows.
pub const CONTROL_TOKEN_AUDIENCE_PREFIX: &str = "silvana.agent.control.";
/// Control-service refusal codes the server repeats for the same upload bytes.
pub const DEFINITE_UPLOAD_REFUSALS: [u16; 6] = [4084, 4085, 4089, 4090, 4091, 4092];
