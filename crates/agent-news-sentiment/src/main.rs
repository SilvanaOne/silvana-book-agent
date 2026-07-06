//! News Sentiment Agent
//!
//! Consumes a stream of news headlines (from a JSONL file — one headline
//! per line, `{ ts, headline, source? }`), scores each headline in the
//! range `[-1..+1]` on a simple keyword lexicon, extracts market mentions
//! from a `market → aliases` map, and emits a signal for headlines whose
//! `|sentiment| > threshold` AND whose text mentions at least one
//! configured market.
//!
//! The lexicon is deliberately basic (positive/negative word lists) so
//! that this binary is dependency-free and deterministic. Swap in a real
//! model (Vader, RoBERTa fine-tune) in production behind the same schema.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Parser)]
#[command(name = "agent-news-sentiment")]
#[command(about = "Score news headlines for sentiment and emit market-tagged signals")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Process every line of `--news-file` and emit signals that pass the threshold
    Scan {
        #[arg(long)]
        news_file: PathBuf,

        /// Only emit if |sentiment| exceeds this (0..1)
        #[arg(long, default_value = "0.35")]
        threshold: f64,

        /// market=alias1,alias2 pairs (repeat). Example:
        /// --market-alias "CC-USDC=canton,cc" --market-alias "BTC-USD=bitcoin,btc"
        #[arg(long)]
        market_alias: Vec<String>,

        /// Append matched signals here (compatible with agent-signal-bot)
        #[arg(long)]
        signals_file: Option<PathBuf>,

        /// Also print each classified headline (not just matched ones)
        #[arg(long)]
        stdout: bool,

        /// Quantity used for the emitted signal per hit (constant here)
        #[arg(long, default_value = "1.0")]
        quantity: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan { news_file, threshold, market_alias, signals_file, stdout, quantity } => {
            if threshold < 0.0 || threshold > 1.0 { anyhow::bail!("--threshold must be in [0, 1]"); }
            if quantity <= 0.0 { anyhow::bail!("--quantity must be > 0"); }
            let aliases = parse_alias_map(&market_alias)?;
            scan(news_file, threshold, aliases, signals_file, stdout, quantity).await
        }
    }
}

fn parse_alias_map(raw: &[String]) -> Result<HashMap<String, Vec<String>>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for entry in raw {
        let eq = entry.find('=').with_context(|| format!("expected MARKET=alias1,alias2 — got {:?}", entry))?;
        let market = entry[..eq].trim().to_uppercase();
        let aliases: Vec<String> = entry[eq+1..].split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        if aliases.is_empty() { anyhow::bail!("market {:?} has no aliases", market); }
        out.insert(market, aliases);
    }
    Ok(out)
}

async fn scan(
    news_file: PathBuf,
    threshold: f64,
    aliases: HashMap<String, Vec<String>>,
    signals_file: Option<PathBuf>,
    stdout: bool,
    quantity: f64,
) -> Result<()> {
    let f = tokio::fs::File::open(&news_file).await.with_context(|| format!("open {:?}", news_file))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();

    let mut out: Option<tokio::fs::File> = match signals_file.as_ref() {
        Some(p) => Some(tokio::fs::OpenOptions::new().create(true).append(true).open(p).await
            .with_context(|| format!("open {:?}", p))?),
        None => None,
    };

    let mut seen = 0u64;
    let mut matched = 0u64;
    let mut skipped_no_market = 0u64;
    let mut skipped_below_thr = 0u64;
    let mut by_market: HashMap<String, u64> = HashMap::new();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let headline = v.get("headline").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if headline.is_empty() { continue; }
        seen += 1;
        let score = sentiment_score(&headline);
        let matched_markets: Vec<&String> = aliases.iter()
            .filter(|(_, al)| al.iter().any(|a| headline.to_lowercase().contains(a)))
            .map(|(m, _)| m)
            .collect();

        if stdout {
            println!("{}", json!({
                "kind": "news.sentiment",
                "ts": v.get("ts"),
                "headline": headline,
                "score": score,
                "matched_markets": matched_markets,
            }));
        }
        if matched_markets.is_empty() { skipped_no_market += 1; continue; }
        if score.abs() < threshold { skipped_below_thr += 1; continue; }

        let side = if score >= 0.0 { "buy" } else { "sell" };
        for m in &matched_markets {
            *by_market.entry((**m).clone()).or_default() += 1;
            if let Some(f) = out.as_mut() {
                let signal = json!({
                    "market": m,
                    "side": side,
                    "quantity": format!("{}", quantity),
                    "price": "0",
                    "ref": format!("news-{}", seen),
                });
                let line_out = serde_json::to_string(&signal)? + "\n";
                f.write_all(line_out.as_bytes()).await?;
            }
            matched += 1;
        }
    }

    if let Some(f) = out.as_mut() { f.flush().await?; }
    println!("{}", serde_json::to_string_pretty(&json!({
        "kind": "news.sentiment.summary",
        "ts": Utc::now().to_rfc3339(),
        "headlines_seen": seen,
        "signals_emitted": matched,
        "skipped_no_market": skipped_no_market,
        "skipped_below_threshold": skipped_below_thr,
        "by_market": by_market,
    }))?);
    Ok(())
}

/// Tiny sentiment scorer — lexicon-based, [-1..+1]. Deterministic.
fn sentiment_score(headline: &str) -> f64 {
    let h = headline.to_lowercase();
    let pos = ["pump", "rally", "surge", "moon", "bullish", "gain", "up", "record high", "beats", "positive", "breakthrough", "approved", "adopts"];
    let neg = ["dump", "crash", "plunge", "bearish", "loss", "down", "record low", "misses", "negative", "collapse", "hacked", "banned", "sued", "sec"];
    let mut score: f64 = 0.0;
    for w in pos { if h.contains(w) { score += 0.25; } }
    for w in neg { if h.contains(w) { score -= 0.25; } }
    for w in ["massive", "huge", "record"] { if h.contains(w) { score *= 1.3; } }
    if score > 1.0 { score = 1.0; } else if score < -1.0 { score = -1.0; }
    (score * 100.0).round() / 100.0
}
