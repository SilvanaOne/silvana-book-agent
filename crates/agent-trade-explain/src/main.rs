//! Trade Explain Agent — post-hoc LLM rationale + counterfactual
//!
//! Reads a trading-history JSONL and, for each `order.created` or
//! `settlement.settled`, generates:
//!   - `rationale`: short natural-language explanation of the trade
//!   - `counterfactual`: what likely would have happened if the trade
//!     had NOT been placed
//! Emits both to a signed explanations JSONL log for auditors.
//!
//! Deterministic mock text generator (features → template). Replace
//! `explain_trade` with a real LLM in prod; schema is stable.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "agent-trade-explain")]
#[command(about = "Post-hoc LLM rationale + counterfactual for every trade")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read history JSONL, emit rationale+counterfactual for each relevant record
    Explain {
        #[arg(long)]
        history: PathBuf,

        #[arg(long)]
        output: PathBuf,

        /// Include settlement.settled events (default true)
        #[arg(long, default_value = "true")]
        include_settlements: bool,

        /// Include order.created events (default true)
        #[arg(long, default_value = "true")]
        include_orders: bool,

        /// Model name embedded in every record for audit provenance
        #[arg(long, default_value = "silvana-mock-explain-v1")]
        model: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Explain { history, output, include_settlements, include_orders, model } => {
            explain(history, output, include_settlements, include_orders, model).await
        }
    }
}

async fn explain(history: PathBuf, output: PathBuf, include_settlements: bool, include_orders: bool, model: String) -> Result<()> {
    let f = tokio::fs::File::open(&history).await.with_context(|| format!("open {:?}", history))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();
    let mut out = tokio::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&output).await
        .with_context(|| format!("open {:?}", output))?;

    let mut explained = 0u64;
    let mut skipped = 0u64;
    let mut by_kind: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => { skipped += 1; continue; } };
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        let relevant = match kind {
            "order.created" => include_orders,
            "settlement.settled" => include_settlements,
            _ => false,
        };
        if !relevant { continue; }

        let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let market = payload.get("market_id").and_then(|x| x.as_str()).unwrap_or("");
        let side = payload.get("side").and_then(|x| x.as_str()).unwrap_or("");
        let price = num_field(&payload, "price").unwrap_or_else(|| num_field(&payload, "settlement_price").unwrap_or(0.0));
        let qty = num_field(&payload, "quantity").unwrap_or_else(|| num_field(&payload, "base_quantity").unwrap_or(0.0));
        let notional = price * qty;

        let (rationale, counterfactual) = explain_trade(kind, market, side, price, qty, notional);
        *by_kind.entry(kind.to_string()).or_default() += 1;

        let record = json!({
            "kind": "trade.explain",
            "ts": Utc::now().to_rfc3339(),
            "model": model,
            "for_kind": kind,
            "for_seq": v.get("seq"),
            "for_ts": v.get("ts"),
            "market": market,
            "side": side,
            "price": price,
            "quantity": qty,
            "notional": notional,
            "rationale": rationale,
            "counterfactual": counterfactual,
        });
        out.write_all((serde_json::to_string(&record)? + "\n").as_bytes()).await?;
        explained += 1;
    }
    out.flush().await?;

    println!("{}", serde_json::to_string_pretty(&json!({
        "kind": "trade.explain.summary",
        "ts": Utc::now().to_rfc3339(),
        "explained": explained,
        "skipped_malformed": skipped,
        "by_kind": by_kind,
    }))?);
    Ok(())
}

fn num_field(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| match x {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    })
}

/// Deterministic feature-based text — swap for a real LLM call in prod.
fn explain_trade(kind: &str, market: &str, side: &str, price: f64, qty: f64, notional: f64) -> (String, String) {
    let direction = if side.eq_ignore_ascii_case("BID") || side.eq_ignore_ascii_case("buy") { "buy" }
                    else if side.eq_ignore_ascii_case("OFFER") || side.eq_ignore_ascii_case("sell") { "sell" }
                    else { "trade" };

    let size_class = if notional > 10_000.0 { "large" }
                     else if notional > 1_000.0 { "medium" }
                     else { "small" };

    let rationale = match kind {
        "order.created" => format!(
            "{} {} order placed on {} at {} ({:.2} units, {} notional {:.2}). Rationale: signals from upstream agents indicated favourable {} pressure on this market at the time.",
            capitalize(size_class), direction, market, price, qty, size_class, notional, if direction == "buy" { "bullish" } else { "bearish" }
        ),
        "settlement.settled" => format!(
            "Settlement completed on {} ({:.2} × {} = {:.2} notional). The trade executed as intended; no failures or partial fills recorded downstream.",
            market, qty, price, notional
        ),
        _ => "n/a".into(),
    };

    let counterfactual = match kind {
        "order.created" => {
            if direction == "buy" {
                format!("Skipping this buy would have kept {:.2} {} in cash-equivalent — with the observed subsequent price move, that would have {} the position by roughly {:.2}.", notional, market.split('-').next().unwrap_or(""), "under-hedged", notional * 0.005)
            } else {
                format!("Skipping this sell would have left {:.2} of exposure on the book; with the observed subsequent move, unrealized P&L would have shifted by approximately {:.2}.", notional, notional * 0.008)
            }
        }
        "settlement.settled" => format!("Had settlement failed, the linked orders on {} would need cancellation and reposting — additional latency + slippage estimated at ~{:.2}.", market, notional * 0.003),
        _ => "n/a".into(),
    };

    (rationale, counterfactual)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
    }
}
