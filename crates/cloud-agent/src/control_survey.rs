//! Observe-only survey of signed transactions per operation kind: hash checks, coverage,
//! facts and package ids. It never changes what is signed.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_logic::config::BaseConfig;
use agent_logic::control_store::{ChainNetwork, chain_network};
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use agent_logic::sync;
use orderbook_proto::orderbook::NodePackage;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use tx_verifier::facts::{PackageIds, TxFacts};
use tx_verifier::{Coverage, HashCheck, OperationExpectation, VerificationResult, WouldRefuse};

use crate::control::intent::{ContentRefusal, OwnRequest};

/// How often the counters are logged.
pub const SUMMARY_INTERVAL: Duration = Duration::from_secs(600);
/// Bytes of SHA-256 shown for a party in logs.
const SHORT_HASH_BYTES: usize = 6;
/// Package ids kept per kind; ids past it are only counted.
const MAX_PACKAGES_PER_KIND: usize = 64;
/// Package names kept from the node's list.
const MAX_NAMES: usize = 4096;
/// Characters of a package name and version shown.
const MAX_LABEL_CHARS: usize = 120;
/// Bound on one package-name lookup, connection included.
pub const NAMES_TIMEOUT: Duration = Duration::from_secs(20);
/// Least time between two package-name lookups.
pub const NAMES_RETRY: Duration = Duration::from_secs(3600);
/// Bound on the one package-name lookup of a refused signature.
pub const REFUSAL_NAMES_TIMEOUT: Duration = Duration::from_secs(5);
/// Least time between two package-name lookups of refusals, answered or not.
pub const REFUSAL_LOOKUP_BACKOFF: Duration = Duration::from_secs(60);
/// The inspector's answer for an operation whose fields it does not check yet.
const STUB_SUMMARY_PREFIX: &str = "[STUB]";
const STUB_WARNING: &str = "Transaction fields not yet verified for this operation";
/// The verifier's own hash comparison, which the hash counters already count.
const HASH_WARNING_PREFIX: &str = "Hash mismatch:";

/// The process-wide survey that the submit paths record into.
pub static SURVEY: Survey = Survey::new();

static SUMMARY_SPAWNED: AtomicBool = AtomicBool::new(false);
/// Set once the pinned platform parties failed to load, so that is logged once.
static PLATFORM_ERROR_LOGGED: AtomicBool = AtomicBool::new(false);
/// The template of a registry holding, the only kind created locked.
const LOCKED_HOLDING_TEMPLATE: &str = "Utility.Registry.Holding.V0.Holding:Holding";

/// Counters for one operation kind, since the process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub signed: u64,
    pub hash_match: u64,
    pub hash_mismatch: u64,
    pub hash_uncomputed: u64,
    pub facts_error: u64,
    pub facts_unmodelled: u64,
    pub outflows_to_others: u64,
    /// The content check of a linked agent would have refused it.
    pub content_would_refuse: u64,
    /// The content check would have refused it only for a platform pin that is not set.
    pub pins_missing: u64,
    /// It consumes contracts that are neither created in it nor among its inputs.
    pub facts_unlisted_inputs: u64,
    /// Holdings created for the party itself with a lock set.
    pub locked_own_holdings: u64,
    /// Signed with its fields checked by the inspector.
    pub coverage_fields: u64,
    /// Signed with its fields not checked.
    pub coverage_unverified: u64,
    /// The inspector answered with its "not yet verified" stub.
    pub stub_hits: u64,
    /// Inspector warnings, besides the stub's and the hash comparison's.
    pub inspector_warnings: u64,
    /// Signed although a field check would refuse it under enforce.
    pub inspector_would_refuse: u64,
    /// Signed with the server hash although enforce would refuse that hash.
    pub hash_would_refuse: u64,
    /// Its package ids could not be read.
    pub packages_error: u64,
}

impl Counters {
    fn add(&mut self, other: &Counters) {
        self.signed = self.signed.saturating_add(other.signed);
        self.hash_match = self.hash_match.saturating_add(other.hash_match);
        self.hash_mismatch = self.hash_mismatch.saturating_add(other.hash_mismatch);
        self.hash_uncomputed = self.hash_uncomputed.saturating_add(other.hash_uncomputed);
        self.facts_error = self.facts_error.saturating_add(other.facts_error);
        self.facts_unmodelled = self.facts_unmodelled.saturating_add(other.facts_unmodelled);
        self.outflows_to_others = self.outflows_to_others.saturating_add(other.outflows_to_others);
        self.content_would_refuse = self.content_would_refuse.saturating_add(other.content_would_refuse);
        self.pins_missing = self.pins_missing.saturating_add(other.pins_missing);
        self.facts_unlisted_inputs = self.facts_unlisted_inputs.saturating_add(other.facts_unlisted_inputs);
        self.locked_own_holdings = self.locked_own_holdings.saturating_add(other.locked_own_holdings);
        self.coverage_fields = self.coverage_fields.saturating_add(other.coverage_fields);
        self.coverage_unverified = self.coverage_unverified.saturating_add(other.coverage_unverified);
        self.stub_hits = self.stub_hits.saturating_add(other.stub_hits);
        self.inspector_warnings = self.inspector_warnings.saturating_add(other.inspector_warnings);
        self.inspector_would_refuse = self.inspector_would_refuse.saturating_add(other.inspector_would_refuse);
        self.hash_would_refuse = self.hash_would_refuse.saturating_add(other.hash_would_refuse);
        self.packages_error = self.packages_error.saturating_add(other.packages_error);
    }
}

/// One kind's counters and the enforced package ids its signed transactions used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct KindStats {
    counters: Counters,
    packages: BTreeSet<String>,
    /// New ids seen once `packages` was full.
    packages_unkept: u64,
}

/// Package names from the node's list, and when they were last asked for.
#[derive(Debug, Default)]
struct Names {
    labels: BTreeMap<String, String>,
    /// Ids a caller asked to name that no signed transaction used.
    wanted: BTreeSet<String>,
    last_attempt: Option<Instant>,
    failure_warned: bool,
    /// When a refusal last started a lookup, on tokio's clock so that tests can pause it.
    refusal_attempt: Option<tokio::time::Instant>,
}

/// A package the node vets, named for the summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedPackage {
    pub id: String,
    /// `name version`, with ` (removed)` once the node no longer vets it.
    pub label: String,
}

/// Where package names come from.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait PackageCatalog: Send + Sync {
    async fn packages(&self) -> Result<Vec<NamedPackage>, String>;
}

/// Per-kind counters and package names behind their locks.
#[derive(Debug, Default)]
pub struct Survey {
    kinds: Mutex<BTreeMap<&'static str, KindStats>>,
    names: Mutex<Names>,
}

impl Survey {
    pub const fn new() -> Self {
        Self {
            kinds: Mutex::new(BTreeMap::new()),
            names: Mutex::new(Names {
                labels: BTreeMap::new(),
                wanted: BTreeSet::new(),
                last_attempt: None,
                failure_warned: false,
                refusal_attempt: None,
            }),
        }
    }

    /// Lets the next refusal look its package up at once.
    #[cfg(test)]
    pub(crate) fn end_refusal_backoff(&self) {
        sync::lock(&self.names).refusal_attempt = None;
    }

    /// Counts one signed transaction of `kind` and logs its anomalies.
    pub fn record(
        &self,
        kind: &'static str,
        party: &str,
        hash_check: &HashCheck,
        facts: &Result<TxFacts, String>,
    ) {
        self.record_with(kind, party, hash_check, facts, None);
    }

    /// [`Self::record`] with the verdict of the content check a linked agent would run.
    pub fn record_with(
        &self,
        kind: &'static str,
        party: &str,
        hash_check: &HashCheck,
        facts: &Result<TxFacts, String>,
        content: Option<&Result<(), ContentRefusal>>,
    ) {
        let mut seen = Counters { signed: 1, ..Counters::default() };
        if let Some(Err(refusal)) = content {
            let pins = matches!(refusal, ContentRefusal::PinMissing { .. });
            if pins {
                seen.pins_missing = 1;
            } else {
                seen.content_would_refuse = 1;
            }
            // The first of a kind is a warning; the rest are counted in the summary.
            let first = sync::lock(&self.kinds).get(kind).is_none_or(|s| {
                if pins { s.counters.pins_missing == 0 } else { s.counters.content_would_refuse == 0 }
            });
            if first {
                warn!(target: "control_survey", "{kind}: content check would refuse: {}", refusal_text(refusal));
            } else {
                debug!(target: "control_survey", "{kind}: content check would refuse: {}", refusal_text(refusal));
            }
        }
        match hash_check {
            HashCheck::Match => seen.hash_match = 1,
            HashCheck::Mismatch { server_hex, computed_hex } => {
                seen.hash_mismatch = 1;
                warn!(
                    target: "control_survey",
                    "{kind}: signed the server hash after a mismatch (server={server_hex}, computed={computed_hex})"
                );
            }
            HashCheck::Uncomputed { reason } => {
                seen.hash_uncomputed = 1;
                warn!(target: "control_survey", "{kind}: signed the server hash; none was computed: {reason}");
            }
        }
        match facts {
            Err(reason) => {
                seen.facts_error = 1;
                warn!(target: "control_survey", "{kind}: transaction facts unreadable: {reason}");
            }
            Ok(facts) => {
                if !facts.unmodelled.is_empty() {
                    seen.facts_unmodelled = 1;
                    warn!(
                        target: "control_survey",
                        "{kind}: unmodelled templates: {}",
                        facts.unmodelled.join(", ")
                    );
                }
                if !facts.unlisted_inputs.is_empty() {
                    seen.facts_unlisted_inputs = 1;
                    debug!(
                        target: "control_survey",
                        "{kind}: {} consumed contracts are neither created nor listed as inputs",
                        facts.unlisted_inputs.len()
                    );
                }
                for held in facts.inflows.iter().filter(|held| held.locked) {
                    seen.locked_own_holdings = seen.locked_own_holdings.saturating_add(1);
                    debug!(
                        target: "control_survey",
                        "{kind}: own holding created locked ({LOCKED_HOLDING_TEMPLATE}): {} amount={}",
                        held.instrument_id,
                        held.amount
                    );
                }
                for flow in facts.outflows.iter().filter(|flow| flow.to != party) {
                    seen.outflows_to_others = seen.outflows_to_others.saturating_add(1);
                    debug!(
                        target: "control_survey",
                        "{kind}: outflow {:?} {}@{} amount={} to={}",
                        flow.kind,
                        flow.instrument_id,
                        short_hash(&flow.instrument_admin),
                        flow.amount,
                        short_hash(&flow.to)
                    );
                }
            }
        }
        self.add(kind, &seen);
    }

    /// Counts what the verifier said of one signed transaction of `kind`: its coverage, the
    /// stub, its inspector warnings and what enforce would have refused.
    pub fn record_verification(&self, kind: &'static str, verification: &VerificationResult) {
        let stub = verification.summary.starts_with(STUB_SUMMARY_PREFIX);
        let warnings = verification
            .warnings
            .iter()
            .filter(|w| !(stub && w.as_str() == STUB_WARNING) && !w.starts_with(HASH_WARNING_PREFIX))
            .count();
        let inspector = verification.would_refuse.iter().any(|w| matches!(w, WouldRefuse::Inspector(_)));
        let hash = verification.would_refuse.iter().any(|w| matches!(w, WouldRefuse::Hash(_)));
        let seen = Counters {
            coverage_fields: u64::from(matches!(verification.coverage, Coverage::Fields)),
            coverage_unverified: u64::from(matches!(verification.coverage, Coverage::Unverified)),
            stub_hits: u64::from(stub),
            inspector_warnings: u64::try_from(warnings).unwrap_or(u64::MAX),
            inspector_would_refuse: u64::from(inspector),
            hash_would_refuse: u64::from(hash),
            ..Counters::default()
        };
        self.add(kind, &seen);
    }

    /// Adds the enforced package ids of one signed transaction of `kind`.
    pub fn record_packages(&self, kind: &'static str, ids: Result<PackageIds, String>) {
        let failed = {
            let mut kinds = sync::lock(&self.kinds);
            let stats = kinds.entry(kind).or_default();
            match ids {
                Ok(ids) => {
                    for id in ids.enforced {
                        if stats.packages.contains(&id) {
                            continue;
                        }
                        if stats.packages.len() < MAX_PACKAGES_PER_KIND {
                            stats.packages.insert(id);
                        } else {
                            stats.packages_unkept = stats.packages_unkept.saturating_add(1);
                        }
                    }
                    None
                }
                Err(reason) => {
                    let first = stats.counters.packages_error == 0;
                    stats.counters.packages_error = stats.counters.packages_error.saturating_add(1);
                    Some((first, reason))
                }
            }
        };
        match failed {
            Some((true, reason)) => warn!(target: "control_survey", "{kind}: package ids unreadable: {reason}"),
            Some((false, reason)) => debug!(target: "control_survey", "{kind}: package ids unreadable: {reason}"),
            None => {}
        }
    }

    fn add(&self, kind: &'static str, seen: &Counters) {
        sync::lock(&self.kinds).entry(kind).or_default().counters.add(seen);
    }

    /// The counters of every kind seen so far, by kind name.
    pub fn snapshot(&self) -> Vec<(&'static str, Counters)> {
        sync::lock(&self.kinds).iter().map(|(kind, s)| (*kind, s.counters)).collect()
    }

    /// The enforced package ids `kind` used, sorted.
    pub fn package_ids(&self, kind: &str) -> Vec<String> {
        sync::lock(&self.kinds).get(kind).map(|s| s.packages.iter().cloned().collect()).unwrap_or_default()
    }

    /// The `name version` of `id` as last read from the node's list; an id without one is
    /// asked for at the next lookup.
    pub fn package_label(&self, id: &str) -> Option<String> {
        let mut names = sync::lock(&self.names);
        let label = names.labels.get(id).cloned();
        if label.is_none() && is_package_id(id) && names.wanted.len() < MAX_PACKAGES_PER_KIND {
            names.wanted.insert(id.to_string());
        }
        label
    }

    /// Adds the names of `packages` that the summary would keep, leaving its lookup schedule alone.
    fn merge_names(&self, packages: &[NamedPackage]) {
        let mut names = sync::lock(&self.names);
        for p in packages.iter().filter(|p| usable_name(p)) {
            if names.labels.len() < MAX_NAMES || names.labels.contains_key(&p.id) {
                names.labels.insert(p.id.clone(), p.label.clone());
            }
        }
        let Names { labels, wanted, .. } = &mut *names;
        wanted.retain(|id| !labels.contains_key(id));
    }

    /// The name of `id` for a refusal: the kept one, else a lookup bounded by [`REFUSAL_NAMES_TIMEOUT`]
    /// (names kept) unless one started within [`REFUSAL_LOOKUP_BACKOFF`]; `None` otherwise.
    pub async fn label_or_lookup(&self, id: &str, catalog: Option<&dyn PackageCatalog>) -> Option<String> {
        if let Some(label) = self.package_label(id) {
            return Some(label);
        }
        let catalog = catalog?;
        {
            let now = tokio::time::Instant::now();
            let mut names = sync::lock(&self.names);
            if names.refusal_attempt.is_some_and(|at| now.saturating_duration_since(at) < REFUSAL_LOOKUP_BACKOFF) {
                debug!(target: "control_survey", "package name of a refusal: a lookup started within {REFUSAL_LOOKUP_BACKOFF:?}");
                return None;
            }
            names.refusal_attempt = Some(now);
        }
        let fetched = match tokio::time::timeout(REFUSAL_NAMES_TIMEOUT, catalog.packages()).await {
            Ok(Ok(packages)) => packages,
            Ok(Err(e)) => {
                debug!(target: "control_survey", "package name of a refusal unavailable: {e}");
                return None;
            }
            Err(_) => {
                debug!(target: "control_survey", "package name of a refusal: no answer within {REFUSAL_NAMES_TIMEOUT:?}");
                return None;
            }
        };
        let label = fetched.iter().find(|p| p.id == id && usable_name(p)).map(|p| p.label.clone());
        self.merge_names(&fetched);
        label
    }

    /// The ids used or asked for so far that have no name.
    fn unnamed_ids(&self) -> BTreeSet<String> {
        let seen: BTreeSet<String> =
            sync::lock(&self.kinds).values().flat_map(|s| s.packages.iter().cloned()).collect();
        let names = sync::lock(&self.names);
        seen.into_iter()
            .chain(names.wanted.iter().cloned())
            .filter(|id| !names.labels.contains_key(id))
            .collect()
    }

    /// Asks `catalog` for names when an id has none and [`NAMES_RETRY`] passed since the last
    /// ask, bounded by [`NAMES_TIMEOUT`]; a failure keeps the ids. Returns whether it asked.
    pub async fn refresh_names(&self, catalog: &dyn PackageCatalog, now: Instant) -> bool {
        if self.unnamed_ids().is_empty() {
            return false;
        }
        {
            let mut names = sync::lock(&self.names);
            if names.last_attempt.is_some_and(|at| now.saturating_duration_since(at) < NAMES_RETRY) {
                return false;
            }
            names.last_attempt = Some(now);
        }
        let fetched = match tokio::time::timeout(NAMES_TIMEOUT, catalog.packages()).await {
            Ok(fetched) => fetched,
            Err(_) => Err(format!("no answer within {NAMES_TIMEOUT:?}")),
        };
        let warn_now = {
            let mut names = sync::lock(&self.names);
            match &fetched {
                Ok(packages) => {
                    names.labels = packages
                        .iter()
                        .filter(|p| usable_name(p))
                        .take(MAX_NAMES)
                        .map(|p| (p.id.clone(), p.label.clone()))
                        .collect();
                    names.wanted.clear();
                    names.failure_warned = false;
                    false
                }
                Err(_) => !std::mem::replace(&mut names.failure_warned, true),
            }
        };
        if let Err(e) = fetched {
            if warn_now {
                warn!(target: "control_survey", "package names unavailable, the survey shows ids: {e}");
            } else {
                debug!(target: "control_survey", "package names unavailable: {e}");
            }
        }
        true
    }

    /// One line with every kind's counters and packages; `None` before anything was signed.
    pub fn summary(&self) -> Option<String> {
        let kinds: Vec<(&'static str, KindStats)> =
            sync::lock(&self.kinds).iter().map(|(kind, s)| (*kind, s.clone())).collect();
        if kinds.is_empty() {
            return None;
        }
        let names = sync::lock(&self.names);
        let parts: Vec<String> = kinds
            .iter()
            .map(|(kind, s)| {
                let c = &s.counters;
                format!(
                    "{kind} signed={} hash_match={} hash_mismatch={} hash_uncomputed={} \
                     facts_error={} facts_unmodelled={} outflows_to_others={} \
                     content_would_refuse={} pins_missing={} unlisted_inputs={} locked_own_holdings={} \
                     coverage_fields={} coverage_unverified={} stub_hits={} inspector_warnings={} \
                     inspector_would_refuse={} hash_would_refuse={} \
                     packages_error={} packages=[{}]",
                    c.signed,
                    c.hash_match,
                    c.hash_mismatch,
                    c.hash_uncomputed,
                    c.facts_error,
                    c.facts_unmodelled,
                    c.outflows_to_others,
                    c.content_would_refuse,
                    c.pins_missing,
                    c.facts_unlisted_inputs,
                    c.locked_own_holdings,
                    c.coverage_fields,
                    c.coverage_unverified,
                    c.stub_hits,
                    c.inspector_warnings,
                    c.inspector_would_refuse,
                    c.hash_would_refuse,
                    c.packages_error,
                    package_list(s, &names.labels)
                )
            })
            .collect();
        Some(parts.join("; "))
    }

    fn log_summary(&self) {
        match self.summary() {
            Some(line) => info!(target: "control_survey", "control survey since start: {line}"),
            None => info!(target: "control_survey", "control survey since start: nothing signed"),
        }
    }
}

/// A kind's packages for the summary: names sorted, then the ids without one.
fn package_list(stats: &KindStats, labels: &BTreeMap<String, String>) -> String {
    let mut named: Vec<&str> = Vec::new();
    let mut unnamed: Vec<&str> = Vec::new();
    for id in &stats.packages {
        match labels.get(id) {
            Some(label) => named.push(label),
            None => unnamed.push(id),
        }
    }
    named.sort_unstable();
    let mut shown: Vec<String> = named.into_iter().chain(unnamed).map(str::to_string).collect();
    if stats.packages_unkept > 0 {
        shown.push(format!("+{} more", stats.packages_unkept));
    }
    shown.join(", ")
}

fn is_package_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A name the survey keeps: a package id with a label that is not blank.
fn usable_name(p: &NamedPackage) -> bool {
    is_package_id(&p.id) && !p.label.trim().is_empty()
}

/// The node's packages as the summary names them; a package without a name is left out.
pub fn named_packages(packages: Vec<NodePackage>) -> Vec<NamedPackage> {
    packages
        .into_iter()
        .filter(|p| !p.package_name.trim().is_empty())
        .map(|p| {
            let removed = if p.removed_at.is_some() { " (removed)" } else { "" };
            let label = format!("{} {}{removed}", p.package_name.trim(), p.package_version.trim());
            NamedPackage {
                label: label.chars().filter(|c| !crate::log_text::is_log_unsafe(*c)).take(MAX_LABEL_CHARS).collect(),
                id: p.package_id,
            }
        })
        .collect()
}

/// ListNodePackages on the config's rpc, as this agent; only on a control network.
pub(crate) struct RpcCatalog {
    config: BaseConfig,
}

impl RpcCatalog {
    pub(crate) fn for_config(config: &BaseConfig) -> Option<Self> {
        match chain_network(config.canton_chain.as_deref()) {
            ChainNetwork::Mapped(_) => Some(Self { config: config.clone() }),
            ChainNetwork::Unset | ChainNetwork::Unmapped(_) => None,
        }
    }
}

#[async_trait::async_trait]
impl PackageCatalog for RpcCatalog {
    async fn packages(&self) -> Result<Vec<NamedPackage>, String> {
        let rpc = crate::control::ControlRpc::connect(&self.config).await.map_err(|e| e.to_string())?;
        let reply = rpc.with_timeout(NAMES_TIMEOUT).node_packages(true).await.map_err(|e| e.to_string())?;
        Ok(named_packages(reply.packages))
    }
}

/// Records one signed transaction into [`SURVEY`].
pub fn record_signed(
    expectation: &OperationExpectation,
    party: &str,
    hash_check: &HashCheck,
    facts: &Result<TxFacts, String>,
) {
    SURVEY.record(operation_kind(expectation), party, hash_check, facts);
}

/// Records one signed transaction into [`SURVEY`], its package ids read off the signing path;
/// an agent that is not linked also logs the verdict of a linked agent's content check.
pub fn record_signed_observed(
    expectation: &OperationExpectation,
    party: &str,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
    linked: bool,
    prepared_transaction: &Arc<[u8]>,
) {
    let kind = operation_kind(expectation);
    let content = if linked { None } else { observe_content(expectation, party, request, verification, facts) };
    SURVEY.record_with(kind, party, &verification.hash_check, facts, content.as_ref());
    SURVEY.record_verification(kind, verification);
    survey_packages(kind, Arc::clone(prepared_transaction));
}

/// Reads the package ids of a signed transaction on the blocking pool, not waited for.
fn survey_packages(kind: &'static str, prepared_transaction: Arc<[u8]>) {
    let read = move || {
        let ids = tx_verifier::facts::package_ids(&prepared_transaction).map_err(|e| e.to_string());
        SURVEY.record_packages(kind, ids);
    };
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => drop(runtime.spawn_blocking(read)),
        Err(_) => read(),
    }
}

/// The `name version` of a package id as last read from the node's list; never blocks.
pub fn package_label(id: &str) -> Option<String> {
    SURVEY.package_label(id)
}

/// [`Survey::label_or_lookup`] on [`SURVEY`], for a refused signature only.
pub async fn refusal_label(id: &str, catalog: Option<&dyn PackageCatalog>) -> Option<String> {
    SURVEY.label_or_lookup(id, catalog).await
}

/// The content check over what the submit path already holds; without the pinned
/// platform parties it is skipped, and that is logged once.
fn observe_content(
    expectation: &OperationExpectation,
    party: &str,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
) -> Option<Result<(), ContentRefusal>> {
    match crate::control::platform_parties() {
        Ok(platform) => Some(crate::control::intent::check_before_signing(
            party,
            &platform,
            expectation,
            request,
            verification,
            facts,
        )),
        Err(e) => {
            if !PLATFORM_ERROR_LOGGED.swap(true, Ordering::SeqCst) {
                warn!(target: "control_survey", "content check not observed: the pinned platform parties do not load: {e:#}");
            }
            None
        }
    }
}

/// A refusal for the logs: instrument ids, amounts and short hashes of parties only.
fn refusal_text(refusal: &ContentRefusal) -> String {
    match refusal {
        ContentRefusal::HashNotMatched => "hash_not_matched".to_string(),
        ContentRefusal::Unreadable(_) => "unreadable".to_string(),
        ContentRefusal::ActAs(parties) => {
            format!("act_as [{}]", parties.iter().map(|p| short_hash(p)).collect::<Vec<_>>().join(", "))
        }
        ContentRefusal::Unmodelled(templates) => format!("unmodelled {}", templates.join(", ")),
        ContentRefusal::BadRequest(_) => "bad_request".to_string(),
        ContentRefusal::PinMissing { pin, .. } => format!("pins_missing {pin}"),
        ContentRefusal::UnexpectedOutflow { flow, .. } => format!(
            "unexpected_outflow {} {} ({:?}) to {}",
            flow.amount,
            flow.instrument_id,
            flow.kind,
            short_hash(&flow.to)
        ),
    }
}

/// Logs [`SURVEY`] every [`SUMMARY_INTERVAL`] until shutdown, naming packages through the
/// rpc of `config` on a control network; started once per process.
pub fn spawn_summary(shutdown: Shutdown, config: &BaseConfig) {
    if SUMMARY_SPAWNED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let catalog: Option<Arc<dyn PackageCatalog>> =
        RpcCatalog::for_config(config).map(|c| Arc::new(c) as Arc<dyn PackageCatalog>);
    let task_shutdown = shutdown.clone();
    let spawned = supervise::spawn_supervised("control survey summary", shutdown, Policy::Restart, move || {
        summary_loop(task_shutdown.clone(), catalog.clone())
    });
    if let Err(e) = spawned {
        SUMMARY_SPAWNED.store(false, Ordering::SeqCst);
        warn!(target: "control_survey", "control survey summary not started: {e:#}");
    }
}

async fn summary_loop(shutdown: Shutdown, catalog: Option<Arc<dyn PackageCatalog>>) {
    loop {
        let stopping = shutdown.sleep(SUMMARY_INTERVAL).await;
        if let (false, Some(catalog)) = (stopping, &catalog) {
            tokio::select! {
                _ = SURVEY.refresh_names(catalog.as_ref(), Instant::now()) => {}
                () = shutdown.wait() => {}
            }
        }
        SURVEY.log_summary();
        if stopping {
            SUMMARY_SPAWNED.store(false, Ordering::SeqCst);
            return;
        }
    }
}

/// The first bytes of SHA-256(`text`) in hex.
fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.get(..SHORT_HASH_BYTES).map(hex::encode).unwrap_or_default()
}

/// The survey's name for an operation.
pub fn operation_kind(expectation: &OperationExpectation) -> &'static str {
    match expectation {
        OperationExpectation::PayFee { .. } => "pay_fee",
        OperationExpectation::ProposeDvp { .. } => "propose_dvp",
        OperationExpectation::AcceptDvp { .. } => "accept_dvp",
        OperationExpectation::Allocate { .. } => "allocate",
        OperationExpectation::CancelDvpProposal { .. } => "cancel_dvp_proposal",
        OperationExpectation::RejectDvpProposal { .. } => "reject_dvp_proposal",
        OperationExpectation::TransferCc { .. } => "transfer_cc",
        OperationExpectation::PrepayTraffic { .. } => "prepay_traffic",
        OperationExpectation::RequestPreapproval { .. } => "request_preapproval",
        OperationExpectation::RequestRecurringPrepaid { .. } => "request_recurring_prepaid",
        OperationExpectation::RequestRecurringPayasyougo { .. } => "request_recurring_payasyougo",
        OperationExpectation::RequestUserService { .. } => "request_user_service",
        OperationExpectation::TransferCip56 { .. } => "transfer_cip56",
        OperationExpectation::AcceptCip56 { .. } => "accept_cip56",
        OperationExpectation::SplitCc { .. } => "split_cc",
        OperationExpectation::ExecuteMulticall { .. } => "execute_multicall",
        OperationExpectation::BatchPayCc { .. } => "batch_pay",
        OperationExpectation::LockHoldings { .. } => "lock_holdings",
        OperationExpectation::ProcessLockUnlockRequests { .. } => "process_lock_unlock_requests",
        OperationExpectation::ResizeLock { .. } => "resize_lock",
        OperationExpectation::TerminateLock { .. } => "terminate_lock",
        OperationExpectation::AtomicDvpSettle { .. } => "atomic_dvp_settle",
        OperationExpectation::IssueTickets { .. } => "issue_tickets",
        OperationExpectation::SplitHoldings { .. } => "split_holdings",
        OperationExpectation::CreateAtomicDvpVenue { .. } => "create_atomic_dvp_venue",
        OperationExpectation::UpdateVenueKey { .. } => "update_venue_key",
        OperationExpectation::RetireVenue { .. } => "retire_venue",
        OperationExpectation::CancelTickets { .. } => "cancel_tickets",
        OperationExpectation::UsdcxRequestAgreement { .. } => "usdcx_request_agreement",
        OperationExpectation::UsdcxMint { .. } => "usdcx_mint",
        OperationExpectation::UsdcxBurn { .. } => "usdcx_burn",
        OperationExpectation::UsdcxReclaimHolding { .. } => "usdcx_reclaim_holding",
        OperationExpectation::UsdcxUpdatePreApproval { .. } => "usdcx_update_pre_approval",
        OperationExpectation::UsdcxTerminateAgreement { .. } => "usdcx_terminate_agreement",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tx_verifier::facts::{Decimal, FlowKind, ValueFlow};

    const ME: &str = "agent::1220aa";
    const OTHER: &str = "bob::1220bb";

    fn flow(to: &str, kind: FlowKind) -> ValueFlow {
        ValueFlow {
            to: to.into(),
            instrument_admin: "dso::1220cc".into(),
            instrument_id: "Amulet".into(),
            amount: Decimal::new(25, 1),
            kind,
        }
    }

    fn mismatch() -> HashCheck {
        HashCheck::Mismatch { server_hex: "aa".into(), computed_hex: "bb".into() }
    }

    /// A verification result; every literal of the type in these tests is this one.
    fn verification(coverage: Coverage, hash_check: HashCheck, summary: &str, warnings: &[&str]) -> VerificationResult {
        VerificationResult {
            accepted: true,
            computed_hash: [7u8; 32],
            summary: summary.into(),
            warnings: warnings.iter().map(|w| w.to_string()).collect(),
            rejection_reason: None,
            burn_holding_cids: Vec::new(),
            hash_check,
            coverage,
            would_refuse: Vec::new(),
        }
    }

    fn pkg(n: u8) -> String {
        hex::encode([n; 32])
    }

    fn ids(enforced: &[String]) -> Result<PackageIds, String> {
        Ok(PackageIds { enforced: enforced.to_vec(), inputs: vec![pkg(99)] })
    }

    fn counters(survey: &Survey, kind: &str) -> Counters {
        survey.snapshot().into_iter().find(|(k, _)| *k == kind).map(|(_, c)| c).unwrap_or_default()
    }

    #[test]
    fn each_signed_transaction_increments_its_kind() {
        let survey = Survey::new();
        assert!(survey.snapshot().is_empty());
        assert_eq!(survey.summary(), None);

        let clean = TxFacts { outflows: vec![flow(OTHER, FlowKind::Holding), flow(ME, FlowKind::Lock)], ..TxFacts::default() };
        survey.record("transfer_cc", ME, &HashCheck::Match, &Ok(clean));
        survey.record("transfer_cc", ME, &mismatch(), &Err("undecodable".into()));
        let odd = TxFacts { unmodelled: vec!["Foo.Bar:Baz".into()], ..TxFacts::default() };
        survey.record(
            "split_cc",
            ME,
            &HashCheck::Uncomputed { reason: "no transaction".into() },
            &Ok(odd),
        );

        let snapshot = survey.snapshot();
        assert_eq!(
            snapshot,
            vec![
                (
                    "split_cc",
                    Counters { signed: 1, hash_uncomputed: 1, facts_unmodelled: 1, ..Counters::default() }
                ),
                (
                    "transfer_cc",
                    Counters {
                        signed: 2,
                        hash_match: 1,
                        hash_mismatch: 1,
                        facts_error: 1,
                        outflows_to_others: 1,
                        ..Counters::default()
                    }
                ),
            ]
        );
        let line = survey.summary().unwrap();
        assert!(line.starts_with("split_cc signed=1 "), "{line}");
        assert!(line.contains("transfer_cc signed=2 hash_match=1 hash_mismatch=1"), "{line}");
    }

    #[test]
    fn counters_saturate() {
        let survey = Survey::new();
        let full = KindStats { counters: Counters { signed: u64::MAX, ..Counters::default() }, ..KindStats::default() };
        sync::lock(&survey.kinds).insert("pay_fee", full);
        survey.record("pay_fee", ME, &HashCheck::Match, &Ok(TxFacts::default()));
        let snapshot = survey.snapshot();
        assert_eq!(snapshot.first().map(|(_, c)| (c.signed, c.hash_match)), Some((u64::MAX, 1)));
    }

    #[test]
    fn the_global_survey_counts_by_operation_kind() {
        let expectation = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let before = SURVEY.snapshot().into_iter().find(|(k, _)| *k == "issue_tickets").map(|(_, c)| c.signed);
        record_signed(&expectation, ME, &HashCheck::Match, &Ok(TxFacts::default()));
        let after = SURVEY.snapshot().into_iter().find(|(k, _)| *k == "issue_tickets").map(|(_, c)| c.signed);
        assert!(after.unwrap_or(0) > before.unwrap_or(0));
    }

    fn locked(cid: &str) -> tx_verifier::facts::HoldingFact {
        tx_verifier::facts::HoldingFact {
            contract_id: cid.into(),
            instrument_admin: "reg::1220ab".into(),
            instrument_id: "TOK".into(),
            amount: Decimal::new(5, 0),
            locked: true,
        }
    }

    fn outflow_refusal() -> ContentRefusal {
        ContentRefusal::UnexpectedOutflow { operation: "transfer_cc", flow: flow(OTHER, FlowKind::Holding) }
    }

    #[test]
    fn the_content_verdict_unlisted_inputs_and_locked_own_holdings_are_counted() {
        let survey = Survey::new();
        let unlocked = tx_verifier::facts::HoldingFact { locked: false, ..locked("h2") };
        let facts = TxFacts {
            unlisted_inputs: vec!["c9".into()],
            inflows: vec![locked("h1"), unlocked, locked("h3")],
            ..TxFacts::default()
        };
        survey.record_with("transfer_cc", ME, &HashCheck::Match, &Ok(facts), Some(&Err(outflow_refusal())));
        survey.record_with("transfer_cc", ME, &HashCheck::Match, &Ok(TxFacts::default()), Some(&Ok(())));
        survey.record_with("transfer_cc", ME, &HashCheck::Match, &Ok(TxFacts::default()), None);
        survey.record("split_cc", ME, &HashCheck::Match, &Ok(TxFacts::default()));
        let snapshot = survey.snapshot();
        let c = snapshot.iter().find(|(kind, _)| *kind == "transfer_cc").map(|(_, c)| *c).unwrap();
        assert_eq!((c.signed, c.content_would_refuse, c.facts_unlisted_inputs, c.locked_own_holdings), (3, 1, 1, 2));
        let line = survey.summary().unwrap();
        assert!(line.contains("transfer_cc signed=3 "), "{line}");
        assert!(line.contains("content_would_refuse=1 pins_missing=0 unlisted_inputs=1 locked_own_holdings=2"), "{line}");
        assert!(line.contains("split_cc signed=1 ") && line.contains("content_would_refuse=0"), "{line}");
    }

    #[test]
    fn a_missing_pin_is_counted_apart_from_content_refusals() {
        let survey = Survey::new();
        let pin = ContentRefusal::PinMissing { operation: "prepay_traffic", pin: "PARTY_PREPAID_TRAFFIC" };
        survey.record_with("prepay_traffic", ME, &HashCheck::Match, &Ok(TxFacts::default()), Some(&Err(pin.clone())));
        survey.record_with("prepay_traffic", ME, &HashCheck::Match, &Ok(TxFacts::default()), Some(&Err(pin.clone())));
        survey.record_with("prepay_traffic", ME, &HashCheck::Match, &Ok(TxFacts::default()), Some(&Err(outflow_refusal())));
        let c = counters(&survey, "prepay_traffic");
        assert_eq!((c.signed, c.pins_missing, c.content_would_refuse), (3, 2, 1));
        let line = survey.summary().unwrap();
        assert!(line.contains("prepay_traffic signed=3 "), "{line}");
        assert!(line.contains("content_would_refuse=1 pins_missing=2 "), "{line}");
        assert_eq!(refusal_text(&pin), "pins_missing PARTY_PREPAID_TRAFFIC");
    }

    #[test]
    fn refusals_are_logged_without_party_ids() {
        let text = refusal_text(&outflow_refusal());
        assert_eq!(text, format!("unexpected_outflow 2.5 Amulet (Holding) to {}", short_hash(OTHER)));
        assert!(!text.contains("bob") && !text.contains("::"), "{text}");
        let act_as = refusal_text(&ContentRefusal::ActAs(vec![ME.into(), OTHER.into()]));
        assert!(!act_as.contains("::") && act_as.contains(&short_hash(ME)), "{act_as}");
        assert_eq!(refusal_text(&ContentRefusal::Unreadable("party bob::1220bb".into())), "unreadable");
        assert_eq!(refusal_text(&ContentRefusal::BadRequest("to bob::1220bb".into())), "bad_request");
        assert_eq!(refusal_text(&ContentRefusal::HashNotMatched), "hash_not_matched");
        assert_eq!(refusal_text(&ContentRefusal::Unmodelled(vec!["A.B:C".into()])), "unmodelled A.B:C");
    }

    #[test]
    fn the_observed_check_is_the_linked_agents_check() {
        let verification = verification(Coverage::Fields, HashCheck::Match, "", &[]);
        let e = OperationExpectation::TransferCc {
            sender_party: ME.into(),
            receiver_party: OTHER.into(),
            amount: "2.5".into(),
            command_id: "c".into(),
            dso_party: None,
        };
        let facts = |to: &str| Ok(TxFacts { act_as: vec![ME.into()], outflows: vec![flow(to, FlowKind::Holding)], ..TxFacts::default() });
        let own = observe_content(&e, ME, OwnRequest::Ledger(None), &verification, &facts(OTHER));
        assert_eq!(own, Some(Ok(())));
        let elsewhere = observe_content(&e, ME, OwnRequest::Ledger(None), &verification, &facts("eve::1220ee"));
        assert!(matches!(elsewhere, Some(Err(ContentRefusal::UnexpectedOutflow { .. }))), "{elsewhere:?}");
        // The global survey counts it for the kind; other tests may record the same kind meanwhile.
        let count = || {
            SURVEY.snapshot().into_iter().find(|(k, _)| *k == "transfer_cc").map_or(0, |(_, c)| c.content_would_refuse)
        };
        let before = count();
        let bytes: Arc<[u8]> = Vec::new().into();
        record_signed_observed(&e, ME, OwnRequest::Ledger(None), &verification, &facts("eve::1220ee"), false, &bytes);
        assert!(count() > before);
    }

    #[test]
    fn recipients_are_logged_as_short_hashes() {
        let h = short_hash(OTHER);
        assert_eq!(h.len(), SHORT_HASH_BYTES * 2);
        assert_ne!(h, short_hash(ME));
        assert!(!h.contains("bob"));
    }

    #[test]
    fn coverage_stub_hits_warnings_and_observed_refusals_are_counted() {
        let survey = Survey::new();
        // A checked operation that matched: no warnings, nothing refused
        survey.record_verification("transfer_cc", &verification(Coverage::Fields, HashCheck::Match, "ok", &[]));
        // An observed hash fallback and an observed field check, with one inspector warning
        let mut observed = verification(
            Coverage::Fields,
            mismatch(),
            "ok",
            &["Hash mismatch: server=aa, computed=bb", "TransferPreapproval_Send not found"],
        );
        observed.would_refuse = vec![
            WouldRefuse::Hash("mismatch".into()),
            WouldRefuse::Inspector("receiver differs".into()),
            WouldRefuse::Inspector("amount differs".into()),
        ];
        survey.record_verification("transfer_cc", &observed);
        // The stub: its own warning is a stub hit, not an inspector warning
        let stub = verification(
            Coverage::Unverified,
            mismatch(),
            "[STUB] SplitCc — 10 bytes, not yet verified\nHash MISMATCH (debug mode — using server hash).",
            &["Transaction fields not yet verified for this operation", "Hash mismatch: server=aa, computed=bb"],
        );
        survey.record_verification("split_cc", &stub);

        let c = counters(&survey, "transfer_cc");
        assert_eq!(
            (c.coverage_fields, c.coverage_unverified, c.stub_hits, c.inspector_warnings),
            (2, 0, 0, 1)
        );
        assert_eq!((c.inspector_would_refuse, c.hash_would_refuse), (1, 1));
        let s = counters(&survey, "split_cc");
        assert_eq!((s.coverage_fields, s.coverage_unverified, s.stub_hits, s.inspector_warnings), (0, 1, 1, 0));
        assert_eq!((s.inspector_would_refuse, s.hash_would_refuse), (0, 0), "nothing observed was listed");
        assert_eq!(s.signed, 0, "the verification alone signs nothing");
    }

    #[test]
    fn package_ids_are_kept_per_kind_bounded_and_errors_counted() {
        let survey = Survey::new();
        survey.record_packages("transfer_cc", ids(&[pkg(2), pkg(1)]));
        survey.record_packages("transfer_cc", ids(&[pkg(1), pkg(3)]));
        survey.record_packages("split_cc", ids(&[pkg(4)]));
        assert_eq!(survey.package_ids("transfer_cc"), vec![pkg(1), pkg(2), pkg(3)]);
        assert_eq!(survey.package_ids("split_cc"), vec![pkg(4)], "input packages are not kept");
        assert!(survey.package_ids("allocate").is_empty());

        survey.record_packages("transfer_cc", Err("not a prepared transaction".into()));
        assert_eq!(counters(&survey, "transfer_cc").packages_error, 1);

        let many: Vec<String> = (0..=u8::try_from(MAX_PACKAGES_PER_KIND + 4).unwrap()).map(pkg).collect();
        survey.record_packages("execute_multicall", ids(&many));
        assert_eq!(survey.package_ids("execute_multicall").len(), MAX_PACKAGES_PER_KIND);
        let line = survey.summary().unwrap();
        assert!(line.contains("+5 more]"), "{line}");
    }

    struct Catalog {
        answer: Mutex<Result<Vec<NamedPackage>, String>>,
        calls: AtomicUsize,
    }

    impl Catalog {
        fn new(answer: Result<Vec<NamedPackage>, String>) -> Self {
            Self { answer: Mutex::new(answer), calls: AtomicUsize::new(0) }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PackageCatalog for Catalog {
        async fn packages(&self) -> Result<Vec<NamedPackage>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.lock().unwrap().clone()
        }
    }

    fn named(n: u8, label: &str) -> NamedPackage {
        NamedPackage { id: pkg(n), label: label.into() }
    }

    #[tokio::test]
    async fn the_summary_names_the_packages_each_kind_used() {
        let survey = Survey::new();
        survey.record("transfer_cc", ME, &HashCheck::Match, &Ok(TxFacts::default()));
        survey.record_packages("transfer_cc", ids(&[pkg(1), pkg(2), pkg(3)]));
        let catalog = Catalog::new(Ok(vec![
            named(2, "splice-api-token-holding-v1 1.0.0"),
            named(1, "splice-amulet 0.1.17"),
            named(9, "utility-registry-v0 0.5.0"),
        ]));
        assert!(survey.refresh_names(&catalog, Instant::now()).await);
        let line = survey.summary().unwrap();
        assert!(
            line.ends_with(&format!(
                "packages_error=0 packages=[splice-amulet 0.1.17, splice-api-token-holding-v1 1.0.0, {}]",
                pkg(3)
            )),
            "{line}"
        );
        assert!(line.starts_with("transfer_cc signed=1 hash_match=1 "), "{line}");
        assert!(line.contains("coverage_fields=0 coverage_unverified=0 stub_hits=0 inspector_warnings=0 "), "{line}");
        let exact = format!(
            "transfer_cc signed=1 hash_match=1 hash_mismatch=0 hash_uncomputed=0 facts_error=0 \
             facts_unmodelled=0 outflows_to_others=0 content_would_refuse=0 pins_missing=0 unlisted_inputs=0 \
             locked_own_holdings=0 coverage_fields=0 coverage_unverified=0 stub_hits=0 inspector_warnings=0 \
             inspector_would_refuse=0 hash_would_refuse=0 packages_error=0 \
             packages=[splice-amulet 0.1.17, splice-api-token-holding-v1 1.0.0, {}]",
            pkg(3)
        );
        assert_eq!(line, exact);
        assert_eq!(survey.package_label(&pkg(9)).as_deref(), Some("utility-registry-v0 0.5.0"));
        assert_eq!(survey.package_label(&pkg(3)), None);
    }

    /// A catalog that never answers.
    struct Stalled;

    #[async_trait::async_trait]
    impl PackageCatalog for Stalled {
        async fn packages(&self) -> Result<Vec<NamedPackage>, String> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_names_its_package_with_one_lookup_kept_for_the_next() {
        let survey = Survey::new();
        let catalog = Catalog::new(Ok(vec![
            named(5, "splice-wallet 0.1.17"),
            named(6, "  "),
            NamedPackage { id: "not a package id".into(), label: "x 1".into() },
        ]));
        assert_eq!(survey.label_or_lookup(&pkg(5), None).await, None, "no catalog, no lookup");
        assert_eq!(survey.label_or_lookup(&pkg(5), Some(&catalog)).await.as_deref(), Some("splice-wallet 0.1.17"));
        assert_eq!(catalog.calls(), 1);
        // Kept: the next refusal and the summary need no lookup
        assert_eq!(survey.label_or_lookup(&pkg(5), Some(&catalog)).await.as_deref(), Some("splice-wallet 0.1.17"));
        assert_eq!(survey.package_label(&pkg(5)).as_deref(), Some("splice-wallet 0.1.17"));
        assert_eq!(catalog.calls(), 1);
        // The same cleaning as the summary's names; its lookup schedule is untouched
        tokio::time::advance(REFUSAL_LOOKUP_BACKOFF).await;
        assert_eq!(survey.label_or_lookup(&pkg(6), Some(&catalog)).await, None);
        assert_eq!(catalog.calls(), 2);
        let names = sync::lock(&survey.names);
        assert_eq!(names.labels.len(), 1);
        assert!(names.last_attempt.is_none() && !names.failure_warned);
        assert!(!names.wanted.contains(&pkg(5)) && names.wanted.contains(&pkg(6)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_or_stalled_refusal_lookup_leaves_the_id_alone() {
        let survey = Survey::new();
        let failing = Catalog::new(Err("the rpc has no control service".into()));
        assert_eq!(survey.label_or_lookup(&pkg(7), Some(&failing)).await, None);
        assert_eq!(failing.calls(), 1);
        tokio::time::advance(REFUSAL_LOOKUP_BACKOFF).await;
        let started = tokio::time::Instant::now();
        assert_eq!(survey.label_or_lookup(&pkg(7), Some(&Stalled)).await, None);
        let waited = started.elapsed();
        assert!(waited >= REFUSAL_NAMES_TIMEOUT && waited < NAMES_TIMEOUT, "{waited:?}");
        let names = sync::lock(&survey.names);
        assert!(names.labels.is_empty() && names.last_attempt.is_none() && !names.failure_warned);
    }

    /// A catalog that answers a second after it is asked.
    struct Slow(Catalog);

    #[async_trait::async_trait]
    impl PackageCatalog for Slow {
        async fn packages(&self) -> Result<Vec<NamedPackage>, String> {
            let answer = self.0.packages().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            answer
        }
    }

    #[tokio::test(start_paused = true)]
    async fn refusal_lookups_wait_out_the_backoff_whatever_their_answer() {
        let survey = Survey::new();
        let catalog = Catalog::new(Ok(vec![named(5, "splice-wallet 0.1.17")]));
        // An id the node does not list: one call, then the id alone until the backoff passed
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&catalog)).await, None);
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&catalog)).await, None);
        assert_eq!(survey.label_or_lookup(&pkg(9), Some(&catalog)).await, None);
        assert_eq!(catalog.calls(), 1);
        // A kept label still needs no call
        assert_eq!(survey.label_or_lookup(&pkg(5), Some(&catalog)).await.as_deref(), Some("splice-wallet 0.1.17"));
        assert_eq!(catalog.calls(), 1);
        tokio::time::advance(REFUSAL_LOOKUP_BACKOFF.saturating_sub(Duration::from_secs(1))).await;
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&catalog)).await, None);
        assert_eq!(catalog.calls(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&catalog)).await, None);
        assert_eq!(catalog.calls(), 2);
        // A failed lookup waits it out too, and the summary's schedule is untouched
        let failing = Catalog::new(Err("the rpc has no control service".into()));
        tokio::time::advance(REFUSAL_LOOKUP_BACKOFF).await;
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&failing)).await, None);
        assert_eq!(survey.label_or_lookup(&pkg(8), Some(&failing)).await, None);
        assert_eq!(failing.calls(), 1);
        let names = sync::lock(&survey.names);
        assert!(names.last_attempt.is_none() && !names.failure_warned);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_refusals_make_one_lookup() {
        let survey = Survey::new();
        let slow = Slow(Catalog::new(Ok(vec![named(5, "splice-wallet 0.1.17")])));
        let id = pkg(5);
        let started = tokio::time::Instant::now();
        let (first, second) = tokio::join!(
            survey.label_or_lookup(&id, Some(&slow)),
            survey.label_or_lookup(&id, Some(&slow)),
        );
        assert_eq!(slow.0.calls(), 1);
        // One is named by the lookup, the other logs the id alone
        let mut answers = [first, second];
        answers.sort();
        assert_eq!(answers, [None, Some("splice-wallet 0.1.17".to_string())]);
        assert!(started.elapsed() < REFUSAL_NAMES_TIMEOUT);
        // The answer is kept for the next refusal
        assert_eq!(survey.label_or_lookup(&pkg(5), Some(&slow)).await.as_deref(), Some("splice-wallet 0.1.17"));
        assert_eq!(slow.0.calls(), 1);
    }

    #[test]
    fn merged_names_stop_at_the_summarys_cap() {
        let survey = Survey::new();
        let many: Vec<NamedPackage> = (0..MAX_NAMES.saturating_add(3))
            .map(|i| NamedPackage { id: format!("{i:064x}"), label: format!("p {i}") })
            .collect();
        survey.merge_names(&many);
        assert_eq!(sync::lock(&survey.names).labels.len(), MAX_NAMES);
        // A kept id is still renamed when the cap is reached
        survey.merge_names(&[NamedPackage { id: format!("{:064x}", 0), label: "p 0 (removed)".into() }]);
        assert_eq!(survey.package_label(&format!("{:064x}", 0)).as_deref(), Some("p 0 (removed)"));
        assert_eq!(sync::lock(&survey.names).labels.len(), MAX_NAMES);
    }

    #[tokio::test]
    async fn names_are_asked_for_only_when_needed_and_a_failure_keeps_the_ids() {
        let survey = Survey::new();
        let catalog = Catalog::new(Err("the rpc has no control service".into()));
        let start = Instant::now();
        assert!(!survey.refresh_names(&catalog, start).await, "nothing used yet");
        survey.record_packages("split_cc", ids(&[pkg(1)]));
        assert!(survey.refresh_names(&catalog, start).await);
        assert_eq!(catalog.calls(), 1);
        assert!(sync::lock(&survey.names).failure_warned);
        let line = survey.summary().unwrap();
        assert!(line.contains(&format!("packages=[{}]", pkg(1))), "{line}");
        // Not again before the retry time
        let soon = start.checked_add(Duration::from_secs(60)).unwrap();
        assert!(!survey.refresh_names(&catalog, soon).await);
        assert_eq!(catalog.calls(), 1);
        // After it, once the rpc answers, the names replace the ids
        *catalog.answer.lock().unwrap() = Ok(vec![named(1, "splice-amulet 0.1.17")]);
        let later = start.checked_add(NAMES_RETRY).unwrap();
        assert!(survey.refresh_names(&catalog, later).await);
        assert_eq!(catalog.calls(), 2);
        assert!(!sync::lock(&survey.names).failure_warned);
        assert!(survey.summary().unwrap().contains("packages=[splice-amulet 0.1.17]"));
        // Every id named: no further call
        let much_later = later.checked_add(NAMES_RETRY).unwrap();
        assert!(!survey.refresh_names(&catalog, much_later).await);
        assert_eq!(catalog.calls(), 2);
    }

    #[tokio::test]
    async fn an_id_asked_for_by_label_is_looked_up_at_the_next_refresh() {
        let survey = Survey::new();
        let catalog = Catalog::new(Ok(vec![named(5, "splice-wallet 0.1.17")]));
        assert!(!survey.refresh_names(&catalog, Instant::now()).await, "nothing used or asked for");
        // A refused package was never signed, so only the ask brings it to the lookup
        assert_eq!(survey.package_label(&pkg(5)), None);
        assert_eq!(survey.package_label("not a package id"), None);
        assert_eq!(sync::lock(&survey.names).wanted.len(), 1);
        assert!(survey.refresh_names(&catalog, Instant::now()).await);
        assert_eq!(survey.package_label(&pkg(5)).as_deref(), Some("splice-wallet 0.1.17"));
        assert!(sync::lock(&survey.names).wanted.is_empty());
        assert_eq!(catalog.calls(), 1);
    }

    #[test]
    fn node_packages_are_named_with_bounded_clean_labels() {
        let package = |id: &str, name: &str, version: &str, removed: bool| NodePackage {
            package_id: id.into(),
            package_name: name.into(),
            package_version: version.into(),
            removed_at: removed.then(prost_types::Timestamp::default),
            ..NodePackage::default()
        };
        let named = named_packages(vec![
            package(&pkg(1), "splice-amulet", "0.1.17", false),
            package(&pkg(2), "old\npkg", "0.1.9", true),
            package(&pkg(3), "  ", "1.0.0", false),
            package(&pkg(4), &"x".repeat(500), "1", false),
        ]);
        assert_eq!(named.len(), 3, "a package without a name is left out");
        assert_eq!(named[0], NamedPackage { id: pkg(1), label: "splice-amulet 0.1.17".into() });
        assert_eq!(named[1].label, "oldpkg 0.1.9 (removed)");
        assert_eq!(named[2].label.chars().count(), MAX_LABEL_CHARS);
        assert!(is_package_id(&pkg(0xab)) && !is_package_id("ab") && !is_package_id(&pkg(0xab).to_uppercase()));
    }

    #[test]
    fn package_labels_drop_separators_and_bidi_marks() {
        let named = named_packages(vec![NodePackage {
            package_id: pkg(5),
            package_name: "spl\u{2028}ice\u{2029}-amulet\u{202E}".into(),
            package_version: "0.\u{2066}1.\u{200F}17".into(),
            ..NodePackage::default()
        }]);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].label, "splice-amulet 0.1.17");
    }

    /// A prepared transaction with one Create node of package `n`.
    fn one_create(n: u8) -> Vec<u8> {
        use prost::Message;
        use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
        use v2::interactive::transaction::v1::{Create, Node as V1Node, node::NodeType};
        use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction, daml_transaction};
        let create = Create {
            lf_version: "2.1".into(),
            contract_id: "c1".into(),
            package_name: "pkg".into(),
            template_id: Some(v2::Identifier {
                package_id: pkg(n),
                module_name: "Splice.Amulet".into(),
                entity_name: "Amulet".into(),
            }),
            ..Default::default()
        };
        PreparedTransaction {
            transaction: Some(DamlTransaction {
                version: "2.1".into(),
                roots: vec!["0".into()],
                nodes: vec![daml_transaction::Node {
                    node_id: "0".into(),
                    versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node {
                        node_type: Some(NodeType::Create(create)),
                    })),
                }],
                node_seeds: vec![],
            }),
            metadata: Some(Metadata::default()),
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn a_signed_transactions_package_ids_are_read_off_the_signing_path() {
        let e = OperationExpectation::UpdateVenueKey { lp_party: ME.into(), venue_cid: "v".into(), new_key_spki_hex: "00".into() };
        let checked = verification(Coverage::Unverified, HashCheck::Match, "[STUB] UpdateVenueKey", &[STUB_WARNING]);
        let errors = || counters(&SURVEY, "update_venue_key").packages_error;
        let errors_before = errors();
        for bytes in [one_create(42), b"not a transaction".to_vec()] {
            let bytes: Arc<[u8]> = bytes.into();
            record_signed_observed(&e, ME, OwnRequest::Atomic(None), &checked, &Ok(TxFacts::default()), true, &bytes);
        }
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while (SURVEY.package_ids("update_venue_key").is_empty() || errors() == errors_before) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(SURVEY.package_ids("update_venue_key").contains(&pkg(42)));
        assert!(errors() > errors_before, "an unreadable transaction counts as a package error");
        let c = counters(&SURVEY, "update_venue_key");
        assert!(c.signed >= 2 && c.stub_hits >= 2 && c.coverage_unverified >= 2, "{c:?}");
    }

    #[test]
    fn only_a_control_network_asks_the_rpc_for_names() {
        let mut config = BaseConfig::test_minimal().unwrap();
        for (chain, asks) in [(None, false), (Some("testnet"), false), (Some("devnet"), true), (Some("mainnet"), true)] {
            config.canton_chain = chain.map(str::to_string);
            assert_eq!(RpcCatalog::for_config(&config).is_some(), asks, "{chain:?}");
        }
    }
}
