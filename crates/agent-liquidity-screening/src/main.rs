//! Liquidity Screening Agent
//!
//! Polls `GetOrderbookDepth` for each configured market and publishes a
//! snapshot of liquidity metrics per tick:
//!
//! - **spread** = best_offer − best_bid, **spread_bps** = spread / mid × 10000
//! - **bid_depth_qty / offer_depth_qty** = sum of quantities across visible levels
//! - **bid_depth_notional / offer_depth_notional** = sum of qty × price
//! - **slippage estimate** for a target market-buy of `--probe-qty`:
//!   walks the offer side level by level until the requested quantity is
//!   covered; reports the average fill price and the bps deviation from mid.
//! - mirror computation for a market-sell against the bid side.
//!
//! Pure read-only screener — no ledger writes, no orders. Pair with the
//! trading agents to make liquidity-aware sizing decisions.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{OrderbookDepth, OrderbookLevel};

#[derive(Parser)]
#[command(name = "agent-liquidity-screening")]
#[command(about = "Depth + slippage analytics publisher")]
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
    Run {
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        #[arg(long, default_value = "30")]
        poll_secs: u64,

        #[arg(long, default_value = "10")]
        depth: u32,

        /// Probe size used to estimate slippage on each side
        #[arg(long)]
        probe_qty: String,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    /// One-off probe for a single market
    Probe {
        #[arg(long)]
        market: String,

        #[arg(long, default_value = "20")]
        depth: u32,

        #[arg(long)]
        probe_qty: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_liquidity_screening", "agent_logic"],
        "agent-liquidity-screening",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            poll_secs,
            depth,
            probe_qty,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let qty = Decimal::from_str(&probe_qty).context("Invalid --probe-qty")?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, poll_secs, depth, qty, sinks).await
        }
        Commands::Probe { market, depth, probe_qty } => {
            let qty = Decimal::from_str(&probe_qty).context("Invalid --probe-qty")?;
            let mut client = OrderbookClient::new(&config).await?;
            let snapshot = sample_market(&mut client, &market, depth, qty).await?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
            Ok(())
        }
    }
}

struct Sinks {
    webhook: Option<String>,
    http: Option<reqwest::Client>,
    log_file: Option<Mutex<tokio::fs::File>>,
    stdout: bool,
}
impl Sinks {
    async fn open(
        webhook: Option<String>,
        log_file: Option<PathBuf>,
        stdout: bool,
    ) -> Result<Arc<Self>> {
        let http = if webhook.is_some() {
            Some(reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?)
        } else {
            None
        };
        let file = match log_file {
            Some(p) => Some(Mutex::new(
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .await
                    .with_context(|| format!("open {:?}", p))?,
            )),
            None => None,
        };
        Ok(Arc::new(Self { webhook, http, log_file: file, stdout }))
    }
    async fn dispatch(&self, payload: Value) {
        let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
        if self.stdout {
            println!("{}", line);
        }
        if let Some(file) = &self.log_file {
            let mut f = file.lock().await;
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
        }
        if let (Some(url), Some(http)) = (&self.webhook, &self.http) {
            match http.post(url).json(&payload).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => warn!("webhook returned {}", r.status()),
                Err(e) => warn!("webhook POST failed: {e}"),
            }
        }
    }
}

async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    poll_secs: u64,
    depth: u32,
    probe_qty: Decimal,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting liquidity-screening: markets={:?} depth={} probe_qty={} poll={}s",
        markets, depth, probe_qty, poll_secs
    );
    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    match sample_market(&mut client, m, depth, probe_qty).await {
                        Ok(snap) => sinks.dispatch(snap).await,
                        Err(e) => error!("sample({}): {:#}", m, e),
                    }
                }
            }
        }
    }
}

async fn sample_market(
    client: &mut OrderbookClient,
    market: &str,
    depth: u32,
    probe_qty: Decimal,
) -> Result<Value> {
    let dep = client
        .get_orderbook_depth(market, Some(depth))
        .await
        .with_context(|| format!("get_orderbook_depth({market})"))?
        .unwrap_or(OrderbookDepth {
            market_id: market.to_string(),
            pair_symbol: String::new(),
            bids: Vec::new(),
            offers: Vec::new(),
            snapshot_time: None,
        });

    let best_bid = dep.bids.first().and_then(level_price);
    let best_offer = dep.offers.first().and_then(level_price);
    let mid = match (best_bid, best_offer) {
        (Some(b), Some(o)) => Some((b + o) / Decimal::from(2)),
        _ => None,
    };

    let (bid_qty_sum, bid_notional_sum) = depth_totals(&dep.bids);
    let (offer_qty_sum, offer_notional_sum) = depth_totals(&dep.offers);

    let spread = match (best_bid, best_offer) {
        (Some(b), Some(o)) => Some(o - b),
        _ => None,
    };
    let spread_bps = match (spread, mid) {
        (Some(s), Some(m)) if m > Decimal::ZERO => Some(s / m * Decimal::from(10000)),
        _ => None,
    };

    // Slippage probe: market BUY against the offer side
    let buy_probe = walk(&dep.offers, probe_qty, mid);
    // Market SELL against the bid side
    let sell_probe = walk(&dep.bids, probe_qty, mid);

    Ok(json!({
        "kind": "liquidity.snapshot",
        "ts": Utc::now().to_rfc3339(),
        "market_id": market,
        "best_bid": best_bid.map(|d| d.to_string()),
        "best_offer": best_offer.map(|d| d.to_string()),
        "mid": mid.map(|d| d.to_string()),
        "spread": spread.map(|d| d.to_string()),
        "spread_bps": spread_bps.map(|d| d.to_string()),
        "bid_levels": dep.bids.len(),
        "offer_levels": dep.offers.len(),
        "bid_qty_total": bid_qty_sum.to_string(),
        "bid_notional_total": bid_notional_sum.to_string(),
        "offer_qty_total": offer_qty_sum.to_string(),
        "offer_notional_total": offer_notional_sum.to_string(),
        "probe_qty": probe_qty.to_string(),
        "probe_buy_vwap": buy_probe.vwap.map(|d| d.to_string()),
        "probe_buy_filled": buy_probe.filled.to_string(),
        "probe_buy_levels_consumed": buy_probe.levels_used,
        "probe_buy_slippage_bps": buy_probe.slippage_bps.map(|d| d.to_string()),
        "probe_sell_vwap": sell_probe.vwap.map(|d| d.to_string()),
        "probe_sell_filled": sell_probe.filled.to_string(),
        "probe_sell_levels_consumed": sell_probe.levels_used,
        "probe_sell_slippage_bps": sell_probe.slippage_bps.map(|d| d.to_string()),
    }))
}

struct Probe {
    vwap: Option<Decimal>,
    filled: Decimal,
    levels_used: u32,
    slippage_bps: Option<Decimal>,
}

fn walk(levels: &[OrderbookLevel], target: Decimal, mid: Option<Decimal>) -> Probe {
    let mut remaining = target;
    let mut weighted_cost = Decimal::ZERO;
    let mut filled = Decimal::ZERO;
    let mut used: u32 = 0;
    for l in levels {
        let lvl_qty = Decimal::from_str(&l.total_quantity).unwrap_or(Decimal::ZERO);
        let lvl_px = Decimal::from_str(&l.price).unwrap_or(Decimal::ZERO);
        if lvl_qty <= Decimal::ZERO {
            continue;
        }
        used += 1;
        if remaining <= lvl_qty {
            weighted_cost += remaining * lvl_px;
            filled += remaining;
            break;
        } else {
            weighted_cost += lvl_qty * lvl_px;
            filled += lvl_qty;
            remaining -= lvl_qty;
        }
    }
    let vwap = if filled > Decimal::ZERO {
        Some(weighted_cost / filled)
    } else {
        None
    };
    let slippage_bps = match (vwap, mid) {
        (Some(v), Some(m)) if m > Decimal::ZERO => Some((v - m).abs() / m * Decimal::from(10000)),
        _ => None,
    };
    Probe {
        vwap,
        filled,
        levels_used: used,
        slippage_bps,
    }
}

fn depth_totals(levels: &[OrderbookLevel]) -> (Decimal, Decimal) {
    let mut qty_sum = Decimal::ZERO;
    let mut not_sum = Decimal::ZERO;
    for l in levels {
        let q = Decimal::from_str(&l.total_quantity).unwrap_or(Decimal::ZERO);
        let p = Decimal::from_str(&l.price).unwrap_or(Decimal::ZERO);
        qty_sum += q;
        not_sum += q * p;
    }
    (qty_sum, not_sum)
}

fn level_price(l: &OrderbookLevel) -> Option<Decimal> {
    Decimal::from_str(&l.price).ok()
}
