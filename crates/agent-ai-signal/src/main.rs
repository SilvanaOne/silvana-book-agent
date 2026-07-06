//! AI Signal Agent — LLM-driven trading-signal generator
//!
//! Wraps an LLM in a strict schema: reads a short natural-language prompt
//! (e.g. "I think CC is going to pump within the hour, medium confidence")
//! + a snapshot of the current market context (recent mids + spreads),
//! calls the model, and emits a structured `{market, side, price, quantity,
//! confidence, rationale}` signal to a JSONL file that `agent-signal-bot`
//! consumes.
//!
//! Deterministic mock model in this binary — swap `mock_llm` for a real
//! OpenAI / Anthropic client in production; the record schema is stable.
//! Any signal below `--min-confidence` is dropped.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncWriteExt;

#[derive(Parser)]
#[command(name = "agent-ai-signal")]
#[command(about = "Generate trading signals from a natural-language prompt via an LLM")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Emit one signal from a prompt + market context and exit
    Emit {
        #[arg(long)]
        prompt: String,

        /// Comma-separated markets the model may pick from (BASE-QUOTE)
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        /// Current mids as MARKET=PRICE pairs (one per --market entry)
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        mids: Vec<String>,

        /// Drop signals with confidence below this threshold [0..1]
        #[arg(long, default_value = "0.55")]
        min_confidence: f64,

        /// Model identifier written into the record (for audit)
        #[arg(long, default_value = "silvana-mock-v1")]
        model: String,

        /// Append signal here (compatible with agent-signal-bot --signals-file)
        #[arg(long)]
        signals_file: Option<PathBuf>,

        #[arg(long)]
        stdout: bool,
    },
    /// Loop: read prompts from a file (one per line), emit a signal for each
    Batch {
        #[arg(long)]
        prompts_file: PathBuf,

        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        #[arg(long, value_delimiter = ',', num_args = 1..)]
        mids: Vec<String>,

        #[arg(long, default_value = "0.55")]
        min_confidence: f64,

        #[arg(long, default_value = "silvana-mock-v1")]
        model: String,

        #[arg(long)]
        signals_file: Option<PathBuf>,

        #[arg(long)]
        stdout: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Commands::Emit { prompt, markets, mids, min_confidence, model, signals_file, stdout } => {
            let ctx = parse_market_ctx(&markets, &mids)?;
            let ok = emit_one(&prompt, &ctx, min_confidence, &model, signals_file.as_ref(), stdout).await?;
            if !ok { std::process::exit(2); }
            Ok(())
        }
        Commands::Batch { prompts_file, markets, mids, min_confidence, model, signals_file, stdout } => {
            let ctx = parse_market_ctx(&markets, &mids)?;
            let body = tokio::fs::read_to_string(&prompts_file).await
                .with_context(|| format!("read {:?}", prompts_file))?;
            let mut emitted = 0u64;
            let mut dropped = 0u64;
            for line in body.lines() {
                let s = line.trim();
                if s.is_empty() || s.starts_with('#') { continue; }
                let ok = emit_one(s, &ctx, min_confidence, &model, signals_file.as_ref(), stdout).await?;
                if ok { emitted += 1; } else { dropped += 1; }
            }
            eprintln!("{{\"emitted\":{},\"dropped_below_min_confidence\":{}}}", emitted, dropped);
            Ok(())
        }
    }
}

async fn emit_one(prompt: &str, ctx: &MarketContext, min_conf: f64, model: &str, signals_file: Option<&PathBuf>, stdout: bool) -> Result<bool> {
    let out = mock_llm(prompt, ctx);
    if out.confidence < min_conf {
        return Ok(false);
    }
    let record = json!({
        "kind": "ai.signal",
        "ts": Utc::now().to_rfc3339(),
        "model": model,
        "prompt": prompt,
        "market": out.market,
        "side": out.side,
        "price": out.price,
        "quantity": out.quantity,
        "confidence": out.confidence,
        "rationale": out.rationale,
    });
    let line = serde_json::to_string(&record)? + "\n";
    let signal_line = signal_line_from(&out);
    if stdout { print!("{}", line); }
    if let Some(p) = signals_file {
        let mut f = tokio::fs::OpenOptions::new().create(true).append(true).open(p).await
            .with_context(|| format!("open {:?}", p))?;
        f.write_all(signal_line.as_bytes()).await?;
        f.write_all(b"\n").await?;
    }
    Ok(true)
}

/// Convert an AI signal into the JSONL row `agent-signal-bot` expects.
fn signal_line_from(o: &LlmOut) -> String {
    let ref_id = format!("ai-{}", NEXT_REF.fetch_add(1, Ordering::Relaxed));
    let record = json!({
        "market": o.market,
        "side": o.side,
        "quantity": format!("{}", o.quantity),
        "price": format!("{}", o.price),
        "ref": ref_id,
    });
    serde_json::to_string(&record).unwrap_or_default()
}

static NEXT_REF: AtomicU64 = AtomicU64::new(1);

struct MarketContext {
    entries: Vec<(String, f64)>,   // (market, mid)
}
fn parse_market_ctx(markets: &[String], mids: &[String]) -> Result<MarketContext> {
    if markets.is_empty() { anyhow::bail!("--markets is required"); }
    if mids.len() != markets.len() { anyhow::bail!("--mids must have exactly one entry per --market"); }
    let mut entries = Vec::with_capacity(markets.len());
    for (m, mid_raw) in markets.iter().zip(mids.iter()) {
        let m = m.trim().to_uppercase();
        // Accept "1.02" or "CC-USDC=1.02"
        let price_str = if let Some(eq) = mid_raw.find('=') { &mid_raw[eq+1..] } else { mid_raw.as_str() };
        let mid: f64 = price_str.trim().parse().with_context(|| format!("bad mid for {}: {:?}", m, mid_raw))?;
        entries.push((m, mid));
    }
    Ok(MarketContext { entries })
}

struct LlmOut {
    market: String,
    side: String,      // "BID" or "OFFER"
    price: f64,
    quantity: f64,
    confidence: f64,   // 0..1
    rationale: String,
}

/// Deterministic mock "LLM". Reads keywords from `prompt` to figure out
/// direction, picks a market from `ctx` that matches, and quantifies
/// confidence based on strength cues. Replace with a real API call in prod.
fn mock_llm(prompt: &str, ctx: &MarketContext) -> LlmOut {
    let p = prompt.to_lowercase();

    // Direction cues
    let mut bull = 0i32;
    let mut bear = 0i32;
    for w in ["pump", "up", "moon", "bullish", "buy", "long", "rally", "rip"] { if p.contains(w) { bull += 1; } }
    for w in ["dump", "down", "bearish", "sell", "short", "crash", "drop", "fall"] { if p.contains(w) { bear += 1; } }
    let side = if bear > bull { "OFFER" } else { "BID" };

    // Market pick: first market whose base appears in the prompt, else first
    let mut picked = &ctx.entries[0];
    for e in &ctx.entries {
        let base = e.0.split('-').next().unwrap_or("").to_lowercase();
        if base.len() >= 2 && p.contains(&base) { picked = e; break; }
    }

    // Confidence cues
    let mut conf: f64 = 0.55;
    if p.contains("high confidence") || p.contains("strong") { conf += 0.25; }
    if p.contains("low confidence") || p.contains("uncertain") || p.contains("maybe") { conf -= 0.15; }
    if p.contains("hourly") || p.contains("within the hour") { conf += 0.05; }
    if p.contains("news") || p.contains("event") { conf += 0.05; }
    if bull + bear >= 3 { conf += 0.05; }
    conf = conf.clamp(0.0, 0.99);

    // Quantity heuristic: 0.5..5% of a "budget" of 100
    let quantity = (0.5 + (bull + bear).abs() as f64 * 0.75).min(5.0);

    // Price: mid ± 0.2% for aggressiveness
    let bias = if side == "BID" { 1.002 } else { 0.998 };
    let price = round8(picked.1 * bias);

    let rationale = format!(
        "keywords: bull={} bear={} → side={} · picked market={} · confidence adjusted by strength/hourly/news cues",
        bull, bear, side, picked.0
    );
    LlmOut { market: picked.0.clone(), side: side.to_string(), price, quantity, confidence: (conf * 100.0).round() / 100.0, rationale }
}

fn round8(n: f64) -> f64 { (n * 1e8).round() / 1e8 }
