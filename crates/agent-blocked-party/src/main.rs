//! Blocked Party Detection Agent
//!
//! Watches this party's settlement stream. Each `SettlementProposal` carries
//! a `buyer` and `seller` party id; if either matches a blocklist entry, the
//! agent emits an alert to stdout / JSONL file / HTTP webhook.
//!
//! The blocklist is a plain-text file, one party id per line, lines starting
//! with `#` are comments. The file is re-read every `--reload-secs` so an
//! operator can add entries live without restarting.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{settlement_update::EventType as SettlementEvent, SettlementUpdate};

#[derive(Parser)]
#[command(name = "agent-blocked-party")]
#[command(about = "Flag settlements involving parties on a blocklist")]
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
        /// Path to a plain-text blocklist file (one party id per line; '#' for comments)
        #[arg(long)]
        blocklist: PathBuf,

        /// How often to re-read the blocklist file
        #[arg(long, default_value = "60")]
        reload_secs: u64,

        /// Filter scanning to a single market (optional)
        #[arg(long)]
        market: Option<String>,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,
    },
    /// Verify a blocklist file parses and print its loaded entries
    Check {
        #[arg(long)]
        blocklist: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_blocked_party", "orderbook_agent_logic"],
        "agent-blocked-party",
    );

    match cli.command {
        Commands::Run {
            blocklist,
            reload_secs,
            market,
            stdout,
            log_file,
            webhook,
        } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink (--stdout / --log-file / --webhook)");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run(config, blocklist, reload_secs, market, sinks).await
        }
        Commands::Check { blocklist } => {
            let entries = load_blocklist(&blocklist).await?;
            println!("Loaded {} block entries from {:?}:", entries.len(), blocklist);
            for e in &entries {
                println!("  {}", e);
            }
            Ok(())
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

async fn load_blocklist(path: &PathBuf) -> Result<HashSet<String>> {
    let body = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {:?}", path))?;
    let entries: HashSet<String> = body
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();
    Ok(entries)
}

async fn run(
    config: BaseConfig,
    blocklist_path: PathBuf,
    reload_secs: u64,
    market: Option<String>,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!(
        "Starting blocked-party: blocklist={:?} reload={}s market={:?}",
        blocklist_path, reload_secs, market
    );

    let blocklist = Arc::new(RwLock::new(load_blocklist(&blocklist_path).await?));
    info!("loaded {} initial entries", blocklist.read().await.len());

    let shutdown = Arc::new(Notify::new());
    let sd_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            sd_signal.notify_waiters();
        }
    });

    // Background reload loop
    let bl_clone = blocklist.clone();
    let path_clone = blocklist_path.clone();
    let sd_reload = shutdown.clone();
    tokio::spawn(async move {
        let mut last = Instant::now();
        loop {
            tokio::select! {
                _ = sd_reload.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(reload_secs)) => {
                    if last.elapsed().as_secs() < reload_secs { continue; }
                    last = Instant::now();
                    match load_blocklist(&path_clone).await {
                        Ok(set) => {
                            let n = set.len();
                            *bl_clone.write().await = set;
                            info!("blocklist reloaded ({} entries)", n);
                        }
                        Err(e) => warn!("blocklist reload failed: {:#}", e),
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
                Some(Ok(u)) => check_event(&u, &blocklist, &sinks).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn check_event(
    u: &SettlementUpdate,
    blocklist: &Arc<RwLock<HashSet<String>>>,
    sinks: &Arc<Sinks>,
) {
    // Only act on substantive events (skip Unspecified)
    if SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified)
        == SettlementEvent::Unspecified
    {
        return;
    }
    let Some(p) = &u.proposal else { return };
    let blocked = blocklist.read().await;
    let buyer_hit = blocked.contains(&p.buyer);
    let seller_hit = blocked.contains(&p.seller);
    if !buyer_hit && !seller_hit {
        return;
    }
    let mut hits = Vec::new();
    if buyer_hit {
        hits.push(("buyer", p.buyer.clone()));
    }
    if seller_hit {
        hits.push(("seller", p.seller.clone()));
    }

    let payload = json!({
        "kind": "compliance.blocked_party_hit",
        "ts": Utc::now().to_rfc3339(),
        "proposal_id": p.proposal_id,
        "market_id": p.market_id,
        "buyer": p.buyer,
        "seller": p.seller,
        "settlement_price": p.settlement_price,
        "base_quantity": p.base_quantity,
        "status": p.status,
        "hits": hits.iter().map(|(role, party)| json!({ "role": role, "party": party })).collect::<Vec<_>>(),
    });
    warn!(
        "BLOCKED PARTY HIT in proposal {} (market {}): {:?}",
        p.proposal_id, p.market_id, hits
    );
    sinks.dispatch(payload).await;
}
