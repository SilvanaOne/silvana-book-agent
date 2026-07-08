//! Fair Value Screening Agent
//!
//! For each market, polls `GetPrice` against every configured source
//! (`binance_spot`, `bybit`, `coingecko`, …) and computes an aggregate "fair
//! value" using one of `median`, `mean`, or `trimmed_mean` (drops the highest
//! and lowest before averaging). Emits one fair-value record per market per
//! poll to stdout / append JSONL / HTTP webhook.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;

#[derive(Clone, Copy, ValueEnum)]
enum AggMethod {
    Median,
    Mean,
    TrimmedMean,
}

#[derive(Parser)]
#[command(name = "agent-fair-value-multi-source")]
#[command(about = "Aggregate multi-source prices into a single fair value per market")]
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

        /// Sources to query, e.g. binance_spot,bybit,coingecko
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        sources: Vec<String>,

        #[arg(long, default_value = "10")]
        poll_secs: u64,

        #[arg(long, value_enum, default_value_t = AggMethod::Median)]
        method: AggMethod,

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
        &["agent_fair_value", "agent_logic"],
        "agent-fair-value-multi-source",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            sources,
            poll_secs,
            method,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            if sources.is_empty() {
                anyhow::bail!("--sources required");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, sources, poll_secs, method, sinks).await
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

async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    sources: Vec<String>,
    poll_secs: u64,
    method: AggMethod,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting fair-value: markets={:?} sources={:?} poll={}s method={:?}",
        markets,
        sources,
        poll_secs,
        method_name(method)
    );
    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    let mut quotes: Vec<(String, f64)> = Vec::new();
                    for src in &sources {
                        match client.get_price_from_source(m, src).await {
                            Ok(p) if p.last > 0.0 => quotes.push((src.clone(), p.last)),
                            Ok(_) => {} // skip 0/missing
                            Err(e) => warn!("{}@{}: {:#}", m, src, e),
                        }
                    }
                    if quotes.is_empty() {
                        error!("no usable quotes for {}", m);
                        continue;
                    }
                    let prices: Vec<f64> = quotes.iter().map(|(_, p)| *p).collect();
                    let fair = aggregate(&prices, method);
                    let (lo, hi) = bounds(&prices);
                    sinks.dispatch(json!({
                        "kind": "fair_value",
                        "ts": Utc::now().to_rfc3339(),
                        "market_id": m,
                        "method": method_name(method),
                        "fair_value": fair,
                        "low": lo,
                        "high": hi,
                        "samples": quotes.iter().map(|(s, p)| json!({"source": s, "price": p})).collect::<Vec<_>>(),
                    })).await;
                }
            }
        }
    }
}

fn method_name(m: AggMethod) -> &'static str {
    match m {
        AggMethod::Median => "median",
        AggMethod::Mean => "mean",
        AggMethod::TrimmedMean => "trimmed_mean",
    }
}

fn aggregate(prices: &[f64], method: AggMethod) -> f64 {
    let mut sorted: Vec<f64> = prices.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    match method {
        AggMethod::Mean => sorted.iter().sum::<f64>() / n as f64,
        AggMethod::Median => {
            if n % 2 == 1 {
                sorted[n / 2]
            } else {
                (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
            }
        }
        AggMethod::TrimmedMean => {
            if n <= 2 {
                sorted.iter().sum::<f64>() / n as f64
            } else {
                let inner = &sorted[1..n - 1];
                inner.iter().sum::<f64>() / inner.len() as f64
            }
        }
    }
}

fn bounds(prices: &[f64]) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in prices {
        if *p < lo {
            lo = *p;
        }
        if *p > hi {
            hi = *p;
        }
    }
    (lo, hi)
}

// std::fmt::Debug for AggMethod (used in info!) — implemented manually since the enum
// is also used by clap::ValueEnum, and Debug isn't auto-derived through ValueEnum.
impl std::fmt::Debug for AggMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(method_name(*self))
    }
}
