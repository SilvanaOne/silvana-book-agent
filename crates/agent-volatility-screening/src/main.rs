//! Volatility Screening Agent — rolling realized-vol publisher
//!
//! For each market polls the mid price every `--poll-secs` and maintains a
//! rolling window of the last `--window` samples. After each tick it computes:
//!
//! - log return at this sample (ln(p_t / p_{t-1}))
//! - sample std-dev of log returns over the window
//! - **realized volatility** (annualized) = std × sqrt(periods_per_year)
//!   where periods_per_year = (365·24·3600) / poll_secs assuming continuous
//!   sampling; this is correct for a tick-level vol estimate.
//!
//! Publishes one record per market per tick to stdout / JSONL file / webhook.

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
#[command(name = "agent-volatility-screening")]
#[command(about = "Rolling realized-volatility publisher")]
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

        #[arg(long, default_value = "100")]
        window: usize,

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
        &["agent_volatility_screening", "orderbook_agent_logic"],
        "agent-volatility-screening",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            window,
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
            if poll_secs == 0 {
                anyhow::bail!("--poll-secs must be >= 1");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, window, poll_secs, sinks).await
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
    window: usize,
    poll_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting volatility-screening: markets={:?} window={} poll={}s",
        markets, window, poll_secs
    );
    let mut client = OrderbookClient::new(&config).await?;
    // Each market keeps a rolling buffer of mid prices and of log returns.
    let mut prices: HashMap<String, VecDeque<f64>> = HashMap::new();
    let mut returns: HashMap<String, VecDeque<f64>> = HashMap::new();
    for m in &markets {
        prices.insert(m.clone(), VecDeque::with_capacity(window + 1));
        returns.insert(m.clone(), VecDeque::with_capacity(window + 1));
    }

    let periods_per_year = (365.0 * 24.0 * 3600.0) / poll_secs as f64;
    let ann = periods_per_year.sqrt();

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

                    let prices_buf = prices.get_mut(m).unwrap();
                    let returns_buf = returns.get_mut(m).unwrap();

                    if let Some(prev) = prices_buf.back().copied() {
                        if prev > 0.0 {
                            returns_buf.push_back((mid / prev).ln());
                            while returns_buf.len() > window { returns_buf.pop_front(); }
                        }
                    }
                    prices_buf.push_back(mid);
                    while prices_buf.len() > window + 1 { prices_buf.pop_front(); }

                    let n = returns_buf.len();
                    let payload = if n < 2 {
                        json!({
                            "kind": "volatility",
                            "ts": Utc::now().to_rfc3339(),
                            "market_id": m,
                            "mid": mid,
                            "samples": n,
                            "warmup": true,
                        })
                    } else {
                        let mean = returns_buf.iter().sum::<f64>() / n as f64;
                        let var = returns_buf.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
                        let std_ret = var.sqrt();
                        let realized_vol_annualized = std_ret * ann;
                        json!({
                            "kind": "volatility",
                            "ts": Utc::now().to_rfc3339(),
                            "market_id": m,
                            "mid": mid,
                            "samples": n,
                            "mean_return": mean,
                            "std_return": std_ret,
                            "realized_vol_annualized": realized_vol_annualized,
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
