//! Audit Retention Agent — chained log rotation
//!
//! Reads a big `agent-trading-history` JSONL, splits it into per-day
//! (or per-week) slice files, and stitches the slices back into one
//! cross-slice chain by embedding each slice's `prev_slice_hash` in a
//! signed slice header record at the top of every slice.
//!
//! Slices are named `slice-YYYY-MM-DD.jsonl` (or `slice-YYYY-Www.jsonl`
//! for weekly). Records older than `--retention-days` (if given) are
//! dropped — but the chain remains provable through the surviving slice
//! headers.
//!
//! Two commands:
//! - `rotate` — read history, emit slices into `--out-dir`
//! - `verify` — walk the slice directory in chronological order and
//!   verify each slice's prev_slice_hash points at the previous slice's
//!   line-hash tail.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use agent_logic::auth::decode_base58_private_key;

#[derive(Parser)]
#[command(name = "agent-audit-retention-chained")]
#[command(about = "Split a trading-history log into signed, chained per-day slices")]
struct Cli {
    /// Ed25519 private key (base58); falls back to PARTY_AGENT_PRIVATE_KEY
    #[arg(long, global = true, env = "PARTY_AGENT_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Split `--history` into slices under `--out-dir`
    Rotate {
        #[arg(long)]
        history: PathBuf,

        #[arg(long)]
        out_dir: PathBuf,

        /// Drop records older than N days (default: keep everything)
        #[arg(long)]
        retention_days: Option<u32>,

        /// Group by week (default: by day)
        #[arg(long)]
        weekly: bool,
    },
    /// Walk `--dir` in chronological order and verify the slice chain
    Verify {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::Rotate { history, out_dir, retention_days, weekly } => {
            let pk = require_private_key(cli.private_key.as_deref())?;
            rotate(history, out_dir, retention_days, weekly, pk).await
        }
        Commands::Verify { dir } => verify(dir).await,
    }
}

fn require_private_key(flag: Option<&str>) -> Result<[u8; 32]> {
    let raw = flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .context("--private-key not provided and PARTY_AGENT_PRIVATE_KEY env not set")?;
    decode_base58_private_key(&raw)
}

fn bucket_key(ts: &str, weekly: bool) -> Option<String> {
    let parsed = DateTime::parse_from_rfc3339(ts).ok()?;
    let utc = parsed.with_timezone(&Utc);
    if weekly {
        let iso = utc.iso_week();
        Some(format!("{}-W{:02}", iso.year(), iso.week()))
    } else {
        Some(utc.format("%Y-%m-%d").to_string())
    }
}

async fn rotate(
    history: PathBuf,
    out_dir: PathBuf,
    retention_days: Option<u32>,
    weekly: bool,
    pk: [u8; 32],
) -> Result<()> {
    tokio::fs::create_dir_all(&out_dir).await.with_context(|| format!("mkdir {:?}", out_dir))?;

    let f = tokio::fs::File::open(&history).await.with_context(|| format!("open {:?}", history))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();

    let cutoff = retention_days.map(|d| Utc::now() - chrono::Duration::days(d as i64));

    // Bucket records into memory (BTreeMap keeps chronological order).
    let mut buckets: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut total: u64 = 0;
    let mut dropped: u64 = 0;
    let mut skipped: u64 = 0;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => { skipped += 1; continue; }
        };
        let ts = match v.get("ts").and_then(|x| x.as_str()) { Some(s) => s.to_string(), None => { skipped += 1; continue; } };
        if let Some(c) = cutoff {
            let parsed = DateTime::parse_from_rfc3339(&ts).ok();
            if parsed.map(|p| p.with_timezone(&Utc) < c).unwrap_or(false) {
                dropped += 1;
                continue;
            }
        }
        let key = match bucket_key(&ts, weekly) { Some(k) => k, None => { skipped += 1; continue; } };
        buckets.entry(key).or_default().push(v);
        total += 1;
    }

    // Emit each slice in chronological order. Each slice starts with a signed
    // header referencing prev_slice_hash.
    let mut prev_slice_hash = String::new();
    let mut slice_num: u64 = 0;
    for (bucket, records) in &buckets {
        slice_num += 1;
        let slice_name = format!("slice-{}.jsonl", bucket);
        let path = out_dir.join(&slice_name);
        let mut out = tokio::fs::File::create(&path).await.with_context(|| format!("create {:?}", path))?;

        // Header record — hash-chained via prev_slice_hash
        let header_payload = json!({
            "kind": "audit.slice_header",
            "slice_num": slice_num,
            "bucket": bucket,
            "record_count": records.len(),
            "prev_slice_hash": prev_slice_hash,
            "created_at": Utc::now().to_rfc3339(),
        });
        let header_str = serde_json::to_string(&header_payload)?;
        let sig = message_signing::sign_canonical(&pk, header_str.as_bytes());
        let signed_header = json!({
            "payload": header_payload,
            "signature": sig.signature_b64,
            "signing_scheme": sig.signing_scheme,
            "public_key": sig.public_key_b64url,
        });
        let mut line_hash_running = Sha256::new();
        let header_line = serde_json::to_string(&signed_header)? + "\n";
        out.write_all(header_line.as_bytes()).await?;
        line_hash_running.update(header_line.trim_end().as_bytes());

        for r in records {
            let line = serde_json::to_string(r)? + "\n";
            out.write_all(line.as_bytes()).await?;
            line_hash_running.update(line.trim_end().as_bytes());
        }
        out.flush().await?;

        prev_slice_hash = hex::encode(line_hash_running.finalize());
        println!("wrote {:?}  records={}  hash={}", path, records.len(), &prev_slice_hash[..16]);
    }
    println!(
        "Rotate complete: {} slices, {} records kept, {} dropped by retention, {} skipped",
        buckets.len(), total, dropped, skipped
    );
    Ok(())
}

async fn verify(dir: PathBuf) -> Result<()> {
    let mut entries = tokio::fs::read_dir(&dir).await.with_context(|| format!("read_dir {:?}", dir))?;
    let mut slice_files: Vec<PathBuf> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if name.starts_with("slice-") { slice_files.push(p); }
            }
        }
    }
    slice_files.sort();
    if slice_files.is_empty() { anyhow::bail!("no slice-*.jsonl files under {:?}", dir); }

    let mut expected_prev = String::new();
    let mut expected_slice: u64 = 0;
    let mut total_records: u64 = 0;

    for slice_path in &slice_files {
        expected_slice += 1;
        let f = tokio::fs::File::open(slice_path).await.with_context(|| format!("open {:?}", slice_path))?;
        let reader = BufReader::new(f);
        let mut lines = reader.lines();
        let header_line = lines.next_line().await?.context("slice missing header line")?;
        let header: Value = serde_json::from_str(&header_line).context("bad header")?;
        let payload = header.get("payload").context("header missing payload")?;
        let slice_num = payload.get("slice_num").and_then(|x| x.as_u64()).context("missing slice_num")?;
        if slice_num != expected_slice {
            anyhow::bail!("slice_num jump in {:?}: expected {} got {}", slice_path, expected_slice, slice_num);
        }
        let prev = payload.get("prev_slice_hash").and_then(|x| x.as_str()).unwrap_or("");
        if prev != expected_prev {
            anyhow::bail!(
                "prev_slice_hash mismatch in {:?}: expected {} got {}",
                slice_path, expected_prev, prev
            );
        }
        let claimed = payload.get("record_count").and_then(|x| x.as_u64()).unwrap_or(0);

        // Rehash the entire slice file (header + all records) to derive next expected_prev.
        let mut hasher = Sha256::new();
        hasher.update(header_line.as_bytes());
        let mut count: u64 = 0;
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() { continue; }
            hasher.update(line.as_bytes());
            count += 1;
        }
        if count != claimed {
            anyhow::bail!("slice {} record_count mismatch: header says {} but file has {}", slice_num, claimed, count);
        }
        expected_prev = hex::encode(hasher.finalize());
        total_records += count;
        println!("OK slice #{} {:?}  records={}  next_prev={}", slice_num, slice_path, count, &expected_prev[..16]);
    }
    println!("OK: verified {} slices, {} total records", expected_slice, total_records);
    Ok(())
}
