//! Compliance Screening Agent — rule-engine for settlement flows
//!
//! Generalization of `agent-blocked-party`. Subscribes to the settlement
//! stream and evaluates each proposal against a TOML policy file:
//!
//! ```toml
//! # Block a specific counterparty pair (either direction)
//! [[blocked_pairs]]
//! a = "party-a-..."
//! b = "party-b-..."
//!
//! # Per-counterparty rolling daily notional cap (quote currency)
//! [[party_caps]]
//! party = "party-x-..."
//! window_hours = 24
//! max_notional = "100000"
//!
//! # Restrict who we'll settle with at all (whitelist; empty = no restriction)
//! [global]
//! allowed_counterparties = []
//! blocked_markets = ["XXX-YYY"]
//! ```
//!
//! For each settlement: emits one event `compliance.accept` or
//! `compliance.reject` with a list of rule hits, to stdout / JSONL file /
//! webhook. Does **not** cancel anything — pair with an enforcement agent
//! (`agent-killswitch`, `agent-witnesses`) if you want action.

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

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{settlement_update::EventType as SettlementEvent, SettlementUpdate};

#[derive(Parser)]
#[command(name = "agent-compliance-screening")]
#[command(about = "Apply a rule-engine to live settlement events")]
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
        market: Option<String>,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,

        /// Also emit `compliance.accept` events (default: only emit rejects)
        #[arg(long)]
        emit_accepts: bool,
    },
    /// Validate policy file structure and print the loaded rules
    Check {
        #[arg(long)]
        policy: PathBuf,
    },
}

#[derive(Deserialize, Default, Clone)]
struct Policy {
    #[serde(default)]
    blocked_pairs: Vec<BlockedPair>,
    #[serde(default)]
    party_caps: Vec<PartyCap>,
    #[serde(default)]
    global: GlobalRules,
}

#[derive(Deserialize, Clone)]
struct BlockedPair {
    a: String,
    b: String,
}

#[derive(Deserialize, Clone)]
struct PartyCap {
    party: String,
    window_hours: u32,
    max_notional: String,
}

#[derive(Deserialize, Default, Clone)]
struct GlobalRules {
    #[serde(default)]
    allowed_counterparties: Vec<String>,
    #[serde(default)]
    blocked_markets: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_compliance_screening", "agent_logic"],
        "agent-compliance-screening",
    );

    match cli.command {
        Commands::Run {
            policy,
            reload_secs,
            market,
            stdout,
            log_file,
            webhook,
            emit_accepts,
        } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, policy, reload_secs, market, sinks, emit_accepts).await
        }
        Commands::Check { policy } => {
            let p = load_policy(&policy).await?;
            println!(
                "Loaded: {} blocked_pairs, {} party_caps, {} allowed_counterparties, {} blocked_markets",
                p.blocked_pairs.len(),
                p.party_caps.len(),
                p.global.allowed_counterparties.len(),
                p.global.blocked_markets.len()
            );
            for bp in &p.blocked_pairs {
                println!("  blocked_pair: {} <-> {}", bp.a, bp.b);
            }
            for c in &p.party_caps {
                println!("  cap: {} max={} window={}h", c.party, c.max_notional, c.window_hours);
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

#[derive(Clone)]
struct DailyTracker {
    samples: HashMap<String, VecDeque<(DateTime<Utc>, Decimal)>>,
}
impl DailyTracker {
    fn new() -> Self {
        Self { samples: HashMap::new() }
    }
    fn record(&mut self, party: &str, notional: Decimal, when: DateTime<Utc>) {
        self.samples
            .entry(party.to_string())
            .or_default()
            .push_back((when, notional));
    }
    fn total_within(&mut self, party: &str, window_hours: u32) -> Decimal {
        let cutoff = Utc::now() - chrono::Duration::hours(window_hours as i64);
        let buf = match self.samples.get_mut(party) {
            Some(b) => b,
            None => return Decimal::ZERO,
        };
        while let Some(front) = buf.front() {
            if front.0 < cutoff {
                buf.pop_front();
            } else {
                break;
            }
        }
        buf.iter().fold(Decimal::ZERO, |acc, (_, n)| acc + *n)
    }
}

async fn run(
    config: BaseConfig,
    policy_path: PathBuf,
    reload_secs: u64,
    market: Option<String>,
    sinks: Arc<Sinks>,
    emit_accepts: bool,
) -> Result<()> {
    info!(
        "Starting compliance-screening: policy={:?} reload={}s market={:?} emit_accepts={}",
        policy_path, reload_secs, market, emit_accepts
    );

    let policy = Arc::new(RwLock::new(load_policy(&policy_path).await?));
    let tracker = Arc::new(Mutex::new(DailyTracker::new()));

    let shutdown = Arc::new(Notify::new());
    let sd_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sd_signal.notify_waiters();
        }
    });

    // Policy reload loop
    let pol_clone = policy.clone();
    let path_clone = policy_path.clone();
    let sd_reload = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sd_reload.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(reload_secs)) => {
                    match load_policy(&path_clone).await {
                        Ok(p) => {
                            *pol_clone.write().await = p;
                            info!("policy reloaded");
                        }
                        Err(e) => warn!("policy reload failed: {:#}", e),
                    }
                }
            }
        }
    });

    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_settlements(market).await?;
    info!("[settlements] stream opened");

    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => evaluate_event(&u, &policy, &tracker, &sinks, emit_accepts).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn evaluate_event(
    u: &SettlementUpdate,
    policy: &Arc<RwLock<Policy>>,
    tracker: &Arc<Mutex<DailyTracker>>,
    sinks: &Arc<Sinks>,
    emit_accepts: bool,
) {
    // Only consider terminal events.
    let evt = SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified);
    if !matches!(
        evt,
        SettlementEvent::ProposalCreated | SettlementEvent::Settled
    ) {
        return;
    }
    let Some(p) = &u.proposal else { return };
    let pol = policy.read().await.clone();
    let notional = Decimal::from_str(&p.quote_quantity).unwrap_or(Decimal::ZERO);
    let mut hits: Vec<String> = Vec::new();

    // Pair block
    for bp in &pol.blocked_pairs {
        let pair_a_b = (p.buyer == bp.a && p.seller == bp.b) || (p.buyer == bp.b && p.seller == bp.a);
        if pair_a_b {
            hits.push(format!("blocked_pair: {} <-> {}", bp.a, bp.b));
        }
    }

    // Whitelist
    if !pol.global.allowed_counterparties.is_empty() {
        if !pol.global.allowed_counterparties.contains(&p.buyer) {
            hits.push(format!("buyer {} not in allowed_counterparties", p.buyer));
        }
        if !pol.global.allowed_counterparties.contains(&p.seller) {
            hits.push(format!("seller {} not in allowed_counterparties", p.seller));
        }
    }

    // Blocked markets
    if pol.global.blocked_markets.contains(&p.market_id) {
        hits.push(format!("market {} is blocked", p.market_id));
    }

    // Party caps: applied per cap entry; checks both buyer and seller if they match
    {
        let mut t = tracker.lock().await;
        if evt == SettlementEvent::Settled {
            // record only on confirmed settlements so caps reflect realized notional
            t.record(&p.buyer, notional, Utc::now());
            t.record(&p.seller, notional, Utc::now());
        }
        for cap in &pol.party_caps {
            let limit = Decimal::from_str(&cap.max_notional).unwrap_or(Decimal::ZERO);
            for party in [&p.buyer, &p.seller] {
                if party == &cap.party {
                    let used = t.total_within(party, cap.window_hours);
                    if used > limit {
                        hits.push(format!(
                            "party_cap exceeded for {}: used {} > max {} ({}h window)",
                            party, used, limit, cap.window_hours
                        ));
                    }
                }
            }
        }
    }

    let accepted = hits.is_empty();
    if accepted && !emit_accepts {
        return;
    }
    let payload = json!({
        "kind": if accepted { "compliance.accept" } else { "compliance.reject" },
        "ts": Utc::now().to_rfc3339(),
        "proposal_id": p.proposal_id,
        "event_type": format!("{:?}", evt),
        "market_id": p.market_id,
        "buyer": p.buyer,
        "seller": p.seller,
        "base_quantity": p.base_quantity,
        "settlement_price": p.settlement_price,
        "quote_notional": notional.to_string(),
        "hits": hits,
    });
    if !accepted {
        warn!(
            "REJECT proposal {} ({} hits)",
            p.proposal_id,
            hits.len()
        );
    }
    sinks.dispatch(payload).await;
}
