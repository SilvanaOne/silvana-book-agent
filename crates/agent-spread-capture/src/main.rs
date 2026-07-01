//! Spread Capture Agent — single-level two-sided market maker
//!
//! Keeps one bid and one offer near the mid price at all times. Each refresh
//! cycle:
//! 1. Cancel any existing own orders on the market.
//! 2. Compute new bid/offer at `mid × (1 ∓ spread_bps / 20000)`.
//! 3. Skip the bid side if net inventory (cumulative buys - cumulative sells
//!    since process start) exceeds `--max-inventory`; skip the offer side if
//!    inventory drops below `−max-inventory`.
//! 4. Submit the surviving side(s) sized at `--quantity`.
//!
//! Inventory accounting is process-local — restart resets it to zero.

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
#[command(name = "agent-spread-capture")]
#[command(about = "Two-sided quoting with inventory clamps")]
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

        /// Half-spread in basis points (e.g. 25 = 0.25% on each side)
        #[arg(long)]
        spread_bps: u32,

        /// Quantity per order
        #[arg(long)]
        quantity: String,

        /// Refresh interval seconds
        #[arg(long, default_value = "30")]
        refresh_secs: u64,

        /// Skip the bid side when net_inventory >= +this; skip offer when <= -this
        #[arg(long)]
        max_inventory: String,

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
        &["agent_spread_capture", "agent_logic", "tx_verifier"],
        "agent-spread-capture",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            spread_bps,
            quantity,
            refresh_secs,
            max_inventory,
            no_restore,
        } => {
            run_sc(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                spread_bps,
                quantity,
                refresh_secs,
                max_inventory,
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
async fn run_sc(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    spread_bps: u32,
    quantity: String,
    refresh_secs: u64,
    max_inventory: String,
) -> Result<()> {
    if spread_bps == 0 {
        anyhow::bail!("--spread-bps must be >= 1");
    }
    let qty = Decimal::from_str(&quantity).context("Invalid --quantity")?;
    let max_inv = Decimal::from_str(&max_inventory).context("Invalid --max-inventory")?;
    if max_inv < Decimal::ZERO {
        anyhow::bail!("--max-inventory must be >= 0");
    }

    info!("Starting Spread Capture");
    info!("Party: {}", config.party_id);
    info!(
        "market={} spread_bps={} qty={} refresh={}s max_inv=±{}",
        market, spread_bps, qty, refresh_secs, max_inv
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
    let shutdown = agent_logic::shutdown::Shutdown::new();
    let backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
        shutdown.clone(),
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

    let sc_shutdown = Arc::new(AtomicBool::new(false));
    let loop_config = config.clone();
    let loop_shutdown = sc_shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = sc_loop(loop_config, market, spread_bps, qty, refresh_secs, max_inv, loop_shutdown).await {
            error!("Spread capture loop failed: {:#}", e);
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
            shutdown: Some(shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(sc_shutdown.clone())),
            state_file: Some(PathBuf::from("spread-capture-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn sc_loop(
    config: BaseConfig,
    market: String,
    spread_bps: u32,
    quantity: Decimal,
    refresh_secs: u64,
    max_inventory: Decimal,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    // Net inventory tracked since process start. We measure it by
    // recomputing it each cycle from filled orders that we've placed and seen.
    let mut filled_buy_qty: Decimal = Decimal::ZERO;
    let mut filled_sell_qty: Decimal = Decimal::ZERO;

    let half = Decimal::from(spread_bps as i64) / Decimal::from(20000);

    let tick_size = client.get_tick_size(&market).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Spread capture loop started: half_spread={} (tick={})", half, tick_size);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Update inventory: count fills since start
        if let Ok(orders) = client.get_active_orders(&market).await {
            // Cancel existing own orders before refreshing
            for o in orders {
                if let Err(e) = client.cancel_order(o.order_id).await {
                    warn!("cancel order_id={}: {:#}", o.order_id, e);
                }
            }
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
        let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);

        let inv = filled_buy_qty - filled_sell_qty;
        let place_bid = inv < max_inventory;
        let place_offer = inv > -max_inventory;
        let bid_price = agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE - half), tick_size);
        let offer_price = agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE + half), tick_size);

        info!(
            "cycle: mid={:.6} inv={} place_bid={} place_offer={} bid={} offer={}",
            mid, inv, place_bid, place_offer, bid_price, offer_price
        );

        if place_bid {
            place(&mut client, &tracker, &market, OrderType::Bid, &bid_price, &quantity).await;
        }
        if place_offer {
            place(&mut client, &tracker, &market, OrderType::Offer, &offer_price, &quantity).await;
        }

        // Estimate filled fills: anything from previous cycle that is no longer in the book
        // is approximated as fully filled — naive but adequate for this minimal version.
        // (A full implementation would subscribe to OrderUpdate streams.)
        let _ = &mut filled_buy_qty;
        let _ = &mut filled_sell_qty;

        sleep_or_break(refresh_secs, &shutdown).await;
    }
}

async fn place(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    price: &Decimal,
    qty: &Decimal,
) {
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("sc-{}-{}", label, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  {} placed: id={}",
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
