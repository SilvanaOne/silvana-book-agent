//! Market Abuse Screening Agent
//!
//! Watches this party's own order stream and flags two classic abuse patterns:
//!
//! - **Spoofing**: an order is cancelled within `--spoof-window-secs` of being
//!   created. A handful of fast cancels is normal market making, but a sustained
//!   burst beyond `--spoof-burst` within `--spoof-burst-window-secs` is the
//!   pattern auditors look for.
//! - **Layering**: at least `--layer-min-orders` open orders on the same side
//!   of the same market clustered within `--layer-price-band-pct` of one
//!   another. This is the classic "stacked book" pattern.
//!
//! Both checks run on **own orders** (the only ones a JWT-authenticated
//! subscription exposes), so the agent is suited to compliance self-audit and
//! to catching strategy bugs that accidentally produce abuse-shaped flow,
//! rather than detection of third parties. Alerts go to stdout / JSONL file /
//! HTTP webhook.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{
    order_update::EventType as OrderEvent, OrderType, OrderUpdate,
};

#[derive(Parser)]
#[command(name = "agent-market-abuse")]
#[command(about = "Flag spoofing / layering patterns in own order flow")]
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
        #[arg(long)]
        market: Option<String>,

        /// An individual cancel within this many seconds of create marks a spoof candidate
        #[arg(long, default_value = "5")]
        spoof_window_secs: i64,

        /// Burst threshold: this many spoof candidates within the burst window triggers an alert
        #[arg(long, default_value = "5")]
        spoof_burst: u32,

        #[arg(long, default_value = "60")]
        spoof_burst_window_secs: i64,

        /// Layering: minimum simultaneous open orders on one side in one market
        #[arg(long, default_value = "5")]
        layer_min_orders: u32,

        /// Layering: price clustering tolerance (max% spread across the cluster)
        #[arg(long, default_value = "1.0")]
        layer_price_band_pct: f64,

        /// How often to re-check the layering pattern against open orders
        #[arg(long, default_value = "15")]
        layer_poll_secs: u64,

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
        &["agent_market_abuse", "orderbook_agent_logic"],
        "agent-market-abuse",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            spoof_window_secs,
            spoof_burst,
            spoof_burst_window_secs,
            layer_min_orders,
            layer_price_band_pct,
            layer_poll_secs,
            stdout,
            log_file,
            webhook,
        } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(
                config,
                market,
                spoof_window_secs,
                spoof_burst,
                spoof_burst_window_secs,
                layer_min_orders,
                layer_price_band_pct,
                layer_poll_secs,
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
    market_filter: Option<String>,
    spoof_window_secs: i64,
    spoof_burst: u32,
    spoof_burst_window_secs: i64,
    layer_min_orders: u32,
    layer_price_band_pct: f64,
    layer_poll_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting market-abuse: market={:?} spoof_window={}s spoof_burst={} layer_min={} layer_band={}%",
        market_filter, spoof_window_secs, spoof_burst, layer_min_orders, layer_price_band_pct
    );

    let order_birth: Arc<Mutex<HashMap<u64, DateTime<Utc>>>> = Arc::new(Mutex::new(HashMap::new()));
    let spoof_recent: Arc<Mutex<VecDeque<DateTime<Utc>>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Background: order subscription processing spoof checks
    let cfg_sub = config.clone();
    let market_for_sub = market_filter.clone();
    let sinks_sub = sinks.clone();
    let birth_sub = order_birth.clone();
    let spoof_sub = spoof_recent.clone();
    tokio::spawn(async move {
        if let Err(e) = stream_orders(
            cfg_sub,
            market_for_sub,
            spoof_window_secs,
            spoof_burst,
            spoof_burst_window_secs,
            birth_sub,
            spoof_sub,
            sinks_sub,
        )
        .await
        {
            error!("orders stream failed: {:#}", e);
        }
    });

    // Foreground: layering check on a poll
    let mut client = OrderbookClient::new(&config).await?;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(layer_poll_secs)) => {
                if let Err(e) = check_layering(
                    &mut client, market_filter.as_deref(),
                    layer_min_orders, layer_price_band_pct, &sinks,
                ).await {
                    warn!("layering check failed: {:#}", e);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_orders(
    config: BaseConfig,
    market: Option<String>,
    spoof_window_secs: i64,
    spoof_burst: u32,
    spoof_burst_window_secs: i64,
    birth: Arc<Mutex<HashMap<u64, DateTime<Utc>>>>,
    recent: Arc<Mutex<VecDeque<DateTime<Utc>>>>,
    sinks: Arc<Sinks>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_orders(market).await?;
    info!("[orders] stream opened");
    loop {
        match stream.next().await {
            Some(Ok(u)) => {
                handle_order(&u, spoof_window_secs, spoof_burst, spoof_burst_window_secs, &birth, &recent, &sinks).await;
            }
            Some(Err(s)) => {
                error!("[orders] stream error: {s}");
                return Ok(());
            }
            None => {
                warn!("[orders] stream ended");
                return Ok(());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_order(
    u: &OrderUpdate,
    spoof_window_secs: i64,
    spoof_burst: u32,
    spoof_burst_window_secs: i64,
    birth: &Arc<Mutex<HashMap<u64, DateTime<Utc>>>>,
    recent: &Arc<Mutex<VecDeque<DateTime<Utc>>>>,
    sinks: &Arc<Sinks>,
) {
    let kind = OrderEvent::try_from(u.event_type).unwrap_or(OrderEvent::Unspecified);
    let Some(o) = &u.order else { return };
    let now = Utc::now();
    match kind {
        OrderEvent::Created => {
            birth.lock().await.insert(o.order_id, now);
        }
        OrderEvent::Cancelled | OrderEvent::Expired => {
            let mut bmap = birth.lock().await;
            if let Some(created_at) = bmap.remove(&o.order_id) {
                let age = (now - created_at).num_seconds();
                if age <= spoof_window_secs {
                    let mut buf = recent.lock().await;
                    buf.push_back(now);
                    // Trim outside burst window
                    let cutoff = now - chrono::Duration::seconds(spoof_burst_window_secs);
                    while let Some(front) = buf.front() {
                        if *front < cutoff { buf.pop_front(); } else { break; }
                    }
                    let count = buf.len() as u32;
                    if count >= spoof_burst {
                        let payload = json!({
                            "kind": "market_abuse.spoof_burst",
                            "ts": now.to_rfc3339(),
                            "order_id": o.order_id,
                            "market_id": o.market_id,
                            "create_to_cancel_secs": age,
                            "fast_cancels_in_window": count,
                            "spoof_burst_window_secs": spoof_burst_window_secs,
                            "spoof_window_secs": spoof_window_secs,
                            "spoof_burst_threshold": spoof_burst,
                        });
                        warn!(
                            "SPOOF BURST: {} fast cancels in last {}s",
                            count, spoof_burst_window_secs
                        );
                        sinks.dispatch(payload).await;
                    }
                }
            }
        }
        _ => {}
    }
}

async fn check_layering(
    client: &mut OrderbookClient,
    market: Option<&str>,
    layer_min_orders: u32,
    layer_price_band_pct: f64,
    sinks: &Arc<Sinks>,
) -> Result<()> {
    let orders = match market {
        Some(m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };
    if orders.is_empty() {
        return Ok(());
    }
    // Group by (market_id, side)
    let mut groups: HashMap<(String, i32), Vec<(u64, Decimal)>> = HashMap::new();
    for o in &orders {
        let p = match Decimal::from_str(&o.price) { Ok(p) => p, Err(_) => continue };
        groups.entry((o.market_id.clone(), o.order_type)).or_default().push((o.order_id, p));
    }
    let band = layer_price_band_pct / 100.0;
    for ((market_id, side), entries) in groups {
        if entries.len() < layer_min_orders as usize {
            continue;
        }
        let mut prices: Vec<f64> = entries.iter().filter_map(|(_, p)| {
            p.to_string().parse::<f64>().ok()
        }).collect();
        if prices.is_empty() { continue; }
        prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lo = prices.first().copied().unwrap_or(0.0);
        let hi = prices.last().copied().unwrap_or(0.0);
        if lo <= 0.0 { continue; }
        let spread = (hi - lo) / lo;
        if spread <= band {
            let payload = json!({
                "kind": "market_abuse.layering",
                "ts": Utc::now().to_rfc3339(),
                "market_id": market_id,
                "side": match OrderType::try_from(side).unwrap_or(OrderType::Unspecified) {
                    OrderType::Bid => "BID",
                    OrderType::Offer => "OFFER",
                    _ => "?",
                },
                "order_count": entries.len(),
                "lowest_price": lo.to_string(),
                "highest_price": hi.to_string(),
                "band_pct": (spread * 100.0).to_string(),
                "threshold_pct": layer_price_band_pct,
                "order_ids": entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            });
            warn!(
                "LAYERING: {} orders on {} {} within {:.4}% band",
                entries.len(),
                market_id,
                if side == OrderType::Bid as i32 { "BID" } else { "OFFER" },
                spread * 100.0
            );
            sinks.dispatch(payload).await;
        }
    }
    Ok(())
}
