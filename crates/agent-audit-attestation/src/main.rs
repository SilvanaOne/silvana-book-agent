//! Audit Attestation Agent — checkpoint publisher for a trading-history log
//!
//! Reads the current head of an `agent-trading-history` JSONL file (last
//! record's chain hash, its `seq` and `ts`), wraps it in a signed
//! `{ ts, party, head_seq, head_hash, signature }` checkpoint, and emits
//! that checkpoint to stdout / an append-only file / an HTTP webhook.
//!
//! Auditors can later use any single checkpoint to prove that any earlier
//! record existed at that time — without having to see the full log.
//!
//! Two modes:
//! - `snapshot` — one-shot: read the head, publish, exit.
//! - `run --interval-secs N` — publish a fresh checkpoint every N seconds
//!   until SIGINT.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{info, warn};

use agent_logic::auth::decode_base58_private_key;

#[derive(Parser)]
#[command(name = "agent-audit-attestation")]
#[command(about = "Publish signed checkpoints of a trading-history chain head")]
struct Cli {
    /// Ed25519 private key (base58); falls back to PARTY_AGENT_PRIVATE_KEY env
    #[arg(long, global = true, env = "PARTY_AGENT_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<String>,

    /// Party id embedded in the checkpoint payload
    #[arg(long, global = true, env = "PARTY_ID")]
    party: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read the current head, publish one checkpoint, exit
    Snapshot(PublishArgs),
    /// Loop — publish a checkpoint every `--interval-secs` until SIGINT
    Run {
        #[command(flatten)]
        common: PublishArgs,

        #[arg(long, default_value = "300")]
        interval_secs: u64,
    },
}

#[derive(Parser)]
struct PublishArgs {
    #[arg(long)]
    history: PathBuf,

    #[arg(long)]
    stdout: bool,

    #[arg(long)]
    log_file: Option<PathBuf>,

    #[arg(long)]
    webhook: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    match cli.command {
        Commands::Snapshot(args) => {
            let pk = require_private_key(cli.private_key.as_deref())?;
            let sinks = Sinks::open(args.webhook.clone(), args.log_file.clone(), args.stdout).await?;
            publish_once(&args.history, cli.party.as_deref(), &pk, &sinks).await
        }
        Commands::Run { common, interval_secs } => {
            if interval_secs == 0 {
                anyhow::bail!("--interval-secs must be >= 1");
            }
            let pk = require_private_key(cli.private_key.as_deref())?;
            let sinks = Sinks::open(common.webhook.clone(), common.log_file.clone(), common.stdout).await?;
            run(common.history, cli.party, pk, interval_secs, sinks).await
        }
    }
}

fn require_private_key(flag: Option<&str>) -> Result<[u8; 32]> {
    let raw = flag
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PARTY_AGENT_PRIVATE_KEY").ok())
        .context("--private-key not provided and PARTY_AGENT_PRIVATE_KEY env not set")?;
    decode_base58_private_key(&raw)
}

async fn run(
    history: PathBuf,
    party: Option<String>,
    pk: [u8; 32],
    interval_secs: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    info!("Starting attestation loop — every {}s", interval_secs);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        if let Err(e) = publish_once(&history, party.as_deref(), &pk, &sinks).await {
            warn!("publish failed: {:#}", e);
        }
        tokio::select! {
            _ = &mut shutdown => { info!("shutdown"); return Ok(()); }
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {}
        }
    }
}

async fn publish_once(
    history: &PathBuf,
    party: Option<&str>,
    pk: &[u8; 32],
    sinks: &Sinks,
) -> Result<()> {
    let head = read_head(history).await?;
    let ts = Utc::now().to_rfc3339();
    let payload = json!({
        "kind": "audit.checkpoint",
        "ts": ts,
        "party": party.unwrap_or(""),
        "history_file": history.to_string_lossy(),
        "head_seq": head.seq,
        "head_ts": head.ts,
        "head_hash": head.line_hash,
        "records": head.count,
    });
    let payload_str = serde_json::to_string(&payload)?;
    let sig = message_signing::sign_canonical(pk, payload_str.as_bytes());
    let signed = json!({
        "payload": payload,
        "signature": sig.signature_b64,
        "signing_scheme": sig.signing_scheme,
        "public_key": sig.public_key_b64url,
    });
    sinks.dispatch(&signed).await;
    info!("attestation published: head_seq={} count={}", head.seq, head.count);
    Ok(())
}

struct Head {
    seq: u64,
    ts: String,
    line_hash: String,
    count: u64,
}

async fn read_head(path: &PathBuf) -> Result<Head> {
    let f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open {:?}", path))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();
    let mut last: Option<String> = None;
    let mut count: u64 = 0;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        last = Some(line);
    }
    let line = last.context("history file is empty — nothing to attest")?;
    let v: Value = serde_json::from_str(&line).context("parse last history line")?;
    let seq = v.get("seq").and_then(|x| x.as_u64()).context("last line missing seq")?;
    let ts = v
        .get("ts")
        .and_then(|x| x.as_str())
        .context("last line missing ts")?
        .to_string();
    let line_hash = hex::encode(Sha256::digest(line.as_bytes()));
    Ok(Head { seq, ts, line_hash, count })
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
            Some(reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?)
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
        Ok(Arc::new(Self { webhook, http, log_file: file, stdout }))
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
