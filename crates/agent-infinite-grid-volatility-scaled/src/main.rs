//! Infinite Grid Agent — volatility-scaled step
//!
//! Sibling of `agent-infinite-grid-mid-following`. The ladder is still rebuilt
//! around the current mid every refresh, but `step_pct` is *not* fixed by CLI:
//! it's derived from a **rolling realized volatility** window over the recent
//! poll samples.
//!
//! - Every `--sample-secs` seconds the agent polls mid and appends `ln(mid/prev)`
//!   to a ring buffer of length `--vol-window`.
//! - `sigma = sample stddev of log returns` (annualization is unnecessary here —
//!   we want per-poll-period spread).
//! - `step_pct = clamp(step_multiplier × sigma × 100, min_step_pct, max_step_pct)`.
//! - Levels are `mid × (1 ± step_pct × i / 100)` for i = 1..N (arithmetic, same
//!   as `mid-following`).
//!
//! Effect: grid tightens when the market is calm and widens when it churns —
//! catching fills in either regime without leaving inventory stuck far from mid.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
#[command(name = "agent-infinite-grid-volatility-scaled")]
#[command(about = "Infinite grid whose step is derived from rolling realized volatility")]
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

        /// Number of grid levels per side
        #[arg(long)]
        levels: u32,

        /// Quantity per level
        #[arg(long)]
        quantity_per_level: String,

        /// Sample interval — how often the vol window is updated
        #[arg(long, default_value = "10")]
        sample_secs: u64,

        /// Number of samples kept in the rolling vol window
        #[arg(long, default_value = "60")]
        vol_window: usize,

        /// Multiplier applied to sigma to derive step_pct
        #[arg(long, default_value = "2.0")]
        step_multiplier: f64,

        /// Lower bound on step_pct (safety floor)
        #[arg(long, default_value = "0.1")]
        min_step_pct: f64,

        /// Upper bound on step_pct (safety ceiling)
        #[arg(long, default_value = "5.0")]
        max_step_pct: f64,

        /// How often to rebuild the ladder
        #[arg(long, default_value = "60")]
        refresh_secs: u64,

        #[arg(long)]
        no_restore: bool,
    },
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_infinite_grid", "agent_logic", "tx_verifier"],
        "agent-infinite-grid-volatility-scaled",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            levels,
            quantity_per_level,
            sample_secs,
            vol_window,
            step_multiplier,
            min_step_pct,
            max_step_pct,
            refresh_secs,
            no_restore,
        } => {
            run_grid(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                levels,
                quantity_per_level,
                sample_secs,
                vol_window,
                step_multiplier,
                min_step_pct,
                max_step_pct,
                refresh_secs,
            )
            .await
        }
        Commands::Status => run_status(config).await,
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
async fn run_grid(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    levels: u32,
    quantity_per_level: String,
    sample_secs: u64,
    vol_window: usize,
    step_multiplier: f64,
    min_step_pct: f64,
    max_step_pct: f64,
    refresh_secs: u64,
) -> Result<()> {
    if levels == 0 {
        anyhow::bail!("--levels must be >= 1");
    }
    if vol_window < 3 {
        anyhow::bail!("--vol-window must be >= 3 samples");
    }
    if step_multiplier <= 0.0 {
        anyhow::bail!("--step-multiplier must be > 0");
    }
    if min_step_pct <= 0.0 || max_step_pct <= min_step_pct {
        anyhow::bail!("expected 0 < min-step-pct < max-step-pct");
    }
    let qty = Decimal::from_str(&quantity_per_level).context("Invalid --quantity-per-level")?;
    if qty <= Decimal::ZERO {
        anyhow::bail!("--quantity-per-level must be > 0");
    }

    info!("Starting Infinite Grid (volatility-scaled)");
    info!("Party: {}", config.party_id);
    info!(
        "market={} levels={} qty={} vol_window={} sample={}s step_mult={} bounds=[{}, {}] refresh={}s",
        market,
        levels,
        qty,
        vol_window,
        sample_secs,
        step_multiplier,
        min_step_pct,
        max_step_pct,
        refresh_secs
    );

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
                    liquidity_manager
                        .register_alias(parts[0], &m.base_instrument)
                        .await;
                    liquidity_manager
                        .register_alias(parts[1], &m.quote_instrument)
                        .await;
                }
            }
        }
    }
    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let runner_shutdown = agent_logic::shutdown::Shutdown::new();
    let backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
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
        if let Err(e) = grid_loop(
            loop_cfg,
            market,
            levels,
            qty,
            sample_secs,
            vol_window,
            step_multiplier,
            min_step_pct,
            max_step_pct,
            refresh_secs,
            loop_sd,
        )
        .await
        {
            error!("Volatility-scaled grid loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("infinite-grid-vol-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn grid_loop(
    config: BaseConfig,
    market: String,
    levels: u32,
    quantity: Decimal,
    sample_secs: u64,
    vol_window: usize,
    step_multiplier: f64,
    min_step_pct: f64,
    max_step_pct: f64,
    refresh_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let tick_size = client.get_tick_size(&market).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!(
        "Volatility-scaled grid loop started (tick={}, warming up vol window)",
        tick_size
    );

    let mut log_returns: VecDeque<f64> = VecDeque::with_capacity(vol_window);
    let mut prev_sample: Option<f64> = None;
    let mut last_sample_at = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(sample_secs))
        .unwrap_or_else(std::time::Instant::now);
    let mut last_rebuild = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(refresh_secs))
        .unwrap_or_else(std::time::Instant::now);

    // Inner poll cadence is min(sample_secs, refresh_secs) but at least 1s
    let tick_cadence = sample_secs.min(refresh_secs).max(1);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let now = std::time::Instant::now();

        // --- 1. Sample the vol window on schedule -----------------------------------
        if now.duration_since(last_sample_at).as_secs() >= sample_secs {
            match client.get_price(&market).await {
                Ok(p) => {
                    let mid = mid_value(&p);
                    if mid > 0.0 {
                        if let Some(prev) = prev_sample {
                            if prev > 0.0 {
                                log_returns.push_back((mid / prev).ln());
                                while log_returns.len() > vol_window {
                                    log_returns.pop_front();
                                }
                            }
                        }
                        prev_sample = Some(mid);
                        last_sample_at = now;
                    }
                }
                Err(e) => warn!("sample get_price failed: {:#}", e),
            }
        }

        // --- 2. Rebuild the ladder on schedule --------------------------------------
        if now.duration_since(last_rebuild).as_secs() >= refresh_secs {
            let sigma = sample_stddev(&log_returns);
            let raw_step = step_multiplier * sigma * 100.0;
            let step_pct = raw_step.clamp(min_step_pct, max_step_pct);

            match client.get_price(&market).await {
                Ok(p) => {
                    let mid = mid_value(&p);
                    if mid <= 0.0 {
                        sleep_or_break(tick_cadence, &shutdown).await;
                        continue;
                    }

                    // Cancel existing own orders
                    if let Ok(orders) = client.get_active_orders(&market).await {
                        for o in orders {
                            if let Err(e) = client.cancel_order(o.order_id).await {
                                warn!("cancel order_id={}: {:#}", o.order_id, e);
                            }
                        }
                    }

                    info!(
                        "rebuild: mid={:.6} sigma={:.6} raw_step={:.4}% clamped step_pct={:.4}% samples={}",
                        mid,
                        sigma,
                        raw_step,
                        step_pct,
                        log_returns.len()
                    );

                    let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);
                    for i in 1..=levels {
                        let offset =
                            Decimal::from_str(&format!("{}", step_pct * i as f64 / 100.0))
                                .unwrap_or(Decimal::ZERO);
                        let bid_price = agent_logic::tick::round_to_tick(
                            mid_dec * (Decimal::ONE - offset),
                            tick_size,
                        );
                        let offer_price = agent_logic::tick::round_to_tick(
                            mid_dec * (Decimal::ONE + offset),
                            tick_size,
                        );

                        place(
                            &mut client,
                            &tracker,
                            &market,
                            OrderType::Bid,
                            "BID",
                            &bid_price,
                            &quantity,
                            i,
                        )
                        .await;
                        place(
                            &mut client,
                            &tracker,
                            &market,
                            OrderType::Offer,
                            "OFFER",
                            &offer_price,
                            &quantity,
                            i,
                        )
                        .await;
                    }
                }
                Err(e) => warn!("rebuild get_price failed: {:#}", e),
            }

            last_rebuild = now;
        }

        sleep_or_break(tick_cadence, &shutdown).await;
    }
}

fn sample_stddev(xs: &VecDeque<f64>) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let mean: f64 = xs.iter().sum::<f64>() / n as f64;
    let var: f64 = xs.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    var.sqrt()
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
    level: u32,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!(
                "inf-vol-{}-{}-{}",
                label,
                level,
                chrono::Utc::now().timestamp_millis()
            )),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  L{} {} @ {} id={}",
            level,
            label,
            price,
            resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
        ),
        Err(e) => warn!("  L{} {} submit failed: {:#}", level, label, e),
    }
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}

async fn run_status(config: BaseConfig) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
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
    println!("Party: {}", config.party_id);
    let balances = ledger.get_balances().await?;
    println!("\nBalances:");
    for b in &balances {
        println!(
            "  {} total={} locked={} unlocked={}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount
        );
    }
    let orders = client.get_all_active_orders().await?;
    println!("\nActive grid orders ({}):", orders.len());
    for o in &orders {
        println!(
            "  #{} {} {} @ {} qty={}",
            o.order_id,
            if o.order_type == OrderType::Bid as i32 {
                "BID"
            } else {
                "OFFER"
            },
            o.market_id,
            o.price,
            o.quantity
        );
    }
    Ok(())
}
