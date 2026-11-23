//! Policy documents (`silvana.agent.policy.v1`): rules, defaults and submission checks.

pub mod eval;
pub mod kinds;
pub mod packages;
pub mod scopes;
pub mod summary;
pub mod widening;

use std::cmp::Ordering;

use crate::SCHEME_POLICY;
use crate::canon::time::UtcTime;
use crate::canon::values::{
    Amount, EthAddress, Hash32, InstrumentId, InstrumentRef, MarketId, Network, PackageId,
    format_bool, parse_bool, parse_u64, sha256,
};
use crate::canon::{Reader, Writer, check_ascending, decode_exact};
use crate::controller::{Capability, ControllerId};
use crate::error::{CanonError, CanonErrorKind, PolicyError, ValueError, ValueKind};
use crate::party::PartyId;
use crate::proof::{ProofEnvelope, verify_controller_proof};
use crate::state::{ControlState, add_secs};

pub use eval::{
    Coverage, Decision, EvalContext, Flow, Intent, PlatformParties, ReasonCode, Recipient,
    UsageBucket, UsageDelta, UsageRecord, UsageTotals, class_is_limited, evaluate, precheck,
    precheck_intent,
};
pub use kinds::{OperationClass, OperationKind};
pub use packages::check_packages;
pub use scopes::{
    CommandScope, MAX_SCOPE_COMMANDS, MAX_SCOPE_LPS, MAX_SCOPE_PAIRS, MAX_SCOPES, ScopePair,
    command_allowed, scope_of, scopes_widen,
};
pub use summary::describe_rules;
pub use widening::{Widening, expected_effective_at, required_delay_secs, widening};

/// Largest policy document, in bytes.
pub const MAX_POLICY_BYTES: usize = 65_536;
/// Longest time from `issued_at` to `expires_at`.
pub const MAX_POLICY_VALIDITY_SECS: u64 = 86_400;
/// Latest `not_before` after `issued_at`.
pub const MAX_NOT_BEFORE_SECS: u64 = 2_592_000;
/// Longest widening delay of any category.
pub const MAX_DELAY_SECS: u64 = 2_592_000;
/// How far `issued_at` may run ahead of the verifier's clock.
pub const POLICY_CLOCK_SKEW_SECS: u64 = 300;
pub const MAX_MARKETS: usize = 64;
pub const MAX_INSTRUMENTS: usize = 32;
pub const MAX_LIMIT_ITEMS: usize = 32;
pub const MAX_RECIPIENTS: usize = 100;
pub const MAX_ETH_DESTINATIONS: usize = 32;
/// Most package ids a `selected` package rule lists; it lists at least one.
pub const MAX_PACKAGES: usize = 256;

/// An instrument in a policy: the Canton Coin alias, or an admin party and id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PolicyInstrument {
    /// Written `cc`/`cc`; each enforcer resolves it from its own config.
    Cc,
    Token(InstrumentRef),
}

impl PolicyInstrument {
    pub const CC_ALIAS: &'static str = "cc";

    pub fn admin_str(&self) -> &str {
        match self {
            Self::Cc => Self::CC_ALIAS,
            Self::Token(r) => r.admin.as_str(),
        }
    }

    pub fn id_str(&self) -> &str {
        match self {
            Self::Cc => Self::CC_ALIAS,
            Self::Token(r) => r.id.as_str(),
        }
    }
}

impl Ord for PolicyInstrument {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.admin_str(), self.id_str()).cmp(&(other.admin_str(), other.id_str()))
    }
}

impl PartialOrd for PolicyInstrument {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The cap of an instrument a limits list does not name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unlisted {
    Unlimited,
    Deny,
}

impl Unlisted {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unlimited => "unlimited",
            Self::Deny => "deny",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "unlimited" => Ok(Self::Unlimited),
            "deny" => Ok(Self::Deny),
            _ => Err(ValueError(ValueKind::Unlisted)),
        }
    }
}

/// `any`, or exactly the listed items (strictly ascending).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Allowlist<T> {
    pub any: bool,
    pub items: Vec<T>,
}

impl<T> Allowlist<T> {
    pub const ANY: Self = Self {
        any: true,
        items: Vec::new(),
    };
}

impl<T: PartialEq> Allowlist<T> {
    pub fn allows(&self, item: &T) -> bool {
        self.any || self.items.contains(item)
    }
}

/// One listed cap; an amount of 0 allows nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LimitItem {
    pub instrument: PolicyInstrument,
    pub amount: Amount,
}

/// Per-instrument caps plus the cap of every unlisted instrument.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    pub unlisted: Unlisted,
    pub items: Vec<LimitItem>,
}

impl Limits {
    pub const UNLIMITED: Self = Self {
        unlisted: Unlisted::Unlimited,
        items: Vec::new(),
    };

    pub fn unlisted_cap(&self) -> CapValue {
        match self.unlisted {
            Unlisted::Unlimited => CapValue::Unlimited,
            Unlisted::Deny => CapValue::Denied,
        }
    }

    /// The listed amount of this exact ref, else the unlisted cap.
    pub fn cap_for(&self, instrument: &PolicyInstrument) -> CapValue {
        self.items
            .iter()
            .find(|item| item.instrument == *instrument)
            .map_or(self.unlisted_cap(), |item| CapValue::Amount(item.amount))
    }
}

/// Cap order for widening: Denied < Amount (numeric) < Unlimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapValue {
    Denied,
    Amount(Amount),
    Unlimited,
}

impl CapValue {
    /// An optional amount; absent means no cap.
    pub fn of_optional(amount: Option<Amount>) -> Self {
        amount.map_or(Self::Unlimited, Self::Amount)
    }
}

/// Widening delays per category, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Delays {
    pub transfers_secs: u64,
    pub trading_secs: u64,
    pub other_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MaintenanceRules {
    pub enabled: bool,
    pub traffic_cc_per_day: Option<Amount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TradingRules {
    pub enabled: bool,
    pub lp_enabled: bool,
    pub markets: Allowlist<MarketId>,
    pub instruments: Allowlist<PolicyInstrument>,
    pub per_trade: Limits,
    pub per_day: Limits,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransferRules {
    pub enabled: bool,
    pub directed_only: bool,
    pub recipients: Allowlist<PartyId>,
    pub eth_destinations: Allowlist<EthAddress>,
    pub per_transfer: Limits,
    pub per_day: Limits,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LockRules {
    pub enabled: bool,
    pub per_lock: Limits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UsdcxRules {
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FeeRules {
    pub cc_per_consent: Option<Amount>,
    pub cc_per_day: Option<Amount>,
}

/// Everything a policy decides, including pause and the widening delays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rules {
    pub paused: bool,
    pub delays: Delays,
    pub maintenance: MaintenanceRules,
    pub trading: TradingRules,
    pub transfers: TransferRules,
    pub locks: LockRules,
    pub usdcx: UsdcxRules,
    pub fees: FeeRules,
    /// The Daml packages a prepared transaction may use: any, or the listed ids.
    pub packages: Allowlist<PackageId>,
    /// Strictly ascending by controller; a direct controller without one is unscoped.
    pub scopes: Vec<CommandScope>,
}

/// The rules of a linked agent before any `accept_policy`.
pub const DEFAULT_LINKED_RULES: Rules = Rules {
    paused: false,
    delays: Delays {
        transfers_secs: 86_400,
        trading_secs: 0,
        other_secs: 0,
    },
    maintenance: MaintenanceRules {
        enabled: true,
        traffic_cc_per_day: None,
    },
    trading: TradingRules {
        enabled: true,
        lp_enabled: true,
        markets: Allowlist::ANY,
        instruments: Allowlist::ANY,
        per_trade: Limits::UNLIMITED,
        per_day: Limits::UNLIMITED,
    },
    transfers: TransferRules {
        enabled: true,
        directed_only: true,
        recipients: Allowlist::ANY,
        eth_destinations: Allowlist::ANY,
        per_transfer: Limits::UNLIMITED,
        per_day: Limits::UNLIMITED,
    },
    locks: LockRules {
        enabled: true,
        per_lock: Limits::UNLIMITED,
    },
    usdcx: UsdcxRules { enabled: true },
    fees: FeeRules {
        cc_per_consent: None,
        cc_per_day: None,
    },
    packages: Allowlist::ANY,
    scopes: Vec::new(),
};

/// A policy a `policy` controller signs and the agent accepts into its log.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PolicyDocument {
    pub network: Network,
    pub agent_party: PartyId,
    pub version: u64,
    pub prev_policy_hash: Hash32,
    pub issued_at: UtcTime,
    pub expires_at: UtcTime,
    pub not_before: Option<UtcTime>,
    pub rules: Rules,
}

fn validity_ok(issued_at: UtcTime, expires_at: UtcTime) -> bool {
    u64::try_from(issued_at.secs_until(expires_at))
        .is_ok_and(|span| (1..=MAX_POLICY_VALIDITY_SECS).contains(&span))
}

fn not_before_ok(issued_at: UtcTime, not_before: UtcTime) -> bool {
    u64::try_from(issued_at.secs_until(not_before)).is_ok_and(|span| span <= MAX_NOT_BEFORE_SECS)
}

impl PolicyDocument {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_POLICY, MAX_POLICY_BYTES);
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("version", &self.version.to_string());
        if self.version == 0 {
            w.fail(CanonErrorKind::InvalidVersion);
        }
        w.line("prev_policy_hash", &self.prev_policy_hash.to_string());
        w.line("issued_at", &self.issued_at.to_string());
        w.line("expires_at", &self.expires_at.to_string());
        if !validity_ok(self.issued_at, self.expires_at) {
            w.fail(CanonErrorKind::InvalidValidity);
        }
        w.opt(
            "not_before",
            self.not_before.map(|t| t.to_string()).as_deref(),
        );
        if self
            .not_before
            .is_some_and(|t| !not_before_ok(self.issued_at, t))
        {
            w.fail(CanonErrorKind::InvalidNotBefore);
        }
        write_rules(&mut w, &self.rules);
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_POLICY,
            MAX_POLICY_BYTES,
            Self::read,
            Self::encode,
        )
    }

    /// SHA-256 of the encoded document.
    pub fn hash(&self) -> Result<Hash32, CanonError> {
        self.encode().map(|bytes| Hash32(sha256(&bytes)))
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let version = r.value("version", parse_u64)?;
        if version == 0 {
            return Err(r.err(CanonErrorKind::InvalidVersion));
        }
        let prev_policy_hash = r.value("prev_policy_hash", Hash32::parse)?;
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        let expires_at = r.value("expires_at", UtcTime::parse)?;
        if !validity_ok(issued_at, expires_at) {
            return Err(r.err(CanonErrorKind::InvalidValidity));
        }
        let not_before = r.opt_value("not_before", UtcTime::parse)?;
        if not_before.is_some_and(|t| !not_before_ok(issued_at, t)) {
            return Err(r.err(CanonErrorKind::InvalidNotBefore));
        }
        let rules = read_rules(r)?;
        Ok(Self {
            network,
            agent_party,
            version,
            prev_policy_hash,
            issued_at,
            expires_at,
            not_before,
            rules,
        })
    }
}

fn amount_text(amount: Option<Amount>) -> Option<String> {
    amount.map(|a| a.to_string())
}

fn write_delay(w: &mut Writer, key: &str, secs: u64) {
    w.line(key, &secs.to_string());
    if secs > MAX_DELAY_SECS {
        w.fail(CanonErrorKind::DelayOutOfRange);
    }
}

fn write_allowlist<T: Ord>(
    w: &mut Writer,
    path: &str,
    list: &Allowlist<T>,
    max: usize,
    write_item: impl Fn(&mut Writer, &str, &T),
) {
    w.line(&format!("{path}.any"), format_bool(list.any));
    w.count(path, list.items.len());
    let limit = if list.any { 0 } else { max };
    if list.items.len() > limit {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&T> = None;
    for (i, item) in list.items.iter().enumerate() {
        write_item(w, &format!("{path}.{i}"), item);
        if let Some(kind) = check_ascending(previous, item) {
            w.fail(kind);
        }
        previous = Some(item);
    }
}

fn write_instrument(w: &mut Writer, prefix: &str, instrument: &PolicyInstrument) {
    w.line(&format!("{prefix}.admin"), instrument.admin_str());
    w.line(&format!("{prefix}.id"), instrument.id_str());
}

fn write_limits(w: &mut Writer, path: &str, limits: &Limits) {
    w.line(&format!("{path}.unlisted"), limits.unlisted.as_str());
    w.count(path, limits.items.len());
    if limits.items.len() > MAX_LIMIT_ITEMS {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&PolicyInstrument> = None;
    for (i, item) in limits.items.iter().enumerate() {
        let prefix = format!("{path}.{i}");
        write_instrument(w, &prefix, &item.instrument);
        if let Some(kind) = check_ascending(previous, &item.instrument) {
            w.fail(kind);
        }
        previous = Some(&item.instrument);
        w.line(&format!("{prefix}.amount"), &item.amount.to_string());
    }
}

fn write_rules(w: &mut Writer, rules: &Rules) {
    w.line("paused", format_bool(rules.paused));
    write_delay(w, "delay.transfers_secs", rules.delays.transfers_secs);
    write_delay(w, "delay.trading_secs", rules.delays.trading_secs);
    write_delay(w, "delay.other_secs", rules.delays.other_secs);
    let maintenance = &rules.maintenance;
    w.line("maintenance.enabled", format_bool(maintenance.enabled));
    w.opt(
        "maintenance.traffic_cc_per_day",
        amount_text(maintenance.traffic_cc_per_day).as_deref(),
    );
    let trading = &rules.trading;
    w.line("trading.enabled", format_bool(trading.enabled));
    w.line("trading.lp_enabled", format_bool(trading.lp_enabled));
    write_allowlist(
        w,
        "trading.markets",
        &trading.markets,
        MAX_MARKETS,
        |w, key, market| w.line(key, market.as_str()),
    );
    write_allowlist(
        w,
        "trading.instruments",
        &trading.instruments,
        MAX_INSTRUMENTS,
        write_instrument,
    );
    write_limits(w, "trading.per_trade", &trading.per_trade);
    write_limits(w, "trading.per_day", &trading.per_day);
    let transfers = &rules.transfers;
    w.line("transfers.enabled", format_bool(transfers.enabled));
    w.line(
        "transfers.directed_only",
        format_bool(transfers.directed_only),
    );
    write_allowlist(
        w,
        "transfers.recipients",
        &transfers.recipients,
        MAX_RECIPIENTS,
        |w, key, party| w.line(key, party.as_str()),
    );
    write_allowlist(
        w,
        "transfers.eth_destinations",
        &transfers.eth_destinations,
        MAX_ETH_DESTINATIONS,
        |w, key, address| w.line(key, &address.to_string()),
    );
    write_limits(w, "transfers.per_transfer", &transfers.per_transfer);
    write_limits(w, "transfers.per_day", &transfers.per_day);
    w.line("locks.enabled", format_bool(rules.locks.enabled));
    write_limits(w, "locks.per_lock", &rules.locks.per_lock);
    w.line("usdcx.enabled", format_bool(rules.usdcx.enabled));
    w.opt(
        "fees.cc_per_consent",
        amount_text(rules.fees.cc_per_consent).as_deref(),
    );
    w.opt(
        "fees.cc_per_day",
        amount_text(rules.fees.cc_per_day).as_deref(),
    );
    write_allowlist(
        w,
        "packages",
        &rules.packages,
        MAX_PACKAGES,
        |w, key, id| w.line(key, &id.to_string()),
    );
    if !rules.packages.any && rules.packages.items.is_empty() {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    scopes::write_scopes(w, &rules.scopes);
}

fn read_delay(r: &mut Reader<'_>, key: &str) -> Result<u64, CanonError> {
    let secs = r.value(key, parse_u64)?;
    if secs > MAX_DELAY_SECS {
        return Err(r.err(CanonErrorKind::DelayOutOfRange));
    }
    Ok(secs)
}

fn read_allowlist<'a, T: Ord>(
    r: &mut Reader<'a>,
    path: &str,
    max: usize,
    read_item: impl Fn(&mut Reader<'a>, &str) -> Result<T, CanonError>,
) -> Result<Allowlist<T>, CanonError> {
    let any = r.value(&format!("{path}.any"), parse_bool)?;
    let count = r.count(path, 0, if any { 0 } else { max })?;
    let mut items: Vec<T> = Vec::with_capacity(count);
    for i in 0..count {
        let item = read_item(r, &format!("{path}.{i}"))?;
        if let Some(kind) = check_ascending(items.last(), &item) {
            return Err(r.err(kind));
        }
        items.push(item);
    }
    Ok(Allowlist { any, items })
}

enum Admin {
    Cc,
    Party(PartyId),
}

fn parse_admin(s: &str) -> Result<Admin, ValueError> {
    if s == PolicyInstrument::CC_ALIAS {
        return Ok(Admin::Cc);
    }
    PartyId::parse(s)
        .map(Admin::Party)
        .map_err(|_| ValueError(ValueKind::InstrumentAdmin))
}

fn read_instrument(r: &mut Reader<'_>, prefix: &str) -> Result<PolicyInstrument, CanonError> {
    let admin = r.value(&format!("{prefix}.admin"), parse_admin)?;
    let id = r.value(&format!("{prefix}.id"), InstrumentId::parse)?;
    match admin {
        Admin::Cc if id.as_str() == PolicyInstrument::CC_ALIAS => Ok(PolicyInstrument::Cc),
        Admin::Cc => Err(r.err(CanonErrorKind::CcAliasMismatch)),
        Admin::Party(admin) => Ok(PolicyInstrument::Token(InstrumentRef { admin, id })),
    }
}

fn read_limits(r: &mut Reader<'_>, path: &str) -> Result<Limits, CanonError> {
    let unlisted = r.value(&format!("{path}.unlisted"), Unlisted::parse)?;
    let count = r.count(path, 0, MAX_LIMIT_ITEMS)?;
    let mut items: Vec<LimitItem> = Vec::with_capacity(count);
    for i in 0..count {
        let prefix = format!("{path}.{i}");
        let instrument = read_instrument(r, &prefix)?;
        if let Some(kind) = check_ascending(items.last().map(|item| &item.instrument), &instrument)
        {
            return Err(r.err(kind));
        }
        let amount = r.value(&format!("{prefix}.amount"), Amount::parse)?;
        items.push(LimitItem { instrument, amount });
    }
    Ok(Limits { unlisted, items })
}

/// `any` lists nothing; a selection lists 1 to `MAX_PACKAGES` ids.
fn read_packages(r: &mut Reader<'_>) -> Result<Allowlist<PackageId>, CanonError> {
    let any = r.value("packages.any", parse_bool)?;
    let (min, max) = if any { (0, 0) } else { (1, MAX_PACKAGES) };
    let count = r.count("packages", min, max)?;
    let mut items: Vec<PackageId> = Vec::with_capacity(count);
    for i in 0..count {
        let item = r.value(&format!("packages.{i}"), PackageId::parse)?;
        if let Some(kind) = check_ascending(items.last(), &item) {
            return Err(r.err(kind));
        }
        items.push(item);
    }
    Ok(Allowlist { any, items })
}

fn read_rules(r: &mut Reader<'_>) -> Result<Rules, CanonError> {
    let paused = r.value("paused", parse_bool)?;
    let delays = Delays {
        transfers_secs: read_delay(r, "delay.transfers_secs")?,
        trading_secs: read_delay(r, "delay.trading_secs")?,
        other_secs: read_delay(r, "delay.other_secs")?,
    };
    let maintenance = MaintenanceRules {
        enabled: r.value("maintenance.enabled", parse_bool)?,
        traffic_cc_per_day: r.opt_value("maintenance.traffic_cc_per_day", Amount::parse)?,
    };
    let trading = TradingRules {
        enabled: r.value("trading.enabled", parse_bool)?,
        lp_enabled: r.value("trading.lp_enabled", parse_bool)?,
        markets: read_allowlist(r, "trading.markets", MAX_MARKETS, |r, key| {
            r.value(key, MarketId::parse)
        })?,
        instruments: read_allowlist(r, "trading.instruments", MAX_INSTRUMENTS, read_instrument)?,
        per_trade: read_limits(r, "trading.per_trade")?,
        per_day: read_limits(r, "trading.per_day")?,
    };
    let transfers = TransferRules {
        enabled: r.value("transfers.enabled", parse_bool)?,
        directed_only: r.value("transfers.directed_only", parse_bool)?,
        recipients: read_allowlist(r, "transfers.recipients", MAX_RECIPIENTS, |r, key| {
            r.value(key, PartyId::parse)
        })?,
        eth_destinations: read_allowlist(
            r,
            "transfers.eth_destinations",
            MAX_ETH_DESTINATIONS,
            |r, key| r.value(key, EthAddress::parse),
        )?,
        per_transfer: read_limits(r, "transfers.per_transfer")?,
        per_day: read_limits(r, "transfers.per_day")?,
    };
    let locks = LockRules {
        enabled: r.value("locks.enabled", parse_bool)?,
        per_lock: read_limits(r, "locks.per_lock")?,
    };
    let usdcx = UsdcxRules {
        enabled: r.value("usdcx.enabled", parse_bool)?,
    };
    let fees = FeeRules {
        cc_per_consent: r.opt_value("fees.cc_per_consent", Amount::parse)?,
        cc_per_day: r.opt_value("fees.cc_per_day", Amount::parse)?,
    };
    let packages = read_packages(r)?;
    let scopes = scopes::read_scopes(r)?;
    Ok(Rules {
        paused,
        delays,
        maintenance,
        trading,
        transfers,
        locks,
        usdcx,
        fees,
        packages,
        scopes,
    })
}

/// A submitted policy that passed every check, with the timing it would get now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPolicy {
    pub document: PolicyDocument,
    pub bytes: Vec<u8>,
    pub hash: Hash32,
    pub signer: ControllerId,
    pub envelope: ProofEnvelope,
    pub widening: Widening,
    pub required_delay_secs: u64,
    pub effective_at: UtcTime,
}

/// Checks a signed policy against the current state before the agent accepts it.
pub fn verify_policy_submission(
    state: &ControlState,
    bytes: &[u8],
    envelope: &ProofEnvelope,
    now: UtcTime,
) -> Result<VerifiedPolicy, PolicyError> {
    let document = PolicyDocument::decode(bytes).map_err(PolicyError::Canon)?;
    if document.network != state.network() {
        return Err(PolicyError::WrongNetwork);
    }
    if document.agent_party != *state.agent_party() {
        return Err(PolicyError::WrongParty);
    }
    let signer = state
        .controller(envelope.controller_id())
        .ok_or(PolicyError::UnknownController)?;
    if !signer.capabilities_at(now).contains(Capability::Policy) {
        return Err(PolicyError::SignerLacksPolicy);
    }
    let latest = state.latest_policy();
    if document.version <= latest.map_or(0, |p| p.version()) {
        return Err(PolicyError::VersionNotIncreasing);
    }
    if document.prev_policy_hash != latest.map_or(Hash32([0; 32]), |p| *p.hash()) {
        return Err(PolicyError::PrevMismatch);
    }
    let latest_issue = add_secs(now, POLICY_CLOCK_SKEW_SECS).ok_or(PolicyError::TimeOverflow)?;
    if document.issued_at > latest_issue {
        return Err(PolicyError::IssuedInFuture);
    }
    if now > document.expires_at {
        return Err(PolicyError::Expired);
    }
    verify_controller_proof(signer.descriptor(), bytes, envelope).map_err(PolicyError::Proof)?;
    let unrecorded = document
        .rules
        .scopes
        .iter()
        .any(|scope| !state.is_recorded_controller(&scope.controller));
    if unrecorded {
        return Err(PolicyError::UnknownScopeController);
    }
    let baseline = state.rules_at(now).rules();
    let required_delay_secs =
        widening::required_delay_secs(document.network, baseline, &document.rules);
    let widening = widening(baseline, &document.rules);
    let effective_at =
        expected_effective_at(now, baseline, &document).ok_or(PolicyError::TimeOverflow)?;
    Ok(VerifiedPolicy {
        document,
        bytes: bytes.to_vec(),
        hash: Hash32(sha256(bytes)),
        signer: *envelope.controller_id(),
        envelope: envelope.clone(),
        widening,
        required_delay_secs,
        effective_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::{InstrumentRef, Label};
    use crate::command::CommandKind;
    use crate::controller::CapabilitySet;
    use crate::testkit::{self, Chain, DAY, DEVNET_DELAY, MAINNET_DELAY, at};

    fn party(hint: &str, key: u8) -> PartyId {
        PartyId::parse(&format!(
            "{hint}::{}",
            crate::party::namespace_for_key(&[key; 32])
        ))
        .unwrap()
    }

    fn token(admin: &PartyId, id: &str) -> PolicyInstrument {
        PolicyInstrument::Token(InstrumentRef {
            admin: admin.clone(),
            id: InstrumentId::parse(id).unwrap(),
        })
    }

    fn amount(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }

    fn document(rules: Rules) -> PolicyDocument {
        PolicyDocument {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            version: 1,
            prev_policy_hash: Hash32([0; 32]),
            issued_at: testkit::t("2026-11-22T12:00:00Z"),
            expires_at: testkit::t("2026-11-23T12:00:00Z"),
            not_before: None,
            rules,
        }
    }

    fn full_rules() -> Rules {
        let dso = party("DSO", 1);
        let usdc = party("Bank", 2);
        let mut rules = DEFAULT_LINKED_RULES;
        rules.paused = true;
        rules.delays = Delays {
            transfers_secs: 172_800,
            trading_secs: 60,
            other_secs: 3600,
        };
        rules.maintenance.traffic_cc_per_day = Some(amount("25.5"));
        rules.trading.lp_enabled = false;
        rules.trading.markets = Allowlist {
            any: false,
            items: vec![
                MarketId::parse("CC/USDCx").unwrap(),
                MarketId::parse("cETH/USDCx").unwrap(),
            ],
        };
        let mut instruments = vec![
            PolicyInstrument::Cc,
            token(&dso, "Amulet"),
            token(&usdc, "USDCx"),
        ];
        instruments.sort();
        rules.trading.instruments = Allowlist {
            any: false,
            items: instruments,
        };
        rules.trading.per_trade = Limits {
            unlisted: Unlisted::Deny,
            items: vec![
                LimitItem {
                    instrument: token(&usdc, "USDCx"),
                    amount: amount("1000"),
                },
                LimitItem {
                    instrument: PolicyInstrument::Cc,
                    amount: amount("0"),
                },
            ],
        };
        rules.trading.per_day.items = vec![LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount("5000.25"),
        }];
        rules.transfers.enabled = false;
        rules.transfers.recipients = Allowlist {
            any: false,
            items: vec![party("alice", 3), party("bob", 4)],
        };
        rules.transfers.eth_destinations = Allowlist {
            any: false,
            items: vec![EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7").unwrap()],
        };
        rules.transfers.per_transfer = Limits {
            unlisted: Unlisted::Deny,
            items: Vec::new(),
        };
        rules.locks.per_lock.items = vec![LimitItem {
            instrument: token(&dso, "Amulet"),
            amount: amount("1"),
        }];
        rules.usdcx.enabled = false;
        rules.fees = FeeRules {
            cc_per_consent: Some(amount("0.5")),
            cc_per_day: Some(amount("10")),
        };
        rules.packages = Allowlist {
            any: false,
            items: vec![PackageId([0x0a; 32]), PackageId([0x0b; 32])],
        };
        let mut pairs = vec![
            pair(PolicyInstrument::Cc, token(&usdc, "USDCx"), "0.15", "1000"),
            pair(token(&usdc, "USDCx"), PolicyInstrument::Cc, "6.5", "150.25"),
        ];
        pairs.sort_by(|a, b| (&a.sell, &a.buy).cmp(&(&b.sell, &b.buy)));
        rules.scopes = vec![
            CommandScope {
                controller: ControllerId([0xab; 32]),
                commands: vec![CommandKind::Swap, CommandKind::TransferToken],
                pairs,
                lps: vec![party("lp-a", 7), party("lp-b", 8)],
            },
            CommandScope {
                controller: ControllerId([0xcd; 32]),
                commands: Vec::new(),
                pairs: Vec::new(),
                lps: Vec::new(),
            },
        ];
        rules
    }

    fn pair(sell: PolicyInstrument, buy: PolicyInstrument, min: &str, max: &str) -> ScopePair {
        ScopePair {
            sell,
            buy,
            min_price: amount(min),
            max_sell: amount(max),
        }
    }

    fn text(doc: &PolicyDocument) -> String {
        String::from_utf8(doc.encode().unwrap()).unwrap()
    }

    fn refusal(text: &str) -> &'static str {
        PolicyDocument::decode(text.as_bytes())
            .unwrap_err()
            .code_name()
    }

    #[test]
    fn the_default_rules_encode_to_the_pinned_text() {
        let doc = document(DEFAULT_LINKED_RULES);
        let expected = format!(
            "scheme=silvana.agent.policy.v1\nnetwork=devnet\nagent_party={}\nversion=1\n\
             prev_policy_hash={}\nissued_at=2026-11-22T12:00:00Z\nexpires_at=2026-11-23T12:00:00Z\n\
             not_before=\npaused=false\ndelay.transfers_secs=86400\ndelay.trading_secs=0\n\
             delay.other_secs=0\nmaintenance.enabled=true\nmaintenance.traffic_cc_per_day=\n\
             trading.enabled=true\ntrading.lp_enabled=true\ntrading.markets.any=true\n\
             trading.markets.count=0\ntrading.instruments.any=true\ntrading.instruments.count=0\n\
             trading.per_trade.unlisted=unlimited\ntrading.per_trade.count=0\n\
             trading.per_day.unlisted=unlimited\ntrading.per_day.count=0\ntransfers.enabled=true\n\
             transfers.directed_only=true\ntransfers.recipients.any=true\n\
             transfers.recipients.count=0\ntransfers.eth_destinations.any=true\n\
             transfers.eth_destinations.count=0\ntransfers.per_transfer.unlisted=unlimited\n\
             transfers.per_transfer.count=0\ntransfers.per_day.unlisted=unlimited\n\
             transfers.per_day.count=0\nlocks.enabled=true\nlocks.per_lock.unlisted=unlimited\n\
             locks.per_lock.count=0\nusdcx.enabled=true\nfees.cc_per_consent=\nfees.cc_per_day=\n\
             packages.any=true\npackages.count=0\nscopes.count=0\n",
            testkit::agent_party(),
            "0".repeat(64)
        );
        assert_eq!(text(&doc), expected);
        assert_eq!(PolicyDocument::decode(expected.as_bytes()).unwrap(), doc);
        assert_eq!(doc.hash().unwrap(), Hash32(sha256(expected.as_bytes())));
    }

    #[test]
    fn a_scoped_package_selection_encodes_to_the_pinned_text() {
        let bank = party("Bank", 2);
        let lp = party("lp-a", 7);
        let mut rules = DEFAULT_LINKED_RULES;
        rules.packages = Allowlist {
            any: false,
            items: vec![PackageId([0x0a; 32])],
        };
        rules.scopes = vec![CommandScope {
            controller: ControllerId([0x11; 32]),
            commands: vec![CommandKind::Swap],
            pairs: vec![pair(
                PolicyInstrument::Cc,
                token(&bank, "USDCx"),
                "0.15",
                "1000",
            )],
            lps: vec![lp.clone()],
        }];
        let doc = document(rules);
        let default = text(&document(DEFAULT_LINKED_RULES));
        let head = default
            .strip_suffix("packages.any=true\npackages.count=0\nscopes.count=0\n")
            .unwrap();
        let expected = format!(
            "{head}packages.any=false\npackages.count=1\npackages.0={}\nscopes.count=1\n\
             scopes.0.controller={}\nscopes.0.commands.count=1\nscopes.0.commands.0=swap\n\
             scopes.0.pairs.count=1\nscopes.0.pairs.0.sell.admin=cc\nscopes.0.pairs.0.sell.id=cc\n\
             scopes.0.pairs.0.buy.admin={bank}\nscopes.0.pairs.0.buy.id=USDCx\n\
             scopes.0.pairs.0.min_price=0.15\nscopes.0.pairs.0.max_sell=1000\n\
             scopes.0.lps.count=1\nscopes.0.lps.0={lp}\n",
            "0a".repeat(32),
            "11".repeat(32),
        );
        assert_eq!(text(&doc), expected);
        assert_eq!(PolicyDocument::decode(expected.as_bytes()).unwrap(), doc);
    }

    #[test]
    fn every_rule_field_round_trips() {
        let mut doc = document(full_rules());
        doc.not_before = Some(testkit::t("2026-11-25T00:00:00Z"));
        let encoded = text(&doc);
        assert_eq!(PolicyDocument::decode(encoded.as_bytes()).unwrap(), doc);
        assert!(encoded.contains("trading.instruments.0.admin=Bank::"));
        assert!(encoded.contains("\ntrading.per_trade.1.admin=cc\ntrading.per_trade.1.id=cc\ntrading.per_trade.1.amount=0\n"));
        assert!(encoded.contains("\nnot_before=2026-11-25T00:00:00Z\n"));
        assert!(encoded.contains("\nfees.cc_per_consent=0.5\nfees.cc_per_day=10\n"));
        assert!(encoded.contains(&format!(
            "\nfees.cc_per_day=10\npackages.any=false\npackages.count=2\npackages.0={}\npackages.1={}\nscopes.count=2\n",
            "0a".repeat(32),
            "0b".repeat(32)
        )));
        assert!(encoded.contains(
            "\nscopes.0.commands.count=2\nscopes.0.commands.0=swap\nscopes.0.commands.1=transfer_token\nscopes.0.pairs.count=2\n"
        ));
        assert!(
            encoded
                .contains("\nscopes.0.pairs.0.min_price=6.5\nscopes.0.pairs.0.max_sell=150.25\n")
        );
        assert!(encoded.ends_with(&format!(
            "\nscopes.1.controller={}\nscopes.1.commands.count=0\nscopes.1.pairs.count=0\nscopes.1.lps.count=0\n",
            "cd".repeat(32)
        )));
    }

    #[test]
    fn decoding_refusals_have_stable_names() {
        let base = text(&document(full_rules()));
        let default = text(&document(DEFAULT_LINKED_RULES));
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "version 0",
                default.replacen("version=1", "version=0", 1),
                "invalid_version",
            ),
            (
                "validity",
                default.replacen(
                    "expires_at=2026-11-23T12:00:00Z",
                    "expires_at=2026-11-23T12:00:01Z",
                    1,
                ),
                "invalid_validity",
            ),
            (
                "zero validity",
                default.replacen(
                    "expires_at=2026-11-23T12:00:00Z",
                    "expires_at=2026-11-22T12:00:00Z",
                    1,
                ),
                "invalid_validity",
            ),
            (
                "not_before early",
                default.replacen("not_before=", "not_before=2026-11-22T11:59:59Z", 1),
                "invalid_not_before",
            ),
            (
                "not_before late",
                default.replacen("not_before=", "not_before=2026-12-22T12:00:01Z", 1),
                "invalid_not_before",
            ),
            (
                "delay",
                default.replacen("delay.other_secs=0", "delay.other_secs=2592001", 1),
                "delay_out_of_range",
            ),
            (
                "any with items",
                default.replacen(
                    "trading.markets.count=0",
                    "trading.markets.count=1\ntrading.markets.0=CC/USDCx",
                    1,
                ),
                "count_out_of_range",
            ),
            (
                "unlisted",
                default.replacen(
                    "locks.per_lock.unlisted=unlimited",
                    "locks.per_lock.unlisted=none",
                    1,
                ),
                "invalid_unlisted",
            ),
            (
                "unsorted",
                base.replacen(
                    "trading.markets.0=CC/USDCx\ntrading.markets.1=cETH/USDCx",
                    "trading.markets.0=cETH/USDCx\ntrading.markets.1=CC/USDCx",
                    1,
                ),
                "unsorted_list",
            ),
            (
                "duplicate",
                base.replacen(
                    "trading.markets.1=cETH/USDCx",
                    "trading.markets.1=CC/USDCx",
                    1,
                ),
                "duplicate_item",
            ),
            (
                "alias",
                base.replacen(
                    "trading.per_trade.1.id=cc",
                    "trading.per_trade.1.id=Amulet",
                    1,
                ),
                "invalid_cc_alias",
            ),
            (
                "admin",
                base.replacen(
                    "trading.per_trade.1.admin=cc",
                    "trading.per_trade.1.admin=CC",
                    1,
                ),
                "invalid_instrument_admin",
            ),
            (
                "amount",
                base.replacen("fees.cc_per_day=10", "fees.cc_per_day=10.0", 1),
                "invalid_amount",
            ),
            (
                "recipient",
                base.replacen(
                    "transfers.recipients.0=alice",
                    "transfers.recipients.0=al ice",
                    1,
                ),
                "invalid_party",
            ),
            (
                "scheme",
                default.replacen("policy.v1", "policy.v2", 1),
                "wrong_scheme",
            ),
            ("trailing", format!("{default}extra=1\n"), "trailing_line"),
        ];
        for (name, t, expected) in cases {
            assert_eq!(refusal(&t), expected, "{name}");
        }
        let mut huge = document(DEFAULT_LINKED_RULES);
        let long_limits = Limits {
            unlisted: Unlisted::Deny,
            items: (0..32u8)
                .map(|i| LimitItem {
                    instrument: token(&party(&format!("h{i:02}{}", "x".repeat(180)), i), "Amulet"),
                    amount: amount("1"),
                })
                .collect(),
        };
        huge.rules.trading.per_trade = long_limits.clone();
        huge.rules.trading.per_day = long_limits.clone();
        huge.rules.transfers.per_transfer = long_limits.clone();
        huge.rules.transfers.per_day = long_limits.clone();
        huge.rules.locks.per_lock = long_limits;
        huge.rules.transfers.recipients = Allowlist {
            any: false,
            items: (0..100u8)
                .map(|i| party(&format!("r{i:03}{}", "x".repeat(180)), i))
                .collect(),
        };
        assert_eq!(huge.encode().unwrap_err().code_name(), "too_large");
        let padded = format!("{default}{}", "x=1\n".repeat(20_000));
        assert_eq!(refusal(&padded), "too_large");
        let mut unsorted = document(DEFAULT_LINKED_RULES);
        unsorted.rules.trading.per_day.items = vec![
            LimitItem {
                instrument: PolicyInstrument::Cc,
                amount: amount("1"),
            },
            LimitItem {
                instrument: PolicyInstrument::Cc,
                amount: amount("2"),
            },
        ];
        assert_eq!(unsorted.encode().unwrap_err().code_name(), "duplicate_item");
    }

    #[test]
    fn package_and_scope_refusals_have_stable_names() {
        let base = text(&document(full_rules()));
        let default = text(&document(DEFAULT_LINKED_RULES));
        let bank = party("Bank", 2).to_string();
        let (lp_a, lp_b) = (party("lp-a", 7).to_string(), party("lp-b", 8).to_string());
        let (p0, p1) = ("0a".repeat(32), "0b".repeat(32));
        let (c0, c1) = ("ab".repeat(32), "cd".repeat(32));
        let line = |key: &str, value: &str| format!("\n{key}={value}\n");
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "package 63 hex",
                base.replacen(&line("packages.0", &p0), &line("packages.0", &p0[..63]), 1),
                "invalid_package_id",
            ),
            (
                "package uppercase",
                base.replacen(
                    &line("packages.0", &p0),
                    &line("packages.0", &p0.to_uppercase()),
                    1,
                ),
                "invalid_package_id",
            ),
            (
                "packages unsorted",
                base.replacen(&line("packages.0", &p0), &line("packages.0", &p1), 1)
                    .replacen(&line("packages.1", &p1), &line("packages.1", &p0), 1),
                "unsorted_list",
            ),
            (
                "packages duplicate",
                base.replacen(&line("packages.1", &p1), &line("packages.1", &p0), 1),
                "duplicate_item",
            ),
            (
                "any with packages",
                base.replacen("\npackages.any=false\n", "\npackages.any=true\n", 1),
                "count_out_of_range",
            ),
            (
                "selected with none",
                default.replacen("\npackages.any=true\n", "\npackages.any=false\n", 1),
                "count_out_of_range",
            ),
            (
                "257 packages",
                base.replacen("\npackages.count=2\n", "\npackages.count=257\n", 1),
                "count_out_of_range",
            ),
            (
                "33 scopes",
                base.replacen("\nscopes.count=2\n", "\nscopes.count=33\n", 1),
                "count_out_of_range",
            ),
            (
                "controllers unsorted",
                base.replacen(
                    &line("scopes.0.controller", &c0),
                    &line("scopes.0.controller", &c1),
                    1,
                )
                .replacen(
                    &line("scopes.1.controller", &c1),
                    &line("scopes.1.controller", &c0),
                    1,
                ),
                "unsorted_list",
            ),
            (
                "controller duplicate",
                base.replacen(
                    &line("scopes.1.controller", &c1),
                    &line("scopes.1.controller", &c0),
                    1,
                ),
                "duplicate_item",
            ),
            (
                "controller uppercase",
                base.replacen(
                    &line("scopes.0.controller", &c0),
                    &line("scopes.0.controller", &c0.to_uppercase()),
                    1,
                ),
                "invalid_hex32",
            ),
            (
                "unknown kind",
                base.replacen(
                    "\nscopes.0.commands.0=swap\n",
                    "\nscopes.0.commands.0=transfer_all\n",
                    1,
                ),
                "invalid_command_op",
            ),
            (
                "5 kinds",
                base.replacen(
                    "\nscopes.0.commands.count=2\n",
                    "\nscopes.0.commands.count=5\n",
                    1,
                ),
                "count_out_of_range",
            ),
            (
                "kinds unsorted",
                base.replacen(
                    "\nscopes.0.commands.0=swap\nscopes.0.commands.1=transfer_token\n",
                    "\nscopes.0.commands.0=transfer_token\nscopes.0.commands.1=swap\n",
                    1,
                ),
                "unsorted_list",
            ),
            (
                "kinds duplicate",
                base.replacen(
                    "\nscopes.0.commands.1=transfer_token\n",
                    "\nscopes.0.commands.1=swap\n",
                    1,
                ),
                "duplicate_item",
            ),
            (
                "17 pairs",
                base.replacen(
                    "\nscopes.0.pairs.count=2\n",
                    "\nscopes.0.pairs.count=17\n",
                    1,
                ),
                "count_out_of_range",
            ),
            (
                "same instrument",
                base.replacen(
                    "\nscopes.0.pairs.0.buy.admin=cc\nscopes.0.pairs.0.buy.id=cc\n",
                    &format!(
                        "\nscopes.0.pairs.0.buy.admin={bank}\nscopes.0.pairs.0.buy.id=USDCx\n"
                    ),
                    1,
                ),
                "same_instrument",
            ),
            (
                "pairs unsorted",
                base.replacen(
                    &line("scopes.0.pairs.0.sell.admin", &bank),
                    &line("scopes.0.pairs.0.sell.admin", &lp_a),
                    1,
                ),
                "unsorted_list",
            ),
            (
                "pairs duplicate",
                base.replacen(
                    &format!(
                        "\nscopes.0.pairs.1.sell.admin=cc\nscopes.0.pairs.1.sell.id=cc\n\
                         scopes.0.pairs.1.buy.admin={bank}\nscopes.0.pairs.1.buy.id=USDCx\n"
                    ),
                    &format!(
                        "\nscopes.0.pairs.1.sell.admin={bank}\nscopes.0.pairs.1.sell.id=USDCx\n\
                         scopes.0.pairs.1.buy.admin=cc\nscopes.0.pairs.1.buy.id=cc\n"
                    ),
                    1,
                ),
                "duplicate_item",
            ),
            (
                "pair alias",
                base.replacen(
                    "\nscopes.0.pairs.0.buy.id=cc\n",
                    "\nscopes.0.pairs.0.buy.id=Amulet\n",
                    1,
                ),
                "invalid_cc_alias",
            ),
            (
                "min price",
                base.replacen(
                    "\nscopes.0.pairs.0.min_price=6.5\n",
                    "\nscopes.0.pairs.0.min_price=6.50\n",
                    1,
                ),
                "invalid_amount",
            ),
            (
                "max sell",
                base.replacen(
                    "\nscopes.0.pairs.0.max_sell=150.25\n",
                    "\nscopes.0.pairs.0.max_sell=-1\n",
                    1,
                ),
                "invalid_amount",
            ),
            (
                "17 lps",
                base.replacen("\nscopes.0.lps.count=2\n", "\nscopes.0.lps.count=17\n", 1),
                "count_out_of_range",
            ),
            (
                "lps unsorted",
                base.replacen(
                    &line("scopes.0.lps.0", &lp_a),
                    &line("scopes.0.lps.0", &lp_b),
                    1,
                )
                .replacen(
                    &line("scopes.0.lps.1", &lp_b),
                    &line("scopes.0.lps.1", &lp_a),
                    1,
                ),
                "unsorted_list",
            ),
            (
                "lps duplicate",
                base.replacen(
                    &line("scopes.0.lps.1", &lp_b),
                    &line("scopes.0.lps.1", &lp_a),
                    1,
                ),
                "duplicate_item",
            ),
            (
                "lp party",
                base.replacen("\nscopes.0.lps.0=lp-a::", "\nscopes.0.lps.0=lp a::", 1),
                "invalid_party",
            ),
            (
                "scope keys out of order",
                base.replacen(
                    "\nscopes.1.commands.count=0\nscopes.1.pairs.count=0\n",
                    "\nscopes.1.pairs.count=0\nscopes.1.commands.count=0\n",
                    1,
                ),
                "unexpected_key",
            ),
            (
                "scope keys missing",
                base.strip_suffix("scopes.1.lps.count=0\n")
                    .unwrap()
                    .to_owned(),
                "missing_line",
            ),
            (
                "scopes before packages",
                default.replacen(
                    "packages.any=true\npackages.count=0\nscopes.count=0\n",
                    "scopes.count=0\npackages.any=true\npackages.count=0\n",
                    1,
                ),
                "unexpected_key",
            ),
        ];
        for (name, t, expected) in cases {
            assert!(
                t != base && t != default,
                "{name}: the replacement did not apply"
            );
            assert_eq!(refusal(&t), expected, "{name}");
        }
        let codes = |rules: Rules| {
            let e = document(rules).encode().unwrap_err();
            (e.code_name(), e.code())
        };
        let ids = |n: u16| -> Vec<PackageId> {
            (0..n)
                .map(|i| {
                    let mut bytes = [0u8; 32];
                    for (byte, value) in bytes.iter_mut().zip(i.to_be_bytes()) {
                        *byte = value;
                    }
                    PackageId(bytes)
                })
                .collect()
        };
        let mut many = full_rules();
        many.packages.items = ids(256);
        let encoded = document(many.clone()).encode().unwrap();
        assert_eq!(PolicyDocument::decode(&encoded).unwrap().rules, many);
        many.packages.items = ids(257);
        assert_eq!(codes(many), ("count_out_of_range", 111));
        let mut none = full_rules();
        none.packages.items.clear();
        assert_eq!(codes(none), ("count_out_of_range", 111));
        let mut any_listed = full_rules();
        any_listed.packages.any = true;
        assert_eq!(codes(any_listed), ("count_out_of_range", 111));
        let mut unsorted = full_rules();
        unsorted.packages.items.reverse();
        assert_eq!(codes(unsorted), ("unsorted_list", 112));
        let scope = |controller: u8| CommandScope {
            controller: ControllerId([controller; 32]),
            commands: Vec::new(),
            pairs: Vec::new(),
            lps: Vec::new(),
        };
        let mut scopes = DEFAULT_LINKED_RULES;
        scopes.scopes = (0..32u8).map(scope).collect();
        assert!(document(scopes.clone()).encode().is_ok());
        scopes.scopes.push(scope(32));
        assert_eq!(codes(scopes), ("count_out_of_range", 111));
        let mut reversed = full_rules();
        reversed.scopes.reverse();
        assert_eq!(codes(reversed), ("unsorted_list", 112));
        let mut kinds = full_rules();
        kinds.scopes[1].commands = vec![CommandKind::Swap; 5];
        assert_eq!(codes(kinds), ("count_out_of_range", 111));
        let mut repeated = full_rules();
        repeated.scopes[1].commands = vec![CommandKind::UsdcxBurn, CommandKind::UsdcxBurn];
        assert_eq!(codes(repeated), ("duplicate_item", 113));
        let mut same = full_rules();
        same.scopes[0].pairs[0].buy = same.scopes[0].pairs[0].sell.clone();
        assert_eq!(codes(same), ("same_instrument", 131));
        let mut pairs = full_rules();
        pairs.scopes[1].pairs = (0..17u8)
            .map(|i| {
                pair(
                    PolicyInstrument::Cc,
                    token(&party(&format!("issuer-{i:02}"), i), "X"),
                    "1",
                    "1",
                )
            })
            .collect();
        assert_eq!(codes(pairs.clone()), ("count_out_of_range", 111));
        pairs.scopes[1].pairs.pop();
        assert!(document(pairs).encode().is_ok());
        let mut lps = full_rules();
        lps.scopes[1].lps = (0..17u8).map(|i| party(&format!("lp-{i:02}"), i)).collect();
        assert_eq!(codes(lps.clone()), ("count_out_of_range", 111));
        lps.scopes[1].lps.pop();
        assert!(document(lps.clone()).encode().is_ok());
        lps.scopes[1].lps.swap(0, 1);
        assert_eq!(codes(lps), ("unsorted_list", 112));
        assert_eq!(
            PolicyDocument::decode(
                base.replacen(&line("packages.0", &p0), &line("packages.0", &p0[..63]), 1)
                    .as_bytes()
            )
            .unwrap_err()
            .code(),
            188
        );
    }

    #[test]
    fn submissions_are_checked_against_the_state() {
        let chain = Chain::linked();
        let state = &chain.state;
        let now = at(2 * DAY);
        let doc = testkit::policy_doc(state, 1, now, testkit::undirected_rules());
        let bytes = doc.encode().unwrap();
        let envelope = testkit::passkey_envelope(&bytes);
        let verified = verify_policy_submission(state, &bytes, &envelope, now).unwrap();
        assert_eq!(verified.signer, testkit::passkey_id());
        assert!(verified.widening.transfers && !verified.widening.trading);
        assert_eq!(verified.required_delay_secs, 86_400);
        assert_eq!(verified.effective_at, at(3 * DAY));
        assert_eq!(verified.hash, Hash32(sha256(&bytes)));
        let refuse =
            |doc: &PolicyDocument, envelope_for: &dyn Fn(&[u8]) -> ProofEnvelope, when: UtcTime| {
                let bytes = doc.encode().unwrap();
                verify_policy_submission(state, &bytes, &envelope_for(&bytes), when).unwrap_err()
            };
        let passkey = |b: &[u8]| testkit::passkey_envelope(b);
        assert_eq!(
            refuse(&doc, &passkey, at(DEVNET_DELAY - 1)),
            PolicyError::SignerLacksPolicy
        );
        assert_eq!(
            refuse(&doc, &|b: &[u8]| testkit::ed25519_envelope(0x21, b), now),
            PolicyError::SignerLacksPolicy
        );
        assert_eq!(
            refuse(&doc, &|b: &[u8]| testkit::ed25519_envelope(0x99, b), now),
            PolicyError::UnknownController
        );
        assert_eq!(
            refuse(&doc, &passkey, at(2 * DAY + 3601)),
            PolicyError::Expired
        );
        assert_eq!(
            refuse(&doc, &passkey, at(2 * DAY - 301)),
            PolicyError::IssuedInFuture
        );
        let mut mainnet = doc.clone();
        mainnet.network = Network::Mainnet;
        assert_eq!(refuse(&mainnet, &passkey, now), PolicyError::WrongNetwork);
        let mut unchained = doc.clone();
        unchained.prev_policy_hash = Hash32([5; 32]);
        assert_eq!(refuse(&unchained, &passkey, now), PolicyError::PrevMismatch);
        let wrong_payload =
            verify_policy_submission(state, &bytes, &testkit::passkey_envelope(b"other"), now);
        assert_eq!(wrong_payload.unwrap_err().code_name(), "payload_mismatch");
        assert_eq!(
            verify_policy_submission(state, b"junk", &envelope, now)
                .unwrap_err()
                .code_name(),
            "too_short"
        );
    }

    fn scope_for(controller: ControllerId, commands: &[CommandKind]) -> CommandScope {
        CommandScope {
            controller,
            commands: commands.to_vec(),
            pairs: Vec::new(),
            lps: Vec::new(),
        }
    }

    fn add_direct_key(chain: &mut Chain, start: u8, when: UtcTime) {
        let key = testkit::ed_key(start).verifying_key().to_bytes();
        let label = Label::parse("Swap key").unwrap();
        let direct = CapabilitySet::of(&[Capability::Direct]);
        let pending = chain
            .builder()
            .add_ed25519(key, label, direct, when)
            .unwrap();
        chain.push(&pending);
    }

    #[test]
    fn scopes_must_name_controllers_the_log_recorded() {
        let mut chain = Chain::linked();
        // 0x82 is added and withdrawn at once; 0x81 is still pending at `now`.
        add_direct_key(&mut chain, 0x82, at(DAY));
        let withdrawn = testkit::ed25519_id(0x82);
        let removal = chain
            .builder()
            .remove_controller(&withdrawn, false, at(DAY + 60))
            .unwrap();
        chain.push(&removal);
        let now = at(2 * DAY);
        add_direct_key(&mut chain, 0x81, at(2 * DAY - 60));
        let pending = testkit::ed25519_id(0x81);
        let state = &chain.state;
        assert!(state.controller(&withdrawn).is_none());
        assert!(state.is_recorded_controller(&withdrawn));
        assert!(!state.controller(&pending).unwrap().is_effective_at(now));
        assert!(!state.is_recorded_controller(&testkit::ed25519_id(0x99)));
        let submit = |scopes: Vec<CommandScope>| {
            let mut rules = DEFAULT_LINKED_RULES;
            rules.scopes = scopes;
            rules.scopes.sort_by_key(|s| s.controller);
            let doc = testkit::policy_doc(state, 1, now, rules);
            let bytes = doc.encode().unwrap();
            verify_policy_submission(state, &bytes, &testkit::passkey_envelope(&bytes), now)
        };
        let unknown = submit(vec![scope_for(
            testkit::ed25519_id(0x99),
            &[CommandKind::Swap],
        )]);
        let refused = unknown.unwrap_err();
        assert_eq!(refused, PolicyError::UnknownScopeController);
        assert_eq!(
            (refused.code_name(), refused.code()),
            ("unknown_scope_controller", 909)
        );
        let recorded = [
            pending,
            withdrawn,
            testkit::ed25519_id(0x21),
            testkit::passkey_id(),
        ];
        let scopes: Vec<CommandScope> = recorded
            .iter()
            .map(|id| scope_for(*id, &[CommandKind::Swap]))
            .collect();
        let verified = submit(scopes.clone()).unwrap();
        assert!(!verified.widening.any());
        assert_eq!(
            (verified.required_delay_secs, verified.effective_at),
            (0, now)
        );
        let mut mixed = scopes;
        mixed.push(scope_for(ControllerId([0x99; 32]), &[]));
        assert_eq!(
            submit(mixed).unwrap_err(),
            PolicyError::UnknownScopeController
        );
        // The builder refuses it too, and accepts a scope for a pending controller.
        let mut rules = DEFAULT_LINKED_RULES;
        rules.scopes = vec![scope_for(pending, &[CommandKind::Swap])];
        let doc = testkit::policy_doc(&chain.state, 1, now, rules);
        let effective = chain.accept_passkey(&doc, now).effective_at();
        assert_eq!(effective, now);
        assert_eq!(chain.replayed(), chain.state);
        assert!(!command_allowed(
            chain.state.rules_at(now).rules(),
            &pending,
            CommandKind::TransferCc
        ));
    }

    #[test]
    fn submissions_report_scope_and_package_widening_per_network() {
        for (network, delay) in [
            (Network::Devnet, DEVNET_DELAY),
            (Network::Mainnet, MAINNET_DELAY),
        ] {
            let mut chain = Chain::linked_on(network);
            let key = testkit::ed25519_id(0x21);
            let t1 = at(delay + 10);
            let mut narrow = DEFAULT_LINKED_RULES;
            narrow.scopes = vec![scope_for(key, &[CommandKind::Swap])];
            narrow.packages = Allowlist {
                any: false,
                items: vec![PackageId([5; 32])],
            };
            let v1 = PolicyDocument {
                network,
                ..testkit::policy_doc(&chain.state, 1, t1, narrow.clone())
            };
            assert_eq!(chain.accept_ed25519(0x21, &v1, t1).effective_at(), t1);
            let t2 = at(delay + 20);
            let check = |rules: Rules| {
                let doc = PolicyDocument {
                    network,
                    ..testkit::policy_doc(&chain.state, 2, t2, rules)
                };
                let bytes = doc.encode().unwrap();
                let envelope = testkit::ed25519_envelope(0x21, &bytes);
                verify_policy_submission(&chain.state, &bytes, &envelope, t2).unwrap()
            };
            let later = t2.checked_add_secs(delay).unwrap();
            let mut more_packages = narrow.clone();
            more_packages.packages.items.push(PackageId([6; 32]));
            let mut more_kinds = narrow.clone();
            more_kinds.scopes[0].commands.push(CommandKind::TransferCc);
            let mut fewer_kinds = narrow.clone();
            fewer_kinds.scopes[0].commands.clear();
            let cases = [
                (DEFAULT_LINKED_RULES, true, true),
                (more_packages, true, false),
                (more_kinds, false, true),
                (fewer_kinds, false, false),
            ];
            for (rules, packages, scopes) in cases {
                let verified = check(rules);
                assert_eq!(
                    (verified.widening.packages, verified.widening.scopes),
                    (packages, scopes)
                );
                let (secs, effective) = if packages || scopes {
                    (u64::try_from(delay).unwrap(), later)
                } else {
                    (0, t2)
                };
                assert_eq!(
                    (verified.required_delay_secs, verified.effective_at),
                    (secs, effective)
                );
            }
        }
    }
}
