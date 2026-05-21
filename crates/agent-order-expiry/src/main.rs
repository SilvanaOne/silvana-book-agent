//! Order Expiry Agent — cancel stale orders by TTL
//!
//! Periodically polls the agent's own active orders and cancels any that have
//! been resting on the book longer than `--max-age-secs`. Optional `--market`
//! filter restricts the scan to one market; otherwise all markets are scanned.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::Order;

#[derive(Parser)]
#[command(name = "agent-order-expiry")]
#[command(about = "Cancel resting orders older than a configurable TTL")]
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
    /// Run the expiry loop until shutdown
    Run {
        /// Cancel orders whose age in seconds exceeds this value
        #[arg(long)]
        max_age_secs: u64,

        /// Optional market filter (e.g. "CC-USDC"); if omitted, all markets are scanned
        #[arg(long)]
        market: Option<String>,

        /// How often to scan in seconds
        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Log what would be cancelled without actually cancelling
        #[arg(long)]
        dry_run: bool,
    },
    /// Print active orders for this party with their current age
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

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_order_expiry", "orderbook_agent_logic"],
        "agent-order-expiry",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            max_age_secs,
            market,
            check_interval,
            dry_run,
        } => run_loop(config, max_age_secs, market, check_interval, dry_run).await,
        Commands::Status { market } => status(config, market).await,
    }
}

async fn run_loop(
    config: BaseConfig,
    max_age_secs: u64,
    market: Option<String>,
    check_interval: u64,
    dry_run: bool,
) -> Result<()> {
    if check_interval == 0 {
        anyhow::bail!("--check-interval must be > 0");
    }

    info!("Starting Order Expiry agent");
    info!("Party: {}", config.party_id);
    info!(
        "max_age_secs={} check_interval={}s market={:?} dry_run={}",
        max_age_secs, check_interval, market, dry_run
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
                if let Err(e) = sweep(&mut client, max_age_secs, market.as_deref(), dry_run).await {
                    error!("sweep failed: {:#}", e);
                }
            }
        }
    }
}

async fn sweep(
    client: &mut OrderbookClient,
    max_age_secs: u64,
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

    let now = Utc::now();
    let mut expired = 0usize;
    let mut scanned = 0usize;

    for order in orders {
        scanned += 1;
        let Some(age) = order_age_secs(&order, now) else { continue };
        if age <= max_age_secs {
            continue;
        }
        expired += 1;
        if dry_run {
            info!(
                "[dry-run] would cancel order_id={} market={} side={:?} price={} qty={} age={}s",
                order.order_id,
                order.market_id,
                order.order_type,
                order.price,
                order.quantity,
                age,
            );
            continue;
        }
        match client.cancel_order(order.order_id).await {
            Ok(resp) if resp.success => info!(
                "cancelled order_id={} market={} age={}s",
                order.order_id, order.market_id, age
            ),
            Ok(resp) => warn!(
                "cancel returned !success for order_id={}: {}",
                order.order_id, resp.message
            ),
            Err(e) => error!("cancel order_id={} failed: {:#}", order.order_id, e),
        }
    }

    info!("sweep: scanned={} expired={}", scanned, expired);
    Ok(())
}

async fn status(config: BaseConfig, market: Option<String>) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let orders = match market {
        Some(ref m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };

    let now = Utc::now();
    println!("Active orders for {}: {}", config.party_id, orders.len());
    for o in orders {
        let age = order_age_secs(&o, now)
            .map(|s| format!("{}s", s))
            .unwrap_or_else(|| "?".to_string());
        let created = o
            .created_at
            .as_ref()
            .and_then(|t| {
                let nanos: i64 = t.nanos.into();
                DateTime::<Utc>::from_timestamp(t.seconds, nanos.max(0) as u32)
            })
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  id={:<10} market={:<14} side={:?} price={:<14} qty={:<14} created={} age={}",
            o.order_id, o.market_id, o.order_type, o.price, o.quantity, created, age
        );
    }
    Ok(())
}

fn order_age_secs(order: &Order, now: DateTime<Utc>) -> Option<u64> {
    let ts: &prost_types::Timestamp = order.created_at.as_ref()?;
    let nanos: i64 = ts.nanos.into();
    let created = DateTime::<Utc>::from_timestamp(ts.seconds, nanos.max(0) as u32)?;
    let delta = now.signed_duration_since(created).num_seconds();
    if delta < 0 { Some(0) } else { Some(delta as u64) }
}
