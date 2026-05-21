//! Yield Rotation Agent
//!
//! Ranks a basket of markets by a simple "yield score" and emits a rotation
//! signal whenever the top-ranked market changes. The score combines three
//! factors per market:
//!
//! - **24h percentage change** (from GetPrice): the directional carry.
//! - **24h volume** (from GetMarketData): depth proxy for whether the carry
//!   is actually realizable at size.
//! - **spread penalty**: `(ask − bid) / mid`, subtracted from the score so
//!   markets with wide spreads (high effective cost) rank lower.
//!
//! Score formula (configurable weights):
//! ```
//! score = w_change × change_24h_pct
//!       + w_volume × log10(volume_24h)
//!       − w_spread × spread_pct
//! ```
//!
//! Each cycle publishes the current ranking. When the top market changes, an
//! additional `yield.rotation_signal` record is emitted. Downstream agents
//! (e.g. an automation script wired to `agent-batch-orders`) can act on those
//! signals to move capital. Read-only by itself — no ledger writes.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-yield-rotation")]
#[command(about = "Rank markets by carry/depth and publish rotation signals")]
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

        #[arg(long, default_value = "300")]
        poll_secs: u64,

        /// Weight for 24h percentage change in the score
        #[arg(long, default_value = "1.0")]
        weight_change: f64,

        /// Weight for log10(volume_24h) in the score
        #[arg(long, default_value = "0.5")]
        weight_volume: f64,

        /// Weight for the spread penalty (subtracted)
        #[arg(long, default_value = "100.0")]
        weight_spread: f64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    /// One-off snapshot of current ranking
    Snapshot {
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        #[arg(long, default_value = "1.0")]
        weight_change: f64,

        #[arg(long, default_value = "0.5")]
        weight_volume: f64,

        #[arg(long, default_value = "100.0")]
        weight_spread: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_yield_rotation", "orderbook_agent_logic"],
        "agent-yield-rotation",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            poll_secs,
            weight_change,
            weight_volume,
            weight_spread,
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
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(
                config,
                markets,
                poll_secs,
                weight_change,
                weight_volume,
                weight_spread,
                sinks,
            )
            .await
        }
        Commands::Snapshot {
            markets,
            weight_change,
            weight_volume,
            weight_spread,
        } => {
            let mut client = OrderbookClient::new(&config).await?;
            let ranking = rank_markets(&mut client, &markets, weight_change, weight_volume, weight_spread).await;
            let payload = json!({
                "kind": "yield.ranking",
                "ts": Utc::now().to_rfc3339(),
                "ranking": ranking,
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
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

#[allow(clippy::too_many_arguments)]
async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    poll_secs: u64,
    w_change: f64,
    w_volume: f64,
    w_spread: f64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting yield-rotation: markets={:?} poll={}s weights(change={}, volume={}, spread={})",
        markets, poll_secs, w_change, w_volume, w_spread
    );

    let mut client = OrderbookClient::new(&config).await?;
    let mut last_top: Option<String> = None;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                let ranking = rank_markets(&mut client, &markets, w_change, w_volume, w_spread).await;
                if ranking.is_empty() {
                    warn!("no markets had usable data this cycle");
                    continue;
                }
                let top = ranking[0]["market_id"].as_str().map(|s| s.to_string());
                let payload = json!({
                    "kind": "yield.ranking",
                    "ts": Utc::now().to_rfc3339(),
                    "ranking": ranking.clone(),
                });
                sinks.dispatch(payload).await;
                if let Some(t) = &top {
                    if last_top.as_ref().map(|prev| prev != t).unwrap_or(true) {
                        let rotation = json!({
                            "kind": "yield.rotation_signal",
                            "ts": Utc::now().to_rfc3339(),
                            "previous_top": last_top,
                            "new_top": t,
                            "ranking": ranking,
                        });
                        warn!("ROTATION: top {:?} → {:?}", last_top, top);
                        sinks.dispatch(rotation).await;
                        last_top = Some(t.clone());
                    }
                }
            }
        }
    }
}

async fn rank_markets(
    client: &mut OrderbookClient,
    markets: &[String],
    w_change: f64,
    w_volume: f64,
    w_spread: f64,
) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for m in markets {
        let price = match client.get_price(m).await {
            Ok(p) => p,
            Err(e) => {
                error!("get_price({}): {:#}", m, e);
                continue;
            }
        };
        let mid = match (price.bid, price.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
            _ if price.last > 0.0 => price.last,
            _ => continue,
        };
        let spread_pct = match (price.bid, price.ask) {
            (Some(b), Some(a)) if mid > 0.0 => (a - b) / mid * 100.0,
            _ => 0.0,
        };
        let change_pct = price.change_24h_percent.unwrap_or(0.0);
        let volume = price.volume_24h.unwrap_or(0.0);
        let log_volume = if volume > 0.0 { volume.log10() } else { 0.0 };
        let score = w_change * change_pct + w_volume * log_volume - w_spread * spread_pct;
        entries.push(json!({
            "market_id": m,
            "score": score,
            "change_24h_pct": change_pct,
            "volume_24h": volume,
            "log10_volume": log_volume,
            "spread_pct": spread_pct,
            "mid": mid,
            "source": price.source,
        }));
    }
    entries.sort_by(|a, b| {
        let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    entries
}
