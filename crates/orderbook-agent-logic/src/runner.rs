//! Shared agent runner — event loop, shutdown, timers
//!
//! Both `orderbook-agent` and `orderbook-cloud-agent` call `run_agent()` with
//! their own `SettlementBackend` and `BalanceProvider` implementations.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::sync::{Mutex, Notify};
use tokio::time::{interval, MissedTickBehavior};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::BaseConfig;
use crate::order_manager::OrderManager;
use crate::order_tracker::OrderTracker;
use crate::settlement::{SettlementBackend, SettlementExecutor};
use crate::state::{
    SavedAcceptedRfqTrade, SavedFillState, SavedQuotedTrade, SavedState, delete_state, load_state,
    save_state,
};

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
    /// Optional external shutdown signal (used by fill loop to stop the background agent)
    pub shutdown_notify: Option<Arc<Notify>>,
    /// Buyer: accepted RFQ trades keyed by proposal_id (for settlement verification)
    pub accepted_rfq_trades: Option<Arc<Mutex<HashMap<String, AcceptedRfqTrade>>>>,
    /// LP: trades we quoted on (for settlement verification by attribute matching)
    pub quoted_rfq_trades: Option<Arc<Mutex<Vec<QuotedTrade>>>>,
    /// Signal to LP settlement stream to stop accepting new RFQs on shutdown
    pub lp_shutdown: Option<Arc<AtomicBool>>,
    /// Path to state file for save/restore on shutdown/restart
    pub state_file: Option<PathBuf>,
    /// Skip state restoration even if state file exists
    pub no_restore: bool,
    /// Fill loop state for save on shutdown (set by fill loop before signaling shutdown)
    pub fill_state: Option<Arc<Mutex<Option<SavedFillState>>>>,
    /// Accept all proposals without verification (for migration from old worker without saved state)
    pub no_reject: bool,
}

/// Run the agent event loop
///
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
    let mut heartbeat_timer = interval(Duration::from_secs(300));
    heartbeat_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(Notify::new());
    let lp_shutdown = options.lp_shutdown.clone();
    {
        let flag = shutdown_flag.clone();
        let notify = shutdown_notify.clone();
        let lp_shutdown = lp_shutdown.clone();
        tokio::spawn(async move {
            signal::ctrl_c().await.ok();
            warn!("Ctrl-C received, shutting down gracefully...");
            flag.store(true, Ordering::SeqCst);
            if let Some(ref lp) = lp_shutdown {
                lp.store(true, Ordering::SeqCst);
            }
            notify.notify_one();

            // Wait for second Ctrl-C → force exit immediately
            signal::ctrl_c().await.ok();
            warn!("Second Ctrl-C received, forcing immediate shutdown");
            std::process::exit(1);
        });
    }

    // If an external shutdown signal was provided (e.g. from fill loop), forward it
    if let Some(ext_notify) = options.shutdown_notify {
        let flag = shutdown_flag.clone();
        let notify = shutdown_notify.clone();
        tokio::spawn(async move {
            ext_notify.notified().await;
            info!("External shutdown signal received");
            flag.store(true, Ordering::SeqCst);
            notify.notify_one();
        });
    }

    // Track consecutive poll failures for connectivity detection
    let mut poll_failures: u32 = 0;

    // Main event loop
    if !options.orders_only {
        loop {
            tokio::select! {
                result = async {
                    match &mut settlement_stream {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Some(Ok(update)) => {
                            let update_desc = format!(
                                "{} event={} market={}",
                                update.proposal.as_ref().map(|p| p.proposal_id.as_str()).unwrap_or("?"),
                                update.event_type,
                                update.proposal.as_ref().map(|p| p.market_id.as_str()).unwrap_or("?"),
                            );
                            match tokio::time::timeout(
                                Duration::from_secs(120),
                                settlement_executor.handle_settlement_update(update),
                            ).await {
                                Ok(Ok(())) => {
                                    // Settlement event may have freed tokens or
                                    // caused partial fills — refresh orders.
                                    if has_markets && !shutdown_flag.load(Ordering::Relaxed) {
                                        match tokio::time::timeout(
                                            Duration::from_secs(10),
                                            balance_provider.fetch_balances(),
                                        ).await {
                                            Ok(Ok(balances)) => order_manager.set_balances(balances),
                                            Ok(Err(e)) => warn!("Failed to fetch balances after settlement: {:#}", e),
                                            Err(_) => warn!("Balance fetch after settlement timed out"),
                                        }
                                        if !shutdown_flag.load(Ordering::Relaxed) {
                                            if let Err(e) = order_manager.update_cycle().await {
                                                warn!("Order update after settlement failed: {:#}", e);
                                            }
                                        }
                                    }
                                }
                                Ok(Err(e)) => error!("[{}] Error handling settlement update: {:#}", update_desc, e),
                                Err(_) => warn!("[{}] Settlement update handling timed out after 120s", update_desc),
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

                    match tokio::time::timeout(Duration::from_secs(120), async {
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
                            if has_markets && !shutdown_flag.load(Ordering::Relaxed) {
                                match tokio::time::timeout(
                                    Duration::from_secs(10),
                                    balance_provider.fetch_balances(),
                                ).await {
                                    Ok(Ok(balances)) => order_manager.set_balances(balances),
                                    Ok(Err(e)) => warn!("Failed to fetch balances after settlement poll: {:#}", e),
                                    Err(_) => warn!("Balance fetch after settlement poll timed out"),
                                }
                                if !shutdown_flag.load(Ordering::Relaxed) {
                                    if let Err(e) = order_manager.update_cycle().await {
                                        warn!("Order update after settlement poll failed: {:#}", e);
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            poll_failures += 1;
                            warn!("Settlement poll cycle timed out after 120s");
                        }
                    }
                }

                _ = order_update_timer.tick(), if has_markets && !shutdown_flag.load(Ordering::Relaxed) => {
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        balance_provider.fetch_balances(),
                    ).await {
                        Ok(Ok(balances)) => order_manager.set_balances(balances),
                        Ok(Err(e)) => warn!("Failed to fetch balances: {:#}", e),
                        Err(_) => warn!("Balance fetch timed out"),
                    }
                    if let Err(e) = order_manager.update_cycle().await {
                        warn!("Order update cycle failed: {:#}", e);
                    }
                }

                _ = result_collect_timer.tick() => {
                    settlement_executor.collect_and_readvance().await;
                }

                _ = heartbeat_timer.tick() => {
                    let active_settlements = settlement_executor.active_settlements();
                    if !active_settlements.is_empty() {
                        let ids: Vec<&str> = active_settlements.keys().map(|s| s.as_str()).collect();
                        info!("Heartbeat: {} active settlement(s): {}", ids.len(), ids.join(", "));
                    }
                }

                _ = shutdown_notify.notified() => {
                    break;
                }
            }
        }
    } else {
        // Orders-only mode
        loop {
            tokio::select! {
                _ = order_update_timer.tick(), if has_markets && !shutdown_flag.load(Ordering::Relaxed) => {
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        balance_provider.fetch_balances(),
                    ).await {
                        Ok(Ok(balances)) => order_manager.set_balances(balances),
                        Ok(Err(e)) => warn!("Failed to fetch balances: {:#}", e),
                        Err(_) => warn!("Balance fetch timed out"),
                    }
                    if let Err(e) = order_manager.update_cycle().await {
                        warn!("Order update cycle failed: {:#}", e);
                    }
                }

                _ = heartbeat_timer.tick() => {
                    info!("Heartbeat: orders-only mode");
                }

                _ = shutdown_notify.notified() => {
                    break;
                }
            }
        }
    }

    // Graceful shutdown — save state and exit immediately
    info!("Shutting down...");
    settlement_executor.set_shutting_down();

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

        match save_state(state_file, &saved) {
            Ok(()) => {
                info!(
                    "State saved ({} active settlements, {} completed, {} rejected). Restart to resume.",
                    active_count,
                    saved.completed_proposals.len(),
                    saved.rejected_proposals.len(),
                );
            }
            Err(e) => {
                error!("Failed to save state: {:#}", e);
            }
        }
    }

    info!("Agent stopped");
    Ok(())
}
