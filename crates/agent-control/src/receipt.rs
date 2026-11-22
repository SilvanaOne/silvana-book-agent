//! Agent-signed receipts (`silvana.agent.receipt.v1`) for commands and policy refusals.

use crate::SCHEME_RECEIPT;
use crate::canon::time::UtcTime;
use crate::canon::values::{Hash32, Network, Token, Uuid7, parse_hex_array, parse_u64};
use crate::canon::{Reader, Writer, decode_exact};
use crate::error::{CanonError, CanonErrorKind, SignedDocError, ValueError, ValueKind};
use crate::party::{PartyId, verify_party_key};
use crate::sig::ed25519::verify_ed25519_strict;
use crate::signable::AgentSignable;

/// Largest receipt, in bytes.
pub const MAX_RECEIPT_BYTES: usize = 2048;

/// What a receipt is about: a command by id, or a policy document by hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReceiptSubject {
    Command(Uuid7),
    Policy(Hash32),
}

impl ReceiptSubject {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Command(_) => "command",
            Self::Policy(_) => "policy",
        }
    }

    pub fn id_string(&self) -> String {
        match self {
            Self::Command(id) => id.to_string(),
            Self::Policy(hash) => hash.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReceiptOutcome {
    Executed,
    Refused,
    Failed,
    Expired,
}

impl ReceiptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "executed" => Ok(Self::Executed),
            "refused" => Ok(Self::Refused),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            _ => Err(ValueError(ValueKind::Outcome)),
        }
    }

    /// Refused and failed receipts carry a reason code; the others do not.
    pub fn has_reason(self) -> bool {
        matches!(self, Self::Refused | Self::Failed)
    }
}

/// Whether a reason code is the agent's own decision or relayed from the ledger-service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeSource {
    Agent,
    Ledger,
}

impl CodeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Ledger => "ledger",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "agent" => Ok(Self::Agent),
            "ledger" => Ok(Self::Ledger),
            _ => Err(ValueError(ValueKind::CodeSource)),
        }
    }
}

/// The agent's signed account of what happened to a command or a policy.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Receipt {
    pub network: Network,
    pub agent_party: PartyId,
    pub agent_key: [u8; 32],
    pub subject: ReceiptSubject,
    /// SHA-256 of the command bytes or of the policy document.
    pub subject_hash: Hash32,
    pub outcome: ReceiptOutcome,
    /// Set exactly when executed.
    pub update_id: Option<Token>,
    /// Set exactly when refused or failed, together with its source.
    pub reason_code: Option<u64>,
    pub code_source: Option<CodeSource>,
    pub log_seq: u64,
    pub issued_at: UtcTime,
}

fn parse_subject_kind(s: &str) -> Result<&'static str, ValueError> {
    ["command", "policy"]
        .into_iter()
        .find(|kind| *kind == s)
        .ok_or(ValueError(ValueKind::ReceiptSubject))
}

fn outcome_fits_subject(subject: &ReceiptSubject, outcome: ReceiptOutcome) -> bool {
    !(matches!(subject, ReceiptSubject::Policy(_)) && outcome == ReceiptOutcome::Executed)
}

impl Receipt {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_RECEIPT, MAX_RECEIPT_BYTES);
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("agent_key", &hex::encode(self.agent_key));
        w.line("subject", self.subject.kind_str());
        w.line("subject_id", &self.subject.id_string());
        w.line("subject_hash", &self.subject_hash.to_string());
        w.line("outcome", self.outcome.as_str());
        if !outcome_fits_subject(&self.subject, self.outcome) {
            w.fail(CanonErrorKind::OutcomeMismatch);
        }
        w.opt("update_id", self.update_id.as_ref().map(Token::as_str));
        if self.update_id.is_some() != (self.outcome == ReceiptOutcome::Executed) {
            w.fail(CanonErrorKind::OutcomeMismatch);
        }
        w.opt(
            "reason_code",
            self.reason_code.map(|code| code.to_string()).as_deref(),
        );
        if self.reason_code.is_some() != self.outcome.has_reason() {
            w.fail(CanonErrorKind::OutcomeMismatch);
        }
        w.opt("code_source", self.code_source.map(CodeSource::as_str));
        if self.code_source.is_some() != self.reason_code.is_some() {
            w.fail(CanonErrorKind::OutcomeMismatch);
        }
        w.line("log_seq", &self.log_seq.to_string());
        w.line("issued_at", &self.issued_at.to_string());
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_RECEIPT,
            MAX_RECEIPT_BYTES,
            Self::read,
            Self::encode,
        )
    }

    /// The bytes the agent signs.
    pub fn signable(&self) -> Result<AgentSignable, CanonError> {
        self.encode().map(AgentSignable::receipt)
    }

    /// Decodes and checks the party, the key binding and the agent's signature.
    pub fn verify(
        bytes: &[u8],
        signature: &[u8; 64],
        expected_party: &PartyId,
    ) -> Result<Self, SignedDocError> {
        let receipt = Self::decode(bytes).map_err(SignedDocError::Canon)?;
        if receipt.agent_party != *expected_party {
            return Err(SignedDocError::WrongParty);
        }
        if !verify_party_key(&receipt.agent_party, &receipt.agent_key) {
            return Err(SignedDocError::KeyPartyMismatch);
        }
        verify_ed25519_strict(&receipt.agent_key, bytes, signature)
            .map_err(SignedDocError::Signature)?;
        Ok(receipt)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let agent_key = r.value("agent_key", |s| {
            parse_hex_array::<32>(s).ok_or(ValueError(ValueKind::Hex32))
        })?;
        let subject = match r.value("subject", parse_subject_kind)? {
            "command" => ReceiptSubject::Command(r.value("subject_id", Uuid7::parse)?),
            _ => ReceiptSubject::Policy(r.value("subject_id", Hash32::parse)?),
        };
        let subject_hash = r.value("subject_hash", Hash32::parse)?;
        let outcome = r.value("outcome", ReceiptOutcome::parse)?;
        if !outcome_fits_subject(&subject, outcome) {
            return Err(r.err(CanonErrorKind::OutcomeMismatch));
        }
        let update_id = r.opt_value("update_id", Token::parse)?;
        if update_id.is_some() != (outcome == ReceiptOutcome::Executed) {
            return Err(r.err(CanonErrorKind::OutcomeMismatch));
        }
        let reason_code = r.opt_value("reason_code", parse_u64)?;
        if reason_code.is_some() != outcome.has_reason() {
            return Err(r.err(CanonErrorKind::OutcomeMismatch));
        }
        let code_source = r.opt_value("code_source", CodeSource::parse)?;
        if code_source.is_some() != reason_code.is_some() {
            return Err(r.err(CanonErrorKind::OutcomeMismatch));
        }
        let log_seq = r.value("log_seq", parse_u64)?;
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        Ok(Self {
            network,
            agent_party,
            agent_key,
            subject,
            subject_hash,
            outcome,
            update_id,
            reason_code,
            code_source,
            log_seq,
            issued_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signable::{SignablePurpose, sign_agent};
    use crate::testkit::{self, at};

    fn executed() -> Receipt {
        Receipt {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            agent_key: testkit::agent_key().verifying_key().to_bytes(),
            subject: ReceiptSubject::Command(
                Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
            ),
            subject_hash: Hash32([3; 32]),
            outcome: ReceiptOutcome::Executed,
            update_id: Some(Token::parse("1220abcd:update-7").unwrap()),
            reason_code: None,
            code_source: None,
            log_seq: 4,
            issued_at: at(60),
        }
    }

    fn refused() -> Receipt {
        Receipt {
            subject: ReceiptSubject::Policy(Hash32([5; 32])),
            outcome: ReceiptOutcome::Refused,
            update_id: None,
            reason_code: Some(4111),
            code_source: Some(CodeSource::Ledger),
            ..executed()
        }
    }

    #[test]
    fn receipts_round_trip_sign_and_verify() {
        for receipt in [executed(), refused()] {
            let signable = receipt.signable().unwrap();
            assert_eq!(signable.purpose(), SignablePurpose::Receipt);
            assert_eq!(Receipt::decode(signable.bytes()).unwrap(), receipt);
            let signature = sign_agent(&testkit::agent_key(), &signable);
            let verified =
                Receipt::verify(signable.bytes(), &signature, &testkit::agent_party()).unwrap();
            assert_eq!(verified, receipt);
            let other = crate::party::PartyId::parse(&format!(
                "agent-1::{}",
                crate::party::namespace_for_key(&[9; 32])
            ))
            .unwrap();
            let refusal = Receipt::verify(signable.bytes(), &signature, &other).unwrap_err();
            assert_eq!(refusal.code_name(), "wrong_party");
            let mut forged = signature;
            forged[0] ^= 1;
            let refusal =
                Receipt::verify(signable.bytes(), &forged, &testkit::agent_party()).unwrap_err();
            assert_eq!(refusal.code_name(), "bad_signature");
        }
        let text = String::from_utf8(executed().encode().unwrap()).unwrap();
        assert!(
            text.contains("\nsubject=command\nsubject_id=01890a5d-ac96-774b-bcce-b302099a8057\n")
        );
        assert!(text.ends_with(
            "\noutcome=executed\nupdate_id=1220abcd:update-7\nreason_code=\ncode_source=\n\
             log_seq=4\nissued_at=2026-11-22T12:01:00Z\n"
        ));
    }

    #[test]
    fn a_receipt_for_another_key_is_refused() {
        let mut receipt = executed();
        receipt.agent_key = testkit::ed_key(0x21).verifying_key().to_bytes();
        let signable = receipt.signable().unwrap();
        let signature = sign_agent(&testkit::ed_key(0x21), &signable);
        let refusal =
            Receipt::verify(signable.bytes(), &signature, &testkit::agent_party()).unwrap_err();
        assert_eq!(refusal.code_name(), "key_party_mismatch");
    }

    #[test]
    fn outcome_coupling_is_enforced() {
        let executed_text = String::from_utf8(executed().encode().unwrap()).unwrap();
        let refused_text = String::from_utf8(refused().encode().unwrap()).unwrap();
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "executed without update",
                executed_text.replacen("update_id=1220abcd:update-7", "update_id=", 1),
                "outcome_mismatch",
            ),
            (
                "refused with update",
                refused_text.replacen("update_id=", "update_id=u-1", 1),
                "outcome_mismatch",
            ),
            (
                "refused without code",
                refused_text.replacen("reason_code=4111", "reason_code=", 1),
                "outcome_mismatch",
            ),
            (
                "code without source",
                refused_text.replacen("code_source=ledger", "code_source=", 1),
                "outcome_mismatch",
            ),
            (
                "executed policy",
                refused_text.replacen("outcome=refused", "outcome=executed", 1),
                "outcome_mismatch",
            ),
            (
                "source",
                refused_text.replacen("code_source=ledger", "code_source=rpc", 1),
                "invalid_code_source",
            ),
            (
                "outcome",
                refused_text.replacen("outcome=refused", "outcome=denied", 1),
                "invalid_outcome",
            ),
            (
                "subject",
                refused_text.replacen("subject=policy", "subject=entry", 1),
                "invalid_receipt_subject",
            ),
            (
                "policy id",
                refused_text.replacen("subject=policy", "subject=command", 1),
                "invalid_uuid7",
            ),
            (
                "code",
                refused_text.replacen("reason_code=4111", "reason_code=04111", 1),
                "invalid_u64",
            ),
        ];
        for (name, text, expected) in cases {
            let err = Receipt::decode(text.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{name}");
        }
        let mut expired = refused();
        expired.outcome = ReceiptOutcome::Expired;
        assert_eq!(
            expired.encode().unwrap_err().code_name(),
            "outcome_mismatch"
        );
        expired.reason_code = None;
        expired.code_source = None;
        assert!(expired.encode().is_ok());
        let mut failed = executed();
        failed.outcome = ReceiptOutcome::Failed;
        failed.update_id = None;
        failed.reason_code = Some(17);
        failed.code_source = Some(CodeSource::Agent);
        assert_eq!(Receipt::decode(&failed.encode().unwrap()).unwrap(), failed);
    }
}
