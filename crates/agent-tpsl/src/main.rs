//! Automated TP/SL Agent — Take Profit and Stop Loss
//!
//! Monitors price and automatically places orders when TP or SL levels are hit.
//! - Long position: TP sells when price >= tp_price, SL sells when price <= sl_price
//! - Short position: TP buys when price <= tp_price, SL buys when price >= sl_price
//! - Optional trailing stop: SL follows price upward (long) or downward (short)
//! Settlement is handled automatically by agent-core.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn, error};

use orderbook_agent_logic::client::OrderbookClient;
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

#[derive(Clone, ValueEnum)]
enum PositionSide {
    Long,
    Short,
}

#[derive(Parser)]
#[command(name = "agent-tpsl")]
#[command(about = "Automated TP/SL agent — take profit and stop loss")]
struct Cli {
    /// Path to agent configuration file
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Dry run: do not execute transactions
    #[arg(long, global = true)]
    dry_run: bool,

    /// Force: execute even if verification fails
    #[arg(long, global = true)]
    force: bool,

    /// Prompt for confirmation before signing
    #[arg(long, global = true)]
    confirm: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the TP/SL monitor (long-running)
    Run {
        /// Market ID (e.g. "CBTC-CC")
        #[arg(long)]
        market: String,

        /// Position side
        #[arg(long)]
        side: PositionSide,

        /// Quantity to sell (TP/SL)
        #[arg(long)]
        quantity: String,

        /// Take profit price — exit with profit at this level
        #[arg(long)]
        tp: Option<String>,

        /// Stop loss price — exit with loss at this level
        #[arg(long)]
        sl: Option<String>,

        /// Trailing stop distance in percent (e.g. 2.0 = 2% below peak for long)
        /// When set, SL moves with price. --sl becomes the initial SL floor.
        #[arg(long)]
        trailing_pct: Option<f64>,

        /// Price poll interval in seconds
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        /// Skip restoring state from previous session
        #[arg(long)]
        no_restore: bool,
    },
    /// Show balances and active orders
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
        &["agent_tpsl", "orderbook_agent_logic", "tx_verifier"],
        "agent-tpsl",
    );

    let base_config = orderbook_agent_logic::config::BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run { market, side, quantity, tp, sl, trailing_pct, poll_secs, no_restore } => {
            run_tpsl(base_config, cli.verbose, cli.dry_run, cli.force, cli.confirm,
                     no_restore, market, side, quantity, tp, sl, trailing_pct, poll_secs).await
        }
        Commands::Status => run_status(base_config).await,
    }
}

// ============================================================================
// Balance provider
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
// TPSL runner
// ============================================================================

#[allow(clippy::too_many_arguments)]
async fn run_tpsl(
    config: orderbook_agent_logic::config::BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_id: String,
    side: PositionSide,
    quantity: String,
    tp_str: Option<String>,
    sl_str: Option<String>,
    trailing_pct: Option<f64>,
    poll_secs: u64,
) -> Result<()> {
    if tp_str.is_none() && sl_str.is_none() {
        anyhow::bail!("At least one of --tp or --sl must be specified");
    }

    let qty = Decimal::from_str(&quantity).context("Invalid quantity")?;
    let tp_price = tp_str.as_ref().map(|s| Decimal::from_str(s)).transpose().context("Invalid --tp")?;
    let sl_price = sl_str.as_ref().map(|s| Decimal::from_str(s)).transpose().context("Invalid --sl")?;

    let side_label = match side { PositionSide::Long => "LONG", PositionSide::Short => "SHORT" };

    info!("Starting TP/SL Agent");
    info!("Party ID: {}", config.party_id);
    info!("Market: {}, Side: {}, Quantity: {}", market_id, side_label, qty);
    if let Some(tp) = tp_price { info!("Take Profit: {}", tp); }
    if let Some(sl) = sl_price { info!("Stop Loss: {}", sl); }
    if let Some(pct) = trailing_pct { info!("Trailing stop: {}%", pct); }
    info!("Poll interval: {}s", poll_secs);

    // Create liquidity manager
    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc, config.liquidity_margin,
        config.flow_ema_window_hours, config.depletion_max_hours, config.depletion_min_hours,
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

    // Settlement backend
    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
    );

    // Balance provider
    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url, &config.party_id, &config.role,
        &config.private_key_bytes, config.token_ttl_secs,
        Some(config.node_name.as_str()), &config.ledger_service_public_key,
        Some(config.connection_timeout_secs), Some(config.request_timeout_secs),
    ).await.context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    // Spawn the TP/SL monitoring loop
    let tpsl_shutdown = Arc::new(AtomicBool::new(false));
    let tpsl_config = config.clone();
    let tpsl_shutdown_clone = tpsl_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = tpsl_loop(
            tpsl_config, market_id, side, qty, tp_price, sl_price,
            trailing_pct, poll_secs, tpsl_shutdown_clone,
        ).await {
            error!("TP/SL loop failed: {:#}", e);
        }
    });

    // run_agent handles settlement, heartbeat, graceful shutdown
    run_agent(
        config, backend, balance_provider,
        AgentOptions {
            settlement_only: true,
            orders_only: false,
            actionable_count: None,
            shutdown_notify: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(tpsl_shutdown),
            state_file: Some(PathBuf::from("tpsl-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    ).await
}

// ============================================================================
// TP/SL monitoring loop
// ============================================================================

#[allow(clippy::too_many_arguments)]
async fn tpsl_loop(
    config: orderbook_agent_logic::config::BaseConfig,
    market_id: String,
    side: PositionSide,
    quantity: Decimal,
    tp_price: Option<Decimal>,
    sl_price: Option<Decimal>,
    trailing_pct: Option<f64>,
    poll_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    // Trailing stop state
    let mut current_sl = sl_price;
    let mut peak_price: Option<Decimal> = None; // highest (long) or lowest (short) seen

    // Wait for run_agent to initialize
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("TP/SL monitor started — polling every {}s", poll_secs);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("TP/SL monitor: shutdown");
            break;
        }

        // Fetch price
        let price_response = match client.get_price(&market_id).await {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Price fetch failed: {:#}", e);
                tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                continue;
            }
        };

        // Compute current price from bid/ask or last
        let current_price = match (price_response.bid, price_response.ask) {
            (Some(bid), Some(ask)) if bid > 0.0 && ask > 0.0 => {
                Decimal::from_str(&format!("{}", (bid + ask) / 2.0)).unwrap_or(Decimal::ZERO)
            }
            _ if price_response.last > 0.0 => {
                Decimal::from_str(&format!("{}", price_response.last)).unwrap_or(Decimal::ZERO)
            }
            _ => Decimal::ZERO,
        };

        if current_price <= Decimal::ZERO {
            warn!("No valid price for {}", market_id);
            tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
            continue;
        }

        // Update trailing stop
        if let Some(trail_pct) = trailing_pct {
            let trail_factor = Decimal::from_str(&format!("{}", trail_pct / 100.0))
                .unwrap_or(Decimal::ZERO);

            match side {
                PositionSide::Long => {
                    // Track highest price, SL trails below peak
                    let peak = peak_price.get_or_insert(current_price);
                    if current_price > *peak {
                        *peak = current_price;
                    }
                    let trailing_sl = *peak * (Decimal::ONE - trail_factor);
                    // Only move SL up, never down
                    current_sl = Some(match current_sl {
                        Some(existing) => existing.max(trailing_sl),
                        None => trailing_sl,
                    });
                }
                PositionSide::Short => {
                    // Track lowest price, SL trails above trough
                    let trough = peak_price.get_or_insert(current_price);
                    if current_price < *trough {
                        *trough = current_price;
                    }
                    let trailing_sl = *trough * (Decimal::ONE + trail_factor);
                    // Only move SL down, never up
                    current_sl = Some(match current_sl {
                        Some(existing) => existing.min(trailing_sl),
                        None => trailing_sl,
                    });
                }
            }
        }

        // Check triggers
        let mut trigger: Option<(&str, Decimal)> = None;

        match side {
            PositionSide::Long => {
                // TP: sell when price >= tp
                if let Some(tp) = tp_price {
                    if current_price >= tp {
                        trigger = Some(("TAKE PROFIT", current_price));
                    }
                }
                // SL: sell when price <= sl
                if trigger.is_none() {
                    if let Some(sl) = current_sl {
                        if current_price <= sl {
                            trigger = Some(("STOP LOSS", current_price));
                        }
                    }
                }
            }
            PositionSide::Short => {
                // TP: buy when price <= tp
                if let Some(tp) = tp_price {
                    if current_price <= tp {
                        trigger = Some(("TAKE PROFIT", current_price));
                    }
                }
                // SL: buy when price >= sl
                if trigger.is_none() {
                    if let Some(sl) = current_sl {
                        if current_price >= sl {
                            trigger = Some(("STOP LOSS", current_price));
                        }
                    }
                }
            }
        }

        if let Some((reason, price)) = trigger {
            // Determine exit order type: long exits with sell, short exits with buy
            let exit_type = match side {
                PositionSide::Long => OrderType::Offer,
                PositionSide::Short => OrderType::Bid,
            };
            let type_str = match exit_type {
                OrderType::Offer => "OFFER",
                OrderType::Bid => "BID",
                _ => "OFFER",
            };

            info!(
                "🔔 {} triggered at {} (current={}) — placing {} {} @ {}",
                reason, price, current_price, type_str, quantity, price
            );

            let (signature, signed_data, nonce) = tracker.sign_order(
                &market_id, type_str, &price.to_string(), &quantity.to_string(),
            );

            match client.submit_order(
                &market_id, exit_type, price.to_string(), quantity.to_string(),
                Some(format!("tpsl-{}-{}", reason.to_lowercase().replace(' ', "-"), chrono::Utc::now().timestamp_millis())),
                Some(signature), signed_data, nonce,
            ).await {
                Ok(resp) => {
                    let order_id = resp.order.as_ref().map(|o| o.order_id).unwrap_or(0);
                    info!("✅ {} order placed: id={}", reason, order_id);
                }
                Err(e) => {
                    error!("Failed to place {} order: {:#}", reason, e);
                }
            }

            // TP/SL is one-shot — we're done
            info!("TP/SL executed. Waiting for settlement to complete before shutdown.");
            // Give settlement some time, then signal shutdown
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            shutdown.store(true, Ordering::SeqCst);
            break;
        }

        // Log status periodically
        let sl_display = current_sl.map(|s| s.to_string()).unwrap_or_else(|| "-".to_string());
        let tp_display = tp_price.map(|t| t.to_string()).unwrap_or_else(|| "-".to_string());
        tracing::debug!(
            "price={} tp={} sl={} peak={}",
            current_price, tp_display, sl_display,
            peak_price.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string())
        );

        tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
    }

    Ok(())
}

// ============================================================================
// Status command
// ============================================================================

async fn run_status(config: orderbook_agent_logic::config::BaseConfig) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url, &config.party_id, &config.role,
        &config.private_key_bytes, config.token_ttl_secs,
        Some(config.node_name.as_str()), &config.ledger_service_public_key,
        Some(config.connection_timeout_secs), Some(config.request_timeout_secs),
    ).await?;

    println!("Party: {}", config.party_id);
    println!();

    let balances = ledger_client.get_balances().await?;
    println!("Balances:");
    for b in &balances {
        println!("  {} — total: {}, locked: {}, unlocked: {}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount);
    }
    println!();

    let orders = client.get_all_active_orders().await?;
    if orders.is_empty() {
        println!("No active orders.");
    } else {
        println!("Active orders ({}):", orders.len());
        for o in &orders {
            println!("  #{} {} {} @ {} qty={}",
                o.order_id,
                if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
                o.market_id, o.price, o.quantity);
        }
    }

    Ok(())
}
