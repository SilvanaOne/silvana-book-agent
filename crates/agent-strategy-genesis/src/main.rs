//! Strategy Genesis Agent — Chat-to-Strategy compiler
//!
//! Reads a natural-language execution spec and emits an equivalent TOML
//! plan for `agent-algo-order`. Deterministic keyword-based parser here
//! (drop-in a real LLM behind the same interface in prod).
//!
//! Supported input patterns:
//!   "buy 100 CC-USDC over 1 hour using TWAP"
//!   "sell 50 BTC-USD with iceberg, visible 5, price 60000"
//!   "buy 200 CC-USDC minimize slippage, max slippage 25 bps"
//!
//! Output is a valid TOML file consumable by `agent-algo-order run --plan`.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "agent-strategy-genesis")]
#[command(about = "Compile a natural-language spec into an algo-order TOML plan")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile a one-line spec into a TOML plan (single step)
    Emit {
        #[arg(long)]
        spec: String,
        /// Where to write the plan TOML (default: stdout)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Compile a multi-line file (one spec per line, blank/# lines ignored)
    Batch {
        #[arg(long)]
        specs_file: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Emit { spec, output } => {
            let plan = compile_step(&spec)?;
            let toml = plan.to_toml();
            match output {
                Some(p) => { tokio::fs::write(&p, &toml).await.with_context(|| format!("write {:?}", p))?; }
                None => println!("{}", toml),
            }
            Ok(())
        }
        Commands::Batch { specs_file, output } => {
            let body = tokio::fs::read_to_string(&specs_file).await.with_context(|| format!("read {:?}", specs_file))?;
            let mut all = String::new();
            for line in body.lines() {
                let s = line.trim();
                if s.is_empty() || s.starts_with('#') { continue; }
                let plan = compile_step(s)?;
                all.push_str(&plan.to_toml());
                all.push('\n');
            }
            tokio::fs::write(&output, &all).await.with_context(|| format!("write {:?}", output))?;
            println!("wrote {} step(s) to {:?}", all.matches("[[step]]").count(), output);
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Step {
    algorithm: String,
    market: String,
    side: String,           // "buy" or "sell"
    total: String,
    kv: Vec<(String, String)>,    // e.g. slices, duration_secs, visible, price, max_chunk, max_slippage_bps
}

impl Step {
    fn to_toml(&self) -> String {
        let mut s = String::new();
        s.push_str("[[step]]\n");
        s.push_str(&format!("algorithm = \"{}\"\n", self.algorithm));
        s.push_str(&format!("market = \"{}\"\n", self.market));
        s.push_str(&format!("side = \"{}\"\n", self.side));
        s.push_str(&format!("total = \"{}\"\n", self.total));
        for (k, v) in &self.kv {
            // Numbers unquoted, strings quoted heuristically
            if v.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-') {
                s.push_str(&format!("{} = {}\n", k, v));
            } else {
                s.push_str(&format!("{} = \"{}\"\n", k, v));
            }
        }
        s
    }
}

fn compile_step(spec: &str) -> Result<Step> {
    let s = spec.to_lowercase();

    // Side
    let side = if s.contains("sell") || s.contains("offer") { "sell" } else { "buy" };

    // Market — first token matching BASE-QUOTE
    let market = extract_market(spec).context("could not find a BASE-QUOTE market in the spec")?;

    // Total — the first bare number that isn't attached to a "hour", "bps", "slices", "visible", or "price"
    let total = extract_total(&s).context("could not find a total quantity in the spec")?;

    // Algorithm selection
    let algorithm = if s.contains("iceberg") || s.contains("visible") { "iceberg" }
        else if s.contains("liquid") || s.contains("slippage") { "liquidity-seeking" }
        else { "twap" };

    let mut kv: Vec<(String, String)> = Vec::new();
    match algorithm {
        "twap" => {
            let slices = extract_after_number(&s, "slices").unwrap_or_else(|| "10".into());
            let duration = extract_duration_secs(&s).unwrap_or_else(|| "600".into());
            kv.push(("slices".into(), slices));
            kv.push(("duration_secs".into(), duration));
            if let Some(off) = extract_after_number(&s, "offset") { kv.push(("price_offset_pct".into(), off)); }
        }
        "iceberg" => {
            let visible = extract_after_number(&s, "visible").unwrap_or_else(|| "1".into());
            let price = extract_after_number(&s, "price").unwrap_or_else(|| "0".into());
            kv.push(("visible".into(), visible));
            kv.push(("price".into(), price));
            if let Some(p) = extract_after_number(&s, "poll") { kv.push(("poll_secs".into(), p)); }
        }
        "liquidity-seeking" => {
            let max_chunk = extract_after_number(&s, "chunk").unwrap_or_else(|| "1".into());
            let bps = extract_after_number(&s, "bps")
                .or_else(|| extract_after_number(&s, "slippage"))
                .unwrap_or_else(|| "25".into());
            let depth = extract_after_number(&s, "depth").unwrap_or_else(|| "20".into());
            kv.push(("max_chunk".into(), max_chunk));
            kv.push(("max_slippage_bps".into(), bps));
            kv.push(("depth".into(), depth));
        }
        _ => unreachable!(),
    }

    Ok(Step { algorithm: algorithm.to_string(), market, side: side.to_string(), total, kv })
}

fn extract_market(spec: &str) -> Option<String> {
    for tok in spec.split_whitespace() {
        let t = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        if let Some((a, b)) = t.split_once('-') {
            if !a.is_empty() && !b.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric()) && b.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some(t.to_uppercase());
            }
        }
    }
    None
}

fn extract_total(s: &str) -> Option<String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    for (i, t) in tokens.iter().enumerate() {
        // Numeric?
        if let Ok(n) = t.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').parse::<f64>() {
            // Following word must not be a keyword the parser is grabbing separately.
            let next = tokens.get(i + 1).copied().unwrap_or("");
            if ["hour", "hours", "minute", "minutes", "second", "seconds", "bps", "slices", "slice",
                "visible", "price", "poll", "depth", "chunk", "offset", "%", "slippage"]
                .iter().any(|k| next.contains(k)) { continue; }
            // Skip if the number is actually the market's number segment (edge case; skip 4+ digit tokens starting with a base-like prefix)
            if n < 100_000_000.0 && n > 0.0 { return Some(t.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string()); }
        }
    }
    None
}

/// Find a number that appears near a keyword. Prefers "keyword N" (N after
/// the keyword) over "N keyword", so "visible 5 price 60000" correctly
/// resolves price=60000, not price=5.
fn extract_after_number(s: &str, keyword: &str) -> Option<String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    for (i, t) in tokens.iter().enumerate() {
        if !t.contains(keyword) { continue; }
        // Prefer the token AFTER the keyword.
        if let Some(nt) = tokens.get(i + 1) {
            let cleaned = nt.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
            if let Ok(n) = cleaned.parse::<f64>() {
                if n >= 0.0 { return Some(cleaned.to_string()); }
            }
        }
        // Fallback: the token BEFORE.
        if i > 0 {
            if let Some(nt) = tokens.get(i - 1) {
                let cleaned = nt.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
                if let Ok(n) = cleaned.parse::<f64>() {
                    if n >= 0.0 { return Some(cleaned.to_string()); }
                }
            }
        }
    }
    None
}

/// "over 1 hour" / "over 30 minutes" / "over 45s"
fn extract_duration_secs(s: &str) -> Option<String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    for i in 0..tokens.len().saturating_sub(1) {
        let n_tok = tokens[i].trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let n: f64 = match n_tok.parse() { Ok(x) => x, Err(_) => continue };
        let unit = tokens[i + 1];
        let secs = if unit.starts_with("hour") { (n * 3600.0) as u64 }
        else if unit.starts_with("minute") || unit.starts_with("min") { (n * 60.0) as u64 }
        else if unit.starts_with("second") || unit.starts_with("sec") || unit == "s" { n as u64 }
        else { continue };
        if secs > 0 { return Some(secs.to_string()); }
    }
    None
}
