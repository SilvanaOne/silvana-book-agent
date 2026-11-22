//! The content check of a linked agent before every transaction signature: a local
//! hash, the agent alone submitting, modelled templates, and only its own legs.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::fmt;

use orderbook_proto::ledger::multi_call_op::Op as McOp;
use orderbook_proto::ledger::prepare_transaction_request::Params;
use orderbook_proto::rfqv2::prepare_atomic_transaction_request::Params as AtomicParams;
use rust_decimal::Decimal;
use tx_verifier::facts::{DvpTerms, FlowKind, QuoteSide, TxFacts, ValueFlow};
use tx_verifier::{HashCheck, OperationExpectation, VerificationResult};

/// The template id of Canton Coin holdings.
const AMULET: &str = "Amulet";
const SETTLE_CHOICE: &str = "AtomicDVP_Settle";
const VENUE_TEMPLATE: &str = "AtomicDVP:AtomicDVP";

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
}

impl fmt::Display for ContentRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("content check refused before signing: ")?;
        match self {
            Self::HashNotMatched => f.write_str("the transaction hash was not computed locally"),
            Self::Unreadable(reason) => write!(f, "the transaction content is unreadable: {reason}"),
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

/// The leg of the one DvP with `proposal_id` that the agent is a party to.
fn dvp_leg_for(agent: &str, facts: &TxFacts, proposal_id: &str) -> Result<Vec<Leg>, ContentRefusal> {
    let mut legs: Vec<Leg> = facts
        .dvp
        .iter()
        .filter(|t| t.proposal_id.as_deref() == Some(proposal_id))
        .filter_map(|t| dvp_leg(agent, t))
        .collect();
    legs.dedup();
    if legs.len() > 1 {
        return Err(ContentRefusal::BadRequest(format!(
            "the transaction carries several DvP terms for proposal {proposal_id}"
        )));
    }
    Ok(legs)
}

/// The legs of an AcceptDvp: the agent's side, to the counterparty of matching terms.
fn accept_dvp_legs(agent: &str, facts: &TxFacts, e: &OperationExpectation) -> Result<Vec<Leg>, ContentRefusal> {
    let OperationExpectation::AcceptDvp {
        seller_party,
        proposal_id,
        expected_delivery_amount,
        expected_payment_amount,
        expected_delivery_instrument_id,
        expected_delivery_instrument_admin,
        expected_payment_instrument_id,
        expected_payment_instrument_admin,
        ..
    } = e
    else {
        return Ok(Vec::new());
    };
    let delivery = amount(expected_delivery_amount, "the delivery amount")?;
    let payment = amount(expected_payment_amount, "the payment amount")?;
    let agreed = |t: &&DvpTerms| {
        t.proposal_id.as_deref() == Some(proposal_id.as_str())
            && t.seller == *seller_party
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
) -> Result<Vec<Leg>, ContentRefusal> {
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
        return Ok(Vec::new());
    };
    if user_party != agent {
        return Ok(Vec::new());
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
    let mut legs = vec![own];
    if let OwnRequest::Atomic(Some(AtomicParams::AtomicDvpSettle(params))) = request {
        for fee in &params.lp_fees {
            if platform.fee.as_deref() == Some(fee.receiver.as_str()) {
                legs.push(Leg::exact(
                    &fee.receiver,
                    Some(&fee.instrument_admin),
                    Some(&fee.instrument_id),
                    amount(&fee.amount, "an LP fee amount")?,
                    TRANSFER_KINDS,
                ));
            }
        }
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
    Ok(legs)
}

/// The legs of the agent's own multicall operations.
fn multicall_legs(agent: &str, facts: &TxFacts, params: &Params) -> Result<Vec<Leg>, ContentRefusal> {
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
                for t in &pay.targets {
                    legs.push(Leg::exact(
                        &t.receiver,
                        None,
                        Some(AMULET),
                        amount(&t.amount, "a payment amount")?,
                        TRANSFER_KINDS,
                    ));
                }
            }
            McOp::Allocate(a) => legs.extend(dvp_leg_for(agent, facts, &a.proposal_id)?),
            McOp::AcceptDvp(a) => legs.extend(dvp_leg_for(agent, facts, &a.proposal_id)?),
            McOp::AcceptDvpAndAllocate(a) => legs.extend(dvp_leg_for(agent, facts, &a.proposal_id)?),
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
) -> Result<Vec<Leg>, ContentRefusal> {
    use OperationExpectation as E;
    let ledger_params = match request {
        OwnRequest::Ledger(params) => params,
        OwnRequest::Atomic(_) => None,
    };
    Ok(match expectation {
        E::TransferCc { receiver_party, amount: a, .. } => {
            vec![Leg::exact(receiver_party, None, Some(AMULET), amount(a, "the transfer amount")?, TRANSFER_KINDS)]
        }
        E::TransferCip56 { sender_party, receiver_party, instrument_id, instrument_admin, amount: a } => {
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
        E::PrepayTraffic { amount: a, .. } => match &platform.prepaid_traffic {
            Some(party) => vec![Leg::exact(party, None, Some(AMULET), amount(a, "the traffic amount")?, TRANSFER_KINDS)],
            None => Vec::new(),
        },
        E::PayFee { .. } => match &platform.fee {
            Some(party) => vec![Leg {
                to: Recipient::Party(party.clone()),
                instrument_admin: None,
                instrument_id: None,
                amount: None,
                kinds: ANY_KIND,
            }],
            None => Vec::new(),
        },
        E::AcceptDvp { .. } => accept_dvp_legs(agent, facts, expectation)?,
        E::Allocate { proposal_id, .. } => dvp_leg_for(agent, facts, proposal_id)?,
        E::ExecuteMulticall { .. } => match ledger_params {
            Some(params) => multicall_legs(agent, facts, params)?,
            None => Vec::new(),
        },
        E::AtomicDvpSettle { .. } => settle_legs(agent, facts, expectation, request, platform)?,
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
    })
}

/// The whole-transaction check of a linked agent, before any signature.
pub fn check_before_signing(
    agent: &str,
    platform: &PlatformParties,
    expectation: &OperationExpectation,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
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
    let legs = expected_legs(agent, platform, expectation, request, facts)?;
    let mut used = vec![false; legs.len()];
    for flow in facts.outflows.iter().filter(|f| f.to != agent) {
        let slot = legs
            .iter()
            .zip(used.iter_mut())
            .find(|(leg, used)| leg.matches(flow) && (leg.amount.is_none() || !**used));
        match slot {
            Some((_, used)) => *used = true,
            None => {
                return Err(ContentRefusal::UnexpectedOutflow {
                    operation: crate::control_survey::operation_kind(expectation),
                    flow: flow.clone(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_proto::ledger::{
        ExecuteMultiCallParams, McAcceptDvpAndAllocate, McBatchTransfer, McTransferTarget, MultiCallOp,
    };
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
        let split = OperationExpectation::SplitCc { party: ME.into(), output_amounts: vec!["1".into()] };
        assert_eq!(check(&split, facts(vec![cc(ME, "1")])), Ok(()));
        refused_outflow(check(&split, facts(vec![cc(EVE, "1")])));
        let merge = OperationExpectation::TransferCip56 {
            sender_party: ME.into(),
            receiver_party: ME.into(),
            instrument_id: "TOK".into(),
            instrument_admin: "reg::1220ab".into(),
            amount: "5".into(),
        };
        assert_eq!(check(&merge, facts(vec![flow(ME, "reg::1220ab", "TOK", "5", FlowKind::PendingTransfer)])), Ok(()));
        refused_outflow(check(&merge, facts(vec![flow(EVE, "reg::1220ab", "TOK", "5", FlowKind::Holding)])));
    }

    #[test]
    fn platform_parties_count_only_for_fee_and_traffic_operations() {
        let traffic = OperationExpectation::PrepayTraffic { sender_party: ME.into(), amount: "20".into(), command_id: "c".into() };
        assert_eq!(check(&traffic, facts(vec![cc(TRAFFIC, "20")])), Ok(()));
        refused_outflow(check(&traffic, facts(vec![cc(EVE, "20")])));
        refused_outflow(check(&traffic, facts(vec![cc(TRAFFIC, "21")])));
        // Unset pins reclassify nothing.
        let none = PlatformParties::default();
        refused_outflow(check_with(&traffic, OwnRequest::Ledger(None), facts(vec![cc(TRAFFIC, "20")]), &none));
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
        let split = OperationExpectation::SplitCc { party: ME.into(), output_amounts: vec![] };
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

    #[test]
    fn dvp_legs_follow_the_agents_side_of_its_own_proposal() {
        let allocate = OperationExpectation::Allocate { party: ME.into(), proposal_id: "p1".into(), dvp_cid: "d".into() };
        let seller_leg = flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation);
        let mut f = facts(vec![seller_leg.clone()]);
        f.dvp = vec![terms("p1", BOB, ME)];
        assert_eq!(check(&allocate, f.clone()), Ok(()));
        // The buyer pays instead.
        let mut buyer = facts(vec![flow(BOB, DSO, AMULET, "8", FlowKind::Allocation)]);
        buyer.dvp = vec![terms("p1", ME, BOB)];
        assert_eq!(check(&allocate, buyer), Ok(()));
        // Terms of another proposal explain nothing.
        let mut other = f.clone();
        other.dvp = vec![terms("p2", BOB, ME)];
        refused_outflow(check(&allocate, other));
        f.outflows = vec![flow(BOB, "reg::1220ab", "TOK", "5", FlowKind::Allocation)];
        refused_outflow(check(&allocate, f));

        let accept = OperationExpectation::AcceptDvp {
            seller_party: ME.into(),
            proposal_id: "p1".into(),
            dvp_proposal_cid: "c".into(),
            expected_delivery_amount: "4".into(),
            expected_payment_amount: "8".into(),
            expected_delivery_instrument_id: "TOK".into(),
            expected_delivery_instrument_admin: "reg::1220ab".into(),
            expected_payment_instrument_id: AMULET.into(),
            expected_payment_instrument_admin: DSO.into(),
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
        let e = OperationExpectation::ExecuteMulticall { party: ME.into(), op_count: 2 };
        let mut f = facts(vec![cc(BOB, "2"), flow(BOB, "reg::1220ab", "TOK", "4", FlowKind::Allocation)]);
        f.dvp = vec![terms("p1", BOB, ME)];
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
        }
    }

    fn settle_facts(outflows: Vec<ValueFlow>) -> TxFacts {
        TxFacts {
            roots: vec![ExerciseFact { template: VENUE_TEMPLATE.into(), choice: SETTLE_CHOICE.into(), contract_id: "v1".into() }],
            atomic_settle: Some(SettleFacts {
                venue_cid: "v1".into(),
                quote_id: "q1".into(),
                ticket_id: String::new(),
                user: ME.into(),
                side: QuoteSide::Buy,
                base_amount: dec("4"),
                quote_amount: dec("8"),
                lp_input_holding_cids: vec!["h-lp".into()],
                user_input_holding_cids: vec!["h2".into(), "h1".into()],
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
        // The fee only to the pinned fee party.
        refused_outflow(check_with(&e, req, f.clone(), &PlatformParties::default()));
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
        assert_eq!(check_with(&settle("Sell"), OwnRequest::Atomic(None), sell, &pins()), Ok(()));
    }

    #[test]
    fn request_amounts_must_be_exact_decimals() {
        for bad in ["1e3", "", "+1", "1.", ".5", "-1", " 1"] {
            assert!(amount(bad, "x").is_err(), "{bad}");
        }
        assert_eq!(amount("1.50", "x").unwrap(), dec("1.5"));
        assert!(matches!(check(&transfer_cc("1e3"), facts(vec![])), Err(ContentRefusal::BadRequest(_))));
    }
}
