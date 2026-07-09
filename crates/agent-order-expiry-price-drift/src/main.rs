//! Order Expiry Agent — cancel orders on price drift
//!
//! Periodically polls the agent's own active orders and cancels any whose
//! price has drifted more than `--max-drift-pct` away from the current mid
//! (obtained via `PricingService.GetPrice`). Optional `--market` filter
//! restricts the scan to one market; otherwise all markets with active
//! orders for this party are scanned.
//!
//! Uses `OrderbookService.GetOrders` + `CancelOrder` — no two-phase signing
//! needed (JWT only).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::Order;

#[derive(Parser)]
#[command(name = "agent-order-expiry-price-drift")]
#[command(about = "Cancel resting orders whose price has drifted too far from mid")]
struct Cli {
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the price-drift expiry loop until shutdown
    Run {
        /// Cancel orders whose |price - mid| / mid * 100 exceeds this percentage
        #[arg(long)]
        max_drift_pct: f64,

        /// Optional market filter (e.g. "CC-USDC"); if omitted, all markets
        /// with own active orders are scanned
        #[arg(long)]
        market: Option<String>,

        /// How often to scan in seconds
        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Log what would be cancelled without actually cancelling
        #[arg(long)]
        dry_run: bool,
    },
    /// Print active orders for this party with their current drift vs mid
    Status {
        /// Optional market filter
        #[arg(long)]
        market: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_order_expiry_price_drift", "agent_logic"],
        "agent-order-expiry-price-drift",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            max_drift_pct,
            market,
            check_interval,
            dry_run,
        } => run_loop(config, max_drift_pct, market, check_interval, dry_run).await,
        Commands::Status { market } => status(config, market).await,
    }
}

async fn run_loop(
    config: BaseConfig,
    max_drift_pct: f64,
    market: Option<String>,
    check_interval: u64,
    dry_run: bool,
) -> Result<()> {
    if check_interval == 0 {
        anyhow::bail!("--check-interval must be > 0");
    }
    if !(max_drift_pct.is_finite()) || max_drift_pct <= 0.0 {
        anyhow::bail!("--max-drift-pct must be > 0");
    }

    info!("Starting Order Expiry (Price Drift) agent");
    info!("Party: {}", config.party_id);
    info!(
        "max_drift_pct={} check_interval={}s market={:?} dry_run={}",
        max_drift_pct, check_interval, market, dry_run
    );

    let mut client = OrderbookClient::new(&config).await?;

    let mut ticker = interval(Duration::from_secs(check_interval));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown signal received");
                return Ok(());
            }
            _ = ticker.tick() => {
                if let Err(e) = sweep(&mut client, max_drift_pct, market.as_deref(), dry_run).await {
                    error!("sweep failed: {:#}", e);
                }
            }
        }
    }
}

async fn sweep(
    client: &mut OrderbookClient,
    max_drift_pct: f64,
    market: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let orders = match market {
        Some(m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };

    if orders.is_empty() {
        info!("no active orders");
        return Ok(());
    }

    // Group orders by market so we fetch GetPrice at most once per market per cycle.
    let mut by_market: HashMap<String, Vec<Order>> = HashMap::new();
    for o in orders {
        by_market.entry(o.market_id.clone()).or_default().push(o);
    }

    let mut scanned = 0usize;
    let mut drifted = 0usize;
    let mut skipped_no_mid = 0usize;

    for (market_id, orders) in by_market {
        let mid = match fetch_mid(client, &market_id).await {
            Some(m) => m,
            None => {
                warn!("no usable mid for market={}, skipping {} order(s)", market_id, orders.len());
                skipped_no_mid += orders.len();
                continue;
            }
        };

        for order in orders {
            scanned += 1;
            let Some(price) = parse_price(&order.price) else {
                warn!("could not parse price for order_id={} ({:?}), skipping", order.order_id, order.price);
                continue;
            };
            let drift_pct = ((price - mid).abs() / mid) * 100.0;
            if drift_pct <= max_drift_pct {
                continue;
            }
            drifted += 1;
            if dry_run {
                info!(
                    "[dry-run] would cancel order_id={} market={} side={:?} price={} qty={} mid={:.6} drift={:.3}%",
                    order.order_id,
                    order.market_id,
                    order.order_type,
                    order.price,
                    order.quantity,
                    mid,
                    drift_pct,
                );
                continue;
            }
            match client.cancel_order(order.order_id).await {
                Ok(resp) if resp.success => info!(
                    "cancelled order_id={} market={} price={} mid={:.6} drift={:.3}%",
                    order.order_id, order.market_id, order.price, mid, drift_pct
                ),
                Ok(resp) => warn!(
                    "cancel returned !success for order_id={}: {}",
                    order.order_id, resp.message
                ),
                Err(e) => error!("cancel order_id={} failed: {:#}", order.order_id, e),
            }
        }
    }

    info!(
        "sweep: scanned={} drifted={} skipped_no_mid={}",
        scanned, drifted, skipped_no_mid
    );
    Ok(())
}

async fn status(config: BaseConfig, market: Option<String>) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let orders = match market {
        Some(ref m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };

    println!("Active orders for {}: {}", config.party_id, orders.len());

    // Group by market so we fetch price once per market.
    let mut by_market: HashMap<String, Vec<Order>> = HashMap::new();
    for o in orders {
        by_market.entry(o.market_id.clone()).or_default().push(o);
    }

    for (market_id, orders) in by_market {
        let mid = fetch_mid(&mut client, &market_id).await;
        let mid_str = mid
            .map(|m| format!("{:.6}", m))
            .unwrap_or_else(|| "?".to_string());
        println!("Market {} (mid={})", market_id, mid_str);
        for o in orders {
            let price = parse_price(&o.price);
            let drift = match (price, mid) {
                (Some(p), Some(m)) if m > 0.0 => format!("{:.3}%", ((p - m).abs() / m) * 100.0),
                _ => "?".to_string(),
            };
            println!(
                "  id={:<10} side={:?} price={:<14} qty={:<14} drift={}",
                o.order_id, o.order_type, o.price, o.quantity, drift
            );
        }
    }
    Ok(())
}

async fn fetch_mid(client: &mut OrderbookClient, market_id: &str) -> Option<f64> {
    match client.get_price(market_id).await {
        Ok(price) => match (price.bid, price.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Some((b + a) / 2.0),
            _ if price.last > 0.0 => Some(price.last),
            _ => None,
        },
        Err(e) => {
            warn!("get_price({}) failed: {:#}", market_id, e);
            None
        }
    }
}

fn parse_price(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)
}
