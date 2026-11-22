//! The policy evaluator: one intent under the rules in force.

use std::collections::BTreeMap;

use crate::canon::time::UtcTime;
use crate::canon::values::{Amount, EthAddress, InstrumentRef, MarketId};
use crate::command::{VerifiedCommand, command_matches};
use crate::party::PartyId;
use crate::policy::kinds::{OperationClass, OperationKind};
use crate::policy::{Allowlist, CapValue, Limits, PolicyInstrument, Rules, Unlisted};

/// Most operations inside one multicall.
pub const MAX_MULTICALL_OPS: usize = 32;
/// Length of the rolling usage window, in seconds.
pub const USAGE_WINDOW_SECS: i64 = 86_400;

/// Where a flow goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
        }
    }
}

/// Platform parties pinned in the enforcer's own config, never taken from server data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlatformParties {
    pub fee: Option<PartyId>,
    pub prepaid_traffic: Option<PartyId>,
}

impl PlatformParties {
    pub const NONE: Self = Self {
        fee: None,
        prepaid_traffic: None,
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
}

impl ReasonCode {
    pub const ALL: [Self; 19] = [
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

/// The class after the self-transfer, platform-party and unexplained-flow rules.
fn part_class(
    part: &Intent,
    coverage: Coverage,
    ctx: &EvalContext<'_>,
    directed: bool,
) -> OperationClass {
    if directed {
        return OperationClass::Transfers;
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
    directed: bool,
) -> Result<OperationClass, Refusal> {
    let class = part_class(part, coverage, ctx, directed);
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
    let class = gates(rules, ctx, part, coverage, command.is_some())?;
    let deny = |reason: ReasonCode| refuse(reason, class);
    if coverage == Coverage::Unverified && (command.is_some() || class_is_limited(rules, class)) {
        return Err(deny(ReasonCode::ContentUnverifiable));
    }
    if let Some(fee) = part.fee_total_cc {
        tally.add_fee(fee, class)?;
    }
    let fees_apply = coverage == Coverage::Fields && command.is_none();
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
            for flow in &part.outgoing {
                if !is_self(flow, ctx.agent_party) {
                    tally.traffic = add(tally.traffic, flow.amount, class)?;
                }
            }
        }
        OperationClass::Trading => {
            let trading = &rules.trading;
            if let Some(market) = &part.market
                && !trading.markets.allows(market)
            {
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
                Some(verified) if !command_matches(&verified.command.op, part, ctx) => {
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

fn evaluate_inner(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    command: Option<&VerifiedCommand>,
) -> Result<(OperationClass, Vec<UsageDelta>), Refusal> {
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

/// Evaluates `intent` under `rules`; a verified command directs a transfer.
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

fn kind_gates(
    rules: &Rules,
    kind: OperationKind,
    directed: bool,
) -> Result<OperationClass, Refusal> {
    let class = if directed {
        OperationClass::Transfers
    } else {
        kind.class()
    };
    if rules.paused && !passes_pause(class, kind) {
        return Err(refuse(ReasonCode::Paused, class));
    }
    if !kind.is_unwind() && !class_enabled(rules, class) {
        return Err(refuse(ReasonCode::ClassDisabled, class));
    }
    if kind.requires_lp() && !rules.trading.lp_enabled {
        return Err(refuse(ReasonCode::LpDisabled, class));
    }
    directed_only(rules, class, directed)
}

/// The class, pause, switch and directed-only checks of a kind, before content is known.
pub fn precheck(rules: &Rules, kind: OperationKind, directed: bool) -> Decision {
    let static_class = kind.class();
    let result = match static_class {
        OperationClass::Identity | OperationClass::Container if directed => {
            Err(refuse(ReasonCode::CommandMismatch, static_class))
        }
        OperationClass::Identity | OperationClass::Container => Ok(static_class),
        OperationClass::Maintenance
        | OperationClass::Trading
        | OperationClass::Transfers
        | OperationClass::Locks
        | OperationClass::Usdcx
        | OperationClass::FeeConsents => kind_gates(rules, kind, directed),
    };
    decided(result)
}

/// As `precheck`, classifying by the flows the request names (taken as given).
pub fn precheck_intent(
    rules: &Rules,
    ctx: &EvalContext<'_>,
    intent: &Intent,
    directed: bool,
) -> Decision {
    let static_class = intent.kind.class();
    let result = match static_class {
        OperationClass::Identity if !identity_shape(intent) => {
            Err(refuse(ReasonCode::MalformedIntent, static_class))
        }
        OperationClass::Identity | OperationClass::Container if directed => {
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
                gates(rules, ctx, sub, Coverage::Fields, false)
                    .and_then(|class| directed_only(rules, class, false))
                    .map(|_| ())
            })
            .map(|()| static_class),
        _ if intent.kind.is_multicall_sub() || !intent.sub.is_empty() => {
            Err(refuse(ReasonCode::MalformedIntent, static_class))
        }
        _ => gates(rules, ctx, intent, Coverage::Fields, directed)
            .and_then(|class| directed_only(rules, class, directed)),
    };
    decided(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{InstrumentId, Network};
    use crate::command::{Command, CommandOp};
    use crate::controller::ControllerId;
    use crate::policy::{DEFAULT_LINKED_RULES, LimitItem};
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
        pinned: PlatformParties,
        none: PlatformParties,
        usage: UsageTotals,
    }

    impl Fixture {
        fn new() -> Self {
            let fee = party("fee", 7);
            let prepaid = party("prepaid", 8);
            Self {
                agent: testkit::agent_party(),
                cc: instrument(&party("DSO", 1), "Amulet"),
                usdc: instrument(&party("Bank", 2), "USDCx"),
                alice: party("alice", 3),
                bob: party("bob", 4),
                pinned: PlatformParties {
                    fee: Some(fee.clone()),
                    prepaid_traffic: Some(prepaid.clone()),
                },
                fee,
                prepaid,
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
            VerifiedCommand {
                hash: crate::canon::values::Hash32(crate::canon::values::sha256(&bytes)),
                bytes,
                command,
                controller_id: ControllerId([2; 32]),
                outcome: ProofOutcome {
                    sign_count: None,
                    backup_eligible: false,
                    backup_state: false,
                },
            }
        }
    }

    fn with_flows(kind: OperationKind, flows: Vec<Flow>) -> Intent {
        Intent {
            outgoing: flows,
            ..Intent::new(kind)
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
        let cases = [
            (K::TransferCc, false, R::DirectedOnly),
            (K::TransferCc, true, R::Allowed),
            (K::UsdcxBurn, false, R::DirectedOnly),
            (K::ProposeDvp, false, R::Allowed),
            (K::Jwt, false, R::Allowed),
            (K::Jwt, true, R::CommandMismatch),
            (K::ExecuteMultiCall, false, R::Allowed),
        ];
        for (kind, directed, expected) in cases {
            assert_eq!(precheck(&rules, kind, directed).reason, expected, "{kind}");
        }
        let mut paused = rules.clone();
        paused.paused = true;
        assert_eq!(precheck(&paused, K::TransferCc, true).reason, R::Paused);
        assert_eq!(precheck(&paused, K::RetireVenue, false).reason, R::Allowed);
        let merge = with_flows(K::TransferCip56, vec![f.flow(&f.usdc, "5", &f.agent)]);
        assert_eq!(
            precheck(&rules, K::TransferCip56, false).reason,
            R::DirectedOnly
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &merge, false).reason,
            R::Allowed
        );
        let batch = multicall(vec![with_flows(
            K::McTransfer,
            vec![f.flow(&f.cc, "1", &f.alice)],
        )]);
        assert_eq!(
            precheck_intent(&rules, &ctx, &batch, false).reason,
            R::DirectedOnly
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::McTransfer), false).reason,
            R::MalformedIntent
        );
        let jwt_with_flow = with_flows(K::Jwt, vec![f.flow(&f.cc, "1", &f.alice)]);
        assert_eq!(
            precheck_intent(&rules, &ctx, &jwt_with_flow, false).reason,
            R::MalformedIntent
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::Jwt), true).reason,
            R::CommandMismatch
        );
        assert_eq!(
            precheck_intent(&rules, &ctx, &Intent::new(K::ExecuteMultiCall), false).reason,
            R::MalformedIntent
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
        assert_eq!(ReasonCode::from_u16(19), None);
        let names: std::collections::HashSet<&str> =
            ReasonCode::ALL.iter().map(|r| r.code_name()).collect();
        assert_eq!(names.len(), ReasonCode::ALL.len());
        for bucket in UsageBucket::ALL {
            assert_eq!(UsageBucket::parse(bucket.as_str()), Some(bucket));
        }
    }
}
