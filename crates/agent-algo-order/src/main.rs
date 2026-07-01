//! Algo Order Agent — pluggable execution dispatcher
//!
//! Reads a "plan" TOML listing one or more algo executions. Each entry picks
//! an algorithm and supplies its parameters; the agent runs them sequentially
//! against the orderbook. Currently supported algorithms (in-process
//! implementations — not external binaries):
//!
//! - **twap**: split `total` into `slices` equal pieces at intervals of
//!   `duration_secs / slices`.
//! - **iceberg**: place visible chunks, wait for each to clear, repeat until
//!   `total` is filled.
//! - **liquidity-seeking**: walk the depth book each cycle, size the chunk to
//!   the largest qty that respects `max_slippage_bps` (capped by `max_chunk`),
//!   place, wait, repeat.
//!
//! Plan TOML:
//!
//! ```toml
//! [[step]]
//! algorithm = "twap"
//! market = "CC-USDC"
//! side = "buy"
//! total = "100"
//! slices = 10
//! duration_secs = 600
//! price_offset_pct = 0.0
//!
//! [[step]]
//! algorithm = "iceberg"
//! market = "CC-USDC"
//! side = "sell"
//! total = "50"
//! visible = "1"
//! price = "1.05"
//!
//! [[step]]
//! algorithm = "liquidity-seeking"
//! market = "BTC-USD"
//! side = "buy"
//! total = "5"
//! max_chunk = "0.1"
//! max_slippage_bps = 25
//! depth = 20
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::{OrderStatus, OrderType, OrderbookLevel};


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-algo-order")]
#[command(about = "Pluggable execution dispatcher (TWAP / Iceberg / Liquidity-Seeking)")]
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
        #[arg(long)]
        plan: PathBuf,

        #[arg(long)]
        no_restore: bool,
    },
    Check {
        #[arg(long)]
        plan: PathBuf,
    },
}

#[derive(Deserialize, Clone)]
struct Plan {
    #[serde(rename = "step")]
    steps: Vec<Step>,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "algorithm", rename_all = "kebab-case")]
enum Step {
    Twap {
        market: String,
        side: String,
        total: String,
        slices: u32,
        duration_secs: u64,
        #[serde(default)]
        price_offset_pct: Option<f64>,
        #[serde(default)]
        limit_price: Option<String>,
    },
    Iceberg {
        market: String,
        side: String,
        total: String,
        visible: String,
        price: String,
        #[serde(default = "default_poll_secs")]
        poll_secs: u64,
    },
    LiquiditySeeking {
        market: String,
        side: String,
        total: String,
        max_chunk: String,
        #[serde(default = "default_slippage_bps")]
        max_slippage_bps: u32,
        #[serde(default = "default_depth")]
        depth: u32,
        #[serde(default = "default_poll_secs")]
        poll_secs: u64,
    },
}

fn default_poll_secs() -> u64 { 5 }
fn default_slippage_bps() -> u32 { 25 }
fn default_depth() -> u32 { 20 }

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_algo_order", "agent_logic", "tx_verifier"],
        "agent-algo-order",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run { plan, no_restore } => {
            let p = load_plan(&plan).await?;
            run_plan(config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore, p).await
        }
        Commands::Check { plan } => {
            let p = load_plan(&plan).await?;
            println!("Loaded plan with {} steps", p.steps.len());
            for (i, s) in p.steps.iter().enumerate() {
                println!("  step {}: {}", i + 1, step_name(s));
            }
            Ok(())
        }
    }
}

fn step_name(s: &Step) -> &'static str {
    match s {
        Step::Twap { .. } => "twap",
        Step::Iceberg { .. } => "iceberg",
        Step::LiquiditySeeking { .. } => "liquidity-seeking",
    }
}

async fn load_plan(path: &PathBuf) -> Result<Plan> {
    let body = tokio::fs::read_to_string(path).await.with_context(|| format!("read {:?}", path))?;
    toml::from_str(&body).with_context(|| format!("parse {:?}", path))
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
async fn run_plan(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    plan: Plan,
) -> Result<()> {
    info!("Starting Algo Order Dispatcher");
    info!("Party: {}", config.party_id);
    info!("Plan: {} steps", plan.steps.len());

    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    {
        let mut client = OrderbookClient::new(&config).await?;
        if let Ok(markets) = client.get_markets().await {
            for m in &markets {
                let parts: Vec<&str> = m.market_id.split('-').collect();
                if parts.len() == 2 {
                    liquidity_manager.register_alias(parts[0], &m.base_instrument).await;
                    liquidity_manager.register_alias(parts[1], &m.quote_instrument).await;
                }
            }
        }
    }
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

    let shutdown = Arc::new(AtomicBool::new(false));
    let loop_cfg = config.clone();
    let loop_sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = dispatcher(loop_cfg, plan, loop_sd).await {
            error!("dispatcher failed: {:#}", e);
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
            state_file: Some(PathBuf::from("algo-order-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn dispatcher(config: BaseConfig, plan: Plan, shutdown: Arc<AtomicBool>) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    for (i, step) in plan.steps.iter().enumerate() {
        if shutdown.load(Ordering::Relaxed) {
            info!("dispatcher: shutdown");
            return Ok(());
        }
        info!("=== step {}/{}: {} ===", i + 1, plan.steps.len(), step_name(step));
        match step {
            Step::Twap { market, side, total, slices, duration_secs, price_offset_pct, limit_price } => {
                run_twap(&mut client, &tracker, market, side, total, *slices, *duration_secs,
                         price_offset_pct.unwrap_or(0.0), limit_price.as_deref(), &shutdown).await?;
            }
            Step::Iceberg { market, side, total, visible, price, poll_secs } => {
                run_iceberg(&mut client, &tracker, market, side, total, visible, price, *poll_secs, &shutdown).await?;
            }
            Step::LiquiditySeeking { market, side, total, max_chunk, max_slippage_bps, depth, poll_secs } => {
                run_liquidity_seeking(
                    &mut client, &tracker, market, side, total, max_chunk,
                    *max_slippage_bps, *depth, *poll_secs, &shutdown,
                ).await?;
            }
        }
    }
    info!("Plan complete");
    Ok(())
}

// ------------------------------------------------------------------ TWAP
#[allow(clippy::too_many_arguments)]
async fn run_twap(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    side: &str,
    total: &str,
    slices: u32,
    duration_secs: u64,
    price_offset_pct: f64,
    limit_price: Option<&str>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let order_type = parse_side(side)?;
    let total_d = Decimal::from_str(total).context("twap.total")?;
    if slices == 0 { anyhow::bail!("twap.slices must be >= 1"); }
    if duration_secs == 0 { anyhow::bail!("twap.duration_secs must be >= 1"); }
    let slice_qty = total_d / Decimal::from(slices);
    let interval = duration_secs / slices as u64;
    let limit_d = limit_price.map(Decimal::from_str).transpose().context("twap.limit_price")?;
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };

    info!("twap: total={} slices={} interval={}s", total_d, slices, interval);
    for i in 1..=slices {
        if shutdown.load(Ordering::Relaxed) { return Ok(()); }
        let price = match client.get_price(market).await {
            Ok(p) => p,
            Err(e) => { warn!("get_price: {:#}", e); sleep_or_break(interval, shutdown).await; continue; }
        };
        let mid = mid_decimal(&price);
        if mid <= Decimal::ZERO {
            sleep_or_break(interval, shutdown).await;
            continue;
        }
        let order_price = (mid * (Decimal::ONE
            + Decimal::from_str(&format!("{}", price_offset_pct / 100.0)).unwrap_or(Decimal::ZERO)))
        .round_dp(8);
        if let Some(limit) = limit_d {
            let bad = match order_type {
                OrderType::Bid => order_price > limit,
                OrderType::Offer => order_price < limit,
                _ => false,
            };
            if bad {
                warn!("twap slice {}: price {} outside limit {} — skipping", i, order_price, limit);
                sleep_or_break(interval, shutdown).await;
                continue;
            }
        }
        place(client, tracker, market, order_type, label, &order_price, &slice_qty, &format!("twap-{}", i)).await;
        if i < slices {
            sleep_or_break(interval, shutdown).await;
        }
    }
    Ok(())
}

// --------------------------------------------------------------- ICEBERG
#[allow(clippy::too_many_arguments)]
async fn run_iceberg(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    side: &str,
    total: &str,
    visible: &str,
    price: &str,
    poll_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let order_type = parse_side(side)?;
    let total_d = Decimal::from_str(total).context("iceberg.total")?;
    let visible_d = Decimal::from_str(visible).context("iceberg.visible")?;
    let price_d = Decimal::from_str(price).context("iceberg.price")?;
    if visible_d > total_d { anyhow::bail!("iceberg.visible must be <= total"); }
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };

    info!("iceberg: total={} visible={} price={}", total_d, visible_d, price_d);
    let mut filled = Decimal::ZERO;
    let mut n: u32 = 0;
    while filled < total_d {
        if shutdown.load(Ordering::Relaxed) { return Ok(()); }
        n += 1;
        let remaining = total_d - filled;
        let qty = if remaining < visible_d { remaining } else { visible_d };
        let order_id = match submit(client, tracker, market, order_type, &price_d, &qty, label, &format!("ice-{}", n)).await {
            Some(id) => id,
            None => { sleep_or_break(poll_secs, shutdown).await; continue; }
        };
        let chunk = wait_clear(client, market, order_id, poll_secs, shutdown).await;
        filled += chunk;
        info!("iceberg chunk #{} done: chunk={} parent={}/{}", n, chunk, filled, total_d);
    }
    Ok(())
}

// ----------------------------------------------------- LIQUIDITY-SEEKING
#[allow(clippy::too_many_arguments)]
async fn run_liquidity_seeking(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    side: &str,
    total: &str,
    max_chunk: &str,
    max_slippage_bps: u32,
    depth: u32,
    poll_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let order_type = parse_side(side)?;
    let total_d = Decimal::from_str(total).context("liquidity-seeking.total")?;
    let max_chunk_d = Decimal::from_str(max_chunk).context("liquidity-seeking.max_chunk")?;
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };
    let _started = Instant::now();

    info!("liquidity-seeking: total={} max_chunk={} bps={}", total_d, max_chunk_d, max_slippage_bps);
    let mut filled = Decimal::ZERO;
    let mut cycle: u32 = 0;
    while filled < total_d {
        if shutdown.load(Ordering::Relaxed) { return Ok(()); }
        cycle += 1;
        let dep = match client.get_orderbook_depth(market, Some(depth)).await? {
            Some(d) => d,
            None => { sleep_or_break(poll_secs, shutdown).await; continue; }
        };
        let mid_dec = match (dep.bids.first().and_then(level_price), dep.offers.first().and_then(level_price)) {
            (Some(b), Some(o)) => Some((b + o) / Decimal::from(2)),
            _ => None,
        };
        let Some(mid) = mid_dec else { sleep_or_break(poll_secs, shutdown).await; continue };
        let levels: &[OrderbookLevel] = if order_type == OrderType::Bid { &dep.offers } else { &dep.bids };
        let bps_limit = Decimal::from(max_slippage_bps);
        let (safe_qty, boundary) = max_safe_qty(levels, mid, bps_limit, order_type);
        if safe_qty <= Decimal::ZERO {
            sleep_or_break(poll_secs, shutdown).await;
            continue;
        }
        let remaining = total_d - filled;
        let chunk = safe_qty.min(max_chunk_d).min(remaining);
        if chunk <= Decimal::ZERO {
            sleep_or_break(poll_secs, shutdown).await;
            continue;
        }
        let order_id = match submit(client, tracker, market, order_type, &boundary, &chunk, label, &format!("liq-{}", cycle)).await {
            Some(id) => id,
            None => { sleep_or_break(poll_secs, shutdown).await; continue; }
        };
        let actual = wait_clear(client, market, order_id, poll_secs, shutdown).await;
        filled += actual;
        info!("liquidity-seeking cycle #{}: chunk={} parent={}/{}", cycle, actual, filled, total_d);
    }
    Ok(())
}

fn max_safe_qty(
    levels: &[OrderbookLevel],
    mid: Decimal,
    max_bps: Decimal,
    side: OrderType,
) -> (Decimal, Decimal) {
    let mut accum_qty = Decimal::ZERO;
    let mut accum_cost = Decimal::ZERO;
    let mut last_price = mid;
    let bps_unit = Decimal::from(10000);
    for l in levels {
        let lvl_qty = Decimal::from_str(&l.total_quantity).unwrap_or(Decimal::ZERO);
        let lvl_px = Decimal::from_str(&l.price).unwrap_or(Decimal::ZERO);
        if lvl_qty <= Decimal::ZERO || lvl_px <= Decimal::ZERO { continue; }
        let cand_qty = accum_qty + lvl_qty;
        let cand_cost = accum_cost + lvl_qty * lvl_px;
        let vwap = cand_cost / cand_qty;
        let dev = match side {
            OrderType::Bid => vwap - mid,
            OrderType::Offer => mid - vwap,
            _ => Decimal::ZERO,
        };
        let bps = dev.abs() / mid * bps_unit;
        if bps > max_bps { break; }
        accum_qty = cand_qty;
        accum_cost = cand_cost;
        last_price = lvl_px;
    }
    (accum_qty, last_price)
}

// -------------------------------------------------------------- HELPERS
fn parse_side(side: &str) -> Result<OrderType> {
    match side.to_lowercase().as_str() {
        "buy" => Ok(OrderType::Bid),
        "sell" => Ok(OrderType::Offer),
        other => anyhow::bail!("side must be 'buy' or 'sell', got {}", other),
    }
}

#[allow(clippy::too_many_arguments)]
async fn place(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: &Decimal,
    qty: &Decimal,
    tag: &str,
) {
    let _ = submit(client, tracker, market, order_type, price, qty, label, tag).await;
}

async fn submit(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    price: &Decimal,
    qty: &Decimal,
    label: &'static str,
    tag: &str,
) -> Option<u64> {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("{}-{}", tag, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => {
            let id = resp.order.as_ref().map(|o| o.order_id);
            info!("  submitted {} {} @ {} id={:?}", label, qty, price, id);
            id
        }
        Err(e) => {
            warn!("  submit failed: {:#}", e);
            None
        }
    }
}

async fn wait_clear(
    client: &mut OrderbookClient,
    market: &str,
    order_id: u64,
    poll_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Decimal {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Decimal::ZERO;
        }
        sleep_or_break(poll_secs, shutdown).await;
        let orders = match client.get_active_orders(market).await {
            Ok(o) => o,
            Err(_) => continue,
        };
        match orders.iter().find(|o| o.order_id == order_id) {
            Some(o) => {
                let st = OrderStatus::try_from(o.status).unwrap_or(OrderStatus::Unspecified);
                if !matches!(st, OrderStatus::Active | OrderStatus::Partial) {
                    return Decimal::from_str(&o.filled_quantity).unwrap_or(Decimal::ZERO);
                }
            }
            None => return Decimal::ZERO,
        }
    }
}

fn level_price(l: &OrderbookLevel) -> Option<Decimal> {
    Decimal::from_str(&l.price).ok()
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
