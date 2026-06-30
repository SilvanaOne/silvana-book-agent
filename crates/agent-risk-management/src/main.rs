//! Risk Management Agent — composite limits enforcer
//!
//! Combines every check that the more focused risk agents do individually
//! (open orders count, open notional, pending settlements, failed settlements,
//! per-market exposure) into one TOML-driven policy. Each cycle evaluates the
//! whole policy and emits a `risk.status` event with per-check pass/fail
//! flags. With `--enforce`, breached checks trigger targeted cancellations:
//!
//! - `max_open_orders` / `max_open_notional` exceeded → cancel the
//!   newest-`order_id` orders until back inside the limit.
//! - `per_market_max_notional` exceeded → cancel orders on the offending
//!   market only.
//! - `max_pending_settlements` and `max_failed_settlements` are reported but
//!   *not* enforced (the orderbook owns settlement state — see
//!   `agent-failure-recovery` and `agent-killswitch` for those flows).

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{Order, SettlementStatus};

#[derive(Parser)]
#[command(name = "agent-risk-management")]
#[command(about = "Composite live-state risk limits + optional cancel enforcement")]
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
        policy: PathBuf,

        #[arg(long, default_value = "30")]
        check_interval: u64,

        /// Cancel orders to bring violated limits back inside the policy
        #[arg(long)]
        enforce: bool,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    Check {
        #[arg(long)]
        policy: PathBuf,
    },
}

#[derive(Deserialize, Default, Clone, Debug)]
struct Policy {
    #[serde(default)]
    max_open_orders: Option<u32>,
    #[serde(default)]
    max_open_notional: Option<String>,
    #[serde(default)]
    max_pending_settlements: Option<u32>,
    #[serde(default)]
    max_failed_settlements: Option<u32>,
    #[serde(default)]
    per_market_max_notional: HashMap<String, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_risk_management", "agent_logic"],
        "agent-risk-management",
    );

    match cli.command {
        Commands::Run { policy, check_interval, enforce, stdout, log_file, webhook } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, policy, check_interval, enforce, sinks).await
        }
        Commands::Check { policy } => {
            let p = load_policy(&policy).await?;
            println!("{}", serde_json::to_string_pretty(&serde_json::to_value(&p)?)?);
            Ok(())
        }
    }
}

async fn load_policy(path: &PathBuf) -> Result<Policy> {
    let body = tokio::fs::read_to_string(path).await.with_context(|| format!("read {:?}", path))?;
    Ok(toml::from_str(&body).with_context(|| format!("parse {:?}", path))?)
}

impl serde::Serialize for Policy {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = serde_json::Map::new();
        if let Some(n) = self.max_open_orders {
            map.insert("max_open_orders".into(), n.into());
        }
        if let Some(n) = &self.max_open_notional {
            map.insert("max_open_notional".into(), n.clone().into());
        }
        if let Some(n) = self.max_pending_settlements {
            map.insert("max_pending_settlements".into(), n.into());
        }
        if let Some(n) = self.max_failed_settlements {
            map.insert("max_failed_settlements".into(), n.into());
        }
        let pm: serde_json::Map<_, _> = self
            .per_market_max_notional
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        if !pm.is_empty() {
            map.insert("per_market_max_notional".into(), serde_json::Value::Object(pm));
        }
        s.collect_map(map)
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
                tokio::fs::OpenOptions::new().create(true).append(true).open(&p).await
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

async fn run(
    config: BaseConfig,
    policy_path: PathBuf,
    check_interval: u64,
    enforce: bool,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting risk-management: policy={:?} interval={}s enforce={}",
        policy_path, check_interval, enforce
    );
    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(check_interval)) => {
                let policy = match load_policy(&policy_path).await {
                    Ok(p) => p,
                    Err(e) => { error!("policy load failed: {:#}", e); continue; }
                };
                if let Err(e) = evaluate(&mut client, &policy, enforce, &sinks).await {
                    error!("evaluate failed: {:#}", e);
                }
            }
        }
    }
}

async fn evaluate(
    client: &mut OrderbookClient,
    policy: &Policy,
    enforce: bool,
    sinks: &Arc<Sinks>,
) -> Result<()> {
    let orders = client.get_all_active_orders().await.unwrap_or_default();
    let proposals = client.get_pending_proposals().await.unwrap_or_default();

    let mut open_notional = Decimal::ZERO;
    let mut per_market: HashMap<String, Decimal> = HashMap::new();
    for o in &orders {
        let p = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
        let q = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
        let n = p * q;
        open_notional += n;
        *per_market.entry(o.market_id.clone()).or_default() += n;
    }
    let pending = proposals.iter().filter(|p| p.status == SettlementStatus::Pending as i32).count() as u32;
    let failed = proposals.iter().filter(|p| p.status == SettlementStatus::Failed as i32).count() as u32;

    let mut hits: Vec<String> = Vec::new();
    let mut cancel_count: u32 = 0;

    // max_open_orders
    if let Some(limit) = policy.max_open_orders {
        let n = orders.len() as u32;
        if n > limit {
            hits.push(format!("max_open_orders: {} > {}", n, limit));
            if enforce {
                let excess = (n - limit) as usize;
                let victims = newest_first(&orders).into_iter().take(excess).collect::<Vec<_>>();
                cancel_count += cancel_each(client, &victims).await;
            }
        }
    }
    // max_open_notional
    if let Some(s) = &policy.max_open_notional {
        if let Ok(limit) = Decimal::from_str(s) {
            if open_notional > limit {
                hits.push(format!("max_open_notional: {} > {}", open_notional, limit));
                if enforce {
                    let mut shaved = open_notional;
                    let mut victims_picked = Vec::new();
                    for o in newest_first(&orders) {
                        if shaved <= limit { break; }
                        let p = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
                        let q = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
                        shaved -= p * q;
                        victims_picked.push(o);
                    }
                    cancel_count += cancel_each(client, &victims_picked).await;
                }
            }
        }
    }
    // per_market_max_notional
    for (market, cap_str) in &policy.per_market_max_notional {
        let val = per_market.get(market).copied().unwrap_or(Decimal::ZERO);
        if let Ok(cap) = Decimal::from_str(cap_str) {
            if val > cap {
                hits.push(format!("per_market_max_notional[{}]: {} > {}", market, val, cap));
                if enforce {
                    let on_market: Vec<&Order> = newest_first(&orders).into_iter().filter(|o| &o.market_id == market).collect();
                    let mut shaved = val;
                    let mut victims_picked = Vec::new();
                    for o in on_market {
                        if shaved <= cap { break; }
                        let p = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
                        let q = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
                        shaved -= p * q;
                        victims_picked.push(o);
                    }
                    cancel_count += cancel_each(client, &victims_picked).await;
                }
            }
        }
    }
    // pending / failed — observe-only
    if let Some(limit) = policy.max_pending_settlements {
        if pending > limit {
            hits.push(format!("max_pending_settlements: {} > {} (not auto-enforced)", pending, limit));
        }
    }
    if let Some(limit) = policy.max_failed_settlements {
        if failed > limit {
            hits.push(format!("max_failed_settlements: {} > {} (not auto-enforced)", failed, limit));
        }
    }

    let payload = json!({
        "kind": "risk.status",
        "ts": Utc::now().to_rfc3339(),
        "open_orders": orders.len(),
        "open_notional": open_notional.to_string(),
        "per_market_notional": per_market.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_,_>>(),
        "pending_settlements": pending,
        "failed_settlements": failed,
        "hits": hits,
        "enforced_cancellations": cancel_count,
    });
    if !hits.is_empty() {
        warn!("RISK BREACH: {} hits (enforce={}, cancellations={})", hits.len(), enforce, cancel_count);
    }
    sinks.dispatch(payload).await;
    Ok(())
}

fn newest_first(orders: &[Order]) -> Vec<&Order> {
    let mut v: Vec<&Order> = orders.iter().collect();
    v.sort_by(|a, b| b.order_id.cmp(&a.order_id));
    v
}

async fn cancel_each(client: &mut OrderbookClient, victims: &[&Order]) -> u32 {
    let mut n = 0;
    for o in victims {
        match client.cancel_order(o.order_id).await {
            Ok(r) if r.success => {
                info!("cancelled order_id={} (market={})", o.order_id, o.market_id);
                n += 1;
            }
            Ok(r) => warn!("cancel order_id={} !success: {}", o.order_id, r.message),
            Err(e) => warn!("cancel order_id={} error: {:#}", o.order_id, e),
        }
    }
    n
}
