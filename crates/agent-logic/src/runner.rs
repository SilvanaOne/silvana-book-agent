//! Shared agent runner — event loop, shutdown, timers
//!
//! Both `orderbook-agent` and `orderbook-cloud-agent` call `run_agent()` with
//! their own `SettlementBackend` and `BalanceProvider` implementations.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::{Mutex, Notify};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{interval, Interval, MissedTickBehavior};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::{CancelOrderResponse, Order, SettlementUpdate};

use crate::client::OrderbookClient;
use crate::config::BaseConfig;
use crate::grid_task::{join_grid, spawn_grid_task, GridSnapshot, GridStats, GridTiming};
use crate::order_manager::OrderManager;
use crate::order_tracker::OrderTracker;
use crate::settlement::{
    SettlementBackend, SettlementExecutor, STREAM_BATCH_BUDGET, STREAM_BATCH_MAX,
};
use crate::shutdown::Shutdown;
use crate::state::{
    SavedAcceptedRfqTrade, SavedFillState, SavedQuotedTrade, SavedState, delete_state, load_state,
    prune_state, save_backup, save_state,
};

/// Bound on joining the grid task, or the order sweep after it exits, at
/// shutdown; covers an in-flight cancel and submit chain.
const GRID_STOP_TIMEOUT: Duration = Duration::from_secs(90);

/// Party-wide order fetches per sweep, confirming fetches included.
const PARTY_CANCEL_ROUNDS: usize = 5;

/// Pause before retrying a failed party-wide order fetch.
const PARTY_CANCEL_RETRY_PAUSE: Duration = Duration::from_millis(500);

/// Pause before the fetch that confirms an empty book.
const PARTY_CANCEL_CONFIRM_PAUSE: Duration = Duration::from_secs(1);

/// Bound on connecting the client that withdraws orders after the grid exits.
const SWEEP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Heartbeat warning while the grid task is gone and its orders were withdrawn.
const GRID_GONE_WARNING: &str = "Grid: task not running; orders were withdrawn";

/// Heartbeat warning while the grid task is gone and no sweep has finished yet.
const GRID_LIVE_WARNING: &str = "Grid: task not running; orders may still be live";

/// An idle grid whose last cycle ended longer ago than this is reported.
const GRID_IDLE_WARN: Duration = Duration::from_secs(30);

/// A grid cycle still running after this long is reported.
const GRID_BUSY_WARN: Duration = Duration::from_secs(120);

/// Result collection not run for this long is reported.
const COLLECT_OVERDUE: Duration = Duration::from_secs(10);

/// A loop arm body taking longer than this is reported.
const SLOW_ARM: Duration = Duration::from_secs(10);

/// Shortest settlement poll period; a zero period would make `interval` panic.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Longest settlement poll period, so timer resets stay far from `Instant` overflow.
const MAX_POLL_INTERVAL_SECS: u64 = 3_600;

/// What made the stream arm ready: queued updates, or the stream itself.
#[allow(clippy::large_enum_variant)]
enum StreamWake {
    Backlog,
    Next(Option<Result<SettlementUpdate, tonic::Status>>),
}

/// Settlement-loop activity between heartbeats, for the "Loop:" line.
struct LoopStats {
    started: Instant,
    stream_batches: u64,
    stream_updates: u64,
    stream_max: usize,
    poll_runs: u64,
    poll_max_ms: u128,
    poll_last: Option<Instant>,
    collect_runs: u64,
    collect_last: Option<Instant>,
}

impl LoopStats {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            stream_batches: 0,
            stream_updates: 0,
            stream_max: 0,
            poll_runs: 0,
            poll_max_ms: 0,
            poll_last: None,
            collect_runs: 0,
            collect_last: None,
        }
    }

    fn note_batch(&mut self, handled: usize) {
        self.stream_batches = self.stream_batches.saturating_add(1);
        self.stream_updates = self.stream_updates.saturating_add(u64::try_from(handled).unwrap_or(u64::MAX));
        self.stream_max = self.stream_max.max(handled);
    }

    fn note_poll(&mut self, took: Duration, now: Instant) {
        self.poll_runs = self.poll_runs.saturating_add(1);
        self.poll_max_ms = self.poll_max_ms.max(took.as_millis());
        self.poll_last = Some(now);
    }

    fn note_collect(&mut self, now: Instant) {
        self.collect_runs = self.collect_runs.saturating_add(1);
        self.collect_last = Some(now);
    }

    /// Heartbeat line; the interval counters restart from zero.
    fn take_line(&mut self, backlog: usize, held: usize, now: Instant) -> String {
        let line = format!(
            "Loop: stream {} batches/{} updates (max {}, backlog {backlog}), poll {} ({}, max {}ms), collect {} ({}), held {held}",
            self.stream_batches,
            self.stream_updates,
            self.stream_max,
            self.poll_runs,
            last_run(self.poll_last, now),
            self.poll_max_ms,
            self.collect_runs,
            last_run(self.collect_last, now),
        );
        self.stream_batches = 0;
        self.stream_updates = 0;
        self.stream_max = 0;
        self.poll_runs = 0;
        self.poll_max_ms = 0;
        self.collect_runs = 0;
        line
    }

    fn warn_overdue(&self, poll_interval: Duration, now: Instant) {
        let poll_age = now.saturating_duration_since(self.poll_last.unwrap_or(self.started));
        if poll_age > poll_interval.saturating_mul(3) {
            warn!("settlement poll overdue: {}s since last run", poll_age.as_secs());
        }
        let collect_age = now.saturating_duration_since(self.collect_last.unwrap_or(self.started));
        if collect_age > COLLECT_OVERDUE {
            warn!("result-collect overdue: {}s since last run", collect_age.as_secs());
        }
    }
}

fn last_run(last: Option<Instant>, now: Instant) -> String {
    match last {
        Some(t) => format!("last {}s ago", now.saturating_duration_since(t).as_secs()),
        None => "last never".to_string(),
    }
}

fn warn_if_slow(arm: &str, started: Instant) {
    let took = started.elapsed();
    if took > SLOW_ARM {
        warn!("{} arm body took {}ms", arm, took.as_millis());
    }
}

/// End a timer arm: report a slow body, then restart the full period so an
/// overrun cannot make the arm ready again at once.
fn finish_arm(timer: &mut Interval, arm: &str, started: Instant) {
    warn_if_slow(arm, started);
    timer.reset();
}

/// Grid heartbeat as (info line, warning). A stopped grid task gives only a
/// warning, saying whether its orders were withdrawn.
fn grid_heartbeat(
    g: &GridSnapshot,
    running: bool,
    withdrawn: bool,
    now: Instant,
) -> (Option<String>, Option<String>) {
    if !running {
        let warning = if withdrawn { GRID_GONE_WARNING } else { GRID_LIVE_WARNING };
        return (None, Some(warning.to_string()));
    }
    let ago = match g.last_end {
        Some(t) => format!("{}s ago", now.saturating_duration_since(t).as_secs()),
        None => "never".to_string(),
    };
    let failed = if g.failed > 0 { format!(", failed {}", g.failed) } else { String::new() };
    let line = format!(
        "Grid: {} cycles, last {}ms ({}, max {}ms), markets {} (deferred {}), parked {}, held {}, refreshed {}, placed {}, cancelled {}{}",
        g.cycles, g.last_ms, ago, g.max_ms, g.markets, g.deferred, g.parked, g.held, g.refreshed, g.placed,
        g.cancelled, failed
    );
    let warning = match g.busy_since {
        Some(t) => {
            let busy = now.saturating_duration_since(t);
            (busy > GRID_BUSY_WARN).then(|| format!("grid cycle running for {}s", busy.as_secs()))
        }
        None => {
            let idle = g.idle_for(now);
            (idle > GRID_IDLE_WARN).then(|| format!("order-update tick overdue: {}s since last run", idle.as_secs()))
        }
    };
    (Some(line), warning)
}

/// Grid heartbeat line, plus a warning when the grid task looks stuck or is gone.
fn log_grid_heartbeat(stats: &GridStats, running: bool, withdrawn: bool) {
    let (line, warning) = grid_heartbeat(&stats.take_interval(), running, withdrawn, Instant::now());
    if let Some(line) = line {
        info!("{}", line);
    }
    if let Some(warning) = warning {
        warn!("{}", warning);
    }
}

/// Settlement poll period from config, clamped to [MIN_POLL_INTERVAL, MAX_POLL_INTERVAL_SECS].
fn poll_period(secs: u64) -> Duration {
    Duration::from_secs(secs.min(MAX_POLL_INTERVAL_SECS)).max(MIN_POLL_INTERVAL)
}

/// Queue what woke the stream arm. True when the stream ended or failed;
/// an error can repeat on every poll, so it ends the subscription.
fn take_wake(wake: StreamWake, backlog: &mut VecDeque<SettlementUpdate>) -> bool {
    match wake {
        StreamWake::Backlog => false,
        StreamWake::Next(Some(Ok(update))) => {
            backlog.push_back(update);
            false
        }
        StreamWake::Next(Some(Err(e))) => {
            error!("Settlement stream error: {}", e);
            true
        }
        StreamWake::Next(None) => true,
    }
}

/// Queue the wake, then drain buffered updates unless the stream already ended.
fn take_and_drain<S>(wake: StreamWake, stream: Option<&mut S>, backlog: &mut VecDeque<SettlementUpdate>) -> bool
where
    S: tokio_stream::Stream<Item = Result<SettlementUpdate, tonic::Status>> + Unpin,
{
    if take_wake(wake, backlog) {
        return true;
    }
    match stream {
        Some(s) => drain_ready(s, backlog, STREAM_BATCH_MAX),
        None => false,
    }
}

/// Move already-buffered updates into `backlog` without waiting, until it holds
/// `cap`. True when the stream has ended or failed (the error is logged).
pub(crate) fn drain_ready<S>(stream: &mut S, backlog: &mut VecDeque<SettlementUpdate>, cap: usize) -> bool
where
    S: tokio_stream::Stream<Item = Result<SettlementUpdate, tonic::Status>> + Unpin,
{
    for _ in 0..cap {
        if backlog.len() >= cap {
            break;
        }
        match futures::FutureExt::now_or_never(tokio_stream::StreamExt::next(stream)) {
            None => break,
            Some(None) => return true,
            Some(Some(Ok(update))) => backlog.push_back(update),
            Some(Some(Err(e))) => {
                error!("Settlement stream error: {}", e);
                return true;
            }
        }
    }
    false
}

/// Party-wide order access used by the cancel sweep.
#[async_trait]
pub(crate) trait PartyOrders: Send {
    async fn live_orders(&mut self) -> Result<Vec<Order>>;
    async fn cancel(&mut self, order_id: u64) -> Result<CancelOrderResponse>;
}

#[async_trait]
impl PartyOrders for OrderbookClient {
    async fn live_orders(&mut self) -> Result<Vec<Order>> {
        self.get_all_active_orders().await
    }

    async fn cancel(&mut self, order_id: u64) -> Result<CancelOrderResponse> {
        self.cancel_order(order_id).await
    }
}

/// Cancel live orders until two fetches in a row, 1s apart, come back empty (at most
/// `PARTY_CANCEL_ROUNDS`, failed ones included). Returns (cancelled, confirmed empty).
pub(crate) async fn cancel_all_party_orders<C: PartyOrders + ?Sized>(
    client: &mut C,
    tracker: Option<&Mutex<OrderTracker>>,
) -> (usize, bool) {
    let mut cancelled = 0usize;
    let mut seen_empty = false;
    for round in 0..PARTY_CANCEL_ROUNDS {
        let more = round.saturating_add(1) < PARTY_CANCEL_ROUNDS;
        let orders = match client.live_orders().await {
            Ok(orders) => orders,
            Err(e) => {
                warn!("Failed to fetch orders for cancellation: {}", e);
                seen_empty = false;
                if more {
                    tokio::time::sleep(PARTY_CANCEL_RETRY_PAUSE).await;
                }
                continue;
            }
        };
        if orders.is_empty() {
            if seen_empty {
                return (cancelled, true);
            }
            // An order submitted just before the sweep can land late
            seen_empty = true;
            if more {
                tokio::time::sleep(PARTY_CANCEL_CONFIRM_PAUSE).await;
            }
            continue;
        }
        seen_empty = false;
        // Orders booked by a failed submit are tracked before they are cancelled
        if let Some(tracker) = tracker {
            tracker.lock().await.adopt_listed(&orders, Instant::now());
        }
        info!("Cancelling {} existing order(s)...", orders.len());
        for order in orders {
            match client.cancel(order.order_id).await {
                Ok(r) if r.success => cancelled = cancelled.saturating_add(1),
                Ok(r) => warn!("Failed to cancel order {}: {}", order.order_id, r.message),
                Err(e) => warn!("Failed to cancel order {}: {}", order.order_id, e),
            }
        }
    }
    warn!("Orders may still be live after {} cancel round(s)", PARTY_CANCEL_ROUNDS);
    (cancelled, false)
}

/// Withdraw the whole party's book within `limit`; the tracker is cleared only
/// when the sweep confirmed the book empty.
async fn withdraw_all<C: PartyOrders + ?Sized>(
    client: &mut C,
    tracker: &Arc<Mutex<OrderTracker>>,
    limit: Duration,
) {
    info!("Cancelling all orders...");
    match tokio::time::timeout(limit, cancel_all_party_orders(client, Some(tracker.as_ref()))).await {
        Ok((n, true)) => {
            tracker.lock().await.cancel_all();
            info!("Cancelled {} order(s) after stopping the grid task", n);
        }
        // The sweep already warned that orders may still be live
        Ok((_, false)) => {}
        Err(_) => warn!("Order cancellation still running after {}s; continuing shutdown", limit.as_secs()),
    }
}

/// Stop the grid task, then withdraw the whole party's book in one bounded
/// sweep. A task that does not return in time is aborted first.
async fn stop_grid<G, C: PartyOrders + ?Sized>(
    handle: Option<JoinHandle<G>>,
    client: &mut C,
    tracker: &Arc<Mutex<OrderTracker>>,
    limit: Duration,
) {
    let Some(handle) = handle else { return };
    let _ = join_grid(handle, limit).await;
    withdraw_all(client, tracker, limit).await;
}

/// Shutdown: stop a running grid task, or wait for the sweep its exit started
/// and redo that sweep here if it did not finish. At most one handle is set.
async fn finish_grid<G, C: PartyOrders + ?Sized>(
    grid: Option<JoinHandle<G>>,
    sweep: Option<JoinHandle<bool>>,
    client: &mut C,
    tracker: &Arc<Mutex<OrderTracker>>,
    limit: Duration,
) {
    stop_grid(grid, client, tracker, limit).await;
    if !join_sweep(sweep, limit).await {
        withdraw_all(client, tracker, limit).await;
    }
}

/// Connect the sweep's own client, retrying a failed or timed-out attempt.
async fn connect_for_sweep(config: &BaseConfig) -> Option<OrderbookClient> {
    for attempt in 1..=PARTY_CANCEL_ROUNDS {
        let failure = match tokio::time::timeout(SWEEP_CONNECT_TIMEOUT, OrderbookClient::new(config)).await {
            Ok(Ok(client)) => return Some(client),
            Ok(Err(e)) => format!("cannot connect: {e:#}"),
            Err(_) => format!("connect timed out after {}s", SWEEP_CONNECT_TIMEOUT.as_secs()),
        };
        if attempt >= PARTY_CANCEL_ROUNDS {
            warn!("Order cancellation skipped, {}", failure);
            return None;
        }
        warn!("Order cancellation connect attempt {}/{} failed, retrying: {}", attempt, PARTY_CANCEL_ROUNDS, failure);
        tokio::time::sleep(PARTY_CANCEL_RETRY_PAUSE).await;
    }
    None
}

/// Withdraw every order on a client of its own, so the settlement loop is not held up.
/// True once the book was confirmed empty and the tracker cleared; `withdrawn` is set then too.
fn spawn_party_sweep<C, F>(
    connect: F,
    tracker: Arc<Mutex<OrderTracker>>,
    unexpected: bool,
    withdrawn: Arc<AtomicBool>,
) -> JoinHandle<bool>
where
    C: PartyOrders + 'static,
    F: std::future::Future<Output = Option<C>> + Send + 'static,
{
    tokio::spawn(async move {
        let Some(mut client) = connect.await else { return false };
        let (n, done) = cancel_all_party_orders(&mut client, Some(tracker.as_ref())).await;
        if !done {
            return false;
        }
        tracker.lock().await.cancel_all();
        withdrawn.store(true, Ordering::SeqCst);
        if unexpected {
            warn!("Grid stopped: cancelled {} order(s); order placement stopped", n);
        } else {
            info!("Cancelled {} order(s) after stopping the grid task", n);
        }
        true
    })
}

/// The grid task ended on its own: start withdrawing every order so none is
/// left unmanaged, and return at once; settlement, if running, carries on.
fn on_grid_exit<G>(
    joined: Result<G, JoinError>,
    shutdown: &Shutdown,
    config: &BaseConfig,
    tracker: &Arc<Mutex<OrderTracker>>,
    withdrawn: &Arc<AtomicBool>,
) -> JoinHandle<bool> {
    let unexpected = match joined {
        Ok(_) if shutdown.is_shutting_down() => {
            info!("Cancelling all orders...");
            false
        }
        Ok(_) => {
            error!("Grid task exited unexpectedly: returned while the agent is running");
            true
        }
        Err(e) => {
            error!("Grid task exited unexpectedly: {}", e);
            true
        }
    };
    let config = config.clone();
    let connect = async move { connect_for_sweep(&config).await };
    spawn_party_sweep(connect, Arc::clone(tracker), unexpected, Arc::clone(withdrawn))
}

/// Orders-only mode has no other work, so a grid that ends while the agent runs
/// stops the agent too. Returns the sweep and whether the exit was unexpected.
fn on_orders_only_grid_exit<G>(
    joined: Result<G, JoinError>,
    shutdown: &Shutdown,
    lp_shutdown: Option<&Shutdown>,
    config: &BaseConfig,
    tracker: &Arc<Mutex<OrderTracker>>,
    withdrawn: &Arc<AtomicBool>,
) -> (JoinHandle<bool>, bool) {
    let unexpected = !shutdown.is_shutting_down();
    let sweep = on_grid_exit(joined, shutdown, config, tracker, withdrawn);
    if unexpected {
        error!("Grid task ended in orders-only mode; stopping the agent");
        shutdown.signal();
        if let Some(lp) = lp_shutdown {
            lp.signal();
        }
    }
    (sweep, unexpected)
}

/// Wait up to `limit` for the order sweep; one still running is aborted.
/// True when there was no sweep or it finished withdrawing the book.
async fn join_sweep(handle: Option<JoinHandle<bool>>, limit: Duration) -> bool {
    let Some(mut handle) = handle else { return true };
    match tokio::time::timeout(limit, &mut handle).await {
        Ok(Ok(done)) => done,
        Ok(Err(e)) => {
            warn!("Order cancellation task failed: {}", e);
            false
        }
        Err(_) => {
            warn!("Order cancellation still running after {}s; aborting it", limit.as_secs());
            handle.abort();
            false
        }
    }
}

fn fmt_sig4(d: rust_decimal::Decimal) -> String {
    if d.is_zero() {
        return "0".to_string();
    }
    use rust_decimal::prelude::ToPrimitive;
    let abs = d.to_f64().unwrap_or(0.0).abs();
    if abs == 0.0 {
        return "0".to_string();
    }
    let magnitude = abs.log10().floor() as i32;
    let dp = (3 - magnitude).max(0) as u32;
    d.round_dp(dp).to_string()
}

/// Trade parameters recorded when buyer accepts an RFQ quote
#[derive(Debug, Clone)]
pub struct AcceptedRfqTrade {
    pub proposal_id: String,
    pub market_id: String,
    pub price: String,
    pub base_quantity: String,
    pub quote_quantity: String,
}

/// Trade parameters recorded when LP sends an RFQ quote
#[derive(Debug, Clone)]
pub struct QuotedTrade {
    pub market_id: String,
    pub price: String,
    pub base_quantity: String,
    pub quote_quantity: String,
}

/// Trait for fetching token balances
///
/// Implementations:
/// - `DirectBalanceProvider` (orderbook-agent) — calls Canton ledger gRPC directly
/// - `CloudBalanceProvider` (orderbook-cloud-agent) — calls LedgerGatewayService
#[async_trait]
pub trait BalanceProvider: Send + Sync {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>>;
}

/// Options for the agent runner
pub struct AgentOptions {
    pub settlement_only: bool,
    pub orders_only: bool,
    /// Dedicated provider for the background balance poller, so it never
    /// contends with the main loop's client. Falls back to the main provider.
    pub poller_balance_provider: Option<Arc<dyn BalanceProvider>>,
    /// Optional shared counter for actionable settlements (used by fill loop)
    pub actionable_count: Option<Arc<AtomicUsize>>,
    /// Optional external shutdown signal (used by fill loop to stop the background agent).
    /// When set, signalling this `Shutdown` triggers the runner's main loop to exit.
    pub shutdown: Option<Shutdown>,
    /// Buyer: accepted RFQ trades keyed by proposal_id (for settlement verification)
    pub accepted_rfq_trades: Option<Arc<Mutex<HashMap<String, AcceptedRfqTrade>>>>,
    /// Buyer: proposal_ids that the settlement executor has rejected.
    /// Populated by the background agent, drained by the fill loop to undo
    /// the optimistic `filled_total` / `remaining` bookkeeping when a
    /// previously-accepted quote fails to settle.
    pub rejected_rfq_trades: Option<Arc<Mutex<HashSet<String>>>>,
    /// LP: trades we quoted on (for settlement verification by attribute matching)
    pub quoted_rfq_trades: Option<Arc<Mutex<Vec<QuotedTrade>>>>,
    /// Signal to LP settlement stream and other background tasks. The
    /// runner's Ctrl-C handler will fire this on shutdown so every loop that
    /// holds a clone wakes immediately.
    pub lp_shutdown: Option<Shutdown>,
    /// Path to state file for save/restore on shutdown/restart
    pub state_file: Option<PathBuf>,
    /// Skip state restoration even if state file exists
    pub no_restore: bool,
    /// Fill loop state for save on shutdown (set by fill loop before signaling shutdown)
    pub fill_state: Option<Arc<Mutex<Option<SavedFillState>>>>,
    /// Accept all proposals without verification (for migration from old worker without saved state)
    pub no_reject: bool,
    /// RFQ V2: snapshot provider invoked at state-save time. The cloud-agent
    /// supplies a closure snapshotting the ticket pool + Confirmed V2 quotes.
    /// RESTORE of these is NOT done here: it happens in `run_cloud_agent`
    /// when the caches are constructed, BEFORE any worker starts.
    pub atomic_v2_snapshot: Option<
        Arc<dyn Fn() -> (Vec<crate::state::SavedTicket>, Vec<crate::state::SavedPendingV2>) + Send + Sync>,
    >,
    /// LP: trailing net tracker, wired into the OrderManager for grid shaping
    /// and checkpointed from the heartbeat. Load/save is the caller's job.
    pub net_positions: Option<Arc<crate::net_position::NetPositionTracker>>,
}

/// Run the agent event loop
///
/// Keep only balance rows the agent can actually spend under the registry it
/// has configured for each instrument. `GetBalances` returns one row per
/// (instrument_id, registrar); for a re-issued token (e.g. devnet cETH held
/// under both an old and a new registrar) only the row whose admin matches
/// `instruments.registry` is spendable — the others would fail any allocation
/// with a "Contract group identifier mismatch". Canton Coin is always kept;
/// an unknown/unconfigured registry is kept too (lenient — never blank
/// liquidity before `GetInstruments` has populated the registry map).
fn spendable_under_configured_registry(
    config: &BaseConfig,
    balances: Vec<TokenBalance>,
) -> Vec<TokenBalance> {
    balances
        .into_iter()
        .map(|mut b| {
            // Ledger balances carry ON-CHAIN wire ids; everything downstream
            // (liquidity manager keys, grid affordability, market configs)
            // speaks orderbook-internal ids, which differ for issuer-minted
            // opaque ids. Normalize here, at the ingest boundary. No-op for
            // legacy tokens whose two ids coincide — and it makes the registry
            // check below exact instead of lenient.
            if let Some(internal) = config.internal_id_for_wire(&b.instrument_id) {
                b.instrument_id = internal;
            }
            b
        })
        .filter(|b| {
            if b.is_canton_coin {
                return true;
            }
            let (_, registry) = config.resolve_instrument(&b.instrument_id);
            registry.is_empty() || registry == b.instrument_admin
        })
        .collect()
}

/// Apply the registry filter and push every non-CC balance into the liquidity
/// manager in one batch. Returns the filtered balances for the order manager.
pub(crate) async fn push_balances_to_lm(
    config: &BaseConfig,
    lm: Option<&Arc<crate::liquidity::LiquidityManager>>,
    balances: Vec<TokenBalance>,
) -> Vec<TokenBalance> {
    let balances = spendable_under_configured_registry(config, balances);
    if let Some(lm) = lm {
        let updates: Vec<(String, rust_decimal::Decimal)> = balances
            .iter()
            .filter(|b| !b.is_canton_coin)
            .filter_map(|b| {
                b.unlocked_amount
                    .parse::<rust_decimal::Decimal>()
                    .ok()
                    .map(|amount| (b.instrument_id.clone(), amount))
            })
            .collect();
        // A token absent from this snapshot loses its freshness stamp so its
        // last value is not quoted or committed as current.
        let present: HashSet<String> = updates.iter().map(|(id, _)| id.clone()).collect();
        lm.update_token_balances(&updates).await;
        lm.mark_missing_unrefreshed(&present).await;
    }
    balances
}

/// Keep liquidity-manager balances fresh from a task of their own, so
/// backpressure on the main loop cannot leave them unrefreshed.
pub(crate) fn spawn_balance_poller(
    config: BaseConfig,
    provider: Arc<dyn BalanceProvider>,
    lm: Arc<crate::liquidity::LiquidityManager>,
    shutdown: Shutdown,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("Balance poller started: interval={}s", period.as_secs());
        let mut ticker = interval(period.max(Duration::from_millis(1)));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_warn: Option<std::time::Instant> = None;
        loop {
            tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    info!("Balance poller shutting down");
                    break;
                }
                _ = ticker.tick() => {}
            }
            let outcome =
                tokio::time::timeout(Duration::from_secs(10), provider.fetch_balances()).await;
            let err = match outcome {
                Ok(Ok(balances)) => {
                    push_balances_to_lm(&config, Some(&lm), balances).await;
                    continue;
                }
                Ok(Err(e)) => format!("{e:#}"),
                Err(_) => "timed out".to_string(),
            };
            if last_warn.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                last_warn = Some(std::time::Instant::now());
                warn!("Balance poller fetch failed: {}", err);
            }
        }
    })
}

/// This is the shared main loop for both the local and cloud agents.
/// It handles:
/// - Order placement and grid management
/// - Settlement stream subscription and polling
/// - On-chain contract sync
/// - Graceful shutdown (cancel orders, reject unconfirmed, drain confirmed)
pub async fn run_agent<B, P>(
    config: BaseConfig,
    backend: B,
    balance_provider: P,
    options: AgentOptions,
) -> Result<()>
where
    B: SettlementBackend + 'static,
    P: BalanceProvider + 'static,
{
    let balance_provider = Arc::new(balance_provider);

    // Create orderbook client
    let mut orderbook_client = OrderbookClient::new(&config)
        .await
        .context("Failed to create orderbook client")?;

    info!("Connected to orderbook service");

    // Try to restore state from previous session
    let restored_state = if let Some(ref state_file) = options.state_file {
        if options.no_restore {
            info!("State restoration disabled (--no-restore)");
            delete_state(state_file);
            None
        } else {
            match load_state(state_file) {
                Some(saved) if saved.party_id == config.party_id => {
                    info!(
                        "Restoring state from previous session (saved at {}, start_time={})",
                        saved.saved_at, saved.start_time_ms
                    );
                    Some(saved)
                }
                Some(saved) => {
                    warn!(
                        "State file party_id '{}' != config '{}', ignoring",
                        saved.party_id, config.party_id
                    );
                    None
                }
                None => None,
            }
        }
    } else {
        None
    };

    // Cancel ALL existing orders (all markets) before setting start_time
    info!("Clearing all existing orders...");
    let _ = cancel_all_party_orders(&mut orderbook_client, None).await;

    // Set start_time: use saved value if restoring, else current time
    let start_time_ms = if let Some(ref saved) = restored_state {
        saved.start_time_ms
    } else {
        chrono::Utc::now().timestamp_millis() as u64
    };
    info!("Start time: {} ms{}", start_time_ms, if restored_state.is_some() { " (restored)" } else { "" });

    // Create shared order tracker
    let tracker = Arc::new(Mutex::new(OrderTracker::new(
        start_time_ms,
        config.private_key.clone(),
    )));

    // Restore order tracker state if available
    if let Some(ref saved) = restored_state {
        let mut t = tracker.lock().await;
        t.import_state(saved.orders.clone(), saved.settlement_orders.clone());
    }

    // Create settlement executor with shared tracker
    let mut settlement_executor = SettlementExecutor::new(&config, tracker.clone(), backend);

    // Enable no-reject mode if requested (skip verification for all proposals)
    if options.no_reject {
        settlement_executor.set_no_reject(true);
    }

    // Restore dedup sets if available
    if let Some(ref saved) = restored_state {
        settlement_executor
            .inject_completed_proposals(saved.completed_proposals.iter().cloned().collect::<HashSet<_>>());
        settlement_executor
            .inject_rejected_proposals(saved.rejected_proposals.iter().cloned().collect::<HashSet<_>>());
        info!(
            "Restored {} completed and {} rejected proposal(s)",
            saved.completed_proposals.len(),
            saved.rejected_proposals.len()
        );
    }

    // If caller provided an actionable_count Arc, inject it into the executor
    if let Some(ext_count) = &options.actionable_count {
        settlement_executor.set_actionable_count(ext_count.clone());
    }

    // Inject RFQ verification state if provided
    if let Some(ref accepted) = options.accepted_rfq_trades {
        // Restore saved RFQ trades into the shared map
        if let Some(ref saved) = restored_state {
            if !saved.accepted_rfq_trades.is_empty() {
                let mut map = accepted.lock().await;
                for trade in &saved.accepted_rfq_trades {
                    map.insert(
                        trade.proposal_id.clone(),
                        AcceptedRfqTrade {
                            proposal_id: trade.proposal_id.clone(),
                            market_id: trade.market_id.clone(),
                            price: trade.price.clone(),
                            base_quantity: trade.base_quantity.clone(),
                            quote_quantity: trade.quote_quantity.clone(),
                        },
                    );
                }
                info!("Restored {} accepted RFQ trade(s)", saved.accepted_rfq_trades.len());
            }
        }
        settlement_executor.set_accepted_rfq_trades(accepted.clone());
    }
    if let Some(ref rejected) = options.rejected_rfq_trades {
        settlement_executor.set_rejected_rfq_trades(rejected.clone());
    }
    if let Some(ref quoted) = options.quoted_rfq_trades {
        // Restore saved quoted trades into the shared vec
        if let Some(ref saved) = restored_state {
            if !saved.quoted_rfq_trades.is_empty() {
                let mut trades = quoted.lock().await;
                for trade in &saved.quoted_rfq_trades {
                    trades.push(QuotedTrade {
                        market_id: trade.market_id.clone(),
                        price: trade.price.clone(),
                        base_quantity: trade.base_quantity.clone(),
                        quote_quantity: trade.quote_quantity.clone(),
                    });
                }
                info!("Restored {} quoted RFQ trade(s)", saved.quoted_rfq_trades.len());
            }
        }
        settlement_executor.set_quoted_rfq_trades(quoted.clone());
    }

    // Inject liquidity manager from backend (if available)
    if let Some(lm) = settlement_executor.backend_liquidity_manager() {
        // Restore flow tracker from saved state before injecting
        if let Some(ref saved) = restored_state {
            if !saved.flow_tracker.is_empty() {
                info!("Restoring flow tracker ({} tokens) from saved state", saved.flow_tracker.len());
                let lm_clone = lm.clone();
                let flows = saved.flow_tracker.clone();
                // Must run async
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(lm_clone.restore_flows(flows));
                });
            }
        }
        settlement_executor.set_liquidity_manager(lm);
    }

    // Delete state file after successful restore
    if restored_state.is_some() {
        if let Some(ref state_file) = options.state_file {
            delete_state(state_file);
        }
    }

    // Create order manager with shared tracker
    let mut order_manager = OrderManager::new(
        config.clone(),
        OrderbookClient::new(&config).await?,
        tracker.clone(),
    );

    // Wire the net tracker so offer rungs can shrink as the desk net-sells.
    // No-op without a config section or a server-supplied size reference.
    if let Some(ref np) = options.net_positions {
        order_manager.set_net_positions(np.clone());
    }

    // Subscribe to settlements if not in orders-only mode
    let mut settlement_stream = if !options.orders_only {
        info!("Subscribing to settlement updates...");
        Some(
            orderbook_client
                .subscribe_settlements(None)
                .await
                .context("Failed to subscribe to settlements")?,
        )
    } else {
        info!("Running in orders-only mode - settlement disabled");
        None
    };

    // Setup timers — use Skip so accumulated ticks don't starve ctrl_c
    let mut heartbeat_timer = interval(Duration::from_secs(60));
    heartbeat_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut heartbeat_count: u64 = 0;
    let poll_interval = poll_period(config.poll_interval_secs);
    let mut settlement_poll_timer = interval(poll_interval);
    settlement_poll_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut result_collect_timer = interval(Duration::from_secs(2));
    result_collect_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Check if we have markets configured for order placement
    let has_markets =
        !config.markets.is_empty() && !options.settlement_only && !config.rfq_v2_only;

    if has_markets {
        info!(
            "Order placement enabled for {} market(s)",
            config.enabled_markets().len()
        );
    } else if config.rfq_v2_only {
        info!("rfq_v2_only mode - grid/limit order placement disabled (RFQ V2 / AtomicDVP only)");
    } else if options.settlement_only {
        info!("Running in settlement-only mode - order placement disabled");
    } else {
        info!("No markets configured - order placement disabled");
    }

    info!("Agent started. Press Ctrl+C to exit.");

    // Spawn a dedicated Ctrl-C handler so signals are processed immediately
    // even if the main loop is busy executing a long-running branch.
    // Handles BOTH first (graceful) and second (force exit) Ctrl-C signals
    // in the same spawned task to guarantee the force-exit always works.
    //
    // We use a single `Shutdown` (Arc<AtomicBool> + Arc<Notify>) — every
    // background loop holds a clone, polls the flag at the top of each
    // iteration, and `select!`s against `shutdown.wait()` so sleeps and
    // recvs unblock the moment Ctrl-C fires.
    let shutdown = Shutdown::new();
    let lp_shutdown = options.lp_shutdown.clone();
    {
        let shutdown = shutdown.clone();
        let lp_shutdown = lp_shutdown.clone();
        tokio::spawn(async move {
            signal::ctrl_c().await.ok();
            warn!("Ctrl-C received, shutting down gracefully...");
            shutdown.signal();
            if let Some(ref lp) = lp_shutdown {
                lp.signal();
            }

            // Wait for second Ctrl-C → force exit immediately
            signal::ctrl_c().await.ok();
            warn!("Second Ctrl-C received, forcing immediate shutdown");
            std::process::exit(1);
        });
    }

    // If an external shutdown signal was provided (e.g. from fill loop), forward it
    if let Some(ext_shutdown) = options.shutdown.clone() {
        let shutdown = shutdown.clone();
        let lp_shutdown = lp_shutdown.clone();
        tokio::spawn(async move {
            ext_shutdown.wait().await;
            info!("External shutdown signal received");
            shutdown.signal();
            if let Some(ref lp) = lp_shutdown {
                lp.signal();
            }
        });
    }

    // Share the runner's `Shutdown` with the settlement executor so spawned
    // tasks observe shutdown immediately (not a stale bool copy) AND so the
    // jitter sleeps inside `spawn_settlement_task` wake instantly on Ctrl-C.
    settlement_executor.set_shutdown(shutdown.clone());

    // Keep the issuance coefficient fresh from a task of its own. It must NOT
    // ride the main loop below: that `select!` is `biased`, so any arm rarer
    // than the loop's iteration period is unreachable under load. Idempotent —
    // `run_cloud_agent` already spawns this, and the guard makes the second
    // call free; the point of calling it here is embedders that bypass it.
    crate::forecast::spawn_forecast_poller(config.clone(), shutdown.clone());

    // Backstop timeout for any single Canton-touching await — generous, but
    // bounded so a stuck connection cannot trap the loop forever.
    let canton_op_timeout = Duration::from_secs(config.canton_op_timeout_secs);

    // Track consecutive poll failures for connectivity detection
    let mut poll_failures: u32 = 0;

    // Initial forecast + balance fetch in parallel so protections and the liquidity
    // gate are ready before the first RFQ arrives and the grid starts sized.
    let initial_started = std::time::Instant::now();
    let forecast_fut = tokio::time::timeout(
        Duration::from_secs(5),
        orderbook_client.get_rounds_data(Some(1)),
    );
    let balances_fut = tokio::time::timeout(
        Duration::from_secs(10),
        balance_provider.fetch_balances(),
    );
    let (forecast_res, balances_res) = tokio::join!(forecast_fut, balances_fut);

    match forecast_res {
        Ok(Ok(resp)) => {
            if let Some(prediction) = resp.prediction {
                crate::forecast::update_forecast(
                    prediction.forecast,
                    prediction.forecast_coefficient,
                );
                info!(
                    "Initial forecast: {}",
                    crate::forecast::forecast_label()
                );
            }
        }
        Ok(Err(e)) => warn!("Initial forecast fetch failed: {:#}", e),
        Err(_) => warn!("Initial forecast fetch timed out"),
    }

    match balances_res {
        Ok(Ok(balances)) => {
            let balances =
                push_balances_to_lm(&config, settlement_executor.liquidity_manager(), balances)
                    .await;
            let n = balances.len();
            order_manager.set_balances(balances);
            info!(
                "Initial balances loaded: {} tokens in {}ms",
                n,
                initial_started.elapsed().as_millis()
            );
        }
        Ok(Err(e)) => warn!("Initial balance fetch failed: {:#}", e),
        Err(_) => warn!("Initial balance fetch timed out"),
    }

    if let Some(lm) = settlement_executor.liquidity_manager() {
        let poller_provider: Arc<dyn BalanceProvider> = match options.poller_balance_provider.clone()
        {
            Some(p) => p,
            None => balance_provider.clone(),
        };
        spawn_balance_poller(
            config.clone(),
            poller_provider,
            Arc::clone(lm),
            shutdown.clone(),
            Duration::from_secs(10),
        );
    }

    // The grid runs on its own task; the settlement loop only wakes it.
    let grid_wake = Arc::new(Notify::new());
    let grid_stats = GridStats::new();
    let mut grid_task: Option<JoinHandle<OrderManager>> = if has_markets {
        Some(spawn_grid_task(
            order_manager,
            config.clone(),
            balance_provider.clone(),
            settlement_executor.liquidity_manager().cloned(),
            shutdown.clone(),
            Arc::clone(&grid_wake),
            grid_stats.clone(),
            GridTiming::default(),
        ))
    } else {
        None
    };
    // Party-wide order sweep started when the grid task exits; the flag is set once it withdrew the book
    let mut grid_sweep: Option<JoinHandle<bool>> = None;
    let grid_withdrawn = Arc::new(AtomicBool::new(false));
    // Set when the grid ends while running in orders-only mode; the agent then exits with an error
    let mut grid_failed = false;

    // Main event loop
    if !options.orders_only {
        let mut stream_backlog: VecDeque<SettlementUpdate> = VecDeque::new();
        let mut loop_stats = LoopStats::new(Instant::now());
        loop {
            if shutdown.is_shutting_down() {
                break;
            }
            let has_backlog = !stream_backlog.is_empty();
            tokio::select! {
                // Arms are polled in order. Timer arms reset after their body, so none
                // can make itself ready again at once and starve the arms below it.
                biased;
                _ = shutdown.wait() => {
                    break;
                }

                _ = heartbeat_timer.tick() => {
                    let arm_started = Instant::now();
                    heartbeat_count += 1;

                    // Net-position checkpoint (dirty-gated, ≤1 write/min) —
                    // covers grid-only agents where the V2 sweep never runs.
                    if let Some(ref np) = options.net_positions {
                        np.checkpoint_if_dirty();
                    }

                    let active_settlements = settlement_executor.active_settlements();
                    let n = active_settlements.len();
                    let (used, max, in_backoff, waiting) = settlement_executor.thread_utilization();
                    let pct = if max > 0 { used * 100 / max } else { 0 };
                    let (alloc, fees) = settlement_executor.queue_depth();
                    let cache_str = if let Some((avail, consumed, reserved, selectable)) = settlement_executor.cache_stats() {
                        format!(", cache {} avail {} consumed {} reserved {} selectable", avail, consumed, reserved, selectable)
                    } else {
                        String::new()
                    };
                    let worker_str = if let Some((aa, am, fa, fm)) = settlement_executor.worker_utilization() {
                        format!(", workers alloc {}/{} fee {}/{}", aa, am, fa, fm)
                    } else {
                        String::new()
                    };
                    let pause_str = {
                        let mut parts = Vec::new();
                        if let Some(secs) = settlement_executor.fee_pause_secs() {
                            parts.push(format!("FEES PAUSED {}s", secs));
                        }
                        if crate::forecast::is_fees_paused_by_overload() {
                            parts.push("FEES PAUSED (sequencer overload)".to_string());
                        }
                        if let Some(secs) = settlement_executor.background_pause_secs() {
                            parts.push(format!("CANCELS PAUSED {}s", secs));
                        }
                        if parts.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", parts.join(", "))
                        }
                    };
                    let forecast_str = {
                        let label = crate::forecast::forecast_label();
                        if label != "unknown" {
                            let coeff = crate::forecast::forecast_coefficient()
                                .unwrap_or_default();
                            // Age exposes a dead poller: without it a frozen
                            // coefficient is indistinguishable from a steady one.
                            let age = match crate::forecast::forecast_age_secs() {
                                Some(a) => format!(" {}s ago", a),
                                None => " never".to_string(),
                            };
                            format!(", forecast {} ({}){}", label, coeff, age)
                        } else {
                            String::new()
                        }
                    };
                    info!("Heartbeat: {} settlements, threads {}/{} ({}%) {} backoff {} waiting, queue {} alloc {} fees{}{}{}{}",
                        n, used, max, pct, in_backoff, waiting, alloc, fees, cache_str, worker_str, pause_str, forecast_str);
                    info!(
                        "{}",
                        loop_stats.take_line(stream_backlog.len(), settlement_executor.held_count(), Instant::now())
                    );
                    loop_stats.warn_overdue(poll_interval, Instant::now());
                    if has_markets {
                        log_grid_heartbeat(&grid_stats, grid_task.is_some(), grid_withdrawn.load(Ordering::SeqCst));
                    }
                    settlement_executor.log_cid_waiting_summary();
                    // Liquidity stats
                    if let Some(lm) = settlement_executor.liquidity_manager() {
                        // Reconcile CC reservations against the authoritative
                        // active set: a missed terminal event would otherwise
                        // leak a per-proposal reservation forever, decaying
                        // available CC to 0 over ~2 days. Self-heals each cycle.
                        let live: std::collections::HashSet<String> =
                            active_settlements.keys().cloned().collect();
                        let dropped = lm.retain_commitments(&live).await;
                        if dropped > 0 {
                            warn!(
                                "Liquidity reconcile: released {} orphaned CC reservation(s) (missed terminal event)",
                                dropped
                            );
                        }
                        let stats = lm.stats().await;
                        for s in &stats {
                            let depl_str = if s.hours_to_depletion.is_infinite() {
                                ">12h".to_string()
                            } else {
                                format!("{:.1}h", s.hours_to_depletion)
                            };
                            // RFQ V2 holdings histogram (this token as an LP-pays
                            // leg): total + reserved + USD-value buckets. Empty
                            // for non-LP backends; unbucketed if no USD price.
                            let holdings_str = match settlement_executor.holdings_histogram(&s.token) {
                                Some(h) if h.priced => format!(
                                    " | holdings {} ({} rsvd) <10:{} 10-20:{} 20-50:{} 50-100:{} >100:{}",
                                    h.total, h.reserved, h.under_10, h.b10_20, h.b20_50, h.b50_100, h.over_100
                                ),
                                Some(h) => format!(
                                    " | holdings {} ({} rsvd) [no USD price]",
                                    h.total, h.reserved
                                ),
                                None => String::new(),
                            };
                            let refreshed_str = match s.refreshed_secs_ago {
                                Some(secs) if lm.is_stale(&s.token).await.is_some() => format!(", STALE {}s", secs),
                                Some(secs) => format!(", refreshed {}s ago", secs),
                                None => ", never refreshed".to_string(),
                            };
                            info!(
                                "LIQUIDITY {}: {} bal / {} committed{}{} / {} avail ({} settlements), flow {:.1}/hr, depl={:.1} ({}){}{}",
                                s.token,
                                fmt_sig4(s.balance),
                                fmt_sig4(s.committed),
                                if s.fee_committed > rust_decimal::Decimal::ZERO {
                                    format!(" + {} fees", fmt_sig4(s.fee_committed))
                                } else {
                                    String::new()
                                },
                                if s.fee_reserve > rust_decimal::Decimal::ZERO {
                                    format!(" + {} reserve", fmt_sig4(s.fee_reserve))
                                } else {
                                    String::new()
                                },
                                fmt_sig4(s.available),
                                s.num_commitments,
                                s.net_outflow_per_hour,
                                s.depletion_coefficient,
                                depl_str,
                                holdings_str,
                                refreshed_str,
                            );
                        }
                    }
                    // Every 5 minutes, also list individual settlement IDs
                    if heartbeat_count % 5 == 0 && !active_settlements.is_empty() {
                        let ids: Vec<&str> = active_settlements.keys().map(|s| s.as_str()).collect();
                        info!("Active settlements: {}", ids.join(", "));
                    }
                    finish_arm(&mut heartbeat_timer, "heartbeat", arm_started);
                }

                joined = async {
                    match grid_task.as_mut() {
                        Some(handle) => handle.await,
                        None => std::future::pending().await,
                    }
                } => {
                    grid_task = None;
                    let arm_started = Instant::now();
                    grid_sweep = Some(on_grid_exit(joined, &shutdown, &config, &tracker, &grid_withdrawn));
                    warn_if_slow("grid-exit", arm_started);
                }

                _ = settlement_poll_timer.tick() => {
                    // Skip poll cycle during shutdown — let the notify branch fire
                    if shutdown.is_shutting_down() {
                        continue;
                    }
                    let arm_started = Instant::now();

                    // Reconnect settlement stream if broken
                    if settlement_stream.is_none() {
                        let resubscribe = orderbook_client.subscribe_settlements(None);
                        match tokio::time::timeout(canton_op_timeout, resubscribe).await {
                            Ok(Ok(s)) => {
                                settlement_stream = Some(s);
                                info!("Settlement stream reconnected");
                                settlement_executor.reset_failed_backoffs();
                            }
                            Ok(Err(e)) => warn!("Settlement stream reconnect failed: {:#}", e),
                            Err(_) => warn!(
                                "Settlement stream reconnect timed out after {}s",
                                canton_op_timeout.as_secs()
                            ),
                        }
                    }

                    let polled = tokio::time::timeout(
                        canton_op_timeout,
                        settlement_executor.poll_pending_proposals(&mut orderbook_client),
                    ).await;
                    // Self-bounded; kept outside the cycle timeout so a slow sync cannot cancel the advance
                    settlement_executor.sync_on_chain_contracts().await;
                    let advanced = tokio::time::timeout(
                        canton_op_timeout,
                        settlement_executor.advance_all_settlements(),
                    ).await;
                    match advanced.and(polled) {
                        Ok(poll_ok) => {
                            if poll_ok && poll_failures > 0 {
                                settlement_executor.reset_failed_backoffs();
                            }
                            poll_failures = if poll_ok { 0 } else { poll_failures.saturating_add(1) };
                        }
                        Err(_) => {
                            poll_failures = poll_failures.saturating_add(1);
                            warn!("Settlement poll cycle timed out after {}s", canton_op_timeout.as_secs());
                        }
                    }
                    loop_stats.note_poll(arm_started.elapsed(), Instant::now());
                    finish_arm(&mut settlement_poll_timer, "poll", arm_started);
                }

                _ = result_collect_timer.tick() => {
                    let arm_started = Instant::now();
                    if tokio::time::timeout(
                        canton_op_timeout,
                        settlement_executor.collect_and_readvance(),
                    ).await.is_err() {
                        warn!("collect_and_readvance timed out after {}s", canton_op_timeout.as_secs());
                    }
                    loop_stats.note_collect(Instant::now());
                    finish_arm(&mut result_collect_timer, "result-collect", arm_started);
                }

                wake = async {
                    if has_backlog {
                        return StreamWake::Backlog;
                    }
                    match settlement_stream.as_mut() {
                        Some(s) => StreamWake::Next(s.next().await),
                        None => std::future::pending().await,
                    }
                } => {
                    // Skip processing new settlement events during shutdown
                    if shutdown.is_shutting_down() {
                        continue;
                    }
                    let arm_started = Instant::now();

                    let closed = take_and_drain(wake, settlement_stream.as_mut(), &mut stream_backlog);
                    if closed {
                        warn!("Settlement stream closed, will reconnect on next poll cycle");
                        settlement_stream = None;
                    }

                    // Handle queued updates in order, then advance each touched proposal once
                    if !stream_backlog.is_empty() {
                        let outcome = settlement_executor
                            .apply_stream_batch(&mut stream_backlog, canton_op_timeout, STREAM_BATCH_BUDGET)
                            .await;
                        loop_stats.note_batch(outcome.handled);
                        if outcome.affects_grid {
                            grid_wake.notify_one();
                        }
                    }
                    warn_if_slow("stream", arm_started);
                }
            }
        }
    } else {
        // Orders-only mode
        loop {
            if shutdown.is_shutting_down() {
                break;
            }
            tokio::select! {
                // Shutdown wins ties; the grid itself runs on its own task.
                biased;
                _ = shutdown.wait() => {
                    break;
                }

                _ = heartbeat_timer.tick() => {
                    info!("Heartbeat: orders-only mode");
                    if has_markets {
                        log_grid_heartbeat(&grid_stats, grid_task.is_some(), grid_withdrawn.load(Ordering::SeqCst));
                    }
                }

                joined = async {
                    match grid_task.as_mut() {
                        Some(handle) => handle.await,
                        None => std::future::pending().await,
                    }
                } => {
                    grid_task = None;
                    let (sweep, unexpected) = on_orders_only_grid_exit(
                        joined, &shutdown, lp_shutdown.as_ref(), &config, &tracker, &grid_withdrawn,
                    );
                    grid_sweep = Some(sweep);
                    grid_failed = unexpected;
                    break;
                }
            }
        }
    }

    // Graceful shutdown — save state and exit immediately
    info!("Shutting down...");
    settlement_executor.set_shutting_down();
    settlement_executor.shutdown_backend();

    // In-flight settlement tasks drain while the grid stops and withdraws its orders
    let (drained, ()) = tokio::join!(
        settlement_executor.drain_tasks(),
        finish_grid(grid_task.take(), grid_sweep.take(), &mut orderbook_client, &tracker, GRID_STOP_TIMEOUT),
    );
    if drained > 0 {
        info!("Drained {} settlement task(s)", drained);
    }

    // Save state to disk for restoration on next restart
    if let Some(ref state_file) = options.state_file {
        let active_count = settlement_executor.active_settlements().len();

        // Export order tracker state
        let (saved_start_time, saved_orders, saved_settlement_orders) = {
            let t = tracker.lock().await;
            t.export_state()
        };

        // Build saved state
        let mut saved = SavedState::new(config.party_id.clone(), saved_start_time);
        saved.completed_proposals = settlement_executor
            .completed_proposals()
            .iter()
            .cloned()
            .collect();
        saved.rejected_proposals = settlement_executor
            .rejected_proposals()
            .iter()
            .cloned()
            .collect();
        saved.orders = saved_orders;
        saved.settlement_orders = saved_settlement_orders;

        // Save accepted RFQ trades
        if let Some(ref accepted) = options.accepted_rfq_trades {
            let map = accepted.lock().await;
            saved.accepted_rfq_trades = map
                .values()
                .map(|t| SavedAcceptedRfqTrade {
                    proposal_id: t.proposal_id.clone(),
                    market_id: t.market_id.clone(),
                    price: t.price.clone(),
                    base_quantity: t.base_quantity.clone(),
                    quote_quantity: t.quote_quantity.clone(),
                })
                .collect();
        }

        // Save quoted RFQ trades
        if let Some(ref quoted) = options.quoted_rfq_trades {
            let trades = quoted.lock().await;
            saved.quoted_rfq_trades = trades
                .iter()
                .map(|t| SavedQuotedTrade {
                    market_id: t.market_id.clone(),
                    price: t.price.clone(),
                    base_quantity: t.base_quantity.clone(),
                    quote_quantity: t.quote_quantity.clone(),
                })
                .collect();
        }

        // Save fill loop state if present
        if let Some(ref fill_state) = options.fill_state {
            saved.fill_state = fill_state.lock().await.clone();
        }

        // Save RFQ V2 ticket pool + Confirmed quote reservations
        if let Some(ref snapshot) = options.atomic_v2_snapshot {
            let (tickets, pending) = snapshot();
            if !tickets.is_empty() || !pending.is_empty() {
                info!(
                    "Saving RFQ V2 state: {} ticket(s), {} pending quote(s)",
                    tickets.len(),
                    pending.len()
                );
            }
            saved.atomic_tickets = tickets;
            saved.pending_v2_quotes = pending;
        }

        // Save flow tracker state for depletion detection on restart
        if let Some(lm) = settlement_executor.liquidity_manager() {
            saved.flow_tracker = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(lm.save_flows())
            });
            if !saved.flow_tracker.is_empty() {
                info!("Saving flow tracker ({} tokens) for restart", saved.flow_tracker.len());
            }
        }

        // Prune stale data before saving
        prune_state(&mut saved);

        match save_state(state_file, &saved) {
            Ok(()) => {
                info!(
                    "State saved ({} active settlements, {} completed, {} rejected). Restart to resume.",
                    active_count,
                    saved.completed_proposals.len(),
                    saved.rejected_proposals.len(),
                );
                save_backup(state_file, &saved);
            }
            Err(e) => {
                error!("Failed to save state: {:#}", e);
            }
        }
    }

    info!("Agent stopped");
    if grid_failed {
        return Err(anyhow::anyhow!("grid task ended in orders-only mode"));
    }
    Ok(())
}

#[cfg(test)]
mod balance_filter_tests {
    use super::*;
    use crate::config::BaseConfig;

    fn tb(id: &str, admin: &str, unlocked: &str, is_cc: bool) -> TokenBalance {
        TokenBalance {
            instrument_id: id.to_string(),
            instrument_admin: admin.to_string(),
            unlocked_amount: unlocked.to_string(),
            is_canton_coin: is_cc,
            ..Default::default()
        }
    }

    /// Non-CC rows reach the liquidity manager; CC is left to its own feed.
    #[tokio::test]
    async fn push_balances_to_lm_updates_non_cc_only() {
        let config = BaseConfig::test_minimal();
        let lm = crate::liquidity::LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        let balances = vec![
            tb("HECTO", "issuer::1", "14000000", false),
            tb("CC", "dso::whatever", "100", true),
            tb("EDELx", "edel::abc", "not-a-number", false),
        ];
        let out = push_balances_to_lm(&config, Some(&lm), balances).await;
        assert_eq!(
            out.len(),
            3,
            "filtered rows are returned for the order manager"
        );
        assert_eq!(
            lm.available("HECTO").await,
            rust_decimal::Decimal::from(14_000_000)
        );
        assert_eq!(lm.available_cc().await, rust_decimal::Decimal::ZERO);
        assert!(lm.is_stale("HECTO").await.is_none());
    }

    pub(super) struct CountingProvider {
        pub(super) calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BalanceProvider for CountingProvider {
        async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok(vec![tb("HECTO", "issuer::1", &n.to_string(), false)])
        }
    }

    /// The poller keeps the liquidity manager current on its own and stops on shutdown.
    #[tokio::test]
    async fn balance_poller_refreshes_lm() {
        let config = BaseConfig::test_minimal();
        let lm = crate::liquidity::LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(CountingProvider { calls: Arc::clone(&calls) });
        let shutdown = Shutdown::new();
        let handle = spawn_balance_poller(
            config,
            provider,
            Arc::clone(&lm),
            shutdown.clone(),
            Duration::from_millis(20),
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        let n = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(n >= 2, "expected repeated fetches, got {n}");
        assert!(lm.available("HECTO").await >= rust_decimal::Decimal::from(2));
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("poller exits on shutdown")
            .expect("poller task joins");
    }

    /// Dual-registry cETH: only the row under the configured registry survives;
    /// CC is always kept; a token with no configured registry stays (lenient).
    #[test]
    fn keeps_only_configured_registry_rows() {
        let mut config = BaseConfig::test_minimal();
        config.instrument_registries.insert("cETH".into(), "rails-new::12200b6d".into());

        let balances = vec![
            tb("cETH", "rails-new::12200b6d", "4.0", false), // configured → keep
            tb("cETH", "ceth-old::122078c9", "0.0537", false), // foreign → drop
            tb("CC", "dso::whatever", "100", true),          // CC → always keep
            tb("EDELx", "edel::abc", "5000", false),          // unconfigured → lenient keep
        ];

        let out = spendable_under_configured_registry(&config, balances);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|b| (b.instrument_id.as_str(), b.instrument_admin.as_str()))
            .collect();

        assert_eq!(
            got,
            vec![
                ("cETH", "rails-new::12200b6d"),
                ("CC", "dso::whatever"),
                ("EDELx", "edel::abc"),
            ],
            "old-registry cETH must be dropped; CC + unconfigured kept"
        );
    }

    /// Wire-id rows are renamed to the internal id used by liquidity keys and grid
    /// funding; the registry check runs against the configured registry exactly.
    #[test]
    fn normalizes_wire_ids_to_internal() {
        const WIRE: &str = "0a1b2c3d-1111-4222-8333-444455556666";
        let mut config = BaseConfig::test_minimal();
        config.instrument_registries.insert("ACME".into(), "issuer-1::1220aaaa".into());
        config.instrument_wire_ids.insert("ACME".into(), WIRE.into());
        config.instrument_wire_ids.insert("USDCx".into(), "USDCx".into());

        let balances = vec![
            tb(WIRE, "issuer-1::1220aaaa", "2400.0", false),   // canonical → keep, renamed
            tb(WIRE, "impostor::1220ffff", "999.0", false),        // foreign admin → drop
            tb("USDCx", "usdc-rep::12208115", "20.0", false),      // legacy id untouched (lenient)
        ];

        let out = spendable_under_configured_registry(&config, balances);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|b| (b.instrument_id.as_str(), b.unlocked_amount.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("ACME", "2400.0"), ("USDCx", "20.0")],
            "UUID row renamed to the internal id and kept; foreign-admin UUID dropped"
        );
    }
}

#[cfg(test)]
mod loop_tests {
    use super::balance_filter_tests::CountingProvider;
    use super::*;
    use crate::grid_task::GridCycle;
    use crate::liquidity::LiquidityManager;
    use crate::order_manager::{past_soft_deadline, CycleReport};
    use futures::FutureExt;
    use std::pin::Pin;
    use std::sync::atomic::Ordering;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    type Item = Result<SettlementUpdate, tonic::Status>;
    type UpdateStream = Pin<Box<dyn tokio_stream::Stream<Item = Item> + Send>>;

    fn update(pid: &str) -> SettlementUpdate {
        SettlementUpdate {
            proposal: Some(orderbook_proto::orderbook::SettlementProposal {
                proposal_id: pid.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn ids(backlog: &VecDeque<SettlementUpdate>) -> Vec<String> {
        backlog
            .iter()
            .filter_map(|u| u.proposal.as_ref().map(|p| p.proposal_id.clone()))
            .collect()
    }

    fn channel_stream() -> (tokio::sync::mpsc::UnboundedSender<Item>, UpdateStream) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (tx, Box::pin(UnboundedReceiverStream::new(rx)))
    }

    // ---- drain_ready ----

    #[tokio::test]
    async fn drain_ready_takes_buffered_updates_and_never_waits() {
        let (tx, mut stream) = channel_stream();
        let mut backlog = VecDeque::from([update("p0")]);
        for pid in ["p1", "p2"] {
            tx.send(Ok(update(pid))).unwrap();
        }
        assert!(!drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(ids(&backlog), ["p0", "p1", "p2"]);

        // Nothing buffered: returns at once and the stream stays usable
        assert!(!drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(backlog.len(), 3);
        tx.send(Ok(update("p3"))).unwrap();
        assert!(!drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(ids(&backlog), ["p0", "p1", "p2", "p3"]);
    }

    #[tokio::test]
    async fn drain_ready_reports_the_stream_end() {
        let (tx, mut stream) = channel_stream();
        tx.send(Ok(update("p1"))).unwrap();
        drop(tx);
        let mut backlog = VecDeque::new();
        assert!(drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(ids(&backlog), ["p1"]);
    }

    #[tokio::test]
    async fn drain_ready_stops_at_the_cap() {
        let (tx, mut stream) = channel_stream();
        for i in 0..10 {
            tx.send(Ok(update(&format!("p{i}")))).unwrap();
        }
        let mut backlog = VecDeque::from([update("old")]);
        assert!(!drain_ready(&mut stream, &mut backlog, 4));
        assert_eq!(ids(&backlog), ["old", "p0", "p1", "p2"]);

        // A full backlog takes nothing more; the rest stays queued in order
        assert!(!drain_ready(&mut stream, &mut backlog, 4));
        assert_eq!(backlog.len(), 4);
        backlog.clear();
        assert!(!drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(ids(&backlog), ["p3", "p4", "p5", "p6", "p7", "p8", "p9"]);
    }

    #[tokio::test]
    async fn drain_ready_ends_on_error() {
        let (tx, mut stream) = channel_stream();
        tx.send(Ok(update("p1"))).unwrap();
        tx.send(Err(tonic::Status::internal("boom"))).unwrap();
        tx.send(Ok(update("p2"))).unwrap();
        let mut backlog = VecDeque::new();
        assert!(drain_ready(&mut stream, &mut backlog, 64));
        assert_eq!(ids(&backlog), ["p1"], "nothing is taken past the error");
    }

    /// Fails on every poll, as a transport stuck on an unexpected EOF does.
    struct StickyError {
        polls: Arc<AtomicUsize>,
    }

    impl tokio_stream::Stream for StickyError {
        type Item = Item;
        fn poll_next(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Item>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::Ready(Some(Err(tonic::Status::internal("unexpected EOF"))))
        }
    }

    #[tokio::test]
    async fn sticky_stream_error_ends_the_subscription_at_once() {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut stream = StickyError { polls: Arc::clone(&polls) };
        let mut backlog = VecDeque::new();

        // The arm's wake: an error closes the stream, so the drain never runs
        let wake = StreamWake::Next(stream.next().await);
        assert!(take_and_drain(wake, Some(&mut stream), &mut backlog));
        assert_eq!(polls.load(Ordering::SeqCst), 1);

        // A drain meeting the error stops at the first poll
        assert!(drain_ready(&mut stream, &mut backlog, STREAM_BATCH_MAX));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        assert!(backlog.is_empty());

        // Updates and a queued backlog keep the stream open
        assert!(!take_wake(StreamWake::Next(Some(Ok(update("p1")))), &mut backlog));
        assert!(!take_wake(StreamWake::Backlog, &mut backlog));
        assert_eq!(ids(&backlog), ["p1"]);
        assert!(take_wake(StreamWake::Next(None), &mut backlog));
    }

    #[tokio::test]
    async fn zero_poll_interval_is_raised_to_the_minimum() {
        assert_eq!(poll_period(0), MIN_POLL_INTERVAL);
        assert_eq!(poll_period(5), Duration::from_secs(5));
        let mut timer = interval(poll_period(0));
        timer.tick().await;
    }

    #[tokio::test]
    async fn huge_poll_interval_is_capped_so_a_reset_cannot_overflow() {
        assert_eq!(poll_period(u64::MAX), Duration::from_secs(MAX_POLL_INTERVAL_SECS));
        let mut timer = interval(poll_period(i64::MAX as u64));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        timer.tick().await;
        timer.reset();
        assert!(timer.tick().now_or_never().is_none());
    }

    // ---- loop timers ----

    /// An arm whose body outlasts its timer period is ready again the moment it ends,
    /// so under `biased` it starves the arms below it; `finish_arm` restores a full period.
    #[tokio::test]
    async fn finish_arm_leaves_a_full_period() {
        let period = Duration::from_millis(60);
        let overrun = period + Duration::from_millis(30);
        let mut timer = interval(period);
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        timer.tick().await;

        tokio::time::sleep(overrun).await;
        assert!(timer.tick().now_or_never().is_some(), "an overrun timer is ready at once");

        let arm_started = Instant::now();
        tokio::time::sleep(overrun).await;
        let ended_at = Instant::now();
        finish_arm(&mut timer, "test", arm_started);
        assert!(timer.tick().now_or_never().is_none(), "a finished arm's timer is not ready at once");
        timer.tick().await;
        assert!(ended_at.elapsed() >= period, "ticked after {:?}", ended_at.elapsed());
    }

    #[test]
    fn loop_stats_line_restarts_interval_counters() {
        let t0 = Instant::now();
        let mut stats = LoopStats::new(t0);
        stats.note_batch(3);
        stats.note_batch(7);
        stats.note_poll(Duration::from_millis(40), t0);
        stats.note_collect(t0);
        let line = stats.take_line(2, 3, t0);
        assert_eq!(
            line,
            "Loop: stream 2 batches/10 updates (max 7, backlog 2), poll 1 (last 0s ago, max 40ms), collect 1 (last 0s ago), held 3"
        );
        let line = stats.take_line(0, 0, t0);
        assert_eq!(
            line,
            "Loop: stream 0 batches/0 updates (max 0, backlog 0), poll 0 (last 0s ago, max 0ms), collect 0 (last 0s ago), held 0"
        );
    }

    #[tokio::test]
    async fn party_sweep_retries_then_gives_up_when_the_fetch_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = BaseConfig::test_minimal();
        config.orderbook_grpc_url = format!("http://127.0.0.1:{port}");
        let mut client = OrderbookClient::lazy_for_tests(&config).unwrap();
        let started = Instant::now();
        assert_eq!(cancel_all_party_orders(&mut client, None).await, (0, false));
        // Every round ran: one pause between each pair of failed fetches
        let pauses = u32::try_from(PARTY_CANCEL_ROUNDS - 1).unwrap();
        assert!(started.elapsed() >= PARTY_CANCEL_RETRY_PAUSE * pauses, "{:?}", started.elapsed());
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    /// Yields its items, then panics if polled after the end, as some adapters do.
    struct PanicsAfterEnd {
        items: VecDeque<Item>,
        ended: bool,
    }

    impl tokio_stream::Stream for PanicsAfterEnd {
        type Item = Item;
        fn poll_next(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Item>> {
            assert!(!self.ended, "polled after the end");
            let next = self.items.pop_front();
            self.ended = next.is_none();
            std::task::Poll::Ready(next)
        }
    }

    // The stream arm drains only when the wake did not report the end
    #[tokio::test]
    async fn stream_end_is_not_polled_again() {
        let mut stream: UpdateStream = Box::pin(PanicsAfterEnd { items: VecDeque::new(), ended: false });
        let mut backlog = VecDeque::new();
        let wake = StreamWake::Next(stream.next().await);
        assert!(take_and_drain(wake, Some(&mut stream), &mut backlog));
        assert!(backlog.is_empty());

        // An open stream still drains what is buffered behind the wake
        let (tx, mut open) = channel_stream();
        tx.send(Ok(update("p2"))).unwrap();
        let wake = StreamWake::Next(Some(Ok(update("p1"))));
        assert!(!take_and_drain(wake, Some(&mut open), &mut backlog));
        assert_eq!(ids(&backlog), ["p1", "p2"]);
        backlog.clear();

        // Items still flow through the drain until the end is reported once
        let mut stream: UpdateStream = Box::pin(PanicsAfterEnd {
            items: VecDeque::from([Ok(update("p1")), Ok(update("p2"))]),
            ended: false,
        });
        let first = stream.next().await;
        assert!(matches!(first, Some(Ok(_))));
        assert!(drain_ready(&mut stream, &mut backlog, STREAM_BATCH_MAX));
        assert_eq!(ids(&backlog), ["p2"]);
    }

    // ---- grid task ----

    #[derive(Default)]
    struct GridLog {
        starts: Vec<Instant>,
        ends: Vec<Instant>,
        balances: usize,
        entered: u32,
        visited: u32,
        reports: Vec<CycleReport>,
    }

    type SharedLog = Arc<std::sync::Mutex<GridLog>>;

    /// Grid stand-in: `markets` markets of `per_market` each, checked like the real cycle.
    struct FakeGrid {
        markets: u32,
        per_market: Duration,
        log: SharedLog,
    }

    impl FakeGrid {
        fn new(markets: u32, per_market: Duration) -> (Self, SharedLog) {
            let log = SharedLog::default();
            (Self { markets, per_market, log: Arc::clone(&log) }, log)
        }
    }

    #[async_trait]
    impl GridCycle for FakeGrid {
        fn apply_balances(&mut self, _balances: Vec<TokenBalance>) {
            self.log.lock().unwrap().balances += 1;
        }

        async fn run_cycle(&mut self, stop: &Shutdown, deadline: Instant) -> Result<CycleReport> {
            self.log.lock().unwrap().starts.push(Instant::now());
            let mut report = CycleReport::default();
            for i in 0..self.markets {
                if stop.is_shutting_down() || past_soft_deadline(i as usize, Instant::now(), deadline) {
                    report.deferred = self.markets - i;
                    break;
                }
                self.log.lock().unwrap().entered += 1;
                tokio::time::sleep(self.per_market).await;
                self.log.lock().unwrap().visited += 1;
                report.markets += 1;
            }
            let mut log = self.log.lock().unwrap();
            log.ends.push(Instant::now());
            log.reports.push(report);
            Ok(report)
        }
    }

    struct SlowProvider(Duration);

    #[async_trait]
    impl BalanceProvider for SlowProvider {
        async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
            tokio::time::sleep(self.0).await;
            Ok(Vec::new())
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Upper bound for a wait that normally ends in milliseconds; generous for loaded CI hosts.
    const WAIT: Duration = Duration::from_secs(10);

    fn timing(period: u64, min_gap: u64, soft_deadline: u64, balance_timeout: u64) -> GridTiming {
        GridTiming {
            period: ms(period),
            min_gap: ms(min_gap),
            soft_deadline: ms(soft_deadline),
            balance_timeout: ms(balance_timeout),
        }
    }

    fn counting() -> (Arc<CountingProvider>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (Arc::new(CountingProvider { calls: Arc::clone(&calls) }), calls)
    }

    async fn wait_for(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let until = Instant::now() + limit;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(ms(5)).await;
        }
    }

    #[tokio::test]
    async fn grid_task_cycles_and_returns_the_grid_on_shutdown() {
        let (grid, log) = FakeGrid::new(2, Duration::ZERO);
        let (provider, calls) = counting();
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        let shutdown = Shutdown::new();
        let stats = GridStats::new();
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            provider,
            Some(Arc::clone(&lm)),
            shutdown.clone(),
            Arc::new(Notify::new()),
            stats.clone(),
            timing(20, 5, 1_000, 1_000),
        );

        assert!(wait_for(WAIT, || stats.snapshot().cycles >= 3).await);
        {
            let log = log.lock().unwrap();
            assert!(log.balances >= 3, "balances applied before every cycle");
            assert!(log.reports.iter().all(|r| r.markets == 2 && r.deferred == 0));
        }
        let snap = stats.snapshot();
        assert_eq!((snap.markets, snap.deferred), (2, 0));
        assert!(calls.load(Ordering::SeqCst) >= 3);
        assert!(lm.available("HECTO").await >= rust_decimal::Decimal::ONE, "the LM is fed each cycle");

        shutdown.signal();
        let grid = join_grid(handle, WAIT).await.expect("grid returned on shutdown");
        assert_eq!(grid.markets, 2);
        let cycles = log.lock().unwrap().reports.len();
        tokio::time::sleep(ms(60)).await;
        assert_eq!(log.lock().unwrap().reports.len(), cycles, "no cycle after the task returned");
    }

    #[tokio::test]
    async fn grid_task_coalesces_wakes_and_keeps_the_min_gap() {
        let min_gap = ms(40);
        let (grid, log) = FakeGrid::new(1, ms(80));
        let (provider, _calls) = counting();
        let shutdown = Shutdown::new();
        let wake = Arc::new(Notify::new());
        let stats = GridStats::new();
        // Only the immediate first tick falls inside the test; later cycles come from wakes
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            provider,
            None,
            shutdown.clone(),
            Arc::clone(&wake),
            stats.clone(),
            GridTiming { min_gap, ..timing(600_000, 0, 1_000, 1_000) },
        );

        // Wakes raised during a cycle collapse into one follow-up cycle
        assert!(wait_for(WAIT, || log.lock().unwrap().starts.len() == 1).await);
        for _ in 0..5 {
            wake.notify_one();
        }
        assert!(wait_for(WAIT, || stats.snapshot().cycles == 2).await);
        tokio::time::sleep(ms(150)).await;
        {
            let log = log.lock().unwrap();
            assert_eq!(log.starts.len(), 2);
            let gap = log.starts[1].saturating_duration_since(log.ends[0]);
            assert!(gap >= min_gap, "woken cycle started {gap:?} after the previous one ended");
        }

        // A burst of wakes while idle runs exactly one cycle too
        for _ in 0..10 {
            wake.notify_one();
        }
        assert!(wait_for(WAIT, || stats.snapshot().cycles == 3).await);
        tokio::time::sleep(ms(150)).await;
        assert_eq!(log.lock().unwrap().starts.len(), 3);

        shutdown.signal();
        assert!(join_grid(handle, WAIT).await.is_some());
    }

    // A shutdown during the wait before a woken cycle ends the task without that cycle
    #[tokio::test]
    async fn grid_task_stops_during_the_min_gap_wait() {
        let min_gap = Duration::from_secs(5);
        let (grid, log) = FakeGrid::new(1, Duration::ZERO);
        let (provider, _calls) = counting();
        let shutdown = Shutdown::new();
        let wake = Arc::new(Notify::new());
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            provider,
            None,
            shutdown.clone(),
            Arc::clone(&wake),
            GridStats::new(),
            GridTiming { min_gap, ..timing(600_000, 0, 1_000, 1_000) },
        );

        assert!(wait_for(WAIT, || log.lock().unwrap().ends.len() == 1).await);
        wake.notify_one();
        // Time for the task to take the wake and start waiting out the gap
        tokio::time::sleep(ms(200)).await;
        let signalled = Instant::now();
        shutdown.signal();
        assert!(join_grid(handle, WAIT).await.is_some());
        assert!(signalled.elapsed() < min_gap, "the gap wait ended on shutdown");
        assert_eq!(log.lock().unwrap().starts.len(), 1, "no cycle after shutdown");
    }

    #[tokio::test]
    async fn grid_task_with_a_zero_period_runs_and_returns_the_grid() {
        let (grid, _log) = FakeGrid::new(1, Duration::ZERO);
        let (provider, _calls) = counting();
        let shutdown = Shutdown::new();
        let stats = GridStats::new();
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            provider,
            None,
            shutdown.clone(),
            Arc::new(Notify::new()),
            stats.clone(),
            timing(0, 0, 1_000, 1_000),
        );

        assert!(wait_for(WAIT, || stats.snapshot().cycles >= 1).await);
        shutdown.signal();
        let grid = join_grid(handle, WAIT).await;
        assert!(grid.is_some(), "the task did not panic and returned the grid");
    }

    #[tokio::test]
    async fn grid_task_stops_between_markets_on_shutdown() {
        // A full cycle takes 2s, so stopping after one market is far below the bound
        let (grid, log) = FakeGrid::new(20, ms(100));
        let (provider, _calls) = counting();
        let shutdown = Shutdown::new();
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            provider,
            None,
            shutdown.clone(),
            Arc::new(Notify::new()),
            GridStats::new(),
            timing(10, 5, 10_000, 1_000),
        );

        assert!(wait_for(WAIT, || log.lock().unwrap().entered >= 2).await);
        let signalled = Instant::now();
        shutdown.signal();
        assert!(join_grid(handle, WAIT).await.is_some());
        assert!(signalled.elapsed() < ms(1_500), "stopped after the market in progress");

        let log = log.lock().unwrap();
        assert_eq!(log.entered, log.visited, "a market in progress is finished, never cut off");
        assert!(log.visited < 20);
        let last = log.reports.last().copied().unwrap_or_default();
        assert!(last.deferred > 0);
        assert_eq!(last.markets + last.deferred, 20);
    }

    #[tokio::test]
    async fn grid_task_survives_slow_cycles_and_balance_timeouts() {
        // Every cycle overruns its soft deadline and every balance fetch times out
        let (grid, log) = FakeGrid::new(10, ms(20));
        let shutdown = Shutdown::new();
        let stats = GridStats::new();
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            Arc::new(SlowProvider(Duration::from_secs(5))),
            None,
            shutdown.clone(),
            Arc::new(Notify::new()),
            stats.clone(),
            timing(10, 5, 50, 20),
        );

        assert!(wait_for(WAIT, || stats.snapshot().cycles >= 3).await);
        assert!(!handle.is_finished());
        {
            let log = log.lock().unwrap();
            assert_eq!(log.balances, 0, "a timed-out fetch applies nothing");
            for r in &log.reports {
                assert!(r.markets >= 1 && r.deferred > 0, "{r:?}");
                assert_eq!(r.markets + r.deferred, 10);
            }
        }
        let snap = stats.snapshot();
        assert!(snap.markets >= 1 && snap.deferred > 0, "{snap:?}");

        shutdown.signal();
        assert!(join_grid(handle, WAIT).await.is_some());
    }

    // ---- party sweep and grid exit ----

    /// Party book stand-in: each fetch takes the next scripted result; an
    /// exhausted script reads as an empty book.
    #[derive(Default)]
    struct ScriptedBook {
        script: VecDeque<Option<Vec<u64>>>,
        fetches: Vec<Instant>,
        cancelled: Vec<u64>,
        /// Listed in full when their id is scripted
        booked: Vec<Order>,
        /// When set, each cancel records whether its order was already tracked
        tracker: Option<Arc<Mutex<OrderTracker>>>,
        tracked_at_cancel: Arc<std::sync::Mutex<Vec<bool>>>,
    }

    impl ScriptedBook {
        fn new(script: Vec<Option<Vec<u64>>>) -> Self {
            Self { script: script.into(), ..Default::default() }
        }
    }

    #[async_trait]
    impl PartyOrders for ScriptedBook {
        async fn live_orders(&mut self) -> Result<Vec<Order>> {
            self.fetches.push(Instant::now());
            match self.script.pop_front() {
                Some(Some(ids)) => Ok(ids
                    .into_iter()
                    .map(|order_id| {
                        let booked = self.booked.iter().find(|o| o.order_id == order_id).cloned();
                        booked.unwrap_or(Order { order_id, ..Default::default() })
                    })
                    .collect()),
                Some(None) => anyhow::bail!("fetch failed"),
                None => Ok(Vec::new()),
            }
        }

        async fn cancel(&mut self, order_id: u64) -> Result<CancelOrderResponse> {
            if let Some(t) = &self.tracker {
                let tracked = t.lock().await.export_state().1.iter().any(|o| o.order_id == order_id);
                self.tracked_at_cancel.lock().unwrap().push(tracked);
            }
            self.cancelled.push(order_id);
            Ok(CancelOrderResponse { success: true, ..Default::default() })
        }
    }

    #[tokio::test]
    async fn party_sweep_catches_an_order_that_lands_after_an_empty_fetch() {
        let mut book = ScriptedBook::new(vec![Some(vec![1]), Some(vec![]), Some(vec![2]), Some(vec![]), Some(vec![])]);
        assert_eq!(cancel_all_party_orders(&mut book, None).await, (2, true));
        assert_eq!(book.cancelled, [1, 2]);
        assert_eq!(book.fetches.len(), 5);
        let gap = book.fetches[2].saturating_duration_since(book.fetches[1]);
        assert!(gap >= PARTY_CANCEL_CONFIRM_PAUSE, "confirming fetch came {gap:?} after the empty one");
    }

    #[tokio::test]
    async fn party_sweep_needs_two_empty_fetches_in_a_row() {
        let started = Instant::now();
        let mut book = ScriptedBook::default();
        assert_eq!(cancel_all_party_orders(&mut book, None).await, (0, true));
        assert_eq!(book.fetches.len(), 2);
        assert!(started.elapsed() >= PARTY_CANCEL_CONFIRM_PAUSE);

        // A failed fetch between two empty ones does not confirm the first
        let mut book = ScriptedBook::new(vec![Some(vec![]), None, Some(vec![]), Some(vec![])]);
        assert_eq!(cancel_all_party_orders(&mut book, None).await, (0, true));
        assert_eq!(book.fetches.len(), 4);
    }

    #[tokio::test]
    async fn party_sweep_counts_confirming_fetches_toward_the_rounds() {
        let script = vec![Some(vec![]), Some(vec![1]), Some(vec![]), Some(vec![2]), Some(vec![]), Some(vec![3])];
        let mut book = ScriptedBook::new(script);
        assert_eq!(cancel_all_party_orders(&mut book, None).await, (2, false));
        assert_eq!(book.fetches.len(), PARTY_CANCEL_ROUNDS);
        assert_eq!(book.cancelled, [1, 2]);
    }

    fn tracker_with_live_orders(ids: &[u64]) -> Arc<Mutex<OrderTracker>> {
        let mut tracker = OrderTracker::new(0, crate::secret::Secret::seal(&mut [7u8; 32]));
        for &id in ids {
            tracker.track_order(id, "AAA-USD", 1, "1", "1", id, "sig", b"data");
        }
        Arc::new(Mutex::new(tracker))
    }

    async fn live_tracked(tracker: &Arc<Mutex<OrderTracker>>) -> usize {
        tracker.lock().await.export_state().1.iter().filter(|o| o.is_active).count()
    }

    // Both party sweeps track an order booked by a failed submit before cancelling it
    #[tokio::test]
    async fn party_sweeps_adopt_a_booked_failed_submit_before_cancelling_it() {
        use crate::order_tracker::FailedSubmit;
        use orderbook_proto::orderbook::OrderType;
        for spawned in [false, true] {
            let tracker = tracker_with_live_orders(&[]);
            let submit = FailedSubmit {
                market_id: "AAA-USD".to_string(),
                order_type: OrderType::Offer as i32,
                price: "1".to_string(),
                quantity: "1".to_string(),
                nonce: 5,
                signature: "sig".to_string(),
                signed_data: b"data".to_vec(),
            };
            tracker.lock().await.note_submit_failed(submit, Instant::now());
            let mut book = ScriptedBook::new(vec![Some(vec![42])]);
            book.booked = vec![Order {
                order_id: 42,
                market_id: "AAA-USD".to_string(),
                nonce: 5,
                signature: Some("sig".to_string()),
                signed_data: b"data".to_vec(),
                ..Default::default()
            }];
            book.tracker = Some(Arc::clone(&tracker));
            let seen = Arc::clone(&book.tracked_at_cancel);
            if spawned {
                let withdrawn = Arc::new(AtomicBool::new(false));
                let sweep = spawn_party_sweep(async { Some(book) }, Arc::clone(&tracker), true, withdrawn);
                assert!(join_sweep(Some(sweep), WAIT).await);
            } else {
                withdraw_all(&mut book, &tracker, WAIT).await;
                assert_eq!(book.cancelled, [42]);
            }
            assert_eq!(*seen.lock().unwrap(), [true], "spawned={spawned}");
            let tracked = tracker.lock().await.export_state().1;
            assert!(tracked.iter().any(|o| o.order_id == 42), "spawned={spawned}");
            assert!(tracker.lock().await.failed_submits().is_empty(), "spawned={spawned}");
        }
    }

    #[tokio::test]
    async fn stop_grid_sweeps_the_party_book_after_the_grid_returns() {
        let tracker = tracker_with_live_orders(&[1, 2]);
        let mut book = ScriptedBook::new(vec![Some(vec![1, 2])]);
        stop_grid(None::<JoinHandle<u8>>, &mut book, &tracker, WAIT).await;
        assert!(book.fetches.is_empty(), "no grid task, nothing to withdraw");

        let (grid, done) = late_grid();
        stop_grid(Some(grid), &mut book, &tracker, WAIT).await;
        let finished = done.lock().unwrap().expect("grid ran");
        assert!(book.fetches[0] >= finished, "sweep started before the grid returned");
        assert_eq!(book.cancelled, [1, 2]);
        assert_eq!(book.fetches.len(), 3);
        assert_eq!(live_tracked(&tracker).await, 0);
    }

    /// A grid task that returns after a delay, recording when it did.
    fn late_grid() -> (JoinHandle<u8>, Arc<std::sync::Mutex<Option<Instant>>>) {
        let done = Arc::new(std::sync::Mutex::new(None::<Instant>));
        let d = Arc::clone(&done);
        let grid = tokio::spawn(async move {
            tokio::time::sleep(ms(200)).await;
            *d.lock().unwrap() = Some(Instant::now());
            1u8
        });
        (grid, done)
    }

    /// One live order whose cancel never answers.
    struct StuckCancel;

    #[async_trait]
    impl PartyOrders for StuckCancel {
        async fn live_orders(&mut self) -> Result<Vec<Order>> {
            Ok(vec![Order { order_id: 1, ..Default::default() }])
        }

        async fn cancel(&mut self, _order_id: u64) -> Result<CancelOrderResponse> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn stop_grid_bounds_a_sweep_whose_cancels_hang() {
        let tracker = tracker_with_live_orders(&[1]);
        let grid = Some(tokio::spawn(async { 1u8 }));
        let stopped = tokio::time::timeout(WAIT, stop_grid(grid, &mut StuckCancel, &tracker, ms(100))).await;
        assert!(stopped.is_ok(), "stop_grid outlived its bound");
        assert_eq!(live_tracked(&tracker).await, 1, "an unfinished sweep leaves the tracker alone");
    }

    #[tokio::test]
    async fn grid_exit_returns_at_once_and_the_sweep_is_bounded_at_shutdown() {
        // Accepts TCP but never answers, so the sweep cannot finish
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = BaseConfig::test_minimal();
        config.orderbook_grpc_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let tracker = tracker_with_live_orders(&[1]);
        let shutdown = Shutdown::new();
        let withdrawn = Arc::new(AtomicBool::new(false));

        let started = Instant::now();
        let sweep = on_grid_exit(Ok(()), &shutdown, &config, &tracker, &withdrawn);
        assert!(started.elapsed() < Duration::from_secs(2), "returned after {:?}", started.elapsed());
        tokio::time::sleep(ms(200)).await;
        assert!(!sweep.is_finished());

        let joined_at = Instant::now();
        assert!(!join_sweep(Some(sweep), ms(100)).await, "an aborted sweep is not complete");
        assert!(joined_at.elapsed() < Duration::from_secs(5), "{:?}", joined_at.elapsed());
        assert_eq!(live_tracked(&tracker).await, 1, "an unfinished sweep leaves the tracker alone");
        assert!(!withdrawn.load(Ordering::SeqCst));
        assert!(join_sweep(None, ms(100)).await, "no sweep, nothing left to withdraw");
        drop(listener);
    }

    #[tokio::test]
    async fn grid_exit_sweep_retries_the_connect_then_reports_not_withdrawn() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = BaseConfig::test_minimal();
        config.orderbook_grpc_url = format!("http://127.0.0.1:{port}");
        let tracker = tracker_with_live_orders(&[1]);
        let withdrawn = Arc::new(AtomicBool::new(false));

        let started = Instant::now();
        let sweep = on_grid_exit(Ok(()), &Shutdown::new(), &config, &tracker, &withdrawn);
        assert!(!join_sweep(Some(sweep), WAIT).await, "a sweep that never connected is not complete");
        // Every attempt ran: one pause between each pair of failed connects
        let pauses = u32::try_from(PARTY_CANCEL_ROUNDS - 1).unwrap();
        assert!(started.elapsed() >= PARTY_CANCEL_RETRY_PAUSE * pauses, "{:?}", started.elapsed());
        assert!(!withdrawn.load(Ordering::SeqCst));
        assert_eq!(live_tracked(&tracker).await, 1);
    }

    #[tokio::test]
    async fn orders_only_grid_exit_stops_the_agent_unless_it_is_shutting_down() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = BaseConfig::test_minimal();
        config.orderbook_grpc_url = format!("http://127.0.0.1:{port}");
        let tracker = tracker_with_live_orders(&[1]);
        let withdrawn = Arc::new(AtomicBool::new(false));

        // A grid that fails while the agent runs stops the agent and the LP loops
        let (shutdown, lp) = (Shutdown::new(), Shutdown::new());
        let grid: JoinHandle<()> = tokio::spawn(async { panic!("grid failed") });
        let joined = grid.await;
        let (sweep, unexpected) =
            on_orders_only_grid_exit(joined, &shutdown, Some(&lp), &config, &tracker, &withdrawn);
        sweep.abort();
        assert!(unexpected);
        assert!(shutdown.is_shutting_down() && lp.is_shutting_down());

        // Returning during shutdown is the normal stop
        let (shutdown, lp) = (Shutdown::new(), Shutdown::new());
        shutdown.signal();
        let (sweep, unexpected) =
            on_orders_only_grid_exit(Ok(()), &shutdown, Some(&lp), &config, &tracker, &withdrawn);
        sweep.abort();
        assert!(!unexpected);
        assert!(!lp.is_shutting_down());
    }

    #[tokio::test]
    async fn a_connected_sweep_withdraws_the_book_and_reports_it() {
        let tracker = tracker_with_live_orders(&[1, 2]);
        let withdrawn = Arc::new(AtomicBool::new(false));
        let book = ScriptedBook::new(vec![Some(vec![1, 2])]);
        let sweep = spawn_party_sweep(async { Some(book) }, Arc::clone(&tracker), true, Arc::clone(&withdrawn));
        assert!(join_sweep(Some(sweep), WAIT).await);
        assert!(withdrawn.load(Ordering::SeqCst));
        assert_eq!(live_tracked(&tracker).await, 0);

        let tracker = tracker_with_live_orders(&[1]);
        let withdrawn = Arc::new(AtomicBool::new(false));
        let sweep = spawn_party_sweep(async { None::<ScriptedBook> }, Arc::clone(&tracker), true, Arc::clone(&withdrawn));
        assert!(!join_sweep(Some(sweep), WAIT).await);
        assert!(!withdrawn.load(Ordering::SeqCst));
        assert_eq!(live_tracked(&tracker).await, 1);
    }

    #[tokio::test]
    async fn a_sweep_that_runs_out_of_rounds_is_not_reported_withdrawn() {
        // Every fetch fails, or an order keeps coming back: the book is never confirmed empty
        for script in [vec![None; PARTY_CANCEL_ROUNDS], vec![Some(vec![1]); PARTY_CANCEL_ROUNDS]] {
            let tracker = tracker_with_live_orders(&[1]);
            let withdrawn = Arc::new(AtomicBool::new(false));
            let book = ScriptedBook::new(script);
            let sweep = spawn_party_sweep(async { Some(book) }, Arc::clone(&tracker), true, Arc::clone(&withdrawn));
            assert!(!join_sweep(Some(sweep), WAIT).await);
            assert!(!withdrawn.load(Ordering::SeqCst));
            assert_eq!(live_tracked(&tracker).await, 1);
        }

        // The shutdown sweep leaves the tracker alone as well
        let tracker = tracker_with_live_orders(&[1]);
        let mut book = ScriptedBook::new(vec![Some(vec![1]); PARTY_CANCEL_ROUNDS]);
        withdraw_all(&mut book, &tracker, WAIT).await;
        assert_eq!(book.fetches.len(), PARTY_CANCEL_ROUNDS);
        assert_eq!(live_tracked(&tracker).await, 1);
    }

    #[tokio::test]
    async fn join_sweep_aborts_a_sweep_still_running() {
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = SetOnDrop(Arc::clone(&dropped));
        let stuck = tokio::spawn(async move {
            let _flag = flag;
            std::future::pending::<bool>().await
        });
        assert!(!join_sweep(Some(stuck), ms(50)).await);
        assert!(wait_for(WAIT, || dropped.load(Ordering::SeqCst)).await);
    }

    #[tokio::test]
    async fn join_sweep_reports_only_a_finished_withdrawal() {
        assert!(join_sweep(Some(tokio::spawn(async { true })), WAIT).await);
        assert!(!join_sweep(Some(tokio::spawn(async { false })), WAIT).await);
        let failed = tokio::spawn(async { panic!("sweep task failed") });
        assert!(!join_sweep(Some(failed), WAIT).await);
    }

    #[tokio::test]
    async fn shutdown_redoes_a_sweep_that_did_not_finish() {
        // A stuck sweep is aborted at the limit, which also bounds the redo (1s confirm pause)
        let stuck = tokio::spawn(std::future::pending::<bool>());
        for (sweep, limit) in [(tokio::spawn(async { false }), WAIT), (stuck, Duration::from_secs(3))] {
            let tracker = tracker_with_live_orders(&[1, 2]);
            let mut book = ScriptedBook::new(vec![Some(vec![1, 2])]);
            finish_grid(None::<JoinHandle<u8>>, Some(sweep), &mut book, &tracker, limit).await;
            assert_eq!(book.cancelled, [1, 2]);
            assert_eq!(live_tracked(&tracker).await, 0);
        }

        // A finished sweep, or none at all, needs no second pass
        for sweep in [Some(tokio::spawn(async { true })), None] {
            let tracker = tracker_with_live_orders(&[1]);
            let mut book = ScriptedBook::new(vec![Some(vec![1])]);
            finish_grid(None::<JoinHandle<u8>>, sweep, &mut book, &tracker, WAIT).await;
            assert!(book.fetches.is_empty());
            assert_eq!(live_tracked(&tracker).await, 1);
        }

        // A running grid is stopped and swept once
        let tracker = tracker_with_live_orders(&[1]);
        let mut book = ScriptedBook::new(vec![Some(vec![1])]);
        let (grid, done) = late_grid();
        finish_grid(Some(grid), None, &mut book, &tracker, WAIT).await;
        let finished = done.lock().unwrap().expect("grid ran");
        assert!(book.fetches[0] >= finished, "sweep started before the grid returned");
        assert_eq!(book.cancelled, [1]);
        assert_eq!(live_tracked(&tracker).await, 0);
    }

    #[test]
    fn grid_stop_bound_outlasts_a_cancel_and_submit_chain() {
        // Covers three back-to-back RPCs at the per-call deadline
        assert!(GRID_STOP_TIMEOUT >= crate::client::RPC_TIMEOUT.saturating_mul(3));
    }

    // ---- grid heartbeat ----

    fn snapshot_at(now: Instant) -> GridSnapshot {
        GridSnapshot {
            cycles: 4,
            max_ms: 900,
            refreshed: 1,
            placed: 6,
            cancelled: 6,
            failed: 0,
            deferred: 3,
            last_ms: 120,
            markets: 9,
            parked: 2,
            held: 1,
            last_end: Some(now),
            busy_since: None,
            started_at: now,
        }
    }

    #[test]
    fn grid_heartbeat_shows_markets_and_deferred() {
        let now = Instant::now();
        let (line, warning) = grid_heartbeat(&snapshot_at(now), true, false, now);
        assert_eq!(
            line.as_deref(),
            Some("Grid: 4 cycles, last 120ms (0s ago, max 900ms), markets 9 (deferred 3), parked 2, held 1, refreshed 1, placed 6, cancelled 6")
        );
        assert_eq!(warning, None);

        let later = now.checked_add(GRID_IDLE_WARN + Duration::from_secs(1)).unwrap();
        let (_, warning) = grid_heartbeat(&snapshot_at(now), true, false, later);
        assert_eq!(warning.as_deref(), Some("order-update tick overdue: 31s since last run"));

        let busy = GridSnapshot { busy_since: Some(now), ..snapshot_at(now) };
        let later = now.checked_add(GRID_BUSY_WARN + Duration::from_secs(1)).unwrap();
        let (_, warning) = grid_heartbeat(&busy, true, false, later);
        assert_eq!(warning.as_deref(), Some("grid cycle running for 121s"));
    }

    #[test]
    fn grid_heartbeat_warns_when_the_grid_task_is_gone() {
        let now = Instant::now();
        let (line, warning) = grid_heartbeat(&snapshot_at(now), false, true, now);
        assert_eq!(line, None);
        assert_eq!(warning.as_deref(), Some("Grid: task not running; orders were withdrawn"));

        // Until a sweep has finished, the orders are not claimed to be gone
        let (line, warning) = grid_heartbeat(&snapshot_at(now), false, false, now);
        assert_eq!(line, None);
        assert_eq!(warning.as_deref(), Some("Grid: task not running; orders may still be live"));
    }

    // ---- grid task warnings ----

    struct FailingProvider;

    #[async_trait]
    impl BalanceProvider for FailingProvider {
        async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
            anyhow::bail!("ledger unavailable")
        }
    }

    #[tokio::test]
    async fn grid_task_rate_limits_balance_fetch_warnings() {
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::WARN);

        let (grid, log) = FakeGrid::new(1, Duration::ZERO);
        let shutdown = Shutdown::new();
        let stats = GridStats::new();
        let handle = spawn_grid_task(
            grid,
            BaseConfig::test_minimal(),
            Arc::new(FailingProvider),
            None,
            shutdown.clone(),
            Arc::new(Notify::new()),
            stats.clone(),
            timing(10, 5, 1_000, 1_000),
        );

        assert!(wait_for(WAIT, || stats.snapshot().cycles >= 5).await);
        shutdown.signal();
        assert!(join_grid(handle, WAIT).await.is_some());
        assert_eq!(log.lock().unwrap().balances, 0);
        assert_eq!(logs.count("Failed to fetch balances: ledger unavailable"), 1);
    }
}
