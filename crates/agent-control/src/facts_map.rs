//! Intents read from transaction facts, the one mapping every enforcer uses, and the whole
//! binding of a directed swap or transfer to the transaction about to be signed.

use std::fmt;

use tx_verifier::facts::{Decimal, FlowKind, HoldingFact, QuoteSide, TxFacts, ValueFlow};

use crate::binding::{BindingError, RootExercise, check_command_id, check_directed_roots};
use crate::canon::time::UtcTime;
use crate::canon::values::{Amount, InstrumentId, InstrumentRef, MarketId};
use crate::command::{Command, CommandOp, command_matches};
use crate::error::CommandError;
use crate::party::PartyId;
use crate::policy::eval::{
    Coverage, Delivery, DirectedAuthorization, DirectedRefusal, EvalContext, Flow, Intent, Leg,
    Recipient, SettleFee, SettleTerms, authorize_directed,
};
use crate::policy::kinds::OperationKind;
use crate::proof::ProofEnvelope;
use crate::state::ControlState;

/// The flow kinds a swap moves value by: plain holdings.
const SWAP_KINDS: &[FlowKind] = &[FlowKind::Holding];
/// The flow kinds a transfer moves value by: a holding or a pending transfer.
const TRANSFER_KINDS: &[FlowKind] = &[FlowKind::Holding, FlowKind::PendingTransfer];

fn amount(value: Decimal) -> Result<Amount, BindingError> {
    Amount::from_decimal(value).map_err(|_| BindingError::InexactAmount)
}

fn party(s: &str) -> Result<PartyId, BindingError> {
    PartyId::parse(s).map_err(|_| BindingError::BadParty)
}

fn instrument(admin: &str, id: &str) -> Result<InstrumentRef, BindingError> {
    match (PartyId::parse(admin), InstrumentId::parse(id)) {
        (Ok(admin), Ok(id)) => Ok(InstrumentRef { admin, id }),
        _ => Err(BindingError::BadInstrument),
    }
}

/// The facts' instrument strings name `instrument`.
fn names(admin: &str, id: &str, instrument: &InstrumentRef) -> bool {
    admin == instrument.admin.as_str() && id == instrument.id.as_str()
}

/// The sum of the holdings of `instrument`.
fn total<'a>(
    holdings: impl Iterator<Item = &'a HoldingFact>,
    instrument: &InstrumentRef,
) -> Result<Amount, BindingError> {
    holdings
        .filter(|h| names(&h.instrument_admin, &h.instrument_id, instrument))
        .try_fold(Amount::ZERO, |sum, h| {
            sum.checked_add(amount(h.amount)?)
                .ok_or(BindingError::AmountOverflow)
        })
}

/// The agent's unlocked holdings of `instrument` created, less every one it consumed.
fn net_of(facts: &TxFacts, instrument: &InstrumentRef) -> Result<Amount, BindingError> {
    let created = total(facts.inflows.iter().filter(|h| !h.locked), instrument)?;
    let consumed = total(facts.consumed.iter(), instrument)?;
    created
        .checked_sub(consumed)
        .ok_or(BindingError::NegativeNet)
}

/// The terms of the agent's atomic settle: the LP and both instruments from the venue input
/// contract, the legs by the quote side, the fees, the expiry and the net received.
pub fn settle_terms(facts: &TxFacts, agent: &PartyId) -> Result<SettleTerms, BindingError> {
    let settle = facts.atomic_settle.as_ref().ok_or(BindingError::NoSettle)?;
    let venue = settle.venue.as_ref().ok_or(BindingError::NoVenueTerms)?;
    if settle.user != agent.as_str() {
        return Err(BindingError::WrongUser);
    }
    if !facts.unlisted_inputs.is_empty() {
        return Err(BindingError::UnlistedInput);
    }
    let own = |cid: &String| facts.consumed.iter().any(|h| h.contract_id == *cid);
    if !settle.user_input_holding_cids.iter().all(own) {
        return Err(BindingError::ForeignInput);
    }
    let base = Leg {
        instrument: instrument(&venue.base.admin, &venue.base.id)?,
        amount: amount(settle.base_amount)?,
    };
    let quote = Leg {
        instrument: instrument(&venue.quote.admin, &venue.quote.id)?,
        amount: amount(settle.quote_amount)?,
    };
    // A user who buys pays the quote and receives the base (AtomicDVP `QuoteSide`).
    let (sent, received) = match settle.side {
        QuoteSide::Buy => (quote, base),
        QuoteSide::Sell => (base, quote),
    };
    let fees = settle
        .fees
        .iter()
        .map(|fee| {
            Ok(SettleFee {
                receiver: party(&fee.receiver)?,
                instrument: instrument(&fee.admin, &fee.id)?,
                amount: amount(fee.amount)?,
            })
        })
        .collect::<Result<Vec<_>, BindingError>>()?;
    let valid_until = SettleTerms::expiry_from_micros(settle.valid_until_micros)
        .ok_or(BindingError::BadExpiry)?;
    let net_received = net_of(facts, &received.instrument)?;
    Ok(SettleTerms {
        lp: party(&venue.lp)?,
        sent,
        received,
        net_received,
        fees,
        valid_until,
    })
}

/// A flow keeps its recipient only when its kind is one of `kinds` and the party parses;
/// any other flow goes to `None`, which no command matches.
fn flow_of(flow: &ValueFlow, kinds: &[FlowKind]) -> Result<Flow, BindingError> {
    let to = if kinds.contains(&flow.kind) {
        PartyId::parse(&flow.to).ok().map(Recipient::Party)
    } else {
        None
    };
    Ok(Flow {
        instrument: instrument(&flow.instrument_admin, &flow.instrument_id)?,
        amount: amount(flow.amount)?,
        to,
    })
}

/// The LP's change: a plain holding created for the LP in the instrument the agent receives.
fn is_lp_change(flow: &ValueFlow, settle: &SettleTerms) -> bool {
    flow.kind == FlowKind::Holding
        && flow.to == settle.lp.as_str()
        && names(
            &flow.instrument_admin,
            &flow.instrument_id,
            &settle.received.instrument,
        )
}

/// The intent of a directed swap: the settle terms, both instruments, the venue's market and
/// every outflow but the LP's change. Fees count once, through their flows.
pub fn swap_intent(
    facts: &TxFacts,
    agent: &PartyId,
    coverage: Coverage,
) -> Result<Intent, BindingError> {
    if !facts.unmodelled.is_empty() {
        return Err(BindingError::Unmodelled);
    }
    if !facts.dvp.is_empty() {
        return Err(BindingError::UnexpectedDvp);
    }
    let settle = settle_terms(facts, agent)?;
    if facts.inflows.iter().any(|h| h.locked) {
        return Err(BindingError::LockedInflow);
    }
    let pair = facts
        .atomic_settle
        .as_ref()
        .and_then(|s| s.venue.as_ref())
        .map(|v| v.pair_name.as_str())
        .ok_or(BindingError::NoVenueTerms)?;
    let market = MarketId::parse(pair).map_err(|_| BindingError::BadMarket)?;
    let outgoing = facts
        .outflows
        .iter()
        .filter(|flow| !is_lp_change(flow, &settle))
        .map(|flow| flow_of(flow, SWAP_KINDS))
        .collect::<Result<Vec<_>, BindingError>>()?;
    Ok(Intent {
        outgoing,
        instruments: vec![
            settle.sent.instrument.clone(),
            settle.received.instrument.clone(),
        ],
        market: Some(market),
        coverage,
        settle: Some(settle),
        ..Intent::new(OperationKind::AtomicDvpSettle)
    })
}

/// The intent of a directed CC or CIP-56 transfer: every outflow, none left out.
pub fn transfer_intent(
    facts: &TxFacts,
    kind: OperationKind,
    coverage: Coverage,
) -> Result<Intent, BindingError> {
    if !matches!(
        kind,
        OperationKind::TransferCc | OperationKind::TransferCip56
    ) {
        return Err(BindingError::WrongOperation);
    }
    if !facts.unmodelled.is_empty() {
        return Err(BindingError::Unmodelled);
    }
    if !facts.dvp.is_empty() {
        return Err(BindingError::UnexpectedDvp);
    }
    let outgoing = facts
        .outflows
        .iter()
        .map(|flow| flow_of(flow, TRANSFER_KINDS))
        .collect::<Result<Vec<_>, BindingError>>()?;
    let mut instruments: Vec<InstrumentRef> = Vec::new();
    for flow in &outgoing {
        if !instruments.contains(&flow.instrument) {
            instruments.push(flow.instrument.clone());
        }
    }
    Ok(Intent {
        outgoing,
        instruments,
        coverage,
        memos: facts.memos.clone(),
        delivery: delivery_of(facts),
        ..Intent::new(kind)
    })
}

/// Direct when every outflow is a holding of its recipient, pending when one is an offer.
pub fn delivery_of(facts: &TxFacts) -> Option<Delivery> {
    if facts
        .outflows
        .iter()
        .any(|flow| flow.kind == FlowKind::PendingTransfer)
    {
        return Some(Delivery::Pending);
    }
    let direct = !facts.outflows.is_empty()
        && facts
            .outflows
            .iter()
            .all(|flow| flow.kind == FlowKind::Holding);
    direct.then_some(Delivery::Direct)
}

/// The root exercises of `facts`, for `check_directed_roots`.
pub fn roots(facts: &TxFacts) -> Vec<RootExercise<'_>> {
    facts
        .roots
        .iter()
        .map(|root| RootExercise {
            template: &root.template,
            choice: &root.choice,
            contract_id: &root.contract_id,
        })
        .collect()
}

/// The transaction a directed command is bound to, as read from the bytes about to be signed.
#[derive(Clone, Copy, Debug)]
pub struct DirectedTx<'a> {
    pub facts: &'a TxFacts,
    /// `Fields` when the facts were read from bytes hashed locally.
    pub coverage: Coverage,
    /// The venue the caller itself requested; a swap without one is refused.
    pub venue_cid: Option<&'a str>,
}

/// Why a directed transaction was refused: its content, or the command and the rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingRefusal {
    /// The transaction is not the one the command directs.
    Binding(BindingError),
    /// The command did not verify, or the rules in force refused it.
    Directed(DirectedRefusal),
}

impl BindingRefusal {
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Binding(e) => e.code_name(),
            Self::Directed(refusal) => refusal.code_name(),
        }
    }
}

impl fmt::Display for BindingRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code_name())
    }
}

impl std::error::Error for BindingRefusal {}

/// The command, decoded to bind it before it is verified, once the context names the
/// state's agent and the transaction is submitted by the agent alone.
fn bound_command(
    state: &ControlState,
    bytes: &[u8],
    tx: &DirectedTx<'_>,
    ctx: &EvalContext<'_>,
) -> Result<Command, BindingRefusal> {
    let agent = state.agent_party();
    if ctx.agent_party != agent {
        return Err(BindingRefusal::Binding(BindingError::WrongAgent));
    }
    if tx.facts.act_as.len() != 1
        || tx.facts.act_as.first().map(String::as_str) != Some(agent.as_str())
    {
        return Err(BindingRefusal::Binding(BindingError::ActAs));
    }
    Command::decode(bytes)
        .map_err(|e| BindingRefusal::Directed(DirectedRefusal::Command(CommandError::Canon(e))))
}

/// The binding checks of `command` against `tx`, in order: the command id, then the roots.
fn bind_roots(command: &Command, tx: &DirectedTx<'_>) -> Result<(), BindingRefusal> {
    check_command_id(Some(command), &tx.facts.command_id).map_err(BindingRefusal::Binding)?;
    check_directed_roots(
        &command.op,
        &roots(tx.facts),
        tx.facts.root_nodes,
        tx.venue_cid,
    )
    .map_err(BindingRefusal::Binding)
}

/// Authorizes, then re-checks that the content is exactly the command's operation.
fn authorize_bound(
    state: &ControlState,
    now: UtcTime,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    intent: &Intent,
    ctx: &EvalContext<'_>,
) -> Result<DirectedAuthorization, BindingRefusal> {
    let done = authorize_directed(state, now, bytes, envelope, intent, ctx)
        .map_err(BindingRefusal::Directed)?;
    let ctx = EvalContext { now, ..*ctx };
    if command_matches(done.command.command(), intent, &ctx) {
        Ok(done)
    } else {
        Err(BindingRefusal::Binding(BindingError::ContentMismatch))
    }
}

/// The whole binding of a directed swap, in order: the command id, the one settle root on
/// the requested venue, the intent from the facts, `authorize_directed`, `command_matches`.
pub fn authorize_directed_swap(
    state: &ControlState,
    now: UtcTime,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    tx: &DirectedTx<'_>,
    ctx: &EvalContext<'_>,
) -> Result<DirectedAuthorization, BindingRefusal> {
    let command = bound_command(state, bytes, tx, ctx)?;
    if !matches!(command.op, CommandOp::Swap { .. }) {
        return Err(BindingRefusal::Binding(BindingError::WrongOperation));
    }
    bind_roots(&command, tx)?;
    let intent =
        swap_intent(tx.facts, state.agent_party(), tx.coverage).map_err(BindingRefusal::Binding)?;
    let settled_on = tx
        .facts
        .atomic_settle
        .as_ref()
        .map(|s| s.venue_cid.as_str());
    if settled_on != tx.venue_cid {
        return Err(BindingRefusal::Binding(BindingError::WrongVenue));
    }
    authorize_bound(state, now, bytes, envelope, &intent, ctx)
}

/// The whole binding of a directed CC or CIP-56 transfer, in order: the command id, the one
/// transfer root, the intent from the facts, `authorize_directed`, `command_matches`.
pub fn authorize_directed_transfer(
    state: &ControlState,
    now: UtcTime,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    tx: &DirectedTx<'_>,
    ctx: &EvalContext<'_>,
) -> Result<DirectedAuthorization, BindingRefusal> {
    let command = bound_command(state, bytes, tx, ctx)?;
    match command.op {
        CommandOp::TransferCc { .. } | CommandOp::TransferToken { .. } => {}
        CommandOp::UsdcxBurn { .. } => {
            return Err(BindingRefusal::Binding(BindingError::NotExecuted));
        }
        CommandOp::Swap { .. } => {
            return Err(BindingRefusal::Binding(BindingError::WrongOperation));
        }
    }
    bind_roots(&command, tx)?;
    let intent = transfer_intent(tx.facts, command.op.kind(), tx.coverage)
        .map_err(BindingRefusal::Binding)?;
    authorize_bound(state, now, bytes, envelope, &intent, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{SETTLE_CHOICE, TRANSFER_CHOICE, VENUE_TEMPLATE, canton_coin};
    use crate::canon::values::{EthAddress, Network, Text, Uuid7};
    use crate::command::{CommandKind, VerifiedCommand};
    use crate::policy::eval::{
        PlatformParties, ReasonCode, UsageBucket, UsageDelta, UsageTotals, evaluate,
    };
    use crate::policy::{Allowlist, CommandScope, DEFAULT_LINKED_RULES};
    use crate::proof::ProofOutcome;
    use crate::testkit::{self, Chain, DEVNET_DELAY, at};
    use tx_verifier::facts::{
        DvpTerms, ExerciseFact, FeeLeg, Instrument, Quantity, SettleFacts, VenueTerms,
    };

    const VENUE: &str = "venue-1";
    const COMMAND_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

    fn key(hint: &str, byte: u8) -> String {
        format!("{hint}::1220{}", hex::encode([byte; 32]))
    }

    fn lp() -> String {
        key("lp", 0xb1)
    }

    fn fee() -> String {
        key("fee", 0xc1)
    }

    fn dso() -> String {
        key("dso", 0xd1)
    }

    fn reg() -> String {
        key("reg", 0xe1)
    }

    fn alice() -> String {
        key("alice", 0xa1)
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    fn amt(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn p(s: &str) -> PartyId {
        PartyId::parse(s).unwrap()
    }

    fn cc() -> InstrumentRef {
        canton_coin(&p(&dso())).unwrap()
    }

    fn usdc() -> InstrumentRef {
        InstrumentRef {
            admin: p(&reg()),
            id: InstrumentId::parse("USDCx").unwrap(),
        }
    }

    fn now() -> UtcTime {
        at(DEVNET_DELAY)
    }

    fn held(cid: &str, admin: &str, id: &str, amount: &str) -> HoldingFact {
        HoldingFact {
            contract_id: cid.into(),
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: dec(amount),
            locked: false,
        }
    }

    fn locked(h: HoldingFact) -> HoldingFact {
        HoldingFact { locked: true, ..h }
    }

    fn out(to: &str, admin: &str, id: &str, amount: &str, kind: FlowKind) -> ValueFlow {
        ValueFlow {
            to: to.into(),
            instrument_admin: admin.into(),
            instrument_id: id.into(),
            amount: dec(amount),
            kind,
        }
    }

    fn holding_to(to: &str, admin: &str, id: &str, amount: &str) -> ValueFlow {
        out(to, admin, id, amount, FlowKind::Holding)
    }

    /// A CC-USDCx settle by the agent, 300 s before its expiry, with a 1.5 CC fee.
    fn settle(side: QuoteSide, base: &str, quote: &str, inputs: &[&str]) -> SettleFacts {
        SettleFacts {
            venue_cid: VENUE.into(),
            quote_id: "q1".into(),
            ticket_id: String::new(),
            user: testkit::agent_party().to_string(),
            side,
            base_amount: dec(base),
            quote_amount: dec(quote),
            valid_until_micros: (now().unix_secs() + 300) * 1_000_000,
            lp_input_holding_cids: vec!["l1".into()],
            user_input_holding_cids: inputs.iter().map(|cid| cid.to_string()).collect(),
            fees: vec![FeeLeg {
                receiver: fee(),
                admin: dso(),
                id: "Amulet".into(),
                amount: dec("1.5"),
            }],
            venue: Some(VenueTerms {
                lp: lp(),
                provider: reg(),
                pair_name: "CC-USDCx".into(),
                base: Instrument {
                    admin: dso(),
                    id: "Amulet".into(),
                },
                quote: Instrument {
                    admin: reg(),
                    id: "USDCx".into(),
                },
            }),
        }
    }

    /// Sells 1000 CC for 151 USDCx with a 1.5 CC fee; the LP keeps 49 USDCx change.
    fn selling_cc() -> TxFacts {
        TxFacts {
            act_as: vec![testkit::agent_party().to_string()],
            command_id: format!("ctl-{COMMAND_ID}"),
            roots: vec![ExerciseFact {
                template: VENUE_TEMPLATE.into(),
                choice: SETTLE_CHOICE.into(),
                contract_id: VENUE.into(),
            }],
            root_nodes: 1,
            outflows: vec![
                holding_to(&lp(), &dso(), "Amulet", "1000"),
                holding_to(&fee(), &dso(), "Amulet", "1.5"),
                holding_to(&lp(), &reg(), "USDCx", "49"),
            ],
            inflows: vec![
                held("p1", &reg(), "USDCx", "151"),
                held("c1", &dso(), "Amulet", "98.5"),
            ],
            consumed: vec![held("a1", &dso(), "Amulet", "1100")],
            atomic_settle: Some(settle(QuoteSide::Sell, "1000", "151", &["a1"])),
            ..TxFacts::default()
        }
    }

    /// Buys 1000 CC for 150 USDCx; the 1.5 CC fee comes out of the proceeds and the LP
    /// keeps 500 CC change.
    fn buying_cc() -> TxFacts {
        TxFacts {
            outflows: vec![
                holding_to(&lp(), &reg(), "USDCx", "150"),
                holding_to(&fee(), &dso(), "Amulet", "1.5"),
                holding_to(&lp(), &dso(), "Amulet", "500"),
            ],
            inflows: vec![
                held("c1", &reg(), "USDCx", "50"),
                held("p1", &dso(), "Amulet", "998.5"),
            ],
            consumed: vec![held("u1", &reg(), "USDCx", "200")],
            atomic_settle: Some(settle(QuoteSide::Buy, "1000", "150", &["u1"])),
            ..selling_cc()
        }
    }

    fn sell_cc() -> CommandOp {
        CommandOp::Swap {
            sell: cc(),
            sell_amount: amt("1000"),
            buy: usdc(),
            min_receive: amt("150"),
            max_fee_cc: amt("2"),
        }
    }

    fn buy_cc(min: &str) -> CommandOp {
        CommandOp::Swap {
            sell: usdc(),
            sell_amount: amt("150"),
            buy: cc(),
            min_receive: amt(min),
            max_fee_cc: amt("2"),
        }
    }

    fn pay_alice() -> CommandOp {
        CommandOp::TransferCc {
            receiver: p(&alice()),
            amount: amt("10"),
            description: None,
            settlement_proposal_id: None,
        }
    }

    /// `op` directed at the chain's head, issued now and valid for 600 s.
    fn command_at(chain: &Chain, op: CommandOp) -> Command {
        let head = chain.state.head().unwrap();
        Command {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            command_id: Uuid7::parse(COMMAND_ID).unwrap(),
            log_seq: head.seq,
            log_head: head.hash,
            issued_at: now(),
            expires_at: now().checked_add_secs(600).unwrap(),
            op,
            summary: Text::parse("Directed").unwrap(),
        }
    }

    fn signed(chain: &Chain, op: CommandOp) -> (Vec<u8>, ProofEnvelope) {
        let bytes = command_at(chain, op).encode().unwrap();
        let envelope = testkit::passkey_envelope(&bytes);
        (bytes, envelope)
    }

    struct Env {
        agent: PartyId,
        cc: InstrumentRef,
        platform: PlatformParties,
        usage: UsageTotals,
    }

    impl Env {
        fn new() -> Self {
            Self {
                agent: testkit::agent_party(),
                cc: cc(),
                platform: PlatformParties {
                    fee: Some(p(&fee())),
                    prepaid_traffic: None,
                    traffic_fee: None,
                },
                usage: UsageTotals::new(),
            }
        }

        fn ctx(&self) -> EvalContext<'_> {
            EvalContext {
                agent_party: &self.agent,
                cc: &self.cc,
                platform: &self.platform,
                now: now(),
                usage: &self.usage,
            }
        }
    }

    fn matches(facts: &TxFacts, op: CommandOp) -> bool {
        let env = Env::new();
        let intent = swap_intent(facts, &env.agent, Coverage::Fields).unwrap();
        command_matches(&command_at(&Chain::linked(), op), &intent, &env.ctx())
    }

    /// DvP terms that bind the agent, as a transaction that plants them carries them.
    fn planted_dvp() -> DvpTerms {
        let quantity = |admin: String, id: &str| Quantity {
            instrument_admin: admin,
            instrument_id: id.into(),
            amount: dec("1000000"),
        };
        DvpTerms {
            proposal_id: Some("p".into()),
            buyer: testkit::agent_party().to_string(),
            seller: key("mallory", 0xf2),
            delivery: quantity(reg(), "USDCx"),
            payment: quantity(dso(), "Amulet"),
        }
    }

    #[test]
    fn dvp_terms_refuse_a_directed_swap_or_transfer() {
        let agent = testkit::agent_party();
        let mut facts = selling_cc();
        facts.dvp = vec![planted_dvp()];
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::UnexpectedDvp)
        );
        let facts = TxFacts {
            outflows: vec![holding_to(&alice(), &dso(), "Amulet", "10")],
            dvp: vec![planted_dvp()],
            ..TxFacts::default()
        };
        for kind in [OperationKind::TransferCc, OperationKind::TransferCip56] {
            assert_eq!(
                transfer_intent(&facts, kind, Coverage::Fields),
                Err(BindingError::UnexpectedDvp)
            );
        }
    }

    #[test]
    fn both_sides_give_the_agents_legs_fees_expiry_and_net() {
        let agent = testkit::agent_party();
        let fee_leg = SettleFee {
            receiver: p(&fee()),
            instrument: cc(),
            amount: amt("1.5"),
        };
        let expiry = now().checked_add_secs(300).unwrap();
        assert_eq!(
            settle_terms(&selling_cc(), &agent).unwrap(),
            SettleTerms {
                lp: p(&lp()),
                sent: Leg {
                    instrument: cc(),
                    amount: amt("1000"),
                },
                received: Leg {
                    instrument: usdc(),
                    amount: amt("151"),
                },
                net_received: amt("151"),
                fees: vec![fee_leg.clone()],
                valid_until: expiry,
            }
        );
        assert_eq!(
            settle_terms(&buying_cc(), &agent).unwrap(),
            SettleTerms {
                lp: p(&lp()),
                sent: Leg {
                    instrument: usdc(),
                    amount: amt("150"),
                },
                received: Leg {
                    instrument: cc(),
                    amount: amt("1000"),
                },
                net_received: amt("998.5"),
                fees: vec![fee_leg],
                valid_until: expiry,
            }
        );
        let mut late = selling_cc();
        late.atomic_settle.as_mut().unwrap().valid_until_micros += 1;
        assert_eq!(
            settle_terms(&late, &agent).unwrap().valid_until,
            now().checked_add_secs(301).unwrap()
        );
        assert!(matches(&selling_cc(), sell_cc()));
        assert!(matches(&buying_cc(), buy_cc("998.5")));
    }

    #[test]
    fn a_swap_intent_leaves_out_only_the_lps_change() {
        let env = Env::new();
        let intent = swap_intent(&selling_cc(), &env.agent, Coverage::Fields).unwrap();
        let to = |party: &str| Some(Recipient::Party(p(party)));
        assert_eq!(
            intent.outgoing,
            vec![
                Flow {
                    instrument: cc(),
                    amount: amt("1000"),
                    to: to(&lp()),
                },
                Flow {
                    instrument: cc(),
                    amount: amt("1.5"),
                    to: to(&fee()),
                },
            ]
        );
        assert_eq!(intent.kind, OperationKind::AtomicDvpSettle);
        assert_eq!(intent.instruments, vec![cc(), usdc()]);
        assert_eq!(intent.market, Some(MarketId::parse("CC-USDCx").unwrap()));
        assert_eq!(intent.fee_total_cc, None);
        assert!(intent.sub.is_empty());
        assert_eq!(intent.coverage, Coverage::Fields);
        let unverified = swap_intent(&selling_cc(), &env.agent, Coverage::Unverified).unwrap();
        assert_eq!(unverified.coverage, Coverage::Unverified);
        // agent-funded CC to the LP while buying USDCx is no change: it stays, and refuses
        let mut facts = selling_cc();
        facts
            .outflows
            .push(holding_to(&lp(), &dso(), "Amulet", "5"));
        let intent = swap_intent(&facts, &env.agent, Coverage::Fields).unwrap();
        assert_eq!(intent.outgoing.len(), 3);
        assert!(!matches(&facts, sell_cc()));
        // so does the received instrument to anyone but the LP
        let mut facts = selling_cc();
        facts
            .outflows
            .push(holding_to(&alice(), &reg(), "USDCx", "1"));
        assert!(!matches(&facts, sell_cc()));
        // the fee party as the LP would merge the sold leg into the fee: refused
        let mut facts = selling_cc();
        facts
            .atomic_settle
            .as_mut()
            .unwrap()
            .venue
            .as_mut()
            .unwrap()
            .lp = fee();
        facts.outflows[0].to = fee();
        facts.outflows[2].to = fee();
        assert!(!matches(&facts, sell_cc()));
    }

    #[test]
    fn buying_cc_nets_the_fee_out_of_the_proceeds() {
        let env = Env::new();
        let intent = swap_intent(&buying_cc(), &env.agent, Coverage::Fields).unwrap();
        assert_eq!(intent.outgoing.len(), 2);
        assert!(intent.outgoing.iter().all(|flow| flow.amount != amt("500")));
        assert!(matches(&buying_cc(), buy_cc("998.5")));
        assert!(!matches(&buying_cc(), buy_cc("998.6")));
        // a fee above the command's cap refuses
        let mut facts = buying_cc();
        facts.atomic_settle.as_mut().unwrap().fees[0].amount = dec("2.5");
        facts.outflows[1].amount = dec("2.5");
        facts.inflows[1].amount = dec("997.5");
        assert!(!matches(&facts, buy_cc("990")));
    }

    #[test]
    fn the_fee_counts_once_against_the_fee_caps() {
        let env = Env::new();
        let ctx = env.ctx();
        let intent = swap_intent(&selling_cc(), &env.agent, Coverage::Fields).unwrap();
        let bytes = command_at(&Chain::linked(), sell_cc()).encode().unwrap();
        let outcome = ProofOutcome {
            sign_count: None,
            backup_eligible: false,
            backup_state: false,
        };
        let verified = VerifiedCommand::stub(&bytes, testkit::passkey_id(), outcome, now());
        let mut rules = DEFAULT_LINKED_RULES;
        rules.fees.cc_per_consent = Some(amt("1.5"));
        let decision = evaluate(&rules, &ctx, &intent, Some(&verified));
        assert!(decision.allowed, "{decision:?}");
        let fees: Vec<&UsageDelta> = decision
            .usage
            .iter()
            .filter(|u| u.bucket == UsageBucket::Fees)
            .collect();
        assert_eq!(
            fees,
            vec![&UsageDelta {
                bucket: UsageBucket::Fees,
                instrument: cc(),
                amount: amt("1.5"),
            }]
        );
        rules.fees.cc_per_consent = Some(amt("1.4"));
        assert_eq!(
            evaluate(&rules, &ctx, &intent, Some(&verified)).reason,
            ReasonCode::FeePerConsentCap
        );
    }

    #[test]
    fn a_lock_or_any_other_kind_is_never_dropped_even_to_the_lp() {
        let env = Env::new();
        for kind in [
            FlowKind::Lock,
            FlowKind::Allocation,
            FlowKind::PendingTransfer,
            FlowKind::Burn,
        ] {
            let mut facts = selling_cc();
            facts.outflows.push(out(&lp(), &reg(), "USDCx", "10", kind));
            let intent = swap_intent(&facts, &env.agent, Coverage::Fields).unwrap();
            let unknown = Flow {
                instrument: usdc(),
                amount: amt("10"),
                to: None,
            };
            assert_eq!(intent.outgoing.last(), Some(&unknown), "{kind:?}");
            assert!(!matches(&facts, sell_cc()), "{kind:?}");
        }
        // a lock in place of the sold leg is not the sold leg
        let mut facts = selling_cc();
        facts.outflows[0].kind = FlowKind::Lock;
        assert!(!matches(&facts, sell_cc()));
        // several lock holders, or any recipient that is not a party, are unknown
        let mut facts = selling_cc();
        facts.outflows[1].to = format!("{},{}", fee(), lp());
        let intent = swap_intent(&facts, &env.agent, Coverage::Fields).unwrap();
        assert_eq!(intent.outgoing[1].to, None);
        assert!(!matches(&facts, sell_cc()));
    }

    #[test]
    fn when_buying_cc_the_agents_own_cc_to_the_lp_shrinks_the_net() {
        let env = Env::new();
        let mut facts = buying_cc();
        facts
            .outflows
            .push(holding_to(&lp(), &dso(), "Amulet", "100"));
        facts.consumed.push(held("a9", &dso(), "Amulet", "100"));
        let intent = swap_intent(&facts, &env.agent, Coverage::Fields).unwrap();
        // it looks like the LP's change, so it is left out of the flows...
        assert_eq!(intent.outgoing.len(), 2);
        // ...but the agent keeps 100 CC less, which the signed minimum bounds
        assert_eq!(intent.settle.unwrap().net_received, amt("898.5"));
        assert!(!matches(&facts, buy_cc("998.5")));
        facts.consumed.push(held("a10", &dso(), "Amulet", "900"));
        assert_eq!(
            swap_intent(&facts, &env.agent, Coverage::Fields),
            Err(BindingError::NegativeNet)
        );
    }

    #[test]
    fn a_locked_holding_of_the_agent_never_counts_and_refuses_a_swap() {
        let agent = testkit::agent_party();
        let mut facts = selling_cc();
        facts.inflows = vec![
            held("p1", &reg(), "USDCx", "100"),
            locked(held("p2", &reg(), "USDCx", "51")),
            held("c1", &dso(), "Amulet", "98.5"),
        ];
        assert_eq!(
            settle_terms(&facts, &agent).unwrap().net_received,
            amt("100")
        );
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::LockedInflow)
        );
        // a locked change of the sold instrument refuses too
        let mut facts = selling_cc();
        facts.inflows[1].locked = true;
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::LockedInflow)
        );
        // a consumed holding counts, locked or not
        let mut facts = selling_cc();
        facts
            .consumed
            .push(locked(held("p0", &reg(), "USDCx", "1")));
        assert_eq!(
            settle_terms(&facts, &agent).unwrap().net_received,
            amt("150")
        );
    }

    #[test]
    fn settle_terms_need_the_venue_and_the_agents_own_listed_inputs() {
        let agent = testkit::agent_party();
        let refused = |edit: fn(&mut TxFacts), error: BindingError| {
            let mut facts = selling_cc();
            edit(&mut facts);
            assert_eq!(settle_terms(&facts, &agent), Err(error), "{facts:?}");
            assert_eq!(swap_intent(&facts, &agent, Coverage::Fields), Err(error));
        };
        refused(
            |f| f.unlisted_inputs = vec!["x1".into()],
            BindingError::UnlistedInput,
        );
        refused(
            |f| {
                let s = f.atomic_settle.as_mut().unwrap();
                s.user_input_holding_cids.push("z9".into());
            },
            BindingError::ForeignInput,
        );
        refused(|f| f.consumed.clear(), BindingError::ForeignInput);
        refused(
            |f| f.atomic_settle.as_mut().unwrap().venue = None,
            BindingError::NoVenueTerms,
        );
        refused(|f| f.atomic_settle = None, BindingError::NoSettle);
        refused(
            |f| f.atomic_settle.as_mut().unwrap().user = key("eve", 0xf1),
            BindingError::WrongUser,
        );
        refused(
            |f| {
                let venue = f.atomic_settle.as_mut().unwrap().venue.as_mut().unwrap();
                venue.lp = "lp".into();
            },
            BindingError::BadParty,
        );
        refused(
            |f| {
                let venue = f.atomic_settle.as_mut().unwrap().venue.as_mut().unwrap();
                venue.quote.id = "US DC".into();
            },
            BindingError::BadInstrument,
        );
        refused(
            |f| f.atomic_settle.as_mut().unwrap().fees[0].receiver = "fee".into(),
            BindingError::BadParty,
        );
        refused(
            |f| f.atomic_settle.as_mut().unwrap().valid_until_micros = i64::MAX,
            BindingError::BadExpiry,
        );
        refused(
            |f| f.atomic_settle.as_mut().unwrap().valid_until_micros = -1,
            BindingError::BadExpiry,
        );
        let mut facts = selling_cc();
        facts.unmodelled = vec!["Foo.Bar:Baz".into()];
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::Unmodelled)
        );
        let mut facts = selling_cc();
        let venue = facts
            .atomic_settle
            .as_mut()
            .unwrap()
            .venue
            .as_mut()
            .unwrap();
        venue.pair_name = "CC USDCx".into();
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::BadMarket)
        );
    }

    #[test]
    fn amounts_convert_exactly_or_refuse() {
        let agent = testkit::agent_party();
        let mut facts = selling_cc();
        facts.atomic_settle.as_mut().unwrap().base_amount = dec("1000.00000000001");
        assert_eq!(
            settle_terms(&facts, &agent),
            Err(BindingError::InexactAmount)
        );
        let mut facts = selling_cc();
        facts.outflows[1].amount = dec("1.50000000001");
        assert_eq!(
            swap_intent(&facts, &agent, Coverage::Fields),
            Err(BindingError::InexactAmount)
        );
        let mut facts = selling_cc();
        facts.atomic_settle.as_mut().unwrap().fees[0].amount = dec("100000000000000000");
        assert_eq!(
            settle_terms(&facts, &agent),
            Err(BindingError::InexactAmount)
        );
        // trailing zeros are only a scale
        let mut facts = selling_cc();
        facts.atomic_settle.as_mut().unwrap().quote_amount = dec("151.0000000000");
        facts.inflows[0].amount = dec("151.00");
        let terms = settle_terms(&facts, &agent).unwrap();
        assert_eq!(terms.received.amount.to_string(), "151");
        assert_eq!(terms.net_received, amt("151"));
        // a sum that leaves the amount grammar refuses
        let mut facts = selling_cc();
        facts
            .inflows
            .push(held("p3", &reg(), "USDCx", "99999999999999999"));
        assert_eq!(
            settle_terms(&facts, &agent),
            Err(BindingError::AmountOverflow)
        );
    }

    #[test]
    fn a_transfer_intent_maps_every_outflow() {
        let env = Env::new();
        let facts = TxFacts {
            outflows: vec![
                holding_to(&alice(), &dso(), "Amulet", "10"),
                out(&alice(), &dso(), "Amulet", "3", FlowKind::PendingTransfer),
                out(&lp(), &dso(), "Amulet", "2", FlowKind::Lock),
                out(&lp(), &reg(), "USDCx", "1", FlowKind::Allocation),
                out("0x01", &reg(), "USDCx", "1", FlowKind::Burn),
            ],
            ..TxFacts::default()
        };
        let intent = transfer_intent(&facts, OperationKind::TransferCc, Coverage::Fields).unwrap();
        let flow = |instrument: InstrumentRef, value: &str, to: Option<&str>| Flow {
            instrument,
            amount: amt(value),
            to: to.map(|party| Recipient::Party(p(party))),
        };
        assert_eq!(
            intent.outgoing,
            vec![
                flow(cc(), "10", Some(&alice())),
                flow(cc(), "3", Some(&alice())),
                flow(cc(), "2", None),
                flow(usdc(), "1", None),
                flow(usdc(), "1", None),
            ]
        );
        assert_eq!(intent.instruments, vec![cc(), usdc()]);
        assert_eq!(intent.kind, OperationKind::TransferCc);
        assert!(intent.settle.is_none() && intent.market.is_none());
        for kind in [OperationKind::AtomicDvpSettle, OperationKind::UsdcxBurn] {
            assert_eq!(
                transfer_intent(&facts, kind, Coverage::Fields),
                Err(BindingError::WrongOperation)
            );
        }
        let unmodelled = TxFacts {
            unmodelled: vec!["Foo.Bar:Baz".into()],
            ..facts.clone()
        };
        assert_eq!(
            transfer_intent(&unmodelled, OperationKind::TransferCc, Coverage::Fields),
            Err(BindingError::Unmodelled)
        );
        // a holding or a pending transfer pays the receiver; a lock does not
        let pay = command_at(&Chain::linked(), pay_alice());
        for (kind, paid) in [
            (FlowKind::Holding, true),
            (FlowKind::PendingTransfer, true),
            (FlowKind::Lock, false),
            (FlowKind::Allocation, false),
        ] {
            let facts = TxFacts {
                outflows: vec![out(&alice(), &dso(), "Amulet", "10", kind)],
                ..TxFacts::default()
            };
            let intent =
                transfer_intent(&facts, OperationKind::TransferCc, Coverage::Fields).unwrap();
            assert_eq!(command_matches(&pay, &intent, &env.ctx()), paid, "{kind:?}");
        }
    }

    // The traffic top-up rule reads the memos and how the outflows are delivered
    #[test]
    fn a_transfer_intent_carries_the_memos_and_the_delivery() {
        let with = |kinds: &[FlowKind], memos: &[&str]| TxFacts {
            outflows: kinds.iter().map(|k| out(&alice(), &reg(), "USDCx", "1", *k)).collect(),
            memos: memos.iter().map(|m| m.to_string()).collect(),
            ..TxFacts::default()
        };
        let intent = |facts: &TxFacts| {
            transfer_intent(facts, OperationKind::TransferCip56, Coverage::Fields).unwrap()
        };
        let direct = intent(&with(&[FlowKind::Holding], &["agent-traffic:p-1"]));
        assert_eq!(direct.memos, vec!["agent-traffic:p-1".to_string()]);
        assert_eq!(direct.delivery, Some(Delivery::Direct));
        let mixed = with(&[FlowKind::Holding, FlowKind::PendingTransfer], &[]);
        assert_eq!(intent(&mixed).delivery, Some(Delivery::Pending));
        assert!(intent(&mixed).memos.is_empty());
        assert_eq!(intent(&with(&[FlowKind::Lock], &[])).delivery, None);
        assert_eq!(delivery_of(&TxFacts::default()), None);
    }

    #[test]
    fn the_root_names_are_the_facts_readers() {
        assert_eq!(SETTLE_CHOICE, tx_verifier::facts::SETTLE_CHOICE);
        assert_eq!(VENUE_TEMPLATE, tx_verifier::facts::VENUE_TEMPLATE);
        let facts = selling_cc();
        assert_eq!(
            roots(&facts),
            vec![RootExercise {
                template: VENUE_TEMPLATE,
                choice: SETTLE_CHOICE,
                contract_id: VENUE,
            }]
        );
        assert_eq!(
            check_directed_roots(&sell_cc(), &roots(&facts), facts.root_nodes, Some(VENUE)),
            Ok(())
        );
    }

    fn scoped_to(lps: Vec<PartyId>) -> Chain {
        let mut chain = Chain::linked();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.scopes = vec![CommandScope {
            controller: testkit::passkey_id(),
            commands: vec![CommandKind::Swap],
            pairs: Vec::new(),
            lps,
        }];
        let doc = testkit::policy_doc(&chain.state, 1, now(), rules);
        chain.accept_passkey(&doc, now());
        chain
    }

    #[test]
    fn a_directed_swap_binds_end_to_end() {
        let env = Env::new();
        let ctx = env.ctx();
        let chain = Chain::linked();
        let (bytes, envelope) = signed(&chain, sell_cc());
        let bind = |facts: &TxFacts, venue: Option<&str>| {
            let tx = DirectedTx {
                facts,
                coverage: Coverage::Fields,
                venue_cid: venue,
            };
            authorize_directed_swap(&chain.state, now(), &bytes, &envelope, &tx, &ctx)
        };
        let done = bind(&selling_cc(), Some(VENUE)).unwrap();
        assert!(done.decision.allowed);
        assert_eq!(
            done.command.command().canton_command_id(),
            selling_cc().command_id
        );
        let refused = |edit: fn(&mut TxFacts), error: BindingError| {
            let mut facts = selling_cc();
            edit(&mut facts);
            assert_eq!(
                bind(&facts, Some(VENUE)),
                Err(BindingRefusal::Binding(error)),
                "{error:?}"
            );
        };
        refused(
            |f| f.command_id = "atomic-swap-1".into(),
            BindingError::CommandIdMismatch,
        );
        refused(|f| f.command_id.clear(), BindingError::CommandIdMismatch);
        refused(|f| f.root_nodes = 2, BindingError::RootCount);
        refused(
            |f| {
                f.roots.push(f.roots[0].clone());
                f.root_nodes = 2;
            },
            BindingError::RootCount,
        );
        refused(
            |f| f.roots[0].choice = "AtomicDVP_Retire".into(),
            BindingError::UnexpectedRoot,
        );
        refused(
            |f| f.roots[0].contract_id = "venue-2".into(),
            BindingError::WrongVenue,
        );
        refused(
            |f| f.atomic_settle.as_mut().unwrap().venue_cid = "venue-2".into(),
            BindingError::WrongVenue,
        );
        refused(|f| f.act_as.push(key("eve", 0xf1)), BindingError::ActAs);
        refused(|f| f.act_as.clear(), BindingError::ActAs);
        refused(
            |f| f.unmodelled = vec!["Foo.Bar:Baz".into()],
            BindingError::Unmodelled,
        );
        refused(
            |f| f.unlisted_inputs = vec!["x1".into()],
            BindingError::UnlistedInput,
        );
        refused(|f| f.inflows[0].locked = true, BindingError::LockedInflow);
        refused(|f| f.dvp = vec![planted_dvp()], BindingError::UnexpectedDvp);
        assert_eq!(
            bind(&selling_cc(), Some("venue-2")),
            Err(BindingRefusal::Binding(BindingError::WrongVenue))
        );
        assert_eq!(
            bind(&selling_cc(), None),
            Err(BindingRefusal::Binding(BindingError::NoVenue))
        );
        // content that is not the command's swap is refused by the rules under the command
        let mut facts = selling_cc();
        facts.outflows[0].amount = dec("999");
        let refusal = bind(&facts, Some(VENUE)).unwrap_err();
        assert!(
            matches!(&refusal, BindingRefusal::Directed(DirectedRefusal::Policy(d))
                if d.reason == ReasonCode::CommandMismatch),
            "{refusal:?}"
        );
        assert_eq!(refusal.code_name(), "command_mismatch");
        // an LP the signer's scope does not list
        let other = scoped_to(vec![p(&alice())]);
        let (scoped_bytes, scoped_envelope) = signed(&other, sell_cc());
        let facts = selling_cc();
        let tx = DirectedTx {
            facts: &facts,
            coverage: Coverage::Fields,
            venue_cid: Some(VENUE),
        };
        assert_eq!(
            authorize_directed_swap(
                &other.state,
                now(),
                &scoped_bytes,
                &scoped_envelope,
                &tx,
                &ctx
            ),
            Err(BindingRefusal::Directed(DirectedRefusal::Command(
                CommandError::LpNotAllowed
            )))
        );
        let listed = scoped_to(vec![p(&lp())]);
        let (listed_bytes, listed_envelope) = signed(&listed, sell_cc());
        assert!(
            authorize_directed_swap(
                &listed.state,
                now(),
                &listed_bytes,
                &listed_envelope,
                &tx,
                &ctx
            )
            .is_ok()
        );
        // a transfer command, an undecodable one, another agent's context
        let (pay_bytes, pay_envelope) = signed(&chain, pay_alice());
        assert_eq!(
            authorize_directed_swap(&chain.state, now(), &pay_bytes, &pay_envelope, &tx, &ctx),
            Err(BindingRefusal::Binding(BindingError::WrongOperation))
        );
        assert!(matches!(
            authorize_directed_swap(&chain.state, now(), b"scheme=x\n", &envelope, &tx, &ctx),
            Err(BindingRefusal::Directed(DirectedRefusal::Command(
                CommandError::Canon(_)
            )))
        ));
        let eve = p(&key("eve", 0xf1));
        let elsewhere = EvalContext {
            agent_party: &eve,
            ..ctx
        };
        assert_eq!(
            authorize_directed_swap(&chain.state, now(), &bytes, &envelope, &tx, &elsewhere),
            Err(BindingRefusal::Binding(BindingError::WrongAgent))
        );
    }

    #[test]
    fn a_market_list_refuses_a_directed_swap_on_a_venue_named_after_a_listed_market() {
        let env = Env::new();
        let ctx = env.ctx();
        let mut chain = Chain::linked();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("CC-USDCx").unwrap()],
        };
        let doc = testkit::policy_doc(&chain.state, 1, now(), rules);
        chain.accept_passkey(&doc, now());
        let (bytes, envelope) = signed(&chain, sell_cc());
        let facts = selling_cc();
        let tx = DirectedTx {
            facts: &facts,
            coverage: Coverage::Fields,
            venue_cid: Some(VENUE),
        };
        let refusal =
            authorize_directed_swap(&chain.state, now(), &bytes, &envelope, &tx, &ctx).unwrap_err();
        assert!(
            matches!(&refusal, BindingRefusal::Directed(DirectedRefusal::Policy(d))
                if d.reason == ReasonCode::MarketNotAllowed),
            "{refusal:?}"
        );
        assert_eq!(refusal.code_name(), "market_not_allowed");
        // under the default `any` list the same swap binds
        let unlisted = Chain::linked();
        let (bytes, envelope) = signed(&unlisted, sell_cc());
        assert!(
            authorize_directed_swap(&unlisted.state, now(), &bytes, &envelope, &tx, &ctx).is_ok()
        );
    }

    #[test]
    fn a_directed_transfer_binds_end_to_end() {
        let env = Env::new();
        let ctx = env.ctx();
        let chain = Chain::linked();
        let (bytes, envelope) = signed(&chain, pay_alice());
        let paid = TxFacts {
            act_as: vec![env.agent.to_string()],
            command_id: format!("ctl-{COMMAND_ID}"),
            roots: vec![ExerciseFact {
                template: "Splice.ExternalPartyAmuletRules:ExternalPartyAmuletRules".into(),
                choice: TRANSFER_CHOICE.into(),
                contract_id: "rules-1".into(),
            }],
            root_nodes: 1,
            outflows: vec![holding_to(&alice(), &dso(), "Amulet", "10")],
            inflows: vec![held("c1", &dso(), "Amulet", "2.5")],
            consumed: vec![held("a1", &dso(), "Amulet", "12.5")],
            ..TxFacts::default()
        };
        let bind = |facts: &TxFacts, bytes: &[u8], envelope: &ProofEnvelope| {
            let tx = DirectedTx {
                facts,
                coverage: Coverage::Fields,
                venue_cid: None,
            };
            authorize_directed_transfer(&chain.state, now(), bytes, envelope, &tx, &ctx)
        };
        let done = bind(&paid, &bytes, &envelope).unwrap();
        assert_eq!(
            done.decision.usage,
            vec![UsageDelta {
                bucket: UsageBucket::Transfers,
                instrument: cc(),
                amount: amt("10"),
            }]
        );
        let refused = |edit: fn(&mut TxFacts)| {
            let mut facts = paid.clone();
            edit(&mut facts);
            bind(&facts, &bytes, &envelope).unwrap_err()
        };
        let binding = |error| BindingRefusal::Binding(error);
        assert_eq!(
            refused(|f| f.command_id = "send-transfer-1".into()),
            binding(BindingError::CommandIdMismatch)
        );
        assert_eq!(
            refused(|f| f.root_nodes = 2),
            binding(BindingError::RootCount)
        );
        assert_eq!(
            refused(|f| f.roots[0].choice = "AmuletRules_Transfer".into()),
            binding(BindingError::UnexpectedRoot)
        );
        assert_eq!(
            refused(|f| f.roots.clear()),
            binding(BindingError::RootNotExercise)
        );
        assert_eq!(
            refused(|f| f.dvp = vec![planted_dvp()]),
            binding(BindingError::UnexpectedDvp)
        );
        for edit in [
            (|f: &mut TxFacts| f.outflows.push(holding_to(&lp(), &dso(), "Amulet", "1")))
                as fn(&mut TxFacts),
            |f| f.outflows[0].kind = FlowKind::Lock,
            |f| f.outflows[0].amount = dec("10.1"),
        ] {
            let refusal = refused(edit);
            assert!(
                matches!(&refusal, BindingRefusal::Directed(DirectedRefusal::Policy(d))
                    if d.reason == ReasonCode::CommandMismatch),
                "{refusal:?}"
            );
        }
        let (swap_bytes, swap_envelope) = signed(&chain, sell_cc());
        assert_eq!(
            bind(&paid, &swap_bytes, &swap_envelope),
            Err(binding(BindingError::WrongOperation))
        );
        let burn = CommandOp::UsdcxBurn {
            amount: amt("10"),
            destination: EthAddress([1; 20]),
        };
        let (burn_bytes, burn_envelope) = signed(&chain, burn);
        assert_eq!(
            bind(&paid, &burn_bytes, &burn_envelope),
            Err(binding(BindingError::NotExecuted))
        );
    }

    /// Directed swaps bound from prepared transaction bytes, read as every enforcer reads them.
    mod from_bytes {
        use super::*;
        use prost::Message;
        use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2;
        use tx_verifier::facts::extract_facts;
        use v2::interactive::metadata::input_contract::Contract as InputOneof;
        use v2::interactive::metadata::{InputContract, SubmitterInfo};
        use v2::interactive::transaction::v1::{Create, Exercise, Node as V1Node, node::NodeType};
        use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction, daml_transaction};
        use v2::value::Sum;
        use v2::{Identifier, Record, RecordField, Value};

        const AMULET: &str = "Splice.Amulet:Amulet";
        const LOCKED_AMULET: &str = "Splice.Amulet:LockedAmulet";
        const HOLDING: &str = "Utility.Registry.Holding.V0.Holding:Holding";
        const CREDENTIAL: &str = "Utility.Credential.V0.Credential:Credential";

        fn val(sum: Sum) -> Value {
            Value { sum: Some(sum) }
        }

        fn party_value(s: &str) -> Value {
            val(Sum::Party(s.into()))
        }

        fn text_value(s: &str) -> Value {
            val(Sum::Text(s.into()))
        }

        fn numeric(s: &str) -> Value {
            val(Sum::Numeric(s.into()))
        }

        fn list(elements: Vec<Value>) -> Value {
            val(Sum::List(v2::List { elements }))
        }

        fn optional(inner: Option<Value>) -> Value {
            val(Sum::Optional(Box::new(v2::Optional {
                value: inner.map(Box::new),
            })))
        }

        fn record(fields: Vec<(&str, Value)>) -> Value {
            val(Sum::Record(Record {
                record_id: None,
                fields: fields
                    .into_iter()
                    .map(|(label, value)| RecordField {
                        label: label.into(),
                        value: Some(value),
                    })
                    .collect(),
            }))
        }

        fn instrument_value(admin: &str, id: &str) -> Value {
            record(vec![("admin", party_value(admin)), ("id", text_value(id))])
        }

        fn ident(template: &str) -> Identifier {
            let (module, entity) = template.split_once(':').unwrap();
            Identifier {
                package_id: "pkg".into(),
                module_name: module.into(),
                entity_name: entity.into(),
            }
        }

        fn create(template: &str, contract_id: &str, argument: Value) -> Create {
            Create {
                contract_id: contract_id.into(),
                template_id: Some(ident(template)),
                argument: Some(argument),
                ..Create::default()
            }
        }

        fn exercise(template: &str, contract_id: &str, choice: &str, consuming: bool) -> Exercise {
            Exercise {
                contract_id: contract_id.into(),
                template_id: Some(ident(template)),
                choice_id: choice.into(),
                consuming,
                ..Exercise::default()
            }
        }

        fn spent(template: &str, contract_id: &str) -> NodeType {
            NodeType::Exercise(exercise(template, contract_id, "Archive", true))
        }

        fn made(c: Create) -> NodeType {
            NodeType::Create(c)
        }

        fn amulet_value(owner: &str, amount: &str) -> Value {
            record(vec![
                ("dso", party_value(&dso())),
                ("owner", party_value(owner)),
                ("amount", record(vec![("initialAmount", numeric(amount))])),
            ])
        }

        fn coin(contract_id: &str, owner: &str, amount: &str) -> Create {
            create(AMULET, contract_id, amulet_value(owner, amount))
        }

        fn locked_coin(contract_id: &str, owner: &str, amount: &str) -> Create {
            let lock = record(vec![("holders", list(vec![party_value(&dso())]))]);
            let arg = record(vec![
                ("amulet", amulet_value(owner, amount)),
                ("lock", lock),
            ]);
            create(LOCKED_AMULET, contract_id, arg)
        }

        fn usdcx_instrument() -> Value {
            record(vec![
                ("source", party_value(&reg())),
                ("id", text_value("USDCx")),
                ("scheme", text_value("s")),
            ])
        }

        fn usdcx(contract_id: &str, owner: &str, amount: &str) -> Create {
            let arg = record(vec![
                ("registrar", party_value(&reg())),
                ("owner", party_value(owner)),
                ("instrument", usdcx_instrument()),
                ("amount", numeric(amount)),
            ]);
            create(HOLDING, contract_id, arg)
        }

        /// The receipt a registry that keeps result contracts creates for each transfer.
        fn executed(contract_id: &str, sender: &str, receiver: &str, amount: &str) -> Create {
            let transfer = record(vec![
                ("operator", party_value(&reg())),
                ("provider", party_value(&reg())),
                ("registrar", party_value(&reg())),
                ("sender", party_value(sender)),
                ("receiver", party_value(receiver)),
                ("instrumentIdentifier", usdcx_instrument()),
                ("amount", numeric(amount)),
                ("reference", text_value("")),
                (
                    "batch",
                    record(vec![("id", text_value("")), ("size", val(Sum::Int64(1)))]),
                ),
            ]);
            let arg = record(vec![
                ("transfer", transfer),
                ("senderLabel", text_value("")),
                ("receiverLabel", text_value("")),
                ("observers", optional(None)),
                ("operatorIsObserver", optional(Some(val(Sum::Bool(false))))),
            ]);
            create(
                "Utility.Registry.V0.Holding.Transfer:ExecutedTransfer",
                contract_id,
                arg,
            )
        }

        fn venue() -> Create {
            let arg = record(vec![
                ("provider", party_value(&reg())),
                ("lp", party_value(&lp())),
                ("pairName", text_value("CC-USDCx")),
                ("baseInstrumentId", instrument_value(&dso(), "Amulet")),
                ("quoteInstrumentId", instrument_value(&reg(), "USDCx")),
            ]);
            create(VENUE_TEMPLATE, VENUE, arg)
        }

        /// The agent's settle of a CC-USDCx quote on the venue with a 1.5 CC fee.
        fn settle_root(side: &str, base_amount: &str, quote_amount: &str, input: &str) -> Exercise {
            let side = val(Sum::Enum(v2::Enum {
                enum_id: None,
                constructor: side.into(),
            }));
            let valid_until = (now().unix_secs() + 300) * 1_000_000;
            let quote = record(vec![
                ("quoteId", text_value("q1")),
                ("ticketId", text_value("")),
                ("user", party_value(testkit::agent_party().as_str())),
                ("side", side),
                ("baseAmount", numeric(base_amount)),
                ("quoteAmount", numeric(quote_amount)),
                ("validUntilMicros", val(Sum::Int64(valid_until))),
            ]);
            let fee_leg = record(vec![
                ("receiver", party_value(&fee())),
                ("amount", numeric("1.5")),
                ("instrumentId", instrument_value(&dso(), "Amulet")),
            ]);
            Exercise {
                chosen_value: Some(record(vec![
                    ("quote", quote),
                    (
                        "lpInputHoldingCids",
                        list(vec![val(Sum::ContractId("l1".into()))]),
                    ),
                    (
                        "userInputHoldingCids",
                        list(vec![val(Sum::ContractId(input.into()))]),
                    ),
                    ("fees", optional(Some(list(vec![fee_leg])))),
                ])),
                ..exercise(VENUE_TEMPLATE, VENUE, SETTLE_CHOICE, false)
            }
        }

        /// One root, the settle, whose children are `steps` in order; submitted by the agent.
        fn settle_bytes(mut root: Exercise, steps: Vec<NodeType>, inputs: Vec<Create>) -> Vec<u8> {
            let node = |id: &str, node_type: NodeType| daml_transaction::Node {
                node_id: id.into(),
                versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node {
                    node_type: Some(node_type),
                })),
            };
            let ids: Vec<String> = (1..=steps.len()).map(|i| i.to_string()).collect();
            let mut nodes: Vec<_> = ids.iter().zip(steps).map(|(id, s)| node(id, s)).collect();
            root.children = ids;
            nodes.insert(0, node("0", NodeType::Exercise(root)));
            let input_contracts = inputs
                .into_iter()
                .map(|c| InputContract {
                    contract: Some(InputOneof::V1(c)),
                    ..InputContract::default()
                })
                .collect();
            PreparedTransaction {
                transaction: Some(DamlTransaction {
                    version: "2.1".into(),
                    roots: vec!["0".into()],
                    nodes,
                    node_seeds: Vec::new(),
                }),
                metadata: Some(Metadata {
                    submitter_info: Some(SubmitterInfo {
                        act_as: vec![testkit::agent_party().to_string()],
                        command_id: format!("ctl-{COMMAND_ID}"),
                    }),
                    input_contracts,
                    ..Metadata::default()
                }),
            }
            .encode_to_vec()
        }

        /// Binds the command `op` to the facts of `tx_bytes`.
        fn bind(op: CommandOp, tx_bytes: &[u8]) -> Result<DirectedAuthorization, BindingRefusal> {
            let env = Env::new();
            let chain = Chain::linked();
            let (bytes, envelope) = signed(&chain, op);
            let facts = extract_facts(tx_bytes, testkit::agent_party().as_str()).unwrap();
            let tx = DirectedTx {
                facts: &facts,
                coverage: Coverage::Fields,
                venue_cid: Some(VENUE),
            };
            authorize_directed_swap(&chain.state, now(), &bytes, &envelope, &tx, &env.ctx())
        }

        /// Sells 1000 CC for 151 USDCx; the USDCx leg runs on a registry that keeps receipts
        /// and reads a credential.
        fn selling_cc_bytes(extra: Vec<NodeType>) -> Vec<u8> {
            let agent = testkit::agent_party().to_string();
            let mut steps = vec![
                spent(AMULET, "a1"),
                made(coin("c-lp", &lp(), "1000")),
                made(coin("c-fee", &fee(), "1.5")),
                made(coin("c-own", &agent, "98.5")),
                spent(HOLDING, "l1"),
                NodeType::Exercise(exercise(CREDENTIAL, "cred-1", "Credential_Get", false)),
                made(usdcx("h-own", &agent, "151")),
                made(usdcx("h-lp", &lp(), "49")),
                made(executed("e1", &lp(), &agent, "151")),
            ];
            steps.extend(extra);
            let inputs = vec![
                venue(),
                coin("a1", &agent, "1100"),
                usdcx("l1", &lp(), "200"),
            ];
            settle_bytes(settle_root("Sell", "1000", "151", "a1"), steps, inputs)
        }

        /// Buys 1000 CC for 150 USDCx; the agent keeps `own` CC after the fee, the LP keeps
        /// `lp_change`, and `extra` runs after both legs on the extra `inputs`.
        fn buying_cc_bytes(
            own: &str,
            lp_change: &str,
            extra: Vec<NodeType>,
            inputs: Vec<Create>,
        ) -> Vec<u8> {
            let agent = testkit::agent_party().to_string();
            let mut steps = vec![
                spent(HOLDING, "u1"),
                made(usdcx("h-lp", &lp(), "150")),
                made(usdcx("h-own", &agent, "50")),
                spent(AMULET, "l1"),
                made(coin("c-own", &agent, own)),
                made(coin("c-fee", &fee(), "1.5")),
                made(coin("c-lp", &lp(), lp_change)),
            ];
            steps.extend(extra);
            let mut all = vec![
                venue(),
                usdcx("u1", &agent, "200"),
                coin("l1", &lp(), "1500"),
            ];
            all.extend(inputs);
            settle_bytes(settle_root("Buy", "1000", "150", "u1"), steps, all)
        }

        #[test]
        fn a_settle_with_registry_receipts_and_a_credential_read_binds() {
            let agent = testkit::agent_party();
            let facts = extract_facts(&selling_cc_bytes(vec![]), agent.as_str()).unwrap();
            assert!(facts.unmodelled.is_empty(), "{facts:?}");
            assert_eq!(facts.outflows, selling_cc().outflows);
            let done = bind(sell_cc(), &selling_cc_bytes(vec![])).unwrap();
            assert!(done.decision.allowed, "{:?}", done.decision);
            // a template outside the model still refuses
            let thing = made(create("Some.Module:Thing", "t1", record(vec![])));
            assert_eq!(
                bind(sell_cc(), &selling_cc_bytes(vec![thing])).unwrap_err(),
                BindingRefusal::Binding(BindingError::Unmodelled)
            );
        }

        #[test]
        fn the_agents_own_locked_coin_counts_against_what_it_receives() {
            let agent = testkit::agent_party().to_string();
            let plain = buying_cc_bytes("998.5", "500", vec![], vec![]);
            assert!(bind(buy_cc("998.5"), &plain).unwrap().decision.allowed);
            // unlocked and paid to the LP, it looks like the LP's change but shrinks the net
            let paid = buying_cc_bytes(
                "998.5",
                "500",
                vec![
                    spent(LOCKED_AMULET, "k1"),
                    made(coin("c-k1", &lp(), "300000")),
                ],
                vec![locked_coin("k1", &agent, "300000")],
            );
            assert_eq!(
                bind(buy_cc("998.5"), &paid).unwrap_err(),
                BindingRefusal::Binding(BindingError::NegativeNet)
            );
            // unlocked back to the agent, it is no proceeds of the swap
            let back = buying_cc_bytes(
                "0.5",
                "1498",
                vec![
                    spent(LOCKED_AMULET, "k2"),
                    made(coin("c-k2", &agent, "300000")),
                ],
                vec![locked_coin("k2", &agent, "300000")],
            );
            let refusal = bind(buy_cc("998.5"), &back).unwrap_err();
            assert!(
                matches!(&refusal, BindingRefusal::Directed(DirectedRefusal::Policy(d))
                    if d.reason == ReasonCode::CommandMismatch),
                "{refusal:?}"
            );
        }
    }
}
