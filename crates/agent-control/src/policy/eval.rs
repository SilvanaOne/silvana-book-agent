//! The policy evaluator: one intent under the rules in force.

use std::collections::BTreeMap;

use crate::canon::time::UtcTime;
use crate::canon::values::{Amount, EthAddress, InstrumentRef, MarketId};
use crate::command::{
    CommandOp, VerifiedCommand, check_swap_bounds, check_swap_lp, command_matches, swap_lp_allowed,
    verify_command,
};
use crate::error::CommandError;
use crate::party::PartyId;
use crate::policy::kinds::{OperationClass, OperationKind};
use crate::policy::scopes::command_allowed;
use crate::policy::{Allowlist, CapValue, Limits, PolicyInstrument, Rules, Unlisted};
use crate::proof::ProofEnvelope;
use crate::state::ControlState;

/// Most operations inside one multicall.
pub const MAX_MULTICALL_OPS: usize = 32;
/// Length of the rolling usage window, in seconds.
pub const USAGE_WINDOW_SECS: i64 = 86_400;

/// Where a flow goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Recipient {
    Party(PartyId),
    Eth(EthAddress),
}

/// Value leaving the agent; `to` is `None` when the recipient is not known.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Flow {
    pub instrument: InstrumentRef,
    pub amount: Amount,
    pub to: Option<Recipient>,
}

/// Whether the flows were read from verified content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Coverage {
    Fields,
    Unverified,
}

impl Coverage {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Fields, Self::Fields) => Self::Fields,
            _ => Self::Unverified,
        }
    }
}

/// An instrument and an amount.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Leg {
    pub instrument: InstrumentRef,
    pub amount: Amount,
}

/// One fee an atomic settle makes the agent pay.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SettleFee {
    pub receiver: PartyId,
    pub instrument: InstrumentRef,
    pub amount: Amount,
}

/// The terms of an atomic settle for the agent, read from verified content.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SettleTerms {
    /// The venue's LP, the other side of both legs.
    pub lp: PartyId,
    /// What the agent delivers to the LP.
    pub sent: Leg,
    /// What the LP delivers to the agent, before any fee.
    pub received: Leg,
    /// The agent's holdings of the received instrument created, less those consumed.
    pub net_received: Amount,
    /// The fees the agent pays, in order.
    pub fees: Vec<SettleFee>,
    /// The quote's expiry, rounded up to the second.
    pub valid_until: UtcTime,
}

impl SettleTerms {
    /// A quote expiry in microseconds since the epoch, rounded up to the second.
    pub fn expiry_from_micros(micros: i64) -> Option<UtcTime> {
        let secs = micros.checked_div_euclid(1_000_000)?;
        let secs = if micros.checked_rem_euclid(1_000_000)? > 0 {
            secs.checked_add(1)?
        } else {
            secs
        };
        UtcTime::from_unix_secs(secs)
    }
}

/// What one signature would authorize.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Intent {
    pub kind: OperationKind,
    pub outgoing: Vec<Flow>,
    pub instruments: Vec<InstrumentRef>,
    pub market: Option<MarketId>,
    pub fee_total_cc: Option<Amount>,
    pub sub: Vec<Intent>,
    pub coverage: Coverage,
    /// The settle terms, for an atomic settle; a directed swap needs them.
    pub settle: Option<SettleTerms>,
    /// The memos the transaction carries, as read.
    pub memos: Vec<String>,
    /// How the outflows reach their recipients; `None` when it was not read.
    pub delivery: Option<Delivery>,
}

/// How a transfer's outflows reach their recipients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delivery {
    /// Every outflow is a holding of its recipient.
    Direct,
    /// An outflow waits for its recipient to accept it (a two-step offer).
    Pending,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Pending => "pending",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Direct, Self::Pending].into_iter().find(|d| d.as_str() == s)
    }
}

/// The memo prefix of a traffic purchase payment; the purchase id follows it.
pub const TRAFFIC_PURCHASE_MEMO_PREFIX: &str = "agent-traffic:";

/// A memo that names a traffic purchase: the prefix, then a non-blank id.
pub fn is_traffic_purchase_memo(memo: &str) -> bool {
    memo.strip_prefix(TRAFFIC_PURCHASE_MEMO_PREFIX)
        .is_some_and(|id| !id.is_empty() && !id.chars().any(char::is_whitespace))
}

impl Intent {
    /// An intent of `kind` with no flows, read from verified content.
    pub fn new(kind: OperationKind) -> Self {
        Self {
            kind,
            outgoing: Vec::new(),
            instruments: Vec::new(),
            market: None,
            fee_total_cc: None,
            sub: Vec::new(),
            coverage: Coverage::Fields,
            settle: None,
            memos: Vec::new(),
            delivery: None,
        }
    }
}

/// Platform parties pinned in the enforcer's own config, never taken from server data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlatformParties {
    pub fee: Option<PartyId>,
    pub prepaid_traffic: Option<PartyId>,
    /// Recipient of traffic purchases: a lone undirected CIP-56 transfer to it is maintenance.
    pub traffic_fee: Option<PartyId>,
}

impl PlatformParties {
    pub const NONE: Self = Self {
        fee: None,
        prepaid_traffic: None,
        traffic_fee: None,
    };

    pub fn contains(&self, party: &PartyId) -> bool {
        self.fee.as_ref() == Some(party) || self.prepaid_traffic.as_ref() == Some(party)
    }
}

/// Everything the evaluator needs besides the rules and the intent.
#[derive(Clone, Copy, Debug)]
pub struct EvalContext<'a> {
    pub agent_party: &'a PartyId,
    /// Canton Coin, resolved from the enforcer's own DSO config.
    pub cc: &'a InstrumentRef,
    pub platform: &'a PlatformParties,
    pub now: UtcTime,
    pub usage: &'a UsageTotals,
}

/// A rolling daily total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageBucket {
    Traffic,
    Trading,
    Transfers,
    Fees,
}

impl UsageBucket {
    pub const ALL: [Self; 4] = [Self::Traffic, Self::Trading, Self::Transfers, Self::Fees];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Traffic => "traffic",
            Self::Trading => "trading",
            Self::Transfers => "transfers",
            Self::Fees => "fees",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|bucket| bucket.as_str() == s)
    }
}

/// One recorded use, at the recorder's clock.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UsageRecord {
    pub bucket: UsageBucket,
    pub instrument: InstrumentRef,
    pub amount: Amount,
    pub at: UtcTime,
}

/// A use to record once the operation succeeded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UsageDelta {
    pub bucket: UsageBucket,
    pub instrument: InstrumentRef,
    pub amount: Amount,
}

/// Window totals per bucket and instrument; an overflowed total stays overflowed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageTotals {
    sums: BTreeMap<(UsageBucket, InstrumentRef), Option<Amount>>,
}

impl UsageTotals {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when a record made at `at` counts at `now`; later records always count.
    pub fn in_window(at: UtcTime, now: UtcTime) -> bool {
        at.secs_until(now) < USAGE_WINDOW_SECS
    }

    /// Sums the records inside the rolling window ending at `now`.
    pub fn from_records(records: &[UsageRecord], now: UtcTime) -> Self {
        let mut totals = Self::new();
        for record in records.iter().filter(|r| Self::in_window(r.at, now)) {
            totals.insert(record.bucket, &record.instrument, record.amount);
        }
        totals
    }

    /// Adds an amount, for example a SQL window aggregate.
    pub fn insert(&mut self, bucket: UsageBucket, instrument: &InstrumentRef, amount: Amount) {
        let slot = self
            .sums
            .entry((bucket, instrument.clone()))
            .or_insert(Some(Amount::ZERO));
        *slot = slot.and_then(|sum| sum.checked_add(amount));
    }

    /// The window total, zero when nothing was used; `None` once it overflowed.
    pub fn get(&self, bucket: UsageBucket, instrument: &InstrumentRef) -> Option<Amount> {
        self.sums
            .get(&(bucket, instrument.clone()))
            .copied()
            .unwrap_or(Some(Amount::ZERO))
    }
}

/// Why an intent was allowed or refused; the numbers are stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReasonCode {
    Allowed,
    Paused,
    ClassDisabled,
    LpDisabled,
    DirectedOnly,
    CommandMismatch,
    RecipientNotAllowed,
    DestinationNotAllowed,
    InstrumentNotAllowed,
    MarketNotAllowed,
    PerOperationCap,
    DailyCap,
    UnlistedInstrument,
    FeePerConsentCap,
    FeeDailyCap,
    TrafficDailyCap,
    MalformedIntent,
    AmountOverflow,
    ContentUnverifiable,
    /// A package the transaction uses is outside the policy's package rule.
    PackageNotAllowed,
    /// The command's signer may not send it under these rules, or it was verified at
    /// another clock.
    CommandOutOfScope,
}

impl ReasonCode {
    pub const ALL: [Self; 21] = [
        Self::Allowed,
        Self::Paused,
        Self::ClassDisabled,
        Self::LpDisabled,
        Self::DirectedOnly,
        Self::CommandMismatch,
        Self::RecipientNotAllowed,
        Self::DestinationNotAllowed,
        Self::InstrumentNotAllowed,
        Self::MarketNotAllowed,
        Self::PerOperationCap,
        Self::DailyCap,
        Self::UnlistedInstrument,
        Self::FeePerConsentCap,
        Self::FeeDailyCap,
        Self::TrafficDailyCap,
        Self::MalformedIntent,
        Self::AmountOverflow,
        Self::ContentUnverifiable,
        Self::PackageNotAllowed,
        Self::CommandOutOfScope,
    ];

    pub fn as_u16(self) -> u16 {
        match self {
            Self::Allowed => 0,
            Self::Paused => 1,
            Self::ClassDisabled => 2,
            Self::LpDisabled => 3,
            Self::DirectedOnly => 4,
            Self::CommandMismatch => 5,
            Self::RecipientNotAllowed => 6,
            Self::DestinationNotAllowed => 7,
            Self::InstrumentNotAllowed => 8,
            Self::MarketNotAllowed => 9,
            Self::PerOperationCap => 10,
            Self::DailyCap => 11,
            Self::UnlistedInstrument => 12,
            Self::FeePerConsentCap => 13,
            Self::FeeDailyCap => 14,
            Self::TrafficDailyCap => 15,
            Self::MalformedIntent => 16,
            Self::AmountOverflow => 17,
            Self::ContentUnverifiable => 18,
            Self::PackageNotAllowed => 19,
            Self::CommandOutOfScope => 20,
        }
    }

    pub fn from_u16(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_u16() == code)
    }

    pub fn code_name(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Paused => "paused",
            Self::ClassDisabled => "class_disabled",
            Self::LpDisabled => "lp_disabled",
            Self::DirectedOnly => "directed_only",
            Self::CommandMismatch => "command_mismatch",
            Self::RecipientNotAllowed => "recipient_not_allowed",
            Self::DestinationNotAllowed => "destination_not_allowed",
            Self::InstrumentNotAllowed => "instrument_not_allowed",
            Self::MarketNotAllowed => "market_not_allowed",
            Self::PerOperationCap => "per_operation_cap",
            Self::DailyCap => "daily_cap",
            Self::UnlistedInstrument => "unlisted_instrument",
            Self::FeePerConsentCap => "fee_per_consent_cap",
            Self::FeeDailyCap => "fee_daily_cap",
            Self::TrafficDailyCap => "traffic_daily_cap",
            Self::MalformedIntent => "malformed_intent",
            Self::AmountOverflow => "amount_overflow",
            Self::ContentUnverifiable => "content_unverifiable",
            Self::PackageNotAllowed => "package_not_allowed",
            Self::CommandOutOfScope => "command_out_of_scope",
        }
    }
}

/// The verdict; `class` is the class of the rule that decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub reason: ReasonCode,
    pub class: OperationClass,
    /// To record after the operation succeeded; empty on a refusal.
    pub usage: Vec<UsageDelta>,
}

struct Refusal {
    reason: ReasonCode,
    class: OperationClass,
}

fn refuse(reason: ReasonCode, class: OperationClass) -> Refusal {
    Refusal { reason, class }
}

pub(crate) fn is_self(flow: &Flow, agent_party: &PartyId) -> bool {
    matches!(&flow.to, Some(Recipient::Party(party)) if party == agent_party)
}

fn is_fee_flow(flow: &Flow, ctx: &EvalContext<'_>) -> bool {
    flow.instrument == *ctx.cc
        && matches!(&flow.to, Some(Recipient::Party(party)) if ctx.platform.contains(party))
}

/// What a flow is to the class rules.
enum Role {
    Change,
    Fee,
    Outflow,
}

fn role(flow: &Flow, fees_apply: bool, ctx: &EvalContext<'_>) -> Role {
    if is_self(flow, ctx.agent_party) {
        Role::Change
    } else if fees_apply && is_fee_flow(flow, ctx) {
        Role::Fee
    } else {
        Role::Outflow
    }
}

fn maintenance_flow_explained(kind: OperationKind, flow: &Flow, ctx: &EvalContext<'_>) -> bool {
    if is_self(flow, ctx.agent_party) {
        return true;
    }
    kind == OperationKind::PrepayTraffic
        && flow.instrument == *ctx.cc
        && match &ctx.platform.prepaid_traffic {
            None => true,
            Some(prepaid) => matches!(&flow.to, Some(Recipient::Party(p)) if p == prepaid),
        }
}

fn all_fee_flows(part: &Intent, ctx: &EvalContext<'_>) -> bool {
    let mut any = false;
    for flow in &part.outgoing {
        match role(flow, true, ctx) {
            Role::Change => {}
            Role::Fee => any = true,
            Role::Outflow => return false,
        }
    }
    any
}

/// A direct CIP-56 transfer whose only outflow goes to the pinned traffic-fee party and
/// whose memos all name a traffic purchase.
fn is_traffic_top_up(part: &Intent, ctx: &EvalContext<'_>) -> bool {
    let Some(traffic_fee) = &ctx.platform.traffic_fee else {
        return false;
    };
    let mut outflows = part
        .outgoing
        .iter()
        .filter(|flow| !is_self(flow, ctx.agent_party));
    part.kind == OperationKind::TransferCip56
        && part.delivery == Some(Delivery::Direct)
        && !part.memos.is_empty()
        && part.memos.iter().all(|memo| is_traffic_purchase_memo(memo))
        && matches!(
            (outflows.next(), outflows.next()),
            (Some(Flow { to: Some(Recipient::Party(party)), .. }), None) if party == traffic_fee
        )
}

/// A CC traffic cap cannot count a maintenance outflow in another token, so it refuses it.
fn check_token_traffic(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    part: &Intent,
    class: OperationClass,
) -> Result<OperationClass, Refusal> {
    let token = |flow: &Flow| !is_self(flow, ctx.agent_party) && flow.instrument != *ctx.cc;
    if class == OperationClass::Maintenance
        && rules.maintenance.traffic_cc_per_day.is_some()
        && part.outgoing.iter().any(token)
    {
        return Err(refuse(ReasonCode::TrafficDailyCap, class));
    }
    Ok(class)
}

/// The class after the self-transfer, platform-party, traffic top-up and unexplained-flow
/// rules; a command's operation decides a directed part.
fn part_class(
    part: &Intent,
    coverage: Coverage,
    ctx: &EvalContext<'_>,
    command: Option<&VerifiedCommand>,
) -> OperationClass {
    if let Some(verified) = command {
        return verified.command().op.class();
    }
    let base = part.kind.class();
    match base {
        OperationClass::Maintenance => {
            let explained = part
                .outgoing
                .iter()
                .all(|flow| maintenance_flow_explained(part.kind, flow, ctx));
            if explained {
                base
            } else {
                OperationClass::Transfers
            }
        }
        OperationClass::Transfers if coverage == Coverage::Fields && !part.outgoing.is_empty() => {
            let to_self = part
                .outgoing
                .iter()
                .all(|flow| is_self(flow, ctx.agent_party));
            if part.kind.is_self_transferable() && to_self {
                OperationClass::Maintenance
            } else if all_fee_flows(part, ctx) {
                OperationClass::FeeConsents
            } else if is_traffic_top_up(part, ctx) {
                OperationClass::Maintenance
            } else {
                base
            }
        }
        _ => base,
    }
}

fn class_enabled(rules: &Rules, class: OperationClass) -> bool {
    match class {
        OperationClass::Maintenance => rules.maintenance.enabled,
        OperationClass::Trading => rules.trading.enabled,
        OperationClass::Transfers => rules.transfers.enabled,
        OperationClass::Locks => rules.locks.enabled,
        OperationClass::Usdcx => rules.usdcx.enabled,
        OperationClass::FeeConsents | OperationClass::Identity | OperationClass::Container => true,
    }
}

fn passes_pause(class: OperationClass, kind: OperationKind) -> bool {
    kind.is_unwind()
        || match class {
            OperationClass::Maintenance
            | OperationClass::FeeConsents
            | OperationClass::Identity => true,
            OperationClass::Trading
            | OperationClass::Transfers
            | OperationClass::Locks
            | OperationClass::Usdcx
            | OperationClass::Container => false,
        }
}

fn restricts(limits: &Limits) -> bool {
    limits.unlisted == Unlisted::Deny || !limits.items.is_empty()
}

/// True when the rules restrict something the class's content decides.
pub fn class_is_limited(rules: &Rules, class: OperationClass) -> bool {
    let fees = rules.fees.cc_per_consent.is_some() || rules.fees.cc_per_day.is_some();
    let trading = &rules.trading;
    let transfers = &rules.transfers;
    match class {
        OperationClass::Maintenance => rules.maintenance.traffic_cc_per_day.is_some(),
        OperationClass::Trading => {
            fees || !trading.markets.any
                || !trading.instruments.any
                || restricts(&trading.per_trade)
                || restricts(&trading.per_day)
        }
        OperationClass::Transfers => {
            fees || transfers.directed_only
                || !transfers.recipients.any
                || !transfers.eth_destinations.any
                || restricts(&transfers.per_transfer)
                || restricts(&transfers.per_day)
        }
        OperationClass::Locks => fees || restricts(&rules.locks.per_lock),
        OperationClass::Usdcx | OperationClass::FeeConsents => fees,
        OperationClass::Identity => false,
        OperationClass::Container => [
            OperationClass::Maintenance,
            OperationClass::Trading,
            OperationClass::Transfers,
            OperationClass::Locks,
            OperationClass::Usdcx,
            OperationClass::FeeConsents,
        ]
        .into_iter()
        .any(|inner| class_is_limited(rules, inner)),
    }
}

/// The cap of a resolved instrument: the lowest cap of every ref that names it.
fn cap_of(limits: &Limits, instrument: &InstrumentRef, cc: &InstrumentRef) -> CapValue {
    let token = limits.cap_for(&PolicyInstrument::Token(instrument.clone()));
    if instrument == cc {
        token.min(limits.cap_for(&PolicyInstrument::Cc))
    } else {
        token
    }
}

/// A resolved instrument is allowed when any ref that names it is listed.
fn instrument_allowed(
    list: &Allowlist<PolicyInstrument>,
    instrument: &InstrumentRef,
    cc: &InstrumentRef,
) -> bool {
    list.any
        || list
            .items
            .contains(&PolicyInstrument::Token(instrument.clone()))
        || (instrument == cc && list.items.contains(&PolicyInstrument::Cc))
}

/// Summed flows of every part, per category.
struct Tally {
    traffic: Amount,
    trading: BTreeMap<InstrumentRef, Amount>,
    transfers: BTreeMap<InstrumentRef, Amount>,
    locks: BTreeMap<InstrumentRef, Amount>,
    fees: Amount,
}

fn add(total: Amount, amount: Amount, class: OperationClass) -> Result<Amount, Refusal> {
    total
        .checked_add(amount)
        .ok_or(refuse(ReasonCode::AmountOverflow, class))
}

fn add_to(
    map: &mut BTreeMap<InstrumentRef, Amount>,
    flow: &Flow,
    class: OperationClass,
) -> Result<(), Refusal> {
    let slot = map.entry(flow.instrument.clone()).or_insert(Amount::ZERO);
    *slot = add(*slot, flow.amount, class)?;
    Ok(())
}

impl Tally {
    fn new() -> Self {
        Self {
            traffic: Amount::ZERO,
            trading: BTreeMap::new(),
            transfers: BTreeMap::new(),
            locks: BTreeMap::new(),
            fees: Amount::ZERO,
        }
    }

    fn add_fee(&mut self, amount: Amount, class: OperationClass) -> Result<(), Refusal> {
        self.fees = add(self.fees, amount, class)?;
        Ok(())
    }
}

/// Steps 3-5: the class, the pause and the class switches.
fn gates(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    part: &Intent,
    coverage: Coverage,
    command: Option<&VerifiedCommand>,
) -> Result<OperationClass, Refusal> {
    let class = part_class(part, coverage, ctx, command);
    if rules.paused && !passes_pause(class, part.kind) {
        return Err(refuse(ReasonCode::Paused, class));
    }
    if !part.kind.is_unwind() && !class_enabled(rules, class) {
        return Err(refuse(ReasonCode::ClassDisabled, class));
    }
    if part.kind.requires_lp() && !rules.trading.lp_enabled {
        return Err(refuse(ReasonCode::LpDisabled, class));
    }
    Ok(class)
}

/// Steps 3-6 for one part, adding its flows to the tally.
fn check_part(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    part: &Intent,
    coverage: Coverage,
    command: Option<&VerifiedCommand>,
    tally: &mut Tally,
) -> Result<OperationClass, Refusal> {
    let class = gates(rules, ctx, part, coverage, command)?;
    let deny = |reason: ReasonCode| refuse(reason, class);
    if coverage == Coverage::Unverified && (command.is_some() || class_is_limited(rules, class)) {
        return Err(deny(ReasonCode::ContentUnverifiable));
    }
    if let Some(fee) = part.fee_total_cc {
        tally.add_fee(fee, class)?;
    }
    let swap = command.is_some_and(|c| matches!(c.command().op, CommandOp::Swap { .. }));
    let fees_apply = coverage == Coverage::Fields && (command.is_none() || swap);
    match class {
        OperationClass::Identity | OperationClass::Container => {
            return Err(deny(ReasonCode::MalformedIntent));
        }
        OperationClass::FeeConsents if part.kind.class() == OperationClass::FeeConsents => {
            if !part.outgoing.is_empty() || part.fee_total_cc.is_none() {
                return Err(deny(ReasonCode::MalformedIntent));
            }
        }
        OperationClass::FeeConsents => {
            for flow in &part.outgoing {
                if let Role::Fee = role(flow, fees_apply, ctx) {
                    tally.add_fee(flow.amount, class)?;
                }
            }
        }
        OperationClass::Maintenance => {
            check_token_traffic(rules, ctx, part, class)?;
            for flow in &part.outgoing {
                if !is_self(flow, ctx.agent_party) && flow.instrument == *ctx.cc {
                    tally.traffic = add(tally.traffic, flow.amount, class)?;
                }
            }
        }
        OperationClass::Trading => {
            if let Some(verified) = command
                && !command_matches(verified.command(), part, ctx)
            {
                return Err(deny(ReasonCode::CommandMismatch));
            }
            let trading = &rules.trading;
            // A directed swap's market is only the venue's own name, so a market list refuses it.
            let unlisted = |market: &MarketId| !trading.markets.allows(market);
            if (swap && !trading.markets.any) || part.market.as_ref().is_some_and(unlisted) {
                return Err(deny(ReasonCode::MarketNotAllowed));
            }
            let listed = |instrument: &InstrumentRef| {
                instrument_allowed(&trading.instruments, instrument, ctx.cc)
            };
            if !part.instruments.iter().all(listed) {
                return Err(deny(ReasonCode::InstrumentNotAllowed));
            }
            for flow in &part.outgoing {
                match role(flow, fees_apply, ctx) {
                    Role::Change => {}
                    Role::Fee => tally.add_fee(flow.amount, class)?,
                    Role::Outflow if !listed(&flow.instrument) => {
                        return Err(deny(ReasonCode::InstrumentNotAllowed));
                    }
                    Role::Outflow => add_to(&mut tally.trading, flow, class)?,
                }
            }
        }
        OperationClass::Transfers => {
            let transfers = &rules.transfers;
            match command {
                Some(verified) if !command_matches(verified.command(), part, ctx) => {
                    return Err(deny(ReasonCode::CommandMismatch));
                }
                None if transfers.directed_only => return Err(deny(ReasonCode::DirectedOnly)),
                _ => {}
            }
            for flow in &part.outgoing {
                match role(flow, fees_apply, ctx) {
                    Role::Change => {}
                    Role::Fee => tally.add_fee(flow.amount, class)?,
                    Role::Outflow => {
                        let allowed = match &flow.to {
                            Some(Recipient::Party(party)) => transfers.recipients.allows(party),
                            Some(Recipient::Eth(address)) => {
                                if !transfers.eth_destinations.allows(address) {
                                    return Err(deny(ReasonCode::DestinationNotAllowed));
                                }
                                true
                            }
                            None => transfers.recipients.any && transfers.eth_destinations.any,
                        };
                        if !allowed {
                            return Err(deny(ReasonCode::RecipientNotAllowed));
                        }
                        add_to(&mut tally.transfers, flow, class)?;
                    }
                }
            }
        }
        OperationClass::Locks => {
            for flow in &part.outgoing {
                match role(flow, fees_apply, ctx) {
                    Role::Change => {}
                    Role::Fee => tally.add_fee(flow.amount, class)?,
                    Role::Outflow => add_to(&mut tally.locks, flow, class)?,
                }
            }
        }
        OperationClass::Usdcx => {
            for flow in &part.outgoing {
                if let Role::Fee = role(flow, fees_apply, ctx) {
                    tally.add_fee(flow.amount, class)?;
                }
            }
        }
    }
    Ok(class)
}

fn per_operation(cap: CapValue, amount: Amount, over: ReasonCode) -> Result<(), ReasonCode> {
    match cap {
        CapValue::Denied => Err(ReasonCode::UnlistedInstrument),
        CapValue::Amount(limit) if amount > limit => Err(over),
        CapValue::Amount(_) | CapValue::Unlimited => Ok(()),
    }
}

/// A zero amount never exceeds a daily cap, whatever was used before.
fn daily(
    cap: CapValue,
    used: Option<Amount>,
    amount: Amount,
    over: ReasonCode,
) -> Result<(), ReasonCode> {
    match cap {
        CapValue::Denied => Err(ReasonCode::UnlistedInstrument),
        CapValue::Unlimited => Ok(()),
        CapValue::Amount(_) if amount.is_zero() => Ok(()),
        CapValue::Amount(limit) => {
            let total = used
                .and_then(|used| used.checked_add(amount))
                .ok_or(ReasonCode::AmountOverflow)?;
            if total > limit { Err(over) } else { Ok(()) }
        }
    }
}

fn check_instrument_caps(
    sums: &BTreeMap<InstrumentRef, Amount>,
    per_operation_caps: &Limits,
    per_day_caps: Option<(&Limits, UsageBucket)>,
    ctx: &EvalContext<'_>,
    class: OperationClass,
) -> Result<(), Refusal> {
    for (instrument, amount) in sums {
        let cap = cap_of(per_operation_caps, instrument, ctx.cc);
        per_operation(cap, *amount, ReasonCode::PerOperationCap)
            .map_err(|reason| refuse(reason, class))?;
        if let Some((caps, bucket)) = per_day_caps {
            let cap = cap_of(caps, instrument, ctx.cc);
            daily(
                cap,
                ctx.usage.get(bucket, instrument),
                *amount,
                ReasonCode::DailyCap,
            )
            .map_err(|reason| refuse(reason, class))?;
        }
    }
    Ok(())
}

/// Step 6, aggregated over every part: the caps, then the usage to record.
fn check_caps(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    tally: &Tally,
) -> Result<Vec<UsageDelta>, Refusal> {
    let cc = ctx.cc;
    daily(
        CapValue::of_optional(rules.maintenance.traffic_cc_per_day),
        ctx.usage.get(UsageBucket::Traffic, cc),
        tally.traffic,
        ReasonCode::TrafficDailyCap,
    )
    .map_err(|reason| refuse(reason, OperationClass::Maintenance))?;
    check_instrument_caps(
        &tally.trading,
        &rules.trading.per_trade,
        Some((&rules.trading.per_day, UsageBucket::Trading)),
        ctx,
        OperationClass::Trading,
    )?;
    check_instrument_caps(
        &tally.transfers,
        &rules.transfers.per_transfer,
        Some((&rules.transfers.per_day, UsageBucket::Transfers)),
        ctx,
        OperationClass::Transfers,
    )?;
    check_instrument_caps(
        &tally.locks,
        &rules.locks.per_lock,
        None,
        ctx,
        OperationClass::Locks,
    )?;
    let fees = OperationClass::FeeConsents;
    per_operation(
        CapValue::of_optional(rules.fees.cc_per_consent),
        tally.fees,
        ReasonCode::FeePerConsentCap,
    )
    .map_err(|reason| refuse(reason, fees))?;
    daily(
        CapValue::of_optional(rules.fees.cc_per_day),
        ctx.usage.get(UsageBucket::Fees, cc),
        tally.fees,
        ReasonCode::FeeDailyCap,
    )
    .map_err(|reason| refuse(reason, fees))?;
    let mut usage = Vec::new();
    let mut push = |bucket: UsageBucket, instrument: &InstrumentRef, amount: Amount| {
        if !amount.is_zero() {
            usage.push(UsageDelta {
                bucket,
                instrument: instrument.clone(),
                amount,
            });
        }
    };
    push(UsageBucket::Traffic, cc, tally.traffic);
    for (instrument, amount) in &tally.trading {
        push(UsageBucket::Trading, instrument, *amount);
    }
    for (instrument, amount) in &tally.transfers {
        push(UsageBucket::Transfers, instrument, *amount);
    }
    push(UsageBucket::Fees, cc, tally.fees);
    Ok(usage)
}

fn container_shape(intent: &Intent) -> bool {
    (1..=MAX_MULTICALL_OPS).contains(&intent.sub.len())
        && intent.outgoing.is_empty()
        && intent
            .sub
            .iter()
            .all(|sub| sub.kind.is_multicall_sub() && sub.sub.is_empty())
}

fn identity_shape(intent: &Intent) -> bool {
    intent.outgoing.is_empty() && intent.sub.is_empty() && intent.fee_total_cc.is_none()
}

/// The signer's scope in `rules` lists the command's kind.
fn scope_lists(rules: &Rules, verified: &VerifiedCommand) -> bool {
    command_allowed(rules, verified.controller_id(), verified.command().op.command_kind())
}

/// The command was verified at this clock, and the signer's scope in `rules` still allows
/// it: its kind, a swap's signed bounds, and the LP the swap settles with.
fn in_scope(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    verified: &VerifiedCommand,
) -> bool {
    let op = &verified.command().op;
    let signer = verified.controller_id();
    let lp_allowed = match (op, &intent.settle) {
        (CommandOp::Swap { .. }, Some(settle)) => swap_lp_allowed(rules, signer, &settle.lp),
        _ => true,
    };
    verified.verified_at() == ctx.now
        && scope_lists(rules, verified)
        && check_swap_bounds(rules, signer, op, Some(ctx.cc)).is_ok()
        && lp_allowed
}

fn out_of_scope(verified: &VerifiedCommand) -> Refusal {
    refuse(ReasonCode::CommandOutOfScope, verified.command().op.class())
}

fn evaluate_inner(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    command: Option<&VerifiedCommand>,
) -> Result<(OperationClass, Vec<UsageDelta>), Refusal> {
    if let Some(verified) = command
        && !in_scope(rules, ctx, intent, verified)
    {
        return Err(out_of_scope(verified));
    }
    let static_class = intent.kind.class();
    let deny = |reason: ReasonCode| refuse(reason, static_class);
    match static_class {
        OperationClass::Identity => {
            if !identity_shape(intent) {
                return Err(deny(ReasonCode::MalformedIntent));
            }
            if command.is_some() {
                return Err(deny(ReasonCode::CommandMismatch));
            }
            Ok((static_class, Vec::new()))
        }
        OperationClass::Container => {
            if !container_shape(intent) {
                return Err(deny(ReasonCode::MalformedIntent));
            }
            if command.is_some() {
                return Err(deny(ReasonCode::CommandMismatch));
            }
            let mut tally = Tally::new();
            if let Some(fee) = intent.fee_total_cc {
                tally.add_fee(fee, static_class)?;
            }
            for sub in &intent.sub {
                let coverage = intent.coverage.and(sub.coverage);
                check_part(rules, ctx, sub, coverage, None, &mut tally)?;
            }
            let usage = check_caps(rules, ctx, &tally)?;
            Ok((static_class, usage))
        }
        OperationClass::Maintenance
        | OperationClass::Trading
        | OperationClass::Transfers
        | OperationClass::Locks
        | OperationClass::Usdcx
        | OperationClass::FeeConsents => {
            if !intent.sub.is_empty() || intent.kind.is_multicall_sub() {
                return Err(deny(ReasonCode::MalformedIntent));
            }
            let mut tally = Tally::new();
            let class = check_part(rules, ctx, intent, intent.coverage, command, &mut tally)?;
            let usage = check_caps(rules, ctx, &tally)?;
            Ok((class, usage))
        }
    }
}

/// Evaluates `intent` under `rules`; a verified command directs a transfer or a swap,
/// and only at the clock it was verified at.
pub fn evaluate(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    command: Option<&VerifiedCommand>,
) -> Decision {
    match evaluate_inner(rules, ctx, intent, command) {
        Ok((class, usage)) => Decision {
            allowed: true,
            reason: ReasonCode::Allowed,
            class,
            usage,
        },
        Err(Refusal { reason, class }) => Decision {
            allowed: false,
            reason,
            class,
            usage: Vec::new(),
        },
    }
}

fn decided(result: Result<OperationClass, Refusal>) -> Decision {
    match result {
        Ok(class) => Decision {
            allowed: true,
            reason: ReasonCode::Allowed,
            class,
            usage: Vec::new(),
        },
        Err(Refusal { reason, class }) => Decision {
            allowed: false,
            reason,
            class,
            usage: Vec::new(),
        },
    }
}

fn directed_only(
    rules: &Rules,
    class: OperationClass,
    directed: bool,
) -> Result<OperationClass, Refusal> {
    if class == OperationClass::Transfers && !directed && rules.transfers.directed_only {
        return Err(refuse(ReasonCode::DirectedOnly, class));
    }
    Ok(class)
}

/// A market list refuses every swap command, as in `check_part`.
fn swap_markets(
    rules: &Rules,
    class: OperationClass,
    command: Option<&VerifiedCommand>,
) -> Result<OperationClass, Refusal> {
    let swap = command.is_some_and(|c| matches!(c.command().op, CommandOp::Swap { .. }));
    if swap && !rules.trading.markets.any {
        return Err(refuse(ReasonCode::MarketNotAllowed, class));
    }
    Ok(class)
}

fn kind_gates(
    rules: &Rules,
    kind: OperationKind,
    command: Option<&VerifiedCommand>,
) -> Result<OperationClass, Refusal> {
    let class = command.map_or(kind.class(), |c| c.command().op.class());
    if rules.paused && !passes_pause(class, kind) {
        return Err(refuse(ReasonCode::Paused, class));
    }
    if !kind.is_unwind() && !class_enabled(rules, class) {
        return Err(refuse(ReasonCode::ClassDisabled, class));
    }
    if kind.requires_lp() && !rules.trading.lp_enabled {
        return Err(refuse(ReasonCode::LpDisabled, class));
    }
    swap_markets(rules, class, command)?;
    directed_only(rules, class, command.is_some())
}

/// `command_mismatch` when a command directs another operation kind.
fn kind_mismatch(kind: OperationKind, command: Option<&VerifiedCommand>) -> Option<Refusal> {
    command
        .filter(|c| c.command().op.kind() != kind)
        .map(|c| refuse(ReasonCode::CommandMismatch, c.command().op.class()))
}

/// The class, pause, switch and directed-only checks of a kind, before content is known; a
/// command must direct `kind`, its signer's scope must list it, and a market list refuses a swap.
pub fn precheck(rules: &Rules, kind: OperationKind, command: Option<&VerifiedCommand>) -> Decision {
    if let Some(verified) = command
        && !scope_lists(rules, verified)
    {
        return decided(Err(out_of_scope(verified)));
    }
    let static_class = kind.class();
    let result = match static_class {
        OperationClass::Identity | OperationClass::Container if command.is_some() => {
            Err(refuse(ReasonCode::CommandMismatch, static_class))
        }
        OperationClass::Identity | OperationClass::Container => Ok(static_class),
        _ => match kind_mismatch(kind, command) {
            Some(refusal) => Err(refusal),
            None => kind_gates(rules, kind, command),
        },
    };
    decided(result)
}

/// As `precheck`, classifying by the flows the request names (taken as given), with the
/// full scope check of a command at the context's clock.
pub fn precheck_intent(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    command: Option<&VerifiedCommand>,
) -> Decision {
    if let Some(verified) = command
        && !in_scope(rules, ctx, intent, verified)
    {
        return decided(Err(out_of_scope(verified)));
    }
    let static_class = intent.kind.class();
    let result = match static_class {
        OperationClass::Identity if !identity_shape(intent) => {
            Err(refuse(ReasonCode::MalformedIntent, static_class))
        }
        OperationClass::Identity | OperationClass::Container if command.is_some() => {
            Err(refuse(ReasonCode::CommandMismatch, static_class))
        }
        OperationClass::Identity => Ok(static_class),
        OperationClass::Container if !container_shape(intent) => {
            Err(refuse(ReasonCode::MalformedIntent, static_class))
        }
        OperationClass::Container => intent
            .sub
            .iter()
            .try_for_each(|sub| {
                gates(rules, ctx, sub, Coverage::Fields, None)
                    .and_then(|class| directed_only(rules, class, false))
                    .and_then(|class| check_token_traffic(rules, ctx, sub, class))
                    .map(|_| ())
            })
            .map(|()| static_class),
        _ if intent.kind.is_multicall_sub() || !intent.sub.is_empty() => {
            Err(refuse(ReasonCode::MalformedIntent, static_class))
        }
        _ => match kind_mismatch(intent.kind, command) {
            Some(refusal) => Err(refusal),
            None => gates(rules, ctx, intent, Coverage::Fields, command)
                .and_then(|class| swap_markets(rules, class, command))
                .and_then(|class| directed_only(rules, class, command.is_some()))
                .and_then(|class| check_token_traffic(rules, ctx, intent, class)),
        },
    };
    decided(result)
}

/// A directed signature the rules in force allow: the command, verified at the clock
/// the rules were read at, and the decision with the usage to record once it succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectedAuthorization {
    pub command: VerifiedCommand,
    pub decision: Decision,
}

/// Why a directed signature was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectedRefusal {
    /// The command did not verify.
    Command(CommandError),
    /// The rules in force refused the intent under the command.
    Policy(Decision),
}

impl DirectedRefusal {
    /// The receipt code: the command error, or the evaluator reason.
    pub fn code(&self) -> u16 {
        match self {
            Self::Command(e) => e.code(),
            Self::Policy(decision) => decision.reason.as_u16(),
        }
    }

    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Command(e) => e.code_name(),
            Self::Policy(decision) => decision.reason.code_name(),
        }
    }
}

/// Verifies a command, checks a swap's LP against the signer's scope (`lp_not_allowed`),
/// then evaluates `intent` under it, all at `now` with the rules `state` has in force then.
pub fn authorize_directed(
    state: &ControlState,
    now: UtcTime,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    intent: &Intent,
    ctx: &EvalContext<'_>,
) -> Result<DirectedAuthorization, DirectedRefusal> {
    let command = verify_command(state, bytes, envelope, now, Some(ctx.cc))
        .map_err(DirectedRefusal::Command)?;
    let rules = state.rules_at(now).rules();
    if let Some(settle) = &intent.settle {
        check_swap_lp(rules, command.controller_id(), &command.command().op, &settle.lp)
            .map_err(DirectedRefusal::Command)?;
    }
    let ctx = EvalContext { now, ..*ctx };
    let decision = evaluate(rules, &ctx, intent, Some(&command));
    if decision.allowed {
        Ok(DirectedAuthorization { command, decision })
    } else {
        Err(DirectedRefusal::Policy(decision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{InstrumentId, Network, Text, Uuid7};
    use crate::command::{Command, CommandKind};
    use crate::controller::ControllerId;
    use crate::policy::{CommandScope, DEFAULT_LINKED_RULES, LimitItem, ScopePair};
    use crate::proof::ProofOutcome;
    use crate::testkit::{self, at};
    use OperationKind as K;
    use ReasonCode as R;

    fn party(hint: &str, key: u8) -> PartyId {
        PartyId::parse(&format!(
            "{hint}::{}",
            crate::party::namespace_for_key(&[key; 32])
        ))
        .unwrap()
    }

    fn instrument(admin: &PartyId, id: &str) -> InstrumentRef {
        InstrumentRef {
            admin: admin.clone(),
            id: InstrumentId::parse(id).unwrap(),
        }
    }

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    struct Fixture {
        agent: PartyId,
        cc: InstrumentRef,
        usdc: InstrumentRef,
        alice: PartyId,
        bob: PartyId,
        fee: PartyId,
        prepaid: PartyId,
        traffic_fee: PartyId,
        pinned: PlatformParties,
        none: PlatformParties,
        usage: UsageTotals,
    }

    impl Fixture {
        fn new() -> Self {
            let fee = party("fee", 7);
            let prepaid = party("prepaid", 8);
            let traffic_fee = party("traffic-fee", 9);
            Self {
                agent: testkit::agent_party(),
                cc: instrument(&party("DSO", 1), "Amulet"),
                usdc: instrument(&party("Bank", 2), "USDCx"),
                alice: party("alice", 3),
                bob: party("bob", 4),
                pinned: PlatformParties {
                    fee: Some(fee.clone()),
                    prepaid_traffic: Some(prepaid.clone()),
                    traffic_fee: Some(traffic_fee.clone()),
                },
                fee,
                prepaid,
                traffic_fee,
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

        fn command(&self, op: CommandOp) -> VerifiedCommand {
            let command = Command {
                network: Network::Devnet,
                agent_party: self.agent.clone(),
                command_id: crate::canon::values::Uuid7::parse(
                    "01890a5d-ac96-774b-bcce-b302099a8057",
                )
                .unwrap(),
                log_seq: 1,
                log_head: crate::canon::values::Hash32([1; 32]),
                issued_at: at(0),
                expires_at: at(600),
                op,
                summary: crate::canon::values::Text::parse("Pay alice").unwrap(),
            };
            let bytes = command.encode().unwrap();
            let outcome = ProofOutcome {
                sign_count: None,
                backup_eligible: false,
                backup_state: false,
            };
            VerifiedCommand::stub(&bytes, ControllerId([2; 32]), outcome, at(0))
        }

        /// Sell 1000 CC for at least 150 USDCx, paying at most 2 CC in fees.
        fn sell_cc(&self) -> CommandOp {
            CommandOp::Swap {
                sell: self.cc.clone(),
                sell_amount: amount("1000"),
                buy: self.usdc.clone(),
                min_receive: amount("150"),
                max_fee_cc: amount("2"),
            }
        }

        /// The settle `sell_cc` directs: 151 USDCx from bob, 1.5 CC to the fee party.
        fn swap_intent(&self) -> Intent {
            Intent {
                outgoing: vec![
                    self.flow(&self.cc, "1000", &self.bob),
                    self.flow(&self.cc, "1.5", &self.fee),
                    self.flow(&self.cc, "40", &self.agent),
                ],
                instruments: vec![self.cc.clone(), self.usdc.clone()],
                market: Some(MarketId::parse("CC/USDCx").unwrap()),
                settle: Some(SettleTerms {
                    lp: self.bob.clone(),
                    sent: Leg {
                        instrument: self.cc.clone(),
                        amount: amount("1000"),
                    },
                    received: Leg {
                        instrument: self.usdc.clone(),
                        amount: amount("151"),
                    },
                    net_received: amount("151"),
                    fees: vec![SettleFee {
                        receiver: self.fee.clone(),
                        instrument: self.cc.clone(),
                        amount: amount("1.5"),
                    }],
                    valid_until: at(300),
                }),
                ..Intent::new(K::AtomicDvpSettle)
            }
        }
    }

    fn with_flows(kind: OperationKind, flows: Vec<Flow>) -> Intent {
        Intent {
            outgoing: flows,
            ..Intent::new(kind)
        }
    }

    /// `intent` as a direct transfer that pays a traffic purchase.
    fn purchase(intent: Intent) -> Intent {
        Intent {
            memos: vec!["agent-traffic:p-1".into()],
            delivery: Some(Delivery::Direct),
            ..intent
        }
    }

    fn fee_consent(kind: OperationKind, total: &str) -> Intent {
        Intent {
            fee_total_cc: Some(amount(total)),
            ..Intent::new(kind)
        }
    }

    fn verdict(rules: &Rules, ctx: &EvalContext<'_>, intent: &Intent) -> ReasonCode {
        evaluate(rules, ctx, intent, None).reason
    }

    fn multicall(sub: Vec<Intent>) -> Intent {
        Intent {
            sub,
            ..Intent::new(K::ExecuteMultiCall)
        }
    }

    /// The smallest well-formed intent of `kind`; sub-operations sit in a container.
    fn minimal(kind: OperationKind) -> Intent {
        if kind.is_multicall_sub() {
            return multicall(vec![Intent::new(kind)]);
        }
        match kind.class() {
            OperationClass::FeeConsents => fee_consent(kind, "1"),
            OperationClass::Container => {
                multicall(vec![Intent::new(K::McAcceptTransferInstruction)])
            }
            _ => Intent::new(kind),
        }
    }

    #[test]
    fn the_defaults_allow_everything_but_undirected_transfers() {
        let f = Fixture::new();
        let ctx = f.ctx();
        for kind in K::ALL {
            let decision = evaluate(&DEFAULT_LINKED_RULES, &ctx, &minimal(kind), None);
            let expected = if kind.class() == OperationClass::Transfers {
                R::DirectedOnly
            } else {
                R::Allowed
            };
            assert_eq!(decision.reason, expected, "{kind}");
            assert_eq!(decision.allowed, expected == R::Allowed, "{kind}");
        }
    }

    #[test]
    fn a_cip56_transfer_to_self_is_maintenance() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let merge = with_flows(K::TransferCip56, vec![f.flow(&f.usdc, "5", &f.agent)]);
        let decision = evaluate(&DEFAULT_LINKED_RULES, &ctx, &merge, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Maintenance)
        );
        let mut paused = DEFAULT_LINKED_RULES;
        paused.paused = true;
        assert_eq!(verdict(&paused, &ctx, &merge), R::Allowed);
        let mixed = with_flows(
            K::TransferCip56,
            vec![
                f.flow(&f.usdc, "5", &f.agent),
                f.flow(&f.usdc, "1", &f.alice),
            ],
        );
        assert_eq!(
            verdict(&DEFAULT_LINKED_RULES, &ctx, &mixed),
            R::DirectedOnly
        );
        let unverified = Intent {
            coverage: Coverage::Unverified,
            ..merge.clone()
        };
        assert_eq!(
            verdict(&DEFAULT_LINKED_RULES, &ctx, &unverified),
            R::ContentUnverifiable
        );
        let in_multicall = multicall(vec![with_flows(
            K::McTransfer,
            vec![f.flow(&f.cc, "5", &f.agent)],
        )]);
        assert_eq!(
            verdict(&DEFAULT_LINKED_RULES, &ctx, &in_multicall),
            R::Allowed
        );
        assert_eq!(
            verdict(&DEFAULT_LINKED_RULES, &ctx, &Intent::new(K::McTransfer)),
            R::MalformedIntent
        );
        let cc_to_self = with_flows(K::TransferCc, vec![f.flow(&f.cc, "5", &f.agent)]);
        assert_eq!(
            verdict(&DEFAULT_LINKED_RULES, &ctx, &cc_to_self),
            R::DirectedOnly
        );
    }

    #[test]
    fn pause_lets_only_maintenance_fees_identity_and_unwinds_through() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.paused = true;
        rules.transfers.directed_only = false;
        for kind in K::ALL {
            let class = kind.class();
            let passes = kind.is_unwind()
                || matches!(
                    class,
                    OperationClass::Maintenance
                        | OperationClass::FeeConsents
                        | OperationClass::Identity
                        | OperationClass::Container
                );
            let expected = if passes { R::Allowed } else { R::Paused };
            assert_eq!(verdict(&rules, &ctx, &minimal(kind)), expected, "{kind}");
        }
        let batch = multicall(vec![Intent::new(K::McAllocate)]);
        assert_eq!(verdict(&rules, &ctx, &batch), R::Paused);
    }

    #[test]
    fn a_disabled_class_still_lets_unwinds_through() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.trading.enabled = false;
        rules.locks.enabled = false;
        rules.usdcx.enabled = false;
        rules.maintenance.enabled = false;
        rules.transfers.enabled = false;
        let cases = [
            (K::ProposeDvp, R::ClassDisabled),
            (K::CancelDvpProposal, R::Allowed),
            (K::McWithdrawAllocation, R::Allowed),
            (K::LockHoldings, R::ClassDisabled),
            (K::TerminateLock, R::Allowed),
            (K::UpdateVenueKey, R::Allowed),
            (K::UsdcxMint, R::ClassDisabled),
            (K::UsdcxReclaimHolding, R::Allowed),
            (K::SplitCc, R::ClassDisabled),
            (K::CancelTickets, R::Allowed),
            (K::TransferCc, R::ClassDisabled),
        ];
        for (kind, expected) in cases {
            assert_eq!(verdict(&rules, &ctx, &minimal(kind)), expected, "{kind}");
        }
        assert_eq!(
            verdict(&rules, &ctx, &fee_consent(K::TxFeesAuthorization, "1")),
            R::Allowed
        );
    }

    #[test]
    fn lp_enabled_gates_quotes_venues_and_tickets() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.trading.lp_enabled = false;
        for kind in K::ALL {
            let expected = if kind.requires_lp() {
                R::LpDisabled
            } else if kind.class() == OperationClass::Transfers {
                R::DirectedOnly
            } else {
                R::Allowed
            };
            assert_eq!(verdict(&rules, &ctx, &minimal(kind)), expected, "{kind}");
        }
        assert_eq!(
            [K::AtomicQuote, K::CreateAtomicDvpVenue, K::IssueTickets].map(|k| k.requires_lp()),
            [true; 3]
        );
    }

    #[test]
    fn directed_transfers_need_a_matching_command() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let pay = |value: &str, to: &PartyId| {
            with_flows(
                K::TransferCc,
                vec![f.flow(&f.cc, value, to), f.flow(&f.cc, "3", &f.agent)],
            )
        };
        let command = f.command(CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("10"),
            description: None,
            settlement_proposal_id: None,
        });
        assert_eq!(verdict(&rules, &ctx, &pay("10", &f.alice)), R::DirectedOnly);
        let allowed = evaluate(&rules, &ctx, &pay("10", &f.alice), Some(&command));
        assert!(allowed.allowed);
        assert_eq!(allowed.class, OperationClass::Transfers);
        assert_eq!(
            allowed.usage,
            vec![UsageDelta {
                bucket: UsageBucket::Transfers,
                instrument: f.cc.clone(),
                amount: amount("10"),
            }]
        );
        let mismatches = [
            pay("10.5", &f.alice),
            pay("10", &f.bob),
            with_flows(K::TransferCc, vec![f.flow(&f.usdc, "10", &f.alice)]),
            with_flows(K::TransferCip56, vec![f.flow(&f.cc, "10", &f.alice)]),
            with_flows(
                K::TransferCc,
                vec![f.flow(&f.cc, "10", &f.alice), f.flow(&f.cc, "1", &f.fee)],
            ),
            Intent::new(K::TransferCc),
        ];
        for intent in &mismatches {
            let decision = evaluate(&rules, &ctx, intent, Some(&command));
            assert_eq!(decision.reason, R::CommandMismatch, "{intent:?}");
        }
        let token = f.command(CommandOp::TransferToken {
            receiver: f.alice.clone(),
            instrument: f.usdc.clone(),
            amount: amount("2.5"),
            reference: None,
        });
        let cip56 = with_flows(K::TransferCip56, vec![f.flow(&f.usdc, "2.5", &f.alice)]);
        assert!(evaluate(&rules, &ctx, &cip56, Some(&token)).allowed);
        assert_eq!(
            evaluate(&rules, &ctx, &pay("10", &f.alice), Some(&token)).reason,
            R::CommandMismatch
        );
        let unverified = Intent {
            coverage: Coverage::Unverified,
            ..pay("10", &f.alice)
        };
        assert_eq!(
            evaluate(&rules, &ctx, &unverified, Some(&command)).reason,
            R::ContentUnverifiable
        );
        let batch = multicall(vec![with_flows(
            K::McTransfer,
            vec![f.flow(&f.cc, "10", &f.alice)],
        )]);
        assert_eq!(
            evaluate(&rules, &ctx, &batch, Some(&command)).reason,
            R::CommandMismatch
        );
        assert_eq!(
            evaluate(&rules, &ctx, &Intent::new(K::Jwt), Some(&command)).reason,
            R::CommandMismatch
        );
        let mut capped = rules.clone();
        capped.transfers.per_transfer.items = vec![LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount("5"),
        }];
        assert_eq!(
            evaluate(&capped, &ctx, &pay("10", &f.alice), Some(&command)).reason,
            R::PerOperationCap
        );
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(
            evaluate(&paused, &ctx, &pay("10", &f.alice), Some(&command)).reason,
            R::Paused
        );
    }

    #[test]
    fn a_directed_swap_is_judged_as_trading_on_its_sold_leg() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let swap = f.command(f.sell_cc());
        let intent = f.swap_intent();
        let rules = DEFAULT_LINKED_RULES;
        let decision = evaluate(&rules, &ctx, &intent, Some(&swap));
        assert!(decision.allowed, "{decision:?}");
        assert_eq!(decision.class, OperationClass::Trading);
        assert_eq!(
            decision.usage,
            vec![
                UsageDelta {
                    bucket: UsageBucket::Trading,
                    instrument: f.cc.clone(),
                    amount: amount("1000"),
                },
                UsageDelta {
                    bucket: UsageBucket::Fees,
                    instrument: f.cc.clone(),
                    amount: amount("1.5"),
                },
            ]
        );
        // Undirected, the same settle is an ordinary trade.
        assert!(evaluate(&rules, &ctx, &intent, None).allowed);
        let mut transfers_off = rules.clone();
        transfers_off.transfers.enabled = false;
        assert!(evaluate(&transfers_off, &ctx, &intent, Some(&swap)).allowed);
        let item = |value: &str| LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount(value),
        };
        let mut paused = rules.clone();
        paused.paused = true;
        let mut off = rules.clone();
        off.trading.enabled = false;
        let mut markets = rules.clone();
        markets.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("cETH/USDCx").unwrap()],
        };
        let mut instruments = rules.clone();
        instruments.trading.instruments = Allowlist {
            any: false,
            items: vec![PolicyInstrument::Token(f.usdc.clone())],
        };
        let mut per_trade = rules.clone();
        per_trade.trading.per_trade.items = vec![item("999.9999999999")];
        let mut per_trade_at = rules.clone();
        per_trade_at.trading.per_trade.items = vec![item("1000")];
        let mut per_day = rules.clone();
        per_day.trading.per_day.items = vec![item("1000")];
        let mut fee_cap = rules.clone();
        fee_cap.fees.cc_per_consent = Some(amount("1.4999999999"));
        let cases = [
            (&paused, R::Paused),
            (&off, R::ClassDisabled),
            (&markets, R::MarketNotAllowed),
            (&instruments, R::InstrumentNotAllowed),
            (&per_trade, R::PerOperationCap),
            (&per_trade_at, R::Allowed),
            (&per_day, R::Allowed),
            (&fee_cap, R::FeePerConsentCap),
        ];
        for (rules, expected) in cases {
            assert_eq!(evaluate(rules, &ctx, &intent, Some(&swap)).reason, expected);
        }
        let mut used = Fixture::new();
        used.usage.insert(
            UsageBucket::Trading,
            &used.cc.clone(),
            amount("0.0000000001"),
        );
        assert_eq!(
            evaluate(&per_day, &used.ctx(), &intent, Some(&swap)).reason,
            R::DailyCap
        );
        // The binding runs first: content other than the command is a mismatch.
        let mut wrong = intent.clone();
        wrong.outgoing[0].amount = amount("1001");
        assert_eq!(
            evaluate(&markets, &ctx, &wrong, Some(&swap)).reason,
            R::CommandMismatch
        );
        let transfer = with_flows(K::TransferCc, vec![f.flow(&f.cc, "1000", &f.bob)]);
        let decision = evaluate(&rules, &ctx, &transfer, Some(&swap));
        assert_eq!(
            (decision.reason, decision.class),
            (R::CommandMismatch, OperationClass::Trading)
        );
        let unverified = Intent {
            coverage: Coverage::Unverified,
            ..intent.clone()
        };
        assert_eq!(
            evaluate(&rules, &ctx, &unverified, Some(&swap)).reason,
            R::ContentUnverifiable
        );
        // A transfer command still cannot direct a settle.
        let pay = f.command(CommandOp::TransferCc {
            receiver: f.bob.clone(),
            amount: amount("1000"),
            description: None,
            settlement_proposal_id: None,
        });
        let decision = evaluate(&rules, &ctx, &intent, Some(&pay));
        assert_eq!(
            (decision.reason, decision.class),
            (R::CommandMismatch, OperationClass::Transfers)
        );
    }

    #[test]
    fn a_market_list_refuses_every_directed_swap() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let swap = f.command(f.sell_cc());
        let intent = f.swap_intent();
        let mut listed = DEFAULT_LINKED_RULES;
        listed.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("CC/USDCx").unwrap()],
        };
        // Even the listed name is refused: a directed swap's market is the venue's own.
        let decision = evaluate(&listed, &ctx, &intent, Some(&swap));
        assert_eq!(
            (decision.allowed, decision.reason, decision.class),
            (false, R::MarketNotAllowed, OperationClass::Trading)
        );
        let unnamed = Intent {
            market: None,
            ..intent.clone()
        };
        assert_eq!(
            evaluate(&listed, &ctx, &unnamed, Some(&swap)).reason,
            R::MarketNotAllowed
        );
        // Undirected, the listed market is still a trade; with `any`, directed ones pass.
        assert!(evaluate(&listed, &ctx, &intent, None).allowed);
        assert!(evaluate(&DEFAULT_LINKED_RULES, &ctx, &intent, Some(&swap)).allowed);
        assert!(evaluate(&DEFAULT_LINKED_RULES, &ctx, &unnamed, Some(&swap)).allowed);
        // The binding still runs first.
        let mut wrong = intent.clone();
        wrong.outgoing[0].amount = amount("1001");
        assert_eq!(
            evaluate(&listed, &ctx, &wrong, Some(&swap)).reason,
            R::CommandMismatch
        );
    }

    #[test]
    fn a_market_list_refuses_a_swap_command_at_precheck() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let swap = f.command(f.sell_cc());
        let intent = f.swap_intent();
        let mut listed = DEFAULT_LINKED_RULES;
        listed.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("CC/USDCx").unwrap()],
        };
        let refused = (false, R::MarketNotAllowed, OperationClass::Trading);
        let decision = precheck(&listed, K::AtomicDvpSettle, Some(&swap));
        assert_eq!((decision.allowed, decision.reason, decision.class), refused);
        let decision = precheck_intent(&listed, &ctx, &intent, Some(&swap));
        assert_eq!((decision.allowed, decision.reason, decision.class), refused);
        let mut none = listed.clone();
        none.trading.markets.items.clear();
        assert_eq!(
            precheck(&none, K::AtomicDvpSettle, Some(&swap)).reason,
            R::MarketNotAllowed
        );
        // The earlier gates keep their order, as in `evaluate`.
        let mut paused = listed.clone();
        paused.paused = true;
        let mut off = listed.clone();
        off.trading.enabled = false;
        for (rules, expected) in [(&paused, R::Paused), (&off, R::ClassDisabled)] {
            assert_eq!(
                precheck(rules, K::AtomicDvpSettle, Some(&swap)).reason,
                expected
            );
            assert_eq!(
                precheck_intent(rules, &ctx, &intent, Some(&swap)).reason,
                expected
            );
        }
        // Undirected trades, transfer commands and `any` lists are unchanged.
        assert!(precheck(&listed, K::AtomicDvpSettle, None).allowed);
        assert!(precheck_intent(&listed, &ctx, &intent, None).allowed);
        let pay = f.command(CommandOp::TransferCc {
            receiver: f.bob.clone(),
            amount: amount("1"),
            description: None,
            settlement_proposal_id: None,
        });
        assert!(precheck(&listed, K::TransferCc, Some(&pay)).allowed);
        assert!(precheck(&DEFAULT_LINKED_RULES, K::AtomicDvpSettle, Some(&swap)).allowed);
        assert!(precheck_intent(&DEFAULT_LINKED_RULES, &ctx, &intent, Some(&swap)).allowed);
    }

    #[test]
    fn the_evaluator_re_checks_the_scope_and_the_clock() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let swap = f.command(f.sell_cc());
        let intent = f.swap_intent();
        let scoped = |scope: CommandScope| {
            let mut rules = DEFAULT_LINKED_RULES;
            rules.scopes = vec![scope];
            rules
        };
        let scope =
            |commands: &[CommandKind], pairs: Vec<ScopePair>, lps: Vec<PartyId>| CommandScope {
                controller: *swap.controller_id(),
                commands: commands.to_vec(),
                pairs,
                lps,
            };
        let pair = |min_price: &str, max_sell: &str| ScopePair {
            sell: PolicyInstrument::Cc,
            buy: PolicyInstrument::Token(f.usdc.clone()),
            min_price: amount(min_price),
            max_sell: amount(max_sell),
        };
        let verdict = |rules: &Rules, command: &VerifiedCommand| {
            let decision = evaluate(rules, &ctx, &intent, Some(command));
            (decision.reason, decision.class)
        };
        let out = (R::CommandOutOfScope, OperationClass::Trading);
        let allowed = (R::Allowed, OperationClass::Trading);
        let swaps = scoped(scope(&[CommandKind::Swap], Vec::new(), Vec::new()));
        assert_eq!(verdict(&swaps, &swap), allowed);
        let transfers = scoped(scope(&[CommandKind::TransferCc], Vec::new(), Vec::new()));
        assert_eq!(verdict(&transfers, &swap), out);
        assert_eq!(
            verdict(&scoped(scope(&[], Vec::new(), Vec::new())), &swap),
            out
        );
        let bounded = |min_price: &str, max_sell: &str, lps: Vec<PartyId>| {
            scoped(scope(
                &[CommandKind::Swap],
                vec![pair(min_price, max_sell)],
                lps,
            ))
        };
        assert_eq!(
            verdict(&bounded("0.15", "1000", vec![f.bob.clone()]), &swap),
            allowed
        );
        assert_eq!(
            verdict(&bounded("0.1500000001", "1000", Vec::new()), &swap),
            out
        );
        assert_eq!(
            verdict(&bounded("0.15", "999.9999999999", Vec::new()), &swap),
            out
        );
        assert_eq!(
            verdict(&bounded("0.15", "1000", vec![f.alice.clone()]), &swap),
            out
        );
        let reversed = scoped(scope(
            &[CommandKind::Swap],
            vec![ScopePair {
                sell: PolicyInstrument::Token(f.usdc.clone()),
                buy: PolicyInstrument::Cc,
                min_price: Amount::ZERO,
                max_sell: amount("1000000"),
            }],
            Vec::new(),
        ));
        assert_eq!(verdict(&reversed, &swap), out);
        // Another controller's scope does not bind this signer.
        let other = scoped(CommandScope {
            controller: ControllerId([3; 32]),
            ..scope(&[], Vec::new(), Vec::new())
        });
        assert_eq!(verdict(&other, &swap), allowed);
        // A command verified at another clock is never taken.
        for secs in [-1, 1] {
            let stale = swap.verified_again_at(at(secs));
            assert_eq!(verdict(&DEFAULT_LINKED_RULES, &stale), out);
        }
        // Transfers are scoped too.
        let pay = f.command(CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("10"),
            description: None,
            settlement_proposal_id: None,
        });
        let paid = with_flows(K::TransferCc, vec![f.flow(&f.cc, "10", &f.alice)]);
        let decision = evaluate(&swaps, &ctx, &paid, Some(&pay));
        assert_eq!(
            (decision.reason, decision.class),
            (R::CommandOutOfScope, OperationClass::Transfers)
        );
        assert!(evaluate(&transfers, &ctx, &paid, Some(&pay)).allowed);
        // An undirected intent is never judged by a scope.
        assert!(evaluate(&transfers, &ctx, &intent, None).allowed);
    }

    #[test]
    fn authorize_directed_verifies_and_evaluates_at_one_clock() {
        let f = Fixture::new();
        let now = at(testkit::DEVNET_DELAY);
        let signed = |chain: &testkit::Chain, op: CommandOp| {
            let head = chain.state.head().unwrap();
            let command = Command {
                network: Network::Devnet,
                agent_party: f.agent.clone(),
                command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
                log_seq: head.seq,
                log_head: head.hash,
                issued_at: now,
                expires_at: now.checked_add_secs(600).unwrap(),
                op,
                summary: Text::parse("Pay alice").unwrap(),
            };
            let bytes = command.encode().unwrap();
            let envelope = testkit::passkey_envelope(&bytes);
            (bytes, envelope)
        };
        let pay = CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("10"),
            description: None,
            settlement_proposal_id: None,
        };
        let chain = testkit::Chain::linked();
        let (bytes, envelope) = signed(&chain, pay.clone());
        let paid = with_flows(K::TransferCc, vec![f.flow(&f.cc, "10", &f.alice)]);
        // The context's clock is replaced by `now`, so the command is never stale.
        let ctx = f.ctx();
        let done = authorize_directed(&chain.state, now, &bytes, &envelope, &paid, &ctx).unwrap();
        assert_eq!(done.command.verified_at(), now);
        assert_eq!(*done.command.controller_id(), testkit::passkey_id());
        assert_eq!(
            done.decision.usage,
            vec![UsageDelta {
                bucket: UsageBucket::Transfers,
                instrument: f.cc.clone(),
                amount: amount("10"),
            }]
        );
        let to_bob = with_flows(K::TransferCc, vec![f.flow(&f.cc, "10", &f.bob)]);
        let refused =
            authorize_directed(&chain.state, now, &bytes, &envelope, &to_bob, &ctx).unwrap_err();
        assert!(matches!(&refused, DirectedRefusal::Policy(d) if !d.allowed));
        assert_eq!(
            (refused.code(), refused.code_name()),
            (5, "command_mismatch")
        );
        let late = now.checked_add_secs(601).unwrap();
        let expired =
            authorize_directed(&chain.state, late, &bytes, &envelope, &paid, &ctx).unwrap_err();
        assert_eq!(expired, DirectedRefusal::Command(CommandError::Expired));
        assert_eq!((expired.code(), expired.code_name()), (1106, "expired"));
        // A scope accepted at the head refuses the kind before any content is read.
        let mut scoped = testkit::Chain::linked();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.scopes = vec![CommandScope {
            controller: testkit::passkey_id(),
            commands: vec![CommandKind::Swap],
            pairs: Vec::new(),
            lps: Vec::new(),
        }];
        let doc = testkit::policy_doc(&scoped.state, 1, now, rules);
        scoped.accept_passkey(&doc, now);
        let (bytes, envelope) = signed(&scoped, pay);
        let refused =
            authorize_directed(&scoped.state, now, &bytes, &envelope, &paid, &ctx).unwrap_err();
        assert_eq!(refused, DirectedRefusal::Command(CommandError::OutOfScope));
        assert_eq!(
            (refused.code(), refused.code_name()),
            (1108, "command_out_of_scope")
        );
        // The rules come from the state at `now`: a paused policy refuses the transfer.
        let mut paused = testkit::Chain::linked();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.paused = true;
        let doc = testkit::policy_doc(&paused.state, 1, now, rules);
        paused.accept_passkey(&doc, now);
        let (bytes, envelope) = signed(
            &paused,
            CommandOp::TransferCc {
                receiver: f.alice.clone(),
                amount: amount("10"),
                description: None,
                settlement_proposal_id: None,
            },
        );
        let refused =
            authorize_directed(&paused.state, now, &bytes, &envelope, &paid, &ctx).unwrap_err();
        assert_eq!((refused.code(), refused.code_name()), (1, "paused"));
    }

    #[test]
    fn authorize_directed_refuses_an_lp_the_scope_does_not_list_with_1112() {
        let f = Fixture::new();
        let now = at(testkit::DEVNET_DELAY);
        let passkey = testkit::passkey_id();
        let scoped_to = |lps: Vec<PartyId>| {
            let mut chain = testkit::Chain::linked();
            let mut rules = DEFAULT_LINKED_RULES;
            rules.scopes = vec![CommandScope {
                controller: passkey,
                commands: vec![CommandKind::Swap],
                pairs: Vec::new(),
                lps,
            }];
            let doc = testkit::policy_doc(&chain.state, 1, now, rules);
            chain.accept_passkey(&doc, now);
            chain
        };
        let signed = |chain: &testkit::Chain| {
            let head = chain.state.head().unwrap();
            let command = Command {
                network: Network::Devnet,
                agent_party: f.agent.clone(),
                command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
                log_seq: head.seq,
                log_head: head.hash,
                issued_at: now,
                expires_at: now.checked_add_secs(600).unwrap(),
                op: f.sell_cc(),
                summary: Text::parse("Sell 1000 CC").unwrap(),
            };
            let bytes = command.encode().unwrap();
            let envelope = testkit::passkey_envelope(&bytes);
            (bytes, envelope)
        };
        let intent = f.swap_intent();
        let ctx = f.ctx();
        for lps in [vec![f.bob.clone()], Vec::new()] {
            let chain = scoped_to(lps);
            let (bytes, envelope) = signed(&chain);
            let done =
                authorize_directed(&chain.state, now, &bytes, &envelope, &intent, &ctx).unwrap();
            assert!(done.decision.allowed);
        }
        let chain = scoped_to(vec![f.alice.clone()]);
        let (bytes, envelope) = signed(&chain);
        let refused =
            authorize_directed(&chain.state, now, &bytes, &envelope, &intent, &ctx).unwrap_err();
        assert_eq!(refused, DirectedRefusal::Command(CommandError::LpNotAllowed));
        assert_eq!(
            (refused.code(), refused.code_name()),
            (1112, "lp_not_allowed")
        );
        // `evaluate` alone keeps refusing it as out of scope, the backstop.
        let rules = chain.state.rules_at(now).rules();
        let verified = verify_command(&chain.state, &bytes, &envelope, now, Some(&f.cc)).unwrap();
        let at_now = EvalContext { now, ..ctx };
        assert_eq!(
            evaluate(rules, &at_now, &intent, Some(&verified)).reason,
            R::CommandOutOfScope
        );
        // Only a swap with an unlisted LP fails, and only under a scope that lists LPs.
        let swap = f.sell_cc();
        assert_eq!(
            check_swap_lp(rules, &passkey, &swap, &f.bob),
            Err(CommandError::LpNotAllowed)
        );
        assert_eq!(check_swap_lp(rules, &passkey, &swap, &f.alice), Ok(()));
        assert_eq!(check_swap_lp(rules, &ControllerId([9; 32]), &swap, &f.bob), Ok(()));
        let pay = CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("10"),
            description: None,
            settlement_proposal_id: None,
        };
        assert_eq!(check_swap_lp(rules, &passkey, &pay, &f.bob), Ok(()));
        assert_eq!(
            check_swap_lp(&DEFAULT_LINKED_RULES, &passkey, &swap, &f.bob),
            Ok(())
        );
    }

    #[test]
    fn settle_expiries_round_up_to_the_second() {
        let secs = at(0).unix_secs();
        let micros = |extra: i64| secs * 1_000_000 + extra;
        assert_eq!(SettleTerms::expiry_from_micros(micros(0)), Some(at(0)));
        assert_eq!(SettleTerms::expiry_from_micros(micros(1)), Some(at(1)));
        assert_eq!(
            SettleTerms::expiry_from_micros(micros(999_999)),
            Some(at(1))
        );
        assert_eq!(SettleTerms::expiry_from_micros(micros(-1)), Some(at(0)));
        assert_eq!(SettleTerms::expiry_from_micros(-1), None);
        assert_eq!(SettleTerms::expiry_from_micros(i64::MAX), None);
    }

    #[test]
    fn allowlists_refuse_what_they_do_not_list() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.directed_only = false;
        rules.transfers.recipients = Allowlist {
            any: false,
            items: vec![f.alice.clone()],
        };
        let eth = EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7").unwrap();
        let other = EthAddress::parse("0x0000000000000000000000000000000000000001").unwrap();
        rules.transfers.eth_destinations = Allowlist {
            any: false,
            items: vec![eth],
        };
        let burn = |to: EthAddress| {
            with_flows(
                K::UsdcxBurn,
                vec![Flow {
                    instrument: f.usdc.clone(),
                    amount: amount("1"),
                    to: Some(Recipient::Eth(to)),
                }],
            )
        };
        let unknown = with_flows(
            K::TransferCc,
            vec![Flow {
                instrument: f.cc.clone(),
                amount: amount("1"),
                to: None,
            }],
        );
        let cases = [
            (
                with_flows(K::TransferCc, vec![f.flow(&f.cc, "1", &f.alice)]),
                R::Allowed,
            ),
            (
                with_flows(K::TransferCc, vec![f.flow(&f.cc, "1", &f.bob)]),
                R::RecipientNotAllowed,
            ),
            (burn(eth), R::Allowed),
            (burn(other), R::DestinationNotAllowed),
            (unknown, R::RecipientNotAllowed),
        ];
        for (intent, expected) in &cases {
            assert_eq!(verdict(&rules, &ctx, intent), *expected, "{intent:?}");
        }
        let mut trading = DEFAULT_LINKED_RULES;
        trading.trading.markets = Allowlist {
            any: false,
            items: vec![MarketId::parse("CC/USDCx").unwrap()],
        };
        trading.trading.instruments = Allowlist {
            any: false,
            items: vec![PolicyInstrument::Token(f.usdc.clone())],
        };
        let trade = |market: &str, instruments: Vec<InstrumentRef>| Intent {
            market: Some(MarketId::parse(market).unwrap()),
            instruments,
            ..Intent::new(K::AtomicDvpSettle)
        };
        let cases = [
            (trade("CC/USDCx", vec![f.usdc.clone()]), R::Allowed),
            (
                trade("cETH/USDCx", vec![f.usdc.clone()]),
                R::MarketNotAllowed,
            ),
            (
                trade("CC/USDCx", vec![f.usdc.clone(), f.cc.clone()]),
                R::InstrumentNotAllowed,
            ),
            (
                Intent {
                    outgoing: vec![f.flow(&f.cc, "1", &f.bob)],
                    ..trade("CC/USDCx", vec![f.usdc.clone()])
                },
                R::InstrumentNotAllowed,
            ),
        ];
        for (intent, expected) in &cases {
            assert_eq!(verdict(&trading, &ctx, intent), *expected, "{intent:?}");
        }
    }

    #[test]
    fn the_cc_alias_takes_the_lowest_cap_and_any_listing() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let trade = |value: &str| with_flows(K::Allocate, vec![f.flow(&f.cc, value, &f.bob)]);
        let item = |instrument: PolicyInstrument, value: &str| LimitItem {
            instrument,
            amount: amount(value),
        };
        let cases: Vec<(Unlisted, Vec<LimitItem>, &str, ReasonCode)> = vec![
            (
                Unlisted::Unlimited,
                vec![item(PolicyInstrument::Cc, "10")],
                "10",
                R::Allowed,
            ),
            (
                Unlisted::Unlimited,
                vec![item(PolicyInstrument::Cc, "10")],
                "11",
                R::PerOperationCap,
            ),
            (
                Unlisted::Unlimited,
                vec![item(PolicyInstrument::Token(f.cc.clone()), "10")],
                "11",
                R::PerOperationCap,
            ),
            (
                Unlisted::Unlimited,
                vec![
                    item(PolicyInstrument::Cc, "1000"),
                    item(PolicyInstrument::Token(f.cc.clone()), "10"),
                ],
                "11",
                R::PerOperationCap,
            ),
            (
                Unlisted::Deny,
                vec![item(PolicyInstrument::Cc, "1000")],
                "1",
                R::UnlistedInstrument,
            ),
            (
                Unlisted::Deny,
                vec![
                    item(PolicyInstrument::Cc, "1000"),
                    item(PolicyInstrument::Token(f.cc.clone()), "500"),
                ],
                "400",
                R::Allowed,
            ),
            (
                Unlisted::Deny,
                vec![item(PolicyInstrument::Token(f.usdc.clone()), "500")],
                "1",
                R::UnlistedInstrument,
            ),
        ];
        for (unlisted, items, value, expected) in cases {
            let mut rules = DEFAULT_LINKED_RULES;
            rules.trading.per_trade = Limits { unlisted, items };
            assert_eq!(verdict(&rules, &ctx, &trade(value)), expected, "{value}");
        }
        let mut listed = DEFAULT_LINKED_RULES;
        listed.trading.instruments = Allowlist {
            any: false,
            items: vec![PolicyInstrument::Cc],
        };
        assert_eq!(verdict(&listed, &ctx, &trade("1")), R::Allowed);
        listed.trading.instruments.items = vec![PolicyInstrument::Token(f.cc.clone())];
        assert_eq!(verdict(&listed, &ctx, &trade("1")), R::Allowed);
        listed.trading.instruments.items = vec![PolicyInstrument::Token(f.usdc.clone())];
        assert_eq!(verdict(&listed, &ctx, &trade("1")), R::InstrumentNotAllowed);
    }

    #[test]
    fn daily_caps_use_the_rolling_window() {
        let mut f = Fixture::new();
        let now = at(0);
        let record = |age: i64, value: &str| UsageRecord {
            bucket: UsageBucket::Transfers,
            instrument: f.cc.clone(),
            amount: amount(value),
            at: now.checked_add_secs(-age).unwrap(),
        };
        let records = vec![
            record(86_400, "50"),
            record(86_399, "40"),
            record(10, "5"),
            record(-30, "1"),
        ];
        f.usage = UsageTotals::from_records(&records, now);
        assert_eq!(
            f.usage.get(UsageBucket::Transfers, &f.cc),
            Some(amount("46"))
        );
        assert_eq!(f.usage.get(UsageBucket::Trading, &f.cc), Some(Amount::ZERO));
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.directed_only = false;
        rules.transfers.per_day.items = vec![LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount("50"),
        }];
        let pay = |value: &str| with_flows(K::TransferCc, vec![f.flow(&f.cc, value, &f.alice)]);
        assert_eq!(verdict(&rules, &ctx, &pay("4")), R::Allowed);
        assert_eq!(verdict(&rules, &ctx, &pay("4.0000000001")), R::DailyCap);
        assert_eq!(verdict(&rules, &ctx, &pay("0")), R::Allowed);
        rules.transfers.per_day.unlisted = Unlisted::Deny;
        let usdc = with_flows(K::TransferCc, vec![f.flow(&f.usdc, "1", &f.alice)]);
        assert_eq!(verdict(&rules, &ctx, &usdc), R::UnlistedInstrument);
        let mut traffic = DEFAULT_LINKED_RULES;
        traffic.maintenance.traffic_cc_per_day = Some(amount("10"));
        let prepay =
            |value: &str| with_flows(K::PrepayTraffic, vec![f.flow(&f.cc, value, &f.prepaid)]);
        assert_eq!(verdict(&traffic, &ctx, &prepay("10")), R::Allowed);
        assert_eq!(verdict(&traffic, &ctx, &prepay("10.5")), R::TrafficDailyCap);
    }

    #[test]
    fn fee_caps_cover_consents_and_platform_flows() {
        let mut f = Fixture::new();
        f.usage
            .insert(UsageBucket::Fees, &f.cc.clone(), amount("8"));
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.fees.cc_per_consent = Some(amount("3"));
        rules.fees.cc_per_day = Some(amount("10"));
        let cases = [
            (fee_consent(K::TxFeesAuthorization, "2"), R::Allowed),
            (
                fee_consent(K::PayFeeAuthorization, "3.5"),
                R::FeePerConsentCap,
            ),
            (fee_consent(K::OffChainAuthorization, "2.5"), R::FeeDailyCap),
            (Intent::new(K::TxFeesAuthorization), R::MalformedIntent),
            (
                Intent {
                    outgoing: vec![f.flow(&f.cc, "1", &f.fee)],
                    ..fee_consent(K::TxFeesAuthorization, "1")
                },
                R::MalformedIntent,
            ),
            (
                with_flows(K::PayDvpFee, vec![f.flow(&f.cc, "2", &f.fee)]),
                R::Allowed,
            ),
            (
                with_flows(K::PayAllocFee, vec![f.flow(&f.cc, "4", &f.fee)]),
                R::FeePerConsentCap,
            ),
        ];
        for (intent, expected) in &cases {
            assert_eq!(verdict(&rules, &ctx, intent), *expected, "{intent:?}");
        }
        let decision = evaluate(
            &rules,
            &ctx,
            &with_flows(K::PayDvpFee, vec![f.flow(&f.cc, "2", &f.fee)]),
            None,
        );
        assert_eq!(decision.class, OperationClass::Trading);
        assert_eq!(
            decision.usage,
            vec![UsageDelta {
                bucket: UsageBucket::Fees,
                instrument: f.cc.clone(),
                amount: amount("2"),
            }]
        );
    }

    #[test]
    fn platform_parties_reclassify_only_when_pinned() {
        let f = Fixture::new();
        let rules = DEFAULT_LINKED_RULES;
        let to_fee = with_flows(K::TransferCc, vec![f.flow(&f.cc, "2", &f.fee)]);
        let to_prepaid = multicall(vec![with_flows(
            K::McBatchPay,
            vec![f.flow(&f.cc, "2", &f.prepaid)],
        )]);
        let pinned = f.ctx();
        let decision = evaluate(&rules, &pinned, &to_fee, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::FeeConsents)
        );
        assert_eq!(decision.usage.len(), 1);
        assert_eq!(decision.usage[0].bucket, UsageBucket::Fees);
        assert_eq!(verdict(&rules, &pinned, &to_prepaid), R::Allowed);
        let unpinned = f.unpinned();
        assert_eq!(verdict(&rules, &unpinned, &to_fee), R::DirectedOnly);
        assert_eq!(verdict(&rules, &unpinned, &to_prepaid), R::DirectedOnly);
        let not_cc = with_flows(K::TransferCc, vec![f.flow(&f.usdc, "2", &f.fee)]);
        assert_eq!(verdict(&rules, &pinned, &not_cc), R::DirectedOnly);
        let mixed = with_flows(
            K::TransferCc,
            vec![f.flow(&f.cc, "2", &f.fee), f.flow(&f.cc, "1", &f.alice)],
        );
        assert_eq!(verdict(&rules, &pinned, &mixed), R::DirectedOnly);
        let unverified = Intent {
            coverage: Coverage::Unverified,
            ..to_fee.clone()
        };
        assert_eq!(
            verdict(&rules, &pinned, &unverified),
            R::ContentUnverifiable
        );
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(verdict(&paused, &pinned, &to_fee), R::Allowed);
        let mut capped = rules.clone();
        capped.fees.cc_per_consent = Some(amount("1"));
        assert_eq!(verdict(&capped, &pinned, &to_fee), R::FeePerConsentCap);
        let elsewhere = with_flows(K::PrepayTraffic, vec![f.flow(&f.cc, "2", &f.alice)]);
        assert_eq!(verdict(&rules, &pinned, &elsewhere), R::DirectedOnly);
        assert_eq!(verdict(&rules, &unpinned, &elsewhere), R::Allowed);
        let hidden = with_flows(K::SplitCc, vec![f.flow(&f.cc, "2", &f.alice)]);
        assert_eq!(verdict(&rules, &unpinned, &hidden), R::DirectedOnly);
    }

    #[test]
    fn a_top_up_to_the_pinned_traffic_fee_party_is_maintenance() {
        let mut f = Fixture::new();
        f.usage
            .insert(UsageBucket::Traffic, &f.cc.clone(), amount("6"));
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let top_up = |instrument: &InstrumentRef, value: &str| {
            purchase(with_flows(
                K::TransferCip56,
                vec![
                    f.flow(instrument, value, &f.traffic_fee),
                    f.flow(instrument, "2", &f.agent),
                ],
            ))
        };
        let token = top_up(&f.usdc, "13");
        let decision = evaluate(&rules, &ctx, &token, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Maintenance)
        );
        assert!(decision.usage.is_empty(), "{:?}", decision.usage);
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(verdict(&paused, &ctx, &token), R::Allowed);
        let mut off = rules.clone();
        off.maintenance.enabled = false;
        assert_eq!(verdict(&off, &ctx, &token), R::ClassDisabled);
        // Unpinned, or not read from verified content, it stays a transfer.
        let decision = evaluate(&rules, &f.unpinned(), &token, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::DirectedOnly, OperationClass::Transfers)
        );
        let unverified = Intent {
            coverage: Coverage::Unverified,
            ..token.clone()
        };
        assert_eq!(
            verdict(&rules, &ctx, &unverified),
            R::ContentUnverifiable
        );
        // A CC top-up counts as traffic.
        let cc = top_up(&f.cc, "4");
        let decision = evaluate(&rules, &ctx, &cc, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Maintenance)
        );
        assert_eq!(
            decision.usage,
            vec![UsageDelta {
                bucket: UsageBucket::Traffic,
                instrument: f.cc.clone(),
                amount: amount("4"),
            }]
        );
        // Under a CC traffic cap a token top-up is refused; a CC one is summed.
        let mut capped = rules.clone();
        capped.maintenance.traffic_cc_per_day = Some(amount("10"));
        let decision = evaluate(&capped, &ctx, &token, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::TrafficDailyCap, OperationClass::Maintenance)
        );
        assert_eq!(verdict(&capped, &ctx, &cc), R::Allowed);
        assert_eq!(
            verdict(&capped, &ctx, &top_up(&f.cc, "4.0000000001")),
            R::TrafficDailyCap
        );
        assert_eq!(
            verdict(&capped, &f.unpinned(), &token),
            R::DirectedOnly
        );
    }

    #[test]
    fn only_a_single_undirected_cip56_flow_is_a_traffic_top_up() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let transfers = [
            purchase(with_flows(
                K::TransferCip56,
                vec![
                    f.flow(&f.usdc, "13", &f.traffic_fee),
                    f.flow(&f.usdc, "1", &f.alice),
                ],
            )),
            purchase(with_flows(
                K::TransferCip56,
                vec![
                    f.flow(&f.usdc, "13", &f.traffic_fee),
                    f.flow(&f.usdc, "1", &f.traffic_fee),
                ],
            )),
            purchase(with_flows(
                K::TransferCip56,
                vec![
                    f.flow(&f.usdc, "13", &f.traffic_fee),
                    Flow {
                        instrument: f.usdc.clone(),
                        amount: amount("1"),
                        to: None,
                    },
                ],
            )),
            purchase(with_flows(K::TransferCc, vec![f.flow(&f.cc, "4", &f.traffic_fee)])),
            multicall(vec![purchase(with_flows(
                K::McTransfer,
                vec![f.flow(&f.cc, "4", &f.traffic_fee)],
            ))]),
        ];
        for intent in &transfers {
            let decision = evaluate(&rules, &ctx, intent, None);
            assert_eq!(
                (decision.reason, decision.class),
                (R::DirectedOnly, OperationClass::Transfers),
                "{intent:?}"
            );
        }
        // A command directs it as the transfer it names.
        let command = f.command(CommandOp::TransferToken {
            receiver: f.traffic_fee.clone(),
            instrument: f.usdc.clone(),
            amount: amount("13"),
            reference: None,
        });
        let token = with_flows(
            K::TransferCip56,
            vec![f.flow(&f.usdc, "13", &f.traffic_fee)],
        );
        let decision = evaluate(&rules, &ctx, &token, Some(&command));
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Transfers)
        );
        let decision = precheck_intent(&rules, &ctx, &token, Some(&command));
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Transfers)
        );
        // The traffic-fee party is not a platform fee party.
        assert!(!f.pinned.contains(&f.traffic_fee));
        let mut capped = rules.clone();
        capped.fees.cc_per_consent = Some(amount("1"));
        let cc = purchase(with_flows(
            K::TransferCip56,
            vec![f.flow(&f.cc, "4", &f.traffic_fee)],
        ));
        assert_eq!(verdict(&capped, &ctx, &cc), R::Allowed);
    }

    // Only a direct transfer that pays a purchase is a top-up: the shared rule passes pauses
    #[test]
    fn a_traffic_top_up_is_a_direct_transfer_with_the_purchase_memo() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let mut paused = rules.clone();
        paused.paused = true;
        paused.transfers.directed_only = false;
        let paid = purchase(with_flows(
            K::TransferCip56,
            vec![f.flow(&f.usdc, "13", &f.traffic_fee)],
        ));
        let decision = evaluate(&rules, &ctx, &paid, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Maintenance)
        );
        assert_eq!(verdict(&paused, &ctx, &paid), R::Allowed);
        type Edit = (&'static str, fn(&mut Intent));
        let edits: [Edit; 6] = [
            ("no memo", |i| i.memos.clear()),
            ("a foreign memo", |i| i.memos = vec!["refund".into()]),
            ("the prefix alone", |i| i.memos = vec!["agent-traffic:".into()]),
            ("a second memo", |i| i.memos.push("other".into())),
            ("a two-step offer", |i| i.delivery = Some(Delivery::Pending)),
            ("no delivery read", |i| i.delivery = None),
        ];
        for (what, edit) in edits {
            let mut intent = paid.clone();
            edit(&mut intent);
            let decision = evaluate(&rules, &ctx, &intent, None);
            assert_eq!(
                (decision.reason, decision.class),
                (R::DirectedOnly, OperationClass::Transfers),
                "{what}"
            );
            assert_eq!(
                precheck_intent(&rules, &ctx, &intent, None).reason,
                R::DirectedOnly,
                "{what}"
            );
            assert_eq!(verdict(&paused, &ctx, &intent), R::Paused, "{what}");
        }
        assert!(is_traffic_purchase_memo("agent-traffic:p-1"));
        for memo in ["", "agent-traffic:", "agent-traffic: p", "agent-traffic:p 1", "AGENT-TRAFFIC:p"] {
            assert!(!is_traffic_purchase_memo(memo), "{memo:?}");
        }
    }

    #[test]
    fn prechecks_judge_a_traffic_top_up_by_its_named_flow() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let token = purchase(with_flows(
            K::TransferCip56,
            vec![f.flow(&f.usdc, "13", &f.traffic_fee)],
        ));
        let cc = purchase(with_flows(
            K::TransferCip56,
            vec![f.flow(&f.cc, "40", &f.traffic_fee)],
        ));
        let decision = precheck_intent(&rules, &ctx, &token, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Maintenance)
        );
        assert_eq!(
            precheck_intent(&rules, &f.unpinned(), &token, None).reason,
            R::DirectedOnly
        );
        let mut capped = rules.clone();
        capped.maintenance.traffic_cc_per_day = Some(amount("10"));
        let decision = precheck_intent(&capped, &ctx, &token, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::TrafficDailyCap, OperationClass::Maintenance)
        );
        // Prechecks sum no caps: a CC top-up over the cap passes here.
        assert_eq!(
            precheck_intent(&capped, &ctx, &cc, None).reason,
            R::Allowed
        );
        let mixed = purchase(with_flows(
            K::TransferCip56,
            vec![
                f.flow(&f.usdc, "13", &f.traffic_fee),
                f.flow(&f.usdc, "1", &f.alice),
            ],
        ));
        assert_eq!(
            precheck_intent(&rules, &ctx, &mixed, None).reason,
            R::DirectedOnly
        );
    }

    #[test]
    fn unexplained_outflows_make_a_transfer_intent() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let to_alice = || vec![f.flow(&f.cc, "2", &f.alice)];
        let cases = [
            with_flows(K::SplitCc, to_alice()),
            with_flows(K::AcceptCip56, to_alice()),
            with_flows(K::PrepayTraffic, to_alice()),
            with_flows(K::RequestPreapproval, to_alice()),
            multicall(vec![with_flows(K::McAcceptTransferInstruction, to_alice())]),
            multicall(vec![
                Intent::new(K::McCreateTransferPreapproval),
                with_flows(K::McCreateUserServiceRequest, to_alice()),
            ]),
        ];
        for intent in &cases {
            let decision = evaluate(&rules, &ctx, intent, None);
            assert_eq!(
                (decision.reason, decision.class),
                (R::DirectedOnly, OperationClass::Transfers),
                "{intent:?}"
            );
        }
        let explained = [
            with_flows(K::SplitCc, vec![f.flow(&f.cc, "2", &f.agent)]),
            with_flows(K::PrepayTraffic, vec![f.flow(&f.cc, "2", &f.prepaid)]),
        ];
        for intent in &explained {
            assert_eq!(verdict(&rules, &ctx, intent), R::Allowed, "{intent:?}");
        }
        let command = f.command(CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("2"),
            description: None,
            settlement_proposal_id: None,
        });
        let second_leg = with_flows(
            K::TransferCc,
            vec![f.flow(&f.cc, "2", &f.alice), f.flow(&f.cc, "2", &f.bob)],
        );
        assert_eq!(
            evaluate(&rules, &ctx, &second_leg, Some(&command)).reason,
            R::CommandMismatch
        );
        let substituted = with_flows(K::TransferCc, vec![f.flow(&f.cc, "2", &f.bob)]);
        assert_eq!(
            evaluate(&rules, &ctx, &substituted, Some(&command)).reason,
            R::CommandMismatch
        );
    }

    #[test]
    fn unverified_content_is_refused_only_where_the_policy_limits_it() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let unverified = |kind: OperationKind| Intent {
            coverage: Coverage::Unverified,
            ..minimal(kind)
        };
        let defaults = DEFAULT_LINKED_RULES;
        assert_eq!(
            verdict(&defaults, &ctx, &unverified(K::SplitCc)),
            R::Allowed
        );
        assert_eq!(
            verdict(&defaults, &ctx, &unverified(K::AtomicDvpSettle)),
            R::Allowed
        );
        assert_eq!(
            verdict(&defaults, &ctx, &unverified(K::TransferCc)),
            R::ContentUnverifiable
        );
        let mut limited = DEFAULT_LINKED_RULES;
        limited.trading.per_day.unlisted = Unlisted::Deny;
        assert_eq!(
            verdict(&limited, &ctx, &unverified(K::AtomicDvpSettle)),
            R::ContentUnverifiable
        );
        let mut fees = DEFAULT_LINKED_RULES;
        fees.fees.cc_per_day = Some(amount("1"));
        assert_eq!(
            verdict(&fees, &ctx, &unverified(K::LockHoldings)),
            R::ContentUnverifiable
        );
        assert_eq!(verdict(&fees, &ctx, &unverified(K::SplitCc)), R::Allowed);
        let mut locks_off = DEFAULT_LINKED_RULES;
        locks_off.locks.enabled = false;
        assert_eq!(
            verdict(&locks_off, &ctx, &unverified(K::UpdateVenueKey)),
            R::Allowed
        );
        let batch = Intent {
            coverage: Coverage::Unverified,
            ..multicall(vec![Intent::new(K::McAllocate)])
        };
        assert_eq!(verdict(&limited, &ctx, &batch), R::ContentUnverifiable);
        assert!(class_is_limited(&limited, OperationClass::Container));
        assert!(!class_is_limited(&defaults, OperationClass::Trading));
        assert!(class_is_limited(&defaults, OperationClass::Transfers));
        assert!(!class_is_limited(&fees, OperationClass::Identity));
    }

    #[test]
    fn multicalls_aggregate_caps_and_refuse_as_a_whole() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.directed_only = false;
        rules.transfers.per_transfer.items = vec![LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount("100"),
        }];
        let send =
            |value: &str, to: &PartyId| with_flows(K::McTransfer, vec![f.flow(&f.cc, value, to)]);
        let ok = multicall(vec![send("60", &f.alice), send("40", &f.bob)]);
        let decision = evaluate(&rules, &ctx, &ok, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Container)
        );
        assert_eq!(decision.usage[0].amount, amount("100"));
        let over = multicall(vec![send("60", &f.alice), send("41", &f.bob)]);
        let decision = evaluate(&rules, &ctx, &over, None);
        assert_eq!(
            (decision.reason, decision.class),
            (R::PerOperationCap, OperationClass::Transfers)
        );
        rules.trading.enabled = false;
        let one_denied = multicall(vec![send("1", &f.alice), Intent::new(K::McAllocate)]);
        assert_eq!(verdict(&rules, &ctx, &one_denied), R::ClassDisabled);
        let merged = multicall(vec![
            send("1", &f.agent),
            Intent::new(K::McWithdrawAllocation),
        ]);
        assert_eq!(verdict(&rules, &ctx, &merged), R::Allowed);
        let malformed = [
            multicall(Vec::new()),
            multicall(vec![multicall(vec![send("1", &f.alice)])]),
            multicall(vec![Intent::new(K::TransferCc)]),
            multicall(vec![Intent {
                sub: vec![send("1", &f.alice)],
                ..Intent::new(K::McTransfer)
            }]),
            multicall((0..33).map(|_| send("1", &f.alice)).collect()),
            Intent {
                outgoing: vec![f.flow(&f.cc, "1", &f.alice)],
                ..multicall(vec![send("1", &f.alice)])
            },
            Intent {
                sub: vec![send("1", &f.alice)],
                ..Intent::new(K::TransferCc)
            },
            with_flows(K::Jwt, vec![f.flow(&f.cc, "1", &f.alice)]),
        ];
        for intent in &malformed {
            assert_eq!(
                verdict(&rules, &ctx, intent),
                R::MalformedIntent,
                "{intent:?}"
            );
        }
        let fees = Intent {
            fee_total_cc: Some(amount("2")),
            ..multicall(vec![with_flows(
                K::McBatchPay,
                vec![f.flow(&f.cc, "2", &f.fee)],
            )])
        };
        let mut fee_cap = DEFAULT_LINKED_RULES;
        fee_cap.fees.cc_per_consent = Some(amount("3"));
        assert_eq!(verdict(&fee_cap, &ctx, &fees), R::FeePerConsentCap);
    }

    #[test]
    fn huge_sums_overflow_without_panicking() {
        let mut f = Fixture::new();
        let big = amount("99999999999999999");
        let ctx = f.ctx();
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.directed_only = false;
        let twice = with_flows(
            K::TransferCc,
            vec![
                Flow {
                    instrument: f.cc.clone(),
                    amount: big,
                    to: Some(Recipient::Party(f.alice.clone())),
                },
                Flow {
                    instrument: f.cc.clone(),
                    amount: big,
                    to: Some(Recipient::Party(f.bob.clone())),
                },
            ],
        );
        assert_eq!(verdict(&rules, &ctx, &twice), R::AmountOverflow);
        f.usage.insert(UsageBucket::Fees, &f.cc.clone(), big);
        f.usage.insert(UsageBucket::Fees, &f.cc.clone(), big);
        assert_eq!(f.usage.get(UsageBucket::Fees, &f.cc), None);
        let ctx = f.ctx();
        rules.fees.cc_per_day = Some(amount("1"));
        assert_eq!(
            verdict(&rules, &ctx, &fee_consent(K::TxFeesAuthorization, "1")),
            R::AmountOverflow
        );
        assert_eq!(
            verdict(&rules, &ctx, &fee_consent(K::TxFeesAuthorization, "0")),
            R::Allowed
        );
    }

    #[test]
    fn prechecks_run_only_the_cheap_gates() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let rules = DEFAULT_LINKED_RULES;
        let pay = f.command(CommandOp::TransferCc {
            receiver: f.alice.clone(),
            amount: amount("1"),
            description: None,
            settlement_proposal_id: None,
        });
        let burn = f.command(CommandOp::UsdcxBurn {
            amount: amount("1"),
            destination: EthAddress([1; 20]),
        });
        let cases = [
            (K::TransferCc, None, R::DirectedOnly),
            (K::TransferCc, Some(&pay), R::Allowed),
            (K::UsdcxBurn, None, R::DirectedOnly),
            (K::UsdcxBurn, Some(&burn), R::Allowed),
            (K::UsdcxBurn, Some(&pay), R::CommandMismatch),
            (K::TransferCip56, Some(&pay), R::CommandMismatch),
            (K::ProposeDvp, None, R::Allowed),
            (K::Jwt, None, R::Allowed),
            (K::Jwt, Some(&pay), R::CommandMismatch),
            (K::ExecuteMultiCall, None, R::Allowed),
            (K::ExecuteMultiCall, Some(&pay), R::CommandMismatch),
        ];
        for (kind, command, expected) in cases {
            assert_eq!(precheck(&rules, kind, command).reason, expected, "{kind}");
        }
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(
            precheck(&paused, K::TransferCc, Some(&pay)).reason,
            R::Paused
        );
        assert_eq!(precheck(&paused, K::RetireVenue, None).reason, R::Allowed);
        let merge = with_flows(K::TransferCip56, vec![f.flow(&f.usdc, "5", &f.agent)]);
        assert_eq!(
            precheck(&rules, K::TransferCip56, None).reason,
            R::DirectedOnly
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &merge, None).reason,
            R::Allowed
        );
        let batch = multicall(vec![with_flows(
            K::McTransfer,
            vec![f.flow(&f.cc, "1", &f.alice)],
        )]);
        assert_eq!(
            precheck_intent(&rules, &ctx, &batch, None).reason,
            R::DirectedOnly
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &batch, Some(&pay)).reason,
            R::CommandMismatch
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::McTransfer), None).reason,
            R::MalformedIntent
        );
        let jwt_with_flow = with_flows(K::Jwt, vec![f.flow(&f.cc, "1", &f.alice)]);
        assert_eq!(
            precheck_intent(&rules, &ctx, &jwt_with_flow, None).reason,
            R::MalformedIntent
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::Jwt), Some(&pay)).reason,
            R::CommandMismatch
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::ExecuteMultiCall), None).reason,
            R::MalformedIntent
        );
        let cc_pay = with_flows(K::TransferCc, vec![f.flow(&f.cc, "1", &f.alice)]);
        assert_eq!(
            precheck_intent(&rules, &ctx, &cc_pay, Some(&pay)).reason,
            R::Allowed
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &cc_pay, Some(&burn)).reason,
            R::CommandMismatch
        );
    }

    #[test]
    fn prechecks_judge_a_swap_as_trading_inside_its_scope() {
        let f = Fixture::new();
        let ctx = f.ctx();
        let swap = f.command(f.sell_cc());
        let rules = DEFAULT_LINKED_RULES;
        let decision = precheck(&rules, K::AtomicDvpSettle, Some(&swap));
        assert_eq!(
            (decision.reason, decision.class),
            (R::Allowed, OperationClass::Trading)
        );
        assert_eq!(
            precheck(&rules, K::TransferCc, Some(&swap)).reason,
            R::CommandMismatch
        );
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(
            precheck(&paused, K::AtomicDvpSettle, Some(&swap)).reason,
            R::Paused
        );
        let mut off = rules.clone();
        off.trading.enabled = false;
        assert_eq!(
            precheck(&off, K::AtomicDvpSettle, Some(&swap)).reason,
            R::ClassDisabled
        );
        let mut no_transfers = rules.clone();
        no_transfers.transfers.enabled = false;
        assert_eq!(
            precheck(&no_transfers, K::AtomicDvpSettle, Some(&swap)).reason,
            R::Allowed
        );
        let intent = f.swap_intent();
        assert_eq!(
            precheck_intent(&rules, &ctx, &intent, Some(&swap)).reason,
            R::Allowed
        );
        // The scope comes first, before any kind or shape rule.
        let mut scoped = rules.clone();
        scoped.scopes = vec![CommandScope {
            controller: *swap.controller_id(),
            commands: vec![CommandKind::TransferCc],
            pairs: Vec::new(),
            lps: Vec::new(),
        }];
        for kind in [K::AtomicDvpSettle, K::TransferCc, K::Jwt] {
            let decision = precheck(&scoped, kind, Some(&swap));
            assert_eq!(
                (decision.reason, decision.class),
                (R::CommandOutOfScope, OperationClass::Trading),
                "{kind}"
            );
        }
        assert_eq!(
            precheck_intent(&scoped, &ctx, &intent, Some(&swap)).reason,
            R::CommandOutOfScope
        );
        // Only `precheck_intent` knows the clock: a stale command is out of scope there.
        let stale = swap.verified_again_at(at(1));
        assert_eq!(
            precheck(&rules, K::AtomicDvpSettle, Some(&stale)).reason,
            R::Allowed
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &intent, Some(&stale)).reason,
            R::CommandOutOfScope
        );
    }

    #[test]
    fn reason_codes_are_stable() {
        for (number, reason) in ReasonCode::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(reason.as_u16()), number);
            assert_eq!(ReasonCode::from_u16(reason.as_u16()), Some(reason));
        }
        assert_eq!(R::ContentUnverifiable.as_u16(), 18);
        assert_eq!(R::AmountOverflow.code_name(), "amount_overflow");
        assert_eq!(R::PackageNotAllowed.as_u16(), 19);
        assert_eq!(R::PackageNotAllowed.code_name(), "package_not_allowed");
        assert_eq!(R::CommandOutOfScope.as_u16(), 20);
        assert_eq!(R::CommandOutOfScope.code_name(), "command_out_of_scope");
        assert_eq!(ReasonCode::from_u16(20), Some(R::CommandOutOfScope));
        assert_eq!(ReasonCode::from_u16(21), None);
        assert_eq!(ReasonCode::ALL.len(), 21);
        let names: std::collections::HashSet<&str> =
            ReasonCode::ALL.iter().map(|r| r.code_name()).collect();
        assert_eq!(names.len(), ReasonCode::ALL.len());
        for bucket in UsageBucket::ALL {
            assert_eq!(UsageBucket::parse(bucket.as_str()), Some(bucket));
        }
    }
}
