//! Anomaly Classifier Agent
//!
//! Consumes the output stream of `agent-audit-anomaly` (JSONL of anomaly
//! records, one per line) and classifies each into one of a small set of
//! trading-abuse buckets:
//!
//!   - `spoofing`         — layer_cluster + rapid_cancel + fill_before_cancel_burst
//!   - `wash_trading`     — repeat market pair with tight fills matching cancels
//!   - `stuck_settlement` — unresolved settlement (pass-through, high-priority)
//!   - `normal_volatility` — anything else
//!
//! Deterministic rule-based classifier here; swap in an ML model behind
//! the same interface in prod.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "agent-anomaly-classifier-rule-clustering")]
#[command(about = "Cluster audit-anomaly records into spoofing / wash_trading / stuck / normal buckets")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read audit-anomaly output and emit classified records
    Classify {
        #[arg(long)]
        input: PathBuf,

        /// Optional output JSONL (default: stdout)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Window (seconds) for co-occurrence checks
        #[arg(long, default_value = "60")]
        window_secs: u64,

        /// Number of anomalies on one market inside the window to escalate to spoofing
        #[arg(long, default_value = "3")]
        cluster_threshold: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Classify { input, output, window_secs, cluster_threshold } => {
            classify(input, output, window_secs, cluster_threshold).await
        }
    }
}

async fn classify(input: PathBuf, output: Option<PathBuf>, window_secs: u64, cluster_threshold: usize) -> Result<()> {
    let f = tokio::fs::File::open(&input).await.with_context(|| format!("open {:?}", input))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();

    let mut out: Option<tokio::fs::File> = match output.as_ref() {
        Some(p) => Some(tokio::fs::OpenOptions::new().create(true).truncate(true).write(true).open(p).await
            .with_context(|| format!("open {:?}", p))?),
        None => None,
    };

    // Sliding window per market for co-occurrence detection.
    let mut window: HashMap<String, VecDeque<(i64, String)>> = HashMap::new();
    let mut by_class: HashMap<String, u64> = HashMap::new();
    let mut total = 0u64;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
        // Filter to actual anomaly records
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        if !kind.starts_with("anomaly.") { continue; }
        total += 1;

        let market = v.get("market").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
        let at_ts = v.get("at_ts").and_then(|x| x.as_str()).unwrap_or("");
        let t = chrono::DateTime::parse_from_rfc3339(at_ts).map(|dt| dt.timestamp()).unwrap_or(0);

        // Push into window + evict old
        let entry = window.entry(market.clone()).or_default();
        entry.push_back((t, kind.to_string()));
        let cutoff = t - window_secs as i64;
        while let Some(front) = entry.front() { if front.0 < cutoff { entry.pop_front(); } else { break; } }

        let bucket = classify_one(kind, entry.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>(), cluster_threshold);
        *by_class.entry(bucket.to_string()).or_default() += 1;

        let out_rec = json!({
            "kind": "anomaly.classified",
            "ts": Utc::now().to_rfc3339(),
            "source_kind": kind,
            "market": market,
            "at_ts": at_ts,
            "class": bucket,
            "window_size": entry.len(),
            "detail": v.get("detail"),
        });
        let line_out = serde_json::to_string(&out_rec)? + "\n";
        if let Some(f) = out.as_mut() { f.write_all(line_out.as_bytes()).await?; } else { print!("{}", line_out); }
    }
    if let Some(f) = out.as_mut() { f.flush().await?; }

    println!("{}", serde_json::to_string_pretty(&json!({
        "kind": "anomaly.classifier.summary",
        "ts": Utc::now().to_rfc3339(),
        "seen": total,
        "by_class": by_class,
    }))?);
    Ok(())
}

/// Return one of: "spoofing" | "wash_trading" | "stuck_settlement" | "normal_volatility"
fn classify_one(kind: &str, window: Vec<&str>, cluster_threshold: usize) -> &'static str {
    if kind == "anomaly.stuck_settlement" { return "stuck_settlement"; }

    let n_layer = window.iter().filter(|k| **k == "anomaly.layer_cluster").count();
    let n_rapid = window.iter().filter(|k| **k == "anomaly.rapid_cancel").count();
    let n_burst = window.iter().filter(|k| **k == "anomaly.fill_before_cancel_burst").count();

    // Spoofing = layering + high cancel churn tightly co-occurring
    if n_layer >= 1 && (n_rapid + n_burst) >= cluster_threshold {
        return "spoofing";
    }
    if n_rapid + n_burst >= cluster_threshold * 2 {
        return "spoofing";
    }

    // Wash-trading heuristic: many events on the same market without price band violations
    if window.len() >= cluster_threshold * 2 && n_layer == 0 {
        return "wash_trading";
    }

    "normal_volatility"
}
