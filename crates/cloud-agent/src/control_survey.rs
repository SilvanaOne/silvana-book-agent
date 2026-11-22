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
use tx_verifier::{HashCheck, OperationExpectation};

/// How often the counters are logged.
pub const SUMMARY_INTERVAL: Duration = Duration::from_secs(600);
/// Bytes of SHA-256 shown for a party in logs.
const SHORT_HASH_BYTES: usize = 6;

/// The process-wide survey that the submit paths record into.
pub static SURVEY: Survey = Survey::new();

static SUMMARY_SPAWNED: AtomicBool = AtomicBool::new(false);

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
        let mut seen = Counters { signed: 1, ..Counters::default() };
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
                     facts_error={} facts_unmodelled={} outflows_to_others={}",
                    c.signed,
                    c.hash_match,
                    c.hash_mismatch,
                    c.hash_uncomputed,
                    c.facts_error,
                    c.facts_unmodelled,
                    c.outflows_to_others
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

    #[test]
    fn recipients_are_logged_as_short_hashes() {
        let h = short_hash(OTHER);
        assert_eq!(h.len(), SHORT_HASH_BYTES * 2);
        assert_ne!(h, short_hash(ME));
        assert!(!h.contains("bob"));
    }
}
