//! Hedging Agent — push directional exposure back to a target
//!
//! Tracks the unlocked balance of `--exposure-instrument` against a
//! `--target-balance`. When the deviation exceeds `--tolerance`, places one
//! offsetting limit order on `--hedge-market`:
//!
//! - balance > target + tolerance  → OFFER on hedge market (sell the surplus)
//! - balance < target − tolerance  → BID on hedge market (buy back to target)
//!
//! Sized by `--hedge-fraction` of the deviation per cycle (so the agent walks
//! the exposure back rather than slamming the book in one go). Only one open
//! hedge per direction at a time.
//!
//! This is the "counter-position" sibling of `agent-inventory-mgmt`. The
//! difference is intent: inventory-mgmt rebalances trading inventory you
//! actively want to hold; hedging neutralizes unwanted directional risk that
//! built up from other strategies.

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
#[command(name = "agent-hedging")]
#[command(about = "Push directional exposure of an instrument back to a target")]
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
        /// Instrument id whose balance is being neutralized
        #[arg(long)]
        exposure_instrument: String,

        /// Market on which to place the hedge order (BASE of the market must equal exposure_instrument)
        #[arg(long)]
        hedge_market: String,

        /// Desired balance level (in base units of exposure_instrument)
        #[arg(long)]
        target_balance: String,

        /// Symmetrical tolerance around target before any hedge fires
        #[arg(long)]
        tolerance: String,

        /// Fraction of the deviation to neutralize per cycle (0 < x ≤ 1)
        #[arg(long, default_value = "0.5")]
        hedge_fraction: f64,

        /// Poll interval in seconds
        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Optional price offset from mid in percent
        #[arg(long, default_value = "0.0")]
        price_offset_pct: f64,

        #[arg(long)]
        no_restore: bool,
    },
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_hedging", "orderbook_agent_logic", "tx_verifier"],
        "agent-hedging",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            exposure_instrument,
            hedge_market,
            target_balance,
            tolerance,
            hedge_fraction,
            check_interval,
            price_offset_pct,
            no_restore,
        } => {
            run_hedge(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                exposure_instrument, hedge_market, target_balance, tolerance, hedge_fraction,
                check_interval, price_offset_pct,
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
async fn run_hedge(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    exposure_instrument: String,
    hedge_market: String,
    target_balance: String,
    tolerance: String,
    hedge_fraction: f64,
    check_interval: u64,
    price_offset_pct: f64,
) -> Result<()> {
    let target = Decimal::from_str(&target_balance).context("Invalid --target-balance")?;
    let tol = Decimal::from_str(&tolerance).context("Invalid --tolerance")?;
    if tol < Decimal::ZERO {
        anyhow::bail!("--tolerance must be >= 0");
    }
    if !(0.0..=1.0).contains(&hedge_fraction) || hedge_fraction == 0.0 {
        anyhow::bail!("--hedge-fraction must be in (0, 1]");
    }
    let frac = Decimal::from_str(&format!("{}", hedge_fraction)).unwrap_or(Decimal::ONE);

    info!("Starting Hedging");
    info!("Party: {}", config.party_id);
    info!(
        "instrument={} hedge_market={} target={} tolerance=±{} fraction={} interval={}s offset={}%",
        exposure_instrument,
        hedge_market,
        target,
        tol,
        frac,
        check_interval,
        price_offset_pct
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
        if let Err(e) = hedge_loop(
            loop_cfg, exposure_instrument, hedge_market, target, tol, frac,
            check_interval, price_offset_pct, loop_sd,
        )
        .await
        {
            error!("Hedge loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("hedging-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn hedge_loop(
    config: BaseConfig,
    exposure_instrument: String,
    hedge_market: String,
    target: Decimal,
    tolerance: Decimal,
    fraction: Decimal,
    check_interval: u64,
    price_offset_pct: f64,
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

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Hedge loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        let balances = match ledger.get_balances().await {
            Ok(b) => b,
            Err(e) => {
                warn!("get_balances failed: {:#}", e);
                sleep_or_break(check_interval, &shutdown).await;
                continue;
            }
        };
        let bal = balances
            .iter()
            .find(|b| b.instrument_id == exposure_instrument)
            .and_then(|b| Decimal::from_str(&b.unlocked_amount).ok())
            .unwrap_or(Decimal::ZERO);
        let delta = bal - target;
        info!(
            "balance({}) = {}  target = {}  delta = {:+}",
            exposure_instrument, bal, target, delta
        );

        if delta.abs() <= tolerance {
            info!("inside tolerance — no hedge");
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }

        let (side, label) = if delta > Decimal::ZERO {
            (OrderType::Offer, "OFFER") // too much → sell off
        } else {
            (OrderType::Bid, "BID") // too little → buy back
        };
        if has_open(&mut ob, &hedge_market, side).await {
            info!("open {} hedge already exists — skipping", label);
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }

        let qty = (delta.abs() * fraction).max(Decimal::ZERO);
        if qty <= Decimal::ZERO {
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }

        // Price at mid ± offset
        let mid = match ob.get_price(&hedge_market).await {
            Ok(p) => mid_decimal(&p),
            Err(e) => {
                warn!("get_price({}): {:#}", hedge_market, e);
                sleep_or_break(check_interval, &shutdown).await;
                continue;
            }
        };
        if mid <= Decimal::ZERO {
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }
        let offset = Decimal::from_str(&format!("{}", price_offset_pct / 100.0))
            .unwrap_or(Decimal::ZERO);
        let order_price = mid * (Decimal::ONE + offset);

        info!(
            "HEDGE {}: {} {} @ {} (mid={}, fraction={})",
            label, qty, hedge_market, order_price, mid, fraction
        );
        let (signature, signed_data, nonce) = tracker.sign_order(
            &hedge_market,
            label,
            &order_price.to_string(),
            &qty.to_string(),
        );
        match ob
            .submit_order(
                &hedge_market,
                side,
                order_price.to_string(),
                qty.to_string(),
                Some(format!("hedge-{}-{}", label, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => info!(
                "  → order id={}",
                resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
            ),
            Err(e) => warn!("  submit failed: {:#}", e),
        }

        sleep_or_break(check_interval, &shutdown).await;
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
    println!("\nActive orders ({}):", orders.len());
    for o in &orders {
        println!(
            "  #{} {} {} @ {} qty={}",
            o.order_id,
            if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
            o.market_id, o.price, o.quantity
        );
    }
    Ok(())
}
