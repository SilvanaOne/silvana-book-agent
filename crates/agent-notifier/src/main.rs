//! Notifier Agent — push events from Silvana subscriptions to external sinks
//!
//! Implements both **Notification** and **Hooks-and-Signals** from the Tier 1
//! roadmap (the descriptions are functionally identical). Subscribes to any
//! combination of `SubscribeOrders`, `SubscribeSettlements` and `StreamPrices`,
//! converts each event to a JSON payload, and dispatches it to one or more
//! sinks: stdout, append-only log file, and/or HTTP webhook (POST JSON).

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{
    order_update::EventType as OrderEvent,
    settlement_update::EventType as SettlementEvent,
    OrderUpdate, SettlementUpdate,
};
use orderbook_proto::pricing::PriceUpdate;

#[derive(Parser)]
#[command(name = "agent-notifier")]
#[command(about = "Subscribe to orderbook events and forward them to external sinks")]
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
    /// Run the notifier loop until shutdown
    Run {
        /// Webhook URL — events POSTed as `{ "kind": ..., "payload": {...} }`
        #[arg(long)]
        webhook: Option<String>,

        /// Append-only JSONL file — one JSON object per line
        #[arg(long)]
        log_file: Option<PathBuf>,

        /// Print events to stdout in addition to other sinks
        #[arg(long)]
        stdout: bool,

        /// Subscribe to own order events
        #[arg(long)]
        orders: bool,

        /// Subscribe to own settlement events
        #[arg(long)]
        settlements: bool,

        /// Subscribe to external price updates for these markets (comma-separated)
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        price_markets: Vec<String>,

        /// Optional market filter for orders/settlements
        #[arg(long)]
        market: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_notifier", "orderbook_agent_logic"],
        "agent-notifier",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            webhook,
            log_file,
            stdout,
            orders,
            settlements,
            price_markets,
            market,
        } => {
            if !orders && !settlements && price_markets.is_empty() {
                anyhow::bail!(
                    "at least one of --orders, --settlements, --price-markets must be set"
                );
            }
            if webhook.is_none() && log_file.is_none() && !stdout {
                anyhow::bail!(
                    "at least one sink required: --webhook, --log-file, or --stdout"
                );
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, sinks, orders, settlements, price_markets, market).await
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
                    .timeout(std::time::Duration::from_secs(10))
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

    async fn dispatch(&self, event: &Event) {
        let payload = json!({
            "kind": event.kind,
            "ts": event.ts,
            "payload": event.payload,
        });
        let line = match serde_json::to_string(&payload) {
            Ok(s) => s,
            Err(e) => {
                error!("failed to serialize event: {e}");
                return;
            }
        };

        if self.inner.stdout {
            println!("{}", line);
        }

        if let Some(file) = &self.inner.log_file {
            let mut f = file.lock().await;
            if let Err(e) = f.write_all(line.as_bytes()).await {
                warn!("log_file write failed: {e}");
            }
            if let Err(e) = f.write_all(b"\n").await {
                warn!("log_file newline write failed: {e}");
            }
        }

        if let (Some(url), Some(http)) = (&self.inner.webhook, &self.inner.http) {
            match http.post(url).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => warn!("webhook returned {} for {}", resp.status(), event.kind),
                Err(e) => warn!("webhook POST failed for {}: {e}", event.kind),
            }
        }
    }
}

#[derive(Serialize)]
struct Event {
    kind: &'static str,
    ts: String,
    payload: Value,
}

async fn run(
    config: BaseConfig,
    sinks: Sinks,
    orders: bool,
    settlements: bool,
    price_markets: Vec<String>,
    market: Option<String>,
) -> Result<()> {
    info!("Starting Notifier");
    info!(
        "orders={} settlements={} price_markets={:?} market={:?}",
        orders, settlements, price_markets, market
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

    if orders {
        let cfg = config.clone();
        let s = sinks.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move { orders_loop(cfg, s, m, sd).await }));
    }
    if settlements {
        let cfg = config.clone();
        let s = sinks.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(
            async move { settlements_loop(cfg, s, m, sd).await },
        ));
    }
    if !price_markets.is_empty() {
        let cfg = config.clone();
        let s = sinks.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(
            async move { prices_loop(cfg, s, price_markets, sd).await },
        ));
    }

    for h in handles {
        if let Err(e) = h.await {
            error!("task panicked: {e}");
        }
    }
    info!("Notifier exited");
    Ok(())
}

async fn orders_loop(
    config: BaseConfig,
    sinks: Sinks,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_orders(market)
        .await
        .context("subscribe_orders failed")?;
    info!("[orders] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => sinks.dispatch(&order_event(&u)).await,
                Some(Err(s)) => { error!("[orders] stream error: {s}"); return Ok(()); }
                None => { warn!("[orders] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn settlements_loop(
    config: BaseConfig,
    sinks: Sinks,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_settlements(market)
        .await
        .context("subscribe_settlements failed")?;
    info!("[settlements] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => sinks.dispatch(&settlement_event(&u)).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn prices_loop(
    config: BaseConfig,
    sinks: Sinks,
    markets: Vec<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .stream_prices(markets.clone(), false, false)
        .await
        .context("stream_prices failed")?;
    info!("[prices] stream opened for {:?}", markets);
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => sinks.dispatch(&price_event(&u)).await,
                Some(Err(s)) => { error!("[prices] stream error: {s}"); return Ok(()); }
                None => { warn!("[prices] stream ended"); return Ok(()); }
            }
        }
    }
}

fn order_event(u: &OrderUpdate) -> Event {
    let kind = match OrderEvent::try_from(u.event_type).unwrap_or(OrderEvent::Unspecified) {
        OrderEvent::Created => "order.created",
        OrderEvent::Updated => "order.updated",
        OrderEvent::Filled => "order.filled",
        OrderEvent::PartiallyFilled => "order.partially_filled",
        OrderEvent::Cancelled => "order.cancelled",
        OrderEvent::Expired => "order.expired",
        OrderEvent::Unspecified => "order.unspecified",
    };
    Event {
        kind,
        ts: Utc::now().to_rfc3339(),
        payload: serde_json::to_value(u.order.as_ref().map(|o| {
            json!({
                "order_id": o.order_id,
                "market_id": o.market_id,
                "order_type": o.order_type,
                "status": o.status,
                "price": o.price,
                "quantity": o.quantity,
                "filled": o.filled_quantity,
                "remaining": o.remaining_quantity,
            })
        }))
        .unwrap_or(Value::Null),
    }
}

fn settlement_event(u: &SettlementUpdate) -> Event {
    let kind = match SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified) {
        SettlementEvent::ProposalCreated => "settlement.proposal_created",
        SettlementEvent::StatusChanged => "settlement.status_changed",
        SettlementEvent::Settled => "settlement.settled",
        SettlementEvent::Failed => "settlement.failed",
        SettlementEvent::Cancelled => "settlement.cancelled",
        SettlementEvent::Unspecified => "settlement.unspecified",
    };
    Event {
        kind,
        ts: Utc::now().to_rfc3339(),
        payload: serde_json::to_value(u.proposal.as_ref().map(|p| {
            json!({
                "proposal_id": p.proposal_id,
                "market_id": p.market_id,
                "buyer": p.buyer,
                "seller": p.seller,
                "base_quantity": p.base_quantity,
                "settlement_price": p.settlement_price,
                "status": p.status,
                "error_message": p.error_message,
            })
        }))
        .unwrap_or(Value::Null),
    }
}

fn price_event(u: &PriceUpdate) -> Event {
    Event {
        kind: "price.update",
        ts: Utc::now().to_rfc3339(),
        payload: json!({
            "market_id": u.market_id,
            "price": u.price,
            "bid": u.bid,
            "ask": u.ask,
            "source": u.source,
        }),
    }
}
