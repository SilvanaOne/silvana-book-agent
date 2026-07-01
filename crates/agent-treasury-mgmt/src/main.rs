//! Treasury Management Agent
//!
//! Policy-driven rebalancer with an approval workflow. Same target structure
//! as `agent-target-allocation` (per-instrument absolute quote-value targets),
//! but trades whose notional exceeds `--approval-threshold-quote` are
//! **enqueued** to a file consumable by `agent-human-approval` instead of
//! being submitted directly. Smaller trades go straight to the book.
//!
//! Additional treasury controls:
//! - `--max-trade-quote` — absolute ceiling per individual trade, regardless
//!   of approval routing.
//! - `--max-daily-trade-quote` — rolling 24h cap across all rebalances.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-treasury-mgmt")]
#[command(about = "Policy-driven treasury rebalancer with approval routing")]
struct Cli {
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(long, global = true)]
    dry_run: bool,

    #[arg(long, global = true)]
    force: bool,

    #[arg(long, global = true)]
    confirm: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        /// Per-instrument absolute quote-value targets: INSTRUMENT@MARKET=VALUE
        #[arg(long, value_parser = parse_target)]
        target: Vec<TargetSpec>,

        /// Trades with notional ≤ this go direct to the book; larger ones queue for human approval
        #[arg(long)]
        approval_threshold_quote: String,

        /// Absolute ceiling on any single trade
        #[arg(long)]
        max_trade_quote: String,

        /// Rolling 24-hour total trade notional cap
        #[arg(long)]
        max_daily_trade_quote: String,

        /// Approval queue file (consumed by agent-human-approval)
        #[arg(long, default_value = "approval-queue.jsonl")]
        approval_queue: PathBuf,

        /// Don't trade if deviation is below this absolute quote value
        #[arg(long, default_value = "10")]
        threshold_quote: String,

        /// Fraction of the gap to close per cycle (0 < x ≤ 1)
        #[arg(long, default_value = "0.5")]
        rebalance_fraction: f64,

        #[arg(long, default_value = "600")]
        check_interval: u64,

        #[arg(long)]
        no_restore: bool,
    },
}

#[derive(Clone, Debug)]
struct TargetSpec {
    instrument: String,
    market: String,
    target_quote_value: Decimal,
}

fn parse_target(s: &str) -> Result<TargetSpec, String> {
    let (lhs, val_str) = s.split_once('=').ok_or_else(|| format!("expected INST@MARKET=VALUE, got {s}"))?;
    let (instrument, market) = lhs.split_once('@').ok_or_else(|| format!("expected INST@MARKET, got {lhs}"))?;
    let value = Decimal::from_str(val_str).map_err(|e| format!("bad value in {s}: {e}"))?;
    if value < Decimal::ZERO {
        return Err(format!("value must be >= 0, got {value}"));
    }
    Ok(TargetSpec {
        instrument: instrument.to_string(),
        market: market.to_string(),
        target_quote_value: value,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_treasury_mgmt", "agent_logic", "tx_verifier"],
        "agent-treasury-mgmt",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            target,
            approval_threshold_quote,
            max_trade_quote,
            max_daily_trade_quote,
            approval_queue,
            threshold_quote,
            rebalance_fraction,
            check_interval,
            no_restore,
        } => {
            if target.is_empty() {
                anyhow::bail!("provide at least one --target");
            }
            if !(0.0..=1.0).contains(&rebalance_fraction) || rebalance_fraction == 0.0 {
                anyhow::bail!("--rebalance-fraction must be in (0, 1]");
            }
            let approval_threshold = Decimal::from_str(&approval_threshold_quote).context("Invalid --approval-threshold-quote")?;
            let max_trade = Decimal::from_str(&max_trade_quote).context("Invalid --max-trade-quote")?;
            let max_daily = Decimal::from_str(&max_daily_trade_quote).context("Invalid --max-daily-trade-quote")?;
            let threshold = Decimal::from_str(&threshold_quote).context("Invalid --threshold-quote")?;

            run_treasury(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                target, approval_threshold, max_trade, max_daily, approval_queue, threshold,
                rebalance_fraction, check_interval,
            ).await
        }
    }
}

struct CloudBalanceProvider {
    client: TokioMutex<DAppProviderClient>,
}
#[async_trait]
impl BalanceProvider for CloudBalanceProvider {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
        self.client.lock().await.get_balances().await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_treasury(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    targets: Vec<TargetSpec>,
    approval_threshold: Decimal,
    max_trade: Decimal,
    max_daily: Decimal,
    approval_queue: PathBuf,
    threshold_quote: Decimal,
    rebalance_fraction: f64,
    check_interval: u64,
) -> Result<()> {
    info!("Starting Treasury Management");
    info!("Party: {}", config.party_id);
    for t in &targets {
        info!("  target: {}@{} = {}", t.instrument, t.market, t.target_quote_value);
    }
    info!(
        "approval_threshold={} max_trade={} max_daily={} threshold={} fraction={} interval={}s queue={:?}",
        approval_threshold, max_trade, max_daily, threshold_quote, rebalance_fraction, check_interval, approval_queue
    );

    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let runner_shutdown = agent_logic::shutdown::Shutdown::new();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
        runner_shutdown.clone(),
    );
    let ledger_client = DAppProviderClient::new(
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
    .await?;
    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    let frac = Decimal::from_str(&format!("{}", rebalance_fraction)).unwrap_or(Decimal::ONE);
    let shutdown = Arc::new(AtomicBool::new(false));
    let loop_cfg = config.clone();
    let loop_sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = treasury_loop(
            loop_cfg, targets, approval_threshold, max_trade, max_daily, approval_queue,
            threshold_quote, frac, check_interval, dry_run, loop_sd,
        )
        .await
        {
            error!("treasury loop failed: {:#}", e);
        }
    });

    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only: true,
            orders_only: false,
            actionable_count: None,
            shutdown: Some(runner_shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(shutdown.clone())),
            state_file: Some(PathBuf::from("treasury-mgmt-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[derive(Serialize)]
struct ApprovalSubmission<'a> {
    market: &'a str,
    side: &'static str,
    quantity: String,
    price: String,
    r#ref: String,
}

#[allow(clippy::too_many_arguments)]
async fn treasury_loop(
    config: BaseConfig,
    targets: Vec<TargetSpec>,
    approval_threshold: Decimal,
    max_trade: Decimal,
    max_daily: Decimal,
    approval_queue: PathBuf,
    threshold_quote: Decimal,
    fraction: Decimal,
    check_interval: u64,
    dry_run: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut ob = OrderbookClient::new(&config).await?;
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
    .await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    // Rolling 24h notional ledger
    let mut daily: VecDeque<(DateTime<Utc>, Decimal)> = VecDeque::new();

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Treasury loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Err(e) = treasury_sweep(
            &mut ob, &mut ledger, &tracker,
            &targets, approval_threshold, max_trade, max_daily, &approval_queue,
            threshold_quote, fraction, dry_run, &mut daily,
        )
        .await
        {
            warn!("sweep failed: {:#}", e);
        }
        sleep_or_break(check_interval, &shutdown).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn treasury_sweep(
    ob: &mut OrderbookClient,
    ledger: &mut DAppProviderClient,
    tracker: &OrderTracker,
    targets: &[TargetSpec],
    approval_threshold: Decimal,
    max_trade: Decimal,
    max_daily: Decimal,
    approval_queue: &PathBuf,
    threshold_quote: Decimal,
    fraction: Decimal,
    dry_run: bool,
    daily: &mut VecDeque<(DateTime<Utc>, Decimal)>,
) -> Result<()> {
    // Trim daily ledger
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    while let Some(front) = daily.front() {
        if front.0 < cutoff { daily.pop_front(); } else { break; }
    }
    let used_today: Decimal = daily.iter().fold(Decimal::ZERO, |acc, (_, n)| acc + *n);

    let balances = ledger.get_balances().await?;
    let by_inst: HashMap<&str, Decimal> = balances
        .iter()
        .map(|b| (b.instrument_id.as_str(), Decimal::from_str(&b.unlocked_amount).unwrap_or(Decimal::ZERO)))
        .collect();

    for t in targets {
        let mid = match ob.get_price(&t.market).await {
            Ok(p) => mid_decimal(&p),
            Err(e) => { warn!("get_price({}): {:#}", t.market, e); continue; }
        };
        if mid <= Decimal::ZERO { continue; }
        let bal = by_inst.get(t.instrument.as_str()).copied().unwrap_or(Decimal::ZERO);
        let value = bal * mid;
        let delta_value = value - t.target_quote_value;
        info!(
            "{}@{}: bal={} mid={} value={} target={} delta={:+}",
            t.instrument, t.market, bal, mid, value, t.target_quote_value, delta_value
        );
        if delta_value.abs() <= threshold_quote { continue; }

        let mut notional = (delta_value.abs() * fraction).min(max_trade);
        let remaining_daily = max_daily - used_today;
        if remaining_daily <= Decimal::ZERO {
            warn!("daily trade budget exhausted ({}/{}) — skipping {}", used_today, max_daily, t.instrument);
            continue;
        }
        if notional > remaining_daily {
            notional = remaining_daily;
        }
        let qty = notional / mid;
        if qty <= Decimal::ZERO { continue; }

        let (order_type, label) = if delta_value > Decimal::ZERO {
            (OrderType::Offer, "OFFER")
        } else {
            (OrderType::Bid, "BID")
        };
        let route_to_approval = notional > approval_threshold;

        if route_to_approval {
            warn!(
                "TREASURY APPROVAL REQUIRED: {} {} on {} qty={} (notional={} > threshold={})",
                label, t.instrument, t.market, qty, notional, approval_threshold
            );
            if dry_run {
                info!("  [dry-run] would enqueue to {:?}", approval_queue);
                continue;
            }
            enqueue_approval(approval_queue, t, label, qty.clone(), mid.clone()).await?;
            // Daily budget reserves the trade even though it hasn't fired yet.
            daily.push_back((Utc::now(), notional));
            continue;
        }

        info!("TREASURY AUTO: {} {} on {} qty={} (notional={})", label, t.instrument, t.market, qty, notional);
        if dry_run {
            info!("  [dry-run] would submit_order");
            continue;
        }
        let price = mid.round_dp(8);
        let (signature, signed_data, nonce) =
            tracker.sign_order(&t.market, label, &price.to_string(), &qty.to_string());
        match ob
            .submit_order(
                &t.market,
                order_type,
                price.to_string(),
                qty.to_string(),
                Some(format!("treas-{}-{}", label, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => {
                info!("  → order id={}", resp.order.as_ref().map(|o| o.order_id).unwrap_or(0));
                daily.push_back((Utc::now(), notional));
            }
            Err(e) => warn!("  submit failed: {:#}", e),
        }
    }
    Ok(())
}

async fn enqueue_approval(
    queue: &PathBuf,
    target: &TargetSpec,
    label: &'static str,
    qty: Decimal,
    price: Decimal,
) -> Result<()> {
    let side = if label == "BID" { "buy" } else { "sell" };
    let entry = serde_json::json!({
        "id": format!("treas-{}-{}", target.instrument, chrono::Utc::now().timestamp_millis()),
        "ts_enqueued": Utc::now().to_rfc3339(),
        "status": "pending",
        "order": {
            "market": target.market,
            "side": side,
            "quantity": qty.to_string(),
            "price": price.to_string(),
            "ref": format!("treasury-{}", target.instrument),
        },
        "decided_at": null,
        "decided_by": null,
        "reason": null,
        "submit_order_id": null,
    });
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(queue)
        .await
        .with_context(|| format!("open queue {:?}", queue))?;
    let line = serde_json::to_string(&entry)? + "\n";
    f.write_all(line.as_bytes()).await?;
    info!("queued approval entry to {:?}", queue);
    let _ = ApprovalSubmission {
        market: &target.market,
        side: label,
        quantity: qty.to_string(),
        price: price.to_string(),
        r#ref: target.instrument.clone(),
    };
    Ok(())
}

fn mid_decimal(p: &orderbook_proto::pricing::GetPriceResponse) -> Decimal {
    let mid = match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    };
    if mid > 0.0 {
        Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO)
    } else {
        Decimal::ZERO
    }
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
