//! Orderbook Streaming Agent — fan out market data to JSONL sinks
//!
//! Subscribes to internal Silvana orderbook depth and the external price stream
//! for a list of markets and forwards every update to any combination of stdout,
//! append-only JSONL file, and HTTP webhook. Pure data fanout: no orders, no
//! ledger writes, no party-specific state.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{orderbook_update::UpdateType, OrderbookLevel, OrderbookUpdate};
use orderbook_proto::pricing::PriceUpdate;

#[derive(Parser)]
#[command(name = "agent-orderbook-streaming")]
#[command(about = "Stream Silvana depth + external prices to JSONL sinks")]
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

        #[arg(long, default_value = "10")]
        depth: u32,

        #[arg(long)]
        include_orderbook: bool,

        #[arg(long)]
        include_trades: bool,

        #[arg(long)]
        no_depth: bool,

        #[arg(long)]
        no_prices: bool,

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
        &["agent_orderbook_streaming", "agent_logic"],
        "agent-orderbook-streaming",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            depth,
            include_orderbook,
            include_trades,
            no_depth,
            no_prices,
            stdout,
            log_file,
            webhook,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets is required");
            }
            if no_depth && no_prices {
                anyhow::bail!("nothing to stream — both --no-depth and --no-prices set");
            }
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink (--stdout / --log-file / --webhook)");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(
                config,
                markets,
                depth,
                include_orderbook,
                include_trades,
                no_depth,
                no_prices,
                sinks,
            )
            .await
        }
    }
}

#[derive(Clone)]
struct Sinks {
    inner: Arc<SinkInner>,
}
struct SinkInner {
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
    ) -> Result<Self> {
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
        Ok(Self {
            inner: Arc::new(SinkInner {
                webhook,
                http,
                log_file: file,
                stdout,
            }),
        })
    }

    async fn dispatch(&self, payload: Value) {
        let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
        if self.inner.stdout {
            println!("{}", line);
        }
        if let Some(file) = &self.inner.log_file {
            let mut f = file.lock().await;
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
        }
        if let (Some(url), Some(http)) = (&self.inner.webhook, &self.inner.http) {
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
    depth: u32,
    include_orderbook: bool,
    include_trades: bool,
    no_depth: bool,
    no_prices: bool,
    sinks: Sinks,
) -> Result<()> {
    info!(
        "Starting orderbook-streaming: markets={:?} depth={} ob={} trades={} depth_sub={} price_sub={}",
        markets, depth, include_orderbook, include_trades, !no_depth, !no_prices
    );

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
        let s = sinks.clone();
        let ms = markets.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            stream_prices(cfg, ms, include_orderbook, include_trades, s, sd).await
        }));
    }
    if !no_depth {
        for market_id in markets.iter().cloned() {
            let cfg = config.clone();
            let s = sinks.clone();
            let sd = shutdown.clone();
            handles.push(tokio::spawn(async move {
                stream_depth(cfg, market_id, depth, s, sd).await
            }));
        }
    }

    for h in handles {
        if let Err(e) = h.await {
            error!("task panicked: {e}");
        }
    }
    info!("orderbook-streaming exited");
    Ok(())
}

async fn stream_prices(
    config: BaseConfig,
    markets: Vec<String>,
    include_orderbook: bool,
    include_trades: bool,
    sinks: Sinks,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .stream_prices(markets, include_orderbook, include_trades)
        .await
        .context("stream_prices failed")?;
    info!("[prices] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => sinks.dispatch(price_payload(&u)).await,
                Some(Err(s)) => { error!("[prices] stream error: {s}"); return Ok(()); }
                None => { warn!("[prices] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn stream_depth(
    config: BaseConfig,
    market_id: String,
    depth: u32,
    sinks: Sinks,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_orderbook_depth(&market_id, Some(depth))
        .await
        .with_context(|| format!("subscribe_orderbook_depth({}) failed", market_id))?;
    info!("[depth:{}] stream opened", market_id);
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => sinks.dispatch(depth_payload(&u)).await,
                Some(Err(s)) => { error!("[depth:{}] stream error: {s}", market_id); return Ok(()); }
                None => { warn!("[depth:{}] stream ended", market_id); return Ok(()); }
            }
        }
    }
}

fn price_payload(u: &PriceUpdate) -> Value {
    json!({
        "kind": "price",
        "ts": Utc::now().to_rfc3339(),
        "market_id": u.market_id,
        "price": u.price,
        "bid": u.bid,
        "ask": u.ask,
        "source": u.source,
        "trade": u.trade.as_ref().map(|t| json!({
            "price": t.price,
            "qty": t.quantity,
            "buyer_maker": t.is_buyer_maker,
        })),
        "orderbook": u.orderbook.as_ref().map(|ob| json!({
            "bids": ob.bids.len(),
            "asks": ob.asks.len(),
        })),
    })
}

fn depth_payload(u: &OrderbookUpdate) -> Value {
    let kind = match UpdateType::try_from(u.update_type).unwrap_or(UpdateType::Unspecified) {
        UpdateType::Snapshot => "snapshot",
        UpdateType::Delta => "delta",
        UpdateType::Unspecified => "?",
    };
    json!({
        "kind": "depth",
        "ts": Utc::now().to_rfc3339(),
        "market_id": u.market_id,
        "update_type": kind,
        "sequence": u.sequence_number,
        "bids": u.bid_updates.iter().map(level_json).collect::<Vec<_>>(),
        "offers": u.offer_updates.iter().map(level_json).collect::<Vec<_>>(),
    })
}

fn level_json(l: &OrderbookLevel) -> Value {
    json!({ "price": l.price, "qty": l.total_quantity, "orders": l.order_count })
}
