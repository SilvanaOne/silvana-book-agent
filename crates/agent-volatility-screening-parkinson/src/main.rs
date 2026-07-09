//! Volatility Screening Agent — Parkinson (range-based) estimator
//!
//! For each market this agent samples the mid every `--poll-secs` and observes
//! the intra-period high/low (using best bid/ask as a proxy when the price
//! service exposes them; otherwise the running max/min of mids inside the
//! period). At the end of every `--period-secs` window the bar is frozen as a
//! `(H, L)` pair and added to a rolling buffer of the last `--window` closed
//! bars.
//!
//! Once at least two bars have closed the Parkinson estimator publishes:
//!
//! - per-period variance: `sigma² = (1 / (4·ln2)) · mean( ln(H/L)² )`
//! - per-period sigma:    `sqrt(sigma²)`
//! - annualized sigma:    `sigma × sqrt(--periods-per-year)`
//!
//! One JSON record per market per poll is dispatched to stdout / JSONL /
//! webhook. No orders. Read-only.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-volatility-screening-parkinson")]
#[command(about = "Parkinson (range-based) realized-volatility publisher")]
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

        /// Number of completed high/low bars kept in the rolling window.
        #[arg(long, default_value = "60")]
        window: usize,

        /// Length (seconds) of each high/low bar.
        #[arg(long, default_value = "60")]
        period_secs: u64,

        /// Annualization multiplier. Default 525_600 = minute bars per year.
        #[arg(long, default_value = "525600")]
        periods_per_year: f64,

        /// Mid-sampling cadence inside a period (seconds).
        #[arg(long, default_value = "5")]
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

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_volatility_screening", "agent_logic"],
        "agent-volatility-screening-parkinson",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            window,
            period_secs,
            periods_per_year,
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
            if period_secs == 0 {
                anyhow::bail!("--period-secs must be >= 1");
            }
            if poll_secs == 0 {
                anyhow::bail!("--poll-secs must be >= 1");
            }
            if poll_secs > period_secs {
                anyhow::bail!("--poll-secs must be <= --period-secs");
            }
            if !periods_per_year.is_finite() || periods_per_year <= 0.0 {
                anyhow::bail!("--periods-per-year must be > 0");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(
                config,
                markets,
                window,
                period_secs,
                periods_per_year,
                poll_secs,
                sinks,
            )
            .await
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

/// The current, still-open bar for one market.
#[derive(Clone, Copy)]
struct OpenBar {
    started_at: Instant,
    high: f64,
    low: f64,
    samples: u64,
}
impl OpenBar {
    fn new(now: Instant, high: f64, low: f64) -> Self {
        Self { started_at: now, high, low, samples: 1 }
    }
    fn observe(&mut self, high: f64, low: f64) {
        if high > self.high {
            self.high = high;
        }
        if low > 0.0 && low < self.low {
            self.low = low;
        }
        self.samples += 1;
    }
    fn is_closed(&self, now: Instant, period: Duration) -> bool {
        now.duration_since(self.started_at) >= period
    }
}

async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    window: usize,
    period_secs: u64,
    periods_per_year: f64,
    poll_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting volatility-screening (Parkinson): markets={:?} window={} period={}s poll={}s periods/year={}",
        markets, window, period_secs, poll_secs, periods_per_year
    );
    let mut client = OrderbookClient::new(&config).await?;

    let period = Duration::from_secs(period_secs);
    let ann = periods_per_year.sqrt();

    // Per-market state.
    let mut open_bar: HashMap<String, Option<OpenBar>> = HashMap::new();
    let mut closed: HashMap<String, VecDeque<(f64, f64)>> = HashMap::new();
    for m in &markets {
        open_bar.insert(m.clone(), None);
        closed.insert(m.clone(), VecDeque::with_capacity(window + 1));
    }

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    let price = match client.get_price(m).await {
                        Ok(p) => p,
                        Err(e) => { error!("get_price({}): {:#}", m, e); continue; }
                    };
                    let mid = mid_value(&price);
                    if mid <= 0.0 { continue; }

                    // Prefer live top-of-book bid/ask as intra-period range proxy.
                    // Fall back to the mid on both sides when the feed doesn't
                    // expose them (e.g. CoinGecko).
                    let (sample_high, sample_low) = match (price.bid, price.ask) {
                        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (a.max(b), a.min(b)),
                        _ => (mid, mid),
                    };

                    let now = Instant::now();
                    let bar_slot = open_bar.get_mut(m).unwrap();
                    let closed_buf = closed.get_mut(m).unwrap();

                    match bar_slot {
                        None => {
                            *bar_slot = Some(OpenBar::new(now, sample_high, sample_low));
                        }
                        Some(bar) => {
                            if bar.is_closed(now, period) {
                                closed_buf.push_back((bar.high, bar.low));
                                while closed_buf.len() > window { closed_buf.pop_front(); }
                                *bar_slot = Some(OpenBar::new(now, sample_high, sample_low));
                            } else {
                                bar.observe(sample_high, sample_low);
                            }
                        }
                    }

                    let n = closed_buf.len();
                    let (current_h, current_l, current_samples) = bar_slot
                        .map(|b| (b.high, b.low, b.samples))
                        .unwrap_or((mid, mid, 0));

                    let payload = if n < 2 {
                        json!({
                            "kind": "volatility.parkinson",
                            "ts": Utc::now().to_rfc3339(),
                            "market_id": m,
                            "mid": mid,
                            "period_secs": period_secs,
                            "current_bar": {
                                "high": current_h,
                                "low":  current_l,
                                "samples": current_samples,
                            },
                            "closed_bars": n,
                            "warmup": true,
                            "periods_per_year": periods_per_year,
                        })
                    } else {
                        let sum_ln2: f64 = closed_buf
                            .iter()
                            .filter(|(h, l)| *h > 0.0 && *l > 0.0)
                            .map(|(h, l)| {
                                let r = (h / l).ln();
                                r * r
                            })
                            .sum();
                        let mean_ln2 = sum_ln2 / n as f64;
                        let variance_per_period = mean_ln2 / (4.0 * std::f64::consts::LN_2);
                        let sigma_per_period = variance_per_period.max(0.0).sqrt();
                        let sigma_annualized = sigma_per_period * ann;
                        let agg_high = closed_buf.iter().map(|(h, _)| *h).fold(f64::NEG_INFINITY, f64::max);
                        let agg_low = closed_buf.iter().map(|(_, l)| *l).fold(f64::INFINITY, f64::min);
                        json!({
                            "kind": "volatility.parkinson",
                            "ts": Utc::now().to_rfc3339(),
                            "market_id": m,
                            "mid": mid,
                            "period_secs": period_secs,
                            "current_bar": {
                                "high": current_h,
                                "low":  current_l,
                                "samples": current_samples,
                            },
                            "high": agg_high,
                            "low":  agg_low,
                            "samples": n,
                            "sigma_per_period": sigma_per_period,
                            "sigma_annualized": sigma_annualized,
                            "periods_per_year": periods_per_year,
                        })
                    };
                    sinks.dispatch(payload).await;
                }
            }
        }
    }
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
