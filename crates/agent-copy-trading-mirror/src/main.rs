//! Copy Trading Agent — mirror a leader party's order flow
//!
//! Subscribes to a "leader" party's `SubscribeOrders` stream. For every
//! `order.created` the leader emits, mirrors the same side + price at a
//! configurable `--scale` (fraction of the leader's quantity) onto this
//! party's own book. Optional filters gate which markets are mirrored and
//! cap how large a single mirror trade can be.
//!
//! This standalone binary uses a local JSONL log as the leader stream
//! (one order per line, same schema as agent-trading-history's payload)
//! so it can be exercised offline. In a real deployment the input would
//! come from OrderbookService.SubscribeOrders scoped to the leader party.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "agent-copy-trading-mirror")]
#[command(about = "Mirror a leader party's order flow onto this party's book")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read a leader-orders JSONL file, emit mirror records to sinks
    Run {
        /// Path to a JSONL of leader `order.created` events. In a real
        /// deployment this is replaced by SubscribeOrders(leader_party).
        #[arg(long)]
        leader_stream: PathBuf,

        /// Scale factor for mirrored quantity (0..1]
        #[arg(long, default_value = "0.5")]
        scale: f64,

        /// Only mirror orders on these markets (repeat / comma-separated). Empty = all.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        markets: Vec<String>,

        /// Refuse to mirror trades whose leader notional (price × qty) exceeds this cap
        #[arg(long)]
        max_leader_notional: Option<f64>,

        /// Cap the mirrored notional (price × scaled qty) per trade
        #[arg(long)]
        max_mirror_notional: Option<f64>,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,

        /// Just print the mirror plan; don't send anything downstream
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            leader_stream,
            scale,
            markets,
            max_leader_notional,
            max_mirror_notional,
            stdout,
            log_file,
            webhook,
            dry_run,
        } => {
            if !(0.0..=1.0).contains(&scale) || scale == 0.0 {
                anyhow::bail!("--scale must be in (0, 1]");
            }
            let sinks = Sinks::open(webhook, log_file, stdout || dry_run).await?;
            run(
                leader_stream,
                scale,
                markets,
                max_leader_notional,
                max_mirror_notional,
                dry_run,
                sinks,
            )
            .await
        }
    }
}

async fn run(
    leader_stream: PathBuf,
    scale: f64,
    markets: Vec<String>,
    max_leader_notional: Option<f64>,
    max_mirror_notional: Option<f64>,
    dry_run: bool,
    sinks: Arc<Sinks>,
) -> Result<()> {
    let markets_set: HashSet<String> = markets.into_iter().collect();
    info!(
        "copy-trading-mirror: leader={:?} scale={} markets={:?} dry_run={}",
        leader_stream, scale, markets_set, dry_run
    );

    let file = tokio::fs::File::open(&leader_stream)
        .await
        .with_context(|| format!("open {:?}", leader_stream))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut seen: u64 = 0;
    let mut mirrored: u64 = 0;
    let mut skipped: u64 = 0;
    let mut refused: u64 = 0;
    let mut refuse_by_rule: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        if kind != "order.created" {
            skipped += 1;
            continue;
        }
        seen += 1;
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        let market = payload.get("market_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let side = payload.get("side").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let price = number_field(&payload, "price").unwrap_or(0.0);
        let quantity = number_field(&payload, "quantity").unwrap_or(0.0);
        let leader_notional = price * quantity;

        if !markets_set.is_empty() && !markets_set.contains(&market) {
            *refuse_by_rule.entry("market_filter".into()).or_default() += 1;
            refused += 1;
            continue;
        }
        if let Some(cap) = max_leader_notional {
            if leader_notional > cap {
                *refuse_by_rule.entry("max_leader_notional".into()).or_default() += 1;
                refused += 1;
                continue;
            }
        }
        let mirror_qty = quantity * scale;
        let mirror_notional = price * mirror_qty;
        if let Some(cap) = max_mirror_notional {
            if mirror_notional > cap {
                *refuse_by_rule.entry("max_mirror_notional".into()).or_default() += 1;
                refused += 1;
                continue;
            }
        }
        if mirror_qty <= 0.0 || price <= 0.0 {
            *refuse_by_rule.entry("degenerate".into()).or_default() += 1;
            refused += 1;
            continue;
        }

        let record = json!({
            "kind": "copy_trading.mirror",
            "ts": Utc::now().to_rfc3339(),
            "leader_seq": v.get("seq"),
            "leader_ts": v.get("ts"),
            "market_id": market,
            "side": side,
            "price": price,
            "leader_quantity": quantity,
            "mirror_quantity": mirror_qty,
            "leader_notional": leader_notional,
            "mirror_notional": mirror_notional,
            "scale": scale,
            "dry_run": dry_run,
        });
        sinks.dispatch(&record).await;
        mirrored += 1;
    }

    let summary = json!({
        "kind": "copy_trading.summary",
        "leader_orders_seen": seen,
        "mirrored": mirrored,
        "refused": refused,
        "refused_by_rule": refuse_by_rule,
        "skipped_non_order": skipped,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn number_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| match x {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    })
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
        if !stdout && log_file.is_none() && webhook.is_none() {
            anyhow::bail!("provide at least one sink (--stdout / --log-file / --webhook)");
        }
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
    async fn dispatch(&self, payload: &Value) {
        let line = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
        if self.stdout {
            println!("{}", line);
        }
        if let Some(f) = &self.log_file {
            let mut file = f.lock().await;
            let _ = file.write_all(line.as_bytes()).await;
            let _ = file.write_all(b"\n").await;
        }
        if let (Some(url), Some(http)) = (&self.webhook, &self.http) {
            match http.post(url).json(payload).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => warn!("webhook returned {}", r.status()),
                Err(e) => warn!("webhook POST failed: {e}"),
            }
        }
    }
}
