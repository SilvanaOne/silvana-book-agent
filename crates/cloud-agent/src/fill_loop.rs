//! Fill loop for the `buy` / `sell` (taker) CLI commands.
//!
//! For each round:
//!   1. Request an RFQ quote from LPs
//!   2. Accept the best quote (LP creates the DvpProposal on-chain)
//!   3. Poll for the DvpProposal contract id + settlement fees
//!   4. Submit a single `Execute_MultiCall` that does Accept_Dvp + Allocate +
//!      all fees + own traffic fee (via `MulticallSettler`)
//!   5. On success: decrement `remaining`, loop until filled
//!
//! Unlike the LP's full `agent` command, the taker path does NOT run the
//! settlement state-machine (`run_agent`) in the background — the multicall
//! settles the proposal atomically, so nothing is left to drive step-by-step.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rust_decimal::prelude::FromStr as _;
use rust_decimal::Decimal;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::confirm::Unanswered;
use agent_logic::rpc_client::OrderbookRpcClient;
use agent_logic::state::{SavedFillState, UnresolvedKind, UnresolvedRound};
use agent_logic::clock;
use orderbook_proto::settlement::{DvpStepStatusEnum, GetSettlementStatusResponse, SettlementStage};
use orderbook_proto::SettlementProposalMessage;

use crate::accept_settle::{MulticallSettler, SettleOutcomeUnknown};
use crate::ledger_client::{within, DAppProviderClient};

/// Bound on connecting the orderbook-rpc client.
const RPC_CONNECT_BUDGET: Duration = Duration::from_secs(15);
/// Bound on looking up a proposal's settlement fees; nothing is submitted yet.
const FEE_FETCH_BUDGET: Duration = Duration::from_secs(30);
/// Bound on one settlement-status read.
const STATUS_CALL_BUDGET: Duration = Duration::from_secs(15);
/// Status reads, and the pause between them, when a settle outcome is unknown.
const SETTLE_RECONCILE_POLLS: u32 = 6;
const SETTLE_RECONCILE_EVERY: Duration = Duration::from_secs(10);
/// One deadline for all progress monitors at exit.
const MONITOR_WAIT: Duration = Duration::from_secs(300);
/// A converted fee above this many CC means a bad rate, not a real fee.
const MAX_FEE_CC: u64 = 1_000_000_000_000;

/// A round's restart marker could not be saved, so its settle never started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkerNotSaved(pub(crate) String);

impl std::fmt::Display for MarkerNotSaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the restart marker of {} could not be saved to the state file; nothing was submitted", self.0)
    }
}

impl std::error::Error for MarkerNotSaved {}

/// Direction of the fill operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillDirection {
    Buy,
    Sell,
}

/// Parameters for a fill loop
pub struct FillParams {
    pub direction: FillDirection,
    pub market_id: String,
    pub total_amount: f64,
    pub price_limit: Option<f64>,
    pub min_settlement: f64,
    pub max_settlement: f64,
    pub interval_secs: u64,
    /// RFQ V2 (AtomicDVP): settle each round in ONE atomic transaction via
    /// `RfqV2Service` + `AtomicDvpProviderService` instead of the v1
    /// accept/proposal/multicall ladder. Default false (v1 path untouched).
    pub atomic: bool,
    /// RFQ V2 settlement-fee token preference, priority order (instruments
    /// symbols, e.g. ["USDC", "CC"]). Empty = CC. Atomic path only.
    pub fee_tokens: Vec<String>,
}

/// Amounts and the price limit must be finite and above 0.
fn validate_params(params: &FillParams) -> Result<()> {
    let values = [
        ("total_amount", Some(params.total_amount)),
        ("min_settlement", Some(params.min_settlement)),
        ("max_settlement", Some(params.max_settlement)),
        ("price_limit", params.price_limit),
    ];
    for (name, value) in values {
        if let Some(v) = value.filter(|v| !(v.is_finite() && *v > 0.0)) {
            anyhow::bail!("{name} must be a finite number above 0, got {v}");
        }
    }
    Ok(())
}

/// First Ctrl-C sets the shutdown flag, a second calls `force_exit`. A failed
/// signal listener is not a shutdown.
async fn watch_ctrl_c<S, Fut>(mut next_signal: S, flag: Arc<AtomicBool>, notify: Arc<Notify>, force_exit: impl FnOnce())
where
    S: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    if let Err(e) = next_signal().await {
        warn!("Ctrl-C listener unavailable ({e}); the fill loop runs until it completes");
        return;
    }
    flag.store(true, Ordering::Relaxed);
    notify.notify_waiters();
    match next_signal().await {
        Ok(()) => {
            warn!("Second Ctrl-C received, forcing immediate exit; unfinished fee debits stay unpaid");
            force_exit();
        }
        Err(e) => warn!("Ctrl-C listener unavailable ({e}); a second Ctrl-C cannot force an exit"),
    }
}

/// Aborts the task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Sleep that can be interrupted by shutdown signal. Returns true if shutdown was requested.
async fn interruptible_sleep(duration: Duration, shutdown: &Notify, flag: &AtomicBool) -> bool {
    if flag.load(Ordering::Relaxed) {
        return true;
    }
    tokio::select! {
        _ = tokio::time::sleep(duration) => flag.load(Ordering::Relaxed),
        _ = shutdown.notified() => true,
    }
}

fn direction_name(direction: FillDirection) -> &'static str {
    match direction {
        FillDirection::Buy => "buy",
        FillDirection::Sell => "sell",
    }
}

/// A fresh orderbook-rpc token; the loop can outlive any single one.
fn rpc_token(config: &BaseConfig) -> Result<String> {
    agent_logic::auth::generate_jwt(
        &config.party_id,
        &config.role,
        &*config.private_key.expose()?,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
    )
}

/// Where the fill state is saved between runs.
#[derive(Clone, Copy)]
struct FillStateSink<'a> {
    path: Option<&'a Path>,
    party_id: &'a str,
}

impl FillStateSink<'_> {
    /// Save `state`; false, after a warning, when it could not be written.
    #[must_use]
    fn save(&self, state: SavedFillState) -> bool {
        let Some(path) = self.path else { return true };
        match save_fill_state_only(path, self.party_id, state) {
            Ok(()) => true,
            Err(e) => {
                warn!("Could not save the fill state to {}: {:#}", path.display(), e);
                false
            }
        }
    }
}

/// The saved progress of `params`' order.
fn fill_snapshot(
    params: &FillParams,
    filled_total: f64,
    remaining: f64,
    round: u32,
    unresolved: Option<UnresolvedRound>,
) -> SavedFillState {
    SavedFillState {
        direction: direction_name(params.direction).to_string(),
        market_id: params.market_id.clone(),
        total_amount: params.total_amount,
        filled_total,
        remaining: remaining.max(0.0),
        round,
        unresolved,
    }
}

/// A round's restart marker in the state file.
struct RoundMark<'a> {
    sink: FillStateSink<'a>,
    marked: SavedFillState,
    unmarked: SavedFillState,
    is_set: AtomicBool,
    label: String,
}

impl<'a> RoundMark<'a> {
    /// The marker of `round` in `params`' order; `progress` is the fill before it.
    fn new(
        sink: FillStateSink<'a>,
        params: &FillParams,
        progress: (f64, f64),
        round: u32,
        unresolved: UnresolvedRound,
    ) -> Self {
        let (filled_total, remaining) = progress;
        let label = format!("round {round} ({})", unresolved.id);
        Self {
            sink,
            marked: fill_snapshot(params, filled_total, remaining, round, Some(unresolved)),
            unmarked: fill_snapshot(params, filled_total, remaining, round, None),
            is_set: AtomicBool::new(false),
            label,
        }
    }

    /// Saved before anything can commit, so a restart never quotes the round again;
    /// false when the save failed, and then nothing may be submitted.
    fn set(&self) -> bool {
        let saved = self.sink.save(self.marked.clone());
        if saved {
            self.is_set.store(true, Ordering::Relaxed);
        }
        saved
    }

    /// Saved once the round is known not to have committed.
    fn clear(&self) {
        if self.is_set.swap(false, Ordering::Relaxed) {
            // A marker left in the file only makes a restart check the round
            let _ = self.sink.save(self.unmarked.clone());
        }
    }

    fn not_saved(&self) -> MarkerNotSaved {
        MarkerNotSaved(self.label.clone())
    }
}

/// Await `settle`, which saves the round's marker itself before it can commit;
/// an outcome that shows nothing committed clears it.
async fn cleared_unless_committed<T>(
    mark: &RoundMark<'_>,
    settle: impl Future<Output = T>,
    nothing_committed: impl FnOnce(&T) -> bool,
) -> T {
    let outcome = settle.await;
    if nothing_committed(&outcome) {
        mark.clear();
    }
    outcome
}

/// What a failed V1 settle means for the loop.
enum V1Failure<'a> {
    /// Nothing was submitted, but the loop must stop.
    Stop(String),
    /// The multicall may have committed; reconcile before anything else.
    Unknown(&'a SettleOutcomeUnknown),
    /// It failed without a commit; a later round may quote again.
    Retry,
}

/// Why a round that submitted nothing still stops the loop: its marker was not
/// saved, or its prompt went unanswered and a retry would accept quote after quote.
fn stop_reason(e: &anyhow::Error) -> Option<String> {
    if let Some(unsaved) = e.downcast_ref::<MarkerNotSaved>() {
        return Some(unsaved.to_string());
    }
    e.downcast_ref::<Unanswered>().map(ToString::to_string)
}

fn v1_failure(e: &anyhow::Error) -> V1Failure<'_> {
    if let Some(reason) = stop_reason(e) {
        return V1Failure::Stop(reason);
    }
    match e.downcast_ref::<SettleOutcomeUnknown>() {
        Some(unknown) => V1Failure::Unknown(unknown),
        None => V1Failure::Retry,
    }
}

/// A multicall that failed with a known outcome committed nothing.
fn v1_nothing_committed<T>(settle: &Result<T>) -> bool {
    settle.as_ref().is_err_and(|e| e.downcast_ref::<SettleOutcomeUnknown>().is_none())
}

/// A re-quote, a dry run or an error committed nothing; a fill or an abort may have.
fn atomic_nothing_committed(settle: &Result<SwapOutcome>) -> bool {
    matches!(settle, Ok(SwapOutcome::Requote { .. } | SwapOutcome::DryRun) | Err(_))
}

/// One V1 round's settle; `accept` starts only once its restart marker is saved,
/// and an outcome that shows nothing committed clears the marker.
async fn v1_settle<T, Fut>(mark: &RoundMark<'_>, accept: impl FnOnce() -> Fut) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    if !mark.set() {
        return Err(mark.not_saved().into());
    }
    cleared_unless_committed(mark, accept(), v1_nothing_committed).await
}

/// Settles an accepted envelope; the atomic swapper in production.
trait AtomicSettle {
    async fn settle_with_hook(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
        before_submit: impl FnOnce() -> bool,
    ) -> Result<SwapOutcome>;
}

impl AtomicSettle for AtomicSwapper {
    async fn settle_with_hook(
        &self,
        envelope: &AtomicQuoteEnvelope,
        accepted: &AtomicQuoteInfo,
        direction: FillDirection,
        max_input_holdings: usize,
        before_submit: impl FnOnce() -> bool,
    ) -> Result<SwapOutcome> {
        self.settle_envelope_marked(envelope, accepted, direction, max_input_holdings, before_submit)
            .await
    }
}

/// One atomic round's settle; its restart marker is saved just before the
/// submit, which does not start unless it was, and cleared when nothing committed.
async fn atomic_settle<S: AtomicSettle>(
    swapper: &S,
    mark: &RoundMark<'_>,
    envelope: &AtomicQuoteEnvelope,
    accepted: &AtomicQuoteInfo,
    direction: FillDirection,
    max_input_holdings: usize,
) -> Result<SwapOutcome> {
    let settle = swapper.settle_with_hook(envelope, accepted, direction, max_input_holdings, || mark.set());
    cleared_unless_committed(mark, settle, atomic_nothing_committed).await
}

/// How a run starts, given the fill state of an earlier one.
#[derive(Debug, PartialEq)]
enum RestorePlan {
    /// Start the order from the beginning.
    Fresh,
    /// Continue the saved order.
    Resume { remaining: f64, filled: f64, round: u32 },
    /// Continue once the saved V1 round's settlement status is known.
    Resolve { proposal_id: String, qty: f64, remaining: f64, filled: f64, round: u32 },
    /// Do not start; the reason names the round.
    Refuse(String),
}

/// A round marked while its settle could commit is resolved or refused, never resumed.
fn restore_plan(saved: Option<&SavedFillState>, params: &FillParams) -> RestorePlan {
    let Some(fs) = saved else {
        return RestorePlan::Fresh;
    };
    let same_order = fs.direction == direction_name(params.direction)
        && fs.market_id == params.market_id
        && (fs.total_amount - params.total_amount).abs() < 0.000001
        && fs.remaining > 0.0;
    if !same_order {
        return RestorePlan::Fresh;
    }
    let (remaining, filled, round) = (fs.remaining, fs.filled_total, fs.round);
    match &fs.unresolved {
        None => RestorePlan::Resume { remaining, filled, round },
        Some(UnresolvedRound { kind: UnresolvedKind::V1, id, qty }) => {
            RestorePlan::Resolve { proposal_id: id.clone(), qty: *qty, remaining, filled, round }
        }
        Some(UnresolvedRound { kind: UnresolvedKind::Atomic, id, qty }) => RestorePlan::Refuse(format!(
            "the previous run stopped while atomic quote {id} ({qty} base) of round {round} had an unknown \
             outcome; check the ledger once the quote's validity window has passed. {}",
            unknown_round_help(UnresolvedKind::Atomic, filled, fs.total_amount, remaining, *qty)
        )),
    }
}

/// The order's progress and the operator's ways past a saved round of unknown outcome.
fn unknown_round_help(kind: UnresolvedKind, filled: f64, total: f64, remaining: f64, qty: f64) -> String {
    let rest = remaining - qty;
    let last = rest <= 0.000001;
    let if_committed = match kind {
        // A V1 round's processing fees are paid only by the run that proves its commit
        UnresolvedKind::V1 => {
            let run = if last {
                format!(
                    "only resolves the saved round: once the commit is proven it counts {qty}, pays the round's \
                     two processing fees, which the LP waits for before it allocates, and buys nothing more"
                )
            } else {
                format!("counts {qty} and pays the round's two processing fees, which the LP waits for before it allocates")
            };
            format!(
                "rerun this command unchanged once the settlement status can be read; that run {run}. Editing \
                 agent-state.json or changing --amount skips those fees, so that trade does not settle and \
                 should not be counted"
            )
        }
        UnresolvedKind::Atomic if last => format!(
            "the order is complete: set fill_state.unresolved to null, add {qty} to fill_state.filled_total \
             and set fill_state.remaining to 0 in agent-state.json, so the next run of this command starts a \
             new order"
        ),
        UnresolvedKind::Atomic => format!(
            "set fill_state.unresolved to null, add {qty} to fill_state.filled_total and subtract it from \
             fill_state.remaining, then rerun this command, or rerun with --amount {rest:.6}"
        ),
    };
    format!(
        "The order had filled {filled:.6} of {total:.6} with {remaining:.6} remaining. If the round did not \
         commit, set fill_state.unresolved to null in agent-state.json and rerun this command; if it \
         committed, {if_committed}."
    )
}

/// Why a saved order is not continued.
fn announce_new_order(fs: &SavedFillState, params: &FillParams) {
    if let Some(u) = &fs.unresolved {
        warn!(
            "The previous order left round {} ({} base) with an unknown outcome; starting a new order",
            u.id, u.qty
        );
        if u.kind == UnresolvedKind::V1 {
            warn!("If round {} committed, its two processing fees were not paid, so that trade does not settle", u.id);
        }
    }
    if fs.remaining <= 0.0 {
        info!("Previous fill completed, starting new order");
    } else {
        info!(
            "New order (was {} {:.6} {}, now {} {:.6} {}), starting fresh",
            fs.direction, fs.total_amount, fs.market_id,
            direction_name(params.direction), params.total_amount, params.market_id
        );
    }
}

/// The base quantity a saved V1 round adds to the fill: all of it with proof
/// of a commit (its fees are then debited), none once the settlement ended.
fn resolve_saved_round(
    settler: &MulticallSettler,
    proposal_id: &str,
    qty: f64,
    check: SettleCheck,
    (filled, total, remaining): (f64, f64, f64),
) -> Result<f64> {
    match check {
        SettleCheck::Committed(stage) => {
            info!("Saved round {proposal_id} committed (settlement stage {stage}); counting {qty} as filled");
            settler.debit_fees(proposal_id);
            Ok(qty)
        }
        SettleCheck::Ended(stage) => {
            info!("Saved round {proposal_id} ended at stage {stage} without a commit; not counted");
            Ok(0.0)
        }
        SettleCheck::NotEstablished(why) => anyhow::bail!(
            "the previous run stopped while proposal {proposal_id} ({qty} base) had an unknown outcome, \
             still unknown ({why}); rerunning this command reads the settlement status again. {}",
            unknown_round_help(UnresolvedKind::V1, filled, total, remaining, qty)
        ),
    }
}

/// The saved order's (remaining, filled, round) once its V1 round is resolved: the
/// status is read as for a settle of unknown outcome, and the result saved.
async fn resume_saved_v1_round<S: StatusSource>(
    source: &mut S,
    settler: &MulticallSettler,
    sink: &FillStateSink<'_>,
    params: &FillParams,
    proposal_id: &str,
    qty: f64,
    (remaining, filled, round): (f64, f64, u32),
) -> Result<(f64, f64, u32)> {
    let check = reconcile_settle(source, proposal_id, SETTLE_RECONCILE_POLLS, SETTLE_RECONCILE_EVERY).await;
    let counted = resolve_saved_round(settler, proposal_id, qty, check, (filled, params.total_amount, remaining))?;
    let (filled, remaining) = (filled + counted, remaining - counted);
    // A failed save keeps the marker, so a restart reads the round again
    let _ = sink.save(fill_snapshot(params, filled, remaining, round, None));
    info!(
        "Restoring fill state: filled={:.6} remaining={:.6} round={}",
        filled, remaining, round
    );
    Ok((remaining, filled, round))
}

/// The value of a step after an accepted quote; on failure, a warning and a
/// pause, so a lasting failure cannot accept quote after quote.
async fn post_accept_step<T>(
    step: std::result::Result<T, String>,
    round: u32,
    pause: Duration,
    shutdown: &Notify,
    flag: &AtomicBool,
) -> Option<T> {
    match step {
        Ok(value) => Some(value),
        Err(why) => {
            warn!("[round {}] {}", round, why);
            interruptible_sleep(pause, shutdown, flag).await;
            None
        }
    }
}

/// Run the fill loop. Settles each accepted quote atomically via a single
/// multicall (Accept+Allocate+fees+traffic) before moving on.
pub async fn run_fill_loop(
    config: BaseConfig,
    settler: Arc<MulticallSettler>,
    params: FillParams,
    atomic_swapper: Option<Arc<crate::atomic_swap::AtomicSwapper>>,
    saved_fill_state: Option<SavedFillState>,
    state_file: Option<PathBuf>,
) -> Result<()> {
    validate_params(&params)?;
    let plan = restore_plan(saved_fill_state.as_ref(), &params);
    if let RestorePlan::Refuse(why) = &plan {
        anyhow::bail!("{why}");
    }
    let rt = tokio::runtime::Handle::try_current().context("the fill loop needs a tokio runtime")?;

    let dir_str = direction_name(params.direction);
    let sink = FillStateSink { path: state_file.as_deref(), party_id: &config.party_id };
    // Written before any quote, keeping what an earlier run saved
    let probe = saved_fill_state.clone().unwrap_or_else(|| fill_snapshot(&params, 0.0, params.total_amount, 0, None));
    if !sink.save(probe) {
        let path = state_file.as_deref().map_or_else(String::new, |p| p.display().to_string());
        anyhow::bail!("cannot save the fill state to {path}; refusing to fill without restart protection");
    }

    // Determine which instrument the taker is allocating. For a buy on CC-USDC
    // the buyer allocates USDC (the quote). For a sell, the seller allocates CC
    // (the base). This controls whether the settler needs CIP-56 Holdings in
    // addition to Amulets for the multicall's unified `holding_cids` pool.
    let (base_instrument, quote_instrument) = split_market(&params.market_id);
    let allocation_instrument = match params.direction {
        FillDirection::Buy => quote_instrument.clone(),
        FillDirection::Sell => base_instrument.clone(),
    };

    // Shutdown signal for the fill loop (Ctrl-C sets persistent flag + wakes sleeps)
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let fill_shutdown = Arc::new(Notify::new());
    let force_exit = || std::process::exit(1);
    let ctrl_c = watch_ctrl_c(tokio::signal::ctrl_c, shutdown_flag.clone(), fill_shutdown.clone(), force_exit);
    let _ctrl_c = AbortOnDrop(rt.spawn(ctrl_c));

    // Create orderbook client for RFQ operations (quotes, prices)
    let mut client = OrderbookClient::new(&config)
        .await
        .context("Failed to create orderbook client")?;

    // Ledger client for DvpProposal / fee lookups
    let mut ledger = DAppProviderClient::new(
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
    .context("Failed to create ledger client")?;

    // Orderbook-rpc client for fetching settlement proposal fees
    let mut rpc_client = within(
        "orderbook-rpc connect",
        RPC_CONNECT_BUDGET,
        OrderbookRpcClient::connect(&config.orderbook_grpc_url, None),
    )
    .await
    .context("Failed to create orderbook-rpc client")?;
    rpc_client.set_jwt(rpc_token(&config)?);

    // Best-effort error reporter (ReportErrors -> orderbook-rpc). Idempotent
    // across the per-market fill loops; mints a fresh short-TTL JWT per flush.
    agent_logic::error_reporter::init_from_config(&config);

    // RFQ V2: receiver-preapproval preflight (once per loop start, design §6.5)
    // and the per-market input-cid cap for own-holdings selection.
    let atomic_max_inputs = config
        .markets
        .iter()
        .find(|m| m.market_id == params.market_id)
        .and_then(|m| m.rfq.as_ref())
        .and_then(|r| r.v2.as_ref())
        .map(|v| v.max_input_holdings)
        .unwrap_or(100)
        .min(100);
    let atomic = match (params.atomic, atomic_swapper.as_ref()) {
        (false, _) => None,
        (true, Some(swapper)) => Some(swapper),
        (true, None) => anyhow::bail!("atomic fill requires an AtomicSwapper"),
    };
    if let Some(swapper) = atomic {
        let receiving_instrument = match params.direction {
            FillDirection::Buy => &base_instrument,
            FillDirection::Sell => &quote_instrument,
        };
        crate::atomic_swap::ensure_receiver_preapproval(
            &config,
            &mut ledger,
            receiving_instrument,
            swapper.verbose,
            swapper.dry_run,
            swapper.force,
        )
        .await
        .context("receiver-preapproval preflight failed")?;
    }

    let (mut remaining, mut filled_total, mut round) = match plan {
        RestorePlan::Fresh => {
            if let Some(fs) = &saved_fill_state {
                announce_new_order(fs, &params);
            }
            (params.total_amount, 0.0_f64, 0u32)
        }
        RestorePlan::Resume { remaining, filled, round } => {
            info!(
                "Restoring fill state: filled={:.6} remaining={:.6} round={}",
                filled, remaining, round
            );
            (remaining, filled, round)
        }
        RestorePlan::Resolve { proposal_id, qty, remaining, filled, round } => {
            let mut status = Reauth { inner: &mut rpc_client, mint: || rpc_token(&config) };
            let saved = (remaining, filled, round);
            resume_saved_v1_round(&mut status, &settler, &sink, &params, &proposal_id, qty, saved).await?
        }
        RestorePlan::Refuse(why) => anyhow::bail!("{why}"),
    };
    let interval = Duration::from_secs(params.interval_secs);

    info!(
        "Fill loop started: {} {:.6} on market {} (min={:.6}, max={:.6}, interval={}s)",
        dir_str, params.total_amount, params.market_id,
        params.min_settlement, params.max_settlement, params.interval_secs
    );

    let mut monitors: JoinSet<()> = JoinSet::new();
    let mut abort_reason: Option<String> = None;

    loop {
        if shutdown_flag.load(Ordering::Relaxed) {
            warn!(
                "Ctrl-C received. Filled {:.6} / {:.6} ({:.1}%). Exiting fill loop.",
                filled_total, params.total_amount,
                (filled_total / params.total_amount) * 100.0
            );
            break;
        }

        if remaining <= 0.0 {
            break;
        }

        round = round.saturating_add(1);

        // Determine request amount
        let mut request_amount = remaining.min(params.max_settlement);
        if request_amount < params.min_settlement {
            info!(
                "Remaining {:.6} < min settlement {:.6} — fill complete ({:.6} filled)",
                remaining, params.min_settlement, filled_total
            );
            break;
        }

        // Get mid-price for limit computation
        let price_limit = match params.price_limit {
            Some(limit) => limit,
            None => {
                match client.get_price(&params.market_id).await {
                    Ok(price_resp) => {
                        let mid = match (price_resp.bid, price_resp.ask) {
                            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
                            _ => price_resp.last,
                        };
                        if mid <= 0.0 {
                            warn!("[round {}] No mid price available, waiting", round);
                            if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                            }
                            continue;
                        }
                        match params.direction {
                            FillDirection::Buy => mid * 1.03,
                            FillDirection::Sell => mid * 0.97,
                        }
                    }
                    Err(e) => {
                        warn!("[round {}] Failed to get price: {}, waiting", round, e);
                        interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await;
                        continue;
                    }
                }
            }
        };

        info!(
            "[round {}] Requesting quotes: {} {:.6} @ limit {:.6} (remaining={:.6})",
            round, dir_str, request_amount, price_limit, remaining
        );

        // ---- RFQ V2 atomic round (one-transaction settle) ----------------
        if let Some(swapper) = atomic {
            match atomic_round(
                &mut client,
                swapper,
                &params,
                dir_str,
                request_amount,
                price_limit,
                round,
                sink,
                (filled_total, remaining),
                atomic_max_inputs,
            )
            .await
            {
                AtomicRoundResult::Filled(fill) => {
                    filled_total += fill.filled_base;
                    remaining -= fill.filled_base;
                    info!(
                        "[round {}] Atomic settle committed: update={} — accepted={:.6}/{:.6} ({:.1}%)",
                        round,
                        fill.update_id,
                        filled_total,
                        params.total_amount,
                        (filled_total / params.total_amount) * 100.0,
                    );
                    // Fill state persistence — identical to the v1 path.
                    let _ = sink.save(fill_snapshot(&params, filled_total, remaining, round, None));
                    interruptible_sleep(Duration::from_millis(100), &fill_shutdown, &shutdown_flag).await;
                }
                AtomicRoundResult::Retry => {
                    if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                    }
                }
                AtomicRoundResult::DryRun => {
                    warn!("[round {}] Dry run — atomic settle prepared+verified only, exiting", round);
                    break;
                }
                AtomicRoundResult::Abort { reason, marker } => {
                    if let Some(marker) = marker {
                        // The submit started only once this marker was saved; saved again as a backstop
                        let _ = sink.save(fill_snapshot(&params, filled_total, remaining, round, Some(marker)));
                    }
                    abort_reason = Some(reason);
                    break;
                }
            }
            continue;
        }

        let rfq_response = match client
            .request_quotes(
                &params.market_id,
                dir_str,
                &format!("{:.10}", request_amount),
                vec![],
                Some(15),
            )
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!("[round {}] RFQ request failed: {}, waiting", round, e);
                if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                }
                continue;
            }
        };

        info!(
            "[round {}] RFQ {}: {} quotes, {} rejections (requested={}, responded={})",
            round, rfq_response.rfq_id,
            rfq_response.quotes.len(), rfq_response.rejections.len(),
            rfq_response.lps_requested, rfq_response.lps_responded
        );

        // Filter quotes by price limit
        let acceptable_quotes: Vec<_> = rfq_response
            .quotes
            .iter()
            .filter(|q| {
                let price: f64 = q.price.parse().unwrap_or(0.0);
                match params.direction {
                    FillDirection::Buy => price <= price_limit,
                    FillDirection::Sell => price >= price_limit,
                }
            })
            .collect();

        let (rfq_id, best_quote_id, best_price, best_qty) = if let Some(best) =
            pick_best_quote_opt(&acceptable_quotes, params.direction)
        {
            (
                rfq_response.rfq_id.clone(),
                best.quote_id.clone(),
                best.price.clone(),
                best.quantity.clone(),
            )
        } else {
            // Try adapted RFQ if LPs returned size hints
            let mut best_max: Option<f64> = None;
            for rejection in &rfq_response.rejections {
                if let Some(ref max_str) = rejection.max_quantity {
                    if let Ok(max_val) = max_str.parse::<f64>() {
                        if max_val >= params.min_settlement {
                            best_max = Some(match best_max {
                                Some(current) => current.max(max_val),
                                None => max_val,
                            });
                        }
                    }
                }
            }
            let adapted_max = match best_max {
                Some(v) if v < request_amount => v,
                _ => {
                    warn!("[round {}] No acceptable quotes within limit {:.6}", round, price_limit);
                    if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                    }
                    continue;
                }
            };
            info!(
                "[round {}] No quotes at limit {:.6}. LP max={:.6}, retrying with adapted amount",
                round, price_limit, adapted_max
            );
            request_amount = adapted_max;
            let retry_resp = match client
                .request_quotes(
                    &params.market_id,
                    dir_str,
                    &format!("{:.10}", request_amount),
                    vec![],
                    Some(15),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("[round {}] Adapted RFQ failed: {}", round, e);
                    if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                    }
                    continue;
                }
            };
            let retry_quotes: Vec<_> = retry_resp
                .quotes
                .iter()
                .filter(|q| {
                    let price: f64 = q.price.parse().unwrap_or(0.0);
                    match params.direction {
                        FillDirection::Buy => price <= price_limit,
                        FillDirection::Sell => price >= price_limit,
                    }
                })
                .collect();
            if let Some(best) = pick_best_quote_opt(&retry_quotes, params.direction) {
                (
                    retry_resp.rfq_id.clone(),
                    best.quote_id.clone(),
                    best.price.clone(),
                    best.quantity.clone(),
                )
            } else {
                warn!(
                    "[round {}] No acceptable quotes even at adapted amount {:.6}",
                    round, request_amount
                );
                if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                }
                continue;
            }
        };

        info!(
            "[round {}] Accepting quote {} @ {} (qty={})",
            round, best_quote_id, best_price, best_qty
        );

        let accept_resp = match client.accept_quote(&rfq_id, &best_quote_id).await {
            Ok(r) if r.success => r,
            Ok(r) => {
                warn!("[round {}] Accept quote failed: {}", round, r.message);
                if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                }
                continue;
            }
            Err(e) => {
                warn!("[round {}] Accept quote error: {}", round, e);
                if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                }
                continue;
            }
        };

        let proposal_id = accept_resp
            .proposal_id
            .ok_or_else(|| "accept_quote returned no proposal_id".to_string());
        let Some(proposal_id) =
            post_accept_step(proposal_id, round, interval, &fill_shutdown, &shutdown_flag).await
        else {
            continue;
        };
        info!("[round {}] Quote accepted, proposal={}", round, proposal_id);

        // 1. Poll for the DvpProposal contract (LP creates it right after accept_quote)
        let discovered = poll_dvp_proposal_cid(
            &mut ledger,
            &proposal_id,
            Duration::from_secs(180),
            &fill_shutdown,
            &shutdown_flag,
        )
        .await
        .map_err(|e| format!("Could not discover DvpProposal for {}: {}", proposal_id, e));
        let Some(dvp_proposal_cid) =
            post_accept_step(discovered, round, interval, &fill_shutdown, &shutdown_flag).await
        else {
            continue;
        };

        // 2. Fetch settlement proposal fees (dvp + allocation processing fees
        // for our role). The proposal stores them in **USD**; we must divide
        // by the current CC/USD rate before handing off to the multicall
        // builder — otherwise on-chain Canton transfers carry 0.15 CC worth
        // ~$0.023 for a $0.15 fee, which the orderbook-rpc amount gate
        // (correctly) rejects as underpayment.
        let mint = || rpc_token(&config);
        let fee_lookup = fetch_settlement_fees(&mut rpc_client, mint, &mut ledger, &proposal_id, params.direction);
        let fees = within("settlement fee lookup", FEE_FETCH_BUDGET, fee_lookup)
            .await
            .map_err(|e| format!("Failed to fetch settlement fees for {}: {}", proposal_id, e));
        let Some((dvp_fee_cc, alloc_fee_cc, dvp_fee_usd, alloc_fee_usd)) =
            post_accept_step(fees, round, interval, &fill_shutdown, &shutdown_flag).await
        else {
            continue;
        };

        let qty: f64 = best_qty.parse().unwrap_or(0.0);
        // 3. Submit the single multicall: Accept + Allocate + fees + traffic.
        // For CC allocation (sell), pass the trade qty so amulet selection covers it.
        let allocation_cc = if allocation_instrument.eq_ignore_ascii_case("cc")
            || allocation_instrument.eq_ignore_ascii_case("amulet")
        {
            Decimal::from_str(&best_qty).ok()
        } else {
            None
        };
        let marker = UnresolvedRound { kind: UnresolvedKind::V1, id: proposal_id.clone(), qty };
        let mark = RoundMark::new(sink, &params, (filled_total, remaining), round, marker);
        let settle = v1_settle(&mark, || {
            settler.accept_and_settle(
                &proposal_id,
                &dvp_proposal_cid,
                &dvp_fee_cc,
                &alloc_fee_cc,
                &dvp_fee_usd,
                &alloc_fee_usd,
                &allocation_instrument,
                allocation_cc,
            )
        })
        .await;
        let settled = match settle {
            Ok(result) => format!(
                "via multicall: cid={} update={} traffic={} bytes",
                result.contract_id, result.update_id, result.traffic_total
            ),
            Err(e) => {
                let mut status = Reauth { inner: &mut rpc_client, mint: || rpc_token(&config) };
                let marked = mark.marked.clone();
                match v1_round_end(&e, &mut status, &settler, &proposal_id, &sink, marked, round).await {
                    V1End::Settled(settled) => settled,
                    V1End::Retry => {
                        warn!(
                            "[round {}] Multicall settlement failed for {}: {:#}",
                            round, proposal_id, e
                        );
                        if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                        }
                        continue;
                    }
                    V1End::Abort(reason) => {
                        abort_reason = Some(reason);
                        break;
                    }
                }
            }
        };
        filled_total += qty;
        remaining -= qty;
        info!(
            "[round {}] Accepted+Allocated {} {} — accepted={:.6}/{:.6} ({:.1}%)",
            round,
            proposal_id,
            settled,
            filled_total,
            params.total_amount,
            (filled_total / params.total_amount) * 100.0,
        );

        // Spawn non-blocking progress monitor
        let mon_config = config.clone();
        let mon_pid = proposal_id.clone();
        let mon_dir = params.direction;
        let mon_flag = shutdown_flag.clone();
        monitors.spawn_on(monitor_settlement_progress(mon_config, mon_pid, round, mon_dir, mon_flag), &rt);

        // Persist fill state after each successful round
        let _ = sink.save(fill_snapshot(&params, filled_total, remaining, round, None));

        interruptible_sleep(Duration::from_millis(100), &fill_shutdown, &shutdown_flag).await;
    }

    let running = settler.fee_debits.running();
    if running > 0 {
        warn!(
            "Waiting up to {:?} for {} off-chain fee debit(s) of committed rounds; a second Ctrl-C exits now",
            settler.fee_debit_wait(),
            running
        );
    }
    let unpaid = settler.wait_for_fee_debits().await;
    if unpaid > 0 {
        warn!(
            "{} off-chain fee debit(s) still running at exit (multicall already committed; reconcile manually)",
            unpaid
        );
    }

    if let Some(reason) = abort_reason {
        let kind = if atomic.is_some() { "atomic fill loop" } else { "fill loop" };
        anyhow::bail!(
            "{} aborted (filled {:.6}/{:.6}): {}",
            kind, filled_total, params.total_amount, reason
        );
    }

    // Wait for all settlement monitors to complete before exiting
    let total_monitors = monitors.len();
    if total_monitors > 0 && !shutdown_flag.load(Ordering::Relaxed) {
        info!("Waiting for {} settlement(s) to complete...", total_monitors);
        let deadline = clock::deadline_after(MONITOR_WAIT);
        let completed = wait_for_monitors(&mut monitors, deadline, &fill_shutdown, &shutdown_flag).await;
        if completed == total_monitors {
            info!("All {} settlement(s) completed. Total filled: {:.6} in {} round(s)",
                  total_monitors, filled_total, round);
        } else {
            warn!("Exiting with {} pending settlement(s). Accepted: {:.6} in {} round(s)",
                  total_monitors.saturating_sub(completed), filled_total, round);
        }
    } else if total_monitors == 0 && filled_total > 0.0 {
        info!("Total accepted: {:.6} in {} round(s) (no settlements to monitor)", filled_total, round);
    }
    monitors.abort_all();

    Ok(())
}

/// Wait for the progress monitors until `deadline` or shutdown, then abort the
/// rest; returns how many completed.
async fn wait_for_monitors(
    monitors: &mut JoinSet<()>,
    deadline: tokio::time::Instant,
    fill_shutdown: &Notify,
    shutdown_flag: &AtomicBool,
) -> usize {
    let mut completed = 0usize;
    while !shutdown_flag.load(Ordering::Relaxed) {
        tokio::select! {
            next = tokio::time::timeout_at(deadline, monitors.join_next()) => match next {
                Ok(Some(_)) => completed = completed.saturating_add(1),
                Ok(None) => break,
                Err(_) => {
                    warn!("Settlement monitor timeout");
                    break;
                }
            },
            _ = fill_shutdown.notified() => break,
        }
    }
    monitors.abort_all();
    completed
}

/// Reads a settlement's status; the orderbook-rpc client in production.
trait StatusSource {
    async fn settlement_status(&mut self, proposal_id: &str) -> Result<GetSettlementStatusResponse>;
}

impl StatusSource for OrderbookRpcClient {
    async fn settlement_status(&mut self, proposal_id: &str) -> Result<GetSettlementStatusResponse> {
        self.get_settlement_status(proposal_id).await
    }
}

/// Reads a settlement proposal; the orderbook-rpc client in production.
trait ProposalSource {
    async fn settlement_proposal(&mut self, proposal_id: &str) -> Result<Option<SettlementProposalMessage>>;
}

impl ProposalSource for OrderbookRpcClient {
    async fn settlement_proposal(&mut self, proposal_id: &str) -> Result<Option<SettlementProposalMessage>> {
        self.get_settlement_proposal_by_id(proposal_id).await
    }
}

/// A client whose bearer token can be replaced.
trait TokenSource {
    fn set_token(&mut self, token: String);
}

impl TokenSource for OrderbookRpcClient {
    fn set_token(&mut self, token: String) {
        self.set_jwt(token);
    }
}

/// Mints a token before every read, so no read carries an expired one; a
/// failed mint fails the read.
struct Reauth<'a, S, F> {
    inner: &'a mut S,
    mint: F,
}

impl<S: TokenSource + StatusSource, F: FnMut() -> Result<String>> StatusSource for Reauth<'_, S, F> {
    async fn settlement_status(&mut self, proposal_id: &str) -> Result<GetSettlementStatusResponse> {
        let token = (self.mint)()?;
        self.inner.set_token(token);
        self.inner.settlement_status(proposal_id).await
    }
}

impl<S: TokenSource + ProposalSource, F: FnMut() -> Result<String>> ProposalSource for Reauth<'_, S, F> {
    async fn settlement_proposal(&mut self, proposal_id: &str) -> Result<Option<SettlementProposalMessage>> {
        let token = (self.mint)()?;
        self.inner.set_token(token);
        self.inner.settlement_proposal(proposal_id).await
    }
}

/// What the settlement status says about a multicall of unknown outcome.
#[derive(Debug, PartialEq, Eq)]
enum SettleCheck {
    /// The taker's accept is recorded: the multicall committed.
    Committed(i32),
    /// The settlement ended (failed or cancelled) without proof of a commit.
    Ended(i32),
    /// No proof of a commit; the reason is kept for the abort message.
    NotEstablished(String),
}

/// Whether the status proves the taker's accept+allocate committed.
fn proves_commit(status: &GetSettlementStatusResponse) -> bool {
    let accepted = SettlementStage::DvpAccepted as i32..=SettlementStage::Settled as i32;
    let ended = [SettlementStage::Failed as i32, SettlementStage::Cancelled as i32];
    let accept_step_done = status.dvp_accept.as_ref().is_some_and(|step| {
        matches!(
            DvpStepStatusEnum::try_from(step.status),
            Ok(DvpStepStatusEnum::DvpStepStatusCompleted | DvpStepStatusEnum::DvpStepStatusConfirmed)
        )
    });
    accepted.contains(&status.stage) || (accept_step_done && !ended.contains(&status.stage))
}

/// A proven commit is a fill and starts its fee debits; otherwise the abort reason.
fn after_reconcile(
    settler: &MulticallSettler,
    proposal_id: &str,
    unknown: &SettleOutcomeUnknown,
    check: SettleCheck,
) -> Result<String, String> {
    let why = match check {
        SettleCheck::Committed(stage) => {
            settler.debit_fees(proposal_id);
            return Ok(format!("(settlement stage {stage}; execute response lost)"));
        }
        SettleCheck::Ended(stage) => format!("settlement ended at stage {stage}"),
        SettleCheck::NotEstablished(why) => why,
    };
    Err(format!(
        "{unknown} for proposal {proposal_id} ({why}) — check the ledger before restarting"
    ))
}

/// Reconcile a multicall of unknown outcome. A proven commit is a fill;
/// otherwise `marked` is saved, so a restart cannot quote the round again.
async fn settle_unknown<S: StatusSource>(
    source: &mut S,
    settler: &MulticallSettler,
    proposal_id: &str,
    unknown: &SettleOutcomeUnknown,
    sink: &FillStateSink<'_>,
    marked: SavedFillState,
) -> Result<String, String> {
    let check = reconcile_settle(source, proposal_id, SETTLE_RECONCILE_POLLS, SETTLE_RECONCILE_EVERY).await;
    let outcome = after_reconcile(settler, proposal_id, unknown, check);
    if outcome.is_err() {
        let _ = sink.save(marked);
    }
    outcome
}

/// How a V1 round whose settle failed ends.
#[derive(Debug, PartialEq, Eq)]
enum V1End {
    /// Nothing committed; a later round may quote again.
    Retry,
    /// Reconciled as a commit; how it was proven.
    Settled(String),
    /// The loop stops for this reason.
    Abort(String),
}

/// The end of a V1 round whose settle failed with `e`; a possible commit is
/// reconciled before anything else, and `marked` is saved unless it is proven.
async fn v1_round_end<S: StatusSource>(
    e: &anyhow::Error,
    source: &mut S,
    settler: &MulticallSettler,
    proposal_id: &str,
    sink: &FillStateSink<'_>,
    marked: SavedFillState,
    round: u32,
) -> V1End {
    let unknown = match v1_failure(e) {
        V1Failure::Stop(reason) => return V1End::Abort(reason),
        V1Failure::Retry => return V1End::Retry,
        V1Failure::Unknown(unknown) => unknown,
    };
    // Never re-quote while this multicall may have committed
    warn!(
        "[round {}] {} for {}; reading the settlement status before anything else",
        round, unknown, proposal_id
    );
    match settle_unknown(source, settler, proposal_id, unknown, sink, marked).await {
        Ok(settled) => V1End::Settled(settled),
        Err(reason) => V1End::Abort(reason),
    }
}

/// Read the settlement status until it proves a commit, the settlement ends,
/// or `polls` reads have passed. Only proof of a commit counts as a fill.
async fn reconcile_settle<S: StatusSource>(
    source: &mut S,
    proposal_id: &str,
    polls: u32,
    every: Duration,
) -> SettleCheck {
    let mut last = "no status read".to_string();
    for poll in 1..=polls {
        let read = tokio::time::timeout(STATUS_CALL_BUDGET, source.settlement_status(proposal_id)).await;
        match read {
            Ok(Ok(status)) if proves_commit(&status) => return SettleCheck::Committed(status.stage),
            Ok(Ok(status))
                if status.stage == SettlementStage::Failed as i32
                    || status.stage == SettlementStage::Cancelled as i32 =>
            {
                return SettleCheck::Ended(status.stage);
            }
            Ok(Ok(status)) => last = format!("settlement still at stage {}", status.stage),
            Ok(Err(e)) => last = format!("status unavailable: {e:#}"),
            Err(_) => last = format!("status read took over {STATUS_CALL_BUDGET:?}"),
        }
        if poll < polls {
            tokio::time::sleep(every).await;
        }
    }
    SettleCheck::NotEstablished(last)
}

/// Background task: polls settlement status and logs progress until settled or timeout.
/// Runs non-blocking — the fill loop continues immediately after spawning this.
async fn monitor_settlement_progress(
    config: BaseConfig,
    proposal_id: String,
    round: u32,
    direction: FillDirection,
    shutdown_flag: Arc<AtomicBool>,
) {
    use orderbook_proto::settlement::NextAction;

    let connect = OrderbookRpcClient::connect(&config.orderbook_grpc_url, None);
    let Ok(mut rpc) = within("orderbook-rpc connect", RPC_CONNECT_BUDGET, connect).await else {
        warn!("[round {}] Could not connect RPC for progress monitoring {}", round, proposal_id);
        return;
    };
    if let Ok(key) = config.private_key.expose() {
        if let Ok(jwt) = agent_logic::auth::generate_jwt(
            &config.party_id, &config.role, &key,
            config.token_ttl_secs, Some(config.node_name.as_str()),
        ) {
            rpc.set_jwt(jwt);
        }
    }

    let deadline = clock::deadline_after(Duration::from_secs(300));
    let mut last_stage = String::new();

    fn action_name(a: i32) -> &'static str {
        match NextAction::try_from(a) {
            Ok(NextAction::None) => "Done",
            Ok(NextAction::Preconfirm) => "Preconfirm",
            Ok(NextAction::PayDvpFee) => "PayDvpFee",
            Ok(NextAction::CreateDvp) => "CreateDvp",
            Ok(NextAction::AcceptDvp) => "AcceptDvp",
            Ok(NextAction::PayAllocFee) => "PayAllocFee",
            Ok(NextAction::Allocate) => "Allocate",
            Ok(NextAction::Wait) => "Wait",
            Ok(NextAction::MulticallAccept) => "MulticallAccept",
            _ => "Unknown",
        }
    }

    loop {
        if shutdown_flag.load(Ordering::Relaxed) || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;

        match rpc.get_settlement_status(&proposal_id).await {
            Ok(status) => {
                let buyer = action_name(status.buyer_next_action);
                let seller = action_name(status.seller_next_action);
                let (us, lp) = match direction {
                    FillDirection::Buy => (buyer, seller),
                    FillDirection::Sell => (seller, buyer),
                };
                let stage = format!("us={}, LP={}", us, lp);
                if stage != last_stage {
                    info!("[round {}] Settlement progress {}: {}", round, proposal_id, stage);
                    last_stage = stage;
                }
                // stage == 11 is SETTLEMENT_STAGE_SETTLED
                if status.stage == 11 {
                    info!("[round {}] Settlement completed {}", round, proposal_id);
                    break;
                }
                // Also stop on failed/cancelled
                if status.stage >= 12 {
                    warn!("[round {}] Settlement ended (stage={}) {}", round, status.stage, proposal_id);
                    break;
                }
            }
            Err(_) => {}
        }
    }
}

/// Poll the ledger for the DvpProposal contract_id corresponding to `proposal_id`.
async fn poll_dvp_proposal_cid(
    ledger: &mut DAppProviderClient,
    proposal_id: &str,
    timeout: Duration,
    fill_shutdown: &Notify,
    shutdown_flag: &AtomicBool,
) -> Result<String> {
    let deadline = clock::deadline_after(timeout);
    let mut poll_every = Duration::from_millis(500);
    let ids = [proposal_id.to_string()];
    loop {
        if shutdown_flag.load(Ordering::Relaxed) {
            anyhow::bail!("shutdown while waiting for DvpProposal");
        }
        let lookup = ledger.get_settlement_contracts(&ids);
        let Ok(contracts) = tokio::time::timeout_at(deadline, lookup).await else {
            anyhow::bail!("timed out waiting for DvpProposal contract");
        };
        let contracts = contracts?;
        for c in &contracts {
            if c.settlement_id == proposal_id && c.contract_type == "DvpProposal" {
                return Ok(c.contract_id.clone());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for DvpProposal contract");
        }
        if interruptible_sleep(poll_every, fill_shutdown, shutdown_flag).await {
            anyhow::bail!("shutdown while waiting for DvpProposal");
        }
        poll_every = poll_every.saturating_mul(2).min(Duration::from_secs(3));
    }
}

/// Fetch the (dvp_processing_fee, allocation_processing_fee) owed by the taker
/// side (buyer for Buy direction, seller for Sell direction).
///
/// Returns CC-denominated amounts. The `settlement_proposals` table stores the
/// fees in **USD**, so this helper also fetches the current CC/USD rate from
/// scan (via `ledger.get_dso_rates()`) and divides to get CC. Both return
/// values are formatted as fixed-10-decimal strings so downstream
/// `Decimal::from_str` round-trips cleanly.
async fn fetch_settlement_fees(
    rpc: &mut OrderbookRpcClient,
    mint: impl FnMut() -> Result<String>,
    ledger: &mut DAppProviderClient,
    proposal_id: &str,
    direction: FillDirection,
) -> Result<(String, String, String, String)> {
    let (dvp_usd_str, alloc_usd_str) = proposal_fees_usd(rpc, mint, proposal_id, direction).await?;

    let dvp_usd = Decimal::from_str(&dvp_usd_str)
        .with_context(|| format!("invalid dvp fee decimal '{}'", dvp_usd_str))?;
    let alloc_usd = Decimal::from_str(&alloc_usd_str)
        .with_context(|| format!("invalid alloc fee decimal '{}'", alloc_usd_str))?;

    // Convert USD → CC via the current amulet price. A zero rate is a hard
    // error: we'd rather fail the round than publish a USD-shaped fee on-chain.
    let rates = ledger
        .get_dso_rates()
        .await
        .context("failed to fetch DSO rates for USD→CC fee conversion")?;
    let rate = Decimal::from_str(&rates.cc_usd_rate)
        .with_context(|| format!("invalid cc_usd_rate '{}'", rates.cc_usd_rate))?;
    if rate <= Decimal::ZERO {
        anyhow::bail!("cc_usd_rate is {} — cannot convert fees", rate);
    }

    let dvp_cc = usd_to_cc(dvp_usd, rate, "dvp")?;
    let alloc_cc = usd_to_cc(alloc_usd, rate, "alloc")?;

    // Defensive sanity check: if the converted CC amount equals the USD value,
    // the rate is ~1.0 (unlikely in production) OR the conversion silently
    // no-op'd. Surface immediately rather than publish an underpayment.
    if dvp_cc == dvp_usd && dvp_usd > Decimal::ZERO {
        anyhow::bail!(
            "dvp fee cc==usd ({} at rate {}) — conversion suspiciously identity",
            dvp_cc, rate
        );
    }

    info!(
        "Taker fees: dvp={} USD → {} CC, alloc={} USD → {} CC (rate={} USD/CC)",
        dvp_usd, dvp_cc, alloc_usd, alloc_cc, rate
    );

    Ok((
        wire_amount(dvp_cc),
        wire_amount(alloc_cc),
        dvp_usd_str,
        alloc_usd_str,
    ))
}

/// The taker side's (dvp, allocation) processing fees in USD from the proposal;
/// every read carries a token minted for it.
async fn proposal_fees_usd<S: TokenSource + ProposalSource>(
    rpc: &mut S,
    mint: impl FnMut() -> Result<String>,
    proposal_id: &str,
    direction: FillDirection,
) -> Result<(String, String)> {
    let mut rpc = Reauth { inner: rpc, mint };
    // Retry a few times — the proposal row may take a moment to appear after accept_quote.
    let mut attempt = 0u32;
    loop {
        match rpc.settlement_proposal(proposal_id).await? {
            Some(p) => {
                let (dvp, alloc) = match direction {
                    FillDirection::Buy => (p.dvp_processing_fee_buyer, p.allocation_processing_fee_buyer),
                    FillDirection::Sell => (p.dvp_processing_fee_seller, p.allocation_processing_fee_seller),
                };
                // Normalise empty strings to "0.0".
                let dvp = if dvp.trim().is_empty() { "0.0".to_string() } else { dvp };
                let alloc = if alloc.trim().is_empty() { "0.0".to_string() } else { alloc };
                return Ok((dvp, alloc));
            }
            None => {
                attempt = attempt.saturating_add(1);
                if attempt > 10 {
                    anyhow::bail!("settlement proposal not found after 10 attempts");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// A USD fee in CC at `rate`, rounded to 10 places; a result out of range or
/// above [`MAX_FEE_CC`] is an error, never a silent 0.
fn usd_to_cc(usd: Decimal, rate: Decimal, what: &str) -> Result<Decimal> {
    let cc = usd
        .checked_div(rate)
        .ok_or_else(|| anyhow!("{what} fee {usd} USD at rate {rate} is out of range"))?
        .round_dp(10);
    if cc.abs() > Decimal::from(MAX_FEE_CC) {
        anyhow::bail!("{what} fee of {cc} CC at rate {rate} exceeds the {MAX_FEE_CC} CC sanity cap");
    }
    Ok(cc)
}

/// The fixed 10-decimal wire form of an amount already rounded to 10 places.
fn wire_amount(mut cc: Decimal) -> String {
    cc.rescale(10);
    cc.to_string()
}

/// The saved fill state of `party_id`; a file that cannot be read, or that
/// belongs to another party, stops the start rather than being replaced.
pub(crate) fn load_fill_state(path: &Path, party_id: &str) -> Result<Option<SavedFillState>> {
    let loaded = agent_logic::state::load_state_strict(path)
        .with_context(|| format!("fix or move {} before filling", path.display()))?;
    match loaded {
        None => Ok(None),
        Some(state) if state.party_id == party_id => Ok(state.fill_state),
        Some(_) => anyhow::bail!("{} belongs to another party; move it first", path.display()),
    }
}

/// Patch `fill_state` into the state file; a new file is started only when none exists.
fn save_fill_state_only(
    path: &std::path::Path,
    party_id: &str,
    fill_state: SavedFillState,
) -> Result<()> {
    use agent_logic::state::SavedState;
    let mut state = match agent_logic::state::load_state_strict(path)? {
        None => SavedState::new(party_id.to_string(), 0),
        Some(state) if state.party_id == party_id => state,
        Some(_) => anyhow::bail!("{} belongs to another party; move it first", path.display()),
    };
    state.fill_state = Some(fill_state);
    state.saved_at = clock::now_utc().to_rfc3339();
    agent_logic::state::save_state(path, &state)?;
    Ok(())
}

/// Split a market id like "CC-USDC" into `(base, quote)` = `("CC", "USDC")`.
/// If the id has no `-`, returns it as base and empty string as quote.
fn split_market(market_id: &str) -> (String, String) {
    match market_id.split_once('-') {
        Some((b, q)) => (b.to_string(), q.to_string()),
        None => (market_id.to_string(), String::new()),
    }
}

// ============================================================================
// RFQ V2 atomic round
// ============================================================================

use orderbook_proto::rfqv2::{AtomicQuoteEnvelope, AtomicQuoteInfo};

use crate::atomic_swap::{AtomicFill, AtomicSwapper, SwapOutcome};

#[derive(Debug)]
enum AtomicRoundResult {
    Filled(AtomicFill),
    /// Transient failure — wait one interval and try a fresh round.
    Retry,
    DryRun,
    /// The loop must stop. A marker names a round whose commit status is
    /// unknowable (double-fill guard); none when nothing was submitted.
    Abort { reason: String, marker: Option<UnresolvedRound> },
}

/// One atomic round: request V2 quotes → pick the best within the limit → accept → settle
/// the envelope; `sink` and `progress` (filled, remaining) give its restart marker.
#[allow(clippy::too_many_arguments)]
async fn atomic_round(
    client: &mut OrderbookClient,
    swapper: &AtomicSwapper,
    params: &FillParams,
    dir_str: &str,
    mut request_amount: f64,
    price_limit: f64,
    round: u32,
    sink: FillStateSink<'_>,
    progress: (f64, f64),
    max_input_holdings: usize,
) -> AtomicRoundResult {
    let rfq_response = match client
        .request_quotes_atomic(
            &params.market_id,
            dir_str,
            &format!("{:.10}", request_amount),
            None,
            vec![],
            Some(15),
            params.fee_tokens.clone(),
        )
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            warn!("[round {}] Atomic RFQ request failed: {}", round, e);
            return AtomicRoundResult::Retry;
        }
    };

    info!(
        "[round {}] Atomic RFQ {}: {} quotes, {} rejections (requested={}, responded={})",
        round, rfq_response.rfq_id,
        rfq_response.quotes.len(), rfq_response.rejections.len(),
        rfq_response.lps_requested, rfq_response.lps_responded
    );

    let within_limit = |q: &&AtomicQuoteInfo| {
        let price: f64 = q.price.parse().unwrap_or(0.0);
        match params.direction {
            FillDirection::Buy => price <= price_limit,
            FillDirection::Sell => price >= price_limit,
        }
    };
    let acceptable: Vec<&AtomicQuoteInfo> = rfq_response.quotes.iter().filter(within_limit).collect();

    let (rfq_id, best) = if let Some(best) = pick_best_atomic_quote_opt(&acceptable, params.direction)
    {
        (rfq_response.rfq_id.clone(), best.clone())
    } else {
        // Adapted retry from LP size hints — mirror of the v1 flow.
        let mut best_max: Option<f64> = None;
        for rejection in &rfq_response.rejections {
            if let Ok(max_val) = rejection.max_quantity.parse::<f64>() {
                if max_val >= params.min_settlement {
                    best_max = Some(match best_max {
                        Some(current) => current.max(max_val),
                        None => max_val,
                    });
                }
            }
        }
        let adapted_max = match best_max {
            Some(v) if v < request_amount => v,
            _ => {
                warn!("[round {}] No acceptable atomic quotes within limit {:.6}", round, price_limit);
                return AtomicRoundResult::Retry;
            }
        };
        info!(
            "[round {}] No atomic quotes at limit {:.6}. LP max={:.6}, retrying with adapted amount",
            round, price_limit, adapted_max
        );
        request_amount = adapted_max;
        let retry_resp = match client
            .request_quotes_atomic(
                &params.market_id,
                dir_str,
                &format!("{:.10}", request_amount),
                None,
                vec![],
                Some(15),
                params.fee_tokens.clone(),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("[round {}] Adapted atomic RFQ failed: {}", round, e);
                return AtomicRoundResult::Retry;
            }
        };
        let retry_quotes: Vec<&AtomicQuoteInfo> =
            retry_resp.quotes.iter().filter(within_limit).collect();
        match pick_best_atomic_quote_opt(&retry_quotes, params.direction) {
            Some(best) => (retry_resp.rfq_id.clone(), best.clone()),
            None => {
                warn!(
                    "[round {}] No acceptable atomic quotes even at adapted amount {:.6}",
                    round, request_amount
                );
                return AtomicRoundResult::Retry;
            }
        }
    };

    // Fee visibility before accept (design §14 D21): accepting implies consent;
    // the settle path asserts the signed lpFees equal exactly this.
    let fee_display = best
        .settlement_fee
        .as_ref()
        .map(|f| format!("{} {} -> {}", f.amount, f.instrument_id, f.receiver))
        .unwrap_or_else(|| "none".to_string());
    info!(
        "[round {}] Accepting atomic quote {} @ {} (qty={}, LP={}, settlement fee: {})",
        round, best.quote_id, best.price, best.quantity, best.lp_name, fee_display
    );

    let accept_resp = match client.accept_quote_atomic(&rfq_id, &best.quote_id, Some(15)).await {
        Ok(r) if r.success => r,
        Ok(r) => {
            // gRPC OK + success=false is an LP reject in the body — never a
            // transport error; branch on the typed reason.
            let reason = agent_logic::client::atomic_reject_reason_name(r.reject_reason);
            let detail = if r.reject_detail.is_empty() { &r.message } else { &r.reject_detail };
            warn!(
                "[round {}] Atomic accept rejected by LP {}: {} ({})",
                round, best.lp_name, reason, detail
            );
            return AtomicRoundResult::Retry;
        }
        Err(e) => {
            warn!("[round {}] Atomic accept error: {}", round, e);
            return AtomicRoundResult::Retry;
        }
    };
    let Some(envelope) = accept_resp.envelope else {
        warn!("[round {}] Atomic accept succeeded but carried no envelope", round);
        return AtomicRoundResult::Retry;
    };
    info!(
        "[round {}] Envelope received for quote {} (ticket='{}', lp_inputs={}, disclosed={})",
        round,
        best.quote_id,
        envelope.quote.as_ref().map(|q| q.ticket_id.as_str()).unwrap_or(""),
        envelope.lp_input_holding_cids.len(),
        envelope.disclosed.len(),
    );

    let base = envelope.quote.as_ref().map(|q| q.base_amount.as_str()).unwrap_or(&best.quantity);
    let qty: f64 = base.parse().unwrap_or(0.0);
    let marker = UnresolvedRound { kind: UnresolvedKind::Atomic, id: best.quote_id.clone(), qty };
    let mark = RoundMark::new(sink, params, progress, round, marker);
    let settled = atomic_settle(swapper, &mark, &envelope, &best, params.direction, max_input_holdings).await;
    round_result(settled, round, &best.quote_id, qty)
}

/// What the loop does after an atomic settle of `quote_id` (`qty` base).
fn round_result(settled: Result<SwapOutcome>, round: u32, quote_id: &str, qty: f64) -> AtomicRoundResult {
    match settled {
        Ok(SwapOutcome::Filled(fill)) => AtomicRoundResult::Filled(fill),
        Ok(SwapOutcome::Requote { reason }) => {
            warn!("[round {}] Atomic settle needs re-quote: {}", round, reason);
            AtomicRoundResult::Retry
        }
        Ok(SwapOutcome::Abort { reason }) => {
            let marker = UnresolvedRound { kind: UnresolvedKind::Atomic, id: quote_id.to_string(), qty };
            AtomicRoundResult::Abort { reason, marker: Some(marker) }
        }
        Ok(SwapOutcome::DryRun) => AtomicRoundResult::DryRun,
        Err(e) => match stop_reason(&e) {
            Some(reason) => AtomicRoundResult::Abort { reason, marker: None },
            None => {
                warn!("[round {}] Atomic settle error: {:#}", round, e);
                AtomicRoundResult::Retry
            }
        },
    }
}

/// Pick the best atomic quote (lowest price for buy, highest for sell).
fn pick_best_atomic_quote_opt<'a>(
    quotes: &[&'a AtomicQuoteInfo],
    direction: FillDirection,
) -> Option<&'a AtomicQuoteInfo> {
    match direction {
        FillDirection::Buy => quotes.iter().min_by(|a, b| {
            let pa: f64 = a.price.parse().unwrap_or(f64::MAX);
            let pb: f64 = b.price.parse().unwrap_or(f64::MAX);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        }),
        FillDirection::Sell => quotes.iter().max_by(|a, b| {
            let pa: f64 = a.price.parse().unwrap_or(f64::NEG_INFINITY);
            let pb: f64 = b.price.parse().unwrap_or(f64::NEG_INFINITY);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        }),
    }
    .copied()
}

use orderbook_proto::orderbook::RfqQuoteInfo;

/// Pick the best quote, returning None if the list is empty
fn pick_best_quote_opt<'a>(quotes: &[&'a RfqQuoteInfo], direction: FillDirection) -> Option<&'a RfqQuoteInfo> {
    match direction {
        FillDirection::Buy => quotes.iter().min_by(|a, b| {
            let pa: f64 = a.price.parse().unwrap_or(f64::MAX);
            let pb: f64 = b.price.parse().unwrap_or(f64::MAX);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        }),
        FillDirection::Sell => quotes.iter().max_by(|a, b| {
            let pa: f64 = a.price.parse().unwrap_or(f64::NEG_INFINITY);
            let pb: f64 = b.price.parse().unwrap_or(f64::NEG_INFINITY);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        }),
    }
    .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{Fake, FakeLedger};
    use agent_logic::settlement::StepResult;
    use orderbook_proto::settlement::DvpStepStatus;
    use std::collections::VecDeque;
    use std::time::Instant;

    fn params() -> FillParams {
        FillParams {
            direction: FillDirection::Buy,
            market_id: "CC-USDC".to_string(),
            total_amount: 10.0,
            price_limit: Some(0.2),
            min_settlement: 5.0,
            max_settlement: 10.0,
            interval_secs: 60,
            atomic: false,
            fee_tokens: Vec::new(),
        }
    }

    // NaN, infinite and non-positive values used to start a loop that could never fill correctly
    #[test]
    fn amounts_and_limit_must_be_finite_and_positive() {
        validate_params(&params()).unwrap();
        validate_params(&FillParams { price_limit: None, ..params() }).unwrap();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            let cases = [
                FillParams { total_amount: v, ..params() },
                FillParams { min_settlement: v, ..params() },
                FillParams { max_settlement: v, ..params() },
                FillParams { price_limit: Some(v), ..params() },
            ];
            for p in cases {
                let err = validate_params(&p).unwrap_err().to_string();
                assert!(err.contains("must be a finite number above 0"), "{v}: {err}");
            }
        }
    }

    /// Signal waits that answer from `script` in order.
    fn signals(script: Vec<std::io::Result<()>>) -> impl FnMut() -> std::future::Ready<std::io::Result<()>> {
        let mut script = script.into_iter();
        move || std::future::ready(script.next().unwrap_or_else(|| Err(std::io::Error::other("script ended"))))
    }

    fn no_handler() -> std::io::Result<()> {
        Err(std::io::Error::other("no handler"))
    }

    // A failing signal listener used to count as Ctrl-C and stop the loop at once
    #[tokio::test]
    async fn a_failed_ctrl_c_listener_is_not_a_shutdown() {
        let flag = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let exits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let exit = || {
            let exits = exits.clone();
            move || {
                exits.fetch_add(1, Ordering::Relaxed);
            }
        };
        watch_ctrl_c(signals(vec![no_handler()]), flag.clone(), notify.clone(), exit()).await;
        assert!(!flag.load(Ordering::Relaxed));
        watch_ctrl_c(signals(vec![Ok(()), no_handler()]), flag.clone(), notify, exit()).await;
        assert!(flag.load(Ordering::Relaxed));
        assert_eq!(exits.load(Ordering::Relaxed), 0, "a failed second wait forces nothing");
    }

    // A second Ctrl-C used to be ignored while the exit waited out the fee debits
    #[tokio::test]
    async fn a_second_ctrl_c_forces_exit() {
        let flag = Arc::new(AtomicBool::new(false));
        let exits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = exits.clone();
        let exit = move || {
            counted.fetch_add(1, Ordering::Relaxed);
        };
        watch_ctrl_c(signals(vec![Ok(()), Ok(())]), flag.clone(), Arc::new(Notify::new()), exit).await;
        assert!(flag.load(Ordering::Relaxed));
        assert_eq!(exits.load(Ordering::Relaxed), 1);
    }

    // A garbage rate used to panic in the division or produce a fee nobody can pay
    #[test]
    fn fee_conversion_fails_closed_and_keeps_the_wire_format() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        let cc = usd_to_cc(d("0.15"), d("0.15"), "dvp").unwrap();
        assert_eq!(wire_amount(cc), "1.0000000000");
        let cc = usd_to_cc(d("0.1"), d("3"), "dvp").unwrap();
        assert_eq!(wire_amount(cc), "0.0333333333");
        assert_eq!(wire_amount(cc), format!("{:.10}", cc), "same bytes as before");
        assert_eq!(wire_amount(Decimal::ZERO), "0.0000000000");
        let err = usd_to_cc(Decimal::MAX, d("0.0000000001"), "dvp").unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
        let err = usd_to_cc(Decimal::ONE, d("0.0000000000001"), "alloc").unwrap_err();
        assert!(err.to_string().contains("sanity cap"), "{err}");
        let at_cap = usd_to_cc(Decimal::from(MAX_FEE_CC), Decimal::ONE, "dvp").unwrap();
        assert_eq!(wire_amount(at_cap), "1000000000000.0000000000");
    }

    #[test]
    fn an_unknown_settle_outcome_is_recognised_through_context() {
        let err = anyhow::Error::from(SettleOutcomeUnknown("x".to_string())).context("round 3");
        assert_eq!(err.downcast_ref::<SettleOutcomeUnknown>(), Some(&SettleOutcomeUnknown("x".to_string())));
        assert!(anyhow!("Transaction failed").downcast_ref::<SettleOutcomeUnknown>().is_none());
    }

    struct Script {
        reads: VecDeque<Result<GetSettlementStatusResponse>>,
        calls: u32,
    }

    impl Script {
        fn new(reads: Vec<Result<GetSettlementStatusResponse>>) -> Self {
            Self { reads: reads.into(), calls: 0 }
        }
    }

    impl StatusSource for Script {
        async fn settlement_status(&mut self, _: &str) -> Result<GetSettlementStatusResponse> {
            self.calls += 1;
            match self.reads.pop_front() {
                Some(read) => read,
                None => std::future::pending().await,
            }
        }
    }

    fn at(stage: SettlementStage) -> Result<GetSettlementStatusResponse> {
        Ok(GetSettlementStatusResponse { stage: stage as i32, ..Default::default() })
    }

    fn accept_step(status: DvpStepStatusEnum, stage: SettlementStage) -> Result<GetSettlementStatusResponse> {
        let step = DvpStepStatus { status: status as i32, ..Default::default() };
        Ok(GetSettlementStatusResponse { stage: stage as i32, dvp_accept: Some(step), ..Default::default() })
    }

    #[tokio::test]
    async fn only_proof_of_the_accept_counts_as_a_fill() {
        let mut s = Script::new(vec![at(SettlementStage::DvpProposalCreated), Err(anyhow!("down")), at(SettlementStage::DvpAccepted)]);
        assert_eq!(reconcile_settle(&mut s, "p1", 6, Duration::ZERO).await, SettleCheck::Committed(7));
        assert_eq!(s.calls, 3);

        let mut s = Script::new(vec![at(SettlementStage::DvpProposalCreated), at(SettlementStage::DvpProposalCreated)]);
        let SettleCheck::NotEstablished(why) = reconcile_settle(&mut s, "p1", 2, Duration::ZERO).await else {
            panic!("no proof, no fill");
        };
        assert!(why.contains("stage 5"), "{why}");

        let mut s = Script::new(vec![at(SettlementStage::Cancelled), at(SettlementStage::Settled)]);
        let check = reconcile_settle(&mut s, "p1", 6, Duration::ZERO).await;
        assert_eq!(check, SettleCheck::Ended(13), "an ended settlement is not a fill");
        assert_eq!(s.calls, 1);

        let lagging = accept_step(DvpStepStatusEnum::DvpStepStatusConfirmed, SettlementStage::DvpProposalCreated);
        let mut s = Script::new(vec![lagging]);
        assert_eq!(reconcile_settle(&mut s, "p1", 1, Duration::ZERO).await, SettleCheck::Committed(5));

        let failed = accept_step(DvpStepStatusEnum::DvpStepStatusCompleted, SettlementStage::Failed);
        let mut s = Script::new(vec![failed]);
        assert_eq!(reconcile_settle(&mut s, "p1", 1, Duration::ZERO).await, SettleCheck::Ended(12));
    }

    // A commit proven only by reconciliation used to be counted without debiting its fees
    #[tokio::test]
    async fn a_reconciled_commit_debits_its_fees_once() {
        let unknown = SettleOutcomeUnknown("lost".to_string());
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let settled = after_reconcile(&settler, "p1", &unknown, SettleCheck::Committed(7)).unwrap();
        assert!(settled.contains("stage 7"), "{settled}");
        assert_eq!(settler.fee_debits.started(), 1);

        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let reason = after_reconcile(&settler, "p1", &unknown, SettleCheck::NotEstablished("x".into())).unwrap_err();
        assert!(reason.contains("check the ledger"), "{reason}");
        let reason = after_reconcile(&settler, "p1", &unknown, SettleCheck::Ended(13)).unwrap_err();
        assert!(reason.contains("(settlement ended at stage 13)"), "{reason}");
        assert_eq!(settler.fee_debits.started(), 0);
    }

    fn saved_fill(unresolved: Option<UnresolvedRound>) -> SavedFillState {
        fill_snapshot(&params(), 4.0, 6.0, 3, unresolved)
    }

    fn round_of(kind: UnresolvedKind, id: &str) -> UnresolvedRound {
        UnresolvedRound { kind, id: id.to_string(), qty: 5.0 }
    }

    // A restart used to resume over a round whose settle may have committed, buying it twice
    #[test]
    fn a_marked_round_is_never_resumed() {
        assert_eq!(restore_plan(None, &params()), RestorePlan::Fresh);
        let resume = RestorePlan::Resume { remaining: 6.0, filled: 4.0, round: 3 };
        assert_eq!(restore_plan(Some(&saved_fill(None)), &params()), resume);

        let v1 = saved_fill(Some(round_of(UnresolvedKind::V1, "p-3")));
        let resolve = RestorePlan::Resolve { proposal_id: "p-3".to_string(), qty: 5.0, remaining: 6.0, filled: 4.0, round: 3 };
        assert_eq!(restore_plan(Some(&v1), &params()), resolve);

        let atomic = saved_fill(Some(round_of(UnresolvedKind::Atomic, "q-3")));
        let RestorePlan::Refuse(why) = restore_plan(Some(&atomic), &params()) else {
            panic!("an atomic round cannot be resolved from here");
        };
        assert!(why.contains("atomic quote q-3 (5 base) of round 3"), "{why}");

        // A different amount starts a new order, the operator's way past the marker
        let other = FillParams { total_amount: 11.0, ..params() };
        assert_eq!(restore_plan(Some(&v1), &other), RestorePlan::Fresh);
        let done = SavedFillState { remaining: 0.0, ..saved_fill(None) };
        assert_eq!(restore_plan(Some(&done), &params()), RestorePlan::Fresh);
    }

    // The refusal used to suggest removing the whole fill state, which re-buys what was filled
    #[test]
    fn a_refused_round_names_the_progress_and_never_drops_it() {
        let atomic = saved_fill(Some(round_of(UnresolvedKind::Atomic, "q-3")));
        let RestorePlan::Refuse(why) = restore_plan(Some(&atomic), &params()) else {
            panic!("an atomic round is refused");
        };
        assert!(why.contains("filled 4.000000 of 10.000000 with 6.000000 remaining"), "{why}");
        assert!(why.contains("set fill_state.unresolved to null") && why.contains("--amount 1.000000"), "{why}");
        assert!(!why.contains("remove fill_state") && !why.contains("processing fees"), "{why}");

        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let unknown = SettleCheck::NotEstablished("stage 5".into());
        let err = resolve_saved_round(&settler, "p-3", 5.0, unknown, (4.0, 10.0, 6.0)).unwrap_err();
        let why = format!("{err:#}");
        assert!(why.contains("6.000000 remaining") && why.contains("reads the settlement status again"), "{why}");
        assert!(!why.contains("remove fill_state"), "{why}");

        // A round that completes the order is saved as complete, so the same command later starts afresh
        let last = unknown_round_help(UnresolvedKind::Atomic, 9.0, 10.0, 1.0, 1.0);
        assert!(last.contains("the order is complete") && !last.contains("--amount"), "{last}");
        assert!(last.contains("set fill_state.remaining to 0") && last.contains("set fill_state.unresolved to null"), "{last}");
        let edited = SavedFillState { unresolved: None, filled_total: 10.0, remaining: 0.0, ..atomic };
        assert_eq!(restore_plan(Some(&edited), &params()), RestorePlan::Fresh);
    }

    // The V1 guidance used to have the state edited, which skips the round's processing fees
    #[test]
    fn a_v1_round_is_resolved_by_rerunning_unchanged() {
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let unknown = SettleCheck::NotEstablished("stage 5".into());
        let err = resolve_saved_round(&settler, "p-3", 5.0, unknown, (4.0, 10.0, 6.0)).unwrap_err();
        let why = format!("{err:#}");
        assert!(why.contains("rerun this command unchanged") && why.contains("two processing fees"), "{why}");
        assert!(!why.contains("add 5 to fill_state.filled_total") && !why.contains("--amount 1.000000"), "{why}");

        let last = unknown_round_help(UnresolvedKind::V1, 9.0, 10.0, 1.0, 1.0);
        assert!(!last.contains("do not rerun") && last.contains("buys nothing more"), "{last}");
        assert!(last.contains("rerun this command unchanged") && last.contains("processing fees"), "{last}");
    }

    /// Removes its directory when dropped, also when a test fails.
    struct ScratchDir(PathBuf);

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A state file path in a fresh directory that lives as long as the guard.
    fn scratch_state_file(name: &str) -> (ScratchDir, PathBuf) {
        let unique = clock::uuid_v7().unwrap();
        let dir = std::env::temp_dir().join(format!("cloud-agent-fill-{}-{name}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-state.json");
        (ScratchDir(dir), path)
    }

    #[test]
    fn a_scratch_directory_is_removed_with_its_guard() {
        let (guard, path) = scratch_state_file("guard");
        std::fs::write(&path, b"{}").unwrap();
        let dir = guard.0.clone();
        drop(guard);
        assert!(!dir.exists());
    }

    // The abort used to leave no trace in the state file, so a restart quoted the round again
    #[tokio::test(start_paused = true)]
    async fn an_unproven_settle_saves_its_round_before_the_abort() {
        let (_abort, path) = scratch_state_file("abort");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let unknown = SettleOutcomeUnknown("lost".to_string());
        let reads = (0..SETTLE_RECONCILE_POLLS).map(|_| at(SettlementStage::DvpProposalCreated)).collect();
        let mut s = Script::new(reads);
        let marked = saved_fill(Some(round_of(UnresolvedKind::V1, "p-3")));
        let reason = settle_unknown(&mut s, &settler, "p-3", &unknown, &sink, marked).await.unwrap_err();
        assert!(reason.contains("p-3") && reason.contains("stage 5"), "{reason}");

        let saved = agent_logic::state::load_state(&path).unwrap().fill_state.unwrap();
        assert_eq!(saved.unresolved, Some(round_of(UnresolvedKind::V1, "p-3")));
        assert_eq!((saved.filled_total, saved.remaining, saved.round), (4.0, 6.0, 3));
        assert!(matches!(restore_plan(Some(&saved), &params()), RestorePlan::Resolve { .. }));

        // A proven commit is a fill; the loop saves it without a marker
        let (_commit, path) = scratch_state_file("commit");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let mut s = Script::new(vec![at(SettlementStage::DvpAccepted)]);
        let marked = saved_fill(Some(round_of(UnresolvedKind::V1, "p-4")));
        assert!(settle_unknown(&mut s, &settler, "p-4", &unknown, &sink, marked).await.is_ok());
        assert!(!path.exists());
    }

    fn unresolved_in(path: &Path) -> Option<UnresolvedRound> {
        agent_logic::state::load_state(path).expect("a saved state").fill_state.expect("a fill state").unresolved
    }

    // A round is marked while its settle runs, so a restart after a kill never buys it again
    #[tokio::test(start_paused = true)]
    async fn a_round_stays_marked_while_its_settle_runs() {
        let (_dir, path) = scratch_state_file("marked");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let v1 = round_of(UnresolvedKind::V1, "p-3");
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, v1.clone());

        let pending = v1_settle(&mark, std::future::pending::<Result<u8>>);
        assert!(tokio::time::timeout(Duration::from_secs(3600), pending).await.is_err());
        assert_eq!(unresolved_in(&path), Some(v1.clone()), "marked while the settle runs");
        let saved = agent_logic::state::load_state(&path).unwrap().fill_state.unwrap();
        assert!(matches!(restore_plan(Some(&saved), &params()), RestorePlan::Resolve { .. }));

        let failed = v1_settle(&mark, || async { Err::<u8, _>(anyhow!("Transaction failed: rejected")) });
        assert!(failed.await.is_err());
        assert_eq!(unresolved_in(&path), None, "a definite failure clears it");

        let lost = anyhow::Error::from(SettleOutcomeUnknown("lost".to_string()));
        assert!(v1_settle(&mark, || async { Err::<u8, _>(lost) }).await.is_err());
        assert_eq!(unresolved_in(&path), Some(v1.clone()), "an unknown outcome keeps it");
        assert!(v1_settle(&mark, || async { Ok::<u8, anyhow::Error>(1) }).await.is_ok());
        assert_eq!(unresolved_in(&path), Some(v1), "a fill keeps it until the loop saves the fill");
    }

    // An atomic round is marked from its submit on; only an outcome without a commit clears it
    #[tokio::test(start_paused = true)]
    async fn an_atomic_round_is_cleared_only_without_a_commit() {
        let (_dir, path) = scratch_state_file("atomic");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let atomic = round_of(UnresolvedKind::Atomic, "q-3");
        let outcomes = [
            (Ok(SwapOutcome::Requote { reason: "rejected".into() }), true),
            (Ok(SwapOutcome::DryRun), true),
            (Err(anyhow!("precheck")), true),
            (Ok(SwapOutcome::Abort { reason: "unknown".into() }), false),
            (Ok(SwapOutcome::Filled(AtomicFill { update_id: "u".into(), filled_base: 5.0 })), false),
        ];
        for (outcome, cleared) in outcomes {
            let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, atomic.clone());
            let submitted = async {
                assert!(mark.set());
                assert_eq!(unresolved_in(&path), Some(atomic.clone()), "marked at the submit");
                outcome
            };
            let _ = cleared_unless_committed(&mark, submitted, atomic_nothing_committed).await;
            let expected = if cleared { None } else { Some(atomic.clone()) };
            assert_eq!(unresolved_in(&path), expected);
        }

        // A re-quote before the submit leaves the state file as it was
        let (_untouched, path) = scratch_state_file("untouched");
        let mark = RoundMark::new(FillStateSink { path: Some(&path), party_id: "party::1" }, &params(), (4.0, 6.0), 3, atomic);
        let early = async { Ok(SwapOutcome::Requote { reason: "pre-check".into() }) };
        let _ = cleared_unless_committed(&mark, early, atomic_nothing_committed).await;
        assert!(!path.exists());
    }

    // The loop's V1 step marks its round before the multicall can commit
    #[tokio::test(start_paused = true)]
    async fn the_v1_step_marks_its_round_before_the_settle() {
        let (_dir, path) = scratch_state_file("v1-step");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let v1 = round_of(UnresolvedKind::V1, "p-3");
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, v1.clone());
        let step = v1_settle(&mark, std::future::pending::<Result<StepResult>>);
        assert!(tokio::time::timeout(Duration::from_secs(3600), step).await.is_err());
        assert_eq!(unresolved_in(&path), Some(v1));
    }

    /// Calls the submit hook, then never answers; without a submit it asks for a re-quote.
    #[derive(Default)]
    struct FakeSwap {
        submits: bool,
        submitted: AtomicBool,
    }

    impl AtomicSettle for FakeSwap {
        async fn settle_with_hook(
            &self,
            _: &AtomicQuoteEnvelope,
            _: &AtomicQuoteInfo,
            _: FillDirection,
            _: usize,
            before_submit: impl FnOnce() -> bool,
        ) -> Result<SwapOutcome> {
            if !self.submits {
                return Ok(SwapOutcome::Requote { reason: "pre-check".into() });
            }
            if !before_submit() {
                return Err(MarkerNotSaved("quote q-3".into()).into());
            }
            self.submitted.store(true, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    // The loop's atomic step marks its round at the submit, and not before
    #[tokio::test(start_paused = true)]
    async fn the_atomic_step_marks_its_round_at_the_submit() {
        let (_dir, path) = scratch_state_file("atomic-step");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let atomic = round_of(UnresolvedKind::Atomic, "q-3");
        let (envelope, best) = (AtomicQuoteEnvelope::default(), AtomicQuoteInfo::default());
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, atomic.clone());
        let swap = FakeSwap { submits: true, ..Default::default() };
        let step = atomic_settle(&swap, &mark, &envelope, &best, FillDirection::Buy, 10);
        assert!(tokio::time::timeout(Duration::from_secs(3600), step).await.is_err());
        assert_eq!(unresolved_in(&path), Some(atomic.clone()));

        let (_untouched, path) = scratch_state_file("atomic-step-early");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, atomic);
        let early = atomic_settle(&FakeSwap::default(), &mark, &envelope, &best, FillDirection::Buy, 10).await;
        assert!(matches!(early, Ok(SwapOutcome::Requote { .. })));
        assert!(!path.exists(), "a re-quote before the submit writes nothing");
    }

    #[tokio::test]
    async fn a_saved_round_counts_only_with_proof() {
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let progress = (4.0, 10.0, 6.0);
        assert_eq!(resolve_saved_round(&settler, "p-3", 5.0, SettleCheck::Committed(7), progress).unwrap(), 5.0);
        assert_eq!(settler.fee_debits.started(), 1, "a proven commit's fees are debited");
        assert_eq!(resolve_saved_round(&settler, "p-3", 5.0, SettleCheck::Ended(13), progress).unwrap(), 0.0);
        let unknown = SettleCheck::NotEstablished("stage 5".into());
        let err = resolve_saved_round(&settler, "p-3", 5.0, unknown, progress).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("proposal p-3 (5 base)") && text.contains("still unknown (stage 5)"), "{text}");
        assert_eq!(settler.fee_debits.started(), 1);
    }

    // A restart used to read the saved round once, so one in progress was refused
    #[tokio::test(start_paused = true)]
    async fn a_restart_reads_the_saved_round_until_it_is_proven() {
        let (_dir, path) = scratch_state_file("resume");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        assert!(sink.save(saved_fill(Some(round_of(UnresolvedKind::V1, "p-3")))));
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let reads = vec![at(SettlementStage::DvpProposalCreated), Err(anyhow!("down")), at(SettlementStage::DvpAccepted)];
        let mut s = Script::new(reads);
        let resumed = resume_saved_v1_round(&mut s, &settler, &sink, &params(), "p-3", 5.0, (6.0, 4.0, 3)).await;
        assert_eq!(resumed.unwrap(), (1.0, 9.0, 3));
        assert_eq!(s.calls, 3);
        assert_eq!(settler.fee_debits.started(), 1);
        let saved = agent_logic::state::load_state(&path).unwrap().fill_state.unwrap();
        assert_eq!((saved.unresolved, saved.filled_total, saved.remaining), (None, 9.0, 1.0));

        // Unproven after every read: no start, and the marker stays for the next run
        let (_dir, path) = scratch_state_file("resume-unproven");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        assert!(sink.save(saved_fill(Some(round_of(UnresolvedKind::V1, "p-3")))));
        let before = std::fs::read(&path).unwrap();
        let reads = (0..SETTLE_RECONCILE_POLLS).map(|_| at(SettlementStage::DvpProposalCreated)).collect();
        let mut s = Script::new(reads);
        let resumed = resume_saved_v1_round(&mut s, &settler, &sink, &params(), "p-3", 5.0, (6.0, 4.0, 3)).await;
        let err = format!("{:#}", resumed.unwrap_err());
        assert!(err.contains("still unknown"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing is saved");
        assert_eq!(unresolved_in(&path), Some(round_of(UnresolvedKind::V1, "p-3")));
    }

    /// A state file whose next save fails, also for root: the save's temporary path is a directory.
    fn blocked_state_file(name: &str) -> (ScratchDir, PathBuf, Vec<u8>) {
        let (dir, path) = scratch_state_file(name);
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        assert!(sink.save(saved_fill(None)));
        let before = std::fs::read(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        (dir, path, before)
    }

    // A marker that could not be saved used to be logged only, and the multicall ran without it
    #[tokio::test]
    async fn an_unsaved_v1_marker_never_starts_the_settle() {
        let (_dir, path, before) = blocked_state_file("v1-unsaved");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, round_of(UnresolvedKind::V1, "p-3"));
        let (called, polled) = (AtomicBool::new(false), Arc::new(AtomicBool::new(false)));
        let p = Arc::clone(&polled);
        let accept = || {
            called.store(true, Ordering::SeqCst);
            async move {
                p.store(true, Ordering::SeqCst);
                Ok::<u8, anyhow::Error>(1)
            }
        };
        let err = v1_settle(&mark, accept).await.unwrap_err();
        let unsaved = err.downcast_ref::<MarkerNotSaved>().expect("a marker error");
        assert_eq!(unsaved, &MarkerNotSaved("round 3 (p-3)".to_string()));
        assert!(!called.load(Ordering::SeqCst) && !polled.load(Ordering::SeqCst), "the settle never started");
        mark.clear();
        assert_eq!(std::fs::read(&path).unwrap(), before, "the saved file is unchanged");
    }

    // An unsaved V1 marker or an unanswered prompt used to be classed as a failure to quote again after
    #[test]
    fn a_v1_failure_is_a_stop_only_for_an_unsaved_marker_or_an_unanswered_prompt() {
        let unsaved = anyhow::Error::from(MarkerNotSaved("round 3 (p-3)".into()));
        assert!(matches!(v1_failure(&unsaved), V1Failure::Stop(reason) if reason.contains("round 3 (p-3)")));
        let unanswered = anyhow::Error::from(Unanswered("no answer within 120s — declined: Allocate (p-3)".into()));
        assert!(matches!(v1_failure(&unanswered), V1Failure::Stop(reason) if reason.contains("no answer within")));
        let lost = anyhow::Error::from(SettleOutcomeUnknown("lost".into())).context("round 3");
        assert!(matches!(v1_failure(&lost), V1Failure::Unknown(_)));
        assert!(matches!(v1_failure(&anyhow!("Transaction failed: rejected")), V1Failure::Retry));
        assert!(matches!(v1_failure(&anyhow!("User declined: Allocate (p-3)")), V1Failure::Retry));
    }

    // How a V1 round with a failed settle ends: re-quote, a reconciled fill, or a stop
    #[tokio::test(start_paused = true)]
    async fn a_failed_v1_round_ends_in_a_retry_a_fill_or_an_abort() {
        let settler = MulticallSettler::for_tests(&crate::test_util::refused_url());
        let marked = || saved_fill(Some(round_of(UnresolvedKind::V1, "p-3")));
        let (_dir, path) = scratch_state_file("v1-end");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        assert!(sink.save(saved_fill(None)));
        let before = std::fs::read(&path).unwrap();
        let mut s = Script::new(vec![at(SettlementStage::DvpAccepted)]);

        // Nothing submitted, yet the loop stops: no status read and no save
        let unsaved = anyhow::Error::from(MarkerNotSaved("round 3 (p-3)".into()));
        let end = v1_round_end(&unsaved, &mut s, &settler, "p-3", &sink, marked(), 3).await;
        assert!(matches!(&end, V1End::Abort(reason) if reason.contains("could not be saved")), "{end:?}");
        let unanswered = anyhow::Error::from(Unanswered("no answer within 120s — declined: Allocate (p-3)".into()));
        let end = v1_round_end(&unanswered, &mut s, &settler, "p-3", &sink, marked(), 3).await;
        assert!(matches!(&end, V1End::Abort(reason) if reason.contains("no answer within")), "{end:?}");

        let rejected = anyhow!("Transaction failed: rejected");
        assert_eq!(v1_round_end(&rejected, &mut s, &settler, "p-3", &sink, marked(), 3).await, V1End::Retry);
        let refused = anyhow!("User declined: Allocate (p-3)");
        assert_eq!(v1_round_end(&refused, &mut s, &settler, "p-3", &sink, marked(), 3).await, V1End::Retry);
        assert_eq!(s.calls, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // A possible commit is read first: proven, it is a fill the loop saves itself
        let lost = anyhow::Error::from(SettleOutcomeUnknown("lost".into()));
        let end = v1_round_end(&lost, &mut s, &settler, "p-3", &sink, marked(), 3).await;
        assert!(matches!(&end, V1End::Settled(how) if how.contains("stage 7")), "{end:?}");
        assert_eq!((s.calls, settler.fee_debits.started()), (1, 1));
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // Unproven, the loop stops with the round's marker saved
        let reads = (0..SETTLE_RECONCILE_POLLS).map(|_| at(SettlementStage::DvpProposalCreated)).collect();
        let mut s = Script::new(reads);
        let end = v1_round_end(&lost, &mut s, &settler, "p-3", &sink, marked(), 3).await;
        assert!(matches!(&end, V1End::Abort(reason) if reason.contains("check the ledger")), "{end:?}");
        assert_eq!(unresolved_in(&path), Some(round_of(UnresolvedKind::V1, "p-3")));
    }

    // The same for an atomic round: no submit, no marker, and an abort instead of a re-quote
    #[tokio::test(start_paused = true)]
    async fn an_unsaved_atomic_marker_ends_the_round_in_an_abort_without_a_submit() {
        let (_dir, path, before) = blocked_state_file("atomic-unsaved");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let mark = RoundMark::new(sink, &params(), (4.0, 6.0), 3, round_of(UnresolvedKind::Atomic, "q-3"));
        let swap = FakeSwap { submits: true, ..Default::default() };
        let (envelope, best) = (AtomicQuoteEnvelope::default(), AtomicQuoteInfo::default());
        let settle = atomic_settle(&swap, &mark, &envelope, &best, FillDirection::Buy, 10);
        let settled = tokio::time::timeout(Duration::from_secs(3600), settle).await.expect("the round ends");
        assert!(!swap.submitted.load(Ordering::SeqCst), "nothing was submitted");
        let AtomicRoundResult::Abort { reason, marker: None } = round_result(settled, 3, "q-3", 5.0) else {
            panic!("an abort without a marker");
        };
        assert!(reason.contains("could not be saved"), "{reason}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the saved file is unchanged");

        let other = round_result(Err(anyhow!("precheck")), 3, "q-3", 5.0);
        assert!(matches!(other, AtomicRoundResult::Retry), "any other error re-quotes");
    }

    // An unanswered atomic prompt used to be logged as a settle error, and the next quote accepted
    #[test]
    fn an_unanswered_atomic_prompt_ends_the_round_in_an_abort() {
        let unanswered = anyhow::Error::from(Unanswered("no answer within 120s — declined: Atomic DVP settle (q-3)".into()));
        let ended = round_result(Err(unanswered), 3, "q-3", 5.0);
        let stopped = matches!(&ended, AtomicRoundResult::Abort { reason, marker: None } if reason.contains("no answer within"));
        assert!(stopped, "{ended:?}");
        let refused = round_result(Err(anyhow!("User declined: Atomic DVP settle (q-3)")), 3, "q-3", 5.0);
        assert!(matches!(refused, AtomicRoundResult::Retry), "a refusal re-quotes");
        let lost = round_result(Ok(SwapOutcome::Abort { reason: "lost".into() }), 3, "q-3", 5.0);
        let marker = Some(round_of(UnresolvedKind::Atomic, "q-3"));
        assert!(matches!(&lost, AtomicRoundResult::Abort { marker: m, .. } if *m == marker), "{lost:?}");
    }

    // A state file that did not parse used to read as none, and the whole order was bought again
    #[test]
    fn an_unreadable_or_foreign_state_file_is_never_replaced() {
        let (_dir, path) = scratch_state_file("strict");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        assert!(sink.save(saved_fill(Some(round_of(UnresolvedKind::Atomic, "q-3")))));
        let saved = std::fs::read_to_string(&path).unwrap();

        // Deleting the marker leaves a trailing comma
        let start = saved.find("\"unresolved\"").unwrap();
        let end = start + saved[start..].find('}').unwrap() + 1;
        let broken = format!("{}{}", &saved[..start], &saved[end..]);
        std::fs::write(&path, &broken).unwrap();
        let err = format!("{:#}", load_fill_state(&path, "party::1").err().unwrap());
        assert!(err.contains("agent-state.json") && !err.contains("CC-USDC"), "{err}");
        assert!(!sink.save(saved_fill(None)), "a file that does not parse is not replaced");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);

        std::fs::write(&path, &saved).unwrap();
        let err = format!("{:#}", load_fill_state(&path, "party::2").err().unwrap());
        assert!(err.contains("belongs to another party"), "{err}");
        let other = FillStateSink { path: Some(&path), party_id: "party::2" };
        assert!(!other.save(saved_fill(None)), "another party's file is not replaced");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
    }

    // The refusal's edit, the marker set to null, resumes the order where it stopped
    #[test]
    fn a_marker_set_to_null_resumes_the_order() {
        let (_dir, path) = scratch_state_file("null");
        let sink = FillStateSink { path: Some(&path), party_id: "party::1" };
        let atomic = saved_fill(Some(round_of(UnresolvedKind::Atomic, "q-3")));
        assert!(sink.save(atomic.clone()));
        let RestorePlan::Refuse(why) = restore_plan(Some(&atomic), &params()) else {
            panic!("an atomic round is refused");
        };
        assert!(why.contains("set fill_state.unresolved to null"), "{why}");

        let saved = std::fs::read_to_string(&path).unwrap();
        let start = saved.find("\"unresolved\": {").unwrap();
        let end = start + saved[start..].find('}').unwrap() + 1;
        std::fs::write(&path, format!("{}\"unresolved\": null{}", &saved[..start], &saved[end..])).unwrap();
        let restored = load_fill_state(&path, "party::1").unwrap();
        let resume = RestorePlan::Resume { remaining: 6.0, filled: 4.0, round: 3 };
        assert_eq!(restore_plan(restored.as_ref(), &params()), resume);
    }

    // A state file that cannot be written used to be found only at the first marker, after a quote was accepted
    #[tokio::test]
    async fn an_unwritable_state_file_stops_the_loop_before_any_quote() {
        let (dir, _) = scratch_state_file("probe");
        let blocker = dir.0.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = crate::test_util::refused_url();
        let settler = Arc::new(MulticallSettler::for_tests(&crate::test_util::refused_url()));
        let run = run_fill_loop(config, settler, params(), None, None, Some(blocker.join("agent-state.json")));
        let err = tokio::time::timeout(Duration::from_secs(20), run).await.expect("bounded").unwrap_err();
        assert!(format!("{err:#}").contains("cannot save the fill state"), "{err:#}");
    }

    /// Records the token in force at each read.
    struct TokenLog {
        token: String,
        seen: Vec<String>,
        stage: SettlementStage,
    }

    impl StatusSource for TokenLog {
        async fn settlement_status(&mut self, _: &str) -> Result<GetSettlementStatusResponse> {
            self.seen.push(self.token.clone());
            at(self.stage)
        }
    }

    impl TokenSource for TokenLog {
        fn set_token(&mut self, token: String) {
            self.token = token;
        }
    }

    impl ProposalSource for TokenLog {
        async fn settlement_proposal(&mut self, _: &str) -> Result<Option<SettlementProposalMessage>> {
            self.seen.push(self.token.clone());
            Ok(Some(SettlementProposalMessage {
                dvp_processing_fee_buyer: "0.1".into(),
                allocation_processing_fee_buyer: "0.2".into(),
                ..Default::default()
            }))
        }
    }

    // The fee lookup used to read with the loop's first token, expired after token_ttl_secs
    #[tokio::test]
    async fn every_fee_lookup_gets_a_fresh_token() {
        let mut log = TokenLog { token: "t0".to_string(), seen: Vec::new(), stage: SettlementStage::DvpProposalCreated };
        let mut minted = 0u32;
        let mut mint = || {
            minted += 1;
            Ok(format!("t{minted}"))
        };
        for _ in 0..2 {
            let fees = proposal_fees_usd(&mut log, &mut mint, "p1", FillDirection::Buy).await.unwrap();
            assert_eq!(fees, ("0.1".to_string(), "0.2".to_string()));
        }
        assert_eq!(log.seen, ["t1", "t2"]);

        let mut log = TokenLog { token: "t0".to_string(), seen: Vec::new(), stage: SettlementStage::DvpProposalCreated };
        let failing = || Err(anyhow!("no key"));
        let err = proposal_fees_usd(&mut log, failing, "p1", FillDirection::Buy).await.unwrap_err();
        assert!(format!("{err:#}").contains("no key"), "{err:#}");
        assert!(log.seen.is_empty(), "nothing is read without a fresh token");
    }

    // The reconcile reads used to carry the loop's first token, long expired by then
    #[tokio::test]
    async fn every_status_read_gets_a_fresh_token() {
        let mut log = TokenLog { token: "t0".to_string(), seen: Vec::new(), stage: SettlementStage::DvpProposalCreated };
        let mut minted = 0u32;
        let mint = || {
            minted += 1;
            Ok(format!("t{minted}"))
        };
        let check = reconcile_settle(&mut Reauth { inner: &mut log, mint }, "p1", 3, Duration::ZERO).await;
        assert!(matches!(check, SettleCheck::NotEstablished(_)));
        assert_eq!(log.seen, ["t1", "t2", "t3"]);

        let mut log = TokenLog { token: "t0".to_string(), seen: Vec::new(), stage: SettlementStage::DvpAccepted };
        let failing = || Err(anyhow!("no key"));
        let SettleCheck::NotEstablished(why) =
            reconcile_settle(&mut Reauth { inner: &mut log, mint: failing }, "p1", 2, Duration::ZERO).await
        else {
            panic!("a failed mint proves nothing");
        };
        assert!(why.contains("no key"), "{why}");
        assert!(log.seen.is_empty(), "nothing is read without a fresh token");
    }

    // A failure after an accept used to start the next round at once, accepting quote after quote
    #[tokio::test(start_paused = true)]
    async fn a_failed_post_accept_step_waits_an_interval() {
        let (notify, running) = (Notify::new(), AtomicBool::new(false));
        let pause = Duration::from_secs(60);
        let started = tokio::time::Instant::now();
        assert_eq!(post_accept_step(Ok::<_, String>(7), 1, pause, &notify, &running).await, Some(7));
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(post_accept_step(Err::<u8, _>("no proposal".to_string()), 1, pause, &notify, &running).await, None);
        assert!(started.elapsed() >= pause, "{:?}", started.elapsed());

        let stopping = AtomicBool::new(true);
        let before = tokio::time::Instant::now();
        assert_eq!(post_accept_step(Err::<u8, _>("x".to_string()), 1, pause, &notify, &stopping).await, None);
        assert_eq!(before.elapsed(), Duration::ZERO, "shutdown skips the pause");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_status_read_is_bounded() {
        let mut s = Script::new(Vec::new());
        let check = tokio::time::timeout(Duration::from_secs(3600), reconcile_settle(&mut s, "p1", 2, Duration::from_secs(1)))
            .await
            .expect("each read is bounded");
        let SettleCheck::NotEstablished(why) = check else {
            panic!("a silent status source proves nothing");
        };
        assert!(why.contains("took over"), "{why}");
        assert_eq!(s.calls, 2);
    }

    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    // Each monitor used to get its own 300s, so a few hung ones held the exit for many minutes
    #[tokio::test]
    async fn monitors_share_one_deadline_and_the_rest_are_aborted() {
        let mut monitors = JoinSet::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = SetOnDrop(dropped.clone());
        for _ in 0..3 {
            monitors.spawn(std::future::pending::<()>());
        }
        monitors.spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await
        });
        monitors.spawn(async {});
        let started = Instant::now();
        let deadline = clock::deadline_after(Duration::from_millis(100));
        let done = wait_for_monitors(&mut monitors, deadline, &Notify::new(), &AtomicBool::new(false)).await;
        assert_eq!(done, 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(dropped.load(Ordering::Relaxed), "the hung monitors are aborted");

        let mut monitors = JoinSet::new();
        monitors.spawn(std::future::pending::<()>());
        let stop = AtomicBool::new(true);
        let deadline = clock::deadline_after(Duration::from_secs(60));
        assert_eq!(wait_for_monitors(&mut monitors, deadline, &Notify::new(), &stop).await, 0);
    }

    // The lookup used to wait out the client's own (much longer) request timeout
    #[tokio::test]
    async fn the_dvp_proposal_lookup_is_bounded_by_its_deadline() {
        let ledger = FakeLedger::start(Fake::Stall).await;
        let key = agent_logic::secret::Secret::seal(&mut [7u8; 32]).unwrap();
        let mut client =
            DAppProviderClient::new(&ledger.url, "p", "agent", &key, 60, None, &[0u8; 32], Some(1), Some(30))
                .await
                .unwrap();
        let (notify, flag) = (Notify::new(), AtomicBool::new(false));
        let lookup = poll_dvp_proposal_cid(&mut client, "p1", Duration::from_millis(200), &notify, &flag);
        let err = tokio::time::timeout(Duration::from_secs(5), lookup)
            .await
            .expect("the lookup deadline bounds the call")
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        ledger.stop().await;
    }
}
