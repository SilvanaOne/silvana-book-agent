//! Contractual Compliance Agent
//!
//! Tracks settled flows against a TOML file of bilateral contracts. Each
//! contract entry declares a counterparty, a market, an obligation window
//! (rolling hours), and a min/max settled notional inside that window:
//!
//! ```toml
//! [[contracts]]
//! id = "lp-quarterly-cc-usdc"
//! counterparty = "party-x-..."
//! market = "CC-USDC"
//! window_hours = 24
//! min_notional = "1000"
//! max_notional = "100000"
//! expires_at = "2026-12-31T23:59:59Z"
//! ```
//!
//! As `SETTLED` events arrive the agent tallies notional per contract and
//! emits `contract.under_floor` / `contract.over_ceiling` events when limits
//! are breached. Periodic `contract.status` events report current tallies so
//! you can see how far you are from each obligation.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{settlement_update::EventType as SettlementEvent, SettlementUpdate};

#[derive(Parser)]
#[command(name = "agent-contractual-compliance")]
#[command(about = "Track settled flow against bilateral contractual obligations")]
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
        contracts: PathBuf,

        #[arg(long, default_value = "60")]
        reload_secs: u64,

        #[arg(long, default_value = "600")]
        status_interval_secs: u64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    Check {
        #[arg(long)]
        contracts: PathBuf,
    },
}

#[derive(Deserialize, Clone, Debug)]
struct ContractsFile {
    #[serde(default)]
    contracts: Vec<Contract>,
}

#[derive(Deserialize, Clone, Debug)]
struct Contract {
    id: String,
    counterparty: String,
    market: String,
    window_hours: u32,
    #[serde(default)]
    min_notional: Option<String>,
    #[serde(default)]
    max_notional: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_contractual_compliance", "orderbook_agent_logic"],
        "agent-contractual-compliance",
    );

    match cli.command {
        Commands::Run {
            contracts,
            reload_secs,
            status_interval_secs,
            stdout,
            log_file,
            webhook,
        } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, contracts, reload_secs, status_interval_secs, sinks).await
        }
        Commands::Check { contracts } => {
            let f = load_contracts(&contracts).await?;
            println!("Loaded {} contracts:", f.contracts.len());
            for c in &f.contracts {
                println!(
                    "  {} party={} market={} window={}h min={:?} max={:?} expires={:?}",
                    c.id, c.counterparty, c.market, c.window_hours, c.min_notional, c.max_notional, c.expires_at
                );
            }
            Ok(())
        }
    }
}

async fn load_contracts(path: &PathBuf) -> Result<ContractsFile> {
    let body = tokio::fs::read_to_string(path).await.with_context(|| format!("read {:?}", path))?;
    Ok(toml::from_str(&body).with_context(|| format!("parse {:?}", path))?)
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

#[derive(Default, Clone)]
struct Tally {
    samples: VecDeque<(DateTime<Utc>, Decimal)>,
}
impl Tally {
    fn record(&mut self, when: DateTime<Utc>, notional: Decimal) {
        self.samples.push_back((when, notional));
    }
    fn total_within(&mut self, window_hours: u32) -> Decimal {
        let cutoff = Utc::now() - chrono::Duration::hours(window_hours as i64);
        while let Some(front) = self.samples.front() {
            if front.0 < cutoff {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        self.samples.iter().fold(Decimal::ZERO, |acc, (_, n)| acc + *n)
    }
}

async fn run(
    config: BaseConfig,
    contracts_path: PathBuf,
    reload_secs: u64,
    status_interval_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting contractual-compliance: contracts={:?} reload={}s status={}s",
        contracts_path, reload_secs, status_interval_secs
    );

    let contracts = Arc::new(RwLock::new(load_contracts(&contracts_path).await?));
    let tallies: Arc<Mutex<HashMap<String, Tally>>> = Arc::new(Mutex::new(HashMap::new()));

    let shutdown = Arc::new(Notify::new());
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sd.notify_waiters();
        }
    });

    // Reload loop
    let c_clone = contracts.clone();
    let p_clone = contracts_path.clone();
    let sd_reload = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sd_reload.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(reload_secs)) => {
                    match load_contracts(&p_clone).await {
                        Ok(c) => {
                            *c_clone.write().await = c;
                            info!("contracts reloaded");
                        }
                        Err(e) => warn!("contracts reload failed: {:#}", e),
                    }
                }
            }
        }
    });

    // Status loop
    let c_st = contracts.clone();
    let t_st = tallies.clone();
    let s_st = sinks.clone();
    let sd_status = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sd_status.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(status_interval_secs)) => {
                    emit_status(&c_st, &t_st, &s_st).await;
                }
            }
        }
    });

    // Settlement stream
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_settlements(None).await?;
    info!("[settlements] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => apply_event(&u, &contracts, &tallies, &sinks).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn apply_event(
    u: &SettlementUpdate,
    contracts: &Arc<RwLock<ContractsFile>>,
    tallies: &Arc<Mutex<HashMap<String, Tally>>>,
    sinks: &Arc<Sinks>,
) {
    let evt = SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified);
    if evt != SettlementEvent::Settled {
        return;
    }
    let Some(p) = &u.proposal else { return };
    let notional = Decimal::from_str(&p.quote_quantity).unwrap_or(Decimal::ZERO);
    let now = Utc::now();
    let c_snap = contracts.read().await.clone();
    let mut t = tallies.lock().await;
    for c in &c_snap.contracts {
        if c.market != p.market_id {
            continue;
        }
        if c.counterparty != p.buyer && c.counterparty != p.seller {
            continue;
        }
        let tally = t.entry(c.id.clone()).or_default();
        tally.record(now, notional);
        let total = tally.total_within(c.window_hours);
        if let Some(maxs) = &c.max_notional {
            if let Ok(max_d) = Decimal::from_str(maxs) {
                if total > max_d {
                    let payload = json!({
                        "kind": "contract.over_ceiling",
                        "ts": now.to_rfc3339(),
                        "contract_id": c.id,
                        "counterparty": c.counterparty,
                        "market": c.market,
                        "window_hours": c.window_hours,
                        "notional_window": total.to_string(),
                        "max_notional": max_d.to_string(),
                        "proposal_id": p.proposal_id,
                    });
                    warn!(
                        "contract {} OVER CEILING: {} > {} ({}h)",
                        c.id, total, max_d, c.window_hours
                    );
                    sinks.dispatch(payload).await;
                }
            }
        }
    }
}

async fn emit_status(
    contracts: &Arc<RwLock<ContractsFile>>,
    tallies: &Arc<Mutex<HashMap<String, Tally>>>,
    sinks: &Arc<Sinks>,
) {
    let c_snap = contracts.read().await.clone();
    let mut t = tallies.lock().await;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    let mut entries: Vec<Value> = Vec::new();
    for c in &c_snap.contracts {
        let total = t.entry(c.id.clone()).or_default().total_within(c.window_hours);
        let expires_passed = c
            .expires_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc) < now)
            .unwrap_or(false);
        let mut under_floor = false;
        if let Some(mins) = &c.min_notional {
            if let Ok(min_d) = Decimal::from_str(mins) {
                if total < min_d {
                    under_floor = true;
                }
            }
        }
        entries.push(json!({
            "contract_id": c.id,
            "counterparty": c.counterparty,
            "market": c.market,
            "window_hours": c.window_hours,
            "notional_window": total.to_string(),
            "min_notional": c.min_notional,
            "max_notional": c.max_notional,
            "expires_at": c.expires_at,
            "expired": expires_passed,
            "under_floor": under_floor,
        }));
        if under_floor {
            sinks.dispatch(json!({
                "kind": "contract.under_floor",
                "ts": now_str,
                "contract_id": c.id,
                "notional_window": total.to_string(),
                "min_notional": c.min_notional,
                "window_hours": c.window_hours,
            })).await;
        }
    }
    sinks.dispatch(json!({
        "kind": "contract.status",
        "ts": now_str,
        "contracts": entries,
    })).await;
}
