//! Scam Screening Agent — categorized threat-feed screener
//!
//! Extension of `agent-blocked-party` with multi-category lists and an
//! optional HTTP feed source. The agent maintains a set of category buckets;
//! each bucket has a severity (`info` / `warn` / `critical`) and a list of
//! party ids. Settlements involving any listed party trigger an alert tagged
//! with the matched categories.
//!
//! Categories file (TOML):
//! ```toml
//! [[category]]
//! name = "scam_db"
//! severity = "warn"
//! source = { kind = "file", path = "scam.list" }
//!
//! [[category]]
//! name = "sanctions"
//! severity = "critical"
//! source = { kind = "url", url = "https://example.com/sanctions.txt" }
//!
//! [[category]]
//! name = "honeypot"
//! severity = "info"
//! source = { kind = "inline", parties = ["party-a", "party-b"] }
//! ```
//!
//! Each list file/URL is a plain-text list, one party id per line, '#' for
//! comments. Lists refresh every `--refresh-secs`.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
#[command(name = "agent-scam-screening")]
#[command(about = "Categorized threat-feed screener (scam / sanctions / honeypot)")]
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
        categories: PathBuf,

        #[arg(long, default_value = "300")]
        refresh_secs: u64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    Check {
        #[arg(long)]
        categories: PathBuf,
    },
}

#[derive(Deserialize, Default, Clone, Debug)]
struct CategoriesFile {
    #[serde(default, rename = "category")]
    categories: Vec<CategorySpec>,
}

#[derive(Deserialize, Clone, Debug)]
struct CategorySpec {
    name: String,
    #[serde(default = "default_severity")]
    severity: String,
    source: SourceSpec,
}

fn default_severity() -> String {
    "warn".into()
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SourceSpec {
    File { path: PathBuf },
    Url { url: String },
    Inline {
        #[serde(default)]
        parties: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_scam_screening", "agent_logic"],
        "agent-scam-screening",
    );

    match cli.command {
        Commands::Run { categories, refresh_secs, stdout, log_file, webhook } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, categories, refresh_secs, sinks).await
        }
        Commands::Check { categories } => {
            let file = load_categories(&categories).await?;
            let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
            let loaded = load_all(&file, &http).await;
            for (name, entries) in &loaded {
                println!("  [{}] {} entries", name, entries.parties.len());
            }
            Ok(())
        }
    }
}

async fn load_categories(path: &PathBuf) -> Result<CategoriesFile> {
    let body = tokio::fs::read_to_string(path).await.with_context(|| format!("read {:?}", path))?;
    Ok(toml::from_str(&body).with_context(|| format!("parse {:?}", path))?)
}

#[derive(Clone)]
struct LoadedCategory {
    severity: String,
    parties: HashSet<String>,
}

async fn load_all(file: &CategoriesFile, http: &reqwest::Client) -> HashMap<String, LoadedCategory> {
    let mut out: HashMap<String, LoadedCategory> = HashMap::new();
    for c in &file.categories {
        let parties = match &c.source {
            SourceSpec::File { path } => match tokio::fs::read_to_string(path).await {
                Ok(body) => parse_list(&body),
                Err(e) => {
                    warn!("category {}: read file {:?} failed: {:#}", c.name, path, e);
                    HashSet::new()
                }
            },
            SourceSpec::Url { url } => match http.get(url).send().await {
                Ok(resp) if resp.status().is_success() => match resp.text().await {
                    Ok(body) => parse_list(&body),
                    Err(e) => {
                        warn!("category {}: read body failed: {:#}", c.name, e);
                        HashSet::new()
                    }
                },
                Ok(resp) => {
                    warn!("category {}: HTTP {}", c.name, resp.status());
                    HashSet::new()
                }
                Err(e) => {
                    warn!("category {}: GET failed: {:#}", c.name, e);
                    HashSet::new()
                }
            },
            SourceSpec::Inline { parties } => parties.iter().cloned().collect(),
        };
        out.insert(c.name.clone(), LoadedCategory { severity: c.severity.clone(), parties });
    }
    out
}

fn parse_list(s: &str) -> HashSet<String> {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
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
    categories_path: PathBuf,
    refresh_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!("Starting scam-screening: categories={:?} refresh={}s", categories_path, refresh_secs);
    let file = load_categories(&categories_path).await?;
    let fetch_http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let loaded = Arc::new(RwLock::new(load_all(&file, &fetch_http).await));

    let shutdown = Arc::new(Notify::new());
    let sd = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sd.notify_waiters();
        }
    });

    // Refresh loop
    let l_clone = loaded.clone();
    let path_clone = categories_path.clone();
    let sd_refresh = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sd_refresh.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(refresh_secs)) => {
                    match load_categories(&path_clone).await {
                        Ok(file) => {
                            *l_clone.write().await = load_all(&file, &fetch_http).await;
                            info!("categories refreshed");
                        }
                        Err(e) => warn!("refresh failed: {:#}", e),
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
                Some(Ok(u)) => check_event(&u, &loaded, &sinks).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn check_event(
    u: &SettlementUpdate,
    loaded: &Arc<RwLock<HashMap<String, LoadedCategory>>>,
    sinks: &Arc<Sinks>,
) {
    if SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified)
        == SettlementEvent::Unspecified
    {
        return;
    }
    let Some(p) = &u.proposal else { return };
    let lookup = loaded.read().await;
    let mut hits: Vec<Value> = Vec::new();
    for (name, cat) in lookup.iter() {
        let buyer_hit = cat.parties.contains(&p.buyer);
        let seller_hit = cat.parties.contains(&p.seller);
        if !buyer_hit && !seller_hit {
            continue;
        }
        let mut parties: Vec<String> = Vec::new();
        if buyer_hit { parties.push(format!("buyer:{}", p.buyer)); }
        if seller_hit { parties.push(format!("seller:{}", p.seller)); }
        hits.push(json!({
            "category": name,
            "severity": cat.severity,
            "parties": parties,
        }));
    }
    if hits.is_empty() {
        return;
    }
    let max_severity = severity_rank(hits.iter().filter_map(|h| h.get("severity").and_then(|s| s.as_str())));
    let payload = json!({
        "kind": "scam.hit",
        "ts": Utc::now().to_rfc3339(),
        "proposal_id": p.proposal_id,
        "market_id": p.market_id,
        "buyer": p.buyer,
        "seller": p.seller,
        "max_severity": max_severity,
        "hits": hits,
    });
    warn!(
        "SCAM HIT in proposal {} (severity={}, categories={})",
        p.proposal_id, max_severity, hits.len()
    );
    sinks.dispatch(payload).await;
}

fn severity_rank<'a>(it: impl Iterator<Item = &'a str>) -> &'static str {
    let mut critical = false;
    let mut warn_seen = false;
    let mut info_seen = false;
    for s in it {
        match s {
            "critical" => critical = true,
            "warn" => warn_seen = true,
            "info" => info_seen = true,
            _ => {}
        }
    }
    if critical { "critical" }
    else if warn_seen { "warn" }
    else if info_seen { "info" }
    else { "info" }
}
