//! Audit Anomaly Agent — post-hoc statistical scan over trading-history
//!
//! Offline reader over an `agent-trading-history` JSONL file. Applies a
//! handful of heuristics that catch patterns worth reviewing after the
//! fact, without needing a live orderbook:
//!
//! - **stuck_settlement** — a filled order whose associated proposal never
//!   settled inside `--settlement-window-secs`.
//! - **rapid_cancel** — an `order.created` and its `order.cancelled` land
//!   within `--rapid-cancel-window-ms` (spoofing indicator).
//! - **layer_cluster** — `--layer-threshold` open same-side orders on one
//!   market clustered within `--layer-band-pct` price band (layering
//!   indicator).
//! - **fill_before_cancel_burst** — a burst of `--burst-count` cancels
//!   inside `--burst-window-secs` right after a fill (churn indicator).
//!
//! Distinct from live `agent-market-abuse`: works on any historical log,
//! any depth of history, without a stream connection.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "agent-audit-anomaly")]
#[command(about = "Scan a trading-history log for statistical anomalies")]
struct Cli {
    #[arg(long)]
    history: PathBuf,

    /// A fill without a corresponding settled proposal within this many seconds is flagged
    #[arg(long, default_value = "3600")]
    settlement_window_secs: i64,

    /// A create followed by a cancel within this many ms → rapid_cancel
    #[arg(long, default_value = "1000")]
    rapid_cancel_window_ms: i64,

    /// Threshold for a layer_cluster hit
    #[arg(long, default_value = "5")]
    layer_threshold: usize,

    /// Price-band (as %) that groups orders into a layer cluster
    #[arg(long, default_value = "1.0")]
    layer_band_pct: f64,

    /// Count of cancels required inside --burst-window-secs to flag a burst
    #[arg(long, default_value = "5")]
    burst_count: usize,

    /// Window for the fill-before-cancel-burst heuristic
    #[arg(long, default_value = "10")]
    burst_window_secs: i64,

    /// Optional JSONL output path (default: stdout)
    #[arg(long)]
    output: Option<PathBuf>,
}

struct OrderMeta {
    seq: u64,
    ts: DateTime<Utc>,
    market: String,
    side: String,
    price: f64,
}

struct FillMeta {
    seq: u64,
    ts: DateTime<Utc>,
    proposal_id: String,
    market: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let f = tokio::fs::File::open(&cli.history).await.with_context(|| format!("open {:?}", cli.history))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();

    let mut out: Option<tokio::fs::File> = match cli.output.as_ref() {
        Some(p) => Some(
            tokio::fs::OpenOptions::new()
                .create(true).truncate(true).write(true).open(p).await
                .with_context(|| format!("open {:?}", p))?,
        ),
        None => None,
    };

    // Per-order-id metadata for create/cancel matching.
    let mut orders: HashMap<u64, OrderMeta> = HashMap::new();
    // Fills by proposal_id, awaiting settled/failed.
    let mut fills: HashMap<String, FillMeta> = HashMap::new();
    // Cancel timestamps per market (for burst detection).
    let mut cancel_history: HashMap<String, Vec<DateTime<Utc>>> = HashMap::new();
    // Last fill ts per market.
    let mut last_fill: HashMap<String, DateTime<Utc>> = HashMap::new();
    // Open same-side orders per market, keyed by (market, side).
    let mut open_orders: HashMap<(String, String), Vec<f64>> = HashMap::new();

    let mut anomalies: Vec<Value> = Vec::new();
    let mut total: u64 = 0;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        total += 1;
        let seq = v.get("seq").and_then(|x| x.as_u64()).unwrap_or(0);
        let ts_str = v.get("ts").and_then(|x| x.as_str()).unwrap_or("");
        let ts = match DateTime::parse_from_rfc3339(ts_str) {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => continue,
        };
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        let market = payload.get("market_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let side = payload.get("side").and_then(|x| x.as_str()).unwrap_or("").to_ascii_uppercase();
        let price = number_field(&payload, "price").unwrap_or(0.0);
        let counterparty = payload.get("counterparty").and_then(|x| x.as_str()).unwrap_or("").to_string();

        match kind {
            "order.created" => {
                let order_id = payload.get("order_id").and_then(|x| x.as_u64());
                if let Some(oid) = order_id {
                    orders.insert(oid, OrderMeta { seq, ts, market: market.clone(), side: side.clone(), price });
                }
                let _ = &counterparty; // reserved for future counterparty-aware heuristics
                if !market.is_empty() && !side.is_empty() && price > 0.0 {
                    open_orders.entry((market.clone(), side.clone())).or_default().push(price);
                    // Layer cluster check.
                    let arr = open_orders.get(&(market.clone(), side.clone())).unwrap();
                    if arr.len() >= cli.layer_threshold {
                        let mean = arr.iter().copied().sum::<f64>() / arr.len() as f64;
                        let band = mean * cli.layer_band_pct / 100.0;
                        let in_band = arr.iter().filter(|p| (**p - mean).abs() <= band).count();
                        if in_band >= cli.layer_threshold {
                            anomalies.push(json!({
                                "kind": "anomaly.layer_cluster",
                                "at_seq": seq, "at_ts": ts_str,
                                "market": market, "side": side,
                                "count": in_band, "band_pct": cli.layer_band_pct,
                                "mean_price": mean,
                            }));
                        }
                    }
                }
            }
            "order.cancelled" => {
                if let Some(oid) = payload.get("order_id").and_then(|x| x.as_u64()) {
                    if let Some(o) = orders.remove(&oid) {
                        let dt = (ts - o.ts).num_milliseconds();
                        if dt >= 0 && dt < cli.rapid_cancel_window_ms {
                            anomalies.push(json!({
                                "kind": "anomaly.rapid_cancel",
                                "at_seq": seq, "at_ts": ts_str,
                                "market": o.market, "side": o.side,
                                "create_seq": o.seq, "elapsed_ms": dt,
                            }));
                        }
                        // remove from open_orders
                        if let Some(arr) = open_orders.get_mut(&(o.market.clone(), o.side)) {
                            if let Some(pos) = arr.iter().position(|p| (*p - o.price).abs() < 1e-9) {
                                arr.remove(pos);
                            }
                        }
                    }
                }
                if !market.is_empty() {
                    let entry = cancel_history.entry(market.clone()).or_default();
                    entry.push(ts);
                    entry.retain(|t| (ts - *t).num_seconds() <= cli.burst_window_secs);
                    // Fill-before-cancel-burst
                    if entry.len() >= cli.burst_count {
                        if let Some(last) = last_fill.get(&market) {
                            let delta = (entry[0] - *last).num_seconds();
                            if delta >= 0 && delta <= cli.burst_window_secs {
                                anomalies.push(json!({
                                    "kind": "anomaly.fill_before_cancel_burst",
                                    "at_seq": seq, "at_ts": ts_str,
                                    "market": market, "cancel_count": entry.len(),
                                    "seconds_after_fill": delta,
                                }));
                            }
                        }
                    }
                }
            }
            "order.filled" => {
                if !market.is_empty() {
                    last_fill.insert(market.clone(), ts);
                }
                if let Some(pid) = payload.get("proposal_id").and_then(|x| x.as_str()) {
                    fills.insert(pid.to_string(), FillMeta { seq, ts, proposal_id: pid.to_string(), market });
                }
            }
            "settlement.settled" | "settlement.failed" => {
                if let Some(pid) = payload.get("proposal_id").and_then(|x| x.as_str()) {
                    fills.remove(pid);
                }
            }
            _ => {}
        }
    }

    // Post-scan: any fills that never settled inside the window count as stuck.
    let now_tail = fills.values().map(|f| f.ts).max().unwrap_or_else(Utc::now);
    for (_pid, f) in &fills {
        let age = (now_tail - f.ts).num_seconds();
        if age >= cli.settlement_window_secs {
            anomalies.push(json!({
                "kind": "anomaly.stuck_settlement",
                "at_seq": f.seq, "at_ts": f.ts.to_rfc3339(),
                "market": f.market, "proposal_id": f.proposal_id,
                "age_secs": age,
            }));
        }
    }

    // Emit findings.
    for a in &anomalies {
        let line = serde_json::to_string(a)? + "\n";
        if let Some(o) = out.as_mut() { o.write_all(line.as_bytes()).await?; }
        else { print!("{}", line); }
    }
    if let Some(o) = out.as_mut() { o.flush().await?; }

    let summary = json!({
        "kind": "anomaly.summary",
        "records_scanned": total,
        "anomalies_found": anomalies.len(),
        "by_kind": anomalies.iter().fold(HashMap::<String, u64>::new(), |mut acc, v| {
            let k = v.get("kind").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
            *acc.entry(k).or_default() += 1;
            acc
        }),
    });
    eprintln!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn number_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| match x {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    })
}
