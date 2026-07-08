//! Risk Alert Agent — passive threshold monitor
//!
//! Polls orderbook state on a schedule and emits an alert when any configured
//! threshold is breached. Unlike `agent-killswitch`, this agent does NOT cancel
//! orders or stop trading — it only notifies.
//!
//! Triggers (any combination):
//! - `--max-open-orders` — total active orders across all markets
//! - `--max-failed-settlements` — count of pending proposals with FAILED status
//! - `--max-open-notional` — sum of price × remaining_quantity across all open orders
//!
//! Sinks: any combination of stdout, append JSONL file, HTTP webhook (POST).
//! At least one sink must be configured.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::SettlementStatus;

#[derive(Parser)]
#[command(name = "agent-risk-alert-passive")]
#[command(about = "Alert when risk thresholds are breached (no cancellation)")]
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
        #[arg(long, default_value = "30")]
        check_interval: u64,

        #[arg(long)]
        max_open_orders: Option<u32>,

        #[arg(long)]
        max_failed_settlements: Option<u32>,

        /// Sum of price × remaining_quantity across all open orders
        #[arg(long)]
        max_open_notional: Option<String>,

        #[arg(long)]
        webhook: Option<String>,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        stdout: bool,
    },
    /// One-off check (single evaluation, no loop)
    Check {
        #[arg(long)]
        max_open_orders: Option<u32>,
        #[arg(long)]
        max_failed_settlements: Option<u32>,
        #[arg(long)]
        max_open_notional: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_risk_alert", "agent_logic"],
        "agent-risk-alert-passive",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            check_interval,
            max_open_orders,
            max_failed_settlements,
            max_open_notional,
            webhook,
            log_file,
            stdout,
        } => {
            if max_open_orders.is_none()
                && max_failed_settlements.is_none()
                && max_open_notional.is_none()
            {
                anyhow::bail!("provide at least one threshold");
            }
            if webhook.is_none() && log_file.is_none() && !stdout {
                anyhow::bail!("provide at least one sink (--webhook / --log-file / --stdout)");
            }
            let notional = max_open_notional
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid --max-open-notional")?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(
                config,
                check_interval,
                max_open_orders,
                max_failed_settlements,
                notional,
                sinks,
            )
            .await
        }
        Commands::Check {
            max_open_orders,
            max_failed_settlements,
            max_open_notional,
        } => {
            let notional = max_open_notional
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid --max-open-notional")?;
            let mut client = OrderbookClient::new(&config).await?;
            let snapshot = sample(&mut client).await?;
            let alerts =
                evaluate(&snapshot, max_open_orders, max_failed_settlements, notional);
            println!(
                "open_orders={} failed_settlements={} open_notional={}",
                snapshot.open_orders, snapshot.failed_settlements, snapshot.open_notional
            );
            if alerts.is_empty() {
                println!("OK");
                Ok(())
            } else {
                for a in &alerts {
                    println!("ALERT: {}", a);
                }
                anyhow::bail!("{} threshold(s) breached", alerts.len())
            }
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

    async fn dispatch(&self, alerts: &[String], snapshot: &Snapshot) {
        let payload = json!({
            "kind": "risk.alert",
            "ts": Utc::now().to_rfc3339(),
            "alerts": alerts,
            "open_orders": snapshot.open_orders,
            "failed_settlements": snapshot.failed_settlements,
            "open_notional": snapshot.open_notional.to_string(),
        });
        let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());

        if self.inner.stdout {
            println!("{}", line);
        }
        if let Some(file) = &self.inner.log_file {
            use tokio::io::AsyncWriteExt;
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

#[derive(Serialize, Clone)]
struct Snapshot {
    open_orders: u32,
    failed_settlements: u32,
    open_notional: Decimal,
}

async fn sample(client: &mut OrderbookClient) -> Result<Snapshot> {
    let orders = client.get_all_active_orders().await.unwrap_or_default();
    let proposals = client.get_pending_proposals().await.unwrap_or_default();
    let mut open_notional = Decimal::ZERO;
    for o in &orders {
        let price = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
        let qty = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
        open_notional += price * qty;
    }
    let failed = proposals
        .iter()
        .filter(|p| p.status == SettlementStatus::Failed as i32)
        .count() as u32;
    Ok(Snapshot {
        open_orders: orders.len() as u32,
        failed_settlements: failed,
        open_notional,
    })
}

fn evaluate(
    s: &Snapshot,
    max_open: Option<u32>,
    max_failed: Option<u32>,
    max_notional: Option<Decimal>,
) -> Vec<String> {
    let mut alerts = Vec::new();
    if let Some(limit) = max_open {
        if s.open_orders > limit {
            alerts.push(format!("open_orders {} > {}", s.open_orders, limit));
        }
    }
    if let Some(limit) = max_failed {
        if s.failed_settlements > limit {
            alerts.push(format!(
                "failed_settlements {} > {}",
                s.failed_settlements, limit
            ));
        }
    }
    if let Some(limit) = max_notional {
        if s.open_notional > limit {
            alerts.push(format!("open_notional {} > {}", s.open_notional, limit));
        }
    }
    alerts
}

async fn run(
    config: BaseConfig,
    check_interval: u64,
    max_open_orders: Option<u32>,
    max_failed_settlements: Option<u32>,
    max_open_notional: Option<Decimal>,
    sinks: Sinks,
) -> Result<()> {
    info!(
        "Starting risk-alert: interval={}s max_open={:?} max_failed={:?} max_notional={:?}",
        check_interval, max_open_orders, max_failed_settlements, max_open_notional
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
            _ = tokio::time::sleep(Duration::from_secs(check_interval)) => {
                let snapshot = match sample(&mut client).await {
                    Ok(s) => s,
                    Err(e) => { error!("sample failed: {:#}", e); continue; }
                };
                info!(
                    "open_orders={} failed_settlements={} open_notional={}",
                    snapshot.open_orders, snapshot.failed_settlements, snapshot.open_notional
                );
                let alerts = evaluate(&snapshot, max_open_orders, max_failed_settlements, max_open_notional);
                if !alerts.is_empty() {
                    for a in &alerts {
                        warn!("ALERT: {}", a);
                    }
                    sinks.dispatch(&alerts, &snapshot).await;
                }
            }
        }
    }
}

