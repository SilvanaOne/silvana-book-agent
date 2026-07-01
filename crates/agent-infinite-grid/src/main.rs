//! Infinite Grid Agent — grid that follows the mid price
//!
//! Unlike `agent-spot-grid` (which reads static `[[markets.bid_levels]]` /
//! `[[markets.offer_levels]]` from `agent.toml`), this agent regenerates its
//! ladder around the *current* mid every refresh cycle:
//!
//! - Level i (1..=N) bid price  = mid × (1 − step_pct × i / 100)
//! - Level i (1..=N) offer price = mid × (1 + step_pct × i / 100)
//!
//! Each refresh cancels existing own orders on the market and re-quotes the
//! whole ladder. There is no fixed price range — the grid drifts with the
//! market indefinitely. Useful for trending or wide-range conditions where a
//! static grid would have its outer levels left far from price.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
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
#[command(name = "agent-infinite-grid")]
#[command(about = "Dynamic grid that follows the mid price (no fixed range)")]
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

        /// Spacing between adjacent grid levels in percent
        #[arg(long)]
        step_pct: f64,

        /// Number of levels per side
        #[arg(long)]
        levels: u32,

        /// Quantity per individual level
        #[arg(long)]
        quantity_per_level: String,

        /// How often to recompute and re-quote the ladder
        #[arg(long, default_value = "60")]
        refresh_secs: u64,

        /// Only re-quote when mid drifts by at least this many percent since the last refresh
        #[arg(long, default_value = "0.0")]
        drift_threshold_pct: f64,

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
        "agent-infinite-grid",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            step_pct,
            levels,
            quantity_per_level,
            refresh_secs,
            drift_threshold_pct,
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
                step_pct,
                levels,
                quantity_per_level,
                refresh_secs,
                drift_threshold_pct,
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
    step_pct: f64,
    levels: u32,
    quantity_per_level: String,
    refresh_secs: u64,
    drift_threshold_pct: f64,
) -> Result<()> {
    if levels == 0 {
        anyhow::bail!("--levels must be >= 1");
    }
    if step_pct <= 0.0 {
        anyhow::bail!("--step-pct must be > 0");
    }
    let qty = Decimal::from_str(&quantity_per_level).context("Invalid --quantity-per-level")?;
    if qty <= Decimal::ZERO {
        anyhow::bail!("--quantity-per-level must be > 0");
    }

    info!("Starting Infinite Grid");
    info!("Party: {}", config.party_id);
    info!(
        "market={} step_pct={} levels={} qty={} refresh={}s drift_threshold={}%",
        market, step_pct, levels, qty, refresh_secs, drift_threshold_pct
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
                    liquidity_manager.register_alias(parts[0], &m.base_instrument).await;
                    liquidity_manager.register_alias(parts[1], &m.quote_instrument).await;
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
            step_pct,
            levels,
            qty,
            refresh_secs,
            drift_threshold_pct,
            loop_sd,
        )
        .await
        {
            error!("Infinite grid loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("infinite-grid-state.json")),
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
    step_pct: f64,
    levels: u32,
    quantity: Decimal,
    refresh_secs: u64,
    drift_threshold_pct: f64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let mut last_pivot: Option<f64> = None;

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Infinite grid loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let price = match client.get_price(&market).await {
            Ok(p) => p,
            Err(e) => {
                warn!("get_price failed: {:#}", e);
                sleep_or_break(refresh_secs, &shutdown).await;
                continue;
            }
        };
        let mid = mid_value(&price);
        if mid <= 0.0 {
            sleep_or_break(refresh_secs, &shutdown).await;
            continue;
        }

        // Skip rebuilding if drift since last anchor is below threshold
        if let Some(prev) = last_pivot {
            let drift_pct = ((mid - prev) / prev).abs() * 100.0;
            if drift_pct < drift_threshold_pct {
                info!(
                    "drift {:+.4}% below threshold {:.4}% — keeping current grid",
                    drift_pct, drift_threshold_pct
                );
                sleep_or_break(refresh_secs, &shutdown).await;
                continue;
            }
        }

        // Cancel existing own orders on this market
        if let Ok(orders) = client.get_active_orders(&market).await {
            for o in orders {
                if let Err(e) = client.cancel_order(o.order_id).await {
                    warn!("cancel order_id={}: {:#}", o.order_id, e);
                }
            }
        }

        info!("rebuilding grid: mid={:.6}", mid);
        let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);

        for i in 1..=levels {
            let offset = Decimal::from_str(&format!("{}", step_pct * i as f64 / 100.0))
                .unwrap_or(Decimal::ZERO);
            let bid_price = (mid_dec * (Decimal::ONE - offset)).round_dp(8);
            let offer_price = (mid_dec * (Decimal::ONE + offset)).round_dp(8);

            place(&mut client, &tracker, &market, OrderType::Bid, "BID", &bid_price, &quantity, i).await;
            place(&mut client, &tracker, &market, OrderType::Offer, "OFFER", &offer_price, &quantity, i).await;
        }

        last_pivot = Some(mid);
        sleep_or_break(refresh_secs, &shutdown).await;
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
            Some(format!("inf-{}-{}-{}", label, level, chrono::Utc::now().timestamp_millis())),
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
            if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
            o.market_id,
            o.price,
            o.quantity
        );
    }
    Ok(())
}
