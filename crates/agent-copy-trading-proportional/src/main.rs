//! Copy Trading Agent — proportional to portfolio ratio
//!
//! Sibling of `agent-copy-trading-mirror`. Instead of a fixed scale factor,
//! this variant sizes every mirror trade proportional to the follower's own
//! portfolio relative to the leader's:
//!
//!     mirror_qty = leader_qty × (follower_portfolio / leader_portfolio)
//!
//! Where `follower_portfolio` and `leader_portfolio` are quote-currency
//! valuations supplied on the CLI. Additional safety knobs cap per-trade
//! notional and refuse degenerate inputs.
//!
//! Like `mirror`, the leader stream is read from a local JSONL file (one
//! `order.created` event per line, same shape as `agent-trading-history`'s
//! payload). In production the input would be `OrderbookService.SubscribeOrders`
//! scoped to the leader party.

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
#[command(name = "agent-copy-trading-proportional")]
#[command(about = "Mirror a leader party's flow, sized by portfolio ratio")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Consume a leader JSONL stream and emit portfolio-ratio-sized mirror records
    Run {
        /// Path to leader `order.created` JSONL stream
        #[arg(long)]
        leader_stream: PathBuf,

        /// Follower portfolio value (quote-currency units)
        #[arg(long)]
        follower_portfolio: f64,

        /// Leader portfolio value (quote-currency units)
        #[arg(long)]
        leader_portfolio: f64,

        /// Absolute cap on mirror scale factor (default 1.0 — do not exceed leader size)
        #[arg(long, default_value = "1.0")]
        max_scale: f64,

        /// Only mirror orders on these markets (repeat / comma-separated). Empty = all.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        markets: Vec<String>,

        /// Refuse to mirror trades whose leader notional (price × qty) exceeds this cap
        #[arg(long)]
        max_leader_notional: Option<f64>,

        /// Cap the mirrored notional per trade
        #[arg(long)]
        max_mirror_notional: Option<f64>,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,

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
            follower_portfolio,
            leader_portfolio,
            max_scale,
            markets,
            max_leader_notional,
            max_mirror_notional,
            stdout,
            log_file,
            webhook,
            dry_run,
        } => {
            if follower_portfolio <= 0.0 {
                anyhow::bail!("--follower-portfolio must be > 0");
            }
            if leader_portfolio <= 0.0 {
                anyhow::bail!("--leader-portfolio must be > 0");
            }
            if max_scale <= 0.0 {
                anyhow::bail!("--max-scale must be > 0");
            }
            let sinks = Sinks::open(webhook, log_file, stdout || dry_run).await?;
            run(
                leader_stream,
                follower_portfolio,
                leader_portfolio,
                max_scale,
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

#[allow(clippy::too_many_arguments)]
async fn run(
    leader_stream: PathBuf,
    follower_portfolio: f64,
    leader_portfolio: f64,
    max_scale: f64,
    markets: Vec<String>,
    max_leader_notional: Option<f64>,
    max_mirror_notional: Option<f64>,
    dry_run: bool,
    sinks: Arc<Sinks>,
) -> Result<()> {
    let markets_set: HashSet<String> = markets.into_iter().collect();
    let raw_scale = follower_portfolio / leader_portfolio;
    let scale = raw_scale.min(max_scale);

    info!(
        "copy-trading-proportional: leader={:?} follower_port={} leader_port={} raw_scale={:.6} effective_scale={:.6} markets={:?} dry_run={}",
        leader_stream, follower_portfolio, leader_portfolio, raw_scale, scale, markets_set, dry_run
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
    let mut refuse_by_rule: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();

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
        let market = payload
            .get("market_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let side = payload
            .get("side")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
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
                *refuse_by_rule
                    .entry("max_leader_notional".into())
                    .or_default() += 1;
                refused += 1;
                continue;
            }
        }
        let mirror_qty = quantity * scale;
        let mirror_notional = price * mirror_qty;
        if let Some(cap) = max_mirror_notional {
            if mirror_notional > cap {
                *refuse_by_rule
                    .entry("max_mirror_notional".into())
                    .or_default() += 1;
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
            "kind": "copy_trading.proportional_mirror",
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
            "raw_scale": raw_scale,
            "max_scale": max_scale,
            "follower_portfolio": follower_portfolio,
            "leader_portfolio": leader_portfolio,
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
        "effective_scale": scale,
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
            Some(
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?,
            )
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
        Ok(Arc::new(Self {
            webhook,
            http,
            log_file: file,
            stdout,
        }))
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
