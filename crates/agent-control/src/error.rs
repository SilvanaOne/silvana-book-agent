//! Error types. Each has a stable snake_case `code_name()` and a numeric `code()`.

use std::fmt;

/// The value grammar a field failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueKind {
    Hex16,
    Hex32,
    Hex64,
    U64,
    Bool,
    Time,
    Amount,
    Party,
    OwnerAccount,
    Label,
    Text,
    RpId,
    Origin,
    Base64Url,
    Uuid7,
    InstrumentId,
    MarketId,
    EthAddress,
    Token,
    Network,
    Purpose,
    ControllerType,
    Capabilities,
    BackupPolicy,
    RequireUv,
    ProofType,
    Spki,
    CredentialId,
    AuthenticatorData,
    ClientDataJson,
    DerSignature,
    EntryKind,
    Unlisted,
    InstrumentAdmin,
    CommandOp,
    ReceiptSubject,
    Outcome,
    CodeSource,
}

impl ValueKind {
    /// Stable name of the refusal: `invalid_<grammar>`.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::Hex16 => "invalid_hex16",
            Self::Hex32 => "invalid_hex32",
            Self::Hex64 => "invalid_hex64",
            Self::U64 => "invalid_u64",
            Self::Bool => "invalid_bool",
            Self::Time => "invalid_time",
            Self::Amount => "invalid_amount",
            Self::Party => "invalid_party",
            Self::OwnerAccount => "invalid_owner_account",
            Self::Label => "invalid_label",
            Self::Text => "invalid_text",
            Self::RpId => "invalid_rp_id",
            Self::Origin => "invalid_origin",
            Self::Base64Url => "invalid_base64url",
            Self::Uuid7 => "invalid_uuid7",
            Self::InstrumentId => "invalid_instrument_id",
            Self::MarketId => "invalid_market_id",
            Self::EthAddress => "invalid_eth_address",
            Self::Token => "invalid_token",
            Self::Network => "invalid_network",
            Self::Purpose => "invalid_purpose",
            Self::ControllerType => "invalid_controller_type",
            Self::Capabilities => "invalid_capabilities",
            Self::BackupPolicy => "invalid_backup_policy",
            Self::RequireUv => "invalid_require_uv",
            Self::ProofType => "invalid_proof_type",
            Self::Spki => "invalid_spki",
            Self::CredentialId => "invalid_credential_id",
            Self::AuthenticatorData => "invalid_authenticator_data",
            Self::ClientDataJson => "invalid_client_data_json",
            Self::DerSignature => "invalid_der_signature",
            Self::EntryKind => "invalid_entry_kind",
            Self::Unlisted => "invalid_unlisted",
            Self::InstrumentAdmin => "invalid_instrument_admin",
            Self::CommandOp => "invalid_command_op",
            Self::ReceiptSubject => "invalid_receipt_subject",
            Self::Outcome => "invalid_outcome",
            Self::CodeSource => "invalid_code_source",
        }
    }

    /// Stable numeric code (150-187).
    pub fn code(self) -> u16 {
        match self {
            Self::Hex16 => 150,
            Self::Hex32 => 151,
            Self::Hex64 => 152,
            Self::U64 => 153,
            Self::Bool => 154,
            Self::Time => 155,
            Self::Amount => 156,
            Self::Party => 157,
            Self::OwnerAccount => 158,
            Self::Label => 159,
            Self::Text => 160,
            Self::RpId => 161,
            Self::Origin => 162,
            Self::Base64Url => 163,
            Self::Uuid7 => 164,
            Self::InstrumentId => 165,
            Self::MarketId => 166,
            Self::EthAddress => 167,
            Self::Token => 168,
            Self::Network => 169,
            Self::Purpose => 170,
            Self::ControllerType => 171,
            Self::Capabilities => 172,
            Self::BackupPolicy => 173,
            Self::RequireUv => 174,
            Self::ProofType => 175,
            Self::Spki => 176,
            Self::CredentialId => 177,
            Self::AuthenticatorData => 178,
            Self::ClientDataJson => 179,
            Self::DerSignature => 180,
            Self::EntryKind => 181,
            Self::Unlisted => 182,
            Self::InstrumentAdmin => 183,
            Self::CommandOp => 184,
            Self::ReceiptSubject => 185,
            Self::Outcome => 186,
            Self::CodeSource => 187,
        }
    }
}

/// A value that does not match its grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValueError(pub ValueKind);

impl ValueError {
    /// The grammar that failed.
    pub fn kind(self) -> ValueKind {
        self.0
    }

    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        self.0.code_name()
    }

    /// Stable numeric code.
    pub fn code(self) -> u16 {
        self.0.code()
    }
}

/// Why a canonical document was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CanonErrorKind {
    TooShort,
    TooLarge,
    InvalidByte,
    MissingFinalNewline,
    EmptyLine,
    MalformedLine,
    WrongScheme,
    UnexpectedKey,
    MissingLine,
    TrailingLine,
    EmptyValue,
    InvalidValue(ValueKind),
    CountOutOfRange,
    UnsortedList,
    DuplicateItem,
    NonCanonical,
    ControllerIdMismatch,
    InvalidPublicKey,
    WeakPublicKey,
    OriginOutsideRpId,
    NotAllowedOnNetwork,
    CapabilityNotCovered,
    PartyRequired,
    PartialDescriptor,
    CcAliasMismatch,
    DelayOutOfRange,
    InvalidValidity,
    InvalidNotBefore,
    InvalidVersion,
    InvalidRange,
    OutcomeMismatch,
    InvalidHead,
}

impl CanonErrorKind {
    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::TooShort => "too_short",
            Self::TooLarge => "too_large",
            Self::InvalidByte => "invalid_byte",
            Self::MissingFinalNewline => "missing_final_newline",
            Self::EmptyLine => "empty_line",
            Self::MalformedLine => "malformed_line",
            Self::WrongScheme => "wrong_scheme",
            Self::UnexpectedKey => "unexpected_key",
            Self::MissingLine => "missing_line",
            Self::TrailingLine => "trailing_line",
            Self::EmptyValue => "empty_value",
            Self::InvalidValue(kind) => kind.code_name(),
            Self::CountOutOfRange => "count_out_of_range",
            Self::UnsortedList => "unsorted_list",
            Self::DuplicateItem => "duplicate_item",
            Self::NonCanonical => "non_canonical",
            Self::ControllerIdMismatch => "controller_id_mismatch",
            Self::InvalidPublicKey => "invalid_public_key",
            Self::WeakPublicKey => "weak_public_key",
            Self::OriginOutsideRpId => "origin_outside_rp_id",
            Self::NotAllowedOnNetwork => "not_allowed_on_network",
            Self::CapabilityNotCovered => "capability_not_covered",
            Self::PartyRequired => "party_required",
            Self::PartialDescriptor => "partial_descriptor",
            Self::CcAliasMismatch => "invalid_cc_alias",
            Self::DelayOutOfRange => "delay_out_of_range",
            Self::InvalidValidity => "invalid_validity",
            Self::InvalidNotBefore => "invalid_not_before",
            Self::InvalidVersion => "invalid_version",
            Self::InvalidRange => "invalid_range",
            Self::OutcomeMismatch => "outcome_mismatch",
            Self::InvalidHead => "invalid_head",
        }
    }

    /// Stable numeric code (100-130, or the value kind code).
    pub fn code(self) -> u16 {
        match self {
            Self::TooShort => 100,
            Self::TooLarge => 101,
            Self::InvalidByte => 102,
            Self::MissingFinalNewline => 103,
            Self::EmptyLine => 104,
            Self::MalformedLine => 105,
            Self::WrongScheme => 106,
            Self::UnexpectedKey => 107,
            Self::MissingLine => 108,
            Self::TrailingLine => 109,
            Self::EmptyValue => 110,
            Self::InvalidValue(kind) => kind.code(),
            Self::CountOutOfRange => 111,
            Self::UnsortedList => 112,
            Self::DuplicateItem => 113,
            Self::NonCanonical => 114,
            Self::ControllerIdMismatch => 115,
            Self::InvalidPublicKey => 116,
            Self::WeakPublicKey => 117,
            Self::OriginOutsideRpId => 118,
            Self::NotAllowedOnNetwork => 119,
            Self::CapabilityNotCovered => 120,
            Self::PartyRequired => 121,
            Self::PartialDescriptor => 122,
            Self::CcAliasMismatch => 123,
            Self::DelayOutOfRange => 124,
            Self::InvalidValidity => 125,
            Self::InvalidNotBefore => 126,
            Self::InvalidVersion => 127,
            Self::InvalidRange => 128,
            Self::OutcomeMismatch => 129,
            Self::InvalidHead => 130,
        }
    }
}

/// A refused canonical document; `line` is 1-based, 0 for the whole document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CanonError {
    pub line: usize,
    pub kind: CanonErrorKind,
}

impl CanonError {
    pub(crate) fn new(line: usize, kind: CanonErrorKind) -> Self {
        Self { line, kind }
    }

    /// Stable name of the refusal.
    pub fn code_name(&self) -> &'static str {
        self.kind.code_name()
    }

    /// Stable numeric code.
    pub fn code(&self) -> u16 {
        self.kind.code()
    }
}

/// Ed25519 or P-256 verification failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignatureError {
    InvalidPublicKey,
    WeakPublicKey,
    InvalidSignatureLength,
    InvalidSignatureEncoding,
    BadSignature,
}

impl SignatureError {
    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::InvalidPublicKey => "invalid_public_key",
            Self::WeakPublicKey => "weak_public_key",
            Self::InvalidSignatureLength => "invalid_signature_length",
            Self::InvalidSignatureEncoding => "invalid_signature_encoding",
            Self::BadSignature => "bad_signature",
        }
    }

    /// Stable numeric code (200-204).
    pub fn code(self) -> u16 {
        match self {
            Self::InvalidPublicKey => 200,
            Self::WeakPublicKey => 201,
            Self::InvalidSignatureLength => 202,
            Self::InvalidSignatureEncoding => 203,
            Self::BadSignature => 204,
        }
    }
}

/// WebAuthn assertion failures, in the order the verifier checks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WebAuthnError {
    InvalidPublicKey,
    ClientDataTooLarge,
    ClientDataInvalid,
    WrongType,
    ChallengeMismatch,
    OriginNotAllowed,
    CrossOrigin,
    AuthDataTooShort,
    AuthDataTooLarge,
    RpIdHashMismatch,
    UserNotPresent,
    UserNotVerified,
    AttestedDataPresent,
    TrailingAuthData,
    InvalidBackupFlags,
    BackupNotAllowed,
    InvalidSignatureEncoding,
    BadSignature,
    CounterRegression,
}

impl WebAuthnError {
    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::InvalidPublicKey => "invalid_public_key",
            Self::ClientDataTooLarge => "client_data_too_large",
            Self::ClientDataInvalid => "client_data_invalid",
            Self::WrongType => "wrong_type",
            Self::ChallengeMismatch => "challenge_mismatch",
            Self::OriginNotAllowed => "origin_not_allowed",
            Self::CrossOrigin => "cross_origin",
            Self::AuthDataTooShort => "auth_data_too_short",
            Self::AuthDataTooLarge => "auth_data_too_large",
            Self::RpIdHashMismatch => "rp_id_hash_mismatch",
            Self::UserNotPresent => "user_not_present",
            Self::UserNotVerified => "user_not_verified",
            Self::AttestedDataPresent => "attested_data_present",
            Self::TrailingAuthData => "trailing_auth_data",
            Self::InvalidBackupFlags => "invalid_backup_flags",
            Self::BackupNotAllowed => "backup_not_allowed",
            Self::InvalidSignatureEncoding => "invalid_signature_encoding",
            Self::BadSignature => "bad_signature",
            Self::CounterRegression => "counter_regression",
        }
    }

    /// Stable numeric code (300-318).
    pub fn code(self) -> u16 {
        match self {
            Self::InvalidPublicKey => 300,
            Self::ClientDataTooLarge => 301,
            Self::ClientDataInvalid => 302,
            Self::WrongType => 303,
            Self::ChallengeMismatch => 304,
            Self::OriginNotAllowed => 305,
            Self::CrossOrigin => 306,
            Self::AuthDataTooShort => 307,
            Self::AuthDataTooLarge => 308,
            Self::RpIdHashMismatch => 309,
            Self::UserNotPresent => 310,
            Self::UserNotVerified => 311,
            Self::AttestedDataPresent => 312,
            Self::TrailingAuthData => 313,
            Self::InvalidBackupFlags => 314,
            Self::BackupNotAllowed => 315,
            Self::InvalidSignatureEncoding => 316,
            Self::BadSignature => 317,
            Self::CounterRegression => 318,
        }
    }
}

/// Invite and link code failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodeError {
    InvalidPrefix,
    InvalidFormat,
    InvalidLength,
    InvalidCharacter,
    NonCanonicalBits,
}

impl CodeError {
    /// Stable name of the refusal.
    pub fn code_name(self) -> &'static str {
        match self {
            Self::InvalidPrefix => "invalid_code_prefix",
            Self::InvalidFormat => "invalid_code_format",
            Self::InvalidLength => "invalid_code_length",
            Self::InvalidCharacter => "invalid_code_character",
            Self::NonCanonicalBits => "invalid_code_trailing_bits",
        }
    }

    /// Stable numeric code (400-404).
    pub fn code(self) -> u16 {
        match self {
            Self::InvalidPrefix => 400,
            Self::InvalidFormat => 401,
            Self::InvalidLength => 402,
            Self::InvalidCharacter => 403,
            Self::NonCanonicalBits => 404,
        }
    }
}

/// Link descriptor verification failures; `Canon` carries a decoding refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkError {
    Canon(CanonError),
    CommitmentMismatch,
    WrongNetwork,
    PartyMismatch,
    InviteNotAllowed,
    Expired,
    RpIdNotAllowed,
    OriginNotAllowed,
}

impl LinkError {
    /// Stable name of the refusal; a decoding refusal keeps its own name.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Canon(e) => e.code_name(),
            Self::CommitmentMismatch => "commitment_mismatch",
            Self::WrongNetwork => "wrong_network",
            Self::PartyMismatch => "party_mismatch",
            Self::InviteNotAllowed => "invite_not_allowed",
            Self::Expired => "expired",
            Self::RpIdNotAllowed => "rp_id_not_allowed",
            Self::OriginNotAllowed => "origin_not_allowed",
        }
    }

    /// Stable numeric code (500-506, or the decoding code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Canon(e) => e.code(),
            Self::CommitmentMismatch => 500,
            Self::WrongNetwork => 501,
            Self::PartyMismatch => 502,
            Self::InviteNotAllowed => 503,
            Self::Expired => 504,
            Self::RpIdNotAllowed => 505,
            Self::OriginNotAllowed => 506,
        }
    }
}

impl From<CanonError> for LinkError {
    fn from(e: CanonError) -> Self {
        Self::Canon(e)
    }
}

/// Controller proof failures; signature failures keep their own names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProofError {
    ControllerMismatch,
    PayloadMismatch,
    ProofTypeMismatch,
    Signature(SignatureError),
    WebAuthn(WebAuthnError),
}

impl ProofError {
    /// Stable name of the refusal.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::ControllerMismatch => "controller_mismatch",
            Self::PayloadMismatch => "payload_mismatch",
            Self::ProofTypeMismatch => "proof_type_mismatch",
            Self::Signature(e) => e.code_name(),
            Self::WebAuthn(e) => e.code_name(),
        }
    }

    /// Stable numeric code (600-602, or the signature code).
    pub fn code(&self) -> u16 {
        match self {
            Self::ControllerMismatch => 600,
            Self::PayloadMismatch => 601,
            Self::ProofTypeMismatch => 602,
            Self::Signature(e) => e.code(),
            Self::WebAuthn(e) => e.code(),
        }
    }
}

impl From<SignatureError> for ProofError {
    fn from(e: SignatureError) -> Self {
        Self::Signature(e)
    }
}

impl From<WebAuthnError> for ProofError {
    fn from(e: WebAuthnError) -> Self {
        Self::WebAuthn(e)
    }
}

/// Why a control log was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LogErrorKind {
    Empty,
    TooManyEntries,
    Canon(CanonError),
    WrongNetwork,
    WrongParty,
    KeyPartyMismatch,
    KeyChanged,
    UnexpectedSeq,
    PrevMismatch,
    TimeRegression,
    Signature(SignatureError),
    NotAFork,
    InvalidHeadClaim,
}

impl LogErrorKind {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Empty => "empty_log",
            Self::TooManyEntries => "too_many_entries",
            Self::Canon(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::WrongParty => "wrong_party",
            Self::KeyPartyMismatch => "key_party_mismatch",
            Self::KeyChanged => "key_changed",
            Self::UnexpectedSeq => "unexpected_seq",
            Self::PrevMismatch => "prev_mismatch",
            Self::TimeRegression => "time_regression",
            Self::Signature(e) => e.code_name(),
            Self::NotAFork => "not_a_fork",
            Self::InvalidHeadClaim => "invalid_head_claim",
        }
    }

    /// Stable numeric code (700-710, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Empty => 700,
            Self::TooManyEntries => 701,
            Self::Canon(e) => e.code(),
            Self::WrongNetwork => 702,
            Self::WrongParty => 703,
            Self::KeyPartyMismatch => 704,
            Self::KeyChanged => 705,
            Self::UnexpectedSeq => 706,
            Self::PrevMismatch => 707,
            Self::TimeRegression => 708,
            Self::Signature(e) => e.code(),
            Self::NotAFork => 709,
            Self::InvalidHeadClaim => 710,
        }
    }
}

/// A refused log; `seq` is the entry position (1-based), 0 for the whole log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LogError {
    pub seq: u64,
    pub kind: LogErrorKind,
}

impl LogError {
    pub(crate) fn new(seq: u64, kind: LogErrorKind) -> Self {
        Self { seq, kind }
    }

    /// Stable name of the refusal.
    pub fn code_name(&self) -> &'static str {
        self.kind.code_name()
    }

    /// Stable numeric code.
    pub fn code(&self) -> u16 {
        self.kind.code()
    }
}

/// Why replaying an entry into the control state failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReplayErrorKind {
    FirstNotLink,
    LinkWhileControlled,
    InviteNotFirst,
    CommitmentMismatch,
    DescriptorExpired,
    EffectiveAtMismatch,
    OwnerMismatch,
    DuplicateController,
    TooManyControllers,
    AgentKeyAsController,
    CommitmentRequired,
    UnknownController,
    NoChange,
    ForcedMismatch,
    SignerLacksPolicy,
    MissingPolicyDocument,
    PolicyDocumentMismatch,
    PolicyDecode(CanonError),
    PolicyWrongNetwork,
    PolicyWrongParty,
    PolicyVersionMismatch,
    PolicyVersionNotIncreasing,
    PolicyPrevMismatch,
    PolicyExpired,
    PolicyIssuedInFuture,
    Proof(ProofError),
    WrongNetwork,
    WrongParty,
    WrongAgentKey,
    UnexpectedSeq,
    PrevMismatch,
    TimeRegression,
    TimeOverflow,
}

impl ReplayErrorKind {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::FirstNotLink => "first_entry_not_link",
            Self::LinkWhileControlled => "link_while_controlled",
            Self::InviteNotFirst => "invite_not_first",
            Self::CommitmentMismatch => "commitment_mismatch",
            Self::DescriptorExpired => "descriptor_expired",
            Self::EffectiveAtMismatch => "effective_at_mismatch",
            Self::OwnerMismatch => "owner_mismatch",
            Self::DuplicateController => "duplicate_controller",
            Self::TooManyControllers => "too_many_controllers",
            Self::AgentKeyAsController => "agent_key_as_controller",
            Self::CommitmentRequired => "commitment_required",
            Self::UnknownController => "unknown_controller",
            Self::NoChange => "no_change",
            Self::ForcedMismatch => "forced_mismatch",
            Self::SignerLacksPolicy => "signer_lacks_policy",
            Self::MissingPolicyDocument => "missing_policy_document",
            Self::PolicyDocumentMismatch => "policy_document_mismatch",
            Self::PolicyDecode(e) => e.code_name(),
            Self::PolicyWrongNetwork => "policy_wrong_network",
            Self::PolicyWrongParty => "policy_wrong_party",
            Self::PolicyVersionMismatch => "policy_version_mismatch",
            Self::PolicyVersionNotIncreasing => "policy_version_not_increasing",
            Self::PolicyPrevMismatch => "policy_prev_mismatch",
            Self::PolicyExpired => "policy_expired",
            Self::PolicyIssuedInFuture => "policy_issued_in_future",
            Self::Proof(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::WrongParty => "wrong_party",
            Self::WrongAgentKey => "wrong_agent_key",
            Self::UnexpectedSeq => "unexpected_seq",
            Self::PrevMismatch => "prev_mismatch",
            Self::TimeRegression => "time_regression",
            Self::TimeOverflow => "time_overflow",
        }
    }

    /// Stable numeric code (800-830, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::FirstNotLink => 800,
            Self::LinkWhileControlled => 801,
            Self::InviteNotFirst => 802,
            Self::CommitmentMismatch => 803,
            Self::DescriptorExpired => 804,
            Self::EffectiveAtMismatch => 805,
            Self::OwnerMismatch => 806,
            Self::DuplicateController => 807,
            Self::TooManyControllers => 808,
            Self::AgentKeyAsController => 809,
            Self::CommitmentRequired => 810,
            Self::UnknownController => 811,
            Self::NoChange => 812,
            Self::ForcedMismatch => 813,
            Self::SignerLacksPolicy => 814,
            Self::MissingPolicyDocument => 815,
            Self::PolicyDocumentMismatch => 816,
            Self::PolicyDecode(e) => e.code(),
            Self::PolicyWrongNetwork => 817,
            Self::PolicyWrongParty => 818,
            Self::PolicyVersionMismatch => 819,
            Self::PolicyVersionNotIncreasing => 820,
            Self::PolicyPrevMismatch => 821,
            Self::PolicyExpired => 822,
            Self::PolicyIssuedInFuture => 823,
            Self::Proof(e) => e.code(),
            Self::WrongNetwork => 824,
            Self::WrongParty => 825,
            Self::WrongAgentKey => 826,
            Self::UnexpectedSeq => 827,
            Self::PrevMismatch => 828,
            Self::TimeRegression => 829,
            Self::TimeOverflow => 830,
        }
    }
}

/// A refused entry during replay; `seq` is the entry's sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReplayError {
    pub seq: u64,
    pub kind: ReplayErrorKind,
}

impl ReplayError {
    pub(crate) fn new(seq: u64, kind: ReplayErrorKind) -> Self {
        Self { seq, kind }
    }

    /// Stable name of the refusal.
    pub fn code_name(&self) -> &'static str {
        self.kind.code_name()
    }

    /// Stable numeric code.
    pub fn code(&self) -> u16 {
        self.kind.code()
    }
}

/// Why a submitted policy document was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyError {
    Canon(CanonError),
    WrongNetwork,
    WrongParty,
    UnknownController,
    SignerLacksPolicy,
    VersionNotIncreasing,
    PrevMismatch,
    IssuedInFuture,
    Expired,
    Proof(ProofError),
    TimeOverflow,
}

impl PolicyError {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Canon(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::WrongParty => "wrong_party",
            Self::UnknownController => "unknown_controller",
            Self::SignerLacksPolicy => "signer_lacks_policy",
            Self::VersionNotIncreasing => "version_not_increasing",
            Self::PrevMismatch => "prev_mismatch",
            Self::IssuedInFuture => "issued_in_future",
            Self::Expired => "expired",
            Self::Proof(e) => e.code_name(),
            Self::TimeOverflow => "time_overflow",
        }
    }

    /// Stable numeric code (900-908, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Canon(e) => e.code(),
            Self::WrongNetwork => 900,
            Self::WrongParty => 901,
            Self::UnknownController => 902,
            Self::SignerLacksPolicy => 903,
            Self::VersionNotIncreasing => 904,
            Self::PrevMismatch => 905,
            Self::IssuedInFuture => 906,
            Self::Expired => 907,
            Self::Proof(e) => e.code(),
            Self::TimeOverflow => 908,
        }
    }
}

/// Why a controller command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CommandError {
    Canon(CanonError),
    WrongNetwork,
    WrongParty,
    UnknownController,
    SignerLacksDirect,
    StaleLogHead,
    IssuedInFuture,
    Expired,
    Proof(ProofError),
    TimeOverflow,
}

impl CommandError {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Canon(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::WrongParty => "wrong_party",
            Self::UnknownController => "unknown_controller",
            Self::SignerLacksDirect => "signer_lacks_direct",
            Self::StaleLogHead => "stale_log_head",
            Self::IssuedInFuture => "issued_in_future",
            Self::Expired => "expired",
            Self::Proof(e) => e.code_name(),
            Self::TimeOverflow => "time_overflow",
        }
    }

    /// Stable numeric code (1100-1107, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Canon(e) => e.code(),
            Self::WrongNetwork => 1100,
            Self::WrongParty => 1101,
            Self::UnknownController => 1102,
            Self::SignerLacksDirect => 1103,
            Self::StaleLogHead => 1104,
            Self::IssuedInFuture => 1105,
            Self::Expired => 1106,
            Self::Proof(e) => e.code(),
            Self::TimeOverflow => 1107,
        }
    }
}

/// Why a read session or a signed read request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReadSessionError {
    Canon(CanonError),
    WrongNetwork,
    UnknownAgent,
    UnknownController,
    SignerLacksRead,
    IssuedInFuture,
    Expired,
    TimeOverflow,
    SessionMismatch,
    AgentNotInSession,
    OutsideWindow,
    Proof(ProofError),
    Signature(SignatureError),
}

impl ReadSessionError {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Canon(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::UnknownAgent => "unknown_agent",
            Self::UnknownController => "unknown_controller",
            Self::SignerLacksRead => "signer_lacks_read",
            Self::IssuedInFuture => "issued_in_future",
            Self::Expired => "expired",
            Self::TimeOverflow => "time_overflow",
            Self::SessionMismatch => "session_mismatch",
            Self::AgentNotInSession => "agent_not_in_session",
            Self::OutsideWindow => "outside_window",
            Self::Proof(e) => e.code_name(),
            Self::Signature(e) => e.code_name(),
        }
    }

    /// Stable numeric code (1200-1209, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Canon(e) => e.code(),
            Self::WrongNetwork => 1200,
            Self::UnknownAgent => 1201,
            Self::UnknownController => 1202,
            Self::SignerLacksRead => 1203,
            Self::IssuedInFuture => 1204,
            Self::Expired => 1205,
            Self::TimeOverflow => 1206,
            Self::SessionMismatch => 1207,
            Self::AgentNotInSession => 1208,
            Self::OutsideWindow => 1209,
            Self::Proof(e) => e.code(),
            Self::Signature(e) => e.code(),
        }
    }
}

/// Why an agent-signed receipt or head claim was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignedDocError {
    Canon(CanonError),
    WrongNetwork,
    WrongParty,
    KeyPartyMismatch,
    Signature(SignatureError),
}

impl SignedDocError {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Canon(e) => e.code_name(),
            Self::WrongNetwork => "wrong_network",
            Self::WrongParty => "wrong_party",
            Self::KeyPartyMismatch => "key_party_mismatch",
            Self::Signature(e) => e.code_name(),
        }
    }

    /// Stable numeric code (1300-1302, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Canon(e) => e.code(),
            Self::WrongNetwork => 1300,
            Self::WrongParty => 1301,
            Self::KeyPartyMismatch => 1302,
            Self::Signature(e) => e.code(),
        }
    }
}

impl fmt::Display for CanonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at line {}", self.code_name(), self.line)
    }
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at entry {}", self.code_name(), self.seq)
    }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at entry {}", self.code_name(), self.seq)
    }
}

macro_rules! display_by_name {
    ($($t:ty),*) => {
        $(
            impl fmt::Display for $t {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(self.code_name())
                }
            }
        )*
    };
}

display_by_name!(
    ValueError,
    SignatureError,
    WebAuthnError,
    CodeError,
    LinkError,
    ProofError,
    PolicyError,
    CommandError,
    ReadSessionError,
    SignedDocError
);

impl std::error::Error for CommandError {}
impl std::error::Error for ReadSessionError {}
impl std::error::Error for SignedDocError {}
impl std::error::Error for LogError {}
impl std::error::Error for ReplayError {}
impl std::error::Error for PolicyError {}
impl std::error::Error for ValueError {}
impl std::error::Error for CanonError {}
impl std::error::Error for SignatureError {}
impl std::error::Error for WebAuthnError {}
impl std::error::Error for CodeError {}
impl std::error::Error for LinkError {}
impl std::error::Error for ProofError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_and_codes_are_unique_within_each_type() {
        let canon = [
            CanonErrorKind::TooShort,
            CanonErrorKind::TooLarge,
            CanonErrorKind::InvalidByte,
            CanonErrorKind::MissingFinalNewline,
            CanonErrorKind::EmptyLine,
            CanonErrorKind::MalformedLine,
            CanonErrorKind::WrongScheme,
            CanonErrorKind::UnexpectedKey,
            CanonErrorKind::MissingLine,
            CanonErrorKind::TrailingLine,
            CanonErrorKind::EmptyValue,
            CanonErrorKind::CountOutOfRange,
            CanonErrorKind::UnsortedList,
            CanonErrorKind::DuplicateItem,
            CanonErrorKind::NonCanonical,
            CanonErrorKind::ControllerIdMismatch,
            CanonErrorKind::InvalidPublicKey,
            CanonErrorKind::WeakPublicKey,
            CanonErrorKind::OriginOutsideRpId,
            CanonErrorKind::NotAllowedOnNetwork,
            CanonErrorKind::CapabilityNotCovered,
            CanonErrorKind::PartyRequired,
            CanonErrorKind::PartialDescriptor,
            CanonErrorKind::CcAliasMismatch,
            CanonErrorKind::DelayOutOfRange,
            CanonErrorKind::InvalidValidity,
            CanonErrorKind::InvalidNotBefore,
            CanonErrorKind::InvalidVersion,
            CanonErrorKind::InvalidRange,
            CanonErrorKind::OutcomeMismatch,
            CanonErrorKind::InvalidHead,
        ];
        let values = [
            ValueKind::Hex16,
            ValueKind::Hex32,
            ValueKind::Hex64,
            ValueKind::U64,
            ValueKind::Bool,
            ValueKind::Time,
            ValueKind::Amount,
            ValueKind::Party,
            ValueKind::OwnerAccount,
            ValueKind::Label,
            ValueKind::Text,
            ValueKind::RpId,
            ValueKind::Origin,
            ValueKind::Base64Url,
            ValueKind::Uuid7,
            ValueKind::InstrumentId,
            ValueKind::MarketId,
            ValueKind::EthAddress,
            ValueKind::Token,
            ValueKind::Network,
            ValueKind::Purpose,
            ValueKind::ControllerType,
            ValueKind::Capabilities,
            ValueKind::BackupPolicy,
            ValueKind::RequireUv,
            ValueKind::ProofType,
            ValueKind::Spki,
            ValueKind::CredentialId,
            ValueKind::AuthenticatorData,
            ValueKind::ClientDataJson,
            ValueKind::DerSignature,
            ValueKind::EntryKind,
            ValueKind::Unlisted,
            ValueKind::InstrumentAdmin,
            ValueKind::CommandOp,
            ValueKind::ReceiptSubject,
            ValueKind::Outcome,
            ValueKind::CodeSource,
        ];
        let all: Vec<CanonErrorKind> = canon
            .into_iter()
            .chain(values.into_iter().map(CanonErrorKind::InvalidValue))
            .collect();
        let names: HashSet<&str> = all.iter().map(|k| k.code_name()).collect();
        let codes: HashSet<u16> = all.iter().map(|k| k.code()).collect();
        assert_eq!(names.len(), all.len());
        assert_eq!(codes.len(), all.len());

        let webauthn = [
            WebAuthnError::InvalidPublicKey,
            WebAuthnError::ClientDataTooLarge,
            WebAuthnError::ClientDataInvalid,
            WebAuthnError::WrongType,
            WebAuthnError::ChallengeMismatch,
            WebAuthnError::OriginNotAllowed,
            WebAuthnError::CrossOrigin,
            WebAuthnError::AuthDataTooShort,
            WebAuthnError::AuthDataTooLarge,
            WebAuthnError::RpIdHashMismatch,
            WebAuthnError::UserNotPresent,
            WebAuthnError::UserNotVerified,
            WebAuthnError::AttestedDataPresent,
            WebAuthnError::TrailingAuthData,
            WebAuthnError::InvalidBackupFlags,
            WebAuthnError::BackupNotAllowed,
            WebAuthnError::InvalidSignatureEncoding,
            WebAuthnError::BadSignature,
            WebAuthnError::CounterRegression,
        ];
        let names: HashSet<&str> = webauthn.iter().map(|e| e.code_name()).collect();
        let codes: HashSet<u16> = webauthn.iter().map(|e| e.code()).collect();
        assert_eq!(names.len(), webauthn.len());
        assert_eq!(codes.len(), webauthn.len());
    }

    #[test]
    fn wave_two_names_and_codes_are_unique_within_each_type() {
        use LogErrorKind as L;
        use ReplayErrorKind as R;
        let log = [
            L::Empty,
            L::TooManyEntries,
            L::WrongNetwork,
            L::WrongParty,
            L::KeyPartyMismatch,
            L::KeyChanged,
            L::UnexpectedSeq,
            L::PrevMismatch,
            L::TimeRegression,
            L::NotAFork,
            L::InvalidHeadClaim,
        ];
        let replay = [
            R::FirstNotLink,
            R::LinkWhileControlled,
            R::InviteNotFirst,
            R::CommitmentMismatch,
            R::DescriptorExpired,
            R::EffectiveAtMismatch,
            R::OwnerMismatch,
            R::DuplicateController,
            R::TooManyControllers,
            R::AgentKeyAsController,
            R::CommitmentRequired,
            R::UnknownController,
            R::NoChange,
            R::ForcedMismatch,
            R::SignerLacksPolicy,
            R::MissingPolicyDocument,
            R::PolicyDocumentMismatch,
            R::PolicyWrongNetwork,
            R::PolicyWrongParty,
            R::PolicyVersionMismatch,
            R::PolicyVersionNotIncreasing,
            R::PolicyPrevMismatch,
            R::PolicyExpired,
            R::PolicyIssuedInFuture,
            R::WrongNetwork,
            R::WrongParty,
            R::WrongAgentKey,
            R::UnexpectedSeq,
            R::PrevMismatch,
            R::TimeRegression,
            R::TimeOverflow,
        ];
        let policy = [
            PolicyError::WrongNetwork,
            PolicyError::WrongParty,
            PolicyError::UnknownController,
            PolicyError::SignerLacksPolicy,
            PolicyError::VersionNotIncreasing,
            PolicyError::PrevMismatch,
            PolicyError::IssuedInFuture,
            PolicyError::Expired,
            PolicyError::TimeOverflow,
        ];
        fn unique(pairs: &[(&'static str, u16)]) -> bool {
            let names: HashSet<&str> = pairs.iter().map(|p| p.0).collect();
            let codes: HashSet<u16> = pairs.iter().map(|p| p.1).collect();
            names.len() == pairs.len() && codes.len() == pairs.len()
        }
        let pairs: Vec<_> = log.iter().map(|k| (k.code_name(), k.code())).collect();
        assert!(unique(&pairs));
        let pairs: Vec<_> = replay.iter().map(|k| (k.code_name(), k.code())).collect();
        assert!(unique(&pairs));
        let pairs: Vec<_> = policy.iter().map(|k| (k.code_name(), k.code())).collect();
        assert!(unique(&pairs));
        let canon = CanonError::new(5, CanonErrorKind::InvalidValidity);
        assert_eq!(R::PolicyDecode(canon).code_name(), "invalid_validity");
        assert_eq!(LogError::new(3, L::Canon(canon)).code(), 125);
        assert_eq!(ReplayError::new(4, R::NoChange).to_string(), "no_change at entry 4");
    }

    #[test]
    fn wave_three_names_and_codes_are_unique_within_each_type() {
        fn unique(pairs: &[(&'static str, u16)]) -> bool {
            let names: HashSet<&str> = pairs.iter().map(|p| p.0).collect();
            let codes: HashSet<u16> = pairs.iter().map(|p| p.1).collect();
            names.len() == pairs.len() && codes.len() == pairs.len()
        }
        let command = [
            CommandError::WrongNetwork,
            CommandError::WrongParty,
            CommandError::UnknownController,
            CommandError::SignerLacksDirect,
            CommandError::StaleLogHead,
            CommandError::IssuedInFuture,
            CommandError::Expired,
            CommandError::TimeOverflow,
        ];
        let pairs: Vec<_> = command.iter().map(|e| (e.code_name(), e.code())).collect();
        assert!(unique(&pairs));
        assert!(pairs.iter().all(|p| (1100..=1107).contains(&p.1)));
        let session = [
            ReadSessionError::WrongNetwork,
            ReadSessionError::UnknownAgent,
            ReadSessionError::UnknownController,
            ReadSessionError::SignerLacksRead,
            ReadSessionError::IssuedInFuture,
            ReadSessionError::Expired,
            ReadSessionError::TimeOverflow,
            ReadSessionError::SessionMismatch,
            ReadSessionError::AgentNotInSession,
            ReadSessionError::OutsideWindow,
        ];
        let pairs: Vec<_> = session.iter().map(|e| (e.code_name(), e.code())).collect();
        assert!(unique(&pairs));
        assert!(pairs.iter().all(|p| (1200..=1209).contains(&p.1)));
        let signed = [
            SignedDocError::WrongNetwork,
            SignedDocError::WrongParty,
            SignedDocError::KeyPartyMismatch,
        ];
        let pairs: Vec<_> = signed.iter().map(|e| (e.code_name(), e.code())).collect();
        assert!(unique(&pairs));
        let canon = CanonError::new(2, CanonErrorKind::OutcomeMismatch);
        assert_eq!(SignedDocError::Canon(canon).code_name(), "outcome_mismatch");
        assert_eq!(CommandError::Canon(canon).code(), 129);
        let wrapped = ReadSessionError::Signature(SignatureError::BadSignature);
        assert_eq!(wrapped.code_name(), "bad_signature");
        assert_eq!(CommandError::StaleLogHead.to_string(), "stale_log_head");
    }

    #[test]
    fn wrapped_errors_keep_the_inner_name() {
        let canon = CanonError::new(3, CanonErrorKind::InvalidValue(ValueKind::Time));
        assert_eq!(LinkError::from(canon).code_name(), "invalid_time");
        assert_eq!(LinkError::from(canon).code(), 155);
        assert_eq!(canon.to_string(), "invalid_time at line 3");
        let proof = ProofError::from(WebAuthnError::UserNotVerified);
        assert_eq!(proof.code_name(), "user_not_verified");
        assert_eq!(ProofError::from(SignatureError::BadSignature).code(), 204);
    }
}
