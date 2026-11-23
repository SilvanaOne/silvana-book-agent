//! Controller commands (`silvana.agent.command.v1`): one directed transfer or swap.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

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
use crate::policy::Rules;
use crate::policy::eval::{Coverage, EvalContext, Intent, Leg, Recipient, SettleFee, is_self};
use crate::policy::kinds::{OperationClass, OperationKind};
use crate::policy::scopes::{command_allowed, scope_of};
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
    /// Sells exactly `sell_amount` through RFQ V2 for at least `min_receive`, settled
    /// atomically; fees are CC only, to the pinned fee party.
    Swap {
        sell: InstrumentRef,
        sell_amount: Amount,
        buy: InstrumentRef,
        /// Net of the CC fee when `buy` is Canton Coin.
        min_receive: Amount,
        /// The most CC paid in fees; zero takes only fee-less quotes.
        max_fee_cc: Amount,
    },
}

impl CommandOp {
    pub fn name(&self) -> &'static str {
        self.command_kind().as_str()
    }

    /// The operation kind the directed transaction must have.
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::TransferCc { .. } => OperationKind::TransferCc,
            Self::TransferToken { .. } => OperationKind::TransferCip56,
            Self::UsdcxBurn { .. } => OperationKind::UsdcxBurn,
            Self::Swap { .. } => OperationKind::AtomicDvpSettle,
        }
    }

    /// The class whose rules judge the directed transaction.
    pub fn class(&self) -> OperationClass {
        match self {
            Self::Swap { .. } => OperationClass::Trading,
            Self::TransferCc { .. } | Self::TransferToken { .. } | Self::UsdcxBurn { .. } => {
                OperationClass::Transfers
            }
        }
    }

    /// The amount sent: a transfer's, or a swap's sold amount.
    pub fn amount(&self) -> Amount {
        match self {
            Self::TransferCc { amount, .. }
            | Self::TransferToken { amount, .. }
            | Self::UsdcxBurn { amount, .. } => *amount,
            Self::Swap { sell_amount, .. } => *sell_amount,
        }
    }

    /// The kind a command scope names this operation by.
    pub fn command_kind(&self) -> CommandKind {
        match self {
            Self::TransferCc { .. } => CommandKind::TransferCc,
            Self::TransferToken { .. } => CommandKind::TransferToken,
            Self::UsdcxBurn { .. } => CommandKind::UsdcxBurn,
            Self::Swap { .. } => CommandKind::Swap,
        }
    }
}

/// A command kind as a scope lists it; the variant order is the text order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandKind {
    Swap,
    TransferCc,
    TransferToken,
    UsdcxBurn,
}

impl CommandKind {
    pub const ALL: [Self; 4] = [
        Self::Swap,
        Self::TransferCc,
        Self::TransferToken,
        Self::UsdcxBurn,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::TransferCc => "transfer_cc",
            Self::TransferToken => "transfer_token",
            Self::UsdcxBurn => "usdcx_burn",
        }
    }

    /// Exactly one of the names; anything else is `invalid_command_op`.
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or(ValueError(ValueKind::CommandOp))
    }
}

impl fmt::Display for CommandKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CommandKind {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

/// An operation one `direct` controller signed for one agent at one log head.
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

/// A non-zero amount under `key`.
fn write_amount(w: &mut Writer, key: &str, amount: &Amount) {
    w.line(key, &amount.to_string());
    if amount.is_zero() {
        w.fail(CanonErrorKind::InvalidValue(ValueKind::Amount));
    }
}

/// `<prefix>_admin` and `<prefix>_id`.
fn write_instrument(w: &mut Writer, prefix: &str, instrument: &InstrumentRef) {
    w.line(&format!("{prefix}_admin"), instrument.admin.as_str());
    w.line(&format!("{prefix}_id"), instrument.id.as_str());
}

fn read_instrument(r: &mut Reader<'_>, prefix: &str) -> Result<InstrumentRef, CanonError> {
    let admin = r.value(&format!("{prefix}_admin"), PartyId::parse)?;
    let id = r.value(&format!("{prefix}_id"), InstrumentId::parse)?;
    Ok(InstrumentRef { admin, id })
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
                write_amount(&mut w, "op.amount", amount);
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
                write_instrument(&mut w, "op.instrument", instrument);
                write_amount(&mut w, "op.amount", amount);
                w.opt("op.reference", reference.as_ref().map(Text::as_str));
            }
            CommandOp::UsdcxBurn {
                amount,
                destination,
            } => {
                write_amount(&mut w, "op.amount", amount);
                w.line("op.destination", &destination.to_string());
            }
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                max_fee_cc,
            } => {
                write_instrument(&mut w, "op.sell_instrument", sell);
                write_amount(&mut w, "op.sell_amount", sell_amount);
                write_instrument(&mut w, "op.buy_instrument", buy);
                if buy == sell {
                    w.fail(CanonErrorKind::SameInstrument);
                }
                write_amount(&mut w, "op.min_receive", min_receive);
                w.line("op.max_fee_cc", &max_fee_cc.to_string());
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
        let op = match r.value("op", CommandKind::parse)? {
            CommandKind::TransferCc => CommandOp::TransferCc {
                receiver: r.value("op.receiver", PartyId::parse)?,
                amount: r.value("op.amount", positive_amount)?,
                description: r.opt_value("op.description", Text::parse)?,
                settlement_proposal_id: r.opt_value("op.settlement_proposal_id", Token::parse)?,
            },
            CommandKind::TransferToken => {
                let receiver = r.value("op.receiver", PartyId::parse)?;
                let instrument = read_instrument(r, "op.instrument")?;
                CommandOp::TransferToken {
                    receiver,
                    instrument,
                    amount: r.value("op.amount", positive_amount)?,
                    reference: r.opt_value("op.reference", Text::parse)?,
                }
            }
            CommandKind::UsdcxBurn => CommandOp::UsdcxBurn {
                amount: r.value("op.amount", positive_amount)?,
                destination: r.value("op.destination", EthAddress::parse)?,
            },
            CommandKind::Swap => {
                let sell = read_instrument(r, "op.sell_instrument")?;
                let sell_amount = r.value("op.sell_amount", positive_amount)?;
                let buy = read_instrument(r, "op.buy_instrument")?;
                if buy == sell {
                    return Err(r.err(CanonErrorKind::SameInstrument));
                }
                CommandOp::Swap {
                    sell,
                    sell_amount,
                    buy,
                    min_receive: r.value("op.min_receive", positive_amount)?,
                    max_fee_cc: r.value("op.max_fee_cc", Amount::parse)?,
                }
            }
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

/// A command that passed `verify_command`, the only way to obtain one; single use is the
/// caller's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCommand {
    command: Command,
    bytes: Vec<u8>,
    hash: Hash32,
    controller_id: ControllerId,
    outcome: ProofOutcome,
    verified_at: UtcTime,
}

impl VerifiedCommand {
    pub fn command(&self) -> &Command {
        &self.command
    }

    /// The signed bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// SHA-256 of the signed bytes.
    pub fn hash(&self) -> &Hash32 {
        &self.hash
    }

    /// The signer.
    pub fn controller_id(&self) -> &ControllerId {
        &self.controller_id
    }

    pub fn outcome(&self) -> &ProofOutcome {
        &self.outcome
    }

    /// The clock the checks ran at; the evaluator takes the command at no other.
    pub fn verified_at(&self) -> UtcTime {
        self.verified_at
    }

    /// A stub for the crate's own tests, as if `controller_id` signed `bytes`.
    #[cfg(test)]
    pub(crate) fn stub(
        bytes: &[u8],
        controller_id: ControllerId,
        outcome: ProofOutcome,
        verified_at: UtcTime,
    ) -> Self {
        Self {
            command: Command::decode(bytes).unwrap(),
            bytes: bytes.to_vec(),
            hash: Hash32(sha256(bytes)),
            controller_id,
            outcome,
            verified_at,
        }
    }

    /// The same command, as if verified at `verified_at`.
    #[cfg(test)]
    pub(crate) fn verified_again_at(&self, verified_at: UtcTime) -> Self {
        Self {
            verified_at,
            ..self.clone()
        }
    }
}

/// Checks a signed command against the current state, clock and the signer's scope; `cc`
/// is Canton Coin from local config, and without it a swap under listed pairs is refused.
pub fn verify_command(
    state: &ControlState,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    now: UtcTime,
    cc: Option<&InstrumentRef>,
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
    let controller_id = *envelope.controller_id();
    let rules = state.rules_at(now).rules();
    if !command_allowed(rules, &controller_id, command.op.command_kind()) {
        return Err(CommandError::OutOfScope);
    }
    check_swap_bounds(rules, &controller_id, &command.op, cc)?;
    Ok(VerifiedCommand {
        command,
        bytes: bytes.to_vec(),
        hash: Hash32(sha256(bytes)),
        controller_id,
        outcome,
        verified_at: now,
    })
}

/// A swap under listed pairs must name one, with `min_receive / sell_amount >= min_price`
/// and `sell_amount <= max_sell`; `cc` resolves the alias, and without it pairs refuse.
pub fn check_swap_bounds(
    rules: &Rules,
    signer: &ControllerId,
    op: &CommandOp,
    cc: Option<&InstrumentRef>,
) -> Result<(), CommandError> {
    let CommandOp::Swap {
        sell,
        sell_amount,
        buy,
        min_receive,
        ..
    } = op
    else {
        return Ok(());
    };
    let Some(scope) = scope_of(rules, signer).filter(|scope| !scope.pairs.is_empty()) else {
        return Ok(());
    };
    let pair = cc
        .and_then(|cc| scope.pair_for(sell, buy, cc))
        .ok_or(CommandError::SwapPairNotAllowed)?;
    if min_receive.ratio_at_least(*sell_amount, pair.min_price) != Some(true) {
        return Err(CommandError::SwapBelowMinPrice);
    }
    if *sell_amount > pair.max_sell {
        return Err(CommandError::SwapAboveMaxSell);
    }
    Ok(())
}

/// True when the signer's scope lets a swap settle with `lp`; a scope without an LP list,
/// or no scope, allows any. A refusal is `lp_not_allowed`.
pub fn swap_lp_allowed(rules: &Rules, signer: &ControllerId, lp: &PartyId) -> bool {
    scope_of(rules, signer).is_none_or(|scope| scope.lp_allowed(lp))
}

/// `lp_not_allowed` for a swap whose signer's scope lists LPs other than `lp`; any other
/// operation, scope or LP passes.
pub fn check_swap_lp(
    rules: &Rules,
    signer: &ControllerId,
    op: &CommandOp,
    lp: &PartyId,
) -> Result<(), CommandError> {
    match op {
        CommandOp::Swap { .. } if !swap_lp_allowed(rules, signer, lp) => {
            Err(CommandError::LpNotAllowed)
        }
        _ => Ok(()),
    }
}

/// The terms of an atomic settle request, checked before it is prepared.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwapRequest {
    pub lp: PartyId,
    /// What the agent would deliver to the LP.
    pub sent: Leg,
    /// What the LP would deliver to the agent, before any fee.
    pub received: Leg,
    /// The fees the agent would pay.
    pub fees: Vec<SettleFee>,
    /// The quote's expiry, rounded up to the second.
    pub valid_until: UtcTime,
}

/// The fee total when every fee is CC to the pinned fee party; no pin allows no fee.
fn swap_fee_total(fees: &[SettleFee], ctx: &EvalContext<'_>) -> Option<Amount> {
    fees.iter().try_fold(Amount::ZERO, |total, fee| {
        let pinned = ctx.platform.fee.as_ref() == Some(&fee.receiver);
        if pinned && fee.instrument == *ctx.cc {
            total.checked_add(fee.amount)
        } else {
            None
        }
    })
}

/// The terms a swap request and its settled content share.
struct SwapTerms<'a> {
    lp: &'a PartyId,
    sent: &'a Leg,
    received: &'a Leg,
    fees: &'a [SettleFee],
    valid_until: UtcTime,
}

/// The fee total when `terms` meet the swap: its legs, an LP that is not the agent or a pinned
/// platform party, capped CC fees to the fee party, `min_receive` net, no later expiry.
fn swap_terms(
    op: &CommandOp,
    terms: &SwapTerms<'_>,
    expires_at: UtcTime,
    ctx: &EvalContext<'_>,
) -> Option<Amount> {
    let CommandOp::Swap {
        sell,
        sell_amount,
        buy,
        min_receive,
        max_fee_cc,
    } = op
    else {
        return None;
    };
    let legs = terms.sent.instrument == *sell
        && terms.sent.amount == *sell_amount
        && terms.received.instrument == *buy;
    let lp_ok = terms.lp != ctx.agent_party && !ctx.platform.contains(terms.lp);
    if !legs || !lp_ok || terms.valid_until > expires_at {
        return None;
    }
    let fee = swap_fee_total(terms.fees, ctx).filter(|fee| fee <= max_fee_cc)?;
    let net = if buy == ctx.cc {
        terms.received.amount.checked_sub(fee)?
    } else {
        terms.received.amount
    };
    (net >= *min_receive).then_some(fee)
}

/// True when a request names exactly the command's swap, checked before it is prepared
/// with the same terms as the settled content.
pub fn swap_request_matches(
    op: &CommandOp,
    request: &SwapRequest,
    expires_at: UtcTime,
    ctx: &EvalContext<'_>,
) -> bool {
    let terms = SwapTerms {
        lp: &request.lp,
        sent: &request.sent,
        received: &request.received,
        fees: &request.fees,
        valid_until: request.valid_until,
    };
    swap_terms(op, &terms, expires_at, ctx).is_some()
}

/// Non-zero sums per (recipient, instrument); `None` once a sum leaves the grammar.
#[derive(Default, PartialEq, Eq)]
struct Outflows(BTreeMap<(Recipient, InstrumentRef), Amount>);

impl Outflows {
    fn add(&mut self, to: &Recipient, instrument: &InstrumentRef, amount: Amount) -> Option<()> {
        if amount.is_zero() {
            return Some(());
        }
        let key = (to.clone(), instrument.clone());
        let sum = match self.0.get(&key) {
            Some(before) => before.checked_add(amount)?,
            None => amount,
        };
        self.0.insert(key, sum);
        Some(())
    }
}

/// The swap arm of `command_matches`; `outgoing` must come from `facts_map::swap_intent`,
/// the one place that leaves out the LP's change.
fn swap_matches(command: &Command, intent: &Intent, ctx: &EvalContext<'_>) -> bool {
    let (
        CommandOp::Swap {
            sell,
            sell_amount,
            min_receive,
            ..
        },
        Some(settle),
    ) = (&command.op, &intent.settle)
    else {
        return false;
    };
    let terms = SwapTerms {
        lp: &settle.lp,
        sent: &settle.sent,
        received: &settle.received,
        fees: &settle.fees,
        valid_until: settle.valid_until,
    };
    let Some(fee) = swap_terms(&command.op, &terms, command.expires_at, ctx) else {
        return false;
    };
    if settle.net_received < *min_receive {
        return false;
    }
    let mut expected = Outflows::default();
    let lp = Recipient::Party(settle.lp.clone());
    let sold = expected.add(&lp, sell, *sell_amount);
    let paid = match &ctx.platform.fee {
        Some(fee_party) => expected.add(&Recipient::Party(fee_party.clone()), ctx.cc, fee),
        None => Some(()),
    };
    let mut actual = Outflows::default();
    for flow in &intent.outgoing {
        if is_self(flow, ctx.agent_party) {
            continue;
        }
        let added = flow
            .to
            .as_ref()
            .and_then(|to| actual.add(to, &flow.instrument, flow.amount));
        if added.is_none() {
            return false;
        }
    }
    sold.is_some() && paid.is_some() && actual == expected
}

/// True when the verified content is exactly the command's operation, change aside; build
/// the intent with `facts_map` (`swap_intent`, `transfer_intent`), never by hand.
pub fn command_matches(command: &Command, intent: &Intent, ctx: &EvalContext<'_>) -> bool {
    let op = &command.op;
    if intent.coverage != Coverage::Fields || !intent.sub.is_empty() || intent.kind != op.kind() {
        return false;
    }
    let (instrument, to) = match op {
        CommandOp::Swap { .. } => return swap_matches(command, intent, ctx),
        _ if intent.settle.is_some() => return false,
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
    use crate::testkit::{self, Chain, DAY, DEVNET_DELAY, at};

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

    fn cc() -> InstrumentRef {
        InstrumentRef {
            admin: party("DSO", 1),
            id: InstrumentId::parse("Amulet").unwrap(),
        }
    }

    /// Sell 1000 CC for at least 150 USDCx, paying at most 2 CC in fees.
    fn sell_cc() -> CommandOp {
        CommandOp::Swap {
            sell: cc(),
            sell_amount: amount("1000"),
            buy: usdc(),
            min_receive: amount("150"),
            max_fee_cc: amount("2"),
        }
    }

    /// Sell 150 USDCx for at least 990 CC net of fees, paying at most 2 CC in fees.
    fn buy_cc() -> CommandOp {
        CommandOp::Swap {
            sell: usdc(),
            sell_amount: amount("150"),
            buy: cc(),
            min_receive: amount("990"),
            max_fee_cc: amount("2"),
        }
    }

    /// A command for `op` at a fixed head, issued at `T0` and valid for 600 s.
    fn command_of(op: CommandOp) -> Command {
        Command {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
            log_seq: 1,
            log_head: Hash32([1; 32]),
            issued_at: at(0),
            expires_at: at(600),
            op,
            summary: Text::parse("Swap 1000 CC for USDCx").unwrap(),
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
            sell_cc(),
            buy_cc(),
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
    fn command_kinds_sort_in_text_order_and_parse_strictly() {
        let names: Vec<&str> = CommandKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            ["swap", "transfer_cc", "transfer_token", "usdcx_burn"]
        );
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, names);
        assert!(CommandKind::ALL.windows(2).all(|w| w[0] < w[1]));
        for kind in CommandKind::ALL {
            assert_eq!(CommandKind::parse(kind.as_str()), Ok(kind));
            assert_eq!(kind.to_string().parse::<CommandKind>(), Ok(kind));
        }
        for bad in [
            "",
            "Swap",
            "transfer",
            "transfer_all",
            "swap ",
            "usdcx-burn",
        ] {
            let err = CommandKind::parse(bad).unwrap_err();
            assert_eq!((err.code_name(), err.code()), ("invalid_command_op", 184));
        }
        let ops = [
            (pay_alice(), CommandKind::TransferCc),
            (
                CommandOp::TransferToken {
                    receiver: party("bob", 4),
                    instrument: usdc(),
                    amount: amount("1"),
                    reference: None,
                },
                CommandKind::TransferToken,
            ),
            (
                CommandOp::UsdcxBurn {
                    amount: amount("1"),
                    destination: EthAddress([1; 20]),
                },
                CommandKind::UsdcxBurn,
            ),
            (sell_cc(), CommandKind::Swap),
        ];
        for (op, kind) in ops {
            assert_eq!(op.command_kind(), kind);
            assert_eq!(op.command_kind().as_str(), op.name());
            let class = if kind == CommandKind::Swap {
                OperationClass::Trading
            } else {
                OperationClass::Transfers
            };
            assert_eq!(op.class(), class, "{kind}");
        }
        assert_eq!(sell_cc().kind(), OperationKind::AtomicDvpSettle);
        assert_eq!(sell_cc().amount(), amount("1000"));
        assert_eq!(buy_cc().amount(), amount("150"));
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
        let now = at(DEVNET_DELAY);
        let command = command_at(state, pay_alice(), now);
        let bytes = command.encode().unwrap();
        let verified =
            verify_command(state, &bytes, &testkit::passkey_envelope(&bytes), now, None).unwrap();
        assert_eq!(verified.command(), &command);
        assert_eq!(*verified.controller_id(), testkit::passkey_id());
        assert_eq!(verified.outcome().sign_count, Some(0));
        let refuse = |command: &Command, signer: u8, when: UtcTime| {
            let bytes = command.encode().unwrap();
            let envelope = match signer {
                0 => testkit::passkey_envelope(&bytes),
                start => testkit::ed25519_envelope(start, &bytes),
            };
            verify_command(state, &bytes, &envelope, when, None)
                .unwrap_err()
                .code_name()
        };
        let later = |secs: i64| now.checked_add_secs(secs).unwrap();
        assert_eq!(
            refuse(&command, 0, at(DEVNET_DELAY - 1)),
            "signer_lacks_direct"
        );
        assert_eq!(refuse(&command, 0x21, now), "signer_lacks_direct");
        assert_eq!(refuse(&command, 0x99, now), "unknown_controller");
        assert_eq!(refuse(&command, 0, later(601)), "expired");
        let at_expiry = verify_command(
            state,
            &bytes,
            &testkit::passkey_envelope(&bytes),
            later(600),
            None,
        );
        assert!(at_expiry.is_ok());
        let early = command_at(state, pay_alice(), later(121));
        assert_eq!(refuse(&early, 0, now), "issued_in_future");
        let skewed = command_at(state, pay_alice(), later(120));
        let skewed_bytes = skewed.encode().unwrap();
        let envelope = testkit::passkey_envelope(&skewed_bytes);
        assert!(verify_command(state, &skewed_bytes, &envelope, now, None).is_ok());
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
            None,
        );
        assert_eq!(wrong_payload.unwrap_err().code_name(), "payload_mismatch");
        assert_eq!(
            verify_command(
                state,
                b"junk",
                &testkit::passkey_envelope(b"junk"),
                now,
                None
            )
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
            verify_command(&moved.state, &bytes, &envelope, now, None)
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
        let removed = DAY + DEVNET_DELAY;
        let command = command_at(state, pay_alice(), at(removed - 300));
        let bytes = command.encode().unwrap();
        let envelope = testkit::ed25519_envelope(0x21, &bytes);
        let verified = verify_command(state, &bytes, &envelope, at(removed - 1), None).unwrap();
        assert_eq!(*verified.controller_id(), testkit::ed25519_id(0x21));
        assert_eq!(verified.outcome().sign_count, None);
        let after = verify_command(state, &bytes, &envelope, at(removed), None).unwrap_err();
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
        let bad = verify_command(state, &bytes, &forged, at(removed - 1), None).unwrap_err();
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
        let command = command_of(pay_alice());
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
            assert_eq!(
                command_matches(&command, intent, &ctx),
                *expected,
                "{intent:?}"
            );
        }
        let destination = EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7").unwrap();
        let burn = command_of(CommandOp::UsdcxBurn {
            amount: amount("5"),
            destination,
        });
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

    #[test]
    fn a_swap_writes_its_lines_after_op_in_order() {
        let command = command_of(sell_cc());
        let bytes = command.encode().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let expected_tail = format!(
            "\nop=swap\nop.sell_instrument_admin={dso}\nop.sell_instrument_id=Amulet\n\
             op.sell_amount=1000\nop.buy_instrument_admin={bank}\nop.buy_instrument_id=USDCx\n\
             op.min_receive=150\nop.max_fee_cc=2\nsummary=Swap 1000 CC for USDCx\n",
            dso = party("DSO", 1),
            bank = party("Bank", 2),
        );
        assert!(text.ends_with(&expected_tail), "{text}");
        assert_eq!(Command::decode(&bytes).unwrap(), command);
        let mut feeless = command.clone();
        if let CommandOp::Swap { max_fee_cc, .. } = &mut feeless.op {
            *max_fee_cc = Amount::ZERO;
        }
        let feeless_bytes = feeless.encode().unwrap();
        let feeless_text = String::from_utf8(feeless_bytes.clone()).unwrap();
        assert!(feeless_text.contains("\nop.max_fee_cc=0\nsummary="));
        assert_eq!(Command::decode(&feeless_bytes).unwrap(), feeless);
    }

    #[test]
    fn swap_decoding_refusals_have_stable_names() {
        let good = String::from_utf8(command_of(sell_cc()).encode().unwrap()).unwrap();
        let bank = format!("op.buy_instrument_admin={}", party("Bank", 2));
        let usdc_lines = format!("{bank}\nop.buy_instrument_id=USDCx");
        let cc_lines = format!(
            "op.buy_instrument_admin={}\nop.buy_instrument_id=Amulet",
            party("DSO", 1)
        );
        let same = good.replacen(&usdc_lines, &cc_lines, 1);
        let cases: Vec<(&str, String, &str)> = vec![
            ("same instrument", same.clone(), "same_instrument"),
            (
                "zero sold",
                good.replacen("op.sell_amount=1000", "op.sell_amount=0", 1),
                "invalid_amount",
            ),
            (
                "zero minimum",
                good.replacen("op.min_receive=150", "op.min_receive=0", 1),
                "invalid_amount",
            ),
            (
                "empty fee cap",
                good.replacen("op.max_fee_cc=2", "op.max_fee_cc=", 1),
                "empty_value",
            ),
            (
                "signed fee cap",
                good.replacen("op.max_fee_cc=2", "op.max_fee_cc=-2", 1),
                "invalid_amount",
            ),
            (
                "fee cap not minimal",
                good.replacen("op.max_fee_cc=2", "op.max_fee_cc=2.0", 1),
                "invalid_amount",
            ),
            (
                "missing buy id",
                good.replacen("op.buy_instrument_id=USDCx\n", "", 1),
                "unexpected_key",
            ),
            (
                "bare admin",
                good.replacen(&bank, "op.buy_instrument_admin=Bank", 1),
                "invalid_party",
            ),
            (
                "transfer keys",
                good.replacen("op=swap", "op=transfer_cc", 1),
                "unexpected_key",
            ),
            (
                "unknown op",
                good.replacen("op=swap", "op=swaps", 1),
                "invalid_command_op",
            ),
        ];
        for (name, text, expected) in cases {
            let err = Command::decode(text.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{name}");
        }
        let err = Command::decode(same.as_bytes()).unwrap_err();
        assert_eq!((err.code(), err.line), (131, 14));
        let mut command = command_of(sell_cc());
        if let CommandOp::Swap { buy, .. } = &mut command.op {
            *buy = cc();
        }
        let err = command.encode().unwrap_err();
        assert_eq!((err.code_name(), err.line), ("same_instrument", 14));
        let mut no_sale = command_of(sell_cc());
        if let CommandOp::Swap { sell_amount, .. } = &mut no_sale.op {
            *sell_amount = Amount::ZERO;
        }
        let mut no_minimum = command_of(sell_cc());
        if let CommandOp::Swap { min_receive, .. } = &mut no_minimum.op {
            *min_receive = Amount::ZERO;
        }
        for command in [no_sale, no_minimum] {
            assert_eq!(command.encode().unwrap_err().code_name(), "invalid_amount");
        }
    }

    fn scope(
        commands: &[CommandKind],
        pairs: Vec<crate::policy::ScopePair>,
        lps: Vec<PartyId>,
    ) -> crate::policy::CommandScope {
        crate::policy::CommandScope {
            controller: testkit::passkey_id(),
            commands: commands.to_vec(),
            pairs,
            lps,
        }
    }

    /// The linked chain with the passkey's scope accepted at the delay, signed by itself.
    fn scoped_chain(scope: crate::policy::CommandScope) -> Chain {
        let mut chain = Chain::linked();
        let mut rules = crate::policy::DEFAULT_LINKED_RULES;
        rules.scopes = vec![scope];
        let doc = testkit::policy_doc(&chain.state, 1, at(DEVNET_DELAY), rules);
        chain.accept_passkey(&doc, at(DEVNET_DELAY));
        chain
    }

    #[test]
    fn verification_refuses_kinds_outside_the_signers_scope() {
        let now = at(DEVNET_DELAY);
        let verdict = |chain: &Chain, op: CommandOp| {
            let bytes = command_at(&chain.state, op, now).encode().unwrap();
            let envelope = testkit::passkey_envelope(&bytes);
            verify_command(&chain.state, &bytes, &envelope, now, Some(&cc())).map(|v| v.verified_at())
        };
        let unscoped = Chain::linked();
        assert_eq!(verdict(&unscoped, pay_alice()), Ok(now));
        assert_eq!(verdict(&unscoped, sell_cc()), Ok(now));
        let swaps_only = scoped_chain(scope(&[CommandKind::Swap], Vec::new(), Vec::new()));
        assert_eq!(verdict(&swaps_only, sell_cc()), Ok(now));
        assert_eq!(verdict(&swaps_only, buy_cc()), Ok(now));
        let refused = verdict(&swaps_only, pay_alice()).unwrap_err();
        assert_eq!(
            (refused.code_name(), refused.code()),
            ("command_out_of_scope", 1108)
        );
        let nothing = scoped_chain(scope(&[], Vec::new(), Vec::new()));
        for op in [pay_alice(), sell_cc()] {
            assert_eq!(verdict(&nothing, op), Err(CommandError::OutOfScope));
        }
        let transfers = scoped_chain(scope(&[CommandKind::TransferCc], Vec::new(), Vec::new()));
        assert_eq!(verdict(&transfers, pay_alice()), Ok(now));
        assert_eq!(
            verdict(&transfers, sell_cc()),
            Err(CommandError::OutOfScope)
        );
        // A scope never grants: a signer without `direct` is refused before it.
        let mut read_only = Chain::linked();
        let mut rules = crate::policy::DEFAULT_LINKED_RULES;
        rules.scopes = vec![crate::policy::CommandScope {
            controller: testkit::ed25519_id(0x21),
            ..scope(&CommandKind::ALL, Vec::new(), Vec::new())
        }];
        let doc = testkit::policy_doc(&read_only.state, 1, now, rules);
        read_only.accept_passkey(&doc, now);
        let bytes = command_at(&read_only.state, sell_cc(), now)
            .encode()
            .unwrap();
        let envelope = testkit::ed25519_envelope(0x21, &bytes);
        let err =
            verify_command(&read_only.state, &bytes, &envelope, now, Some(&cc())).unwrap_err();
        assert_eq!(err.code_name(), "signer_lacks_direct");
    }

    #[test]
    fn verification_applies_a_scopes_pair_bounds_to_swaps() {
        let now = at(DEVNET_DELAY);
        let lp = party("lp", 7);
        let pair = crate::policy::ScopePair {
            sell: crate::policy::PolicyInstrument::Cc,
            buy: crate::policy::PolicyInstrument::Token(usdc()),
            min_price: amount("0.15"),
            max_sell: amount("1000"),
        };
        let chain = scoped_chain(scope(
            &[CommandKind::Swap, CommandKind::TransferCc],
            vec![pair],
            vec![lp.clone()],
        ));
        let state = &chain.state;
        let swap =
            |sell: InstrumentRef, sold: &str, buy: InstrumentRef, minimum: &str| CommandOp::Swap {
                sell,
                sell_amount: amount(sold),
                buy,
                min_receive: amount(minimum),
                max_fee_cc: amount("2"),
            };
        let verdict = |op: CommandOp, cc_ref: Option<&InstrumentRef>| {
            let bytes = command_at(state, op, now).encode().unwrap();
            let envelope = testkit::passkey_envelope(&bytes);
            verify_command(state, &bytes, &envelope, now, cc_ref)
                .map(|_| ())
                .map_err(|e| (e.code_name(), e.code()))
        };
        let cc = cc();
        assert_eq!(verdict(sell_cc(), Some(&cc)), Ok(()));
        assert_eq!(
            verdict(swap(cc.clone(), "1", usdc(), "0.15"), Some(&cc)),
            Ok(())
        );
        assert_eq!(
            verdict(sell_cc(), None),
            Err(("swap_pair_not_allowed", 1109))
        );
        assert_eq!(
            verdict(buy_cc(), Some(&cc)),
            Err(("swap_pair_not_allowed", 1109))
        );
        let other_cc = InstrumentRef {
            admin: party("DSO", 9),
            id: InstrumentId::parse("Amulet").unwrap(),
        };
        assert_eq!(
            verdict(sell_cc(), Some(&other_cc)),
            Err(("swap_pair_not_allowed", 1109))
        );
        assert_eq!(
            verdict(
                swap(cc.clone(), "1000", usdc(), "149.9999999999"),
                Some(&cc)
            ),
            Err(("swap_below_min_price", 1110))
        );
        assert_eq!(
            verdict(swap(cc.clone(), "3", usdc(), "0.4499999999"), Some(&cc)),
            Err(("swap_below_min_price", 1110))
        );
        assert_eq!(
            verdict(
                swap(cc.clone(), "1000.0000000001", usdc(), "200"),
                Some(&cc)
            ),
            Err(("swap_above_max_sell", 1111))
        );
        // A transfer under the same scope ignores the pairs.
        assert_eq!(verdict(pay_alice(), None), Ok(()));
        let rules = state.rules_at(now).rules();
        let signer = testkit::passkey_id();
        assert!(swap_lp_allowed(rules, &signer, &lp));
        assert!(!swap_lp_allowed(rules, &signer, &party("lp", 8)));
        assert!(swap_lp_allowed(
            rules,
            &testkit::ed25519_id(0x21),
            &party("lp", 8)
        ));
        assert_eq!(
            check_swap_bounds(rules, &signer, &pay_alice(), None),
            Ok(())
        );
        assert_eq!(CommandError::LpNotAllowed.code(), 1112);
        // Without pairs every pair passes, with or without Canton Coin known.
        let open = scoped_chain(scope(&[CommandKind::Swap], Vec::new(), Vec::new()));
        let open_rules = open.state.rules_at(now).rules();
        assert_eq!(
            check_swap_bounds(open_rules, &signer, &buy_cc(), None),
            Ok(())
        );
        assert!(swap_lp_allowed(open_rules, &signer, &party("lp", 8)));
    }

    /// An agent, its LP, a pinned fee party and the context they make.
    struct Venue {
        agent: PartyId,
        lp: PartyId,
        fee: PartyId,
        cc: InstrumentRef,
        pinned: PlatformParties,
        none: PlatformParties,
        usage: UsageTotals,
    }

    impl Venue {
        fn new() -> Self {
            let fee = party("fee", 6);
            Self {
                agent: testkit::agent_party(),
                lp: party("lp", 7),
                pinned: PlatformParties {
                    fee: Some(fee.clone()),
                    prepaid_traffic: Some(party("prepaid", 8)),
                },
                fee,
                cc: cc(),
                none: PlatformParties::NONE,
                usage: UsageTotals::new(),
            }
        }

        fn ctx(&self) -> EvalContext<'_> {
            EvalContext {
                agent_party: &self.agent,
                cc: &self.cc,
                platform: &self.pinned,
                now: at(0),
                usage: &self.usage,
            }
        }

        fn unpinned(&self) -> EvalContext<'_> {
            EvalContext {
                platform: &self.none,
                ..self.ctx()
            }
        }

        fn flow(&self, instrument: &InstrumentRef, value: &str, to: &PartyId) -> Flow {
            Flow {
                instrument: instrument.clone(),
                amount: amount(value),
                to: Some(Recipient::Party(to.clone())),
            }
        }

        fn fee_of(&self, value: &str) -> SettleFee {
            SettleFee {
                receiver: self.fee.clone(),
                instrument: self.cc.clone(),
                amount: amount(value),
            }
        }

        /// 1000 CC for 151 USDCx with a 1.5 CC fee, 48.5 CC change back.
        fn sell_cc(&self) -> Intent {
            Intent {
                outgoing: vec![
                    self.flow(&self.cc, "1000", &self.lp),
                    self.flow(&self.cc, "1.5", &self.fee),
                    self.flow(&self.cc, "48.5", &self.agent),
                ],
                instruments: vec![self.cc.clone(), usdc()],
                settle: Some(crate::policy::eval::SettleTerms {
                    lp: self.lp.clone(),
                    sent: Leg {
                        instrument: self.cc.clone(),
                        amount: amount("1000"),
                    },
                    received: Leg {
                        instrument: usdc(),
                        amount: amount("151"),
                    },
                    net_received: amount("151"),
                    fees: vec![self.fee_of("1.5")],
                    valid_until: at(300),
                }),
                ..Intent::new(OperationKind::AtomicDvpSettle)
            }
        }

        /// 150 USDCx for 1000 CC with a 1.5 CC fee: 998.5 CC net.
        fn buy_cc(&self) -> Intent {
            Intent {
                outgoing: vec![
                    self.flow(&usdc(), "150", &self.lp),
                    self.flow(&self.cc, "1.5", &self.fee),
                ],
                instruments: vec![usdc(), self.cc.clone()],
                settle: Some(crate::policy::eval::SettleTerms {
                    lp: self.lp.clone(),
                    sent: Leg {
                        instrument: usdc(),
                        amount: amount("150"),
                    },
                    received: Leg {
                        instrument: self.cc.clone(),
                        amount: amount("1000"),
                    },
                    net_received: amount("998.5"),
                    fees: vec![self.fee_of("1.5")],
                    valid_until: at(600),
                }),
                ..Intent::new(OperationKind::AtomicDvpSettle)
            }
        }
    }

    fn with_settle(
        intent: &Intent,
        edit: impl FnOnce(&mut crate::policy::eval::SettleTerms),
    ) -> Intent {
        let mut intent = intent.clone();
        if let Some(settle) = intent.settle.as_mut() {
            edit(settle);
        }
        intent
    }

    fn with_min(op: CommandOp, value: &str) -> CommandOp {
        match op {
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                max_fee_cc,
                ..
            } => CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive: amount(value),
                max_fee_cc,
            },
            other => other,
        }
    }

    #[test]
    fn a_swap_matches_its_exact_settle_in_both_orientations() {
        let v = Venue::new();
        let ctx = v.ctx();
        let selling = command_of(sell_cc());
        let buying = command_of(buy_cc());
        assert!(command_matches(&selling, &v.sell_cc(), &ctx));
        assert!(command_matches(&buying, &v.buy_cc(), &ctx));
        assert!(!command_matches(&selling, &v.buy_cc(), &ctx));
        assert!(!command_matches(&buying, &v.sell_cc(), &ctx));
        // Buying CC, the minimum is net of the fee: 1000 - 1.5 = 998.5.
        let at_net = command_of(with_min(buy_cc(), "998.5"));
        assert!(command_matches(&at_net, &v.buy_cc(), &ctx));
        let above_net = command_of(with_min(buy_cc(), "998.5000000001"));
        assert!(!command_matches(&above_net, &v.buy_cc(), &ctx));
        // Selling CC, the fee comes from the sold side, so the whole 151 counts.
        let at_received = command_of(with_min(sell_cc(), "151"));
        assert!(command_matches(&at_received, &v.sell_cc(), &ctx));
        let above_received = command_of(with_min(sell_cc(), "151.0000000001"));
        assert!(!command_matches(&above_received, &v.sell_cc(), &ctx));
        // The net received the content shows must meet the minimum as well.
        let short = with_settle(&v.buy_cc(), |s| s.net_received = amount("998.4999999999"));
        assert!(!command_matches(&at_net, &short, &ctx));
        assert!(command_matches(&command_of(buy_cc()), &short, &ctx));
        // A fee-less quote under a zero cap, with no fee party pinned.
        let feeless = |intent: &Intent| {
            let mut intent = with_settle(intent, |s| s.fees.clear());
            intent
                .outgoing
                .retain(|f| f.to != Some(Recipient::Party(v.fee.clone())));
            intent
        };
        let zero_cap = |op: CommandOp| match op {
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                ..
            } => command_of(CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                max_fee_cc: Amount::ZERO,
            }),
            other => command_of(other),
        };
        assert!(command_matches(
            &zero_cap(sell_cc()),
            &feeless(&v.sell_cc()),
            &ctx
        ));
        assert!(command_matches(
            &zero_cap(sell_cc()),
            &feeless(&v.sell_cc()),
            &v.unpinned()
        ));
        assert!(!command_matches(&zero_cap(sell_cc()), &v.sell_cc(), &ctx));
        assert!(!command_matches(&selling, &v.sell_cc(), &v.unpinned()));
    }

    #[test]
    fn a_swap_refuses_any_other_settle() {
        let v = Venue::new();
        let ctx = v.ctx();
        let selling = command_of(sell_cc());
        let base = v.sell_cc();
        let other = party("other", 9);
        let cases: Vec<(&str, Intent)> = vec![
            (
                "sold one unit too many",
                with_settle(&base, |s| s.sent.amount = amount("1000.0000000001")),
            ),
            (
                "sold leg in flows above the terms",
                Intent {
                    outgoing: vec![
                        v.flow(&v.cc, "1000.0000000001", &v.lp),
                        v.flow(&v.cc, "1.5", &v.fee),
                    ],
                    ..base.clone()
                },
            ),
            (
                "another sold instrument",
                with_settle(&base, |s| s.sent.instrument = usdc()),
            ),
            (
                "another bought instrument",
                with_settle(&base, |s| s.received.instrument = v.cc.clone()),
            ),
            (
                "received below the minimum",
                with_settle(&base, |s| s.received.amount = amount("149.9999999999")),
            ),
            (
                "an extra outflow",
                Intent {
                    outgoing: [base.outgoing.clone(), vec![v.flow(&usdc(), "1", &other)]].concat(),
                    ..base.clone()
                },
            ),
            (
                "LP change in the outflows",
                Intent {
                    outgoing: [base.outgoing.clone(), vec![v.flow(&usdc(), "849", &v.lp)]].concat(),
                    ..base.clone()
                },
            ),
            (
                "the sold leg to another party",
                Intent {
                    outgoing: vec![v.flow(&v.cc, "1000", &other), v.flow(&v.cc, "1.5", &v.fee)],
                    ..base.clone()
                },
            ),
            (
                "an unknown recipient",
                Intent {
                    outgoing: [
                        base.outgoing.clone(),
                        vec![Flow {
                            to: None,
                            ..v.flow(&v.cc, "1", &other)
                        }],
                    ]
                    .concat(),
                    ..base.clone()
                },
            ),
            (
                "a fee to the LP",
                with_settle(&base, |s| {
                    s.fees = vec![SettleFee {
                        receiver: v.lp.clone(),
                        ..v.fee_of("1.5")
                    }]
                }),
            ),
            (
                "a fee in another instrument",
                with_settle(&base, |s| {
                    s.fees = vec![SettleFee {
                        instrument: usdc(),
                        ..v.fee_of("1.5")
                    }]
                }),
            ),
            (
                "fees above the cap",
                with_settle(&base, |s| {
                    s.fees = vec![v.fee_of("1.5"), v.fee_of("0.5000000001")]
                }),
            ),
            (
                "fee flows that differ from the fee terms",
                with_settle(&base, |s| s.fees = vec![v.fee_of("1")]),
            ),
            (
                "a quote that outlives the command",
                with_settle(&base, |s| s.valid_until = at(601)),
            ),
            (
                "the agent as its own LP",
                with_settle(&base, |s| s.lp = v.agent.clone()),
            ),
            (
                "no settle terms",
                Intent {
                    settle: None,
                    ..base.clone()
                },
            ),
            (
                "unverified content",
                Intent {
                    coverage: Coverage::Unverified,
                    ..base.clone()
                },
            ),
            (
                "another kind",
                Intent {
                    kind: OperationKind::AcceptDvp,
                    ..base.clone()
                },
            ),
            (
                "sub-operations",
                Intent {
                    sub: vec![Intent::new(OperationKind::McAllocate)],
                    ..base.clone()
                },
            ),
        ];
        for (name, intent) in &cases {
            assert!(!command_matches(&selling, intent, &ctx), "{name}");
        }
        // Zero flows move nothing, and the fee may sit at the cap exactly.
        let zero = Intent {
            outgoing: [base.outgoing.clone(), vec![v.flow(&usdc(), "0", &other)]].concat(),
            ..base.clone()
        };
        assert!(command_matches(&selling, &zero, &ctx));
        let at_cap = Intent {
            outgoing: vec![
                v.flow(&v.cc, "1000", &v.lp),
                v.flow(&v.cc, "1.5", &v.fee),
                v.flow(&v.cc, "0.5", &v.fee),
            ],
            ..with_settle(&base, |s| s.fees = vec![v.fee_of("1.5"), v.fee_of("0.5")])
        };
        assert!(command_matches(&selling, &at_cap, &ctx));
        assert!(command_matches(
            &selling,
            &with_settle(&base, |s| s.valid_until = at(600)),
            &ctx
        ));
        // A transfer never carries settle terms.
        let transfer = command_of(pay_alice());
        let paid = Intent {
            outgoing: vec![v.flow(&v.cc, "10", &party("alice", 3))],
            ..Intent::new(OperationKind::TransferCc)
        };
        assert!(command_matches(&transfer, &paid, &ctx));
        let with_terms = Intent {
            settle: base.settle.clone(),
            ..paid
        };
        assert!(!command_matches(&transfer, &with_terms, &ctx));
    }

    #[test]
    fn a_swap_request_is_checked_before_prepare() {
        let v = Venue::new();
        let ctx = v.ctx();
        let expires_at = at(600);
        let request = |sent: Leg, received: Leg, fees: Vec<SettleFee>| SwapRequest {
            lp: v.lp.clone(),
            sent,
            received,
            fees,
            valid_until: at(300),
        };
        let leg = |instrument: InstrumentRef, value: &str| Leg {
            instrument,
            amount: amount(value),
        };
        let selling = request(
            leg(v.cc.clone(), "1000"),
            leg(usdc(), "150"),
            vec![v.fee_of("2")],
        );
        let buying = request(
            leg(usdc(), "150"),
            leg(v.cc.clone(), "992"),
            vec![v.fee_of("2")],
        );
        assert!(swap_request_matches(&sell_cc(), &selling, expires_at, &ctx));
        assert!(swap_request_matches(&buy_cc(), &buying, expires_at, &ctx));
        assert!(!swap_request_matches(&buy_cc(), &selling, expires_at, &ctx));
        let refused: Vec<(&str, CommandOp, SwapRequest)> = vec![
            (
                "net of the fee below the minimum",
                buy_cc(),
                SwapRequest {
                    received: leg(v.cc.clone(), "991.9999999999"),
                    ..buying.clone()
                },
            ),
            (
                "fee over the cap",
                sell_cc(),
                SwapRequest {
                    fees: vec![v.fee_of("2.0000000001")],
                    ..selling.clone()
                },
            ),
            (
                "fee to another party",
                sell_cc(),
                SwapRequest {
                    fees: vec![SettleFee {
                        receiver: v.lp.clone(),
                        ..v.fee_of("1")
                    }],
                    ..selling.clone()
                },
            ),
            (
                "inexact size",
                sell_cc(),
                SwapRequest {
                    sent: leg(v.cc.clone(), "999.9999999999"),
                    ..selling.clone()
                },
            ),
            (
                "late quote",
                sell_cc(),
                SwapRequest {
                    valid_until: at(601),
                    ..selling.clone()
                },
            ),
            (
                "the fee party as the LP",
                sell_cc(),
                SwapRequest {
                    lp: v.fee.clone(),
                    ..selling.clone()
                },
            ),
            (
                "the prepaid traffic party as the LP",
                sell_cc(),
                SwapRequest {
                    lp: v.pinned.prepaid_traffic.clone().unwrap(),
                    ..selling.clone()
                },
            ),
            ("not a swap", pay_alice(), selling.clone()),
        ];
        for (name, op, request) in &refused {
            assert!(
                !swap_request_matches(op, request, expires_at, &ctx),
                "{name}"
            );
        }
        assert!(!swap_request_matches(
            &sell_cc(),
            &selling,
            expires_at,
            &v.unpinned()
        ));
        let feeless = SwapRequest {
            fees: Vec::new(),
            ..selling.clone()
        };
        assert!(swap_request_matches(
            &sell_cc(),
            &feeless,
            expires_at,
            &v.unpinned()
        ));
        // Without a fee all of 991.9999999999 CC is net, which meets 990.
        let barely = SwapRequest {
            received: leg(v.cc.clone(), "991.9999999999"),
            fees: Vec::new(),
            ..buying
        };
        assert!(swap_request_matches(&buy_cc(), &barely, expires_at, &ctx));
    }
}
