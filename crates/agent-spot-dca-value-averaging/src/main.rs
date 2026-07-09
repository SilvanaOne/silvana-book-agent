//! Spot DCA Agent — Value Averaging (dynamic order size)
//!
//! Sibling of `agent-spot-dca-scheduled`. Instead of placing a *fixed* amount
//! every interval, this variant targets a **linear position value** schedule:
//!
//! - After `k` completed cycles the target accumulated notional is
//!   `value_per_period * k` (quote-currency units).
//! - Each cycle: fetch mid, compute current position notional (running sum),
//!   place an order sized to close the gap between actual and target.
//! - Under-target ⇒ buy more (positive gap). Over-target ⇒ smaller buy or
//!   even sell to shed the excess, depending on `--side`.
//!
//! Effect: buys *more* when price is low (a fixed quote value buys more base
//! quantity) and *less* when price is high — classic value-averaging. Behaviour
//! is bounded by `--max-order-quote` per cycle and an optional `--max-total-quote`.

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
#[command(name = "agent-spot-dca-value-averaging")]
#[command(about = "Spot DCA agent — value averaging (dynamic quantity per cycle)")]
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
    /// Run the value-averaging DCA loop
    Run {
        #[arg(long)]
        no_restore: bool,

        /// Market ID (e.g. "CC-USDC")
        #[arg(long)]
        market: String,

        /// Target notional increment per cycle in quote currency
        #[arg(long)]
        value_per_period: String,

        /// Cycle interval in seconds
        #[arg(long, default_value = "3600")]
        interval: u64,

        /// Buy or sell side (usually "buy" to accumulate)
        #[arg(long, default_value = "buy")]
        side: String,

        /// Price offset from mid in percent (e.g. -0.5 = 0.5% below mid for buys)
        #[arg(long, default_value = "0.0")]
        price_offset_pct: f64,

        /// Cap on order notional per cycle (quote currency). Optional.
        #[arg(long)]
        max_order_quote: Option<String>,

        /// Stop when cumulative target reaches this notional. Optional.
        #[arg(long)]
        max_total_quote: Option<String>,
    },
    /// Show status and balances
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_spot_dca", "agent_logic", "tx_verifier"],
        "agent-spot-dca-value-averaging",
    );

    let base_config = agent_logic::config::BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            no_restore,
            market,
            value_per_period,
            interval,
            side,
            price_offset_pct,
            max_order_quote,
            max_total_quote,
        } => {
            run_va(
                base_config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                value_per_period,
                interval,
                side,
                price_offset_pct,
                max_order_quote,
                max_total_quote,
            )
            .await
        }
        Commands::Status => run_status(base_config).await,
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
async fn run_va(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_id: String,
    value_per_period: String,
    interval_secs: u64,
    side: String,
    price_offset_pct: f64,
    max_order_quote: Option<String>,
    max_total_quote: Option<String>,
) -> Result<()> {
    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        _ => anyhow::bail!("Invalid side '{}', must be 'buy' or 'sell'", side),
    };

    let target_per_period =
        Decimal::from_str(&value_per_period).context("Invalid --value-per-period")?;
    if target_per_period <= Decimal::ZERO {
        anyhow::bail!("--value-per-period must be > 0");
    }
    let max_order = max_order_quote
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .context("Invalid --max-order-quote")?;
    let max_total = max_total_quote
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .context("Invalid --max-total-quote")?;

    info!("Starting Spot DCA — value averaging");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} value_per_period={} interval={}s offset={}%",
        market_id, side, target_per_period, interval_secs, price_offset_pct
    );
    if let Some(m) = &max_order {
        info!("Max order quote: {}", m);
    }
    if let Some(m) = &max_total {
        info!("Max total quote: {}", m);
    }

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
    .await
    .context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    let va_shutdown = Arc::new(AtomicBool::new(false));
    let va_config = config.clone();
    let va_shutdown_clone = va_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = va_loop(
            va_config,
            market_id,
            order_type,
            target_per_period,
            interval_secs,
            price_offset_pct,
            max_order,
            max_total,
            va_shutdown_clone,
        )
        .await
        {
            error!("Value-averaging loop failed: {:#}", e);
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
            accepted_rfq_trades: None,
            rejected_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(va_shutdown.clone())),
            state_file: Some(PathBuf::from("dca-va-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn va_loop(
    config: BaseConfig,
    market_id: String,
    order_type: OrderType,
    target_per_period: Decimal,
    interval_secs: u64,
    price_offset_pct: f64,
    max_order_quote: Option<Decimal>,
    max_total_quote: Option<Decimal>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let mut cycle: u64 = 0;
    let mut placed_notional_actual = Decimal::ZERO;
    let tick_size = client.get_tick_size(&market_id).await;
    info!("Market {} tick_size = {}", market_id, tick_size);

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("VA loop started — interval {}s", interval_secs);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("VA loop: shutdown signal received");
            break;
        }

        cycle += 1;
        let target_cumulative = target_per_period * Decimal::from(cycle);

        if let Some(cap) = max_total_quote {
            if target_cumulative > cap {
                info!(
                    "VA complete: target cumulative {} would exceed max_total {}",
                    target_cumulative, cap
                );
                break;
            }
        }

        // Fetch mid
        let price_response = match client.get_price(&market_id).await {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Failed to get price for {}: {:#}", market_id, e);
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                continue;
            }
        };
        let mid_price = mid_value(&price_response);
        if mid_price <= 0.0 {
            warn!("No valid price for {}, skipping cycle", market_id);
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            continue;
        }

        let offset_multiplier = Decimal::ONE
            + Decimal::from_str(&format!("{}", price_offset_pct / 100.0)).unwrap_or(Decimal::ZERO);
        let mid_dec = Decimal::from_str(&format!("{}", mid_price)).unwrap_or(Decimal::ZERO);
        let order_price = agent_logic::tick::round_to_tick(mid_dec * offset_multiplier, tick_size);
        if order_price <= Decimal::ZERO {
            warn!("Computed order price <= 0, skipping cycle");
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            continue;
        }

        // Gap between target and actual notional placed so far
        let mut gap = target_cumulative - placed_notional_actual;
        if let Some(cap) = max_order_quote {
            if gap > cap {
                gap = cap;
            }
        }
        if gap <= Decimal::ZERO {
            info!(
                "cycle {} target={} actual={} — no order this cycle",
                cycle, target_cumulative, placed_notional_actual
            );
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            continue;
        }

        // Convert quote-gap to base quantity via the placement price
        let this_quantity = gap / order_price;

        let type_str = match order_type {
            OrderType::Bid => "BID",
            OrderType::Offer => "OFFER",
            _ => "BID",
        };
        let (signature, signed_data, nonce) = tracker.sign_order(
            &market_id,
            type_str,
            &order_price.to_string(),
            &this_quantity.to_string(),
        );

        info!(
            "VA #{}: target_cum={} placed={} gap={} → {} {} @ {} qty={}",
            cycle,
            target_cumulative,
            placed_notional_actual,
            gap,
            type_str,
            market_id,
            order_price,
            this_quantity
        );

        match client
            .submit_order(
                &market_id,
                order_type,
                order_price.to_string(),
                this_quantity.to_string(),
                Some(format!(
                    "dca-va-{}-{}",
                    cycle,
                    chrono::Utc::now().timestamp_millis()
                )),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => {
                placed_notional_actual += gap;
                info!(
                    "Order placed: id={}, cumulative placed={}",
                    resp.order.as_ref().map(|o| o.order_id).unwrap_or(0),
                    placed_notional_actual
                );
            }
            Err(e) => {
                warn!("VA #{} submit failed: {:#}", cycle, e);
            }
        }

        for _ in 0..interval_secs {
            if shutdown.load(Ordering::Relaxed) {
                info!("VA loop: shutdown during wait");
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    Ok(())
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

    println!("Party: {}", config.party_id);
    println!();

    let mut lc = ledger_client;
    let balances = lc.get_balances().await?;
    println!("Balances:");
    for b in &balances {
        println!(
            "  {} — total: {}, locked: {}, unlocked: {}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount
        );
    }
    println!();

    let orders = client.get_all_active_orders().await?;
    if orders.is_empty() {
        println!("No active orders.");
    } else {
        println!("Active orders ({}):", orders.len());
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
    }

    Ok(())
}
