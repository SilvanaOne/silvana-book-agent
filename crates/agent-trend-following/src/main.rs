//! Trend Following Agent — EMA crossover momentum
//!
//! Maintains two EMAs of the live mid: `fast_ema` (period `--fast`) and
//! `slow_ema` (period `--slow`). At each poll:
//!
//! - Fast crosses **above** slow (was ≤, now >) → **BULLISH** signal: place a
//!   BID at mid for `--quantity` (entry long).
//! - Fast crosses **below** slow (was ≥, now <) → **BEARISH** signal: place an
//!   OFFER at mid for `--quantity` (entry short, or exit long).
//!
//! Skips re-entry on the same side while a previous order is still open
//! (prevents stacking). Warm-up: `--warmup-samples` ticks are observed before
//! any signal is emitted, so the EMAs are stable.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::order_tracker::OrderTracker;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::{OrderStatus, OrderType};

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-trend-following")]
#[command(about = "EMA crossover momentum trader")]
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
        market: String,

        #[arg(long, default_value = "9")]
        fast: u32,

        #[arg(long, default_value = "21")]
        slow: u32,

        #[arg(long)]
        quantity: String,

        #[arg(long, default_value = "10")]
        poll_secs: u64,

        #[arg(long, default_value = "30")]
        warmup_samples: u32,

        #[arg(long)]
        no_restore: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_trend_following", "orderbook_agent_logic", "tx_verifier"],
        "agent-trend-following",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            fast,
            slow,
            quantity,
            poll_secs,
            warmup_samples,
            no_restore,
        } => {
            run_trend(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                market, fast, slow, quantity, poll_secs, warmup_samples,
            )
            .await
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
async fn run_trend(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    fast: u32,
    slow: u32,
    quantity: String,
    poll_secs: u64,
    warmup_samples: u32,
) -> Result<()> {
    if fast == 0 || slow == 0 || fast >= slow {
        anyhow::bail!("require 0 < --fast < --slow");
    }
    let qty = Decimal::from_str(&quantity).context("Invalid --quantity")?;

    info!("Starting Trend Following");
    info!("Party: {}", config.party_id);
    info!(
        "market={} fast={} slow={} qty={} poll={}s warmup={}",
        market, fast, slow, qty, poll_secs, warmup_samples
    );

    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
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
    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
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
        if let Err(e) = trend_loop(
            loop_cfg, market, fast, slow, qty, poll_secs, warmup_samples, loop_sd,
        )
        .await
        {
            error!("Trend loop failed: {:#}", e);
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
            shutdown_notify: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(shutdown),
            state_file: Some(PathBuf::from("trend-following-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn trend_loop(
    config: BaseConfig,
    market: String,
    fast: u32,
    slow: u32,
    quantity: Decimal,
    poll_secs: u64,
    warmup_samples: u32,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let alpha_fast = 2.0 / (fast as f64 + 1.0);
    let alpha_slow = 2.0 / (slow as f64 + 1.0);
    let mut fast_ema: Option<f64> = None;
    let mut slow_ema: Option<f64> = None;
    let mut samples: u32 = 0;
    let mut last_diff_sign: i8 = 0; // +1, 0, -1

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Trend loop started (alpha_fast={:.4}, alpha_slow={:.4})", alpha_fast, alpha_slow);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mid = match client.get_price(&market).await {
            Ok(p) => mid_value(&p),
            Err(e) => {
                warn!("get_price failed: {:#}", e);
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        if mid <= 0.0 {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        fast_ema = Some(match fast_ema { None => mid, Some(p) => alpha_fast * mid + (1.0 - alpha_fast) * p });
        slow_ema = Some(match slow_ema { None => mid, Some(p) => alpha_slow * mid + (1.0 - alpha_slow) * p });
        samples += 1;

        let (f, s) = (fast_ema.unwrap(), slow_ema.unwrap());
        let diff = f - s;
        let sign: i8 = if diff > 0.0 { 1 } else if diff < 0.0 { -1 } else { 0 };

        info!(
            "mid={:.6} fast_ema={:.6} slow_ema={:.6} diff={:+.6}",
            mid, f, s, diff
        );

        if samples < warmup_samples {
            last_diff_sign = sign;
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let cross_up = last_diff_sign <= 0 && sign > 0;
        let cross_down = last_diff_sign >= 0 && sign < 0;
        last_diff_sign = sign;

        if cross_up {
            if has_open(&mut client, &market, OrderType::Bid).await {
                info!("BULLISH crossover but open BID exists — skipping");
            } else {
                let price_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);
                info!("BULLISH crossover — entering long at mid={}", price_dec);
                place(&mut client, &tracker, &market, OrderType::Bid, "BID", &price_dec, &quantity).await;
            }
        } else if cross_down {
            if has_open(&mut client, &market, OrderType::Offer).await {
                info!("BEARISH crossover but open OFFER exists — skipping");
            } else {
                let price_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);
                info!("BEARISH crossover — entering short at mid={}", price_dec);
                place(&mut client, &tracker, &market, OrderType::Offer, "OFFER", &price_dec, &quantity).await;
            }
        }

        sleep_or_break(poll_secs, &shutdown).await;
    }
}

async fn has_open(client: &mut OrderbookClient, market: &str, side: OrderType) -> bool {
    match client.get_active_orders(market).await {
        Ok(orders) => orders.iter().any(|o| {
            o.order_type == side as i32
                && (o.status == OrderStatus::Active as i32 || o.status == OrderStatus::Partial as i32)
        }),
        Err(_) => false,
    }
}

async fn place(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: &Decimal,
    qty: &Decimal,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("trend-{}-{}", label, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  {} placed id={}",
            label,
            resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
        ),
        Err(e) => warn!("  {} submit failed: {:#}", label, e),
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

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
