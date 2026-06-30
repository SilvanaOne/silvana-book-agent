//! Legal Compliance Agent — jurisdiction rule evaluator
//!
//! Maps parties to jurisdictions, then applies per-jurisdiction rules to live
//! settlement events. Rules currently supported:
//!
//! - `allowed_markets` — settlement is only legal when ALL parties' jurisdictions
//!   list the market as allowed.
//! - `prohibited_markets` — settlement is illegal if ANY party's jurisdiction
//!   prohibits the market.
//! - `max_notional_per_trade` — any party's jurisdiction can cap a single trade.
//! - `prohibited_counterparty_jurisdictions` — sanction-style block of pair
//!   combinations (e.g. jurisdiction A cannot trade with jurisdiction B).
//!
//! Config TOML:
//!
//! ```toml
//! [party_jurisdictions]
//! "party-x-..." = "US"
//! "party-y-..." = "EU"
//! "party-z-..." = "SG"
//!
//! [jurisdictions.US]
//! allowed_markets = ["CC-USDC", "BTC-USD"]
//! max_notional_per_trade = "1000000"
//! prohibited_counterparty_jurisdictions = ["IR", "KP"]
//!
//! [jurisdictions.EU]
//! prohibited_markets = ["XXX-YYY"]
//! ```
//!
//! Read-only: emits `legal.violation` events to sinks but does not enforce.

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
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{settlement_update::EventType as SettlementEvent, SettlementUpdate};

#[derive(Parser)]
#[command(name = "agent-legal-compliance")]
#[command(about = "Apply jurisdiction-based legal rules to settlement events")]
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

        #[arg(long, default_value = "60")]
        reload_secs: u64,

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
    party_jurisdictions: HashMap<String, String>,
    #[serde(default)]
    jurisdictions: HashMap<String, JurisdictionRules>,
}

#[derive(Deserialize, Default, Clone, Debug)]
struct JurisdictionRules {
    #[serde(default)]
    allowed_markets: Vec<String>,
    #[serde(default)]
    prohibited_markets: Vec<String>,
    #[serde(default)]
    max_notional_per_trade: Option<String>,
    #[serde(default)]
    prohibited_counterparty_jurisdictions: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_legal_compliance", "agent_logic"],
        "agent-legal-compliance",
    );

    match cli.command {
        Commands::Run { policy, reload_secs, stdout, log_file, webhook } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, policy, reload_secs, sinks).await
        }
        Commands::Check { policy } => {
            let p = load_policy(&policy).await?;
            println!(
                "Loaded: {} parties mapped to jurisdictions, {} jurisdictions configured",
                p.party_jurisdictions.len(), p.jurisdictions.len()
            );
            for (j, r) in &p.jurisdictions {
                println!(
                    "  [{}] allowed={:?} prohibited={:?} max={:?} prohibited_pairs={:?}",
                    j, r.allowed_markets, r.prohibited_markets, r.max_notional_per_trade, r.prohibited_counterparty_jurisdictions
                );
            }
            Ok(())
        }
    }
}

async fn load_policy(path: &PathBuf) -> Result<Policy> {
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
    reload_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!("Starting legal-compliance: policy={:?} reload={}s", policy_path, reload_secs);

    let policy = Arc::new(RwLock::new(load_policy(&policy_path).await?));
    let shutdown = Arc::new(Notify::new());
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sd.notify_waiters();
        }
    });

    let p_clone = policy.clone();
    let path_clone = policy_path.clone();
    let sd_reload = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sd_reload.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(reload_secs)) => {
                    match load_policy(&path_clone).await {
                        Ok(p) => { *p_clone.write().await = p; info!("policy reloaded"); }
                        Err(e) => warn!("reload failed: {:#}", e),
                    }
                }
            }
        }
    });

    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_settlements(None).await?;
    info!("[settlements] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => evaluate(&u, &policy, &sinks).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn evaluate(
    u: &SettlementUpdate,
    policy: &Arc<RwLock<Policy>>,
    sinks: &Arc<Sinks>,
) {
    let evt = SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified);
    if !matches!(evt, SettlementEvent::ProposalCreated | SettlementEvent::Settled) {
        return;
    }
    let Some(p) = &u.proposal else { return };
    let pol = policy.read().await.clone();
    let buyer_j = pol.party_jurisdictions.get(&p.buyer).cloned();
    let seller_j = pol.party_jurisdictions.get(&p.seller).cloned();
    let notional = Decimal::from_str(&p.quote_quantity).unwrap_or(Decimal::ZERO);

    let mut hits: Vec<String> = Vec::new();

    let check_side = |juris: &Option<String>, role: &str, hits: &mut Vec<String>| {
        let Some(j) = juris else {
            hits.push(format!("{} party has no jurisdiction mapping", role));
            return;
        };
        let Some(rules) = pol.jurisdictions.get(j) else {
            return; // jurisdiction present but no rules configured — allowed
        };
        if !rules.allowed_markets.is_empty() && !rules.allowed_markets.contains(&p.market_id) {
            hits.push(format!("{} jurisdiction {} does not allow market {}", role, j, p.market_id));
        }
        if rules.prohibited_markets.contains(&p.market_id) {
            hits.push(format!("{} jurisdiction {} prohibits market {}", role, j, p.market_id));
        }
        if let Some(max_s) = &rules.max_notional_per_trade {
            if let Ok(max_d) = Decimal::from_str(max_s) {
                if notional > max_d {
                    hits.push(format!(
                        "{} jurisdiction {} caps trade at {} (notional {})",
                        role, j, max_d, notional
                    ));
                }
            }
        }
    };

    check_side(&buyer_j, "buyer", &mut hits);
    check_side(&seller_j, "seller", &mut hits);

    if let (Some(bj), Some(sj)) = (&buyer_j, &seller_j) {
        if let Some(rules) = pol.jurisdictions.get(bj) {
            if rules.prohibited_counterparty_jurisdictions.contains(sj) {
                hits.push(format!("buyer jurisdiction {} prohibits counterparty jurisdiction {}", bj, sj));
            }
        }
        if let Some(rules) = pol.jurisdictions.get(sj) {
            if rules.prohibited_counterparty_jurisdictions.contains(bj) {
                hits.push(format!("seller jurisdiction {} prohibits counterparty jurisdiction {}", sj, bj));
            }
        }
    }

    if hits.is_empty() {
        return;
    }
    let payload = json!({
        "kind": "legal.violation",
        "ts": Utc::now().to_rfc3339(),
        "proposal_id": p.proposal_id,
        "event_type": format!("{:?}", evt),
        "market_id": p.market_id,
        "buyer": p.buyer,
        "seller": p.seller,
        "buyer_jurisdiction": buyer_j,
        "seller_jurisdiction": seller_j,
        "quote_notional": notional.to_string(),
        "hits": hits,
    });
    warn!(
        "LEGAL VIOLATION on proposal {} ({} hits)",
        p.proposal_id,
        hits.len()
    );
    sinks.dispatch(payload).await;
}
