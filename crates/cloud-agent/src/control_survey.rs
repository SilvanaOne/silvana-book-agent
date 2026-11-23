//! Observe-only survey of signed transactions: hash checks and whole-transaction
//! facts, counted per operation kind. It never changes what is signed.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use agent_logic::sync;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};
use tx_verifier::facts::TxFacts;
use tx_verifier::{HashCheck, OperationExpectation, VerificationResult};

use crate::control::intent::{ContentRefusal, OwnRequest};

/// How often the counters are logged.
pub const SUMMARY_INTERVAL: Duration = Duration::from_secs(600);
/// Bytes of SHA-256 shown for a party in logs.
const SHORT_HASH_BYTES: usize = 6;

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
    /// It consumes contracts that are neither created in it nor among its inputs.
    pub facts_unlisted_inputs: u64,
    /// Holdings created for the party itself with a lock set.
    pub locked_own_holdings: u64,
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
        self.facts_unlisted_inputs = self.facts_unlisted_inputs.saturating_add(other.facts_unlisted_inputs);
        self.locked_own_holdings = self.locked_own_holdings.saturating_add(other.locked_own_holdings);
    }
}

/// Per-kind counters behind one lock.
#[derive(Debug, Default)]
pub struct Survey {
    kinds: Mutex<BTreeMap<&'static str, Counters>>,
}

impl Survey {
    pub const fn new() -> Self {
        Self { kinds: Mutex::new(BTreeMap::new()) }
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
            seen.content_would_refuse = 1;
            // The first of a kind is a warning; the rest are counted in the summary.
            let first = sync::lock(&self.kinds).get(kind).is_none_or(|c| c.content_would_refuse == 0);
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
        sync::lock(&self.kinds).entry(kind).or_default().add(&seen);
    }

    /// The counters of every kind seen so far, by kind name.
    pub fn snapshot(&self) -> Vec<(&'static str, Counters)> {
        sync::lock(&self.kinds).iter().map(|(kind, c)| (*kind, *c)).collect()
    }

    /// One line with every kind's counters; `None` before anything was signed.
    pub fn summary(&self) -> Option<String> {
        let kinds = self.snapshot();
        if kinds.is_empty() {
            return None;
        }
        let parts: Vec<String> = kinds
            .iter()
            .map(|(kind, c)| {
                format!(
                    "{kind} signed={} hash_match={} hash_mismatch={} hash_uncomputed={} \
                     facts_error={} facts_unmodelled={} outflows_to_others={} \
                     content_would_refuse={} unlisted_inputs={} locked_own_holdings={}",
                    c.signed,
                    c.hash_match,
                    c.hash_mismatch,
                    c.hash_uncomputed,
                    c.facts_error,
                    c.facts_unmodelled,
                    c.outflows_to_others,
                    c.content_would_refuse,
                    c.facts_unlisted_inputs,
                    c.locked_own_holdings
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

/// Records one signed transaction into [`SURVEY`].
pub fn record_signed(
    expectation: &OperationExpectation,
    party: &str,
    hash_check: &HashCheck,
    facts: &Result<TxFacts, String>,
) {
    SURVEY.record(operation_kind(expectation), party, hash_check, facts);
}

/// Records one signed transaction into [`SURVEY`]; for an agent that is not linked, also
/// runs the content check a linked agent would, and only logs its verdict.
pub fn record_signed_observed(
    expectation: &OperationExpectation,
    party: &str,
    request: OwnRequest<'_>,
    verification: &VerificationResult,
    facts: &Result<TxFacts, String>,
    linked: bool,
) {
    let content = if linked { None } else { observe_content(expectation, party, request, verification, facts) };
    SURVEY.record_with(operation_kind(expectation), party, &verification.hash_check, facts, content.as_ref());
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
        ContentRefusal::UnexpectedOutflow { flow, .. } => format!(
            "unexpected_outflow {} {} ({:?}) to {}",
            flow.amount,
            flow.instrument_id,
            flow.kind,
            short_hash(&flow.to)
        ),
    }
}

/// Logs [`SURVEY`] every [`SUMMARY_INTERVAL`] until shutdown; started once per process.
pub fn spawn_summary(shutdown: Shutdown) {
    if SUMMARY_SPAWNED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let task_shutdown = shutdown.clone();
    let spawned = supervise::spawn_supervised("control survey summary", shutdown, Policy::Restart, move || {
        summary_loop(task_shutdown.clone())
    });
    if let Err(e) = spawned {
        SUMMARY_SPAWNED.store(false, Ordering::SeqCst);
        warn!(target: "control_survey", "control survey summary not started: {e:#}");
    }
}

async fn summary_loop(shutdown: Shutdown) {
    loop {
        let stopping = shutdown.sleep(SUMMARY_INTERVAL).await;
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
        sync::lock(&survey.kinds).insert("pay_fee", Counters { signed: u64::MAX, ..Counters::default() });
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
        assert!(line.contains("content_would_refuse=1 unlisted_inputs=1 locked_own_holdings=2"), "{line}");
        assert!(line.contains("split_cc signed=1 ") && line.contains("content_would_refuse=0"), "{line}");
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
        let verification = VerificationResult {
            accepted: true,
            computed_hash: [7u8; 32],
            summary: String::new(),
            warnings: Vec::new(),
            rejection_reason: None,
            burn_holding_cids: Vec::new(),
            hash_check: HashCheck::Match,
            coverage: tx_verifier::Coverage::Fields,
        };
        let e = OperationExpectation::TransferCc {
            sender_party: ME.into(),
            receiver_party: OTHER.into(),
            amount: "2.5".into(),
            command_id: "c".into(),
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
        record_signed_observed(&e, ME, OwnRequest::Ledger(None), &verification, &facts("eve::1220ee"), false);
        assert!(count() > before);
    }

    #[test]
    fn recipients_are_logged_as_short_hashes() {
        let h = short_hash(OTHER);
        assert_eq!(h.len(), SHORT_HASH_BYTES * 2);
        assert_ne!(h, short_hash(ME));
        assert!(!h.contains("bob"));
    }
}
