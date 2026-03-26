//! Spot DCA Agent — accumulate assets gradually at lower average cost
//!
//! Places a limit buy (or sell) order at regular intervals on a spot market.
//! Settlement is handled automatically by the agent-core settlement executor.
//! All ledger operations are proxied through DAppProviderService (CIP-0103).

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn, error};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::order_tracker::OrderTracker;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;

mod amulet_cache;
mod acs_worker;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "agent-spot-dca")]
#[command(about = "Spot DCA agent — accumulate assets at regular intervals")]
struct Cli {
    /// Path to agent configuration file
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Dry run: prepare and verify transactions but do not sign or execute
    #[arg(long, global = true)]
    dry_run: bool,

    /// Force: sign and execute even if verification fails
    #[arg(long, global = true)]
    force: bool,

    /// Prompt for confirmation before signing each transaction
    #[arg(long, global = true)]
    confirm: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the DCA agent (long-running, places orders at intervals + settles)
    Run {
        /// Skip restoring state from previous session
        #[arg(long)]
        no_restore: bool,

        /// Market ID to trade on (e.g. "CBTC-CC")
        #[arg(long)]
        market: String,

        /// Amount per order (in base currency)
        #[arg(long)]
        amount: String,

        /// Interval between orders in seconds
        #[arg(long, default_value = "3600")]
        interval: u64,

        /// Side: "buy" or "sell"
        #[arg(long, default_value = "buy")]
        side: String,

        /// Price offset from mid price in percent (e.g. -0.5 means 0.5% below mid for buys)
        #[arg(long, default_value = "0.0")]
        price_offset_pct: f64,

        /// Maximum total amount to accumulate (stop after reaching this)
        #[arg(long)]
        max_total: Option<String>,
    },
    /// Show current DCA status and balances
    Status,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_spot_dca", "orderbook_agent_logic", "tx_verifier"],
        "agent-spot-dca",
    );

    let base_config = orderbook_agent_logic::config::BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            no_restore,
            market,
            amount,
            interval,
            side,
            price_offset_pct,
            max_total,
        } => {
            run_dca(
                base_config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                amount,
                interval,
                side,
                price_offset_pct,
                max_total,
            )
            .await
        }
        Commands::Status => run_status(base_config).await,
    }
}

// ============================================================================
// Balance provider (same pattern as agent-orderbook)
// ============================================================================

struct CloudBalanceProvider {
    client: TokioMutex<DAppProviderClient>,
}

#[async_trait]
impl BalanceProvider for CloudBalanceProvider {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
        self.client.lock().await.get_balances().await
    }
}

// ============================================================================
// DCA runner
// ============================================================================

async fn run_dca(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_id: String,
    amount: String,
    interval_secs: u64,
    side: String,
    price_offset_pct: f64,
    max_total: Option<String>,
) -> Result<()> {
    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        _ => anyhow::bail!("Invalid side '{}', must be 'buy' or 'sell'", side),
    };

    let order_amount = Decimal::from_str(&amount)
        .with_context(|| format!("Invalid amount: {}", amount))?;
    let max_total_amount = max_total
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .with_context(|| "Invalid max_total")?;

    info!("Starting Spot DCA Agent");
    info!("Party ID: {}", config.party_id);
    info!("Market: {}", market_id);
    info!("Side: {}", side);
    info!("Amount per order: {}", order_amount);
    info!("Interval: {}s", interval_secs);
    info!("Price offset: {}%", price_offset_pct);
    if let Some(ref max) = max_total_amount {
        info!("Max total: {}", max);
    }

    // Create liquidity manager
    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );

    // Register token aliases from markets
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

    // Create settlement backend
    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
    );

    // Create balance provider
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

    // Spawn the DCA order placement loop as a background task.
    // The main thread runs run_agent() which handles settlement, heartbeat, shutdown.
    let dca_shutdown = Arc::new(AtomicBool::new(false));
    let dca_config = config.clone();
    let dca_shutdown_clone = dca_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = dca_loop(
            dca_config,
            market_id,
            order_type,
            order_amount,
            interval_secs,
            price_offset_pct,
            max_total_amount,
            dca_shutdown_clone,
        )
        .await
        {
            error!("DCA loop failed: {:#}", e);
        }
    });

    // run_agent handles: settlement stream, settlement polling, heartbeat, shutdown, state save.
    // We pass settlement_only=false (we do place orders, just from the spawned task)
    // and orders_only=false (we want settlement).
    // No markets configured in config → OrderManager stays idle.
    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only: true, // don't use OrderManager for grid orders
            orders_only: false,
            actionable_count: None,
            shutdown_notify: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(dca_shutdown),
            state_file: Some(PathBuf::from("dca-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

// ============================================================================
// DCA order placement loop
// ============================================================================

async fn dca_loop(
    config: BaseConfig,
    market_id: String,
    order_type: OrderType,
    order_amount: Decimal,
    interval_secs: u64,
    price_offset_pct: f64,
    max_total: Option<Decimal>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let mut total_placed = Decimal::ZERO;
    let mut order_count: u64 = 0;

    // Wait a few seconds for run_agent to initialize settlement stream
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    info!("DCA loop started — placing orders every {}s", interval_secs);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("DCA loop: shutdown signal received");
            break;
        }

        // Check max total
        if let Some(max) = max_total {
            if total_placed >= max {
                info!("DCA complete: total {} reached max {}", total_placed, max);
                break;
            }
        }

        // Get current price
        let price_response = match client.get_price(&market_id).await {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Failed to get price for {}: {:#}", market_id, e);
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                continue;
            }
        };

        // Compute mid price from bid/ask, or fall back to last
        let mid_price = match (price_response.bid, price_response.ask) {
            (Some(bid), Some(ask)) if bid > 0.0 && ask > 0.0 => {
                Decimal::from_str(&format!("{}", (bid + ask) / 2.0))
                    .unwrap_or(Decimal::ZERO)
            }
            _ => {
                let last = price_response.last;
                if last > 0.0 {
                    Decimal::from_str(&format!("{}", last)).unwrap_or(Decimal::ZERO)
                } else {
                    Decimal::ZERO
                }
            }
        };
        if mid_price <= Decimal::ZERO {
            warn!("No valid price for {}, skipping", market_id);
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            continue;
        }

        // Apply price offset
        let offset_multiplier = Decimal::ONE + Decimal::from_str(
            &format!("{}", price_offset_pct / 100.0)
        ).unwrap_or(Decimal::ZERO);
        let order_price = mid_price * offset_multiplier;

        // Cap amount if it would exceed max_total
        let this_amount = if let Some(max) = max_total {
            let remaining = max - total_placed;
            if remaining < order_amount {
                remaining
            } else {
                order_amount
            }
        } else {
            order_amount
        };

        if this_amount <= Decimal::ZERO {
            info!("DCA complete: no remaining amount");
            break;
        }

        // Sign and place order
        let type_str = match order_type {
            OrderType::Bid => "BID",
            OrderType::Offer => "OFFER",
            _ => "BID",
        };
        let (signature, signed_data, nonce) = tracker.sign_order(
            &market_id,
            type_str,
            &order_price.to_string(),
            &this_amount.to_string(),
        );

        order_count += 1;
        info!(
            "DCA #{}: {} {} {} @ {} (mid={}, offset={}%)",
            order_count, type_str, this_amount, market_id, order_price, mid_price, price_offset_pct
        );

        match client
            .submit_order(
                &market_id,
                order_type,
                order_price.to_string(),
                this_amount.to_string(),
                Some(format!("dca-{}-{}", order_count, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => {
                total_placed += this_amount;
                info!(
                    "Order placed: id={}, total accumulated={}",
                    resp.order.as_ref().map(|o| o.order_id).unwrap_or(0), total_placed
                );
            }
            Err(e) => {
                warn!("Failed to place DCA order #{}: {:#}", order_count, e);
            }
        }

        // Wait for next interval
        for _ in 0..interval_secs {
            if shutdown.load(Ordering::Relaxed) {
                info!("DCA loop: shutdown during wait");
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    Ok(())
}

// ============================================================================
// Status command
// ============================================================================

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

    // Show balances
    let mut lc = ledger_client;
    let balances = lc.get_balances().await?;
    println!("Balances:");
    for b in &balances {
        println!("  {} — total: {}, locked: {}, unlocked: {}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount);
    }
    println!();

    // Show active orders
    let orders = client.get_all_active_orders().await?;
    if orders.is_empty() {
        println!("No active orders.");
    } else {
        println!("Active orders ({}):", orders.len());
        for o in &orders {
            println!("  #{} {} {} @ {} qty={}",
                o.order_id,
                if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
                o.market_id,
                o.price,
                o.quantity);
        }
    }

    Ok(())
}
