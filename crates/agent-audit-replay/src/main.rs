//! Audit Replay Agent — offline policy back-test on a trading-history log
//!
//! Reads an `agent-trading-history` JSONL file line by line and replays
//! every event through a TOML rule set — the same shape used by
//! `agent-pre-trade-check`, `agent-compliance-screening`, and
//! `agent-risk-management`. Emits per-event verdicts (`accept` / `reject`
//! plus rule hits) so you can answer "how many trades would have been
//! blocked if this new policy had been active since date X?" before
//! rolling it out live.
//!
//! Read-only. Never touches the ledger.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "agent-audit-replay")]
#[command(about = "Back-test a rule policy against a signed trading-history log")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Replay every record and print per-record verdicts + a summary
    Replay {
        #[arg(long)]
        history: PathBuf,

        #[arg(long)]
        rules: PathBuf,

        /// Include accepted records in the output (default: rejects only)
        #[arg(long)]
        emit_accepts: bool,

        /// Optional JSONL output path (default: stdout)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Parse the rules file and print the loaded policy
    Check {
        #[arg(long)]
        rules: PathBuf,
    },
}

#[derive(Deserialize, Default, Debug, Clone)]
struct Rules {
    #[serde(default)]
    max_notional_per_order: Option<String>,
    #[serde(default)]
    max_quantity_per_order: Option<String>,
    #[serde(default)]
    min_price: Option<String>,
    #[serde(default)]
    max_price: Option<String>,
    #[serde(default)]
    blocked_markets: Vec<String>,
    #[serde(default)]
    allowed_markets: Vec<String>,
    #[serde(default)]
    allowed_sides: Vec<String>,
    #[serde(default)]
    blocked_counterparties: Vec<String>,
    /// Per-market daily notional cap. Same key-shape as agent-compliance-screening.
    #[serde(default)]
    per_market_max_daily_notional: HashMap<String, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Commands::Check { rules } => {
            let r = load_rules(&rules).await?;
            println!("{}", serde_json::to_string_pretty(&serde_json::to_value(&r)?)?);
            Ok(())
        }
        Commands::Replay { history, rules, emit_accepts, output } => {
            let r = load_rules(&rules).await?;
            replay(history, r, emit_accepts, output).await
        }
    }
}

impl serde::Serialize for Rules {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = serde_json::Map::new();
        if let Some(v) = &self.max_notional_per_order { m.insert("max_notional_per_order".into(), v.clone().into()); }
        if let Some(v) = &self.max_quantity_per_order { m.insert("max_quantity_per_order".into(), v.clone().into()); }
        if let Some(v) = &self.min_price { m.insert("min_price".into(), v.clone().into()); }
        if let Some(v) = &self.max_price { m.insert("max_price".into(), v.clone().into()); }
        if !self.blocked_markets.is_empty() { m.insert("blocked_markets".into(), serde_json::to_value(&self.blocked_markets).unwrap()); }
        if !self.allowed_markets.is_empty() { m.insert("allowed_markets".into(), serde_json::to_value(&self.allowed_markets).unwrap()); }
        if !self.allowed_sides.is_empty() { m.insert("allowed_sides".into(), serde_json::to_value(&self.allowed_sides).unwrap()); }
        if !self.blocked_counterparties.is_empty() { m.insert("blocked_counterparties".into(), serde_json::to_value(&self.blocked_counterparties).unwrap()); }
        if !self.per_market_max_daily_notional.is_empty() {
            m.insert("per_market_max_daily_notional".into(), serde_json::to_value(&self.per_market_max_daily_notional).unwrap());
        }
        s.collect_map(m)
    }
}

async fn load_rules(path: &PathBuf) -> Result<Rules> {
    let body = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {:?}", path))?;
    toml::from_str(&body).with_context(|| format!("parse {:?}", path))
}

async fn replay(
    history: PathBuf,
    rules: Rules,
    emit_accepts: bool,
    output: Option<PathBuf>,
) -> Result<()> {
    let input = tokio::fs::File::open(&history)
        .await
        .with_context(|| format!("open {:?}", history))?;
    let reader = BufReader::new(input);
    let mut lines = reader.lines();

    let mut out: Option<tokio::fs::File> = match output {
        Some(p) => Some(
            tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&p)
                .await
                .with_context(|| format!("open {:?}", p))?,
        ),
        None => None,
    };

    let mut total = 0u64;
    let mut accepts = 0u64;
    let mut rejects = 0u64;
    let mut per_rule: HashMap<String, u64> = HashMap::new();
    let mut per_market_daily: HashMap<String, f64> = HashMap::new();
    let mut current_day: Option<String> = None;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => { eprintln!("warn: skipping malformed line: {}", e); continue; }
        };
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        // Only order.created and settlement.settled carry enough for policy checks.
        let interesting = kind == "order.created" || kind == "settlement.settled";
        if !interesting { continue; }
        total += 1;

        let ts_day = v
            .get("ts").and_then(|x| x.as_str())
            .map(|s| s.split('T').next().unwrap_or("").to_string())
            .unwrap_or_default();
        if current_day.as_deref() != Some(ts_day.as_str()) {
            per_market_daily.clear();
            current_day = Some(ts_day);
        }

        let hits = evaluate(&v, &rules, &mut per_market_daily);
        let verdict = if hits.is_empty() { "accept" } else { "reject" };
        if verdict == "accept" { accepts += 1; } else { rejects += 1; }
        for h in &hits { *per_rule.entry(h.clone()).or_default() += 1; }
        if verdict == "reject" || emit_accepts {
            let record = json!({
                "kind": "replay.verdict",
                "seq": v.get("seq"),
                "ts": v.get("ts"),
                "event_kind": kind,
                "verdict": verdict,
                "hits": hits,
                "payload": v.get("payload"),
            });
            let line_out = serde_json::to_string(&record)? + "\n";
            if let Some(f) = out.as_mut() {
                f.write_all(line_out.as_bytes()).await?;
            } else {
                print!("{}", line_out);
            }
        }
    }

    if let Some(f) = out.as_mut() { f.flush().await?; }

    let summary = json!({
        "kind": "replay.summary",
        "total": total,
        "accepts": accepts,
        "rejects": rejects,
        "hits_by_rule": per_rule,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn evaluate(v: &Value, r: &Rules, daily: &mut HashMap<String, f64>) -> Vec<String> {
    let mut hits = Vec::new();
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let market = payload.get("market_id").and_then(|x| x.as_str()).unwrap_or("");
    let side = payload.get("side").and_then(|x| x.as_str()).unwrap_or("").to_ascii_lowercase();
    let price = number_field(&payload, "price").unwrap_or(0.0);
    let quantity = number_field(&payload, "quantity").unwrap_or(0.0);
    let notional = number_field(&payload, "settlement_price")
        .zip(number_field(&payload, "base_quantity"))
        .map(|(p, q)| p * q)
        .unwrap_or_else(|| price * quantity);
    let counterparty = payload
        .get("buyer").and_then(|x| x.as_str())
        .or_else(|| payload.get("seller").and_then(|x| x.as_str()))
        .unwrap_or("");

    if !r.blocked_markets.is_empty() && r.blocked_markets.iter().any(|m| m == market) {
        hits.push("blocked_market".into());
    }
    if !r.allowed_markets.is_empty() && !r.allowed_markets.iter().any(|m| m == market) {
        hits.push("market_not_allowed".into());
    }
    if !r.allowed_sides.is_empty() && !side.is_empty() && !r.allowed_sides.iter().any(|s| s.eq_ignore_ascii_case(&side)) {
        hits.push("side_not_allowed".into());
    }
    if let Some(max_notional) = parse_dec(&r.max_notional_per_order) {
        if notional > max_notional { hits.push("max_notional_per_order".into()); }
    }
    if let Some(max_qty) = parse_dec(&r.max_quantity_per_order) {
        if quantity > max_qty { hits.push("max_quantity_per_order".into()); }
    }
    if let Some(min_p) = parse_dec(&r.min_price) {
        if price > 0.0 && price < min_p { hits.push("min_price".into()); }
    }
    if let Some(max_p) = parse_dec(&r.max_price) {
        if price > max_p { hits.push("max_price".into()); }
    }
    if !r.blocked_counterparties.is_empty() && r.blocked_counterparties.iter().any(|p| p == counterparty) {
        hits.push("blocked_counterparty".into());
    }
    if let Some(cap_str) = r.per_market_max_daily_notional.get(market) {
        if let Some(cap) = parse_dec(&Some(cap_str.clone())) {
            let entry = daily.entry(market.to_string()).or_insert(0.0);
            *entry += notional;
            if *entry > cap { hits.push(format!("per_market_max_daily_notional[{}]", market)); }
        }
    }

    hits
}

fn number_field(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| match x {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    })
}

fn parse_dec(s: &Option<String>) -> Option<f64> {
    s.as_deref().and_then(|s| s.parse::<f64>().ok())
}
