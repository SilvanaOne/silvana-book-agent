//! Controller commands (`silvana.agent.command.v1`): one directed transfer.

use crate::SCHEME_COMMAND;
use crate::canon::time::UtcTime;
use crate::canon::values::{
    Amount, EthAddress, Hash32, InstrumentId, InstrumentRef, Network, Text, Token, Uuid7,
    parse_u64, sha256,
};
use crate::canon::{Reader, Writer, decode_exact};
use crate::controller::{Capability, ControllerId};
use crate::error::{CanonError, CanonErrorKind, CommandError, ValueError, ValueKind};
use crate::log::HeadRef;
use crate::party::PartyId;
use crate::policy::eval::{Coverage, EvalContext, Intent, Recipient, is_self};
use crate::policy::kinds::OperationKind;
use crate::proof::{ProofEnvelope, ProofOutcome, verify_controller_proof};
use crate::state::{ControlState, add_secs};

/// Largest command, in bytes.
pub const MAX_COMMAND_BYTES: usize = 4096;
/// Longest time from `issued_at` to `expires_at`.
pub const MAX_COMMAND_VALIDITY_SECS: u64 = 900;
/// How far `issued_at` may run ahead of the verifier's clock.
pub const COMMAND_CLOCK_SKEW_SECS: u64 = 120;
/// Prefix of the Canton command id of a directed transaction.
pub const CANTON_COMMAND_PREFIX: &str = "ctl-";

/// The operation a command directs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommandOp {
    TransferCc {
        receiver: PartyId,
        amount: Amount,
        description: Option<Text<140>>,
        settlement_proposal_id: Option<Token>,
    },
    /// A CIP-56 transfer, USDCx included.
    TransferToken {
        receiver: PartyId,
        instrument: InstrumentRef,
        amount: Amount,
        reference: Option<Text<140>>,
    },
    UsdcxBurn {
        amount: Amount,
        destination: EthAddress,
    },
}

impl CommandOp {
    pub fn name(&self) -> &'static str {
        match self {
            Self::TransferCc { .. } => "transfer_cc",
            Self::TransferToken { .. } => "transfer_token",
            Self::UsdcxBurn { .. } => "usdcx_burn",
        }
    }

    /// The operation kind the directed transaction must have.
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::TransferCc { .. } => OperationKind::TransferCc,
            Self::TransferToken { .. } => OperationKind::TransferCip56,
            Self::UsdcxBurn { .. } => OperationKind::UsdcxBurn,
        }
    }

    pub fn amount(&self) -> Amount {
        match self {
            Self::TransferCc { amount, .. }
            | Self::TransferToken { amount, .. }
            | Self::UsdcxBurn { amount, .. } => *amount,
        }
    }
}

/// A transfer one `direct` controller signed for one agent at one log head.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Command {
    pub network: Network,
    pub agent_party: PartyId,
    pub command_id: Uuid7,
    pub log_seq: u64,
    pub log_head: Hash32,
    pub issued_at: UtcTime,
    pub expires_at: UtcTime,
    pub op: CommandOp,
    pub summary: Text<280>,
}

fn validity_ok(issued_at: UtcTime, expires_at: UtcTime) -> bool {
    u64::try_from(issued_at.secs_until(expires_at))
        .is_ok_and(|span| (1..=MAX_COMMAND_VALIDITY_SECS).contains(&span))
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

fn parse_op_name(s: &str) -> Result<&'static str, ValueError> {
    ["transfer_cc", "transfer_token", "usdcx_burn"]
        .into_iter()
        .find(|name| *name == s)
        .ok_or(ValueError(ValueKind::CommandOp))
}

fn write_amount(w: &mut Writer, amount: &Amount) {
    w.line("op.amount", &amount.to_string());
    if amount.is_zero() {
        w.fail(CanonErrorKind::InvalidValue(ValueKind::Amount));
    }
}

impl Command {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_COMMAND, MAX_COMMAND_BYTES);
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("command_id", self.command_id.as_str());
        w.line("log_seq", &self.log_seq.to_string());
        w.line("log_head", &self.log_head.to_string());
        w.line("issued_at", &self.issued_at.to_string());
        w.line("expires_at", &self.expires_at.to_string());
        if !validity_ok(self.issued_at, self.expires_at) {
            w.fail(CanonErrorKind::InvalidValidity);
        }
        w.line("op", self.op.name());
        match &self.op {
            CommandOp::TransferCc {
                receiver,
                amount,
                description,
                settlement_proposal_id,
            } => {
                w.line("op.receiver", receiver.as_str());
                write_amount(&mut w, amount);
                w.opt("op.description", description.as_ref().map(Text::as_str));
                w.opt(
                    "op.settlement_proposal_id",
                    settlement_proposal_id.as_ref().map(Token::as_str),
                );
            }
            CommandOp::TransferToken {
                receiver,
                instrument,
                amount,
                reference,
            } => {
                w.line("op.receiver", receiver.as_str());
                w.line("op.instrument_admin", instrument.admin.as_str());
                w.line("op.instrument_id", instrument.id.as_str());
                write_amount(&mut w, amount);
                w.opt("op.reference", reference.as_ref().map(Text::as_str));
            }
            CommandOp::UsdcxBurn {
                amount,
                destination,
            } => {
                write_amount(&mut w, amount);
                w.line("op.destination", &destination.to_string());
            }
        }
        w.line("summary", self.summary.as_str());
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_COMMAND,
            MAX_COMMAND_BYTES,
            Self::read,
            Self::encode,
        )
    }

    /// SHA-256 of the encoded command.
    pub fn hash(&self) -> Result<Hash32, CanonError> {
        self.encode().map(|bytes| Hash32(sha256(&bytes)))
    }

    /// The Canton command id of the directed transaction.
    pub fn canton_command_id(&self) -> String {
        format!("{CANTON_COMMAND_PREFIX}{}", self.command_id)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let command_id = r.value("command_id", Uuid7::parse)?;
        let log_seq = r.value("log_seq", parse_u64)?;
        let log_head = r.value("log_head", Hash32::parse)?;
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        let expires_at = r.value("expires_at", UtcTime::parse)?;
        if !validity_ok(issued_at, expires_at) {
            return Err(r.err(CanonErrorKind::InvalidValidity));
        }
        let op = match r.value("op", parse_op_name)? {
            "transfer_cc" => CommandOp::TransferCc {
                receiver: r.value("op.receiver", PartyId::parse)?,
                amount: r.value("op.amount", positive_amount)?,
                description: r.opt_value("op.description", Text::parse)?,
                settlement_proposal_id: r.opt_value("op.settlement_proposal_id", Token::parse)?,
            },
            "transfer_token" => {
                let receiver = r.value("op.receiver", PartyId::parse)?;
                let admin = r.value("op.instrument_admin", PartyId::parse)?;
                let id = r.value("op.instrument_id", InstrumentId::parse)?;
                CommandOp::TransferToken {
                    receiver,
                    instrument: InstrumentRef { admin, id },
                    amount: r.value("op.amount", positive_amount)?,
                    reference: r.opt_value("op.reference", Text::parse)?,
                }
            }
            _ => CommandOp::UsdcxBurn {
                amount: r.value("op.amount", positive_amount)?,
                destination: r.value("op.destination", EthAddress::parse)?,
            },
        };
        let summary = r.value("summary", Text::parse)?;
        Ok(Self {
            network,
            agent_party,
            command_id,
            log_seq,
            log_head,
            issued_at,
            expires_at,
            op,
            summary,
        })
    }
}

/// A command that passed `verify_command`; single use is the caller's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCommand {
    pub command: Command,
    pub bytes: Vec<u8>,
    pub hash: Hash32,
    pub controller_id: ControllerId,
    pub outcome: ProofOutcome,
}

/// Checks a signed command against the current state and clock.
pub fn verify_command(
    state: &ControlState,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    now: UtcTime,
) -> Result<VerifiedCommand, CommandError> {
    let command = Command::decode(bytes).map_err(CommandError::Canon)?;
    if command.network != state.network() {
        return Err(CommandError::WrongNetwork);
    }
    if command.agent_party != *state.agent_party() {
        return Err(CommandError::WrongParty);
    }
    let signer = state
        .controller(envelope.controller_id())
        .ok_or(CommandError::UnknownController)?;
    if !signer.capabilities_at(now).contains(Capability::Direct) {
        return Err(CommandError::SignerLacksDirect);
    }
    let signed_at = HeadRef {
        seq: command.log_seq,
        hash: command.log_head,
    };
    if state.head() != Some(signed_at) {
        return Err(CommandError::StaleLogHead);
    }
    let latest_issue = add_secs(now, COMMAND_CLOCK_SKEW_SECS).ok_or(CommandError::TimeOverflow)?;
    if command.issued_at > latest_issue {
        return Err(CommandError::IssuedInFuture);
    }
    if now > command.expires_at {
        return Err(CommandError::Expired);
    }
    let outcome = verify_controller_proof(signer.descriptor(), bytes, envelope)
        .map_err(CommandError::Proof)?;
    Ok(VerifiedCommand {
        command,
        bytes: bytes.to_vec(),
        hash: Hash32(sha256(bytes)),
        controller_id: *envelope.controller_id(),
        outcome,
    })
}

/// True when the verified outflows are exactly the command's leg, with change only to self.
pub fn command_matches(op: &CommandOp, intent: &Intent, ctx: &EvalContext<'_>) -> bool {
    if intent.coverage != Coverage::Fields || !intent.sub.is_empty() || intent.kind != op.kind() {
        return false;
    }
    let (instrument, to) = match op {
        CommandOp::TransferCc { receiver, .. } => {
            (Some(ctx.cc), Recipient::Party(receiver.clone()))
        }
        CommandOp::TransferToken {
            receiver,
            instrument,
            ..
        } => (Some(instrument), Recipient::Party(receiver.clone())),
        CommandOp::UsdcxBurn { destination, .. } => (None, Recipient::Eth(*destination)),
    };
    let mut total = Amount::ZERO;
    let mut legs = false;
    for flow in &intent.outgoing {
        if is_self(flow, ctx.agent_party) {
            continue;
        }
        if flow.to.as_ref() != Some(&to) || instrument.is_some_and(|i| *i != flow.instrument) {
            return false;
        }
        match total.checked_add(flow.amount) {
            Some(sum) => total = sum,
            None => return false,
        }
        legs = true;
    }
    legs && total == op.amount()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::CapabilitySet;
    use crate::policy::eval::{Flow, PlatformParties, UsageTotals};
    use crate::testkit::{self, Chain, DAY, at};

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn party(hint: &str, key: u8) -> PartyId {
        PartyId::parse(&format!(
            "{hint}::{}",
            crate::party::namespace_for_key(&[key; 32])
        ))
        .unwrap()
    }

    fn usdc() -> InstrumentRef {
        InstrumentRef {
            admin: party("Bank", 2),
            id: InstrumentId::parse("USDCx").unwrap(),
        }
    }

    fn command_at(state: &ControlState, op: CommandOp, issued_at: UtcTime) -> Command {
        let head = state.head().unwrap();
        Command {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
            log_seq: head.seq,
            log_head: head.hash,
            issued_at,
            expires_at: issued_at.checked_add_secs(600).unwrap(),
            op,
            summary: Text::parse("Pay alice 10 CC").unwrap(),
        }
    }

    fn pay_alice() -> CommandOp {
        CommandOp::TransferCc {
            receiver: party("alice", 3),
            amount: amount("10"),
            description: Some(Text::parse("rent").unwrap()),
            settlement_proposal_id: None,
        }
    }

    #[test]
    fn every_op_round_trips() {
        let state = Chain::linked().state;
        let ops = [
            pay_alice(),
            CommandOp::TransferCc {
                receiver: party("alice", 3),
                amount: amount("0.5"),
                description: None,
                settlement_proposal_id: Some(Token::parse("settle-42").unwrap()),
            },
            CommandOp::TransferToken {
                receiver: party("bob", 4),
                instrument: usdc(),
                amount: amount("2.25"),
                reference: Some(Text::parse("invoice 7").unwrap()),
            },
            CommandOp::UsdcxBurn {
                amount: amount("100"),
                destination: EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7")
                    .unwrap(),
            },
        ];
        for op in ops {
            let command = command_at(&state, op, at(DAY));
            let bytes = command.encode().unwrap();
            assert_eq!(Command::decode(&bytes).unwrap(), command);
            assert_eq!(command.hash().unwrap(), Hash32(sha256(&bytes)));
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains(&format!("\nop={}\n", command.op.name())));
        }
        let command = command_at(&state, pay_alice(), at(DAY));
        assert_eq!(
            command.canton_command_id(),
            "ctl-01890a5d-ac96-774b-bcce-b302099a8057"
        );
        let text = String::from_utf8(command.encode().unwrap()).unwrap();
        let expected_tail = format!(
            "\nop=transfer_cc\nop.receiver={}\nop.amount=10\nop.description=rent\n\
             op.settlement_proposal_id=\nsummary=Pay alice 10 CC\n",
            party("alice", 3)
        );
        assert!(text.ends_with(&expected_tail), "{text}");
        assert!(text.starts_with("scheme=silvana.agent.command.v1\nnetwork=devnet\n"));
    }

    #[test]
    fn decoding_refusals_have_stable_names() {
        let state = Chain::linked().state;
        let good =
            String::from_utf8(command_at(&state, pay_alice(), at(DAY)).encode().unwrap()).unwrap();
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "zero amount",
                good.replacen("op.amount=10", "op.amount=0", 1),
                "invalid_amount",
            ),
            (
                "unknown op",
                good.replacen("op=transfer_cc", "op=transfer_all", 1),
                "invalid_command_op",
            ),
            (
                "too long",
                good.replacen(
                    "expires_at=2026-11-23T12:10:00Z",
                    "expires_at=2026-11-23T12:15:01Z",
                    1,
                ),
                "invalid_validity",
            ),
            (
                "backwards",
                good.replacen(
                    "expires_at=2026-11-23T12:10:00Z",
                    "expires_at=2026-11-23T12:00:00Z",
                    1,
                ),
                "invalid_validity",
            ),
            (
                "uuid v4",
                good.replacen("-774b-", "-474b-", 1),
                "invalid_uuid7",
            ),
            (
                "empty summary",
                good.replacen("summary=Pay alice 10 CC", "summary=", 1),
                "empty_value",
            ),
            (
                "double space",
                good.replacen("summary=Pay alice", "summary=Pay  alice", 1),
                "invalid_text",
            ),
            (
                "missing field",
                good.replacen("op.settlement_proposal_id=\n", "", 1),
                "unexpected_key",
            ),
            (
                "scheme",
                good.replacen("command.v1", "command.v2", 1),
                "wrong_scheme",
            ),
        ];
        for (name, text, expected) in cases {
            let err = Command::decode(text.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{name}");
        }
        let mut long = command_at(&state, pay_alice(), at(DAY));
        long.expires_at = at(DAY + 901);
        assert_eq!(long.encode().unwrap_err().code_name(), "invalid_validity");
        let mut zero = command_at(&state, pay_alice(), at(DAY));
        zero.op = CommandOp::UsdcxBurn {
            amount: Amount::ZERO,
            destination: EthAddress([1; 20]),
        };
        assert_eq!(zero.encode().unwrap_err().code_name(), "invalid_amount");
    }

    #[test]
    fn verification_checks_the_signer_head_window_and_proof() {
        let chain = Chain::linked();
        let state = &chain.state;
        let now = at(DAY);
        let command = command_at(state, pay_alice(), now);
        let bytes = command.encode().unwrap();
        let verified =
            verify_command(state, &bytes, &testkit::passkey_envelope(&bytes), now).unwrap();
        assert_eq!(verified.command, command);
        assert_eq!(verified.controller_id, testkit::passkey_id());
        assert_eq!(verified.outcome.sign_count, Some(0));
        let refuse = |command: &Command, signer: u8, when: UtcTime| {
            let bytes = command.encode().unwrap();
            let envelope = match signer {
                0 => testkit::passkey_envelope(&bytes),
                start => testkit::ed25519_envelope(start, &bytes),
            };
            verify_command(state, &bytes, &envelope, when)
                .unwrap_err()
                .code_name()
        };
        let later = |secs: i64| now.checked_add_secs(secs).unwrap();
        assert_eq!(refuse(&command, 0, at(DAY - 1)), "signer_lacks_direct");
        assert_eq!(refuse(&command, 0x21, now), "signer_lacks_direct");
        assert_eq!(refuse(&command, 0x99, now), "unknown_controller");
        assert_eq!(refuse(&command, 0, later(601)), "expired");
        let at_expiry = verify_command(
            state,
            &bytes,
            &testkit::passkey_envelope(&bytes),
            later(600),
        );
        assert!(at_expiry.is_ok());
        let early = command_at(state, pay_alice(), later(121));
        assert_eq!(refuse(&early, 0, now), "issued_in_future");
        let skewed = command_at(state, pay_alice(), later(120));
        let skewed_bytes = skewed.encode().unwrap();
        let envelope = testkit::passkey_envelope(&skewed_bytes);
        assert!(verify_command(state, &skewed_bytes, &envelope, now).is_ok());
        let mut stale = command.clone();
        stale.log_seq = 2;
        assert_eq!(refuse(&stale, 0, now), "stale_log_head");
        let mut other_hash = command.clone();
        other_hash.log_head = Hash32([9; 32]);
        assert_eq!(refuse(&other_hash, 0, now), "stale_log_head");
        let mut mainnet = command.clone();
        mainnet.network = Network::Mainnet;
        assert_eq!(refuse(&mainnet, 0, now), "wrong_network");
        let mut other_agent = command.clone();
        other_agent.agent_party = party("other", 9);
        assert_eq!(refuse(&other_agent, 0, now), "wrong_party");
        let wrong_payload = verify_command(
            state,
            &bytes,
            &testkit::passkey_envelope(b"other bytes"),
            now,
        );
        assert_eq!(wrong_payload.unwrap_err().code_name(), "payload_mismatch");
        assert_eq!(
            verify_command(state, b"junk", &testkit::passkey_envelope(b"junk"), now)
                .unwrap_err()
                .code_name(),
            "too_short"
        );

        let mut moved = Chain::linked();
        let key = testkit::ed_key(0x51).verifying_key().to_bytes();
        let label = crate::canon::values::Label::parse("Spare key").unwrap();
        let pending = moved
            .builder()
            .add_ed25519(key, label, CapabilitySet::of(&[Capability::Read]), now)
            .unwrap();
        moved.push(&pending);
        let envelope = testkit::passkey_envelope(&bytes);
        assert_eq!(
            verify_command(&moved.state, &bytes, &envelope, now)
                .unwrap_err()
                .code_name(),
            "stale_log_head"
        );
    }

    #[test]
    fn ed25519_commands_verify_until_a_pending_removal_takes_effect() {
        let mut chain = Chain::new();
        let descriptor = testkit::link_descriptor(
            vec![
                testkit::ed25519_controller(CapabilitySet::ALL),
                testkit::passkey_controller(CapabilitySet::ALL),
            ],
            0x11,
        );
        let bytes = descriptor.encode().unwrap();
        let commitment = descriptor.commitment().unwrap();
        let pending = chain
            .builder()
            .link(&bytes, &commitment, &testkit::rp(), false, at(0))
            .unwrap();
        chain.push(&pending);
        let removal = chain
            .builder()
            .remove_controller(&testkit::ed25519_id(0x21), false, at(DAY))
            .unwrap();
        chain.push(&removal);
        let state = &chain.state;
        let command = command_at(state, pay_alice(), at(2 * DAY - 300));
        let bytes = command.encode().unwrap();
        let envelope = testkit::ed25519_envelope(0x21, &bytes);
        let verified = verify_command(state, &bytes, &envelope, at(2 * DAY - 1)).unwrap();
        assert_eq!(verified.controller_id, testkit::ed25519_id(0x21));
        assert_eq!(verified.outcome.sign_count, None);
        let after = verify_command(state, &bytes, &envelope, at(2 * DAY)).unwrap_err();
        assert_eq!(after.code_name(), "signer_lacks_direct");
        let mut forged = envelope.clone();
        if let crate::proof::ControllerProof::Ed25519 { signature } = envelope.proof() {
            let mut signature = *signature;
            signature[5] ^= 1;
            forged = ProofEnvelope::new(
                *envelope.controller_id(),
                &bytes,
                crate::proof::ControllerProof::Ed25519 { signature },
            );
        }
        let bad = verify_command(state, &bytes, &forged, at(2 * DAY - 1)).unwrap_err();
        assert_eq!(bad.code_name(), "bad_signature");
    }

    #[test]
    fn commands_match_exactly_their_leg_with_change_to_self() {
        let agent = testkit::agent_party();
        let alice = party("alice", 3);
        let cc = InstrumentRef {
            admin: party("DSO", 1),
            id: InstrumentId::parse("Amulet").unwrap(),
        };
        let platform = PlatformParties::NONE;
        let usage = UsageTotals::new();
        let ctx = EvalContext {
            agent_party: &agent,
            cc: &cc,
            platform: &platform,
            now: at(0),
            usage: &usage,
        };
        let flow = |instrument: &InstrumentRef, value: &str, to: &PartyId| Flow {
            instrument: instrument.clone(),
            amount: amount(value),
            to: Some(Recipient::Party(to.clone())),
        };
        let intent = |kind: OperationKind, outgoing: Vec<Flow>| Intent {
            outgoing,
            ..Intent::new(kind)
        };
        let op = pay_alice();
        let cases = [
            (
                intent(OperationKind::TransferCc, vec![flow(&cc, "10", &alice)]),
                true,
            ),
            (
                intent(
                    OperationKind::TransferCc,
                    vec![
                        flow(&cc, "4", &alice),
                        flow(&cc, "6", &alice),
                        flow(&cc, "90", &agent),
                    ],
                ),
                true,
            ),
            (
                intent(OperationKind::TransferCc, vec![flow(&cc, "9", &alice)]),
                false,
            ),
            (
                intent(OperationKind::TransferCc, vec![flow(&usdc(), "10", &alice)]),
                false,
            ),
            (
                intent(OperationKind::TransferCc, vec![flow(&cc, "10", &agent)]),
                false,
            ),
            (
                intent(OperationKind::TransferCip56, vec![flow(&cc, "10", &alice)]),
                false,
            ),
            (
                Intent {
                    coverage: Coverage::Unverified,
                    ..intent(OperationKind::TransferCc, vec![flow(&cc, "10", &alice)])
                },
                false,
            ),
        ];
        for (intent, expected) in &cases {
            assert_eq!(command_matches(&op, intent, &ctx), *expected, "{intent:?}");
        }
        let destination = EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7").unwrap();
        let burn = CommandOp::UsdcxBurn {
            amount: amount("5"),
            destination,
        };
        let burned = Intent {
            outgoing: vec![Flow {
                instrument: usdc(),
                amount: amount("5"),
                to: Some(Recipient::Eth(destination)),
            }],
            ..Intent::new(OperationKind::UsdcxBurn)
        };
        assert!(command_matches(&burn, &burned, &ctx));
        let elsewhere = Intent {
            outgoing: vec![Flow {
                to: Some(Recipient::Eth(EthAddress([1; 20]))),
                ..burned.outgoing[0].clone()
            }],
            ..burned.clone()
        };
        assert!(!command_matches(&burn, &elsewhere, &ctx));
    }
}
