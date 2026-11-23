//! Agent-signed receipts for commands and policy refusals: `silvana.agent.receipt.v1`,
//! and `silvana.agent.receipt.v2` for an executed swap, which adds the swap's result.

use crate::canon::time::UtcTime;
use crate::canon::values::{Amount, Hash32, Network, Token, Uuid7, parse_hex_array, parse_u64};
use crate::canon::{Reader, Writer, decode_exact};
use crate::error::{CanonError, CanonErrorKind, SignedDocError, ValueError, ValueKind};
use crate::party::{PartyId, verify_party_key};
use crate::sig::ed25519::verify_ed25519_strict;
use crate::signable::AgentSignable;
use crate::{SCHEME_RECEIPT, SCHEME_RECEIPT_V2};

/// Largest receipt, in bytes.
pub const MAX_RECEIPT_BYTES: usize = 2048;
/// Largest v2 receipt, in bytes.
pub const MAX_RECEIPT_V2_BYTES: usize = 3072;

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

/// A policy is never executed; a v2 receipt is only for an executed command.
fn outcome_fits_subject(subject: &ReceiptSubject, outcome: ReceiptOutcome, v2: bool) -> bool {
    match subject {
        ReceiptSubject::Policy(_) => outcome != ReceiptOutcome::Executed && !v2,
        ReceiptSubject::Command(_) => outcome == ReceiptOutcome::Executed || !v2,
    }
}

/// Checks the party, the key binding and the agent's signature over `bytes`.
fn check_signed(
    receipt: &Receipt,
    bytes: &[u8],
    signature: &[u8; 64],
    expected_party: &PartyId,
) -> Result<(), SignedDocError> {
    if receipt.agent_party != *expected_party {
        return Err(SignedDocError::WrongParty);
    }
    if !verify_party_key(&receipt.agent_party, &receipt.agent_key) {
        return Err(SignedDocError::KeyPartyMismatch);
    }
    verify_ed25519_strict(&receipt.agent_key, bytes, signature).map_err(SignedDocError::Signature)
}

impl Receipt {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_RECEIPT, MAX_RECEIPT_BYTES);
        self.write_fields(&mut w, false);
        w.finish()
    }

    /// Every line after the scheme; `v2` allows only an executed command.
    fn write_fields(&self, w: &mut Writer, v2: bool) {
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("agent_key", &hex::encode(self.agent_key));
        w.line("subject", self.subject.kind_str());
        w.line("subject_id", &self.subject.id_string());
        w.line("subject_hash", &self.subject_hash.to_string());
        w.line("outcome", self.outcome.as_str());
        if !outcome_fits_subject(&self.subject, self.outcome, v2) {
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
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_RECEIPT,
            MAX_RECEIPT_BYTES,
            |r| Self::read_fields(r, false),
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
        check_signed(&receipt, bytes, signature, expected_party)?;
        Ok(receipt)
    }

    fn read_fields(r: &mut Reader<'_>, v2: bool) -> Result<Self, CanonError> {
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
        if !outcome_fits_subject(&subject, outcome, v2) {
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

/// What an executed swap settled.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwapResult {
    /// The RFQ quote the agent accepted.
    pub quote_id: Token,
    /// The LP that settled it.
    pub lp: PartyId,
    /// The amount of the sold instrument delivered.
    pub sold: Amount,
    /// The amount of the bought instrument received, before any CC fee.
    pub received: Amount,
    /// The CC fees paid in total.
    pub fee_cc: Amount,
}

/// The v2 receipt of an executed swap: the v1 fields, then `result.*`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwapReceipt {
    /// Subject `command` and outcome `executed`; anything else is `outcome_mismatch`.
    pub receipt: Receipt,
    pub result: SwapResult,
}

fn positive_amount(s: &str) -> Result<Amount, ValueError> {
    Amount::parse(s).and_then(|amount| {
        if amount.is_zero() {
            Err(ValueError(ValueKind::Amount))
        } else {
            Ok(amount)
        }
    })
}

impl SwapReceipt {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_RECEIPT_V2, MAX_RECEIPT_V2_BYTES);
        self.receipt.write_fields(&mut w, true);
        let result = &self.result;
        w.line("result.quote_id", result.quote_id.as_str());
        w.line("result.lp", result.lp.as_str());
        for (key, amount) in [
            ("result.sold", result.sold),
            ("result.received", result.received),
        ] {
            w.line(key, &amount.to_string());
            if amount.is_zero() {
                w.fail(CanonErrorKind::InvalidValue(ValueKind::Amount));
            }
        }
        w.line("result.fee_cc", &result.fee_cc.to_string());
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_RECEIPT_V2,
            MAX_RECEIPT_V2_BYTES,
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
        check_signed(&receipt.receipt, bytes, signature, expected_party)?;
        Ok(receipt)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let receipt = Receipt::read_fields(r, true)?;
        let result = SwapResult {
            quote_id: r.value("result.quote_id", Token::parse)?,
            lp: r.value("result.lp", PartyId::parse)?,
            sold: r.value("result.sold", positive_amount)?,
            received: r.value("result.received", positive_amount)?,
            fee_cc: r.value("result.fee_cc", Amount::parse)?,
        };
        Ok(Self { receipt, result })
    }
}

/// A receipt of either version, told apart by the scheme line.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AnyReceipt {
    V1(Receipt),
    V2(SwapReceipt),
}

impl AnyReceipt {
    /// Decodes v2 when the scheme line says so, otherwise strictly as v1.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        if bytes.starts_with(format!("scheme={SCHEME_RECEIPT_V2}\n").as_bytes()) {
            SwapReceipt::decode(bytes).map(Self::V2)
        } else {
            Receipt::decode(bytes).map(Self::V1)
        }
    }

    /// Decodes either version and checks the party, the key binding and the signature.
    pub fn verify(
        bytes: &[u8],
        signature: &[u8; 64],
        expected_party: &PartyId,
    ) -> Result<Self, SignedDocError> {
        let receipt = Self::decode(bytes).map_err(SignedDocError::Canon)?;
        check_signed(receipt.receipt(), bytes, signature, expected_party)?;
        Ok(receipt)
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        match self {
            Self::V1(receipt) => receipt.encode(),
            Self::V2(receipt) => receipt.encode(),
        }
    }

    /// The bytes the agent signs.
    pub fn signable(&self) -> Result<AgentSignable, CanonError> {
        self.encode().map(AgentSignable::receipt)
    }

    /// The fields both versions carry.
    pub fn receipt(&self) -> &Receipt {
        match self {
            Self::V1(receipt) => receipt,
            Self::V2(swap) => &swap.receipt,
        }
    }

    /// The result of an executed swap; `None` for a v1 receipt.
    pub fn swap_result(&self) -> Option<&SwapResult> {
        match self {
            Self::V1(_) => None,
            Self::V2(swap) => Some(&swap.result),
        }
    }

    pub fn scheme(&self) -> &'static str {
        match self {
            Self::V1(_) => SCHEME_RECEIPT,
            Self::V2(_) => SCHEME_RECEIPT_V2,
        }
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

    fn lp() -> PartyId {
        PartyId::parse(&format!(
            "lp-1::{}",
            crate::party::namespace_for_key(&[7; 32])
        ))
        .unwrap()
    }

    fn swapped() -> SwapReceipt {
        SwapReceipt {
            receipt: executed(),
            result: SwapResult {
                quote_id: Token::parse("q-01890a5d").unwrap(),
                lp: lp(),
                sold: Amount::parse("1000").unwrap(),
                received: Amount::parse("151").unwrap(),
                fee_cc: Amount::parse("1.5").unwrap(),
            },
        }
    }

    #[test]
    fn a_swap_receipt_is_v1_with_its_result_appended() {
        let receipt = swapped();
        let bytes = receipt.encode().unwrap();
        let v1 = String::from_utf8(executed().encode().unwrap()).unwrap();
        let expected = format!(
            "{}result.quote_id=q-01890a5d\nresult.lp={}\nresult.sold=1000\n\
             result.received=151\nresult.fee_cc=1.5\n",
            v1.replacen("receipt.v1\n", "receipt.v2\n", 1),
            lp()
        );
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), expected);
        assert!(bytes.len() <= MAX_RECEIPT_V2_BYTES);
        assert_eq!(SwapReceipt::decode(&bytes).unwrap(), receipt);
        assert_eq!(
            Receipt::decode(&bytes).unwrap_err().code_name(),
            "wrong_scheme"
        );
        assert_eq!(
            SwapReceipt::decode(v1.as_bytes()).unwrap_err().code_name(),
            "wrong_scheme"
        );
        let signable = receipt.signable().unwrap();
        assert_eq!(signable.purpose(), SignablePurpose::Receipt);
        assert_eq!(signable.bytes(), bytes.as_slice());
        let signature = sign_agent(&testkit::agent_key(), &signable);
        let party = testkit::agent_party();
        assert_eq!(
            SwapReceipt::verify(&bytes, &signature, &party).unwrap(),
            receipt
        );
        let any = AnyReceipt::verify(&bytes, &signature, &party).unwrap();
        assert_eq!(any, AnyReceipt::V2(receipt.clone()));
        assert_eq!(any.receipt(), &receipt.receipt);
        assert_eq!(any.swap_result(), Some(&receipt.result));
        assert_eq!(any.scheme(), "silvana.agent.receipt.v2");
        assert_eq!(any.encode().unwrap(), bytes);
        assert_eq!(any.signable().unwrap(), signable);
        let refusal = |bytes: &[u8], signature: &[u8; 64], party: &PartyId| {
            AnyReceipt::verify(bytes, signature, party)
                .unwrap_err()
                .code_name()
        };
        assert_eq!(refusal(&bytes, &signature, &lp()), "wrong_party");
        let mut forged = signature;
        forged[9] ^= 1;
        assert_eq!(refusal(&bytes, &forged, &party), "bad_signature");
        // The v1 signature does not cover the v2 bytes, nor the reverse.
        let v1_signature = sign_agent(&testkit::agent_key(), &executed().signable().unwrap());
        assert_eq!(refusal(&bytes, &v1_signature, &party), "bad_signature");
        assert_eq!(refusal(v1.as_bytes(), &signature, &party), "bad_signature");
        let v1_any = AnyReceipt::verify(v1.as_bytes(), &v1_signature, &party).unwrap();
        assert_eq!(v1_any, AnyReceipt::V1(executed()));
        assert_eq!(v1_any.swap_result(), None);
        assert_eq!(v1_any.scheme(), "silvana.agent.receipt.v1");
        let mut other_key = receipt.clone();
        other_key.receipt.agent_key = testkit::ed_key(0x21).verifying_key().to_bytes();
        let signable = other_key.signable().unwrap();
        let signature = sign_agent(&testkit::ed_key(0x21), &signable);
        assert_eq!(
            SwapReceipt::verify(signable.bytes(), &signature, &party)
                .unwrap_err()
                .code_name(),
            "key_party_mismatch"
        );
    }

    #[test]
    fn a_swap_receipt_is_only_for_an_executed_command() {
        let mut not_executed = swapped();
        not_executed.receipt = Receipt {
            outcome: ReceiptOutcome::Failed,
            update_id: None,
            reason_code: Some(1401),
            code_source: Some(CodeSource::Agent),
            ..executed()
        };
        assert_eq!(
            not_executed.encode().unwrap_err().code_name(),
            "outcome_mismatch"
        );
        let mut for_policy = swapped();
        for_policy.receipt = refused();
        assert_eq!(
            for_policy.encode().unwrap_err().code_name(),
            "outcome_mismatch"
        );
        let text = String::from_utf8(swapped().encode().unwrap()).unwrap();
        let quote_and_lp = format!("result.quote_id=q-01890a5d\nresult.lp={}\n", lp());
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "failed",
                text.replacen(
                    "outcome=executed\nupdate_id=1220abcd:update-7\nreason_code=\ncode_source=\n",
                    "outcome=failed\nupdate_id=\nreason_code=1401\ncode_source=agent\n",
                    1,
                ),
                "outcome_mismatch",
            ),
            (
                "zero sold",
                text.replacen("result.sold=1000", "result.sold=0", 1),
                "invalid_amount",
            ),
            (
                "zero received",
                text.replacen("result.received=151", "result.received=0", 1),
                "invalid_amount",
            ),
            (
                "empty fee",
                text.replacen("result.fee_cc=1.5", "result.fee_cc=", 1),
                "empty_value",
            ),
            (
                "bare lp",
                text.replacen(&format!("result.lp={}", lp()), "result.lp=lp-1", 1),
                "invalid_party",
            ),
            (
                "quote id",
                text.replacen("result.quote_id=q-01890a5d", "result.quote_id=q 1", 1),
                "invalid_token",
            ),
            (
                "no quote or lp",
                text.replacen(&quote_and_lp, "", 1),
                "unexpected_key",
            ),
            (
                "result cut short",
                text.replacen("result.fee_cc=1.5\n", "", 1),
                "missing_line",
            ),
        ];
        for (name, text, expected) in cases {
            let err = SwapReceipt::decode(text.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{name}");
            let any = AnyReceipt::decode(text.as_bytes()).unwrap_err();
            assert_eq!(any.code_name(), expected, "{name}");
        }
        let feeless = text.replacen("result.fee_cc=1.5", "result.fee_cc=0", 1);
        let decoded = SwapReceipt::decode(feeless.as_bytes()).unwrap();
        assert!(decoded.result.fee_cc.is_zero());
        let v1 = String::from_utf8(executed().encode().unwrap()).unwrap();
        let with_result = format!("{v1}result.quote_id=q-01890a5d\n");
        let err = Receipt::decode(with_result.as_bytes()).unwrap_err();
        assert_eq!(err.code_name(), "trailing_line");
        let v3 = text.replacen("receipt.v2", "receipt.v3", 1);
        assert_eq!(
            AnyReceipt::decode(v3.as_bytes()).unwrap_err().code_name(),
            "wrong_scheme"
        );
    }
}
