//! RFQ V2 (AtomicDVP) taker state machine (design §7.1).
//!
//! Analog of `accept_settle::MulticallSettler` for the atomic path: one
//! accepted envelope → H14 pre-check → own-holdings selection + reservation →
//! `PrepareAtomicTransaction(AtomicDvpSettleParams)` → tx-verify → sign →
//! `ExecuteAtomicTransaction` — ONE transaction, no proposal ladder.
//!
//! Failure classification is exactly-once disciplined: an ambiguous execute
//! failure passes through the mandatory reconciliation gate before anything
//! is released or re-quoted (see [`AtomicSwapper::settle_envelope`]).

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use serde_json::Value;
use tracing::{debug, info, warn};

use agent_logic::clock;
use agent_logic::config::BaseConfig;
use agent_logic::confirm::ConfirmLock;
use agent_logic::supervise;
use atomic_quote::envelope::{pre_submit_check, AcsContractJson, QuoteEnvelope, QuoteJson};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params as V1Params, PreapprovalInfo, PrepareTransactionRequest,
    RequestPreapprovalParams, TransactionOperation,
};
use orderbook_proto::rfqv2::{
    prepare_atomic_transaction_request::Params as AtomicParams, AtomicDvpSettleParams,
    AtomicQuoteEnvelope, ExecuteAtomicTransactionResponse as AtomicExecuteResponse,
    PrepareAtomicTransactionRequest,
};
use tx_verifier::OperationExpectation;

use crate::fill_loop::{FillDirection, MarkerNotSaved};
use crate::holdings_cache::{
    instrument_key, CachedHolding, HoldingsCache, CC_INSTRUMENT, TEMPLATE_AMULET, TEMPLATE_HOLDING,
};
use crate::ledger_client::{
    self, is_ambiguous_execute_error, within, AtomicProviderClient, DAppProviderClient,
};

/// H14 safety margin: the signed window must exceed this at pre-check time
/// (the reference CLI uses 0; agents leave room for prepare+sign+execute).
/// Shared with `ledger_client`'s submit-retry loop, which uses the same budget
/// to decide whether re-preparing can still land inside the signed window.
pub(crate) const PRECHECK_VALIDITY_MARGIN_MICROS: i64 = 10_000_000;

/// Grace beyond the signed `valid_until` for the taker's own input
/// reservations (mirrors the LP's `settle_grace_secs` default).
const RESERVATION_GRACE: Duration = Duration::from_secs(30);

/// Reconciliation gate polling: attempts x interval before concluding the
/// user's own inputs are still live.
const RECONCILE_ATTEMPTS: u32 = 3;
const RECONCILE_POLL: Duration = Duration::from_secs(10);

/// Prepare-stage transient errors are retried within the validity window,
/// bounded by this (nothing has reached the ledger yet — safe).
const MAX_PREPARE_RETRIES: u32 = 5;

/// Hard budget for the cold-cache blob backfill (client creation + targeted
/// `GetAtomicContracts`) in [`AtomicSwapper::select_leg_with_backfill`].
/// Healthy-path cost is well under a second; without this cap a degraded
/// ledger service holds the round for the channel timeouts (~30 s connect +
/// ~120 s request) where the pre-backfill path was ceilinged at the 28 s
/// passive wait. On timeout the round falls through to the passive re-poll,
/// whose deadline stays anchored on the signed window.
const BACKFILL_BUDGET: Duration = Duration::from_secs(10);

/// Longest signed validity accepted: the LP-side maximum plus clock-skew slack.
const MAX_ACCEPTED_VALIDITY: Duration = Duration::from_secs(3_600 + 300);

/// Bound on one reconciliation poll of the own input holdings.
const RECONCILE_CALL_BUDGET: Duration = Duration::from_secs(20);

/// Registry pre-check rejections and builder pre-check bails are deterministic
/// within a quote window: re-preparing the same inputs cannot succeed.
fn is_deterministic_prepare_rejection(msg: &str) -> bool {
    const MARKERS: &[&str] = &[
        "holdings are invalid",
        "not found or may already be archived",
        "different instrument ids",
        "no holdings provided",
        "cannot be pre-checked",
        "no representative holding could provide",
        "could represent the settlement fee",
        "cannot complete one-step",
        "not direct",
    ];
    let lower = msg.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// A committed atomic fill.
#[derive(Debug, Clone)]
pub struct AtomicFill {
    pub update_id: String,
    /// Filled base quantity (the signed quote's `base_amount`).
    pub filled_base: f64,
}

/// Outcome of one envelope settle attempt.
pub enum SwapOutcome {
    /// The settle committed (directly, or established via reconciliation).
    Filled(AtomicFill),
    /// Round failed cleanly — own reservations released; request fresh quotes.
    Requote { reason: String },
    /// Commit status could not be established (reconciliation gate exhausted).
    /// The caller MUST stop: a blind re-quote after an unnoticed success is a
    /// second real settle (double fill). Own reservations are left to expire.
    Abort { reason: String },
    /// `--dry-run`: prepared + verified, never signed or executed.
    DryRun,
}

/// Taker-side driver for the atomic settle round.
pub struct AtomicSwapper {
    pub config: BaseConfig,
    /// Shared holdings cache (populated by the fill backend's ACS worker).
    pub cache: Arc<HoldingsCache>,
    /// Select the splitter reserve too (a reserve-enabled cache withholds the
    /// party's LARGEST holding per instrument for the LP ladder). The taker
    /// spends the user's own funds, so a harness sharing one cache across
    /// both roles sets this; the production fill backend's cache has the
    /// reserve off and leaves it false.
    pub spend_reserve: bool,
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
    pub confirm: bool,
    pub confirm_lock: ConfirmLock,
}

// ============================================================================
// Envelope conversion (proto transport → reference file-format shape)
// ============================================================================

fn acs_from_proto(c: &orderbook_proto::rfqv2::AtomicAcsContract) -> Result<AcsContractJson> {
    Ok(AcsContractJson {
        contract_id: c.contract_id.clone(),
        template_id: c.template_id.clone(),
        created_event_blob: c.created_event_blob.clone(),
        payload: serde_json::from_str(&c.payload_json)
            .with_context(|| format!("invalid payload_json on contract {}", c.contract_id))?,
    })
}

/// Proto `AtomicQuoteEnvelope` → reference [`QuoteEnvelope`] for the H14
/// pre-check. Transport-only fields (rfq_id, quote_id, lp_party_id, market_id)
/// are dropped; disclosed contracts become the canonical 4-camelCase-key shape.
pub fn envelope_from_proto(env: &AtomicQuoteEnvelope) -> Result<QuoteEnvelope> {
    let dvp = env.dvp.as_ref().ok_or_else(|| anyhow!("envelope missing dvp contract"))?;
    let quote = env.quote.as_ref().ok_or_else(|| anyhow!("envelope missing quote"))?;
    Ok(QuoteEnvelope {
        version: env.version.clone(),
        synchronizer_id: env.synchronizer_id.clone(),
        dvp: acs_from_proto(dvp)?,
        quote: QuoteJson {
            quote_id: quote.quote_id.clone(),
            ticket_id: quote.ticket_id.clone(),
            user: quote.user.clone(),
            side: quote.side.clone(),
            base_amount: quote.base_amount.clone(),
            quote_amount: quote.quote_amount.clone(),
            created_at_micros: quote.created_at_micros.to_string(),
            valid_until_micros: quote.valid_until_micros.to_string(),
            lp_fees: if quote.lp_fees.is_empty() {
                None
            } else {
                Some(
                    quote
                        .lp_fees
                        .iter()
                        .map(|f| atomic_quote::envelope::LpFeeJson {
                            receiver: f.receiver.clone(),
                            instrument_id: atomic_quote::envelope::InstrumentIdJson {
                                admin: f.instrument_admin.clone(),
                                id: f.instrument_id.clone(),
                            },
                            amount: f.amount.clone(),
                        })
                        .collect(),
                )
            },
        },
        canonical_message: env.canonical_message.clone(),
        quote_signature: env.quote_signature.clone(),
        ticket: env.ticket.as_ref().map(acs_from_proto).transpose()?,
        lp_input_holding_cids: env.lp_input_holding_cids.clone(),
        disclosed: env
            .disclosed
            .iter()
            .map(|d| {
                serde_json::json!({
                    "contractId": d.contract_id,
                    "templateId": d.template_id,
                    "createdEventBlob": d.created_event_blob,
                    "synchronizerId": d.synchronizer_id,
                })
            })
            .collect(),
    })
}

// ============================================================================
// Clients
// ============================================================================

pub async fn create_atomic_client(config: &BaseConfig) -> Result<AtomicProviderClient> {
    AtomicProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
}

pub async fn create_v1_client(config: &BaseConfig) -> Result<DAppProviderClient> {
    DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
}

// ============================================================================
// Receiver-preapproval preflight (design §6.5)
// ============================================================================

/// Ensure this party holds a utility `TransferPreapproval` for the instrument
/// it RECEIVES (registry transfer factories only return a one-step Completed
/// context when the receiver is preapproved). CC needs none (the two-step
/// in-tx accept is forced by design). Returns `true` when a preapproval was
/// created, `false` when already present / not needed.
pub async fn ensure_receiver_preapproval(
    config: &BaseConfig,
    client: &mut DAppProviderClient,
    orderbook_instrument_id: &str,
    verbose: bool,
    dry_run: bool,
    force: bool,
) -> Result<bool> {
    let (on_chain_id, admin) = config.resolve_instrument(orderbook_instrument_id);
    if on_chain_id == "Amulet" {
        return Ok(false); // CC leg: no preapproval needed
    }
    if admin.is_empty() {
        bail!(
            "cannot resolve registry admin for instrument '{}' — instrument registry not populated",
            orderbook_instrument_id
        );
    }

    let call_wait = ledger_client::call_wait(config.request_timeout_secs);
    let existing = within("GetPreapprovals", call_wait, client.get_preapprovals()).await?;
    if preapproval_present(&existing, &admin) {
        info!(
            "Receiver preapproval for {} (admin {}) already present",
            on_chain_id, admin
        );
        return Ok(false);
    }

    // Operator resolution mirrors `run_preapproval`: match the faucet
    // instrument list by registry.
    let faucet_instruments = within("ListFaucetInstruments", call_wait, client.list_faucet_instruments())
        .await
        .context("failed to fetch faucet instruments for preapproval operator resolution")?;
    let operator = crate::operator_for_registry(&faucet_instruments, &admin)?;

    info!(
        "Creating receiver preapproval for {} (admin {}, operator {})",
        on_chain_id, admin, operator
    );
    let expectation = OperationExpectation::RequestPreapproval {
        party: config.party_id.clone(),
    };
    let request = PrepareTransactionRequest {
        operation: TransactionOperation::RequestPreapproval as i32,
        params: Some(V1Params::RequestPreapproval(RequestPreapprovalParams {
            instrument_admin: admin.clone(),
            instrument_allowances: vec![],
            operator,
        })),
        request_signature: None,
    };
    // The submit runs in its own task; past its wait the ledger state decides
    let mut submitter = client.clone();
    let submit = supervise::try_spawn("receiver preapproval", async move {
        submitter
            .submit_transaction(request, &expectation, verbose, dry_run, force)
            .await
    });
    let Some(mut submit) = submit else {
        bail!("receiver preapproval submission not started: no tokio runtime");
    };
    let wait = ledger_client::SUBMIT_BUDGET.max(Duration::from_secs(config.request_timeout_secs));
    let lost = match tokio::time::timeout(wait, &mut submit).await {
        Ok(Ok(result)) => {
            let result = result.context("receiver preapproval submission failed")?;
            info!("Receiver preapproval created (update {})", result.update_id);
            return Ok(true);
        }
        Ok(Err(e)) => format!("ended abnormally ({e})"),
        Err(_) => {
            let _ = supervise::try_spawn("receiver preapproval outcome", async move {
                match submit.await {
                    Ok(Ok(r)) => info!("Receiver preapproval created after the wait (update {})", r.update_id),
                    Ok(Err(e)) => warn!("Receiver preapproval submission failed after the wait: {e:#}"),
                    Err(e) => warn!("Receiver preapproval submission ended after the wait: {e}"),
                }
            });
            format!("still running after {wait:?}")
        }
    };
    let after = within("GetPreapprovals", call_wait, client.get_preapprovals()).await?;
    if preapproval_present(&after, &admin) {
        info!(
            "Receiver preapproval for {} (admin {}) present; its submission {}",
            on_chain_id, admin, lost
        );
        return Ok(true);
    }
    bail!("receiver preapproval submission {lost}, and no preapproval for admin {admin} is visible yet")
}

/// Whether a full (no allowance list) preapproval for `admin` exists.
fn preapproval_present(existing: &[PreapprovalInfo], admin: &str) -> bool {
    existing
        .iter()
        .any(|p| p.instrument_admin == admin && p.instrument_allowances.is_empty())
}

// ============================================================================
// The settle round
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileStatus {
    Consumed,
    Live,
    Unknown,
}

/// (verbose, dry_run, force) for the submit.
type SubmitFlags = (bool, bool, bool);

/// The client handed back by a finished submit, with its result.
type SubmitEnd = (AtomicProviderClient, Result<AtomicExecuteResponse>);

/// A submit with no result: still running past its wait, or its task failed.
/// Either way it may have executed.
struct LostSubmit<T> {
    running: Option<tokio::task::JoinHandle<T>>,
    why: String,
}

enum SubmitWatch<T> {
    Done(T),
    Lost(LostSubmit<T>),
}

/// Run `task` on its own, so a slow submit is never cancelled, and wait up to `wait`.
async fn watch_task<T, F>(task: F, wait: Duration) -> SubmitWatch<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let Some(mut handle) = supervise::try_spawn("atomic submit", task) else {
        return SubmitWatch::Lost(LostSubmit { running: None, why: "not started".to_string() });
    };
    match tokio::time::timeout(wait, &mut handle).await {
        Ok(Ok(end)) => SubmitWatch::Done(end),
        Ok(Err(e)) => SubmitWatch::Lost(LostSubmit { running: None, why: format!("ended abnormally ({e})") }),
        Err(_) => SubmitWatch::Lost(LostSubmit {
            running: Some(handle),
            why: format!("gave no result within {wait:?}"),
        }),
    }
}

/// The production submit: prepare, verify, sign and execute, handing the client back.
async fn submit_atomic(
    mut client: AtomicProviderClient,
    req: PrepareAtomicTransactionRequest,
    expectation: OperationExpectation,
    (verbose, dry_run, force): SubmitFlags,
) -> SubmitEnd {
    let result = client
        .submit_atomic_transaction(req, &expectation, verbose, dry_run, force)
        .await;
    (client, result)
}

/// How long to wait for one submit: the rest of the signed window plus the
/// pre-check margin and the reservation grace.
fn submit_wait(valid_until_micros: i64, now_micros: i64) -> Duration {
    let remaining = u64::try_from(valid_until_micros.saturating_sub(now_micros)).unwrap_or(0);
    let margin = u64::try_from(PRECHECK_VALIDITY_MARGIN_MICROS).unwrap_or(0);
    Duration::from_micros(remaining.saturating_add(margin)).saturating_add(RESERVATION_GRACE)
}

/// Why a signed validity ending more than [`MAX_ACCEPTED_VALIDITY`] ahead is
/// refused; `None` when it is acceptable.
fn validity_too_far(valid_until_micros: i64, now_micros: i64) -> Option<String> {
    let max_micros = i64::try_from(MAX_ACCEPTED_VALIDITY.as_micros()).unwrap_or(i64::MAX);
    match valid_until_micros.checked_sub(now_micros) {
        Some(ahead) if ahead <= max_micros => None,
        _ => Some(format!(
            "signed validity ends at {valid_until_micros} µs, more than {}s ahead (now {now_micros} µs)",
            MAX_ACCEPTED_VALIDITY.as_secs()
        )),
    }
}

/// Expiry of the own-input reservation: the end of the signed window plus
/// the grace; `None` when that is not a representable instant.
fn reservation_expiry(now: Instant, valid_until_micros: i64, now_micros: i64) -> Option<Instant> {
    let remaining = u64::try_from(valid_until_micros.saturating_sub(now_micros)).unwrap_or(0);
    now.checked_add(Duration::from_micros(remaining))?
        .checked_add(RESERVATION_GRACE)
}

/// Log how a submit that outlived its wait ends.
fn log_late_outcome<T, O>(quote_id: &str, task: tokio::task::JoinHandle<T>, outcome: O)
where
    T: Send + 'static,
    O: FnOnce(T) -> Result<AtomicExecuteResponse> + Send + 'static,
{
    let quote_id = quote_id.to_string();
    let _ = supervise::try_spawn("atomic submit outcome", async move {
        match task.await.map(outcome) {
            Ok(Ok(r)) => warn!(
                "Atomic submit for quote {} finished late: update={} success={}",
                quote_id, r.update_id, r.success
            ),
            Ok(Err(e)) => warn!("Atomic submit for quote {} failed late: {:#}", quote_id, e),
            Err(e) => warn!("Atomic submit for quote {} ended late: {}", quote_id, e),
        }
    });
}

/// Passive-wait deadline for a leg selection (LP confirm-path parity): an
/// empty selection is often a seconds-long gap — fresh proceeds blob-pending
/// until the next ACS snapshot, holdings momentarily reserved — not
/// depletion. Wait inside the signed validity window, keeping the pre-check
/// margin for prepare+sign+execute — but only when the cache holds enough in
/// PRINCIPLE (counting stale/blob-pending entries a refresh can revive): a
/// genuinely underfunded taker must fail fast, not burn the window.
async fn selection_deadline(
    cache: &HoldingsCache,
    key: &str,
    target: Decimal,
    valid_until_micros: i64,
) -> Instant {
    if cache.total_available_amount(key).await < target {
        return Instant::now();
    }
    let usable_micros = valid_until_micros
        .saturating_sub(PRECHECK_VALIDITY_MARGIN_MICROS)
        .saturating_sub(clock::now_micros_i64());
    let usable = Duration::from_micros(u64::try_from(usable_micros).unwrap_or(0));
    let now = Instant::now();
    now.checked_add(usable.min(crate::rfq_v2::MAX_CONFIRM_SPLIT_WAIT)).unwrap_or(now)
}

impl AtomicSwapper {
    /// One accepted-envelope round: H14 pre-check → select+reserve own inputs
    /// → prepare/verify/sign/execute → confirm or classify the failure.
    ///
    /// `accepted` is the indicative quote the user accepted (`AtomicQuoteInfo`);
    /// `max_input_holdings` is the per-market config cap (protocol bound 100).
    pub async fn settle_envelope(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &orderbook_proto::rfqv2::AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
    ) -> Result<SwapOutcome> {
        let rounds = (submit_atomic, submit_wait);
        self.settle_envelope_with(envelope, accepted, direction, max_input_holdings, rounds, || true)
            .await
    }

    /// [`Self::settle_envelope`] that calls `before_submit` once, when nothing
    /// is submitted yet and the submit is next; false releases and stops the round.
    pub(crate) async fn settle_envelope_marked(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &orderbook_proto::rfqv2::AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
        before_submit: impl FnOnce() -> bool,
    ) -> Result<SwapOutcome> {
        let rounds = (submit_atomic, submit_wait);
        self.settle_envelope_with(envelope, accepted, direction, max_input_holdings, rounds, before_submit)
            .await
    }

    /// [`Self::settle_envelope_marked`] with the submit and its wait supplied.
    async fn settle_envelope_with<X, F, W>(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &orderbook_proto::rfqv2::AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
        (submit, wait_for): (X, W),
        before_submit: impl FnOnce() -> bool,
    ) -> Result<SwapOutcome>
    where
        X: Fn(AtomicProviderClient, PrepareAtomicTransactionRequest, OperationExpectation, SubmitFlags) -> F,
        F: std::future::Future<Output = SubmitEnd> + Send + 'static,
        W: Fn(i64, i64) -> Duration,
    {
        // ---- H14 PRECHECK ----------------------------------------------
        let env = envelope_from_proto(envelope)?;
        let now_micros = clock::now_micros_i64();
        if let Err(e) = pre_submit_check(
            &env,
            &self.config.party_id,
            &self.config.synchronizer_id,
            now_micros,
            PRECHECK_VALIDITY_MARGIN_MICROS,
        ) {
            // Nothing reserved yet — reject the quote and let the caller
            // move on to the next quote/LP.
            return Ok(SwapOutcome::Requote {
                reason: format!("H14 pre-check failed: {e:#}"),
            });
        }
        let valid_until_micros: i64 = env.quote.valid_until_micros.parse()?;
        if let Some(reason) = validity_too_far(valid_until_micros, now_micros) {
            // Refused before anything is reserved, never clamped
            return Ok(SwapOutcome::Requote { reason });
        }

        let venue = |ptr: &str| -> Result<String> {
            env.dvp
                .payload
                .pointer(ptr)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow!("AtomicDVP venue payload missing {ptr}"))
        };
        let lp_party = venue("/lp")?;
        if lp_party != accepted.lp_party_id {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "H14: venue lp {} does not match the quoted LP {}",
                    lp_party, accepted.lp_party_id
                ),
            });
        }
        // Amounts must equal the accepted indicative quote (decimal compare —
        // the LP must not re-price at confirm).
        let base_amount = Decimal::from_str(&env.quote.base_amount)
            .with_context(|| format!("invalid base_amount '{}'", env.quote.base_amount))?;
        let quote_amount = Decimal::from_str(&env.quote.quote_amount)
            .with_context(|| format!("invalid quote_amount '{}'", env.quote.quote_amount))?;
        let accepted_base = Decimal::from_str(&accepted.quantity)
            .with_context(|| format!("invalid accepted quantity '{}'", accepted.quantity))?;
        let accepted_quote = Decimal::from_str(&accepted.quote_quantity)
            .with_context(|| format!("invalid accepted quote_quantity '{}'", accepted.quote_quantity))?;
        if base_amount != accepted_base || quote_amount != accepted_quote {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "H14: signed amounts ({}, {}) differ from accepted quote ({}, {})",
                    base_amount, quote_amount, accepted_base, accepted_quote
                ),
            });
        }
        let expected_side = match direction {
            FillDirection::Buy => "Buy",
            FillDirection::Sell => "Sell",
        };
        if env.quote.side != expected_side {
            return Ok(SwapOutcome::Requote {
                reason: format!("H14: quote side {} != {}", env.quote.side, expected_side),
            });
        }
        // Fee consent (design §14 D21): the SIGNED lpFees must equal the fee
        // displayed on the accepted quote — neither the LP nor the relay can
        // raise the fee after display.
        let signed_fees = envelope
            .quote
            .as_ref()
            .map(|q| q.lp_fees.clone())
            .unwrap_or_default();
        let displayed_fees: Vec<_> = accepted.settlement_fee.iter().cloned().collect();
        if signed_fees != displayed_fees {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "signed lpFees {:?} differ from the displayed settlement fee {:?}",
                    signed_fees, displayed_fees
                ),
            });
        }

        // ---- SELECT own holdings (sending leg) --------------------------
        // User Buy ⇒ user pays quote leg; Sell ⇒ user pays base leg.
        let (pay_admin, pay_id, amount_needed) = match direction {
            FillDirection::Buy => (
                venue("/quoteInstrumentId/admin")?,
                venue("/quoteInstrumentId/id")?,
                quote_amount,
            ),
            FillDirection::Sell => (
                venue("/baseInstrumentId/admin")?,
                venue("/baseInstrumentId/id")?,
                base_amount,
            ),
        };
        let is_cc = pay_id == "Amulet";
        let pay_key = if is_cc {
            CC_INSTRUMENT.to_string()
        } else {
            instrument_key(&pay_admin, &pay_id)
        };
        // Fee funding (design §14 D21, generalized for fee-token selection):
        // fees ride the ONE user-side pool — coverage must be GUARANTEED, not
        // incidental. Per fee instrument: CC targets fee × 1.02 + 1 CC (amulet
        // sender-fee headroom, the dvp CLI margin); utility tokens (USDC…)
        // transfer one-step via the receiver's preapproval with no
        // token-denominated sender fee, so the exact amount suffices.
        let mut fee_totals: Vec<((String, String), Decimal)> = Vec::new();
        for f in &signed_fees {
            let Ok(a) = Decimal::from_str(&f.amount) else {
                continue;
            };
            let key = (f.instrument_admin.clone(), f.instrument_id.clone());
            match fee_totals.iter_mut().find(|(k, _)| *k == key) {
                Some((_, total)) => {
                    let Some(sum) = total.checked_add(a) else {
                        return Ok(SwapOutcome::Requote {
                            reason: format!("settlement fee total in {} is out of range", key.1),
                        });
                    };
                    *total = sum;
                }
                None => fee_totals.push((key, a)),
            }
        }
        // `None` when the funding target is out of range
        let fee_target = |id: &str, total: Decimal| -> Option<Decimal> {
            if id == "Amulet" {
                total.checked_mul(Decimal::new(102, 2))?.checked_add(Decimal::ONE)
            } else {
                Some(total)
            }
        };
        // CC is unique by id "Amulet"; utility instruments match on admin+id.
        let fee_matches = |admin: &str, id: &str, other_admin: &str, other_id: &str| {
            id == other_id && (id == "Amulet" || admin == other_admin)
        };

        // What the user RECEIVES (proceeds can fund a same-instrument fee).
        let (receive_admin, receive_id) = match direction {
            FillDirection::Buy => (
                venue("/baseInstrumentId/admin")?,
                venue("/baseInstrumentId/id")?,
            ),
            FillDirection::Sell => (
                venue("/quoteInstrumentId/admin")?,
                venue("/quoteInstrumentId/id")?,
            ),
        };
        let receive_amount = match direction {
            FillDirection::Buy => base_amount,
            FillDirection::Sell => quote_amount,
        };

        let max_inputs = max_input_holdings.min(100);
        // When a fee is denominated in the PAY leg's instrument, the leg
        // selection itself must also cover it (leg change alone can be
        // arbitrarily small).
        let select_target = fee_totals
            .iter()
            .filter(|((admin, id), _)| fee_matches(admin, id, &pay_admin, &pay_id))
            .try_fold(amount_needed, |acc, ((_, id), total)| acc.checked_add(fee_target(id, *total)?));
        let Some(select_target) = select_target else {
            return Ok(SwapOutcome::Requote {
                reason: format!("amount plus settlement fee in {pay_key} is out of range"),
            });
        };
        // Selection is backfill-aware ([`Self::select_leg_with_backfill`]):
        // immediate try, then an on-demand blob backfill for the cold-cache
        // case, then the bounded passive re-poll. The client opened for the
        // backfill is reused for the fee leg and the submit loop.
        let mut client_slot: Option<AtomicProviderClient> = None;
        let picks = self
            .select_leg_with_backfill(
                &mut client_slot,
                &pay_key,
                select_target,
                max_inputs,
                is_cc,
                valid_until_micros,
            )
            .await;
        let Some(mut picks) = picks else {
            return Ok(SwapOutcome::Requote {
                reason: format!(
                    "own holdings selection failed for {} (need {}; {})",
                    pay_key,
                    select_target,
                    self.selection_failure_detail(&pay_key, select_target).await
                ),
            });
        };
        // Fees in OTHER instruments: covered by same-instrument proceeds when
        // large enough, else add dedicated funding cids to the pool.
        for ((admin, id), total) in &fee_totals {
            if fee_matches(admin, id, &pay_admin, &pay_id) {
                continue; // already inside select_target
            }
            let Some(target) = fee_target(id, *total) else {
                return Ok(SwapOutcome::Requote {
                    reason: format!("{id} settlement fee target is out of range"),
                });
            };
            if target <= Decimal::ZERO {
                continue;
            }
            if fee_matches(admin, id, &receive_admin, &receive_id) && receive_amount >= target {
                continue; // proceeds fund the fee
            }
            let fee_key = if id == "Amulet" {
                CC_INSTRUMENT.to_string()
            } else {
                instrument_key(admin, id)
            };
            let slots = max_inputs.saturating_sub(picks.len()).max(1);
            // Same backfill-aware selection as the pay leg — fee cids are
            // blob-pending just as often (the shared pool's change outputs),
            // and the passive-wait deadline recomputes off valid_until so the
            // window shrinks by whatever the pay leg already used.
            let fee_picks = self
                .select_leg_with_backfill(
                    &mut client_slot,
                    &fee_key,
                    target,
                    slots,
                    id == "Amulet",
                    valid_until_micros,
                )
                .await;
            let Some(fee_picks) = fee_picks else {
                return Ok(SwapOutcome::Requote {
                    reason: format!(
                        "{id} fee-funding selection failed (need {target} {id} for the settlement fee; {})",
                        self.selection_failure_detail(&fee_key, target).await
                    ),
                });
            };
            picks.extend(fee_picks);
        }
        let own_cids: Vec<String> = picks.iter().map(|h| h.contract_id.clone()).collect();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for h in &picks {
            let count = counts.entry(h.instrument.as_str()).or_default();
            *count = count.saturating_add(1);
        }
        debug!(quote = %env.quote.quote_id, pool = ?counts, "own input holdings by instrument");

        // Fresh timestamp: now_micros is from the pre-check, and the bounded
        // selection re-poll may have slept since — anchoring on it would
        // extend the reservation past valid_until + grace by the waited time.
        let expires_at =
            reservation_expiry(Instant::now(), valid_until_micros, clock::now_micros_i64());
        let Some(expires_at) = expires_at else {
            return Ok(SwapOutcome::Requote {
                reason: format!("reservation window of quote {} is out of range", env.quote.quote_id),
            });
        };
        if !self
            .cache
            .reserve_v2(&own_cids, &env.quote.quote_id, expires_at)
            .await
        {
            return Ok(SwapOutcome::Requote {
                reason: "own holdings reservation raced".to_string(),
            });
        }

        // ---- PREPARE params straight from the envelope -------------------
        let params = AtomicDvpSettleParams {
            venue_cid: env.dvp.contract_id.clone(),
            quote: envelope.quote.clone(),
            quote_signature_der_hex: envelope.quote_signature.clone(),
            canonical_message: envelope.canonical_message.clone(),
            lp_party: lp_party.clone(),
            pair_name: venue("/pairName")?,
            base_instrument_id: venue("/baseInstrumentId/id")?,
            base_instrument_admin: venue("/baseInstrumentId/admin")?,
            quote_instrument_id: venue("/quoteInstrumentId/id")?,
            quote_instrument_admin: venue("/quoteInstrumentId/admin")?,
            quote_public_key_spki_hex: venue("/quotePublicKey")?,
            ticket_cid: env.ticket.as_ref().map(|t| t.contract_id.clone()),
            lp_input_holding_cids: envelope.lp_input_holding_cids.clone(),
            envelope_disclosures: envelope.disclosed.clone(),
            user_input_holding_cids: own_cids.clone(),
            synchronizer_id: self.config.synchronizer_id.clone(),
            lp_fees: signed_fees.clone(),
        };
        let expectation = OperationExpectation::AtomicDvpSettle {
            user_party: self.config.party_id.clone(),
            venue_cid: params.venue_cid.clone(),
            template_id: env.dvp.template_id.clone(),
            quote_id: env.quote.quote_id.clone(),
            ticket_id: env.quote.ticket_id.clone(),
            ticket_cid: params.ticket_cid.clone(),
            side: env.quote.side.clone(),
            base_amount: env.quote.base_amount.clone(),
            quote_amount: env.quote.quote_amount.clone(),
            lp_party,
            base_instrument_id: params.base_instrument_id.clone(),
            base_instrument_admin: params.base_instrument_admin.clone(),
            quote_instrument_id: params.quote_instrument_id.clone(),
            quote_instrument_admin: params.quote_instrument_admin.clone(),
            valid_until_micros,
            lp_input_holding_cids: params.lp_input_holding_cids.clone(),
            user_input_holding_cids: own_cids.clone(),
        };
        let req = PrepareAtomicTransactionRequest {
            params: Some(AtomicParams::AtomicDvpSettle(params)),
            request_signature: None,
        };

        if self.confirm && !self.dry_run {
            if let Err(e) = agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Atomic DVP settle",
                &format!(
                    "quote {}: {} {} base / {} quote (LP {})",
                    env.quote.quote_id,
                    env.quote.side,
                    env.quote.base_amount,
                    env.quote.quote_amount,
                    accepted.lp_name
                ),
            )
            .await
            {
                self.cache.release_reservations(&own_cids).await;
                return Err(e);
            }
        }

        let filled_base = base_amount.try_into().unwrap_or(0.0_f64);

        // ---- VERIFY + SIGN + EXEC (with FAIL classification) -------------
        // Reuse the client the cold-cache backfill may have opened.
        let mut atomic_client = match client_slot.take() {
            Some(c) => c,
            None => {
                let connect_wait = ledger_client::connect_wait(
                    self.config.connection_timeout_secs,
                    self.config.request_timeout_secs,
                );
                match tokio::time::timeout(connect_wait, create_atomic_client(&self.config)).await {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        self.cache.release_reservations(&own_cids).await;
                        return Err(e);
                    }
                    Err(_) => {
                        self.cache.release_reservations(&own_cids).await;
                        return Ok(SwapOutcome::Requote {
                            reason: format!("ledger connect did not finish within {connect_wait:?}"),
                        });
                    }
                }
            }
        };

        let flags = (self.verbose, self.dry_run, self.force);
        let mut prepare_retries = 0u32;
        if !before_submit() {
            self.cache.release_reservations(&own_cids).await;
            return Err(MarkerNotSaved(format!("quote {}", env.quote.quote_id)).into());
        }
        loop {
            // The submit runs in its own task and is never cancelled mid-execute
            let wait = wait_for(valid_until_micros, clock::now_micros_i64());
            let submit_task = submit(atomic_client, req.clone(), expectation.clone(), flags);
            let result = match watch_task(submit_task, wait).await {
                SubmitWatch::Done((client, result)) => {
                    atomic_client = client;
                    result
                }
                SubmitWatch::Lost(lost) => {
                    let reconcile = self.poll_own_inputs(&own_cids);
                    let outcome = |(_, result): SubmitEnd| result;
                    return Ok(self
                        .after_lost_submit(&own_cids, &env.quote.quote_id, filled_base, lost, outcome, reconcile)
                        .await);
                }
            };

            match result {
                Ok(resp) if resp.success => {
                    // CONFIRM — process_tx_result semantics: consume own
                    // inputs (drops their reservations), adopt created holdings.
                    self.cache.mark_consumed(&own_cids, &resp.update_id).await;
                    let adopt = self.adopt_created(&mut atomic_client, &resp.created_contracts_json);
                    if tokio::time::timeout(BACKFILL_BUDGET, adopt).await.is_err() {
                        warn!(
                            "Adopting settle-created holdings took over {BACKFILL_BUDGET:?}; the ACS refresh will catch up"
                        );
                    }
                    return Ok(SwapOutcome::Filled(AtomicFill {
                        update_id: resp.update_id,
                        filled_base,
                    }));
                }
                Ok(_) => {
                    // Only the --dry-run path returns Ok with success=false.
                    self.cache.release_reservations(&own_cids).await;
                    return Ok(SwapOutcome::DryRun);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let names_own_cid = own_cids.iter().any(|c| msg.contains(c.as_str()));

                    // RECONCILIATION GATE (design §7.1 FAIL): an ambiguous
                    // execute failure, or INACTIVE_CONTRACTS naming the user's
                    // OWN input cids, means the settle MAY HAVE LANDED. A
                    // same-envelope replay aborts on-ledger, but a re-quote
                    // after an unnoticed success is a SECOND real settle — the
                    // user fills (and pays) twice. Reconcile via ledger state
                    // BEFORE releasing or re-quoting; unknowable ⇒ Abort.
                    if is_ambiguous_execute_error(&e)
                        || (msg.contains("INACTIVE_CONTRACTS") && names_own_cid)
                    {
                        warn!(
                            "Ambiguous atomic execute for quote {} — reconciling own inputs before anything else: {}",
                            env.quote.quote_id, msg
                        );
                        return match self.poll_own_inputs(&own_cids).await {
                            ReconcileStatus::Consumed => {
                                info!(
                                    "Reconciliation: own inputs consumed — the settle for quote {} COMMITTED",
                                    env.quote.quote_id
                                );
                                self.cache
                                    .mark_consumed(&own_cids, "atomic-settle-reconciled")
                                    .await;
                                Ok(SwapOutcome::Filled(AtomicFill {
                                    update_id: "(reconciled; execute response lost)".to_string(),
                                    filled_base,
                                }))
                            }
                            ReconcileStatus::Live => {
                                self.cache.release_reservations(&own_cids).await;
                                Ok(SwapOutcome::Requote {
                                    reason: format!(
                                        "execute did not commit (own inputs verified live): {msg}"
                                    ),
                                })
                            }
                            ReconcileStatus::Unknown => Ok(SwapOutcome::Abort {
                                reason: format!(
                                    "cannot establish commit status of quote {} — check the ledger manually before re-quoting: {}",
                                    env.quote.quote_id, msg
                                ),
                            }),
                        };
                    }

                    // INACTIVE_CONTRACTS naming only LP cids: verify our own
                    // inputs are live, then it is safe to release + re-quote
                    // (the LP's ticket survives — only a real settle spends it).
                    if msg.contains("INACTIVE_CONTRACTS") {
                        return match self.poll_own_inputs(&own_cids).await {
                            ReconcileStatus::Live => {
                                self.cache.release_reservations(&own_cids).await;
                                Ok(SwapOutcome::Requote {
                                    reason: format!("LP inputs inactive (envelope stale): {msg}"),
                                })
                            }
                            ReconcileStatus::Consumed => {
                                self.cache
                                    .mark_consumed(&own_cids, "atomic-settle-reconciled")
                                    .await;
                                Ok(SwapOutcome::Filled(AtomicFill {
                                    update_id: "(reconciled; execute response lost)".to_string(),
                                    filled_base,
                                }))
                            }
                            ReconcileStatus::Unknown => Ok(SwapOutcome::Abort {
                                reason: format!(
                                    "INACTIVE_CONTRACTS with undeterminable own-input state: {msg}"
                                ),
                            }),
                        };
                    }

                    // Deterministic prepare rejections: the same inputs would
                    // be rejected again — release + re-quote without retrying.
                    if msg.contains("PrepareAtomicTransaction")
                        && is_deterministic_prepare_rejection(&msg)
                    {
                        self.cache.release_reservations(&own_cids).await;
                        return Ok(SwapOutcome::Requote {
                            reason: format!("prepare rejected (not retried): {msg}"),
                        });
                    }

                    // Prepare-stage errors: nothing was submitted — retry
                    // within the signed validity window.
                    let now = clock::now_micros_i64();
                    let window_open =
                        now.saturating_add(PRECHECK_VALIDITY_MARGIN_MICROS) < valid_until_micros;
                    if msg.contains("PrepareAtomicTransaction")
                        && window_open
                        && prepare_retries < MAX_PREPARE_RETRIES
                    {
                        prepare_retries = prepare_retries.saturating_add(1);
                        warn!(
                            "Prepare failed (attempt {}/{}), retrying within the window: {}",
                            prepare_retries, MAX_PREPARE_RETRIES, msg
                        );
                        tokio::time::sleep(Duration::from_millis(2000)).await;
                        continue;
                    }

                    // Everything else (window abort, verification rejection,
                    // exhausted retries): nothing committed — release + re-quote.
                    self.cache.release_reservations(&own_cids).await;
                    return Ok(SwapOutcome::Requote { reason: msg });
                }
            }
        }
    }

    /// RECONCILIATION GATE for a submit without a result: commit status comes
    /// from the own inputs, and a submit still running is never re-quoted.
    async fn after_lost_submit<T, O>(
        &self,
        own_cids: &[String],
        quote_id: &str,
        filled_base: f64,
        lost: LostSubmit<T>,
        outcome: O,
        reconcile: impl std::future::Future<Output = ReconcileStatus>,
    ) -> SwapOutcome
    where
        T: Send + 'static,
        O: FnOnce(T) -> Result<AtomicExecuteResponse> + Send + 'static,
    {
        let LostSubmit { running, why } = lost;
        warn!(
            "Atomic submit for quote {} {} — reconciling own inputs before anything else",
            quote_id, why
        );
        let status = reconcile.await;
        let (running, finished) = match running {
            Some(task) if task.is_finished() => (None, Some(task)),
            other => (other, None),
        };
        if let Some(task) = running {
            log_late_outcome(quote_id, task, outcome);
            return match status {
                ReconcileStatus::Consumed => self.reconciled_fill(own_cids, quote_id, filled_base).await,
                _ => SwapOutcome::Abort {
                    reason: format!(
                        "submit for quote {quote_id} {why} and is still running; own inputs not seen consumed — check the ledger before re-quoting"
                    ),
                },
            };
        }
        match status {
            ReconcileStatus::Consumed => self.reconciled_fill(own_cids, quote_id, filled_base).await,
            ReconcileStatus::Unknown => SwapOutcome::Abort {
                reason: format!(
                    "cannot establish commit status of quote {quote_id} — check the ledger manually before re-quoting: submit {why}"
                ),
            },
            ReconcileStatus::Live => {
                let late = match finished {
                    Some(task) => task.await.map(outcome),
                    None => Ok(Err(anyhow!("submit {why}"))),
                };
                match late {
                    Ok(Ok(resp)) if resp.success => {
                        self.cache.mark_consumed(own_cids, &resp.update_id).await;
                        SwapOutcome::Filled(AtomicFill { update_id: resp.update_id, filled_base })
                    }
                    Ok(Ok(_)) => {
                        self.cache.release_reservations(own_cids).await;
                        SwapOutcome::DryRun
                    }
                    Ok(Err(e)) if !is_ambiguous_execute_error(&e) => {
                        self.cache.release_reservations(own_cids).await;
                        SwapOutcome::Requote {
                            reason: format!("execute did not commit (own inputs verified live): {e:#}"),
                        }
                    }
                    Ok(Err(e)) => SwapOutcome::Abort {
                        reason: format!(
                            "cannot establish commit status of quote {quote_id} — check the ledger manually before re-quoting: {e:#}"
                        ),
                    },
                    Err(e) => SwapOutcome::Abort {
                        reason: format!(
                            "cannot establish commit status of quote {quote_id} — check the ledger manually before re-quoting: submit ended abnormally ({e})"
                        ),
                    },
                }
            }
        }
    }

    async fn reconciled_fill(&self, own_cids: &[String], quote_id: &str, filled_base: f64) -> SwapOutcome {
        info!(
            "Reconciliation: own inputs consumed — the settle for quote {} COMMITTED",
            quote_id
        );
        self.cache.mark_consumed(own_cids, "atomic-settle-reconciled").await;
        SwapOutcome::Filled(AtomicFill {
            update_id: "(reconciled; execute response lost)".to_string(),
            filled_base,
        })
    }

    /// Post-mortem detail for a failed leg selection, splitting the two very
    /// different failures that used to share one "cache cold or insufficient"
    /// message. The discriminator is the one [`selection_deadline`] and the
    /// backfill gate already use: `total_available_amount` counts holdings a
    /// refresh could revive (blob-pending, TTL-stale), so below-target there
    /// means genuinely underfunded, while at-or-above-target means the pool
    /// exists but nothing was selectable (cold cache / reservations).
    async fn selection_failure_detail(
        &self,
        key: &crate::holdings_cache::InstrumentKey,
        target: Decimal,
    ) -> String {
        let in_principle = self.cache.total_available_amount(key).await;
        if in_principle < target {
            format!("insufficient: {in_principle} available in principle")
        } else {
            format!("cache cold: {in_principle} available in principle, none selectable in time")
        }
    }

    /// Own-holdings selection for one leg (pay or fee) with cold-cache
    /// self-heal. A blob-pending cache entry is a purely LOCAL artifact — the
    /// contract already exists on-ledger with its blob (only the fast
    /// updates-watcher path dropped it), so ONE targeted `GetAtomicContracts`
    /// resolves it deterministically. Without this, a back-to-back taker's
    /// change output could stay unselectable through the whole passive wait:
    /// the 30 s ACS cycle EXCEEDS the 28 s `MAX_CONFIRM_SPLIT_WAIT` ceiling.
    ///
    /// 1. Immediate try — happy path, zero added latency.
    /// 2. Backfill — only when the cache holds enough in PRINCIPLE (the same
    ///    gate [`selection_deadline`] uses to wait rather than fail fast) and
    ///    the signed window still has the pre-check margin: fetch blobs for
    ///    the leg's otherwise-selectable blob-pending cids, retry once.
    /// 3. Passive bounded re-poll — reservations clearing, racing ACS
    ///    refreshes. Genuine insufficiency keeps deadline == now ⇒ fail fast.
    ///
    /// The client opened for the backfill stays in `client_slot` for reuse
    /// (fee leg, submit loop). All of this runs BEFORE `reserve_v2`.
    async fn select_leg_with_backfill(
        &self,
        client_slot: &mut Option<AtomicProviderClient>,
        key: &crate::holdings_cache::InstrumentKey,
        target: Decimal,
        max_inputs: usize,
        is_cc: bool,
        valid_until_micros: i64,
    ) -> Option<Vec<CachedHolding>> {
        if let Some(picks) = self
            .cache
            .select_for_disclosure_with(key, target, max_inputs, is_cc, self.spend_reserve)
            .await
        {
            return Some(picks);
        }

        let now = clock::now_micros_i64();
        if self.cache.total_available_amount(key).await >= target
            && now.saturating_add(PRECHECK_VALIDITY_MARGIN_MICROS) < valid_until_micros
        {
            let pending = self.cache.blob_pending_cids(key, self.spend_reserve).await;
            if !pending.is_empty() {
                // Time-boxed: the channel timeouts (30 s connect / 120 s
                // request) must not hold the round — on expiry the passive
                // wait below takes over (window-anchored deadline).
                let window_micros = valid_until_micros
                    .saturating_sub(PRECHECK_VALIDITY_MARGIN_MICROS)
                    .saturating_sub(now);
                let budget = Duration::from_micros(u64::try_from(window_micros).unwrap_or(0))
                    .min(BACKFILL_BUDGET);
                info!(
                    "Cold cache for {}: backfilling blobs for {} pending holding(s)",
                    key,
                    pending.len()
                );
                let backfill = async {
                    if client_slot.is_none() {
                        match create_atomic_client(&self.config).await {
                            Ok(c) => *client_slot = Some(c),
                            Err(e) => warn!("Cold-cache backfill client unavailable: {e:#}"),
                        }
                    }
                    if let Some(client) = client_slot.as_mut() {
                        self.fetch_and_adopt(client, &pending).await;
                    }
                };
                if tokio::time::timeout(budget, backfill).await.is_err() {
                    warn!(
                        "Cold-cache backfill for {} timed out after {budget:?} — falling back to the passive wait",
                        key
                    );
                    // A client that just stalled is not reused for the submit
                    *client_slot = None;
                }
                if let Some(picks) = self
                    .cache
                    .select_for_disclosure_with(key, target, max_inputs, is_cc, self.spend_reserve)
                    .await
                {
                    return Some(picks);
                }
            }
        }

        self.cache
            .select_for_disclosure_until(
                key,
                target,
                max_inputs,
                is_cc,
                self.spend_reserve,
                selection_deadline(&self.cache, key, target, valid_until_micros).await,
                crate::rfq_v2::CONFIRM_SPLIT_POLL,
            )
            .await
    }

    /// Poll the ledger for the user's own input cids: consumed ⇒ the settle
    /// landed; live ⇒ it did not; unknown ⇒ every poll failed (conservative).
    async fn poll_own_inputs(&self, own_cids: &[String]) -> ReconcileStatus {
        let connect_wait = ledger_client::connect_wait(
            self.config.connection_timeout_secs,
            self.config.request_timeout_secs,
        );
        let mut client = match within("reconciliation connect", connect_wait, create_v1_client(&self.config)).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Reconciliation: cannot create ledger client: {e:#}");
                return ReconcileStatus::Unknown;
            }
        };
        let templates = [TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()];
        for attempt in 1..=RECONCILE_ATTEMPTS {
            let is_last = attempt == RECONCILE_ATTEMPTS;
            let poll = client.get_active_contracts(&templates);
            match within("reconciliation poll", RECONCILE_CALL_BUDGET, poll).await {
                Ok(contracts) => {
                    let live: HashSet<&str> =
                        contracts.iter().map(|c| c.contract_id.as_str()).collect();
                    if own_cids.iter().any(|c| !live.contains(c.as_str())) {
                        return ReconcileStatus::Consumed;
                    }
                    if is_last {
                        return ReconcileStatus::Live;
                    }
                }
                Err(e) => {
                    warn!(
                        "Reconciliation poll {}/{} failed: {e:#}",
                        attempt, RECONCILE_ATTEMPTS
                    );
                    if is_last {
                        return ReconcileStatus::Unknown;
                    }
                }
            }
            tokio::time::sleep(RECONCILE_POLL).await;
        }
        ReconcileStatus::Unknown
    }

    /// Adopt holdings created by our own settle into the cache. The execute
    /// response carries only `[{contract_id, template_id}]`, so amounts are
    /// backfilled via a targeted `GetAtomicContracts` fetch. Best-effort — the
    /// ACS worker reconciles within its 30 s cycle regardless.
    async fn adopt_created(&self, client: &mut AtomicProviderClient, created_json: &str) {
        let entries: Vec<Value> = match serde_json::from_str(created_json) {
            Ok(v) => v,
            Err(_) => return,
        };
        let cids: Vec<String> = entries
            .iter()
            .filter_map(|e| {
                let template = e.get("template_id")?.as_str()?;
                let holdingish = (template.contains("Amulet")
                    && !template.contains("Rules")
                    && !template.contains("Locked"))
                    || template.contains("Utility.Registry.Holding.V0.Holding:Holding");
                if !holdingish {
                    return None;
                }
                e.get("contract_id")?.as_str().map(str::to_string)
            })
            .collect();
        self.fetch_and_adopt(client, &cids).await;
    }

    /// Targeted `GetAtomicContracts` blob backfill: fetch `cids`, parse
    /// Amulet / Utility Holding payloads, and `add_created` them into the
    /// cache (blob-ready, fresh `discovered_at`). Best-effort. Shared by
    /// [`Self::adopt_created`] (own settle outputs) and the cold-cache
    /// selection backfill in [`Self::select_leg_with_backfill`].
    async fn fetch_and_adopt(&self, client: &mut AtomicProviderClient, cids: &[String]) {
        if cids.is_empty() {
            return;
        }

        let resp = match client.get_atomic_contracts(&[], cids).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Created-holdings backfill fetch failed (ACS refresh will catch up): {e:#}");
                return;
            }
        };
        let sync_by_cid: std::collections::HashMap<&str, &str> = resp
            .disclosures
            .iter()
            .map(|d| (d.contract_id.as_str(), d.synchronizer_id.as_str()))
            .collect();

        let now = Instant::now();
        let mut adopted: Vec<CachedHolding> = Vec::new();
        for c in &resp.contracts {
            let Ok(payload) = serde_json::from_str::<Value>(&c.payload_json) else {
                continue;
            };
            let synchronizer_id = sync_by_cid
                .get(c.contract_id.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| self.config.synchronizer_id.clone());
            let blob = (!c.created_event_blob.is_empty()).then(|| c.created_event_blob.clone());

            if c.template_id.contains("Splice.Amulet:Amulet") && !c.template_id.contains("Locked") {
                let Some(amount) = payload
                    .pointer("/amount/initialAmount")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Decimal>().ok())
                else {
                    continue;
                };
                adopted.push(CachedHolding {
                    contract_id: c.contract_id.clone(),
                    template_id: c.template_id.clone(),
                    instrument: CC_INSTRUMENT.to_string(),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id,
                    discovered_at: now,
                });
            } else if c
                .template_id
                .contains("Utility.Registry.Holding.V0.Holding:Holding")
            {
                let owner_ok =
                    payload.get("owner").and_then(|o| o.as_str()) == Some(self.config.party_id.as_str());
                let lock = payload.pointer("/lock");
                let unlocked = lock.is_none() || lock.is_some_and(|l| l.is_null());
                if !owner_ok || !unlocked {
                    continue;
                }
                let (Some(amount), Some(admin), Some(id)) = (
                    payload.get("amount").and_then(|v| v.as_str()).and_then(|s| s.parse::<Decimal>().ok()),
                    payload.pointer("/instrument/source").and_then(|v| v.as_str()),
                    payload.pointer("/instrument/id").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                adopted.push(CachedHolding {
                    contract_id: c.contract_id.clone(),
                    template_id: c.template_id.clone(),
                    instrument: instrument_key(admin, id),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id,
                    discovered_at: now,
                });
            }
        }
        if !adopted.is_empty() {
            info!("Adopted {} settle-created holdings into the cache", adopted.len());
            self.cache.add_created(adopted).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_prepare_rejections_are_classified() {
        // Registry pre-check rejections, as surfaced through the prepare RPC.
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): transfer-factory \
             https://x/registry/transfer-instruction/v1/transfer-factory -> HTTP 400 Bad Request: \
             {\"error\":\"Given input holdings have different instrument IDs\"}"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): transfer-factory \
             https://x/registry/transfer-instruction/v1/transfer-factory -> HTTP 400 Bad Request: \
             {\"error\":\"One or more input holdings were not found or may already be archived: 00aa\"}"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): transfer-factory \
             https://x/registry/transfer-instruction/v1/transfer-factory -> HTTP 400 Bad Request: \
             {\"error\":\"Given holdings are invalid\"}"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): transfer-factory \
             https://x/registry/transfer-instruction/v1/transfer-factory -> HTTP 400 Bad Request: \
             {\"error\":\"No holdings provided\"}"
        ));
        // Builder pre-check bails.
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): settlement fee in TOK \
             cannot be pre-checked: no listed TOK input"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): registry rejected the TOK \
             inputs of p::1 and no representative holding could provide a factory context"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): no usable TOK holding \
             could represent the settlement fee (5 LP inputs probed)"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): fee receiver fees has no \
             utility TransferPreapproval for TOK (transferKind=offer) — the fee cannot \
             complete one-step (H18)"
        ));
        assert!(is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Internal error): LP input 00aa resolved \
             transferKind=self, not direct"
        ));

        // Ledger-side and transport failures keep their existing handling.
        assert!(!is_deterministic_prepare_rejection("INACTIVE_CONTRACTS"));
        assert!(!is_deterministic_prepare_rejection("QUOTE_WINDOW_CLOSED"));
        assert!(!is_deterministic_prepare_rejection("connection reset by peer"));
        assert!(!is_deterministic_prepare_rejection(
            "PrepareAtomicTransaction RPC failed (Unavailable): transport error"
        ));
    }
}

#[cfg(test)]
mod settle_tests {
    use super::*;
    use crate::test_util::{refused_url, Fake, FakeLedger};
    use orderbook_proto::rfqv2::{
        AtomicAcsContract, AtomicDisclosedContract, AtomicFeeSpec, AtomicQuote, AtomicQuoteInfo,
    };

    const USER: &str = "test-party";
    const SYNC: &str = "sync::1";
    const PAY_CID: &str = "00usdc-holding";
    const USDC_ADMIN: &str = "reg::1220dd";

    fn disclosed(cid: &str) -> AtomicDisclosedContract {
        AtomicDisclosedContract { contract_id: cid.to_string(), ..Default::default() }
    }

    /// A signed Buy envelope (user pays 25 USDC for 5 CC) and the indicative quote it confirms.
    fn envelope(valid_until_micros: i64, lp_fees: Vec<AtomicFeeSpec>) -> (AtomicQuoteEnvelope, AtomicQuoteInfo) {
        let kf = atomic_quote::gen_keypair().unwrap();
        let payload = serde_json::json!({
            "lp": "lp::1220aa",
            "provider": "prov::1220bb",
            "pairName": "CC-USDC",
            "baseInstrumentId": {"admin": "dso::1220cc", "id": "Amulet"},
            "quoteInstrumentId": {"admin": USDC_ADMIN, "id": "USDC"},
            "quotePublicKey": kf.pub_spki_hex,
        });
        let mut env = AtomicQuoteEnvelope {
            version: atomic_quote::envelope::ENVELOPE_VERSION.to_string(),
            synchronizer_id: SYNC.to_string(),
            dvp: Some(AtomicAcsContract {
                contract_id: "00venue".to_string(),
                template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".to_string(),
                created_event_blob: "blob".to_string(),
                payload_json: payload.to_string(),
            }),
            quote: Some(AtomicQuote {
                quote_id: "0198-quote".to_string(),
                ticket_id: String::new(),
                user: USER.to_string(),
                side: "Buy".to_string(),
                base_amount: "5.0".to_string(),
                quote_amount: "25.0".to_string(),
                created_at_micros: 1_000_000,
                valid_until_micros,
                lp_fees: lp_fees.clone(),
            }),
            lp_input_holding_cids: vec!["00lp1".to_string()],
            disclosed: vec![disclosed("00venue"), disclosed("00lp1")],
            ..Default::default()
        };
        let plain = envelope_from_proto(&env).unwrap();
        env.canonical_message =
            atomic_quote::envelope::canonical_from_dvp(&plain.dvp.payload, &plain.quote).unwrap();
        env.quote_signature = atomic_quote::sign_quote(&kf.priv_scalar_hex, &env.canonical_message).unwrap();
        let accepted = AtomicQuoteInfo {
            quote_id: "0198-quote".to_string(),
            lp_party_id: "lp::1220aa".to_string(),
            quantity: "5.0".to_string(),
            quote_quantity: "25.0".to_string(),
            settlement_fee: lp_fees.first().cloned(),
            ..Default::default()
        };
        (env, accepted)
    }

    fn fee(admin: &str, id: &str, amount: &str) -> AtomicFeeSpec {
        AtomicFeeSpec {
            receiver: "fees::1220ff".to_string(),
            instrument_admin: admin.to_string(),
            instrument_id: id.to_string(),
            amount: amount.to_string(),
        }
    }

    fn swapper(url: &str) -> AtomicSwapper {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = url.to_string();
        config.synchronizer_id = SYNC.to_string();
        config.connection_timeout_secs = 1;
        config.request_timeout_secs = 1;
        AtomicSwapper {
            config,
            cache: HoldingsCache::new(false),
            spend_reserve: false,
            verbose: false,
            dry_run: false,
            force: false,
            confirm: false,
            confirm_lock: agent_logic::confirm::new_confirm_lock(),
        }
    }

    async fn funded(url: &str) -> AtomicSwapper {
        let s = swapper(url);
        s.cache
            .add_created(vec![CachedHolding {
                contract_id: PAY_CID.to_string(),
                template_id: TEMPLATE_HOLDING.to_string(),
                instrument: instrument_key(USDC_ADMIN, "USDC"),
                amount: Decimal::from(1000),
                created_event_blob: Some("blob".to_string()),
                synchronizer_id: SYNC.to_string(),
                discovered_at: Instant::now(),
            }])
            .await;
        s
    }

    async fn pay_state(s: &AtomicSwapper) -> &'static str {
        match s.cache.stats(&instrument_key(USDC_ADMIN, "USDC")).await {
            (_, 0, 0, _) => "free",
            (_, 0, 1, _) => "reserved",
            (_, 1, 0, _) => "consumed",
            other => panic!("unexpected cache state {other:?}"),
        }
    }

    fn in_secs(secs: i64) -> i64 {
        clock::now_micros_i64() + secs * 1_000_000
    }

    fn requote_reason(outcome: SwapOutcome) -> String {
        match outcome {
            SwapOutcome::Requote { reason } => reason,
            SwapOutcome::Filled(f) => panic!("expected a re-quote, got a fill {}", f.update_id),
            SwapOutcome::Abort { reason } => panic!("expected a re-quote, got an abort: {reason}"),
            SwapOutcome::DryRun => panic!("expected a re-quote, got a dry run"),
        }
    }

    // A far-future validity used to be reserved for the whole window (and overflowed the Instant at the extreme)
    #[tokio::test]
    async fn a_far_future_validity_is_refused_before_reserving() {
        for valid_until in [in_secs(2 * 3600), i64::MAX] {
            let s = funded(&refused_url()).await;
            let (env, accepted) = envelope(valid_until, vec![]);
            let outcome = s.settle_envelope(&env, &accepted, FillDirection::Buy, 100).await.unwrap();
            let reason = requote_reason(outcome);
            assert!(reason.contains("ahead"), "{reason}");
            assert_eq!(pay_state(&s).await, "free");
        }
    }

    // Fee totals near the Decimal limit used to panic in the funding arithmetic
    #[tokio::test]
    async fn out_of_range_fee_targets_requote_before_reserving() {
        let huge = Decimal::MAX.to_string();
        for lp_fee in [fee("dso::1220cc", "Amulet", &huge), fee(USDC_ADMIN, "USDC", &huge)] {
            let s = funded(&refused_url()).await;
            let (env, accepted) = envelope(in_secs(60), vec![lp_fee]);
            let outcome = s.settle_envelope(&env, &accepted, FillDirection::Buy, 100).await.unwrap();
            let reason = requote_reason(outcome);
            assert!(reason.contains("out of range"), "{reason}");
            assert_eq!(pay_state(&s).await, "free");
        }
    }

    // A submit with no result used to be awaited without limit; past its wait it must reconcile, never re-quote
    #[tokio::test]
    async fn a_submit_without_a_result_is_reconciled_not_requoted() {
        let ledger = FakeLedger::start(Fake::Answer).await;
        let s = funded(&ledger.url).await;
        let (env, accepted) = envelope(in_secs(60), vec![]);
        let hung = |client: AtomicProviderClient, _: PrepareAtomicTransactionRequest, _: OperationExpectation, _: SubmitFlags| async move {
            std::future::pending::<()>().await;
            (client, Err(anyhow!("never")))
        };
        let rounds = (hung, |_: i64, _: i64| Duration::from_millis(100));
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            s.settle_envelope_with(&env, &accepted, FillDirection::Buy, 100, rounds, || true),
        )
        .await
        .expect("the submit wait bounds the round")
        .unwrap();
        // The fake ACS lists none of the own inputs, so they read as consumed
        let SwapOutcome::Filled(fill) = outcome else { panic!("expected the reconciled fill") };
        assert!(fill.update_id.contains("reconciled"), "{}", fill.update_id);
        assert_eq!(pay_state(&s).await, "consumed");
        ledger.stop().await;
    }

    // The caller's marker is saved once, just before the submit; a refusal before it saves nothing
    #[tokio::test]
    async fn the_marker_hook_runs_once_just_before_the_submit() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let marks = Arc::new(AtomicUsize::new(0));
        let s = funded(&refused_url()).await;
        let (env, accepted) = envelope(in_secs(2 * 3600), vec![]);
        let m = Arc::clone(&marks);
        let refused = s.settle_envelope_marked(&env, &accepted, FillDirection::Buy, 100, move || {
            m.fetch_add(1, SeqCst);
            true
        });
        assert!(requote_reason(refused.await.unwrap()).contains("ahead"));
        assert_eq!(marks.load(SeqCst), 0, "nothing was submitted");

        let ledger = FakeLedger::start(Fake::Answer).await;
        let s = funded(&ledger.url).await;
        let (env, accepted) = envelope(in_secs(60), vec![]);
        let at_submit = Arc::new(AtomicUsize::new(usize::MAX));
        let (m, seen) = (Arc::clone(&marks), Arc::clone(&at_submit));
        let rejected = move |client: AtomicProviderClient, _: PrepareAtomicTransactionRequest, _: OperationExpectation, _: SubmitFlags| {
            seen.store(m.load(SeqCst), SeqCst);
            async move { (client, Err(anyhow!("Transaction failed: rejected"))) }
        };
        let rounds = (rejected, |_: i64, _: i64| Duration::from_secs(5));
        let m = Arc::clone(&marks);
        let outcome = s.settle_envelope_with(&env, &accepted, FillDirection::Buy, 100, rounds, move || {
            m.fetch_add(1, SeqCst);
            true
        });
        let outcome = tokio::time::timeout(Duration::from_secs(20), outcome).await.expect("bounded").unwrap();
        assert!(requote_reason(outcome).contains("rejected"));
        assert_eq!(at_submit.load(SeqCst), 1, "marked before the submit started");
        assert_eq!(marks.load(SeqCst), 1);
        ledger.stop().await;
    }

    // An unsaved marker used to be logged only, and the submit ran without it
    #[tokio::test]
    async fn an_unsaved_marker_stops_the_round_before_the_submit() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let ledger = FakeLedger::start(Fake::Answer).await;
        let s = funded(&ledger.url).await;
        let (env, accepted) = envelope(in_secs(60), vec![]);
        let submits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&submits);
        let submit = move |client: AtomicProviderClient, _: PrepareAtomicTransactionRequest, _: OperationExpectation, _: SubmitFlags| {
            seen.fetch_add(1, SeqCst);
            async move { (client, Err(anyhow!("Transaction failed: rejected"))) }
        };
        let rounds = (submit, |_: i64, _: i64| Duration::from_secs(5));
        let outcome = s.settle_envelope_with(&env, &accepted, FillDirection::Buy, 100, rounds, || false);
        let ended = tokio::time::timeout(Duration::from_secs(20), outcome).await.expect("bounded");
        let err = ended.err().expect("the round stops with an error");
        assert_eq!(err.downcast_ref::<MarkerNotSaved>(), Some(&MarkerNotSaved("quote 0198-quote".to_string())));
        assert_eq!(submits.load(SeqCst), 0, "nothing was submitted");
        assert_eq!(pay_state(&s).await, "free", "its inputs are released");
        ledger.stop().await;
    }

    async fn reserved(url: &str) -> AtomicSwapper {
        let s = funded(url).await;
        let until = Instant::now() + Duration::from_secs(60);
        assert!(s.cache.reserve_v2(&[PAY_CID.to_string()], "q1", until).await);
        s
    }

    fn lost<T>(running: Option<tokio::task::JoinHandle<T>>) -> LostSubmit<T> {
        LostSubmit { running, why: "gave no result within 100ms".to_string() }
    }

    async fn finished<T: Send + 'static>(value: T) -> tokio::task::JoinHandle<T> {
        let task = tokio::spawn(async move { value });
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        task
    }

    async fn after(
        s: &AtomicSwapper,
        lost: LostSubmit<Result<AtomicExecuteResponse>>,
        status: ReconcileStatus,
    ) -> SwapOutcome {
        s.after_lost_submit(&[PAY_CID.to_string()], "q1", 5.0, lost, |r| r, async move { status })
            .await
    }

    #[tokio::test]
    async fn a_still_running_submit_is_never_requoted() {
        let s = reserved(&refused_url()).await;
        for status in [ReconcileStatus::Live, ReconcileStatus::Unknown] {
            let task = tokio::spawn(std::future::pending::<Result<AtomicExecuteResponse>>());
            let SwapOutcome::Abort { reason } = after(&s, lost(Some(task)), status).await else {
                panic!("a running submit with live inputs must abort");
            };
            assert!(reason.contains("still running"), "{reason}");
            assert_eq!(pay_state(&s).await, "reserved");
        }
        let task = tokio::spawn(std::future::pending::<Result<AtomicExecuteResponse>>());
        let SwapOutcome::Filled(fill) = after(&s, lost(Some(task)), ReconcileStatus::Consumed).await else {
            panic!("consumed inputs prove the commit");
        };
        assert!(fill.update_id.contains("reconciled"));
        assert_eq!(pay_state(&s).await, "consumed");
    }

    #[tokio::test]
    async fn a_lost_submit_follows_the_reconciliation() {
        let s = reserved(&refused_url()).await;
        let SwapOutcome::Abort { .. } = after(&s, lost(None), ReconcileStatus::Unknown).await else {
            panic!("unknown status must abort");
        };
        assert_eq!(pay_state(&s).await, "reserved");
        let reason = requote_reason(after(&s, lost(None), ReconcileStatus::Live).await);
        assert!(reason.contains("verified live"), "{reason}");
        assert_eq!(pay_state(&s).await, "free");
    }

    #[tokio::test]
    async fn a_late_result_decides_once_the_inputs_are_live() {
        let s = reserved(&refused_url()).await;
        let failed = finished(Err(anyhow!("Transaction failed: rejected"))).await;
        let reason = requote_reason(after(&s, lost(Some(failed)), ReconcileStatus::Live).await);
        assert!(reason.contains("rejected"), "{reason}");
        assert_eq!(pay_state(&s).await, "free");

        let s = reserved(&refused_url()).await;
        let ambiguous = finished(Err(anyhow!("{}: x", ledger_client::ATOMIC_EXECUTE_AMBIGUOUS))).await;
        let SwapOutcome::Abort { .. } = after(&s, lost(Some(ambiguous)), ReconcileStatus::Live).await else {
            panic!("an ambiguous late result must abort");
        };
        assert_eq!(pay_state(&s).await, "reserved");

        let s = reserved(&refused_url()).await;
        let done = AtomicExecuteResponse { success: true, update_id: "u9".to_string(), ..Default::default() };
        let committed = finished(Ok(done)).await;
        let SwapOutcome::Filled(fill) = after(&s, lost(Some(committed)), ReconcileStatus::Live).await else {
            panic!("a late success is a fill");
        };
        assert_eq!(fill.update_id, "u9");
        assert_eq!(pay_state(&s).await, "consumed");
    }

    #[tokio::test]
    async fn watch_task_reports_done_slow_and_failed() {
        let SubmitWatch::Done(7) = watch_task(async { 7 }, Duration::from_secs(5)).await else {
            panic!("a quick task is done");
        };
        let slow = tokio::time::timeout(
            Duration::from_secs(5),
            watch_task(std::future::pending::<u8>(), Duration::from_millis(20)),
        )
        .await
        .expect("the wait is bounded");
        let SubmitWatch::Lost(slow) = slow else {
            panic!("a hung task is lost");
        };
        assert!(slow.running.is_some_and(|t| !t.is_finished()), "the slow task keeps running");
        let SubmitWatch::Lost(failed) = watch_task(async { panic!("boom") }, Duration::from_secs(5)).await else {
            panic!("a panicked task is lost");
        };
        let _: &LostSubmit<u8> = &failed;
        assert!(failed.running.is_none() && failed.why.contains("abnormally"), "{}", failed.why);
    }

    #[tokio::test]
    async fn window_arithmetic_never_panics() {
        assert!(validity_too_far(in_secs(3600), clock::now_micros_i64()).is_none());
        assert!(validity_too_far(in_secs(3600 + 600), clock::now_micros_i64()).is_some());
        assert!(validity_too_far(i64::MAX, i64::MIN).is_some());
        assert!(validity_too_far(0, 5).is_none(), "an expired window is the pre-check's call");
        assert!(validity_too_far(i64::MIN, i64::MAX).is_some());
        let now = Instant::now();
        let _ = reservation_expiry(now, i64::MAX, i64::MIN);
        assert_eq!(reservation_expiry(now, 5_000_000, 0), Some(now + Duration::from_secs(5) + RESERVATION_GRACE));
        assert_eq!(reservation_expiry(now, 0, 5_000_000), Some(now + RESERVATION_GRACE));
        assert_eq!(submit_wait(5_000_000, 0), Duration::from_secs(5 + 10) + RESERVATION_GRACE);
        assert_eq!(submit_wait(i64::MIN, i64::MAX), Duration::from_secs(10) + RESERVATION_GRACE);
        assert!(submit_wait(i64::MAX, i64::MIN) > Duration::from_secs(1 << 40));
        let s = funded(&refused_url()).await;
        let key = instrument_key(USDC_ADMIN, "USDC");
        let soon = selection_deadline(&s.cache, &key, Decimal::ONE, i64::MIN).await;
        assert!(soon <= Instant::now());
        let capped = selection_deadline(&s.cache, &key, Decimal::ONE, i64::MAX).await;
        assert!(capped <= Instant::now() + crate::rfq_v2::MAX_CONFIRM_SPLIT_WAIT);
    }

    #[test]
    fn only_a_full_preapproval_for_the_admin_counts() {
        let full = PreapprovalInfo { instrument_admin: "a".to_string(), ..Default::default() };
        assert!(preapproval_present(&[full.clone()], "a"));
        assert!(!preapproval_present(&[full], "b"));
        let partial = PreapprovalInfo {
            instrument_admin: "a".to_string(),
            instrument_allowances: vec![Default::default()],
            ..Default::default()
        };
        assert!(!preapproval_present(&[partial], "a"));
    }
}
