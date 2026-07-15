//! Shared agent runner — event loop, shutdown, timers
//!
//! Both `orderbook-agent` and `orderbook-cloud-agent` call `run_agent()` with
//! their own `SettlementBackend` and `BalanceProvider` implementations.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::BaseConfig;
use crate::order_manager::OrderManager;
use crate::order_tracker::OrderTracker;
use crate::settlement::{SettlementBackend, SettlementExecutor};
use crate::shutdown::Shutdown;
use crate::state::{
    SavedAcceptedRfqTrade, SavedFillState, SavedQuotedTrade, SavedState, delete_state, load_state,
    prune_state, save_backup, save_state,
};

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
        .filter(|b| {
            if b.is_canton_coin {
                return true;
            }
            let (_, registry) = config.resolve_instrument(&b.instrument_id);
            registry.is_empty() || registry == b.instrument_admin
        })
        .collect()
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
    P: BalanceProvider,
{
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
    {
        let all_orders = orderbook_client
            .get_all_active_orders()
            .await
            .unwrap_or_else(|e| {
                warn!("Failed to fetch orders for cancellation: {}", e);
                vec![]
            });
        if !all_orders.is_empty() {
            info!("Cancelling {} existing order(s)...", all_orders.len());
        }
        for order in all_orders {
            if let Err(e) = orderbook_client.cancel_order(order.order_id).await {
                warn!("Failed to cancel order {}: {}", order.order_id, e);
            }
        }
    }

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
        config.private_key_bytes,
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
    let mut order_update_timer = interval(Duration::from_secs(5));
    order_update_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut settlement_poll_timer = interval(Duration::from_secs(config.poll_interval_secs));
    settlement_poll_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut result_collect_timer = interval(Duration::from_secs(2));
    result_collect_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Check if we have markets configured for order placement
    let has_markets = !config.markets.is_empty() && !options.settlement_only;

    if has_markets {
        info!(
            "Order placement enabled for {} market(s)",
            config.enabled_markets().len()
        );
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

    // Backstop timeout for any single Canton-touching await — generous, but
    // bounded so a stuck connection cannot trap the loop forever.
    let canton_op_timeout = Duration::from_secs(config.canton_op_timeout_secs);

    // Track consecutive poll failures for connectivity detection
    let mut poll_failures: u32 = 0;

    // Initial forecast + balance fetch in parallel so protections AND the liquidity
    // gate are ready before the first RFQ arrives (don't wait for first heartbeat
    // or the 5s order_update_timer tick).
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
            let balances = spendable_under_configured_registry(&config, balances);
            if let Some(lm) = settlement_executor.liquidity_manager() {
                for b in &balances {
                    if !b.is_canton_coin {
                        if let Ok(amount) = b.unlocked_amount.parse::<rust_decimal::Decimal>() {
                            lm.update_token_balance(&b.instrument_id, amount).await;
                        }
                    }
                }
            }
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

    // Main event loop
    if !options.orders_only {
        loop {
            if shutdown.is_shutting_down() {
                break;
            }
            tokio::select! {
                // `biased` polls arms in declaration order so shutdown wins
                // any tie with a ready timer arm — gives deterministic ~1s
                // shutdown latency. Once a non-shutdown arm has been chosen,
                // its body runs to completion (in-flight Canton tx finishes).
                biased;
                _ = shutdown.wait() => {
                    break;
                }
                result = async {
                    match &mut settlement_stream {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    // Skip processing new settlement events during shutdown
                    if shutdown.is_shutting_down() {
                        continue;
                    }

                    match result {
                        Some(Ok(update)) => {
                            // Extract proposal_id before handle_settlement_update consumes update
                            let update_proposal_id = update.proposal.as_ref()
                                .map(|p| p.proposal_id.clone());
                            let update_desc = format!(
                                "{} event={} market={}",
                                update.proposal.as_ref().map(|p| p.proposal_id.as_str()).unwrap_or("?"),
                                update.event_type,
                                update.proposal.as_ref().map(|p| p.market_id.as_str()).unwrap_or("?"),
                            );
                            match tokio::time::timeout(
                                canton_op_timeout,
                                settlement_executor.handle_settlement_update(update),
                            ).await {
                                Ok(Ok(())) => {
                                    // Immediately advance the specific settlement that was updated
                                    // (bypasses poll timer delay, clears backoff)
                                    if let Some(ref pid) = update_proposal_id {
                                        let _ = tokio::time::timeout(
                                            canton_op_timeout,
                                            settlement_executor.advance_proposal(pid),
                                        ).await;
                                    }
                                    // Settlement event may have freed tokens or
                                    // caused partial fills — refresh orders.
                                    if has_markets && !shutdown.is_shutting_down() {
                                        match tokio::time::timeout(
                                            Duration::from_secs(10),
                                            balance_provider.fetch_balances(),
                                        ).await {
                                            Ok(Ok(balances)) => order_manager.set_balances(balances),
                                            Ok(Err(e)) => warn!("Failed to fetch balances after settlement: {:#}", e),
                                            Err(_) => warn!("Balance fetch after settlement timed out"),
                                        }
                                        if !shutdown.is_shutting_down() {
                                            match tokio::time::timeout(
                                                canton_op_timeout,
                                                order_manager.update_cycle(),
                                            ).await {
                                                Ok(Err(e)) => warn!("Order update after settlement failed: {:#}", e),
                                                Err(_) => warn!("Order update after settlement timed out after {}s", canton_op_timeout.as_secs()),
                                                Ok(Ok(())) => {}
                                            }
                                        }
                                    }
                                }
                                Ok(Err(e)) => error!("[{}] Error handling settlement update: {:#}", update_desc, e),
                                Err(_) => warn!("[{}] Settlement update handling timed out after {}s", update_desc, canton_op_timeout.as_secs()),
                            }
                        }
                        Some(Err(e)) => {
                            error!("Settlement stream error: {}", e);
                        }
                        None => {
                            warn!("Settlement stream closed, will reconnect on next poll cycle");
                            settlement_stream = None;
                        }
                    }
                }

                _ = settlement_poll_timer.tick() => {
                    // Skip poll cycle during shutdown — let the notify branch fire
                    if shutdown.is_shutting_down() {
                        continue;
                    }

                    // Reconnect settlement stream if broken
                    if settlement_stream.is_none() {
                        match orderbook_client.subscribe_settlements(None).await {
                            Ok(s) => {
                                settlement_stream = Some(s);
                                info!("Settlement stream reconnected");
                                settlement_executor.reset_failed_backoffs();
                            }
                            Err(e) => warn!("Settlement stream reconnect failed: {:#}", e),
                        }
                    }

                    match tokio::time::timeout(canton_op_timeout, async {
                        let poll_ok = settlement_executor.poll_pending_proposals(&mut orderbook_client).await;
                        settlement_executor.sync_on_chain_contracts().await;
                        settlement_executor.advance_all_settlements().await;
                        poll_ok
                    }).await {
                        Ok(poll_ok) => {
                            if poll_ok && poll_failures > 0 {
                                settlement_executor.reset_failed_backoffs();
                            }
                            poll_failures = if poll_ok { 0 } else { poll_failures + 1 };

                            // Settlements may have completed — refresh orders.
                            if has_markets && !shutdown.is_shutting_down() {
                                match tokio::time::timeout(
                                    Duration::from_secs(10),
                                    balance_provider.fetch_balances(),
                                ).await {
                                    Ok(Ok(balances)) => order_manager.set_balances(balances),
                                    Ok(Err(e)) => warn!("Failed to fetch balances after settlement poll: {:#}", e),
                                    Err(_) => warn!("Balance fetch after settlement poll timed out"),
                                }
                                if !shutdown.is_shutting_down() {
                                    match tokio::time::timeout(
                                        canton_op_timeout,
                                        order_manager.update_cycle(),
                                    ).await {
                                        Ok(Err(e)) => warn!("Order update after settlement poll failed: {:#}", e),
                                        Err(_) => warn!("Order update after settlement poll timed out after {}s", canton_op_timeout.as_secs()),
                                        Ok(Ok(())) => {}
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            poll_failures += 1;
                            warn!("Settlement poll cycle timed out after {}s", canton_op_timeout.as_secs());
                        }
                    }
                }

                _ = order_update_timer.tick() => {
                    // Always refresh balances — LiquidityManager needs non-CC balances
                    // for settlement allocation gating even when the agent isn't
                    // placing orders (buyer/seller / settlement_only mode).
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        balance_provider.fetch_balances(),
                    ).await {
                        Ok(Ok(balances)) => {
                            let balances = spendable_under_configured_registry(&config, balances);
                            if let Some(lm) = settlement_executor.liquidity_manager() {
                                for b in &balances {
                                    if !b.is_canton_coin {
                                        if let Ok(amount) = b.unlocked_amount.parse::<rust_decimal::Decimal>() {
                                            lm.update_token_balance(&b.instrument_id, amount).await;
                                        }
                                    }
                                }
                            }
                            order_manager.set_balances(balances);
                        }
                        Ok(Err(e)) => warn!("Failed to fetch balances: {:#}", e),
                        Err(_) => warn!("Balance fetch timed out"),
                    }

                    // Order grid only runs when the agent has markets configured
                    // and isn't in settlement-only mode.
                    if has_markets {
                        match tokio::time::timeout(
                            canton_op_timeout,
                            order_manager.update_cycle(),
                        ).await {
                            Ok(Err(e)) => warn!("Order update cycle failed: {:#}", e),
                            Err(_) => warn!("Order update cycle timed out after {}s", canton_op_timeout.as_secs()),
                            Ok(Ok(())) => {}
                        }
                    }
                }

                _ = result_collect_timer.tick() => {
                    if let Err(_) = tokio::time::timeout(
                        canton_op_timeout,
                        settlement_executor.collect_and_readvance(),
                    ).await {
                        warn!("collect_and_readvance timed out after {}s", canton_op_timeout.as_secs());
                    }
                }

                _ = heartbeat_timer.tick() => {
                    heartbeat_count += 1;

                    // Poll issuance forecast from orderbook RPC (non-blocking, ignore errors)
                    match tokio::time::timeout(
                        Duration::from_secs(5),
                        orderbook_client.get_rounds_data(Some(1)),
                    ).await {
                        Ok(Ok(resp)) => {
                            if let Some(prediction) = resp.prediction {
                                crate::forecast::update_forecast(
                                    prediction.forecast,
                                    prediction.forecast_coefficient,
                                );
                            }
                        }
                        Ok(Err(e)) => debug!("Forecast poll failed: {:#}", e),
                        Err(_) => debug!("Forecast poll timed out"),
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
                            format!(", forecast {} ({})", label, coeff)
                        } else {
                            String::new()
                        }
                    };
                    info!("Heartbeat: {} settlements, threads {}/{} ({}%) {} backoff {} waiting, queue {} alloc {} fees{}{}{}{}",
                        n, used, max, pct, in_backoff, waiting, alloc, fees, cache_str, worker_str, pause_str, forecast_str);
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
                            info!(
                                "LIQUIDITY {}: {} bal / {} committed{}{} / {} avail ({} settlements), flow {:.1}/hr, depl={:.1} ({})",
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
                            );
                        }
                    }
                    // Every 5 minutes, also list individual settlement IDs
                    if heartbeat_count % 5 == 0 && !active_settlements.is_empty() {
                        let ids: Vec<&str> = active_settlements.keys().map(|s| s.as_str()).collect();
                        info!("Active settlements: {}", ids.join(", "));
                    }
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
                biased;
                _ = shutdown.wait() => {
                    break;
                }
                _ = order_update_timer.tick(), if has_markets => {
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        balance_provider.fetch_balances(),
                    ).await {
                        Ok(Ok(balances)) => {
                            let balances = spendable_under_configured_registry(&config, balances);
                            // Update non-CC token balances in liquidity manager
                            if let Some(lm) = settlement_executor.liquidity_manager() {
                                for b in &balances {
                                    if !b.is_canton_coin {
                                        if let Ok(amount) = b.unlocked_amount.parse::<rust_decimal::Decimal>() {
                                            lm.update_token_balance(&b.instrument_id, amount).await;
                                        }
                                    }
                                }
                            }
                            order_manager.set_balances(balances);
                        }
                        Ok(Err(e)) => warn!("Failed to fetch balances: {:#}", e),
                        Err(_) => warn!("Balance fetch timed out"),
                    }
                    match tokio::time::timeout(
                        canton_op_timeout,
                        order_manager.update_cycle(),
                    ).await {
                        Ok(Err(e)) => warn!("Order update cycle failed: {:#}", e),
                        Err(_) => warn!("Order update cycle timed out after {}s", canton_op_timeout.as_secs()),
                        Ok(Ok(())) => {}
                    }
                }

                _ = heartbeat_timer.tick() => {
                    info!("Heartbeat: orders-only mode");
                }
            }
        }
    }

    // Graceful shutdown — save state and exit immediately
    info!("Shutting down...");
    settlement_executor.set_shutting_down();
    settlement_executor.shutdown_backend();

    // Wait for in-flight settlement tasks to finish (they stop looping on shutdown flag)
    let drained = settlement_executor.drain_tasks().await;
    if drained > 0 {
        info!("Drained {} settlement task(s)", drained);
    }

    // Cancel market orders (quick)
    if has_markets {
        info!("Cancelling all orders...");
        if let Err(e) = order_manager.cancel_all_market_orders().await {
            warn!("Failed to cancel some orders: {}", e);
        }
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
}
