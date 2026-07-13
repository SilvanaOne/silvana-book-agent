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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rust_decimal::prelude::FromStr as _;
use rust_decimal::Decimal;
use tokio::sync::Notify;
use tracing::{info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::state::SavedFillState;

use crate::accept_settle::MulticallSettler;
use crate::ledger_client::DAppProviderClient;

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
    if params.total_amount <= 0.0 {
        anyhow::bail!("total_amount must be positive, got {}", params.total_amount);
    }

    let dir_str = match params.direction {
        FillDirection::Buy => "buy",
        FillDirection::Sell => "sell",
    };

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
    {
        let flag = shutdown_flag.clone();
        let notify = fill_shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            flag.store(true, Ordering::Relaxed);
            notify.notify_waiters();
        });
    }

    // Create orderbook client for RFQ operations (quotes, prices)
    let mut client = OrderbookClient::new(&config)
        .await
        .context("Failed to create orderbook client")?;

    // Ledger client for DvpProposal / fee lookups
    let mut ledger = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
    .context("Failed to create ledger client")?;

    // Orderbook-rpc client for fetching settlement proposal fees
    let mut rpc_client = agent_logic::rpc_client::OrderbookRpcClient::connect(
        &config.orderbook_grpc_url,
        None,
    )
    .await
    .context("Failed to create orderbook-rpc client")?;
    rpc_client.set_jwt(agent_logic::auth::generate_jwt(
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
    )?);

    // RFQ V2: receiver-preapproval preflight (once per loop start, design §6.5)
    // and the per-market input-cid cap for own-holdings selection.
    let atomic_max_inputs = config
        .markets
        .iter()
        .find(|m| m.market_id == params.market_id)
        .and_then(|m| m.rfq.as_ref())
        .and_then(|r| r.v2.as_ref())
        .map(|v| v.max_input_holdings)
        .unwrap_or(20)
        .min(20);
    if params.atomic {
        let swapper = atomic_swapper
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("atomic fill requires an AtomicSwapper"))?;
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

    let (mut remaining, mut filled_total, mut round) = if let Some(ref fs) = saved_fill_state {
        let same_order = fs.direction == dir_str
            && fs.market_id == params.market_id
            && (fs.total_amount - params.total_amount).abs() < 0.000001
            && fs.remaining > 0.0;
        if same_order {
            info!(
                "Restoring fill state: filled={:.6} remaining={:.6} round={}",
                fs.filled_total, fs.remaining, fs.round
            );
            (fs.remaining, fs.filled_total, fs.round)
        } else {
            if fs.remaining <= 0.0 {
                info!("Previous fill completed, starting new order");
            } else {
                info!(
                    "New order (was {} {:.6} {}, now {} {:.6} {}), starting fresh",
                    fs.direction, fs.total_amount, fs.market_id,
                    dir_str, params.total_amount, params.market_id
                );
            }
            (params.total_amount, 0.0_f64, 0u32)
        }
    } else {
        (params.total_amount, 0.0_f64, 0u32)
    };
    let interval = Duration::from_secs(params.interval_secs);

    info!(
        "Fill loop started: {} {:.6} on market {} (min={:.6}, max={:.6}, interval={}s)",
        dir_str, params.total_amount, params.market_id,
        params.min_settlement, params.max_settlement, params.interval_secs
    );

    let dir_name = dir_str;
    let mut monitor_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
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

        round += 1;

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
        if params.atomic {
            let swapper = atomic_swapper.as_ref().expect("checked at loop start");
            match atomic_round(
                &mut client,
                swapper,
                &params,
                dir_str,
                request_amount,
                price_limit,
                round,
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
                    if let Some(ref path) = state_file {
                        let snapshot = SavedFillState {
                            direction: dir_name.to_string(),
                            market_id: params.market_id.clone(),
                            total_amount: params.total_amount,
                            filled_total,
                            remaining: remaining.max(0.0),
                            round,
                        };
                        let _ = save_fill_state_only(path, &config.party_id, snapshot);
                    }
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
                AtomicRoundResult::Abort(reason) => {
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

        let (rfq_id, best_quote_id, best_price, best_qty) = if !acceptable_quotes.is_empty() {
            let best = pick_best_quote(&acceptable_quotes, params.direction);
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

        let proposal_id = match accept_resp.proposal_id {
            Some(pid) => pid,
            None => {
                warn!("[round {}] accept_quote returned no proposal_id", round);
                continue;
            }
        };
        info!("[round {}] Quote accepted, proposal={}", round, proposal_id);

        // 1. Poll for the DvpProposal contract (LP creates it right after accept_quote)
        let dvp_proposal_cid = match poll_dvp_proposal_cid(
            &mut ledger,
            &proposal_id,
            Duration::from_secs(180),
            &fill_shutdown,
            &shutdown_flag,
        )
        .await
        {
            Ok(cid) => cid,
            Err(e) => {
                warn!("[round {}] Could not discover DvpProposal for {}: {}", round, proposal_id, e);
                continue;
            }
        };

        // 2. Fetch settlement proposal fees (dvp + allocation processing fees
        // for our role). The proposal stores them in **USD**; we must divide
        // by the current CC/USD rate before handing off to the multicall
        // builder — otherwise on-chain Canton transfers carry 0.15 CC worth
        // ~$0.023 for a $0.15 fee, which the orderbook-rpc amount gate
        // (correctly) rejects as underpayment.
        let (dvp_fee_cc, alloc_fee_cc, dvp_fee_usd, alloc_fee_usd) = match fetch_settlement_fees(
            &mut rpc_client,
            &mut ledger,
            &proposal_id,
            params.direction,
        )
        .await
        {
            Ok(tup) => tup,
            Err(e) => {
                warn!("[round {}] Failed to fetch settlement fees for {}: {}", round, proposal_id, e);
                continue;
            }
        };

        // 3. Submit the single multicall: Accept + Allocate + fees + traffic.
        // For CC allocation (sell), pass the trade qty so amulet selection covers it.
        let allocation_cc = if allocation_instrument.eq_ignore_ascii_case("cc")
            || allocation_instrument.eq_ignore_ascii_case("amulet")
        {
            Decimal::from_str(&best_qty).ok()
        } else {
            None
        };
        match settler
            .accept_and_settle(
                &proposal_id,
                &dvp_proposal_cid,
                &dvp_fee_cc,
                &alloc_fee_cc,
                &dvp_fee_usd,
                &alloc_fee_usd,
                &allocation_instrument,
                allocation_cc,
            )
            .await
        {
            Ok(result) => {
                let qty: f64 = best_qty.parse().unwrap_or(0.0);
                filled_total += qty;
                remaining -= qty;
                info!(
                    "[round {}] Accepted+Allocated {} via multicall: cid={} update={} traffic={} bytes — accepted={:.6}/{:.6} ({:.1}%)",
                    round,
                    proposal_id,
                    result.contract_id,
                    result.update_id,
                    result.traffic_total,
                    filled_total,
                    params.total_amount,
                    (filled_total / params.total_amount) * 100.0,
                );

                // Spawn non-blocking progress monitor
                let mon_config = config.clone();
                let mon_pid = proposal_id.clone();
                let mon_dir = params.direction;
                let mon_flag = shutdown_flag.clone();
                monitor_handles.push(
                    tokio::spawn(monitor_settlement_progress(mon_config, mon_pid, round, mon_dir, mon_flag))
                );
            }
            Err(e) => {
                warn!(
                    "[round {}] Multicall settlement failed for {}: {:#}",
                    round, proposal_id, e
                );
                if interruptible_sleep(interval, &fill_shutdown, &shutdown_flag).await {
                }
                continue;
            }
        }

        // Persist fill state after each successful round
        if let Some(ref path) = state_file {
            let snapshot = SavedFillState {
                direction: dir_name.to_string(),
                market_id: params.market_id.clone(),
                total_amount: params.total_amount,
                filled_total,
                remaining: remaining.max(0.0),
                round,
            };
            let _ = save_fill_state_only(path, &config.party_id, snapshot);
        }

        interruptible_sleep(Duration::from_millis(100), &fill_shutdown, &shutdown_flag).await;
    }

    if let Some(reason) = abort_reason {
        anyhow::bail!(
            "atomic fill loop aborted (filled {:.6}/{:.6}): {}",
            filled_total, params.total_amount, reason
        );
    }

    // Wait for all settlement monitors to complete before exiting
    let total_monitors = monitor_handles.len();
    if total_monitors > 0 && !shutdown_flag.load(Ordering::Relaxed) {
        info!("Waiting for {} settlement(s) to complete...", total_monitors);
        let mut completed = 0usize;
        for handle in monitor_handles {
            tokio::select! {
                _ = handle => { completed += 1; }
                _ = tokio::time::sleep(Duration::from_secs(300)) => {
                    warn!("Settlement monitor timeout");
                    break;
                }
            }
            if shutdown_flag.load(Ordering::Relaxed) { break; }
        }
        if completed == total_monitors {
            info!("All {} settlement(s) completed. Total filled: {:.6} in {} round(s)",
                  total_monitors, filled_total, round);
        } else {
            warn!("Exiting with {} pending settlement(s). Accepted: {:.6} in {} round(s)",
                  total_monitors - completed, filled_total, round);
        }
    } else if total_monitors == 0 && filled_total > 0.0 {
        info!("Total accepted: {:.6} in {} round(s) (no settlements to monitor)", filled_total, round);
    }

    Ok(())
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

    let Ok(mut rpc) = agent_logic::rpc_client::OrderbookRpcClient::connect(
        &config.orderbook_grpc_url, None,
    ).await else {
        warn!("[round {}] Could not connect RPC for progress monitoring {}", round, proposal_id);
        return;
    };
    if let Ok(jwt) = agent_logic::auth::generate_jwt(
        &config.party_id, &config.role, &config.private_key_bytes,
        config.token_ttl_secs, Some(config.node_name.as_str()),
    ) {
        rpc.set_jwt(jwt);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(300);
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
        if shutdown_flag.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline {
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
    let deadline = std::time::Instant::now() + timeout;
    let mut poll_every = Duration::from_millis(500);
    loop {
        if shutdown_flag.load(Ordering::Relaxed) {
            anyhow::bail!("shutdown while waiting for DvpProposal");
        }
        let contracts = ledger
            .get_settlement_contracts(&[proposal_id.to_string()])
            .await?;
        for c in &contracts {
            if c.settlement_id == proposal_id && c.contract_type == "DvpProposal" {
                return Ok(c.contract_id.clone());
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for DvpProposal contract");
        }
        if interruptible_sleep(poll_every, fill_shutdown, shutdown_flag).await {
            anyhow::bail!("shutdown while waiting for DvpProposal");
        }
        poll_every = (poll_every * 2).min(Duration::from_secs(3));
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
    rpc: &mut agent_logic::rpc_client::OrderbookRpcClient,
    ledger: &mut DAppProviderClient,
    proposal_id: &str,
    direction: FillDirection,
) -> Result<(String, String, String, String)> {
    // Retry a few times — the proposal row may take a moment to appear after accept_quote.
    let mut attempt = 0;
    let (dvp_usd_str, alloc_usd_str) = loop {
        match rpc.get_settlement_proposal_by_id(proposal_id).await? {
            Some(p) => {
                let (dvp, alloc) = match direction {
                    FillDirection::Buy => (
                        p.dvp_processing_fee_buyer.clone(),
                        p.allocation_processing_fee_buyer.clone(),
                    ),
                    FillDirection::Sell => (
                        p.dvp_processing_fee_seller.clone(),
                        p.allocation_processing_fee_seller.clone(),
                    ),
                };
                // Normalise empty strings to "0.0".
                let dvp = if dvp.trim().is_empty() { "0.0".to_string() } else { dvp };
                let alloc = if alloc.trim().is_empty() { "0.0".to_string() } else { alloc };
                break (dvp, alloc);
            }
            None => {
                attempt += 1;
                if attempt > 10 {
                    anyhow::bail!("settlement proposal not found after 10 attempts");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };

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

    let dvp_cc = (dvp_usd / rate).round_dp(10);
    let alloc_cc = (alloc_usd / rate).round_dp(10);

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
        format!("{:.10}", dvp_cc),
        format!("{:.10}", alloc_cc),
        dvp_usd_str,
        alloc_usd_str,
    ))
}

fn save_fill_state_only(
    path: &std::path::Path,
    party_id: &str,
    fill_state: SavedFillState,
) -> Result<()> {
    use agent_logic::state::SavedState;
    // Load existing, patch fill_state, save back. Best-effort.
    let existing = agent_logic::state::load_state(path)
        .filter(|s| s.party_id == party_id);
    let mut state = existing.unwrap_or_else(|| SavedState::new(party_id.to_string(), 0));
    state.fill_state = Some(fill_state);
    state.saved_at = chrono::Utc::now().to_rfc3339();
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

use orderbook_proto::rfqv2::AtomicQuoteInfo;

use crate::atomic_swap::{AtomicFill, AtomicSwapper, SwapOutcome};

enum AtomicRoundResult {
    Filled(AtomicFill),
    /// Transient failure — wait one interval and try a fresh round.
    Retry,
    DryRun,
    /// Commit status unknowable — the loop must stop (double-fill guard).
    Abort(String),
}

/// One atomic round: request V2 quotes → price-limit filter + pick best
/// (same selection logic as v1) → AcceptQuoteAtomic → settle the envelope.
#[allow(clippy::too_many_arguments)]
async fn atomic_round(
    client: &mut OrderbookClient,
    swapper: &AtomicSwapper,
    params: &FillParams,
    dir_str: &str,
    mut request_amount: f64,
    price_limit: f64,
    round: u32,
    max_input_holdings: usize,
) -> AtomicRoundResult {
    let rfq_response = match client
        .request_quotes_atomic(
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
                vec![],
                Some(15),
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

    match swapper
        .settle_envelope(&envelope, &best, params.direction, max_input_holdings)
        .await
    {
        Ok(SwapOutcome::Filled(fill)) => AtomicRoundResult::Filled(fill),
        Ok(SwapOutcome::Requote { reason }) => {
            warn!("[round {}] Atomic settle needs re-quote: {}", round, reason);
            AtomicRoundResult::Retry
        }
        Ok(SwapOutcome::Abort { reason }) => AtomicRoundResult::Abort(reason),
        Ok(SwapOutcome::DryRun) => AtomicRoundResult::DryRun,
        Err(e) => {
            warn!("[round {}] Atomic settle error: {:#}", round, e);
            AtomicRoundResult::Retry
        }
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
