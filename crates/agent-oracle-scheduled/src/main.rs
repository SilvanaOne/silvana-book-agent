//! Oracle Agent — schedule-based price publisher
//!
//! Polls `GetPrice` for every market in `--markets` on a fixed interval and
//! publishes timestamped quotes to one or more sinks (stdout / append JSONL /
//! HTTP webhook). Lets a downstream consumer treat Silvana's aggregated price
//! feed as a structured oracle stream without needing direct gRPC access.

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

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::pricing::GetPriceResponse;

#[derive(Parser)]
#[command(name = "agent-oracle-scheduled")]
#[command(about = "Poll prices on a schedule and publish a structured oracle feed")]
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

        /// Optional source override: pass to GetPrice e.g. "binance_spot", "bybit", "coingecko"
        #[arg(long)]
        source: Option<String>,

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
        &["agent_oracle", "agent_logic"],
        "agent-oracle-scheduled",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            poll_secs,
            source,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets is required");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink (--stdout / --log-file / --webhook)");
            }
            if poll_secs == 0 {
                anyhow::bail!("--poll-secs must be >= 1");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, markets, poll_secs, source, sinks).await
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
            Some(
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .context("Failed to build HTTP client")?,
            )
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
        Ok(Arc::new(Self {
            webhook,
            http,
            log_file: file,
            stdout,
        }))
    }

    async fn dispatch(&self, payload: Value) {
        let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
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
    source: Option<String>,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting oracle: markets={:?} poll={}s source={:?}",
        markets, poll_secs, source
    );
    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown signal received");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                for m in &markets {
                    let resp = match &source {
                        Some(src) => client.get_price_from_source(m, src).await,
                        None => client.get_price(m).await,
                    };
                    match resp {
                        Ok(p) => sinks.dispatch(payload(m, &p)).await,
                        Err(e) => error!("get_price({}): {:#}", m, e),
                    }
                }
            }
        }
    }
}

fn payload(market_id: &str, p: &GetPriceResponse) -> Value {
    json!({
        "kind": "oracle.price",
        "ts": Utc::now().to_rfc3339(),
        "market_id": market_id,
        "last": p.last,
        "bid": p.bid,
        "ask": p.ask,
        "volume_24h": p.volume_24h,
        "change_24h_percent": p.change_24h_percent,
        "high_24h": p.high_24h,
        "low_24h": p.low_24h,
        "source": p.source,
        "source_timestamp": p.timestamp.as_ref().map(|t| format!("{}.{:09}", t.seconds, t.nanos)),
    })
}
