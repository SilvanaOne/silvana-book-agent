//! Trend Analysis Agent — Multi-Timeframe confluence publisher
//!
//! For each market, polls the mid price at a fixed interval and maintains
//! three simultaneous simple moving averages of different windows: short,
//! mid, long. On every tick the agent computes each SMA and its slope
//! (delta versus the previous poll's value) and derives a confluence
//! `alignment` verdict:
//!
//! - `aligned_up`   — all three slopes strictly positive
//! - `aligned_down` — all three slopes strictly negative
//! - `mixed`        — otherwise
//!
//! Warmup: nothing is emitted until the buffer holds at least `--long`
//! samples so every SMA is well-defined. Publishes one JSON record per
//! market per tick to stdout / JSONL file / HTTP webhook. Read-only — no
//! orders, no ledger writes.

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

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-trend-analysis-multi-timeframe")]
#[command(about = "Publish a multi-timeframe SMA confluence signal per market")]
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

        /// Short SMA window (samples)
        #[arg(long, default_value = "5")]
        short: usize,

        /// Mid SMA window (samples)
        #[arg(long, default_value = "20")]
        mid: usize,

        /// Long SMA window (samples). Buffer capacity == this value.
        #[arg(long, default_value = "60")]
        long: usize,

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

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_trend_analysis", "agent_logic"],
        "agent-trend-analysis-multi-timeframe",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            short,
            mid,
            long,
            poll_secs,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            if short < 2 || mid < 2 || long < 2 {
                anyhow::bail!("--short, --mid, --long must each be >= 2");
            }
            if !(short < mid && mid < long) {
                anyhow::bail!("--short must be < --mid and --mid must be < --long");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, short, mid, long, poll_secs, sinks).await
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

/// Per-market rolling state.
struct MarketState {
    buf: VecDeque<f64>,
    prev_short: Option<f64>,
    prev_mid: Option<f64>,
    prev_long: Option<f64>,
}

impl MarketState {
    fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity + 1),
            prev_short: None,
            prev_mid: None,
            prev_long: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    short: usize,
    mid: usize,
    long: usize,
    poll_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting trend-analysis multi-timeframe: markets={:?} short={} mid={} long={} poll={}s",
        markets, short, mid, long, poll_secs
    );
    let mut client = OrderbookClient::new(&config).await?;
    let mut states: HashMap<String, MarketState> = HashMap::new();
    for m in &markets {
        states.insert(m.clone(), MarketState::new(long));
    }

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    let mid_price = match client.get_price(m).await {
                        Ok(p) => mid_value(&p),
                        Err(e) => { error!("get_price({}): {:#}", m, e); continue; }
                    };
                    if mid_price <= 0.0 { continue; }
                    let st = states.get_mut(m).expect("inserted above");
                    st.buf.push_back(mid_price);
                    while st.buf.len() > long {
                        st.buf.pop_front();
                    }

                    // Warmup: need at least `long` samples before emitting.
                    if st.buf.len() < long {
                        continue;
                    }

                    let sma_short = tail_mean(&st.buf, short);
                    let sma_mid = tail_mean(&st.buf, mid);
                    let sma_long = tail_mean(&st.buf, long);

                    let slope_short = st.prev_short.map(|p| sma_short - p);
                    let slope_mid = st.prev_mid.map(|p| sma_mid - p);
                    let slope_long = st.prev_long.map(|p| sma_long - p);

                    st.prev_short = Some(sma_short);
                    st.prev_mid = Some(sma_mid);
                    st.prev_long = Some(sma_long);

                    let alignment = classify(slope_short, slope_mid, slope_long);

                    sinks.dispatch(json!({
                        "kind": "trend.multi_timeframe",
                        "ts": Utc::now().to_rfc3339(),
                        "market": m,
                        "mid": mid_price,
                        "short_window": short,
                        "mid_window": mid,
                        "long_window": long,
                        "sma_short": sma_short,
                        "sma_mid": sma_mid,
                        "sma_long": sma_long,
                        "slope_short": slope_short,
                        "slope_mid": slope_mid,
                        "slope_long": slope_long,
                        "alignment": alignment,
                    })).await;
                }
            }
        }
    }
}

/// Mean of the last `n` samples of `buf`. Caller guarantees `buf.len() >= n >= 1`.
fn tail_mean(buf: &VecDeque<f64>, n: usize) -> f64 {
    let len = buf.len();
    let start = len - n;
    let mut sum = 0.0;
    for i in start..len {
        sum += buf[i];
    }
    sum / n as f64
}

fn classify(s: Option<f64>, m: Option<f64>, l: Option<f64>) -> &'static str {
    match (s, m, l) {
        (Some(a), Some(b), Some(c)) if a > 0.0 && b > 0.0 && c > 0.0 => "aligned_up",
        (Some(a), Some(b), Some(c)) if a < 0.0 && b < 0.0 && c < 0.0 => "aligned_down",
        _ => "mixed",
    }
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
