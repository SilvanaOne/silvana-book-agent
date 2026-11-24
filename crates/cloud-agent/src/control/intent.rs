//! The content check of a linked agent before every transaction signature: a local
//! hash, the agent alone submitting, modelled templates, and only its own legs.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::BTreeSet;
use std::fmt;

use agent_control::binding::{CC_INSTRUMENT_ID as AMULET, SETTLE_CHOICE, VENUE_TEMPLATE};
use agent_control::{
    BindingRefusal, Command, CommandError, ControlState, Coverage, DirectedAuthorization, DirectedTx, EvalContext,
    PackageId, PartyId, ProofEnvelope, ReasonCode, UtcTime, authorize_directed_swap, authorize_directed_transfer,
    check_command_id, check_packages,
};
use orderbook_proto::ledger::multi_call_op::Op as McOp;
use orderbook_proto::ledger::prepare_transaction_request::Params;
use orderbook_proto::rfqv2::prepare_atomic_transaction_request::Params as AtomicParams;
use rust_decimal::Decimal;
use tx_verifier::facts::{DvpTerms, FlowKind, QuoteSide, TxFacts, ValueFlow};
use tx_verifier::{ExpectedDvpTerms, HashCheck, OperationExpectation, VerificationResult};

/// Platform parties pinned in the agent's own env, never taken from server data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformParties {
    pub fee: Option<String>,
    pub prepaid_traffic: Option<String>,
}

impl PlatformParties {
    pub fn from_config(config: &agent_logic::config::BaseConfig) -> Self {
        Self {
            fee: config.party_fee.clone(),
            prepaid_traffic: config.party_prepaid_traffic.clone(),
        }
    }

    /// The pins as the evaluator takes them; a pin that is not a party id is refused.
    pub fn pinned(&self) -> Result<agent_control::PlatformParties, String> {
        let parse = |pin: &Option<String>, name: &str| {
            pin.as_deref()
                .map(|p| PartyId::parse(p).map_err(|_| format!("{name}={p} is not a party id control accepts")))
                .transpose()
        };
        Ok(agent_control::PlatformParties {
            fee: parse(&self.fee, "PARTY_FEE")?,
            prepaid_traffic: parse(&self.prepaid_traffic, "PARTY_PREPAID_TRAFFIC")?,
        })
    }
}

/// The typed request the agent itself built.
#[derive(Clone, Copy, Debug)]
pub enum OwnRequest<'a> {
    Ledger(Option<&'a Params>),
    Atomic(Option<&'a AtomicParams>),
}

/// Why a transaction was refused before signing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentRefusal {
    HashNotMatched,
    Unreadable(String),
    ActAs(Vec<String>),
    Unmodelled(Vec<String>),
    BadRequest(String),
    UnexpectedOutflow { operation: &'static str, flow: ValueFlow },
    /// Nothing else is wrong, but a leg goes to a platform party whose pin is not set.
    PinMissing { operation: &'static str, pin: &'static str },
}

/// The pins a request may need, by their env names.
const FEE_PIN: &str = "PARTY_FEE";
const PREPAID_TRAFFIC_PIN: &str = "PARTY_PREPAID_TRAFFIC";

/// `text` with control, line separator and bidi characters replaced by spaces.
fn clean(text: &str) -> String {
    text.chars().map(|c| if crate::log_text::is_log_unsafe(c) { ' ' } else { c }).collect()
}

impl fmt::Display for ContentRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("content check refused before signing: ")?;
        match self {
            Self::HashNotMatched => f.write_str("the transaction hash was not computed locally"),
            Self::Unreadable(reason) => write!(f, "the transaction content is unreadable: {}", clean(reason)),
            Self::ActAs(parties) => write!(f, "act_as is {parties:?}, not exactly this agent"),
            Self::Unmodelled(templates) => {
                write!(f, "templates outside the model: {}", templates.join(", "))
            }
            Self::BadRequest(reason) => write!(f, "{reason}"),
            Self::UnexpectedOutflow { operation, flow } => write!(
                f,
                "{} {}@{} ({:?}) to {} is not a leg of this {operation} request",
                flow.amount, flow.instrument_id, flow.instrument_admin, flow.kind, flow.to
            ),
            Self::PinMissing { operation, pin } => {
                write!(f, "{pin} is not set, so the platform leg of this {operation} request is unchecked")
            }
        }
    }
}

impl std::error::Error for ContentRefusal {}

/// Who may receive a leg.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Recipient {
    Party(String),
    /// The holders of a lock the agent asked for; not known locally.
    LockHolders,
    /// A platform party whose pin is not set: any receiver, and the check then refuses.
    Unpinned,
}

/// One value movement the agent's request explains.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leg {
    to: Recipient,
    instrument_admin: Option<String>,
    instrument_id: Option<String>,
    /// `None` only for counterparty change and platform fees, which absorb any number of flows.
    amount: Option<Decimal>,
    kinds: &'static [FlowKind],
}

const TRANSFER_KINDS: &[FlowKind] = &[FlowKind::Holding, FlowKind::PendingTransfer];
const HOLDING_KINDS: &[FlowKind] = &[FlowKind::Holding];
const ALLOCATION_KINDS: &[FlowKind] = &[FlowKind::Allocation];
const LOCK_KINDS: &[FlowKind] = &[FlowKind::Lock];
const BURN_KINDS: &[FlowKind] = &[FlowKind::Burn];
const ANY_KIND: &[FlowKind] = &[
    FlowKind::Holding,
    FlowKind::PendingTransfer,
    FlowKind::Allocation,
    FlowKind::Lock,
    FlowKind::Burn,
];

impl Leg {
    fn exact(to: &str, admin: Option<&str>, id: Option<&str>, amount: Decimal, kinds: &'static [FlowKind]) -> Self {
        Self {
            to: Recipient::Party(to.to_string()),
            instrument_admin: admin.filter(|a| !a.is_empty()).map(str::to_string),
            instrument_id: id.filter(|i| !i.is_empty()).map(str::to_string),
            amount: Some(amount),
            kinds,
        }
    }

    fn matches(&self, flow: &ValueFlow) -> bool {
        let to = match &self.to {
            Recipient::Party(party) => flow.to == *party,
            Recipient::LockHolders => flow.kind == FlowKind::Lock,
            Recipient::Unpinned => true,
        };
        to && self.kinds.contains(&flow.kind)
            && self.instrument_admin.as_ref().is_none_or(|a| *a == flow.instrument_admin)
            && self.instrument_id.as_ref().is_none_or(|i| *i == flow.instrument_id)
            && self.amount.is_none_or(|a| a == flow.amount)
    }
}

/// An exact, non-negative decimal from the agent's own request.
fn amount(text: &str, what: &str) -> Result<Decimal, ContentRefusal> {
    let bad = || ContentRefusal::BadRequest(format!("{what} {text:?} is not an exact decimal"));
    let (whole, fraction) = match text.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (text, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|f| !digits(f)) {
        return Err(bad());
    }
    Decimal::from_str_exact(text).map_err(|_| bad())
}

/// The agent's own side of a DvP: the seller delivers, the buyer pays.
fn dvp_leg(agent: &str, terms: &DvpTerms) -> Option<Leg> {
    let (to, quantity) = if terms.seller == agent {
        (&terms.buyer, &terms.delivery)
    } else if terms.buyer == agent {
        (&terms.seller, &terms.payment)
    } else {
        return None;
    };
    Some(Leg::exact(
        to,
        Some(&quantity.instrument_admin),
        Some(&quantity.instrument_id),
        quantity.amount,
        ALLOCATION_KINDS,
    ))
}

/// One CC payment leg per receiver, its targets summed in first-seen order, as the
/// batch payment pays them.
fn cc_payment_legs<'t>(targets: impl IntoIterator<Item = (&'t str, &'t str)>) -> Result<Vec<Leg>, ContentRefusal> {
    let mut sums: Vec<(&str, Decimal)> = Vec::new();
    for (receiver, text) in targets {
        let paid = amount(text, "a payment amount")?;
        match sums.iter_mut().find(|(r, _)| *r == receiver) {
            Some((_, sum)) => {
                *sum = sum
                    .checked_add(paid)
                    .ok_or_else(|| ContentRefusal::BadRequest(format!("the payments to {receiver} overflow")))?;
            }
            None => sums.push((receiver, paid)),
        }
    }
    Ok(sums.into_iter().map(|(to, sum)| Leg::exact(to, None, Some(AMULET), sum, HOLDING_KINDS)).collect())
}

/// The longest a settlement the agent allocates to may stay open from now: 72 h.
const MAX_SETTLE_WINDOW_MICROS: i64 = 259_200_000_000;

/// Why an allocation's deadlines are refused at `now_micros`, if they are.
fn deadline_refusal(allocation: &tx_verifier::facts::AllocationFact, now_micros: i64) -> Option<String> {
    let settle_before = allocation.settle_before_micros;
    if allocation.allocate_before_micros > settle_before {
        return Some("its allocation deadline is after its settlement deadline".to_string());
    }
    let window = settle_before.checked_sub(now_micros);
    window
        .is_none_or(|w| w > MAX_SETTLE_WINDOW_MICROS)
        .then(|| "its settlement deadline is more than 72 h away".to_string())
}

/// The leg of an Allocate: the agent's side of the terms it recorded, explained only by
/// the transaction's allocation for that proposal.
fn allocate_legs(
    agent: &str,
    facts: &TxFacts,
    proposal_id: &str,
    terms: Option<&ExpectedDvpTerms>,
) -> Result<Vec<Leg>, ContentRefusal> {
    let Some(terms) = terms else {
        return Err(ContentRefusal::BadRequest(format!(
            "the allocation for proposal {proposal_id} carries no recorded DvP terms"
        )));
    };
    if terms.proposal_id != proposal_id {
        return Err(ContentRefusal::BadRequest(format!(
            "the recorded terms are of proposal {}, not {proposal_id}",
            terms.proposal_id
        )));
    }
    let (to, quantity) = if terms.seller == agent {
        (&terms.buyer, &terms.delivery)
    } else if terms.buyer == agent {
        (&terms.seller, &terms.payment)
    } else {
        return Err(ContentRefusal::BadRequest(format!(
            "the agent is neither buyer nor seller of proposal {proposal_id}"
        )));
    };
    let leg = Leg::exact(
        to,
        Some(&quantity.instrument_admin),
        Some(&quantity.instrument_id),
        amount(&quantity.amount, "the allocation amount")?,
        ALLOCATION_KINDS,
    );
    let now = agent_logic::clock::now_utc().timestamp_micros();
    let own = facts.allocations.iter().filter(|a| a.settlement_ref_id == proposal_id && a.sender == agent);
    if let Some(why) = own.filter_map(|a| deadline_refusal(a, now)).next() {
        return Err(ContentRefusal::BadRequest(format!("the allocation for proposal {proposal_id} is refused: {why}")));
    }
    let bound = facts.allocations.iter().any(|a| {
        a.settlement_ref_id == proposal_id
            && a.sender == agent
            && leg.matches(&ValueFlow {
                to: a.receiver.clone(),
                instrument_admin: a.instrument_admin.clone(),
                instrument_id: a.instrument_id.clone(),
                amount: a.amount,
                kind: FlowKind::Allocation,
            })
    });
    Ok(if bound { vec![leg] } else { Vec::new() })
}

/// The legs of an AcceptDvp: the agent's side, to the counterparty of matching terms.
fn accept_dvp_legs(agent: &str, facts: &TxFacts, e: &OperationExpectation) -> Result<Vec<Leg>, ContentRefusal> {
    let OperationExpectation::AcceptDvp {
        acceptor,
        proposal_id,
        expected_delivery_amount,
        expected_payment_amount,
        expected_delivery_instrument_id,
        expected_delivery_instrument_admin,
        expected_payment_instrument_id,
        expected_payment_instrument_admin,
        terms,
        ..
    } = e
    else {
        return Ok(Vec::new());
    };
    let delivery = amount(expected_delivery_amount, "the delivery amount")?;
    let payment = amount(expected_payment_amount, "the payment amount")?;
    let agreed = |t: &&DvpTerms| {
        t.proposal_id.as_deref() == Some(proposal_id.as_str())
            && (t.buyer == *acceptor || t.seller == *acceptor)
            && terms.as_ref().is_none_or(|r| t.buyer == r.buyer && t.seller == r.seller)
            && t.delivery.amount == delivery
            && t.delivery.instrument_id == *expected_delivery_instrument_id
            && t.delivery.instrument_admin == *expected_delivery_instrument_admin
            && t.payment.amount == payment
            && t.payment.instrument_id == *expected_payment_instrument_id
            && t.payment.instrument_admin == *expected_payment_instrument_admin
    };
    let mut legs: Vec<Leg> = facts.dvp.iter().filter(agreed).filter_map(|t| dvp_leg(agent, t)).collect();
    legs.dedup();
    Ok(legs)
}

/// The legs of an atomic settle the agent takes; LP change in the LP's own instrument
/// is allowed only when the settle is the one root and its terms are the request's.
fn settle_legs(
    agent: &str,
    facts: &TxFacts,
    e: &OperationExpectation,
    request: OwnRequest<'_>,
    platform: &PlatformParties,
) -> Result<(Vec<Leg>, Option<&'static str>), ContentRefusal> {
    let OperationExpectation::AtomicDvpSettle {
        user_party,
        venue_cid,
        quote_id,
        side,
        base_amount,
        quote_amount,
        lp_party,
        base_instrument_id,
        base_instrument_admin,
        quote_instrument_id,
        quote_instrument_admin,
        user_input_holding_cids,
        ..
    } = e
    else {
        return Ok((Vec::new(), None));
    };
    if user_party != agent {
        return Ok((Vec::new(), None));
    }
    let base = amount(base_amount, "the base amount")?;
    let quote = amount(quote_amount, "the quote amount")?;
    let buy = match side.as_str() {
        "Buy" => true,
        "Sell" => false,
        other => return Err(ContentRefusal::BadRequest(format!("unknown quote side {other:?}"))),
    };
    let base_leg = |amount| Leg::exact(lp_party, Some(base_instrument_admin), Some(base_instrument_id), amount, TRANSFER_KINDS);
    let quote_leg = |amount| Leg::exact(lp_party, Some(quote_instrument_admin), Some(quote_instrument_id), amount, TRANSFER_KINDS);
    let (own, lp_change) = if buy {
        (quote_leg(quote), base_leg(Decimal::ZERO))
    } else {
        (base_leg(base), quote_leg(Decimal::ZERO))
    };
    let (pays, gets) = if buy {
        ((quote_instrument_admin, quote_instrument_id, quote), (base_instrument_admin, base_instrument_id, base))
    } else {
        ((base_instrument_admin, base_instrument_id, base), (quote_instrument_admin, quote_instrument_id, quote))
    };
    // Every fee a settle makes the agent pay goes to the pinned fee party, or it is refused;
    // without the pin it is refused for that alone.
    let unpinned = |receiver: &str| platform.fee.as_deref() != Some(receiver);
    let own_fees = facts.atomic_settle.as_ref().filter(|s| s.user == agent).map(|s| s.fees.as_slice()).unwrap_or_default();
    let mut missing = (platform.fee.is_none() && !own_fees.is_empty()).then_some(FEE_PIN);
    if let Some(fee) = own_fees.iter().filter(|_| platform.fee.is_some()).find(|f| unpinned(&f.receiver)) {
        return Err(ContentRefusal::BadRequest(format!(
            "a settle fee of {} {}@{} goes to {}, not to the pinned PARTY_FEE",
            fee.amount, fee.id, fee.admin, fee.receiver
        )));
    }
    let mut legs = vec![own];
    let mut fees = Vec::new();
    if let OwnRequest::Atomic(Some(AtomicParams::AtomicDvpSettle(params))) = request {
        for fee in &params.lp_fees {
            if platform.fee.is_none() {
                missing = Some(FEE_PIN);
            } else if unpinned(&fee.receiver) {
                return Err(ContentRefusal::BadRequest(format!(
                    "the request pays a settle fee to {}, not to the pinned PARTY_FEE",
                    fee.receiver
                )));
            }
            let fee_amount = amount(&fee.amount, "an LP fee amount")?;
            fees.push((&fee.instrument_admin, &fee.instrument_id, fee_amount));
            legs.push(Leg::exact(
                &fee.receiver,
                Some(&fee.instrument_admin),
                Some(&fee.instrument_id),
                fee_amount,
                TRANSFER_KINDS,
            ));
        }
    }
    // Per instrument, the agent's consumed holdings less the unlocked ones it gets are exactly
    // its leg and the signed fees less its proceeds, so LP change never absorbs its own inputs.
    use std::collections::BTreeMap;
    fn tally<'a>(
        totals: &mut BTreeMap<(&'a str, &'a str), Decimal>,
        instrument: (&'a str, &'a str),
        amount: Decimal,
        add: bool,
    ) -> Result<(), ContentRefusal> {
        let total = totals.entry(instrument).or_insert(Decimal::ZERO);
        let next = if add { total.checked_add(amount) } else { total.checked_sub(amount) };
        *total = next.ok_or_else(|| ContentRefusal::BadRequest("the agent's holdings overflow a decimal".to_string()))?;
        Ok(())
    }
    let mut lost = BTreeMap::new();
    let mut owed = BTreeMap::new();
    for h in &facts.consumed {
        tally(&mut lost, (&h.instrument_admin, &h.instrument_id), h.amount, true)?;
    }
    for h in facts.inflows.iter().filter(|h| !h.locked) {
        tally(&mut lost, (&h.instrument_admin, &h.instrument_id), h.amount, false)?;
    }
    tally(&mut owed, (pays.0, pays.1), pays.2, true)?;
    tally(&mut owed, (gets.0, gets.1), gets.2, false)?;
    for &(admin, id, fee) in &fees {
        tally(&mut owed, (admin, id), fee, true)?;
    }
    for instrument in owed.keys() {
        lost.entry(*instrument).or_insert(Decimal::ZERO);
    }
    let unbalanced = lost.iter().find(|(instrument, have)| owed.get(*instrument).copied().unwrap_or(Decimal::ZERO) != **have);
    if let Some(((admin, id), have)) = unbalanced {
        let want = owed.get(&(*admin, *id)).copied().unwrap_or(Decimal::ZERO);
        return Err(ContentRefusal::BadRequest(format!(
            "the agent's {}@{} holdings drop by {have}, not by its leg and fees less its proceeds, {want}",
            clean(id),
            clean(admin)
        )));
    }
    let settle_is_the_request = facts.roots.len() == 1
        && facts.roots.first().is_some_and(|r| {
            r.choice == SETTLE_CHOICE && r.template == VENUE_TEMPLATE && r.contract_id == *venue_cid
        })
        && facts.atomic_settle.as_ref().is_some_and(|s| {
            let mut have = s.user_input_holding_cids.clone();
            let mut want = user_input_holding_cids.clone();
            have.sort();
            want.sort();
            s.quote_id == *quote_id
                && s.user == agent
                && s.base_amount == base
                && s.quote_amount == quote
                && s.side == if buy { QuoteSide::Buy } else { QuoteSide::Sell }
                && have == want
        });
    if settle_is_the_request {
        legs.push(Leg { amount: None, ..lp_change });
    }
    Ok((legs, missing))
}

/// The legs of the agent's own multicall operations; an accept or allocation pays only
/// the leg of the DvP terms the agent recorded.
fn multicall_legs(
    agent: &str,
    facts: &TxFacts,
    params: &Params,
    terms: Option<&ExpectedDvpTerms>,
) -> Result<Vec<Leg>, ContentRefusal> {
    let Params::ExecuteMulticall(params) = params else {
        return Ok(Vec::new());
    };
    let mut legs = Vec::new();
    for op in params.operations.iter().filter_map(|o| o.op.as_ref()) {
        match op {
            McOp::BatchTransfer(batch) => {
                for t in &batch.targets {
                    legs.push(Leg::exact(
                        &t.receiver,
                        Some(&batch.instrument_admin),
                        Some(&batch.instrument_id),
                        amount(&t.amount, "a batch transfer amount")?,
                        TRANSFER_KINDS,
                    ));
                }
            }
            McOp::Transfer(t) => {
                if let Some(d) = t.transfer.as_ref().filter(|d| d.sender == agent) {
                    legs.push(Leg::exact(
                        &d.receiver,
                        Some(&d.instrument_admin),
                        Some(&d.instrument_id),
                        amount(&d.amount, "a transfer amount")?,
                        TRANSFER_KINDS,
                    ));
                }
            }
            McOp::BatchPay(pay) => {
                legs.extend(cc_payment_legs(pay.targets.iter().map(|t| (t.receiver.as_str(), t.amount.as_str())))?);
            }
            McOp::Allocate(a) => legs.extend(allocate_legs(agent, facts, &a.proposal_id, terms)?),
            McOp::AcceptDvp(a) => legs.extend(allocate_legs(agent, facts, &a.proposal_id, terms)?),
            McOp::AcceptDvpAndAllocate(a) => legs.extend(allocate_legs(agent, facts, &a.proposal_id, terms)?),
            McOp::AcceptTransferInstruction(_)
            | McOp::WithdrawAllocation(_)
            | McOp::ProposeDvp(_)
            | McOp::CreateTransferPreapproval(_)
            | McOp::CreateUserServiceRequest(_) => {}
        }
    }
    Ok(legs)
}

/// Everything the request explains, by operation; anything else is refused.
fn expected_legs(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    facts: &TxFacts,
) -> Result<(Vec<Leg>, Option<&'static str>), ContentRefusal> {
    use OperationExpectation as E;
    let ledger_params = match request {
        OwnRequest::Ledger(params) => params,
        OwnRequest::Atomic(_) => None,
    };
    let mut missing = None;
    let legs = match expectation {
        E::TransferCc { receiver_party, amount: a, .. } => {
            vec![Leg::exact(receiver_party, None, Some(AMULET), amount(a, "the transfer amount")?, TRANSFER_KINDS)]
        }
        E::TransferCip56 { sender_party, receiver_party, instrument_id, instrument_admin, amount: a, .. } => {
            if receiver_party == sender_party {
                Vec::new()
            } else {
                vec![Leg::exact(
                    receiver_party,
                    Some(instrument_admin),
                    Some(instrument_id),
                    amount(a, "the transfer amount")?,
                    TRANSFER_KINDS,
                )]
            }
        }
        E::PrepayTraffic { amount: a, .. } => {
            let leg = Leg::exact("", None, Some(AMULET), amount(a, "the traffic amount")?, TRANSFER_KINDS);
            let to = match &platform.prepaid_traffic {
                Some(party) => Recipient::Party(party.clone()),
                None => {
                    missing = Some(PREPAID_TRAFFIC_PIN);
                    Recipient::Unpinned
                }
            };
            vec![Leg { to, ..leg }]
        }
        E::PayFee { .. } => {
            let to = match &platform.fee {
                Some(party) => Recipient::Party(party.clone()),
                None => {
                    missing = Some(FEE_PIN);
                    Recipient::Unpinned
                }
            };
            vec![Leg { to, instrument_admin: None, instrument_id: None, amount: None, kinds: ANY_KIND }]
        }
        E::AcceptDvp { .. } => accept_dvp_legs(agent, facts, expectation)?,
        E::Allocate { proposal_id, terms, .. } => allocate_legs(agent, facts, proposal_id, terms.as_ref())?,
        E::ExecuteMulticall { terms, .. } => match ledger_params {
            Some(params) => multicall_legs(agent, facts, params, terms.as_ref())?,
            None => Vec::new(),
        },
        E::BatchPayCc { targets, .. } => {
            cc_payment_legs(targets.iter().map(|t| (t.receiver.as_str(), t.amount.as_str())))?
        }
        E::AtomicDvpSettle { .. } => {
            let (legs, pin) = settle_legs(agent, facts, expectation, request, platform)?;
            missing = pin;
            legs
        }
        E::RequestRecurringPrepaid { app_party, amount: a, .. } => {
            let mut legs = vec![Leg::exact(app_party, None, None, amount(a, "the payment amount")?, TRANSFER_KINDS)];
            if let Some(Params::RequestRecurringPrepaid(p)) = ledger_params {
                legs.push(Leg {
                    to: Recipient::LockHolders,
                    ..Leg::exact(app_party, None, None, amount(&p.locked_amount, "the locked amount")?, LOCK_KINDS)
                });
            }
            legs
        }
        E::RequestRecurringPayasyougo { app_party, amount: a, .. } => {
            vec![Leg::exact(app_party, None, None, amount(a, "the payment amount")?, TRANSFER_KINDS)]
        }
        E::LockHoldings { amount: a, instrument_id, .. } => vec![Leg {
            to: Recipient::LockHolders,
            ..Leg::exact("", None, Some(instrument_id), amount(a, "the lock amount")?, LOCK_KINDS)
        }],
        E::ResizeLock { new_amount, .. } => vec![Leg {
            to: Recipient::LockHolders,
            ..Leg::exact("", None, None, amount(new_amount, "the lock amount")?, LOCK_KINDS)
        }],
        E::UsdcxBurn { amount: a, destination_recipient, .. } => {
            vec![Leg::exact(destination_recipient, None, None, amount(a, "the burn amount")?, BURN_KINDS)]
        }
        // Moves nothing to others: merges, splits, venue and ticket upkeep, requests, acceptances.
        E::ProposeDvp { .. }
        | E::CancelDvpProposal { .. }
        | E::RejectDvpProposal { .. }
        | E::RequestPreapproval { .. }
        | E::RequestUserService { .. }
        | E::AcceptCip56 { .. }
        | E::SplitCc { .. }
        | E::ProcessLockUnlockRequests { .. }
        | E::TerminateLock { .. }
        | E::IssueTickets { .. }
        | E::SplitHoldings { .. }
        | E::CreateAtomicDvpVenue { .. }
        | E::UpdateVenueKey { .. }
        | E::RetireVenue { .. }
        | E::CancelTickets { .. }
        | E::UsdcxRequestAgreement { .. }
        | E::UsdcxMint { .. }
        | E::UsdcxReclaimHolding { .. }
        | E::UsdcxUpdatePreApproval { .. }
        | E::UsdcxTerminateAgreement { .. } => Vec::new(),
    };
    Ok((legs, missing))
}

/// The whole-transaction check of a linked agent, before any signature that no command
/// directs: such a transaction never carries a directed (`ctl-`) command id.
pub fn check_before_signing(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
) -> Result<(), ContentRefusal> {
    check_content(agent, platform, expectation, request, verification, facts, None)
}

/// [`check_before_signing`] for a transaction `command` directs: it carries exactly the
/// command's Canton command id.
pub fn check_before_signing_directed(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
    command: &Command,
) -> Result<(), ContentRefusal> {
    check_content(agent, platform, expectation, request, verification, facts, Some(command))
}

fn check_content(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
    directed: Option<&Command>,
) -> Result<(), ContentRefusal> {
    if verification.hash_check != HashCheck::Match || verification.computed_hash == [0u8; 32] {
        return Err(ContentRefusal::HashNotMatched);
    }
    let facts = facts.as_ref().map_err(|e| ContentRefusal::Unreadable(e.clone()))?;
    if facts.act_as.len() != 1 || facts.act_as.first().map(String::as_str) != Some(agent) {
        return Err(ContentRefusal::ActAs(facts.act_as.clone()));
    }
    if !facts.unmodelled.is_empty() {
        return Err(ContentRefusal::Unmodelled(facts.unmodelled.clone()));
    }
    check_command_id(directed, &facts.command_id).map_err(|e| {
        ContentRefusal::BadRequest(format!("the transaction's command id {:?} is refused: {}", facts.command_id, e.code_name()))
    })?;
    let (legs, missing_pin) = expected_legs(agent, platform, expectation, request, facts)?;
    let operation = crate::control_survey::operation_kind(expectation);
    let mut used = vec![false; legs.len()];
    for flow in facts.outflows.iter().filter(|f| f.to != agent) {
        let slot = legs
            .iter()
            .zip(used.iter_mut())
            .find(|(leg, used)| leg.matches(flow) && (leg.amount.is_none() || !**used));
        match slot {
            Some((_, used)) => *used = true,
            None => {
                return Err(ContentRefusal::UnexpectedOutflow { operation, flow: flow.clone() });
            }
        }
    }
    match missing_pin {
        Some(pin) => Err(ContentRefusal::PinMissing { operation, pin }),
        None => Ok(()),
    }
}

/// What one directed signature is bound to: the state read under the control lock, the
/// command as its controller signed it, and the agent's own evaluation context.
pub struct DirectedSignature<'a> {
    pub state: &'a ControlState,
    pub now: UtcTime,
    /// The signed command bytes.
    pub command: &'a [u8],
    pub envelope: &'a ProofEnvelope,
    pub ctx: &'a EvalContext<'a>,
}

/// Why a directed signature was refused before it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureRefusal {
    /// The content check every linked signature passes.
    Content(ContentRefusal),
    /// The crate's binding of the command to the transaction, or the command and the rules.
    Binding(BindingRefusal),
    /// A package outside the rules in force.
    Package(PackageId),
    /// The package ids of the transaction could not be read.
    Packages(String),
}

impl SignatureRefusal {
    /// The receipt's answer: (refused rather than failed, code, name).
    pub fn receipt_code(&self) -> (bool, u16, &'static str) {
        let content = agent_control::CommandFailure::ContentRefused;
        match self {
            Self::Content(_) | Self::Packages(_) => (false, content.code(), content.code_name()),
            Self::Binding(BindingRefusal::Binding(e)) => (false, e.failure().code(), e.failure().code_name()),
            Self::Binding(BindingRefusal::Directed(d)) => (true, d.code(), d.code_name()),
            Self::Package(_) => {
                let reason = ReasonCode::PackageNotAllowed;
                (true, reason.as_u16(), reason.code_name())
            }
        }
    }
}

impl fmt::Display for SignatureRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content(refusal) => write!(f, "{refusal}"),
            Self::Binding(BindingRefusal::Binding(e)) => {
                write!(f, "the transaction is not the one the command directs: {}", e.code_name())
            }
            Self::Binding(BindingRefusal::Directed(d)) => write!(f, "the command is refused: {}", d.code_name()),
            Self::Package(id) => write!(f, "package {id} is outside the rules in force"),
            Self::Packages(reason) => write!(f, "the transaction's packages are unreadable: {}", clean(reason)),
        }
    }
}

impl std::error::Error for SignatureRefusal {}

/// The packages the rule judges: every node's template and interface package.
fn enforced_packages(prepared_transaction: &[u8]) -> Result<BTreeSet<PackageId>, SignatureRefusal> {
    let ids = tx_verifier::facts::package_ids(prepared_transaction)
        .map_err(|e| SignatureRefusal::Packages(e.to_string()))?;
    ids.enforced
        .iter()
        .map(|id| PackageId::parse(id).map_err(|_| SignatureRefusal::Packages(format!("package id {id:?}"))))
        .collect()
}

/// Why the packages of a signature were refused (`package_not_allowed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageRefusal {
    /// A package outside `[vetted_packages]`.
    Local(PackageId),
    /// A package outside the packages rule of the policy in force.
    Policy(PackageId),
    /// The package ids could not be read, so a selection cannot be checked.
    Unreadable(String),
    /// No local hash matched the server's, so the checked bytes need not be the signed ones.
    HashUnmatched(UnmatchedHash),
}

/// What the hash check saw instead of a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnmatchedHash {
    /// The local hash differs from the server's.
    Mismatch,
    /// No local hash was computed, for this reason.
    Uncomputed(String),
}

impl PackageRefusal {
    /// The refused package, when the ids were read.
    pub fn package(&self) -> Option<&PackageId> {
        match self {
            Self::Local(id) | Self::Policy(id) => Some(id),
            Self::Unreadable(_) | Self::HashUnmatched(_) => None,
        }
    }

    /// The same refusal as a directed signature's binding gives it, for its receipt.
    pub fn signature_refusal(&self) -> SignatureRefusal {
        match self {
            Self::Local(id) | Self::Policy(id) => SignatureRefusal::Package(*id),
            Self::Unreadable(reason) => SignatureRefusal::Packages(reason.clone()),
            Self::HashUnmatched(UnmatchedHash::Mismatch) => {
                SignatureRefusal::Packages("the transaction hash did not match".to_string())
            }
            Self::HashUnmatched(UnmatchedHash::Uncomputed(_)) => {
                SignatureRefusal::Packages("the transaction hash was not computed".to_string())
            }
        }
    }
}

impl fmt::Display for PackageRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = ReasonCode::PackageNotAllowed.code_name();
        match self {
            Self::Local(id) => write!(f, "{code}: package {id} is outside [vetted_packages]"),
            Self::Policy(id) => write!(f, "{code}: package {id} is outside the packages rule of the policy in force"),
            Self::Unreadable(reason) => write!(f, "{code}: the transaction's packages are unreadable: {}", clean(reason)),
            Self::HashUnmatched(UnmatchedHash::Mismatch) => write!(
                f,
                "{code}: the local transaction hash does not match the server's, so its packages are unverified"
            ),
            Self::HashUnmatched(UnmatchedHash::Uncomputed(reason)) => {
                let reason: String = reason.chars().take(MAX_REFUSAL_REASON_CHARS).collect();
                let reason = clean(&reason);
                write!(f, "{code}: the local transaction hash was not computed ({reason}), so its packages are unverified")
            }
        }
    }
}

/// Characters of an uncomputed hash's reason shown in its refusal.
const MAX_REFUSAL_REASON_CHARS: usize = 160;

impl std::error::Error for PackageRefusal {}

/// Every template and interface package must be in `[vetted_packages]` and in `policy`'s packages
/// rule, each when it selects, on bytes whose hash matched; neither selecting reads nothing.
pub fn check_signature_packages(
    prepared_transaction: &[u8],
    hash_check: &HashCheck,
    local: &agent_logic::config::VettedPackages,
    policy: Option<&agent_control::Rules>,
) -> Result<(), PackageRefusal> {
    let local = match local {
        agent_logic::config::VettedPackages::All => None,
        agent_logic::config::VettedPackages::Selected(ids) => Some(ids),
    };
    let policy = policy.filter(|rules| !rules.packages.any);
    if local.is_none() && policy.is_none() {
        return Ok(());
    }
    // Otherwise the server hash is signed, which need not be of the bytes read here
    match hash_check {
        HashCheck::Match => {}
        HashCheck::Mismatch { .. } => return Err(PackageRefusal::HashUnmatched(UnmatchedHash::Mismatch)),
        HashCheck::Uncomputed { reason } => {
            return Err(PackageRefusal::HashUnmatched(UnmatchedHash::Uncomputed(reason.clone())));
        }
    }
    let used = enforced_packages(prepared_transaction).map_err(|e| match e {
        SignatureRefusal::Packages(reason) => PackageRefusal::Unreadable(reason),
        other => PackageRefusal::Unreadable(other.to_string()),
    })?;
    if let Some(id) = local.and_then(|ids| used.iter().find(|id| !ids.contains(*id))) {
        return Err(PackageRefusal::Local(*id));
    }
    match policy {
        Some(rules) => check_packages(rules, &used).map_err(PackageRefusal::Policy),
        None => Ok(()),
    }
}

/// The crate's end-to-end binding of one directed operation.
type Authorize = fn(
    &ControlState,
    UtcTime,
    &[u8],
    &ProofEnvelope,
    &DirectedTx<'_>,
    &EvalContext<'_>,
) -> Result<DirectedAuthorization, BindingRefusal>;

/// What a directed signature is checked on: the agent's own request and the locally
/// hashed bytes about to be signed, with what was read from them.
#[derive(Clone, Copy)]
pub struct Prepared<'a> {
    pub expectation: &'a OperationExpectation,
    pub request: OwnRequest<'a>,
    pub verification: &'a VerificationResult,
    pub facts: &'a Result<TxFacts, String>,
    pub prepared_transaction: &'a [u8],
}

/// The content check, the crate's binding, then the packages rule in force.
fn check_directed(
    agent: &str,
    platform: &PlatformParties,
    prepared: &Prepared<'_>,
    directed: &DirectedSignature<'_>,
    venue_cid: Option<&str>,
    authorize: Authorize,
) -> Result<DirectedAuthorization, SignatureRefusal> {
    let command = Command::decode(directed.command).map_err(|e| {
        SignatureRefusal::Binding(BindingRefusal::Directed(agent_control::DirectedRefusal::Command(
            CommandError::Canon(e),
        )))
    })?;
    let Prepared { expectation, request, verification, facts, prepared_transaction } = *prepared;
    check_before_signing_directed(agent, platform, expectation, request, verification, facts, &command)
        .map_err(SignatureRefusal::Content)?;
    let facts = facts
        .as_ref()
        .map_err(|e| SignatureRefusal::Content(ContentRefusal::Unreadable(e.clone())))?;
    let coverage = if verification.hash_check == HashCheck::Match { Coverage::Fields } else { Coverage::Unverified };
    let tx = DirectedTx { facts, coverage, venue_cid };
    let authorization = authorize(directed.state, directed.now, directed.command, directed.envelope, &tx, directed.ctx)
        .map_err(SignatureRefusal::Binding)?;
    let used = enforced_packages(prepared_transaction)?;
    check_packages(directed.state.rules_at(directed.now).rules(), &used).map_err(SignatureRefusal::Package)?;
    Ok(authorization)
}

/// The directed transfer binding, on the locally hashed bytes about to be signed: the
/// content check, the crate's end-to-end binding, then the packages rule in force.
#[allow(clippy::too_many_arguments)]
pub fn check_directed_transfer(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
    prepared_transaction: &[u8],
    directed: &DirectedSignature<'_>,
) -> Result<DirectedAuthorization, SignatureRefusal> {
    let prepared = Prepared { expectation, request, verification, facts, prepared_transaction };
    check_directed(agent, platform, &prepared, directed, None, authorize_directed_transfer)
}

/// The directed swap binding on the locally hashed bytes: the content check, the crate's
/// end-to-end swap binding on the venue the agent requested, then the packages rule.
pub fn check_directed_swap(
    agent: &str,
    platform: &PlatformParties,
    prepared: &Prepared<'_>,
    directed: &DirectedSignature<'_>,
    venue_cid: &str,
) -> Result<DirectedAuthorization, SignatureRefusal> {
    check_directed(agent, platform, prepared, directed, Some(venue_cid), authorize_directed_swap)
}

#[cfg(test)] #[path = "intent_captured_tests.rs"] mod captured_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_proto::ledger::{
        ExecuteMultiCallParams, McAcceptDvpAndAllocate, McBatchPay, McBatchTransfer, McPaymentTarget,
        McTransferTarget, MultiCallOp,
    };
    use tx_verifier::facts::AllocationFact;
    use tx_verifier::{BatchPayTarget, ExpectedQuantity};
    use orderbook_proto::rfqv2::{AtomicDvpSettleParams, AtomicFeeSpec};
    use tx_verifier::Coverage;
    use tx_verifier::facts::{ExerciseFact, Quantity, SettleFacts};

    const ME: &str = "agent::1220aa";
    const BOB: &str = "bob::1220bb";
    const EVE: &str = "eve::1220ee";
    const DSO: &str = "dso::1220dd";
    const FEE: &str = "fee::1220ff";
    const TRAFFIC: &str = "traffic::1220cc";

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    fn flow(to: &str, admin: &str, id: &str, amount: &str, kind: FlowKind) -> ValueFlow {
        ValueFlow { to: to.into(), instrument_admin: admin.into(), instrument_id: id.into(), amount: dec(amount), kind }
    }

    fn cc(to: &str, amount: &str) -> ValueFlow {
        flow(to, DSO, AMULET, amount, FlowKind::Holding)
    }

    fn facts(outflows: Vec<ValueFlow>) -> TxFacts {
        TxFacts { act_as: vec![ME.into()], outflows, ..TxFacts::default() }
    }

    fn matched() -> VerificationResult {
        VerificationResult {
            accepted: true,
            computed_hash: [7u8; 32],
            summary: String::new(),
            warnings: Vec::new(),
            rejection_reason: None,
            burn_holding_cids: Vec::new(),
            hash_check: HashCheck::Match,
            coverage: Coverage::Fields,
            would_refuse: Vec::new(),
        }
    }

    fn pins() -> PlatformParties {
        PlatformParties { fee: Some(FEE.into()), prepaid_traffic: Some(TRAFFIC.into()) }
    }

    fn check_with(
        expectation: &OperationExpectation,
        request: OwnRequest<'_>,
        facts: TxFacts,
        platform: &PlatformParties,
    ) -> Result<(), ContentRefusal> {
        check_before_signing(ME, platform, expectation, request, &matched(), &Ok(facts))
    }

    fn check(expectation: &OperationExpectation, facts: TxFacts) -> Result<(), ContentRefusal> {
        check_with(expectation, OwnRequest::Ledger(None), facts, &pins())
    }

    fn transfer_cc(amount: &str) -> OperationExpectation {
        OperationExpectation::TransferCc {
            sender_party: ME.into(),
            receiver_party: BOB.into(),
            amount: amount.into(),
            command_id: "c".into(),
            dso_party: None,
        }
    }

    fn refused_outflow(result: Result<(), ContentRefusal>) -> ValueFlow {
        match result {
            Err(ContentRefusal::UnexpectedOutflow { flow, .. }) => flow,
            other => panic!("expected an unexpected outflow, got {other:?}"),
        }
    }

    #[test]
    fn a_transfer_moves_exactly_its_own_leg() {
        let e = transfer_cc("1.5");
        assert_eq!(check(&e, facts(vec![cc(BOB, "1.5000000000"), cc(ME, "98.5")])), Ok(()));
        // A second root sending more, a wrong amount and a duplicate leg are refused.
        assert_eq!(refused_outflow(check(&e, facts(vec![cc(BOB, "1.5"), cc(EVE, "3.25")]))).to, EVE);
        refused_outflow(check(&e, facts(vec![cc(BOB, "1.6")])));
        refused_outflow(check(&e, facts(vec![cc(BOB, "1.5"), cc(BOB, "1.5")])));
        // Another instrument with the same amount is not the leg.
        refused_outflow(check(&e, facts(vec![flow(BOB, DSO, "USDCx", "1.5", FlowKind::Holding)])));
        let err = check(&e, facts(vec![cc(EVE, "1")])).unwrap_err().to_string();
        assert!(err.contains("content check refused before signing") && err.contains(EVE), "{err}");
    }

    #[test]
    fn hash_act_as_templates_and_readability_are_required() {
        let e = transfer_cc("1");
        let ok = facts(vec![cc(BOB, "1")]);
        let mut mismatch = matched();
        mismatch.hash_check = HashCheck::Mismatch { server_hex: "aa".into(), computed_hex: "bb".into() };
        mismatch.computed_hash = [0u8; 32];
        let r = check_before_signing(ME, &pins(), &e, OwnRequest::Ledger(None), &mismatch, &Ok(ok.clone()));
        assert_eq!(r, Err(ContentRefusal::HashNotMatched));
        let r = check_before_signing(ME, &pins(), &e, OwnRequest::Ledger(None), &matched(), &Err("bad bytes".into()));
        assert_eq!(r, Err(ContentRefusal::Unreadable("bad bytes".into())));
        for act_as in [vec![], vec![ME.to_string(), BOB.to_string()], vec![BOB.to_string()]] {
            let f = TxFacts { act_as: act_as.clone(), ..ok.clone() };
            assert_eq!(check(&e, f), Err(ContentRefusal::ActAs(act_as)));
        }
        let f = TxFacts { unmodelled: vec!["Foo.Bar:Baz".into()], ..ok };
        assert!(matches!(check(&e, f), Err(ContentRefusal::Unmodelled(_))));
    }

    #[test]
    fn merges_and_splits_move_nothing_to_others() {
        let split = OperationExpectation::SplitCc { party: ME.into(), output_amounts: vec!["1".into()], amulet_cids: vec![], dso_party: None };
        assert_eq!(check(&split, facts(vec![cc(ME, "1")])), Ok(()));
        refused_outflow(check(&split, facts(vec![cc(EVE, "1")])));
        let merge = OperationExpectation::TransferCip56 {
            sender_party: ME.into(),
            receiver_party: ME.into(),
            instrument_id: "TOK".into(),
            instrument_admin: "reg::1220ab".into(),
            amount: "5".into(),
            command_id: None,
            input_holding_cids: vec![],
        };
        assert_eq!(check(&merge, facts(vec![flow(ME, "reg::1220ab", "TOK", "5", FlowKind::PendingTransfer)])), Ok(()));
        refused_outflow(check(&merge, facts(vec![flow(EVE, "reg::1220ab", "TOK", "5", FlowKind::Holding)])));
    }

    #[test]
    fn platform_parties_count_only_for_fee_and_traffic_operations() {
        let traffic = OperationExpectation::PrepayTraffic { sender_party: ME.into(), amount: "20".into(), command_id: "c".into(), receiver_party: None, dso_party: None };
        assert_eq!(check(&traffic, facts(vec![cc(TRAFFIC, "20")])), Ok(()));
        refused_outflow(check(&traffic, facts(vec![cc(EVE, "20")])));
        refused_outflow(check(&traffic, facts(vec![cc(TRAFFIC, "21")])));
        // Without the pin the top-up is refused for that alone; anything else wrong still refuses as such.
        let none = PlatformParties::default();
        let unpinned = check_with(&traffic, OwnRequest::Ledger(None), facts(vec![cc(TRAFFIC, "20")]), &none);
        assert_eq!(unpinned, Err(ContentRefusal::PinMissing { operation: "prepay_traffic", pin: PREPAID_TRAFFIC_PIN }));
        let text = unpinned.unwrap_err().to_string();
        assert!(text.contains("PARTY_PREPAID_TRAFFIC is not set"), "{text}");
        refused_outflow(check_with(&traffic, OwnRequest::Ledger(None), facts(vec![cc(TRAFFIC, "20"), cc(EVE, "1")]), &none));
        refused_outflow(check_with(&traffic, OwnRequest::Ledger(None), facts(vec![cc(TRAFFIC, "21")]), &none));
        let fee = OperationExpectation::PayFee {
            sender_party: ME.into(),
            fee_party: EVE.into(),
            proposal_id: "p".into(),
            fee_type: "dvp".into(),
        };
        assert_eq!(check(&fee, facts(vec![cc(FEE, "0.3"), cc(FEE, "0.1")])), Ok(()));
        refused_outflow(check(&fee, facts(vec![cc(EVE, "0.3")])));
        // A pinned party is no leg of another operation.
        refused_outflow(check(&transfer_cc("1"), facts(vec![cc(BOB, "1"), cc(FEE, "0.3")])));
        let split = OperationExpectation::SplitCc { party: ME.into(), output_amounts: vec![], amulet_cids: vec![], dso_party: None };
        refused_outflow(check(&split, facts(vec![cc(TRAFFIC, "1")])));
    }

    fn terms(proposal: &str, buyer: &str, seller: &str) -> DvpTerms {
        DvpTerms {
            proposal_id: Some(proposal.into()),
            buyer: buyer.into(),
            seller: seller.into(),
            delivery: Quantity { instrument_admin: "reg::1220ab".into(), instrument_id: "TOK".into(), amount: dec("4") },
            payment: Quantity { instrument_admin: DSO.into(), instrument_id: AMULET.into(), amount: dec("8") },
        }
    }

    fn recorded(buyer: &str, seller: &str) -> ExpectedDvpTerms {
        let quantity = |admin: &str, id: &str, amount: &str| ExpectedQuantity {
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: amount.into(),
        };
        ExpectedDvpTerms {
            proposal_id: "p1".into(),
            buyer: buyer.into(),
            seller: seller.into(),
            delivery: quantity("reg::1220ab", "TOK", "4.0000000000"),
            payment: quantity(DSO, AMULET, "8"),
        }
    }

    fn allocated(reference: &str, leg: &ValueFlow) -> AllocationFact {
        AllocationFact {
            contract_id: "a1".into(),
            settlement_ref_id: reference.into(),
            transfer_leg_id: "1".into(),
            sender: ME.into(),
            receiver: leg.to.clone(),
            instrument_admin: leg.instrument_admin.clone(),
            instrument_id: leg.instrument_id.clone(),
            amount: leg.amount,
            executor: "op::1220dd".into(),
            allocate_before_micros: 1,
            settle_before_micros: 2,
        }
    }

    #[test]
    fn an_allocation_pays_the_recorded_leg_of_its_own_proposal() {
        let allocate = |terms: Option<ExpectedDvpTerms>| OperationExpectation::Allocate {
            party: ME.into(),
            proposal_id: "p1".into(),
            dvp_cid: "d".into(),
            terms,
            settlement_operator: None,
        };
        let with = |reference: &str, leg: &ValueFlow| TxFacts { allocations: vec![allocated(reference, leg)], ..facts(vec![leg.clone()]) };
        let seller_leg = flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation);
        let buyer_leg = flow(BOB, DSO, AMULET, "8", FlowKind::Allocation);
        // The seller delivers and the buyer pays, as the agent recorded it, with no DvP in the transaction.
        assert_eq!(check(&allocate(Some(recorded(BOB, ME))), with("p1", &seller_leg)), Ok(()));
        assert_eq!(check(&allocate(Some(recorded(ME, BOB))), with("p1", &buyer_leg)), Ok(()));
        // Without recorded terms, or with another proposal's, nothing is explained.
        assert!(matches!(check(&allocate(None), with("p1", &seller_leg)), Err(ContentRefusal::BadRequest(_))));
        let mut other = recorded(BOB, ME);
        other.proposal_id = "p2".into();
        assert!(matches!(check(&allocate(Some(other)), with("p1", &seller_leg)), Err(ContentRefusal::BadRequest(_))));
        assert!(matches!(check(&allocate(Some(recorded(BOB, EVE))), with("p1", &seller_leg)), Err(ContentRefusal::BadRequest(_))));
        // Only the allocation of this proposal explains the leg; DvP terms alone no longer do.
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), with("p2", &seller_leg)));
        let mut dvp_only = facts(vec![seller_leg.clone()]);
        dvp_only.dvp = vec![terms("p1", BOB, ME)];
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), dvp_only));
        // Another amount, the other side's leg, or the leg twice is refused.
        let five = flow(BOB, "reg::1220ab", "TOK", "5", FlowKind::Allocation);
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), with("p1", &five)));
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), with("p1", &buyer_leg)));
        let mut twice = with("p1", &seller_leg);
        twice.outflows.push(seller_leg.clone());
        twice.allocations.push(allocated("p1", &seller_leg));
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), twice));
        // A leg another party sends explains nothing of the agent's.
        let mut theirs = with("p1", &seller_leg);
        if let Some(a) = theirs.allocations.first_mut() {
            a.sender = BOB.into();
        }
        refused_outflow(check(&allocate(Some(recorded(BOB, ME))), theirs));
    }

    #[test]
    fn dvp_legs_follow_the_agents_side_of_its_own_proposal() {
        let seller_leg = flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation);

        let accept = OperationExpectation::AcceptDvp {
            acceptor: ME.into(),
            proposal_id: "p1".into(),
            dvp_proposal_cid: "c".into(),
            expected_delivery_amount: "4".into(),
            expected_payment_amount: "8".into(),
            expected_delivery_instrument_id: "TOK".into(),
            expected_delivery_instrument_admin: "reg::1220ab".into(),
            expected_payment_instrument_id: AMULET.into(),
            expected_payment_instrument_admin: DSO.into(),
            terms: None,
        };
        let mut f = facts(vec![seller_leg.clone()]);
        f.dvp = vec![terms("p1", BOB, ME)];
        assert_eq!(check(&accept, f.clone()), Ok(()));
        // Terms that differ from the agent's own expectation explain nothing.
        let mut bigger = terms("p1", BOB, ME);
        bigger.delivery.amount = dec("40");
        f.dvp = vec![bigger];
        f.outflows = vec![flow(BOB, "reg::1220ab", "TOK", "40", FlowKind::Allocation)];
        refused_outflow(check(&accept, f));
        // A buyer's accept agrees by its role too, and recorded terms must name both parties.
        let mut buyer = facts(vec![flow(BOB, DSO, AMULET, "8", FlowKind::Allocation)]);
        buyer.dvp = vec![terms("p1", ME, BOB)];
        assert_eq!(check(&accept, buyer.clone()), Ok(()));
        let with_terms = |t: ExpectedDvpTerms| {
            let mut e = accept.clone();
            if let OperationExpectation::AcceptDvp { terms, .. } = &mut e {
                *terms = Some(t);
            }
            e
        };
        assert_eq!(check(&with_terms(recorded(ME, BOB)), buyer.clone()), Ok(()));
        refused_outflow(check(&with_terms(recorded(ME, EVE)), buyer));
    }

    #[test]
    fn batch_payments_accept_only_direct_holdings() {
        let pending = flow(BOB, DSO, AMULET, "2", FlowKind::PendingTransfer);
        let e = OperationExpectation::BatchPayCc {
            party: ME.into(),
            targets: vec![BatchPayTarget { receiver: BOB.into(), amount: "2".into() }],
            holding_cids: vec![],
            dso_party: None,
        };
        assert_eq!(check(&e, facts(vec![cc(BOB, "2")])), Ok(()));
        refused_outflow(check(&e, facts(vec![pending.clone()])));
        let params = Params::ExecuteMulticall(ExecuteMultiCallParams {
            operations: vec![MultiCallOp {
                op: Some(McOp::BatchPay(McBatchPay {
                    targets: vec![McPaymentTarget { receiver: BOB.into(), amount: "2".into(), ..Default::default() }],
                })),
            }],
            holding_cids: vec![],
        });
        let multicall = OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 1, terms: None };
        let request = OwnRequest::Ledger(Some(&params));
        assert_eq!(check_with(&multicall, request, facts(vec![cc(BOB, "2")]), &pins()), Ok(()));
        refused_outflow(check_with(&multicall, request, facts(vec![pending]), &pins()));
    }

    #[test]
    fn a_batch_payment_pays_each_receiver_its_summed_targets() {
        let target = |receiver: &str, amount: &str| BatchPayTarget { receiver: receiver.into(), amount: amount.into() };
        let e = OperationExpectation::BatchPayCc {
            party: ME.into(),
            targets: vec![target(BOB, "1.5"), target(EVE, "2"), target(BOB, "2.5")],
            holding_cids: vec![],
            dso_party: None,
        };
        assert_eq!(check(&e, facts(vec![cc(BOB, "4"), cc(EVE, "2")])), Ok(()));
        refused_outflow(check(&e, facts(vec![cc(BOB, "1.5"), cc(BOB, "2.5"), cc(EVE, "2")])));
        refused_outflow(check(&e, facts(vec![cc(BOB, "4.1"), cc(EVE, "2")])));
        let huge = OperationExpectation::BatchPayCc {
            party: ME.into(),
            targets: vec![target(BOB, "79228162514264337593543950335"), target(BOB, "1")],
            holding_cids: vec![],
            dso_party: None,
        };
        assert!(matches!(check(&huge, facts(vec![])), Err(ContentRefusal::BadRequest(_))));
        // The batch-pay operations of a multicall are paid the same way.
        let pay = |receiver: &str, amount: &str| McPaymentTarget { receiver: receiver.into(), amount: amount.into(), ..Default::default() };
        let params = Params::ExecuteMulticall(ExecuteMultiCallParams {
            operations: vec![MultiCallOp {
                op: Some(McOp::BatchPay(McBatchPay { targets: vec![pay(BOB, "1.5"), pay(EVE, "2"), pay(BOB, "2.5")] })),
            }],
            holding_cids: vec![],
        });
        let multicall = OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 1, terms: None };
        let request = OwnRequest::Ledger(Some(&params));
        assert_eq!(check_with(&multicall, request, facts(vec![cc(BOB, "4"), cc(EVE, "2")]), &pins()), Ok(()));
        refused_outflow(check_with(&multicall, request, facts(vec![cc(BOB, "1.5"), cc(BOB, "2.5"), cc(EVE, "2")]), &pins()));
    }

    #[test]
    fn a_taker_multicall_allocates_only_the_recorded_leg() {
        let quantity = |admin: &str, id: &str, amount: &str| Quantity {
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: dec(amount),
        };
        let recorded_quantity = |admin: &str, id: &str, amount: &str| ExpectedQuantity {
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: amount.into(),
        };
        // The accepted quote: the agent buys 10 TOK for 20 USD.
        let quote = ExpectedDvpTerms {
            proposal_id: "p1".into(),
            buyer: ME.into(),
            seller: BOB.into(),
            delivery: recorded_quantity("reg::1220ab", "TOK", "10"),
            payment: recorded_quantity("usd::1220ab", "USD", "20"),
        };
        let paid = |reference: &str, amount: &str| {
            let leg = flow(BOB, "usd::1220ab", "USD", amount, FlowKind::Allocation);
            let dvp = DvpTerms {
                delivery: quantity("reg::1220ab", "TOK", "10"),
                payment: quantity("usd::1220ab", "USD", amount),
                ..terms("p1", ME, BOB)
            };
            TxFacts { dvp: vec![dvp], allocations: vec![allocated(reference, &leg)], ..facts(vec![leg]) }
        };
        let params = Params::ExecuteMulticall(ExecuteMultiCallParams {
            operations: vec![MultiCallOp {
                op: Some(McOp::AcceptDvpAndAllocate(McAcceptDvpAndAllocate { proposal_id: "p1".into(), dvp_proposal_cid: "c".into() })),
            }],
            holding_cids: vec![],
        });
        let request = OwnRequest::Ledger(Some(&params));
        let multicall = |terms: Option<ExpectedDvpTerms>| OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 1, terms };
        assert_eq!(check_with(&multicall(Some(quote.clone())), request, paid("p1", "20"), &pins()), Ok(()));
        // A proposal of the same id that makes the agent pay 200 USD is not the accepted quote.
        refused_outflow(check_with(&multicall(Some(quote.clone())), request, paid("p1", "200"), &pins()));
        // Without recorded terms nothing is explained, and another proposal's allocation is refused.
        let unrecorded = check_with(&multicall(None), request, paid("p1", "200"), &pins());
        assert!(matches!(unrecorded, Err(ContentRefusal::BadRequest(_))), "{unrecorded:?}");
        refused_outflow(check_with(&multicall(Some(quote.clone())), request, paid("p2", "20"), &pins()));
        let mut other = quote;
        other.proposal_id = "p2".into();
        let mismatched = check_with(&multicall(Some(other)), request, paid("p1", "20"), &pins());
        assert!(matches!(mismatched, Err(ContentRefusal::BadRequest(_))), "{mismatched:?}");
    }

    #[test]
    fn an_allocation_settles_within_the_deadline_bound() {
        let allocate = OperationExpectation::Allocate {
            party: ME.into(),
            proposal_id: "p1".into(),
            dvp_cid: "d".into(),
            terms: Some(recorded(BOB, ME)),
            settlement_operator: None,
        };
        let leg = flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation);
        let now = agent_logic::clock::now_utc().timestamp_micros();
        let hour = 3_600_000_000_i64;
        let with = |allocate_before: i64, settle_before: i64| {
            let mut a = allocated("p1", &leg);
            a.allocate_before_micros = now + allocate_before * hour;
            a.settle_before_micros = now + settle_before * hour;
            TxFacts { allocations: vec![a], ..facts(vec![leg.clone()]) }
        };
        assert_eq!(check(&allocate, with(24, 48)), Ok(()));
        for (allocate_before, settle_before) in [(24, 365 * 24), (2, 1)] {
            let result = check(&allocate, with(allocate_before, settle_before));
            assert!(matches!(result, Err(ContentRefusal::BadRequest(_))), "{allocate_before} {settle_before}: {result:?}");
        }
    }

    #[test]
    fn refusal_texts_carry_no_control_characters() {
        let text = ContentRefusal::Unreadable("a\u{1b}[31mb\nc\u{2028}d\u{202e}e\u{7f}f\u{200f}g\u{2066}h".into()).to_string();
        for c in ['\u{1b}', '\n', '\u{2028}', '\u{202e}', '\u{7f}', '\u{200f}', '\u{2066}'] {
            assert!(!text.contains(c), "{text:?}");
        }
        assert!(text.ends_with("unreadable: a [31mb c d e f g h"), "{text:?}");
        let uncomputed = PackageRefusal::HashUnmatched(UnmatchedHash::Uncomputed("x\u{2029}y".into()));
        assert_eq!(uncomputed.signature_refusal(), SignatureRefusal::Packages("the transaction hash was not computed".into()));
        assert!(uncomputed.to_string().contains("(x y)"), "{uncomputed}");
        let mismatch = PackageRefusal::HashUnmatched(UnmatchedHash::Mismatch).signature_refusal();
        assert_eq!(mismatch, SignatureRefusal::Packages("the transaction hash did not match".into()));
    }

    #[test]
    fn multicall_legs_come_from_its_own_operations() {
        let op = |op| MultiCallOp { op: Some(op) };
        let params = Params::ExecuteMulticall(ExecuteMultiCallParams {
            operations: vec![
                op(McOp::BatchTransfer(McBatchTransfer {
                    instrument_admin: DSO.into(),
                    instrument_id: AMULET.into(),
                    targets: vec![McTransferTarget { receiver: BOB.into(), amount: "2".into(), description: None }],
                    ..Default::default()
                })),
                op(McOp::AcceptDvpAndAllocate(McAcceptDvpAndAllocate { proposal_id: "p1".into(), dvp_proposal_cid: "c".into() })),
            ],
            holding_cids: vec![],
        });
        let e = OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 2, terms: Some(recorded(BOB, ME)) };
        let leg = flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation);
        let mut f = facts(vec![cc(BOB, "2"), leg.clone()]);
        f.dvp = vec![terms("p1", BOB, ME)];
        f.allocations = vec![allocated("p1", &leg)];
        assert_eq!(check_with(&e, OwnRequest::Ledger(Some(&params)), f.clone(), &pins()), Ok(()));
        // A sub-operation the agent did not ask for is refused.
        f.outflows.push(cc(EVE, "2"));
        refused_outflow(check_with(&e, OwnRequest::Ledger(Some(&params)), f, &pins()));
    }

    fn settle(side: &str) -> OperationExpectation {
        OperationExpectation::AtomicDvpSettle {
            user_party: ME.into(),
            venue_cid: "v1".into(),
            template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".into(),
            quote_id: "q1".into(),
            ticket_id: String::new(),
            ticket_cid: None,
            side: side.into(),
            base_amount: "4".into(),
            quote_amount: "8".into(),
            lp_party: BOB.into(),
            base_instrument_id: "TOK".into(),
            base_instrument_admin: "reg::1220ab".into(),
            quote_instrument_id: AMULET.into(),
            quote_instrument_admin: DSO.into(),
            valid_until_micros: 0,
            lp_input_holding_cids: vec!["h-lp".into()],
            user_input_holding_cids: vec!["h1".into(), "h2".into()],
            expected_fees: vec![],
            fee_party: None,
        }
    }

    fn held(cid: &str, admin: &str, id: &str, amount: &str) -> tx_verifier::facts::HoldingFact {
        tx_verifier::facts::HoldingFact { contract_id: cid.into(), instrument_admin: admin.into(), instrument_id: id.into(), amount: dec(amount), locked: false }
    }

    /// A buy of 4 TOK for 8 CC, whose 0.5 CC fee and payment come from a 10 CC input.
    fn settle_facts(outflows: Vec<ValueFlow>) -> TxFacts {
        TxFacts {
            consumed: vec![held("h1", DSO, AMULET, "10")],
            inflows: vec![held("c1", "reg::1220ab", "TOK", "4"), held("c2", DSO, AMULET, "1.5")],
            roots: vec![ExerciseFact { template: VENUE_TEMPLATE.into(), choice: SETTLE_CHOICE.into(), contract_id: "v1".into() }],
            atomic_settle: Some(SettleFacts {
                venue_cid: "v1".into(),
                quote_id: "q1".into(),
                ticket_id: String::new(),
                user: ME.into(),
                side: QuoteSide::Buy,
                base_amount: dec("4"),
                quote_amount: dec("8"),
                valid_until_micros: 0,
                lp_input_holding_cids: vec!["h-lp".into()],
                user_input_holding_cids: vec!["h2".into(), "h1".into()],
                fees: Vec::new(),
                venue: None,
            }),
            ..facts(outflows)
        }
    }

    #[test]
    fn an_atomic_settle_pays_its_leg_and_signed_fees_and_lets_lp_change_through() {
        let e = settle("Buy");
        let params = AtomicParams::AtomicDvpSettle(AtomicDvpSettleParams {
            lp_fees: vec![AtomicFeeSpec { receiver: FEE.into(), instrument_admin: DSO.into(), instrument_id: AMULET.into(), amount: "0.5".into() }],
            ..Default::default()
        });
        let req = OwnRequest::Atomic(Some(&params));
        let lp_change = flow(BOB, "reg::1220ab", "TOK", "96", FlowKind::Holding);
        let f = settle_facts(vec![cc(BOB, "8"), cc(FEE, "0.5"), lp_change.clone()]);
        assert_eq!(check_with(&e, req, f.clone(), &pins()), Ok(()));
        // The fee only to the pinned fee party: without the pin it is refused for that alone.
        assert_eq!(
            check_with(&e, req, f.clone(), &PlatformParties::default()),
            Err(ContentRefusal::PinMissing { operation: "atomic_dvp_settle", pin: FEE_PIN })
        );
        // A second root voids the LP change allowance.
        let mut two = f.clone();
        two.roots.push(ExerciseFact { template: "Splice.AmuletRules:AmuletRules".into(), choice: "AmuletRules_Transfer".into(), contract_id: "x".into() });
        assert_eq!(refused_outflow(check_with(&e, req, two, &pins())), lp_change);
        // So do other user inputs than the agent selected.
        let mut inputs = f.clone();
        if let Some(s) = inputs.atomic_settle.as_mut() {
            s.user_input_holding_cids.push("h3".into());
        }
        refused_outflow(check_with(&e, req, inputs, &pins()));
        // The agent's own payment is exact.
        refused_outflow(check_with(&e, req, settle_facts(vec![cc(BOB, "9")]), &pins()));
        // A sell delivers base instead.
        let mut sell = settle_facts(vec![flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Holding)]);
        if let Some(s) = sell.atomic_settle.as_mut() {
            s.side = QuoteSide::Sell;
        }
        sell.consumed = vec![held("h1", "reg::1220ab", "TOK", "4")];
        sell.inflows = vec![held("c1", DSO, AMULET, "8")];
        assert_eq!(check_with(&settle("Sell"), OwnRequest::Atomic(None), sell, &pins()), Ok(()));
    }

    #[test]
    fn every_settle_fee_goes_to_the_pinned_fee_party() {
        use tx_verifier::facts::FeeLeg;
        let e = settle("Buy");
        let lp_change = flow(BOB, "reg::1220ab", "TOK", "96", FlowKind::Holding);
        let with_fee = |receiver: &str, id: &str| {
            let mut f = settle_facts(vec![cc(BOB, "8"), lp_change.clone()]);
            if let Some(s) = f.atomic_settle.as_mut() {
                s.fees = vec![FeeLeg { receiver: receiver.into(), admin: DSO.into(), id: id.into(), amount: dec("0.5") }];
            }
            if receiver == FEE {
                f.outflows.push(cc(FEE, "0.5"));
            }
            f
        };
        let params = |receiver: &str| {
            AtomicParams::AtomicDvpSettle(AtomicDvpSettleParams {
                lp_fees: vec![AtomicFeeSpec { receiver: receiver.into(), instrument_admin: DSO.into(), instrument_id: AMULET.into(), amount: "0.5".into() }],
                ..Default::default()
            })
        };
        let pinned = params(FEE);
        assert_eq!(check_with(&e, OwnRequest::Atomic(Some(&pinned)), with_fee(FEE, AMULET), &pins()), Ok(()));
        // A fee leg to the LP used to pass as LP change; it is refused now, directed or not.
        for (receiver, request) in [(BOB, None), (EVE, None), (BOB, Some(params(BOB)))] {
            let request = OwnRequest::Atomic(request.as_ref());
            match check_with(&e, request, with_fee(receiver, AMULET), &pins()) {
                Err(ContentRefusal::BadRequest(text)) => assert!(text.contains(receiver) && text.contains("PARTY_FEE"), "{text}"),
                other => panic!("expected a fee to {receiver} to refuse, got {other:?}"),
            }
        }
        // Without a pin no settle fee passes; another party's settle is not judged by it.
        assert!(matches!(
            check_with(&e, OwnRequest::Atomic(Some(&pinned)), with_fee(FEE, AMULET), &PlatformParties::default()),
            Err(ContentRefusal::PinMissing { pin: FEE_PIN, .. })
        ));
        // A fee the request does not sign is still an unexplained outflow without the pin.
        let mut unsigned = with_fee(FEE, AMULET);
        unsigned.outflows.push(cc(EVE, "0.5"));
        refused_outflow(check_with(&e, OwnRequest::Atomic(Some(&pinned)), unsigned, &PlatformParties::default()));
        let mut theirs = with_fee(BOB, AMULET);
        if let Some(s) = theirs.atomic_settle.as_mut() {
            s.user = EVE.into();
        }
        let mut lp = settle("Buy");
        if let OperationExpectation::AtomicDvpSettle { user_party, .. } = &mut lp {
            *user_party = EVE.into();
        }
        assert_eq!(check_with(&lp, OwnRequest::Atomic(None), TxFacts { outflows: vec![], ..theirs }, &pins()), Ok(()));
    }

    #[test]
    fn lp_change_never_absorbs_the_agents_own_inputs() {
        // A buy of 100 CC for 10 USDCx whose 1 CC fee the agent funds from a 5000 CC input.
        const USDCX: &str = "reg::1220ab";
        let mut e = settle("Buy");
        if let OperationExpectation::AtomicDvpSettle {
            base_amount,
            quote_amount,
            base_instrument_id,
            base_instrument_admin,
            quote_instrument_id,
            quote_instrument_admin,
            ..
        } = &mut e
        {
            *base_amount = "100".into();
            *quote_amount = "10".into();
            *base_instrument_id = AMULET.into();
            *base_instrument_admin = DSO.into();
            *quote_instrument_id = "USDCx".into();
            *quote_instrument_admin = USDCX.into();
        }
        let params = AtomicParams::AtomicDvpSettle(AtomicDvpSettleParams {
            lp_fees: vec![AtomicFeeSpec { receiver: FEE.into(), instrument_admin: DSO.into(), instrument_id: AMULET.into(), amount: "1".into() }],
            ..Default::default()
        });
        let req = OwnRequest::Atomic(Some(&params));
        let pays = || vec![flow(BOB, USDCX, "USDCx", "10", FlowKind::Holding), cc(FEE, "1"), cc(BOB, "900")];
        let settled = |outflows: Vec<ValueFlow>, inflows: Vec<tx_verifier::facts::HoldingFact>| {
            let mut f = settle_facts(outflows);
            f.consumed = vec![held("h1", USDCX, "USDCx", "10"), held("h2", DSO, AMULET, "5000")];
            f.inflows = inflows;
            if let Some(s) = f.atomic_settle.as_mut() {
                s.base_amount = dec("100");
                s.quote_amount = dec("10");
            }
            f
        };
        let honest = || vec![held("c1", DSO, AMULET, "100"), held("c2", DSO, AMULET, "4999")];
        assert_eq!(check_with(&e, req, settled(pays(), honest()), &pins()), Ok(()));
        // Its 4999 CC change sent to the LP would pass for the LP's change.
        let mut robbed = pays();
        robbed.push(cc(BOB, "4999"));
        match check_with(&e, req, settled(robbed, vec![held("c1", DSO, AMULET, "100")]), &pins()) {
            Err(ContentRefusal::BadRequest(text)) => assert!(text.contains("4900") && text.contains(AMULET), "{text}"),
            other => panic!("expected the agent's CC to fall short, got {other:?}"),
        }
        // Short proceeds do not balance either, nor change locked to others.
        let short = vec![held("c1", DSO, AMULET, "99"), held("c2", DSO, AMULET, "4999")];
        assert!(matches!(check_with(&e, req, settled(pays(), short), &pins()), Err(ContentRefusal::BadRequest(_))));
        let mut locked = honest();
        if let Some(change) = locked.last_mut() {
            change.locked = true;
        }
        assert!(matches!(check_with(&e, req, settled(pays(), locked), &pins()), Err(ContentRefusal::BadRequest(_))));
    }

    #[test]
    fn an_unreadable_packages_reason_is_shown_clean() {
        let shown = SignatureRefusal::Packages("a\u{2028}b\nc\u{202E}d".into()).to_string();
        assert!(shown.contains("a b c d"), "{shown:?}");
        assert!(!shown.chars().any(crate::log_text::is_log_unsafe), "{shown:?}");
    }

    #[test]
    fn request_amounts_must_be_exact_decimals() {
        for bad in ["1e3", "", "+1", "1.", ".5", "-1", " 1"] {
            assert!(amount(bad, "x").is_err(), "{bad}");
        }
        assert_eq!(amount("1.50", "x").unwrap(), dec("1.5"));
        assert!(matches!(check(&transfer_cc("1e3"), facts(vec![])), Err(ContentRefusal::BadRequest(_))));
    }

    #[test]
    fn an_undirected_transaction_never_carries_a_directed_command_id() {
        let mut f = facts(vec![cc(BOB, "10")]);
        f.command_id = "ctl-01890a5d-ac96-774b-bcce-b302099a8057".into();
        match check(&transfer_cc("10"), f.clone()) {
            Err(ContentRefusal::BadRequest(text)) => assert!(text.contains("reserved_command_id"), "{text}"),
            other => panic!("expected the reserved command id to refuse, got {other:?}"),
        }
        // The prefix is exact bytes; any other id passes this rule.
        for id in ["CTL-x", "cli-cc-1", "send-transfer-1", ""] {
            f.command_id = id.into();
            assert_eq!(check(&transfer_cc("10"), f.clone()), Ok(()), "{id}");
        }
    }

    /// The directed binding on real prepared transactions, signed commands and replayed states.
    mod directed {
        use super::*;
        use crate::control::commands::tests::{accepting, fixture, signed_command};
        use agent_control::policy::Allowlist;
        use agent_control::{
            Amount, BindingError, CommandOp, DEFAULT_LINKED_RULES, Network, PackageId, Rules, UsageTotals, canton_coin,
        };
        use agent_logic::control_store::testing::{TestChain, party_for};
        use base64::Engine;
        use orderbook_proto::orderbook::PendingCommand;
        use tx_verifier::VerifyMode;

        const SEED: [u8; 32] = [0x5c; 32];

        fn now() -> UtcTime {
            UtcTime::from_unix_secs(agent_logic::clock::now_secs_i64()).unwrap()
        }

        /// A linked state (with an accepted policy of `rules`) and a 10 CC transfer to bob it allows.
        fn case(rules: Option<Rules>) -> (PartyId, ControlState, PendingCommand) {
            let party = party_for(SEED, "agent-1").unwrap();
            let chain = match rules {
                Some(rules) => accepting(SEED, &party, rules).0,
                None => TestChain::build(SEED, &party, Network::Devnet, 1).unwrap(),
            };
            let op = CommandOp::TransferCc {
                receiver: PartyId::parse(&fixture::bob()).unwrap(),
                amount: Amount::parse("10").unwrap(),
                description: None,
                settlement_proposal_id: None,
            };
            let item = signed_command(&party, op, now(), 600, chain.state.head().unwrap());
            (party, chain.state, item)
        }

        fn bound(
            party: &PartyId,
            state: &ControlState,
            item: &PendingCommand,
            alter: impl Fn(&mut fixture::Shape),
        ) -> Result<DirectedAuthorization, SignatureRefusal> {
            bound_in(party, state, item, alter, VerifyMode { strict_all: true, ..VerifyMode::default() }).1
        }

        /// [`bound`] on a verification in `mode`, which it also returns.
        fn bound_in(
            party: &PartyId,
            state: &ControlState,
            item: &PendingCommand,
            alter: impl Fn(&mut fixture::Shape),
            mode: VerifyMode,
        ) -> (VerificationResult, Result<DirectedAuthorization, SignatureRefusal>) {
            let command = Command::decode(&item.command).unwrap();
            let mut shape = fixture::Shape {
                agent: party.to_string(),
                paid: fixture::bob(),
                declared: fixture::bob(),
                amount: "10".into(),
                input: "100".into(),
                change: "90".into(),
                extra: None,
                second_root: false,
                command_id: command.canton_command_id(),
            };
            alter(&mut shape);
            let bytes = fixture::cc_transfer(&shape);
            let expectation = OperationExpectation::TransferCc {
                sender_party: party.to_string(),
                receiver_party: fixture::bob(),
                amount: "10".into(),
                command_id: command.canton_command_id(),
                dso_party: Some(fixture::dso()),
            };
            let hash = tx_verifier::hasher::try_compute_hash(&bytes, "V2").unwrap();
            let server_hash = base64::engine::general_purpose::STANDARD.encode(hash);
            let verification = tx_verifier::verify_and_hash_with(&bytes, &server_hash, "V2", &expectation, mode, false).unwrap();
            let facts = tx_verifier::facts::extract_facts(&bytes, party.as_str()).map_err(|e| e.to_string());
            let envelope = ProofEnvelope::decode(&item.proof).unwrap();
            let cc = canton_coin(&PartyId::parse(&fixture::dso()).unwrap()).unwrap();
            let (platform, usage, now) = (agent_control::PlatformParties::NONE, UsageTotals::new(), now());
            let ctx = EvalContext { agent_party: party, cc: &cc, platform: &platform, now, usage: &usage };
            let directed = DirectedSignature { state, now, command: &item.command, envelope: &envelope, ctx: &ctx };
            let request = OwnRequest::Ledger(None);
            let bound =
                check_directed_transfer(party.as_str(), &PlatformParties::default(), &expectation, request, &verification, &facts, &bytes, &directed);
            (verification, bound)
        }

        #[test]
        fn an_honest_transfer_binds_and_every_rewrite_is_refused_by_its_own_rule() {
            let (party, state, item) = case(None);
            let done = bound(&party, &state, &item, |_| {}).unwrap();
            assert!(done.decision.allowed, "{:?}", done.decision);
            let eve = fixture::eve();
            type Rewrite = fn(&mut fixture::Shape);
            let rewrites: [(Rewrite, String); 4] = [
                (|s| s.second_root = true, "expected 1 root node, got 2".into()),
                (
                    |s| s.command_id = "ctl-00000000-0000-7000-8000-000000000000".into(),
                    "command id ctl-00000000-0000-7000-8000-000000000000 is not the requested ctl-".into(),
                ),
                (|s| s.extra = Some((fixture::eve(), "1".into())), format!("unexpected Amulet of 1 for {eve}")),
                (|s| s.paid = fixture::eve(), format!("unexpected Amulet of 10 for {eve}")),
            ];
            // Linked, the inspection refuses each rewrite first, so its hash is never matched
            let linked = VerifyMode { strict_all: true, ..VerifyMode::default() };
            for (alter, reason) in &rewrites {
                let (verification, refused) = bound_in(&party, &state, &item, alter, linked);
                let rejected = verification.rejection_reason.unwrap_or_default();
                assert!(rejected.contains(reason.as_str()), "{reason}: {rejected}");
                assert_eq!(refused.err(), Some(SignatureRefusal::Content(ContentRefusal::HashNotMatched)), "{reason}");
            }
            // With the inspection only observed, each rewrite is still refused by its own rule
            let observed = VerifyMode { strict_all: false, setting: tx_verifier::VerifySetting::Observe };
            let bound = |alter: Rewrite| bound_in(&party, &state, &item, alter, observed).1;
            match bound(|s| s.second_root = true) {
                Err(SignatureRefusal::Binding(BindingRefusal::Binding(BindingError::RootCount))) => {}
                other => panic!("expected root_count, got {other:?}"),
            }
            match bound(|s| s.command_id = "ctl-00000000-0000-7000-8000-000000000000".into()) {
                Err(SignatureRefusal::Content(ContentRefusal::BadRequest(text))) => {
                    assert!(text.contains("command_id_mismatch"), "{text}")
                }
                other => panic!("expected command_id_mismatch, got {other:?}"),
            }
            match bound(|s| s.extra = Some((fixture::eve(), "1".into()))) {
                Err(SignatureRefusal::Content(ContentRefusal::UnexpectedOutflow { flow, .. })) => {
                    assert_eq!(flow.to, fixture::eve())
                }
                other => panic!("expected the extra outflow to refuse, got {other:?}"),
            }
            match bound(|s| s.paid = fixture::eve()) {
                Err(SignatureRefusal::Content(ContentRefusal::UnexpectedOutflow { .. })) => {}
                other => panic!("expected the substituted leg to refuse, got {other:?}"),
            }
            let refused = SignatureRefusal::Binding(BindingRefusal::Binding(BindingError::RootCount));
            assert_eq!(refused.receipt_code(), (false, 1407, "content_refused"));
        }

        #[test]
        fn the_package_rule_in_force_judges_every_directed_signature() {
            let other = Rules { packages: Allowlist { any: false, items: vec![PackageId([1; 32])] }, ..DEFAULT_LINKED_RULES };
            let (party, state, item) = case(Some(other));
            let refused = bound(&party, &state, &item, |_| {}).unwrap_err();
            assert!(matches!(refused, SignatureRefusal::Package(_)), "{refused:?}");
            assert_eq!(refused.receipt_code(), (true, 19, "package_not_allowed"));
            let ours = PackageId::parse(fixture::PKG).unwrap();
            let selected = Rules { packages: Allowlist { any: false, items: vec![ours] }, ..DEFAULT_LINKED_RULES };
            let (party, state, item) = case(Some(selected));
            assert!(bound(&party, &state, &item, |_| {}).is_ok());
        }

        #[test]
        fn a_package_selection_signs_only_bytes_whose_hash_matched() {
            use agent_logic::config::VettedPackages;
            let bytes = fixture::cc_transfer(&fixture::Shape {
                agent: ME.into(),
                paid: fixture::bob(),
                declared: fixture::bob(),
                amount: "10".into(),
                input: "100".into(),
                change: "90".into(),
                extra: None,
                second_root: false,
                command_id: "cmd-1".into(),
            });
            let ours = PackageId::parse(fixture::PKG).unwrap();
            let listed = VettedPackages::Selected([ours].into_iter().collect());
            let rules = Rules { packages: Allowlist { any: false, items: vec![ours] }, ..DEFAULT_LINKED_RULES };
            let unmatched = [
                (HashCheck::Mismatch { server_hex: "aa".into(), computed_hex: "bb".into() }, UnmatchedHash::Mismatch),
                (HashCheck::Uncomputed { reason: "no hash".into() }, UnmatchedHash::Uncomputed("no hash".into())),
            ];
            for (hash, seen) in &unmatched {
                let refused = Err(PackageRefusal::HashUnmatched(seen.clone()));
                // A selection, local or the policy's, signs nothing it could not bind to its hash
                assert_eq!(check_signature_packages(&bytes, hash, &listed, None), refused);
                assert_eq!(check_signature_packages(&bytes, hash, &VettedPackages::All, Some(&rules)), refused);
                // No selection: nothing changes and nothing is read
                assert_eq!(check_signature_packages(b"not a transaction", hash, &VettedPackages::All, None), Ok(()));
                assert_eq!(check_signature_packages(&bytes, hash, &VettedPackages::All, Some(&DEFAULT_LINKED_RULES)), Ok(()));
            }
            assert_eq!(check_signature_packages(&bytes, &HashCheck::Match, &listed, None), Ok(()));
            assert_eq!(check_signature_packages(&bytes, &HashCheck::Match, &listed, Some(&rules)), Ok(()));
            let mismatch = PackageRefusal::HashUnmatched(UnmatchedHash::Mismatch);
            assert_eq!(
                mismatch.to_string(),
                "package_not_allowed: the local transaction hash does not match the server's, so its packages are unverified"
            );
            let forced = UnmatchedHash::Uncomputed("the inspection refused the transaction".into());
            let uncomputed = PackageRefusal::HashUnmatched(forced);
            assert_eq!(
                uncomputed.to_string(),
                "package_not_allowed: the local transaction hash was not computed (the inspection refused the \
                 transaction), so its packages are unverified"
            );
            // A reason is cut and kept on one line
            let text = |reason: String| PackageRefusal::HashUnmatched(UnmatchedHash::Uncomputed(reason)).to_string();
            let shown = |reason: &str| {
                format!("package_not_allowed: the local transaction hash was not computed ({reason}), so its packages are unverified")
            };
            assert_eq!(text(format!("a\nb\t\u{7f}{}", "x".repeat(300))), shown(&format!("a b  {}", "x".repeat(155))));
            assert_eq!(text("é".repeat(200)), shown(&"é".repeat(160)));
            let unreadable = PackageRefusal::Unreadable("x".into()).signature_refusal().receipt_code();
            for refused in [mismatch, uncomputed] {
                assert_eq!(refused.package(), None);
                assert_eq!(refused.signature_refusal().receipt_code(), unreadable);
            }
        }
    }

    /// Prepared transactions in the shape the SDK's CC flows produce: the transfer fetches the
    /// DSO config state and logs the holdings change on it, which moves nothing.
    mod real_shape {
        use super::*;
        use prost::Message;
        use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
        use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
        use v2::interactive::metadata::{InputContract, SubmitterInfo};
        use v2::interactive::transaction::v1::{node::NodeType, Create, Exercise, Fetch, Node as V1Node};
        use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction, daml_transaction};
        use v2::value::Sum;
        use v2::{Identifier, Record, RecordField, Value};

        const RULES: (&str, &str) = ("Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules");
        const CONFIG: (&str, &str) = ("Splice.ExternalPartyConfigState", "ExternalPartyConfigState");
        const AMULET_T: (&str, &str) = ("Splice.Amulet", "Amulet");

        fn ident((module, entity): (&str, &str)) -> Identifier {
            Identifier { package_id: "pkg".into(), module_name: module.into(), entity_name: entity.into() }
        }

        fn val(sum: Sum) -> Value {
            Value { sum: Some(sum) }
        }

        fn record(fields: Vec<(&str, Value)>) -> Value {
            val(Sum::Record(Record {
                record_id: None,
                fields: fields.into_iter().map(|(label, value)| RecordField { label: label.into(), value: Some(value) }).collect(),
            }))
        }

        fn party(p: &str) -> Value {
            val(Sum::Party(p.into()))
        }

        fn numeric(n: &str) -> Value {
            val(Sum::Numeric(n.into()))
        }

        fn create(cid: &str, template: (&str, &str), argument: Value, signatories: &[&str]) -> Create {
            Create {
                lf_version: "2.1".into(),
                contract_id: cid.into(),
                package_name: "pkg".into(),
                template_id: Some(ident(template)),
                argument: Some(argument),
                signatories: signatories.iter().map(|p| p.to_string()).collect(),
                ..Default::default()
            }
        }

        fn exercise(cid: &str, template: (&str, &str), choice: &str, acting: &[&str], consuming: bool, children: &[&str]) -> Exercise {
            Exercise {
                lf_version: "2.1".into(),
                contract_id: cid.into(),
                package_name: "pkg".into(),
                template_id: Some(ident(template)),
                acting_parties: acting.iter().map(|p| p.to_string()).collect(),
                choice_id: choice.into(),
                chosen_value: Some(val(Sum::Bool(true))),
                consuming,
                children: children.iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            }
        }

        fn node(id: &str, node_type: NodeType) -> daml_transaction::Node {
            daml_transaction::Node {
                node_id: id.into(),
                versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node { node_type: Some(node_type) })),
            }
        }

        fn amulet(cid: &str, owner: &str, amount: &str) -> Create {
            let arg = record(vec![
                ("dso", party(DSO)),
                ("owner", party(owner)),
                ("amount", record(vec![("initialAmount", numeric(amount))])),
            ]);
            create(cid, AMULET_T, arg, &[DSO, owner])
        }

        fn config_state(cid: &str) -> Create {
            let arg = record(vec![("dso", party(DSO)), ("amuletPrice", numeric("0.15")), ("targetArchiveAfter", record(vec![]))]);
            create(cid, CONFIG, arg, &[DSO])
        }

        /// The DSO's nonconsuming event-log choice on the config state; it creates nothing.
        fn holdings_change(cid: &str) -> Exercise {
            let mut ex = exercise(cid, CONFIG, "EventLog_HoldingsChange", &[DSO], false, &[]);
            ex.interface_id = Some(ident(("Splice.Api.Token.TransferEventsV2", "EventLog")));
            ex
        }

        /// One CC transfer under `id`: the config fetch, the spent input, the event log, the
        /// payment, the change back to the agent and the activity marker.
        fn transfer_nodes(id: &str, receiver: &str, amount: &str, input: &str, change: &str) -> Vec<daml_transaction::Node> {
            transfer_nodes_with(id, receiver, amount, input, change, &[])
        }

        /// [`transfer_nodes`] whose root also has the nodes `extra` as children.
        fn transfer_nodes_with(
            id: &str,
            receiver: &str,
            amount: &str,
            input: &str,
            change: &str,
            extra: &[&str],
        ) -> Vec<daml_transaction::Node> {
            let child = |n: &str| format!("{id}-{n}");
            let ids = ["f", "s", "l", "t", "c", "m"].map(child);
            let children: Vec<&str> = ids.iter().map(String::as_str).chain(extra.iter().copied()).collect();
            let [f, s, l, t, c, m] = ids.clone();
            vec![
                node(id, NodeType::Exercise(exercise("rules", RULES, "TransferFactory_Transfer", &[ME], false, &children))),
                node(&f, NodeType::Fetch(Fetch {
                    lf_version: "2.1".into(),
                    contract_id: "cs1".into(),
                    package_name: "pkg".into(),
                    template_id: Some(ident(CONFIG)),
                    acting_parties: vec![ME.into()],
                    ..Default::default()
                })),
                node(&s, NodeType::Exercise(exercise(input, AMULET_T, "Archive", &[ME], true, &[]))),
                node(&l, NodeType::Exercise(holdings_change("cs1"))),
                node(&t, NodeType::Create(amulet(&format!("{id}-paid"), receiver, amount))),
                node(&c, NodeType::Create(amulet(&format!("{id}-change"), ME, change))),
                node(&m, NodeType::Create(create(&format!("{id}-marker"), ("Splice.Amulet", "FeaturedAppActivityMarker"), record(vec![]), &[DSO]))),
            ]
        }

        fn encode(roots: &[&str], nodes: Vec<daml_transaction::Node>, inputs: Vec<Create>) -> Vec<u8> {
            PreparedTransaction {
                transaction: Some(DamlTransaction {
                    version: "2.1".into(),
                    roots: roots.iter().map(|r| r.to_string()).collect(),
                    nodes,
                    node_seeds: Vec::new(),
                }),
                metadata: Some(Metadata {
                    submitter_info: Some(SubmitterInfo { act_as: vec![ME.into()], command_id: "cmd".into() }),
                    input_contracts: inputs
                        .into_iter()
                        .map(|c| InputContract { created_at: 1, event_blob: vec![1, 2, 3], contract: Some(InputContractOneof::V1(c)) })
                        .collect(),
                    ..Default::default()
                }),
            }
            .encode_to_vec()
        }

        /// A single CC transfer of `amount` to `receiver` from an input of `amount + change`.
        fn cc_transfer(receiver: &str, amount: &str, input_amount: &str, change: &str) -> Vec<u8> {
            encode(
                &["r"],
                transfer_nodes("r", receiver, amount, "in1", change),
                vec![config_state("cs1"), amulet("in1", ME, input_amount)],
            )
        }

        fn facts_of(bytes: &[u8]) -> Result<TxFacts, String> {
            tx_verifier::facts::extract_facts(bytes, ME).map_err(|e| e.to_string())
        }

        fn checked(e: &OperationExpectation, request: OwnRequest<'_>, bytes: &[u8]) -> Result<(), ContentRefusal> {
            check_before_signing(ME, &pins(), e, request, &matched(), &facts_of(bytes))
        }

        #[test]
        fn a_prepaid_traffic_top_up_passes_with_the_config_state() {
            let bytes = cc_transfer(TRAFFIC, "500.0000000000", "509.5000000000", "9.5000000000");
            let facts = facts_of(&bytes).unwrap();
            assert!(facts.unmodelled.is_empty(), "{facts:?}");
            assert_eq!(facts.outflows, vec![cc(TRAFFIC, "500")]);
            let e = OperationExpectation::PrepayTraffic { sender_party: ME.into(), amount: "500".into(), command_id: "c".into(), receiver_party: None, dso_party: None };
            assert_eq!(checked(&e, OwnRequest::Ledger(None), &bytes), Ok(()));
            // Without the pinned traffic party the same payment is refused for the pin alone.
            let none = check_before_signing(ME, &PlatformParties::default(), &e, OwnRequest::Ledger(None), &matched(), &facts_of(&bytes));
            assert!(matches!(none, Err(ContentRefusal::PinMissing { pin: PREPAID_TRAFFIC_PIN, .. })), "{none:?}");
        }

        #[test]
        fn a_cc_transfer_and_a_fee_payment_pass_with_the_config_state() {
            let transfer = cc_transfer(BOB, "10.0000000000", "100.0000000000", "90.0000000000");
            assert_eq!(checked(&transfer_cc("10"), OwnRequest::Ledger(None), &transfer), Ok(()));
            assert!(matches!(
                checked(&transfer_cc("9"), OwnRequest::Ledger(None), &transfer),
                Err(ContentRefusal::UnexpectedOutflow { .. })
            ));
            let fee = cc_transfer(FEE, "0.3000000000", "10.0000000000", "9.7000000000");
            let e = OperationExpectation::PayFee {
                sender_party: ME.into(),
                fee_party: FEE.into(),
                proposal_id: "p".into(),
                fee_type: "dvp".into(),
            };
            assert_eq!(checked(&e, OwnRequest::Ledger(None), &fee), Ok(()));
        }

        #[test]
        fn a_multicall_of_cc_transfers_passes_with_the_config_state() {
            let mut nodes = vec![node(
                "mc",
                NodeType::Exercise(exercise("mc1", ("MultiCall", "MultiCall"), "MultiCall_Execute", &[ME], false, &["a", "b"])),
            )];
            nodes.extend(transfer_nodes("a", BOB, "10.0000000000", "in1", "90.0000000000"));
            nodes.extend(transfer_nodes("b", EVE, "0.5000000000", "in2", "4.5000000000"));
            let inputs = vec![config_state("cs1"), amulet("in1", ME, "100.0000000000"), amulet("in2", ME, "5.0000000000")];
            let bytes = encode(&["mc"], nodes, inputs);
            let op = |op| MultiCallOp { op: Some(op) };
            let target = |receiver: &str, amount: &str| McTransferTarget { receiver: receiver.into(), amount: amount.into(), description: None };
            let params = Params::ExecuteMulticall(ExecuteMultiCallParams {
                operations: vec![op(McOp::BatchTransfer(McBatchTransfer {
                    instrument_admin: DSO.into(),
                    instrument_id: AMULET.into(),
                    targets: vec![target(BOB, "10"), target(EVE, "0.5")],
                    ..Default::default()
                }))],
                holding_cids: vec![],
            });
            let e = OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 1, terms: None };
            assert_eq!(checked(&e, OwnRequest::Ledger(Some(&params)), &bytes), Ok(()));
        }

        /// One root create of the agent's own request, as the ledger-service prepares it.
        fn request_tx(cid: &str, template: (&str, &str), argument: Value) -> Vec<u8> {
            encode(&["r"], vec![node("r", NodeType::Create(create(cid, template, argument, &[ME])))], Vec::new())
        }

        #[test]
        fn a_preapproval_and_a_user_service_request_of_a_linked_agent_pass() {
            use orderbook_proto::ledger::{RequestPreapprovalParams, RequestUserServiceParams};
            let dso = val(Sum::Optional(Box::new(v2::Optional { value: Some(Box::new(party(DSO))) })));
            let proposal = record(vec![("receiver", party(ME)), ("provider", party(BOB)), ("expectedDso", dso)]);
            let bytes = request_tx("p1", ("Splice.Wallet.TransferPreapproval", "TransferPreapprovalProposal"), proposal);
            let params = Params::RequestPreapproval(RequestPreapprovalParams {
                instrument_admin: DSO.into(),
                instrument_allowances: vec![],
                operator: BOB.into(),
            });
            let e = OperationExpectation::RequestPreapproval { party: ME.into(), operator: None, instrument_admin: None };
            assert_eq!(checked(&e, OwnRequest::Ledger(Some(&params)), &bytes), Ok(()));
            let user = record(vec![("operator", party(BOB)), ("user", party(ME))]);
            let bytes = request_tx("u1", ("Utility.Settlement.App.V1.Service.User", "UserServiceRequest"), user.clone());
            let params = Params::RequestUserService(RequestUserServiceParams { reference_id: None, party_name: None });
            let e = OperationExpectation::RequestUserService { party: ME.into() };
            assert_eq!(checked(&e, OwnRequest::Ledger(Some(&params)), &bytes), Ok(()));
            // The same entity in another module is still outside the model.
            let bytes = request_tx("u2", ("Utility.Collateral.App.Service.User", "UserServiceRequest"), user);
            match checked(&e, OwnRequest::Ledger(Some(&params)), &bytes) {
                Err(ContentRefusal::Unmodelled(templates)) => {
                    assert_eq!(templates, ["Utility.Collateral.App.Service.User:UserServiceRequest"])
                }
                other => panic!("expected the lookalike to refuse, got {other:?}"),
            }
        }

        #[test]
        fn an_unknown_splice_template_still_refuses() {
            let mut nodes = transfer_nodes_with("r", TRAFFIC, "500.0000000000", "in1", "9.5000000000", &["x", "y"]);
            let extra = [
                ("x", ("Splice.ExternalPartyAmuletRules", "ExternalPartyConfigState")),
                ("y", ("Splice.AmuletEventLog", "AmuletEventLogV2")),
            ];
            for (id, template) in extra {
                nodes.push(node(id, NodeType::Create(create(id, template, record(vec![("dso", party(DSO))]), &[DSO]))));
            }
            let bytes = encode(&["r"], nodes, vec![config_state("cs1"), amulet("in1", ME, "509.5000000000")]);
            let e = OperationExpectation::PrepayTraffic { sender_party: ME.into(), amount: "500".into(), command_id: "c".into(), receiver_party: None, dso_party: None };
            match checked(&e, OwnRequest::Ledger(None), &bytes) {
                Err(ContentRefusal::Unmodelled(templates)) => assert_eq!(
                    templates,
                    ["Splice.AmuletEventLog:AmuletEventLogV2", "Splice.ExternalPartyAmuletRules:ExternalPartyConfigState"]
                ),
                other => panic!("expected the unmodelled templates to refuse, got {other:?}"),
            }
        }
    }
}
