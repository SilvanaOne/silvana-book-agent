//! Selective Disclosure Agent
//!
//! Post-processor over an `agent-trading-history` JSONL log. Reads the signed,
//! hash-chained log, applies filters (event-kind / market / field redaction),
//! and emits a **fresh** hash-chained signed log that contains only what you
//! intend to share with an auditor or regulator.
//!
//! Two modes:
//! - `filter` — produce the disclosure log
//! - `verify` — independently verify chain + signatures of a disclosure log
//!
//! The disclosure log uses the same record schema as `agent-trading-history`
//! (`seq, ts, prev_hash, kind, payload, payload_sha256, signing_scheme,
//! signature, public_key`) and signs every line with this party's Ed25519 key.
//! That means a recipient can verify the chain locally with the standard
//! `agent-trading-history verify` if they wish.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use orderbook_agent_logic::auth::decode_base58_private_key;

#[derive(Parser)]
#[command(name = "agent-selective-disclosure")]
#[command(about = "Filter and re-sign a trading-history JSONL audit log")]
struct Cli {
    /// Base58 Ed25519 private key (falls back to PARTY_AGENT_PRIVATE_KEY env)
    #[arg(long, global = true, env = "PARTY_AGENT_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Apply filters to a history file and emit a new signed chained log
    Filter {
        #[arg(long)]
        history: PathBuf,

        #[arg(long)]
        output: PathBuf,

        /// Keep only these event kinds (e.g. settlement.settled,order.filled). Empty = keep all.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        kinds: Vec<String>,

        /// Keep only events for these markets. Empty = keep all.
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        markets: Vec<String>,

        /// Redact (drop) these fields from each record's payload object
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        redact_fields: Vec<String>,
    },
    /// Verify chain + signatures of a disclosure log
    Verify {
        #[arg(long)]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::Filter {
            history,
            output,
            kinds,
            markets,
            redact_fields,
        } => {
            let pk = require_private_key(cli.private_key.as_deref())?;
            filter(history, output, kinds, markets, redact_fields, pk).await
        }
        Commands::Verify { output } => verify(output).await,
    }
}

fn require_private_key(flag: Option<&str>) -> Result<[u8; 32]> {
    let raw = flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .context("--private-key not provided and PARTY_AGENT_PRIVATE_KEY env not set")?;
    decode_base58_private_key(&raw)
}

async fn filter(
    history: PathBuf,
    output: PathBuf,
    kinds: Vec<String>,
    markets: Vec<String>,
    redact_fields: Vec<String>,
    private_key: [u8; 32],
) -> Result<()> {
    let kinds_set: HashSet<String> = kinds.into_iter().collect();
    let markets_set: HashSet<String> = markets.into_iter().collect();
    let redact_set: HashSet<String> = redact_fields.into_iter().collect();

    let input = tokio::fs::File::open(&history)
        .await
        .with_context(|| format!("open {:?}", history))?;
    let reader = BufReader::new(input);
    let mut lines = reader.lines();

    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&output)
        .await
        .with_context(|| format!("open {:?}", output))?;

    let mut seq: u64 = 0;
    let mut prev_hash = String::new();
    let mut kept = 0u64;
    let mut skipped = 0u64;

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                skipped += 1;
                eprintln!("warn: skipping malformed input line: {}", e);
                continue;
            }
        };
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        if !kinds_set.is_empty() && !kinds_set.contains(kind) {
            skipped += 1;
            continue;
        }
        let market_id = v
            .pointer("/payload/market_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if !markets_set.is_empty() && !markets_set.contains(&market_id) {
            skipped += 1;
            continue;
        }

        // Build redacted payload
        let mut payload = v.get("payload").cloned().unwrap_or(Value::Null);
        if let Some(obj) = payload.as_object_mut() {
            for field in &redact_set {
                obj.remove(field);
            }
        }

        // Emit a fresh signed record
        seq += 1;
        let ts = Utc::now().to_rfc3339();
        let payload_str = serde_json::to_string(&payload)?;
        let payload_hash = hex::encode(Sha256::digest(payload_str.as_bytes()));
        let canonical = format!(
            "seq={}\nts={}\nprev_hash={}\nkind={}\npayload_sha256={}\n",
            seq, ts, prev_hash, kind, payload_hash
        );
        let sig = message_signing::sign_canonical(&private_key, canonical.as_bytes());

        let record = json!({
            "seq": seq,
            "ts": ts,
            "prev_hash": prev_hash,
            "kind": kind,
            "payload": payload,
            "payload_sha256": payload_hash,
            "signing_scheme": sig.signing_scheme,
            "signature": sig.signature_b64,
            "public_key": sig.public_key_b64url,
            "source": {
                "original_seq": v.get("seq"),
                "original_ts": v.get("ts"),
            },
        });
        let line_out = serde_json::to_string(&record)? + "\n";
        out.write_all(line_out.as_bytes()).await?;

        prev_hash = hex::encode(Sha256::digest(line_out.trim_end_matches('\n').as_bytes()));
        kept += 1;
    }

    out.flush().await?;
    println!("Disclosure written: {} records kept, {} skipped", kept, skipped);
    Ok(())
}

async fn verify(output: PathBuf) -> Result<()> {
    let f = tokio::fs::File::open(&output)
        .await
        .with_context(|| format!("open {:?}", output))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();
    let mut expected_prev = String::new();
    let mut expected_seq: u64 = 0;
    let mut ok: u64 = 0;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).context("parse line")?;
        expected_seq += 1;

        let seq = v.get("seq").and_then(|x| x.as_u64()).context("missing seq")?;
        if seq != expected_seq {
            anyhow::bail!("seq jump at line {}: expected {} got {}", ok + 1, expected_seq, seq);
        }
        let prev = v.get("prev_hash").and_then(|x| x.as_str()).unwrap_or("");
        if prev != expected_prev {
            anyhow::bail!(
                "prev_hash mismatch at seq {}: expected {} got {}",
                seq, expected_prev, prev
            );
        }
        let kind = v.get("kind").and_then(|x| x.as_str()).context("missing kind")?;
        let ts = v.get("ts").and_then(|x| x.as_str()).context("missing ts")?;
        let payload_hash = v
            .get("payload_sha256")
            .and_then(|x| x.as_str())
            .context("missing payload_sha256")?;
        let sig_b64 = v.get("signature").and_then(|x| x.as_str()).context("missing signature")?;
        let scheme = v
            .get("signing_scheme")
            .and_then(|x| x.as_str())
            .context("missing signing_scheme")?;
        let pk_b64url = v
            .get("public_key")
            .and_then(|x| x.as_str())
            .context("missing public_key")?;

        let payload_str = serde_json::to_string(v.get("payload").unwrap_or(&Value::Null))?;
        let actual_hash = hex::encode(Sha256::digest(payload_str.as_bytes()));
        if actual_hash != payload_hash {
            anyhow::bail!("payload_sha256 mismatch at seq {}", seq);
        }
        let canonical = format!(
            "seq={}\nts={}\nprev_hash={}\nkind={}\npayload_sha256={}\n",
            seq, ts, prev, kind, payload_hash
        );
        let pk_bytes = URL_SAFE_NO_PAD
            .decode(pk_b64url)
            .context("bad public_key b64url")?;
        let pk_arr: [u8; 32] = pk_bytes
            .try_into()
            .map_err(|_| anyhow!("public_key must be 32 bytes"))?;
        message_signing::verify_canonical(&pk_arr, canonical.as_bytes(), sig_b64, scheme)
            .with_context(|| format!("signature verification failed at seq {}", seq))?;

        expected_prev = hex::encode(Sha256::digest(line.as_bytes()));
        ok += 1;
    }
    println!("OK: verified {} disclosure record(s)", ok);
    Ok(())
}
