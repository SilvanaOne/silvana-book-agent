//! Trend Analysis Agent — technical-analysis indicator publisher
//!
//! For each market, polls the mid price at a fixed interval, maintains a
//! rolling buffer of the last `--window` samples, and computes a small set of
//! technical indicators each tick:
//!
//! - SMA (simple moving average)
//! - EMA (exponential moving average), α = 2 / (window + 1)
//! - Bollinger Bands (upper, lower) at ± k × standard deviation
//! - RSI (Wilder, 14-period default within the window)
//! - MACD (12-EMA − 26-EMA) using sub-windows when the buffer permits
//!
//! Publishes one JSON record per market per tick to stdout / JSONL file /
//! HTTP webhook. Read-only — no orders, no ledger writes.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-trend-analysis")]
#[command(about = "Compute technical-analysis indicators and publish them")]
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

        /// Rolling window length (used for SMA/EMA/Bollinger)
        #[arg(long, default_value = "30")]
        window: usize,

        /// Bollinger band width in standard deviations
        #[arg(long, default_value = "2.0")]
        bollinger_k: f64,

        /// RSI lookback in samples (typical 14)
        #[arg(long, default_value = "14")]
        rsi_period: usize,

        /// Poll interval in seconds
        #[arg(long, default_value = "30")]
        poll_secs: u64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_trend_analysis", "orderbook_agent_logic"],
        "agent-trend-analysis",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            window,
            bollinger_k,
            rsi_period,
            poll_secs,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            if window < 2 {
                anyhow::bail!("--window must be >= 2");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, window, bollinger_k, rsi_period, poll_secs, sinks).await
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
            Some(path) => Some(Mutex::new(
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                    .with_context(|| format!("Failed to open log file {:?}", path))?,
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
    window: usize,
    bollinger_k: f64,
    rsi_period: usize,
    poll_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting trend-analysis: markets={:?} window={} bollinger_k={} rsi_period={} poll={}s",
        markets, window, bollinger_k, rsi_period, poll_secs
    );
    let mut client = OrderbookClient::new(&config).await?;
    let mut buffers: HashMap<String, VecDeque<f64>> = HashMap::new();
    for m in &markets {
        buffers.insert(m.clone(), VecDeque::with_capacity(window + 1));
    }

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    let mid = match client.get_price(m).await {
                        Ok(p) => mid_value(&p),
                        Err(e) => { error!("get_price({}): {:#}", m, e); continue; }
                    };
                    if mid <= 0.0 { continue; }
                    let buf = buffers.get_mut(m).expect("inserted above");
                    buf.push_back(mid);
                    while buf.len() > window {
                        buf.pop_front();
                    }
                    sinks.dispatch(snapshot(m, mid, buf, window, bollinger_k, rsi_period)).await;
                }
            }
        }
    }
}

fn snapshot(
    market: &str,
    mid: f64,
    buf: &VecDeque<f64>,
    window: usize,
    bollinger_k: f64,
    rsi_period: usize,
) -> Value {
    let n = buf.len();
    let sma = if n > 0 { buf.iter().sum::<f64>() / n as f64 } else { 0.0 };
    let ema = compute_ema(buf, window);
    let (lower, upper) = bollinger(buf, sma, bollinger_k);
    let rsi = compute_rsi(buf, rsi_period);
    let macd = compute_macd(buf);

    json!({
        "kind": "trend",
        "ts": Utc::now().to_rfc3339(),
        "market_id": market,
        "mid": mid,
        "samples": n,
        "window": window,
        "sma": sma,
        "ema": ema,
        "bollinger_lower": lower,
        "bollinger_upper": upper,
        "rsi_period": rsi_period,
        "rsi": rsi,
        "macd": macd,
    })
}

fn compute_ema(buf: &VecDeque<f64>, window: usize) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    let alpha = 2.0 / (window as f64 + 1.0);
    let mut ema = *buf.front().unwrap();
    for v in buf.iter().skip(1) {
        ema = alpha * v + (1.0 - alpha) * ema;
    }
    ema
}

fn ema_period(buf: &VecDeque<f64>, period: usize) -> Option<f64> {
    if buf.len() < period {
        return None;
    }
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut iter = buf.iter().rev().take(period).collect::<Vec<_>>();
    iter.reverse();
    let mut ema = *iter[0];
    for v in iter.iter().skip(1) {
        ema = alpha * **v + (1.0 - alpha) * ema;
    }
    Some(ema)
}

fn bollinger(buf: &VecDeque<f64>, sma: f64, k: f64) -> (Option<f64>, Option<f64>) {
    let n = buf.len();
    if n < 2 {
        return (None, None);
    }
    let var = buf.iter().map(|v| (v - sma).powi(2)).sum::<f64>() / n as f64;
    let std = var.sqrt();
    (Some(sma - k * std), Some(sma + k * std))
}

fn compute_rsi(buf: &VecDeque<f64>, period: usize) -> Option<f64> {
    if buf.len() <= period {
        return None;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    let slice: Vec<&f64> = buf.iter().rev().take(period + 1).collect();
    let prices: Vec<f64> = slice.iter().rev().map(|v| **v).collect();
    for w in prices.windows(2) {
        let diff = w[1] - w[0];
        if diff > 0.0 {
            gains += diff;
        } else {
            losses -= diff;
        }
    }
    if losses == 0.0 {
        return Some(100.0);
    }
    let rs = gains / losses;
    Some(100.0 - 100.0 / (1.0 + rs))
}

fn compute_macd(buf: &VecDeque<f64>) -> Option<f64> {
    let fast = ema_period(buf, 12)?;
    let slow = ema_period(buf, 26)?;
    Some(fast - slow)
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
