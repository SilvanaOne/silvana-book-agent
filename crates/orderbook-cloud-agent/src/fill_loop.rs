//! Fill loop for buyer/seller CLI commands
//!
//! Repeatedly requests quotes via RFQ, picks the best, accepts it, and
//! tracks remaining quantity until the full amount is filled and settled.
//! Runs the settlement agent in the background to process settlements.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{info, warn, error};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::runner::{run_agent, AcceptedRfqTrade, AgentOptions, BalanceProvider};
use orderbook_agent_logic::settlement::SettlementBackend;

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
}

/// Sleep that can be interrupted by shutdown signal. Returns true if shutdown was requested.
async fn interruptible_sleep(duration: Duration, shutdown: &Notify) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = shutdown.notified() => true,
    }
}

/// Run the fill loop with background settlement processing
pub async fn run_fill_loop<B, P>(
    config: BaseConfig,
    backend: B,
    balance_provider: P,
    params: FillParams,
) -> Result<()>
where
    B: SettlementBackend + 'static,
    P: BalanceProvider + 'static,
{
    if params.total_amount <= 0.0 {
        anyhow::bail!("total_amount must be positive, got {}", params.total_amount);
    }

    let dir_str = match params.direction {
        FillDirection::Buy => "buy",
        FillDirection::Sell => "sell",
    };

    // Shared state between fill loop and background settlement agent
    let actionable_count = Arc::new(AtomicUsize::new(0));
    let max_active = config.max_active_settlements;
    let agent_shutdown = Arc::new(Notify::new());
    let accepted_rfq_trades: Arc<Mutex<HashMap<String, AcceptedRfqTrade>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Shutdown signal for the fill loop (Ctrl-C handler notifies this)
    let fill_shutdown = Arc::new(Notify::new());
    {
        let notify = fill_shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            notify.notify_waiters();
        });
    }

    // Spawn the settlement agent in the background (settlement-only mode)
    let agent_config = config.clone();
    let agent_actionable = actionable_count.clone();
    let agent_shutdown_clone = agent_shutdown.clone();
    let agent_accepted_trades = accepted_rfq_trades.clone();
    let agent_handle = tokio::spawn(async move {
        if let Err(e) = run_agent(
            agent_config,
            backend,
            balance_provider,
            AgentOptions {
                settlement_only: true,
                orders_only: false,
                actionable_count: Some(agent_actionable),
                shutdown_notify: Some(agent_shutdown_clone),
                accepted_rfq_trades: Some(agent_accepted_trades),
                quoted_rfq_trades: None,
            },
        )
        .await
        {
            error!("Background settlement agent failed: {}", e);
        }
    });

    // Create orderbook client for RFQ operations
    let mut client = OrderbookClient::new(&config)
        .await
        .context("Failed to create orderbook client")?;

    let mut remaining = params.total_amount;
    let mut filled_total = 0.0_f64;
    let mut round = 0u32;
    let interval = Duration::from_secs(params.interval_secs);
    let mut shutdown_received = false;

    info!(
        "Fill loop started: {} {:.6} on market {} (min={:.6}, max={:.6}, interval={}s)",
        dir_str, params.total_amount, params.market_id,
        params.min_settlement, params.max_settlement, params.interval_secs
    );

    loop {
        if shutdown_received {
            warn!(
                "Ctrl-C received. Filled {:.6} / {:.6} ({:.1}%). Exiting fill loop.",
                filled_total, params.total_amount,
                (filled_total / params.total_amount) * 100.0
            );
            break;
        }

        if remaining <= 0.0 {
            info!(
                "Fill complete! Total filled: {:.6} in {} round(s)",
                filled_total, round
            );
            break;
        }

        round += 1;

        // Check settlement throttle
        let current_actionable = actionable_count.load(Ordering::Relaxed);
        if current_actionable >= max_active {
            info!(
                "[round {}] Throttled: {} active settlements >= max {} — waiting {}s",
                round, current_actionable, max_active, params.interval_secs
            );
            if interruptible_sleep(interval, &fill_shutdown).await {
                shutdown_received = true;
            }
            continue;
        }

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
                        // Compute mid from bid/ask, fall back to last price
                        let mid = match (price_resp.bid, price_resp.ask) {
                            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
                            _ => price_resp.last,
                        };
                        if mid <= 0.0 {
                            warn!("[round {}] No mid price available, waiting", round);
                            if interruptible_sleep(interval, &fill_shutdown).await {
                                shutdown_received = true;
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
                        if interruptible_sleep(interval, &fill_shutdown).await {
                            shutdown_received = true;
                        }
                        continue;
                    }
                }
            }
        };

        info!(
            "[round {}] Requesting quotes: {} {:.6} @ limit {:.6} (remaining={:.6}, active={})",
            round, dir_str, request_amount, price_limit, remaining, current_actionable
        );

        // Request quotes from LPs
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
                if interruptible_sleep(interval, &fill_shutdown).await {
                    shutdown_received = true;
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

        if !acceptable_quotes.is_empty() {
            // Pick best quote: lowest price for buy, highest for sell
            let best = pick_best_quote(&acceptable_quotes, params.direction);

            info!(
                "[round {}] Accepting quote {} from {} @ {} (qty={})",
                round, best.quote_id, best.lp_name, best.price, best.quantity
            );

            match client.accept_quote(&rfq_response.rfq_id, &best.quote_id).await {
                Ok(resp) => {
                    if resp.success {
                        if let Some(ref pid) = resp.proposal_id {
                            accepted_rfq_trades.lock().await.insert(pid.clone(), AcceptedRfqTrade {
                                proposal_id: pid.clone(),
                                market_id: params.market_id.clone(),
                                price: best.price.clone(),
                                base_quantity: best.quantity.clone(),
                                quote_quantity: best.quote_quantity.clone(),
                            });
                        }
                        let qty: f64 = best.quantity.parse().unwrap_or(0.0);
                        filled_total += qty;
                        remaining -= qty;
                        info!(
                            "[round {}] Quote accepted! proposal={} filled={:.6} remaining={:.6} ({:.1}%)",
                            round,
                            resp.proposal_id.as_deref().unwrap_or("?"),
                            filled_total,
                            remaining.max(0.0),
                            (filled_total / params.total_amount) * 100.0
                        );
                    } else {
                        warn!("[round {}] Accept quote failed: {}", round, resp.message);
                    }
                }
                Err(e) => {
                    warn!("[round {}] Accept quote error: {}", round, e);
                }
            }
        } else {
            // No acceptable quotes — check rejections for min/max hints
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

            if let Some(adapted_max) = best_max {
                if adapted_max < request_amount {
                    info!(
                        "[round {}] No quotes at limit {:.6}. LP max={:.6}, retrying with adapted amount",
                        round, price_limit, adapted_max
                    );
                    request_amount = adapted_max;

                    // Retry immediately with adapted amount
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
                            if interruptible_sleep(interval, &fill_shutdown).await {
                                shutdown_received = true;
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
                        info!(
                            "[round {}] Accepting adapted quote {} from {} @ {} (qty={})",
                            round, best.quote_id, best.lp_name, best.price, best.quantity
                        );
                        match client.accept_quote(&retry_resp.rfq_id, &best.quote_id).await {
                            Ok(resp) if resp.success => {
                                if let Some(ref pid) = resp.proposal_id {
                                    accepted_rfq_trades.lock().await.insert(pid.clone(), AcceptedRfqTrade {
                                        proposal_id: pid.clone(),
                                        market_id: params.market_id.clone(),
                                        price: best.price.clone(),
                                        base_quantity: best.quantity.clone(),
                                        quote_quantity: best.quote_quantity.clone(),
                                    });
                                }
                                let qty: f64 = best.quantity.parse().unwrap_or(0.0);
                                filled_total += qty;
                                remaining -= qty;
                                info!(
                                    "[round {}] Adapted quote accepted! filled={:.6} remaining={:.6}",
                                    round, filled_total, remaining.max(0.0)
                                );
                            }
                            Ok(resp) => warn!("[round {}] Adapted accept failed: {}", round, resp.message),
                            Err(e) => warn!("[round {}] Adapted accept error: {}", round, e),
                        }
                    } else {
                        warn!("[round {}] No acceptable quotes even at adapted amount {:.6}", round, request_amount);
                    }
                } else {
                    warn!("[round {}] No quotes within price limit {:.6}", round, price_limit);
                }
            } else {
                warn!(
                    "[round {}] No quotes and no LP size hints — waiting {}s",
                    round, params.interval_secs
                );
            }
        }

        // Wait before next round
        if interruptible_sleep(interval, &fill_shutdown).await {
            shutdown_received = true;
        }
    }

    // Wait for all settlements to complete before signalling shutdown.
    // The background agent is still running and advancing settlements.
    let settle_timeout = Duration::from_secs(300);
    let settle_poll = Duration::from_secs(5);
    info!("Fill loop done. Waiting for {} active settlement(s) to complete...",
        actionable_count.load(Ordering::Relaxed));

    let settle_start = tokio::time::Instant::now();
    loop {
        let active = actionable_count.load(Ordering::Relaxed);
        if active == 0 {
            info!("All settlements completed.");
            break;
        }
        if settle_start.elapsed() > settle_timeout {
            warn!("Timed out waiting for {} settlement(s) after {}s, proceeding with shutdown",
                active, settle_timeout.as_secs());
            break;
        }
        if interruptible_sleep(settle_poll, &fill_shutdown).await {
            warn!("Ctrl-C received while waiting for settlements. {} still active.", active);
            break;
        }
    }

    // Signal the background agent to shut down gracefully
    info!("Signalling background agent to shut down...");
    agent_shutdown.notify_one();

    // Wait for the agent to complete its graceful shutdown
    match tokio::time::timeout(Duration::from_secs(30), agent_handle).await {
        Ok(result) => { let _ = result; }
        Err(_) => { warn!("Background agent did not shut down within 30s, exiting anyway"); }
    }

    Ok(())
}

use orderbook_proto::orderbook::RfqQuoteInfo;

/// Pick the best quote from a non-empty list (lowest for buy, highest for sell)
fn pick_best_quote<'a>(quotes: &[&'a RfqQuoteInfo], direction: FillDirection) -> &'a RfqQuoteInfo {
    pick_best_quote_opt(quotes, direction).unwrap()
}

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
