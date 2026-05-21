//! Watchlist Agent — read-only market monitor
//!
//! Subscribes to external price feeds (`StreamPrices`) and internal Silvana
//! orderbook depth (`SubscribeOrderbook`) for a configurable set of markets and
//! prints every update to the log. No orders, no settlement, no ledger writes.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{Market, OrderbookDepth, OrderbookLevel, orderbook_update::UpdateType};
use orderbook_proto::pricing::PriceUpdate;

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "agent-watchlist")]
#[command(about = "Watchlist — read-only market monitor (prices + orderbook depth)")]
struct Cli {
    /// Path to agent configuration file
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Stream live updates for the given markets until shutdown
    Run {
        /// Comma-separated list of market IDs to watch (e.g. "CC-USDC,BTC-USD")
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        /// Number of price levels for Silvana orderbook depth subscription
        #[arg(long, default_value = "10")]
        depth: u32,

        /// Include external orderbook updates in the price stream
        #[arg(long)]
        include_orderbook: bool,

        /// Include external trade ticks in the price stream
        #[arg(long)]
        include_trades: bool,

        /// Skip the internal Silvana orderbook depth subscription
        #[arg(long)]
        no_depth: bool,

        /// Skip the external price stream subscription
        #[arg(long)]
        no_prices: bool,
    },
    /// Print a one-off snapshot (markets, current prices, current depth) and exit
    Snapshot {
        /// Comma-separated list of market IDs to snapshot
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        /// Number of price levels to include in the depth snapshot
        #[arg(long, default_value = "10")]
        depth: u32,
    },
    /// List all markets available on the orderbook
    Markets,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_watchlist", "orderbook_agent_logic"],
        "agent-watchlist",
    );

    let base_config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            depth,
            include_orderbook,
            include_trades,
            no_depth,
            no_prices,
        } => {
            run_watchlist(
                base_config,
                markets,
                depth,
                include_orderbook,
                include_trades,
                no_depth,
                no_prices,
            )
            .await
        }
        Commands::Snapshot { markets, depth } => snapshot(base_config, markets, depth).await,
        Commands::Markets => list_markets(base_config).await,
    }
}

// ============================================================================
// Run loop
// ============================================================================

async fn run_watchlist(
    config: BaseConfig,
    markets: Vec<String>,
    depth: u32,
    include_orderbook: bool,
    include_trades: bool,
    no_depth: bool,
    no_prices: bool,
) -> Result<()> {
    if markets.is_empty() {
        anyhow::bail!("--markets is required (e.g. --markets CC-USDC,BTC-USD)");
    }
    if no_depth && no_prices {
        anyhow::bail!("--no-depth and --no-prices together leave nothing to watch");
    }

    info!("Starting Watchlist agent");
    info!("Party: {}", config.party_id);
    info!("Markets: {}", markets.join(", "));
    info!(
        "Depth subscription: {}, Price stream: {}, External orderbook: {}, External trades: {}",
        !no_depth, !no_prices, include_orderbook, include_trades
    );

    // Validate that the markets exist on the orderbook before subscribing.
    {
        let mut client = OrderbookClient::new(&config).await?;
        let available: Vec<Market> = client.get_markets().await?;
        for m in &markets {
            if !available.iter().any(|av| &av.market_id == m) {
                warn!(
                    "Market '{}' not present in GetMarkets response — subscription may fail",
                    m
                );
            }
        }
        let initial = client.get_market_data(markets.clone()).await.unwrap_or_default();
        for md in initial {
            info!(
                "[snapshot] market={} best_bid={:?} best_offer={:?} last={:?} vol_24h={} trades_24h={}",
                md.market_id, md.best_bid, md.best_offer, md.last_price, md.volume_24h, md.trades_24h
            );
        }
    }

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Shutdown signal received");
            shutdown_signal.notify_waiters();
        }
    });

    let mut handles = Vec::new();

    if !no_prices {
        let cfg = config.clone();
        let ms = markets.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            run_price_stream(cfg, ms, include_orderbook, include_trades, sd).await
        }));
    }

    if !no_depth {
        for market_id in markets.iter().cloned() {
            let cfg = config.clone();
            let sd = shutdown.clone();
            handles.push(tokio::spawn(async move {
                run_depth_stream(cfg, market_id, depth, sd).await
            }));
        }
    }

    for h in handles {
        if let Err(e) = h.await {
            error!("subscription task panicked: {e}");
        }
    }

    info!("Watchlist exited");
    Ok(())
}

async fn run_price_stream(
    config: BaseConfig,
    markets: Vec<String>,
    include_orderbook: bool,
    include_trades: bool,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .stream_prices(markets.clone(), include_orderbook, include_trades)
        .await
        .context("stream_prices failed")?;

    info!("[prices] stream opened for {}", markets.join(","));

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                info!("[prices] shutdown notified — closing stream");
                return Ok(());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(update)) => log_price_update(&update),
                    Some(Err(status)) => {
                        error!("[prices] stream error: {} — exiting", status);
                        return Ok(());
                    }
                    None => {
                        warn!("[prices] stream ended by server");
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn run_depth_stream(
    config: BaseConfig,
    market_id: String,
    depth: u32,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_orderbook_depth(&market_id, Some(depth))
        .await
        .with_context(|| format!("subscribe_orderbook_depth({}) failed", market_id))?;

    info!("[depth:{}] stream opened (depth={})", market_id, depth);

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                info!("[depth:{}] shutdown notified — closing stream", market_id);
                return Ok(());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(update)) => {
                        let kind = match UpdateType::try_from(update.update_type).unwrap_or(UpdateType::Unspecified) {
                            UpdateType::Snapshot => "SNAPSHOT",
                            UpdateType::Delta => "DELTA",
                            UpdateType::Unspecified => "?",
                        };
                        let ts = format_proto_ts(update.timestamp.as_ref());
                        info!(
                            "[depth:{}] {} seq={} ts={} bids={} offers={} top_bid={:?} top_offer={:?}",
                            update.market_id,
                            kind,
                            update.sequence_number,
                            ts,
                            update.bid_updates.len(),
                            update.offer_updates.len(),
                            update.bid_updates.first().map(level_summary),
                            update.offer_updates.first().map(level_summary),
                        );
                    }
                    Some(Err(status)) => {
                        error!("[depth:{}] stream error: {} — exiting", market_id, status);
                        return Ok(());
                    }
                    None => {
                        warn!("[depth:{}] stream ended by server", market_id);
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn log_price_update(u: &PriceUpdate) {
    let ts = format_proto_ts(u.timestamp.as_ref());
    let trade_part = u
        .trade
        .as_ref()
        .map(|t| {
            format!(
                " trade(price={} qty={} buyer_maker={})",
                t.price, t.quantity, t.is_buyer_maker
            )
        })
        .unwrap_or_default();
    let ob_part = u
        .orderbook
        .as_ref()
        .map(|ob| format!(" ob(bids={} asks={})", ob.bids.len(), ob.asks.len()))
        .unwrap_or_default();
    info!(
        "[prices] {} src={} ts={} last={} bid={:?} ask={:?}{}{}",
        u.market_id, u.source, ts, u.price, u.bid, u.ask, trade_part, ob_part
    );
}

// ============================================================================
// Snapshot
// ============================================================================

async fn snapshot(config: BaseConfig, markets: Vec<String>, depth: u32) -> Result<()> {
    if markets.is_empty() {
        anyhow::bail!("--markets is required");
    }

    let mut client = OrderbookClient::new(&config).await?;
    let market_data = client.get_market_data(markets.clone()).await?;
    println!("=== Market data ===");
    if market_data.is_empty() {
        println!("  (no data returned)");
    }
    for md in &market_data {
        println!(
            "  {} pair={} best_bid={:?} best_offer={:?} last={:?} vol_24h={} trades_24h={} open_orders={}",
            md.market_id,
            md.pair_symbol,
            md.best_bid,
            md.best_offer,
            md.last_price,
            md.volume_24h,
            md.trades_24h,
            md.open_orders,
        );
    }

    println!("\n=== External prices ===");
    for m in &markets {
        match client.get_price(m).await {
            Ok(p) => println!(
                "  {} last={} bid={:?} ask={:?} src={} vol24h={:?} chg24h%={:?}",
                p.market_id, p.last, p.bid, p.ask, p.source, p.volume_24h, p.change_24h_percent
            ),
            Err(e) => println!("  {} ERROR: {}", m, e),
        }
    }

    println!("\n=== Orderbook depth (top {}) ===", depth);
    for m in &markets {
        match client.get_orderbook_depth(m, Some(depth)).await {
            Ok(Some(d)) => print_depth(&d),
            Ok(None) => println!("  {}: (no depth)", m),
            Err(e) => println!("  {} ERROR: {}", m, e),
        }
    }

    Ok(())
}

fn print_depth(d: &OrderbookDepth) {
    println!(
        "  {} ({})  snapshot={}",
        d.market_id,
        d.pair_symbol,
        format_proto_ts(d.snapshot_time.as_ref())
    );
    println!("    bids: {}", levels_inline(&d.bids));
    println!("    offers: {}", levels_inline(&d.offers));
}

fn levels_inline(levels: &[OrderbookLevel]) -> String {
    if levels.is_empty() {
        return "(empty)".to_string();
    }
    levels
        .iter()
        .map(|l| format!("{}@{}(x{})", l.total_quantity, l.price, l.order_count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn level_summary(l: &OrderbookLevel) -> String {
    format!("{}@{}(x{})", l.total_quantity, l.price, l.order_count)
}

// ============================================================================
// Markets listing
// ============================================================================

async fn list_markets(config: BaseConfig) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let markets = client.get_markets().await?;
    println!("Found {} markets:", markets.len());
    for m in markets {
        println!(
            "  {:<20}  pair={:<16}  active={}  tick={}  min={}  base={} quote={}",
            m.market_id,
            m.pair_symbol,
            m.is_active,
            m.tick_size,
            m.min_order_size,
            m.base_instrument,
            m.quote_instrument,
        );
    }
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

fn format_proto_ts(ts: Option<&prost_types::Timestamp>) -> String {
    let Some(ts) = ts else {
        return "-".to_string();
    };
    let nanos: i64 = ts.nanos.into();
    match DateTime::<Utc>::from_timestamp(ts.seconds, nanos.max(0) as u32) {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        None => format!("{}.{}", ts.seconds, ts.nanos),
    }
}

