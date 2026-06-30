//! Trading History Agent — append-only, signed event log
//!
//! Subscribes to this party's order + settlement events and writes each event
//! to a JSONL file as a *signed* record:
//!
//! ```json
//! {
//!   "seq": 17,
//!   "ts": "2026-05-20T12:34:56.123Z",
//!   "prev_hash": "<hex sha256 of previous line>",
//!   "kind": "settlement.settled",
//!   "payload": { ... },
//!   "signature": "<base64 Ed25519 over canonical(seq||ts||prev_hash||kind||payload)>",
//!   "public_key": "<base64url Ed25519 public key>"
//! }
//! ```
//!
//! The `prev_hash` chains records into an append-only tamper-evident log —
//! editing any line invalidates the chain from that point on. Verification
//! can be performed offline with `agent-signature verify-canonical`.

use anyhow::{Context, Result};
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Notify};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{
    order_update::EventType as OrderEvent,
    settlement_update::EventType as SettlementEvent,
    OrderUpdate, SettlementUpdate,
};

#[derive(Parser)]
#[command(name = "agent-trading-history")]
#[command(about = "Append-only signed log of own orders + settlements")]
struct Cli {
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        /// Path of the JSONL history file (one signed record per line)
        #[arg(long)]
        history_file: PathBuf,

        #[arg(long)]
        market: Option<String>,

        #[arg(long)]
        no_orders: bool,

        #[arg(long)]
        no_settlements: bool,
    },
    /// Verify chain integrity and signatures of an existing history file
    Verify {
        #[arg(long)]
        history_file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_trading_history", "agent_logic"],
        "agent-trading-history",
    );

    match cli.command {
        Commands::Run {
            history_file,
            market,
            no_orders,
            no_settlements,
        } => {
            if no_orders && no_settlements {
                anyhow::bail!("--no-orders and --no-settlements together leave nothing to log");
            }
            let config = BaseConfig::load(&cli.config)
                .with_context(|| format!("Failed to load config from {:?}", cli.config))?;
            run(config, history_file, market, no_orders, no_settlements).await
        }
        Commands::Verify { history_file } => verify(history_file).await,
    }
}

struct Writer {
    file: Mutex<tokio::fs::File>,
    seq: Mutex<u64>,
    prev_hash: Mutex<String>,
    private_key: [u8; 32],
}

impl Writer {
    async fn open(path: PathBuf, private_key: [u8; 32]) -> Result<Arc<Self>> {
        // Resume from existing file: scan to find max seq and last line's hash
        let (start_seq, last_hash) = resume_from(&path).await.unwrap_or((0, String::new()));
        info!("opening history at {:?} (next_seq={}, prev_hash={}…)", path, start_seq, &last_hash[..last_hash.len().min(12)]);

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("open {:?}", path))?;

        Ok(Arc::new(Self {
            file: Mutex::new(file),
            seq: Mutex::new(start_seq),
            prev_hash: Mutex::new(last_hash),
            private_key,
        }))
    }

    async fn append(&self, kind: &'static str, payload: Value) -> Result<()> {
        let mut seq = self.seq.lock().await;
        let mut prev = self.prev_hash.lock().await;
        *seq += 1;
        let ts = Utc::now().to_rfc3339();

        // Canonical bytes signed over the record's identity:
        // seq, ts, prev_hash, kind, sha256(payload as compact JSON)
        let payload_str = serde_json::to_string(&payload)?;
        let payload_hash = hex::encode(Sha256::digest(payload_str.as_bytes()));
        let canonical = format!(
            "seq={}\nts={}\nprev_hash={}\nkind={}\npayload_sha256={}\n",
            *seq, ts, *prev, kind, payload_hash
        );
        let sig = message_signing::sign_canonical(&self.private_key, canonical.as_bytes());

        let record = json!({
            "seq": *seq,
            "ts": ts,
            "prev_hash": *prev,
            "kind": kind,
            "payload": payload,
            "payload_sha256": payload_hash,
            "signing_scheme": sig.signing_scheme,
            "signature": sig.signature_b64,
            "public_key": sig.public_key_b64url,
        });

        let line = serde_json::to_string(&record)? + "\n";
        let mut f = self.file.lock().await;
        f.write_all(line.as_bytes()).await?;
        // Update prev_hash for the *next* line: sha256 of this line (without trailing newline)
        *prev = hex::encode(Sha256::digest(line.trim_end_matches('\n').as_bytes()));
        Ok(())
    }
}

async fn resume_from(path: &PathBuf) -> Option<(u64, String)> {
    let f = tokio::fs::File::open(path).await.ok()?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();
    let mut last_seq: u64 = 0;
    let mut last_line: Option<String> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if let Some(s) = v.get("seq").and_then(|x| x.as_u64()) {
                last_seq = s;
            }
        }
        last_line = Some(line);
    }
    let last_hash = last_line
        .map(|l| hex::encode(Sha256::digest(l.as_bytes())))
        .unwrap_or_default();
    Some((last_seq, last_hash))
}

async fn run(
    config: BaseConfig,
    history_file: PathBuf,
    market: Option<String>,
    no_orders: bool,
    no_settlements: bool,
) -> Result<()> {
    info!("Starting trading-history");
    info!("Party: {}", config.party_id);
    info!(
        "history_file={:?} market={:?} orders={} settlements={}",
        history_file, market, !no_orders, !no_settlements
    );

    let writer = Writer::open(history_file, config.private_key_bytes).await?;
    let shutdown = Arc::new(Notify::new());
    let sd_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Shutdown signal received");
            sd_signal.notify_waiters();
        }
    });

    let mut handles = Vec::new();
    if !no_orders {
        let cfg = config.clone();
        let w = writer.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move { orders_stream(cfg, w, m, sd).await }));
    }
    if !no_settlements {
        let cfg = config.clone();
        let w = writer.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move { settlements_stream(cfg, w, m, sd).await }));
    }
    for h in handles {
        if let Err(e) = h.await {
            error!("task panicked: {e}");
        }
    }
    Ok(())
}

async fn orders_stream(
    config: BaseConfig,
    writer: Arc<Writer>,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_orders(market).await?;
    info!("[orders] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => {
                    if let (kind, Some(payload)) = order_record(&u) {
                        if let Err(e) = writer.append(kind, payload).await {
                            warn!("history write failed: {:#}", e);
                        }
                    }
                }
                Some(Err(s)) => { error!("[orders] stream error: {s}"); return Ok(()); }
                None => { warn!("[orders] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn settlements_stream(
    config: BaseConfig,
    writer: Arc<Writer>,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_settlements(market).await?;
    info!("[settlements] stream opened");
    loop {
        tokio::select! {
            _ = shutdown.notified() => return Ok(()),
            next = stream.next() => match next {
                Some(Ok(u)) => {
                    if let (kind, Some(payload)) = settlement_record(&u) {
                        if let Err(e) = writer.append(kind, payload).await {
                            warn!("history write failed: {:#}", e);
                        }
                    }
                }
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

fn order_record(u: &OrderUpdate) -> (&'static str, Option<Value>) {
    let kind = match OrderEvent::try_from(u.event_type).unwrap_or(OrderEvent::Unspecified) {
        OrderEvent::Created => "order.created",
        OrderEvent::Updated => "order.updated",
        OrderEvent::Filled => "order.filled",
        OrderEvent::PartiallyFilled => "order.partially_filled",
        OrderEvent::Cancelled => "order.cancelled",
        OrderEvent::Expired => "order.expired",
        OrderEvent::Unspecified => return ("order.unspecified", None),
    };
    let payload = u.order.as_ref().map(|o| {
        json!({
            "order_id": o.order_id,
            "market_id": o.market_id,
            "order_type": o.order_type,
            "status": o.status,
            "price": o.price,
            "quantity": o.quantity,
            "filled": o.filled_quantity,
            "remaining": o.remaining_quantity,
        })
    });
    (kind, payload)
}

fn settlement_record(u: &SettlementUpdate) -> (&'static str, Option<Value>) {
    let kind = match SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified) {
        SettlementEvent::ProposalCreated => "settlement.proposal_created",
        SettlementEvent::StatusChanged => "settlement.status_changed",
        SettlementEvent::Settled => "settlement.settled",
        SettlementEvent::Failed => "settlement.failed",
        SettlementEvent::Cancelled => "settlement.cancelled",
        SettlementEvent::Unspecified => return ("settlement.unspecified", None),
    };
    let payload = u.proposal.as_ref().map(|p| {
        json!({
            "proposal_id": p.proposal_id,
            "market_id": p.market_id,
            "buyer": p.buyer,
            "seller": p.seller,
            "base_quantity": p.base_quantity,
            "settlement_price": p.settlement_price,
            "status": p.status,
            "error_message": p.error_message,
        })
    });
    (kind, payload)
}

async fn verify(history_file: PathBuf) -> Result<()> {
    let f = tokio::fs::File::open(&history_file)
        .await
        .with_context(|| format!("open {:?}", history_file))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();
    let mut expected_prev = String::new();
    let mut expected_seq: u64 = 0;
    let mut ok = 0u64;
    while let Ok(Some(line)) = lines.next_line().await {
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
        // Verify signature
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

        // Verify payload hash matches the stored payload
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
            .map_err(|_| anyhow::anyhow!("public_key must be 32 bytes"))?;
        let _ = BASE64.decode(sig_b64).context("bad signature b64")?;
        message_signing::verify_canonical(&pk_arr, canonical.as_bytes(), sig_b64, scheme)
            .with_context(|| format!("signature verification failed at seq {}", seq))?;

        expected_prev = hex::encode(Sha256::digest(line.as_bytes()));
        ok += 1;
    }
    println!("OK: verified {} record(s)", ok);
    Ok(())
}
