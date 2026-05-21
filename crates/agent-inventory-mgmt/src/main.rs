//! Inventory Management Agent
//!
//! Keeps the unlocked balance of a single instrument inside the target band
//! `[target − tolerance, target + tolerance]`. On every poll cycle:
//!
//! - If balance > target + tolerance → place an OFFER (sell) at the live mid
//!   for `chunk_size` to reduce inventory.
//! - If balance < target − tolerance → place a BID (buy) at the live mid for
//!   `chunk_size` to top up inventory.
//! - Otherwise → no action.
//!
//! Unlike `agent-cash-buffer` (which uses `TransferCc` for Canton Coin), this
//! agent rebalances *trading* inventory by placing market-side orders.

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

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::order_tracker::OrderTracker;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-inventory-mgmt")]
#[command(about = "Keep inventory of an instrument inside a target band")]
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
        /// Market on which to place rebalancing orders (e.g. "CC-USDC")
        #[arg(long)]
        market: String,

        /// Instrument id whose unlocked balance is being managed (base of the market)
        #[arg(long)]
        instrument: String,

        #[arg(long)]
        target: String,

        #[arg(long)]
        tolerance: String,

        /// Quantity per rebalancing order
        #[arg(long)]
        chunk_size: String,

        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Optional price offset from mid in percent (negative tightens buys, positive tightens sells)
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
        &["agent_inventory_mgmt", "orderbook_agent_logic", "tx_verifier"],
        "agent-inventory-mgmt",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            instrument,
            target,
            tolerance,
            chunk_size,
            check_interval,
            price_offset_pct,
            no_restore,
        } => {
            run_inv(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                instrument,
                target,
                tolerance,
                chunk_size,
                check_interval,
                price_offset_pct,
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
async fn run_inv(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    instrument: String,
    target: String,
    tolerance: String,
    chunk_size: String,
    check_interval: u64,
    price_offset_pct: f64,
) -> Result<()> {
    let target_dec = Decimal::from_str(&target).context("Invalid --target")?;
    let tol_dec = Decimal::from_str(&tolerance).context("Invalid --tolerance")?;
    let chunk = Decimal::from_str(&chunk_size).context("Invalid --chunk-size")?;
    if tol_dec < Decimal::ZERO {
        anyhow::bail!("--tolerance must be >= 0");
    }
    if chunk <= Decimal::ZERO {
        anyhow::bail!("--chunk-size must be > 0");
    }

    info!("Starting Inventory Management");
    info!("Party: {}", config.party_id);
    info!(
        "market={} instrument={} target={} tolerance=±{} chunk={} interval={}s offset={}%",
        market, instrument, target_dec, tol_dec, chunk, check_interval, price_offset_pct
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
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
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
        if let Err(e) = inv_loop(
            loop_cfg,
            market,
            instrument,
            target_dec,
            tol_dec,
            chunk,
            check_interval,
            price_offset_pct,
            loop_sd,
        )
        .await
        {
            error!("Inventory loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("inventory-mgmt-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn inv_loop(
    config: BaseConfig,
    market: String,
    instrument: String,
    target: Decimal,
    tolerance: Decimal,
    chunk: Decimal,
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

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Inventory loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Fetch unlocked balance for the configured instrument
        let balances = match ledger.get_balances().await {
            Ok(b) => b,
            Err(e) => {
                warn!("get_balances failed: {:#}", e);
                sleep_or_break(check_interval, &shutdown).await;
                continue;
            }
        };
        let unlocked = balances
            .iter()
            .find(|b| b.instrument_id == instrument)
            .and_then(|b| Decimal::from_str(&b.unlocked_amount).ok())
            .unwrap_or(Decimal::ZERO);
        let upper = target + tolerance;
        let lower = target - tolerance;
        info!("balance({}) unlocked={} band=[{}, {}]", instrument, unlocked, lower, upper);

        if unlocked > upper {
            // Too much inventory — place an OFFER (sell)
            if let Err(e) = rebalance(
                &mut ob,
                &tracker,
                &market,
                OrderType::Offer,
                "OFFER",
                chunk,
                price_offset_pct,
            )
            .await
            {
                warn!("rebalance OFFER failed: {:#}", e);
            }
        } else if unlocked < lower {
            // Too little — place a BID (buy)
            if let Err(e) = rebalance(
                &mut ob,
                &tracker,
                &market,
                OrderType::Bid,
                "BID",
                chunk,
                price_offset_pct,
            )
            .await
            {
                warn!("rebalance BID failed: {:#}", e);
            }
        } else {
            info!("inventory in band — no action");
        }

        sleep_or_break(check_interval, &shutdown).await;
    }
}

async fn rebalance(
    ob: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    qty: Decimal,
    price_offset_pct: f64,
) -> Result<()> {
    let price = ob.get_price(market).await?;
    let mid = mid_value(&price);
    if mid <= 0.0 {
        anyhow::bail!("no valid mid price for {}", market);
    }
    let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);
    let order_price = mid_dec
        * (Decimal::ONE
            + Decimal::from_str(&format!("{}", price_offset_pct / 100.0)).unwrap_or(Decimal::ZERO));

    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &order_price.to_string(), &qty.to_string());
    info!(
        "REBAL {}: {} {} @ {} (mid={:.6})",
        label, qty, market, order_price, mid
    );
    let resp = ob
        .submit_order(
            market,
            order_type,
            order_price.to_string(),
            qty.to_string(),
            Some(format!("inv-{}-{}", label, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await?;
    info!(
        "  → order id={}",
        resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
    );
    Ok(())
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
    println!("\nActive orders ({}):", orders.len());
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
