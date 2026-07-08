//! Human Approval Agent — manual sign-off queue
//!
//! File-backed approval queue for orders that need a human signature before
//! hitting the orderbook. The flow:
//!
//! 1. Some upstream agent (or a script) calls `enqueue --file orders.jsonl`
//!    which appends each line as a *pending* entry in the queue file.
//! 2. An operator runs `list` to inspect the queue.
//! 3. The operator runs `approve <id>` or `reject <id>`.
//!    - `approve` signs the order with the agent's Ed25519 key and submits it
//!      via `OrderbookService.SubmitOrder`; the entry is marked APPROVED.
//!    - `reject` marks the entry REJECTED and never submits anything.
//! 4. `purge` drops APPROVED+REJECTED entries from the file.
//!
//! Entry shape inside the queue file (one JSONL record per line):
//!
//! ```json
//! {"id":"...","ts_enqueued":"...","status":"pending|approved|rejected",
//!  "order":{"market":"...","side":"buy|sell","quantity":"...","price":"...","ref":"..."},
//!  "decided_at":null,"decided_by":null,"reason":null,"submit_order_id":null}
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-human-approval-queue")]
#[command(about = "Manual sign-off queue for orders")]
struct Cli {
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(long, global = true)]
    dry_run: bool,

    #[arg(long, global = true)]
    force: bool,

    #[arg(long, global = true)]
    confirm: bool,

    /// Path of the queue file (JSONL, one entry per line)
    #[arg(long, global = true, default_value = "approval-queue.jsonl")]
    queue: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Append orders from a JSONL file into the queue as pending entries
    Enqueue {
        #[arg(long)]
        file: PathBuf,
    },
    /// Print queue contents
    List {
        /// Only show entries with this status (pending|approved|rejected)
        #[arg(long)]
        status: Option<String>,
    },
    /// Approve a pending entry by id and submit the order
    Approve {
        #[arg(long)]
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        by: Option<String>,
    },
    /// Reject a pending entry by id (no submission)
    Reject {
        #[arg(long)]
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        by: Option<String>,
    },
    /// Drop approved + rejected entries from the file
    Purge,
    /// Settlement worker (run after approves caused fills)
    Settle {
        #[arg(long)]
        no_restore: bool,
    },
}

#[derive(Serialize, Deserialize, Clone)]
struct QueueEntry {
    id: String,
    ts_enqueued: String,
    status: String, // "pending" | "approved" | "rejected"
    order: OrderSpec,
    #[serde(default)]
    decided_at: Option<String>,
    #[serde(default)]
    decided_by: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    submit_order_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OrderSpec {
    market: String,
    side: String,
    quantity: String,
    price: String,
    #[serde(default)]
    r#ref: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_human_approval", "agent_logic", "tx_verifier"],
        "agent-human-approval-queue",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Enqueue { file } => enqueue(&cli.queue, file).await,
        Commands::List { status } => list(&cli.queue, status).await,
        Commands::Approve { id, reason, by } => approve(&cli.queue, &config, id, reason, by).await,
        Commands::Reject { id, reason, by } => reject(&cli.queue, id, reason, by).await,
        Commands::Purge => purge(&cli.queue).await,
        Commands::Settle { no_restore } => {
            settle(config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore).await
        }
    }
}

async fn read_queue(path: &PathBuf) -> Result<Vec<QueueEntry>> {
    let mut entries = Vec::new();
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(entries);
    }
    let f = tokio::fs::File::open(path).await?;
    let mut reader = BufReader::new(f).lines();
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let entry: QueueEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warn: skipping malformed queue entry: {}", e);
                continue;
            }
        };
        entries.push(entry);
    }
    Ok(entries)
}

async fn write_queue(path: &PathBuf, entries: &[QueueEntry]) -> Result<()> {
    let mut tmp = path.clone();
    tmp.set_extension({
        let mut e = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if !e.is_empty() { e.push('.'); }
        e.push_str("tmp");
        e
    });
    let mut f = tokio::fs::File::create(&tmp).await?;
    for entry in entries {
        let s = serde_json::to_string(entry)? + "\n";
        f.write_all(s.as_bytes()).await?;
    }
    f.flush().await?;
    drop(f);
    tokio::fs::rename(&tmp, path).await.with_context(|| format!("rename {:?} -> {:?}", tmp, path))?;
    Ok(())
}

async fn enqueue(queue: &PathBuf, file: PathBuf) -> Result<()> {
    let f = tokio::fs::File::open(&file).await.with_context(|| format!("open {:?}", file))?;
    let mut reader = BufReader::new(f).lines();
    let mut existing = read_queue(queue).await?;
    let start_n = existing.len();
    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() || line.trim().starts_with('#') {
            continue;
        }
        let order: OrderSpec = serde_json::from_str(&line)
            .with_context(|| format!("parse order: {}", line))?;
        let id = format!("ha-{}-{}", chrono::Utc::now().timestamp_millis(), existing.len() + 1);
        existing.push(QueueEntry {
            id,
            ts_enqueued: Utc::now().to_rfc3339(),
            status: "pending".into(),
            order,
            decided_at: None,
            decided_by: None,
            reason: None,
            submit_order_id: None,
        });
    }
    let added = existing.len() - start_n;
    write_queue(queue, &existing).await?;
    println!("enqueued {} orders into {:?} (queue size: {})", added, queue, existing.len());
    Ok(())
}

async fn list(queue: &PathBuf, status: Option<String>) -> Result<()> {
    let entries = read_queue(queue).await?;
    let filter = status.as_deref();
    for e in &entries {
        if let Some(f) = filter {
            if e.status != f {
                continue;
            }
        }
        let order_ref = e.order.r#ref.as_deref().unwrap_or("-");
        let decided = e
            .decided_at
            .as_deref()
            .unwrap_or("-");
        println!(
            "{:<26} [{:<8}] {} {} {} @ {} ref={} decided={} reason={}",
            e.id,
            e.status,
            e.order.side,
            e.order.quantity,
            e.order.market,
            e.order.price,
            order_ref,
            decided,
            e.reason.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

async fn approve(
    queue: &PathBuf,
    config: &BaseConfig,
    id: String,
    reason: Option<String>,
    by: Option<String>,
) -> Result<()> {
    let mut entries = read_queue(queue).await?;
    let pos = entries
        .iter()
        .position(|e| e.id == id)
        .ok_or_else(|| anyhow::anyhow!("no queue entry with id {}", id))?;
    if entries[pos].status != "pending" {
        anyhow::bail!("entry {} is in status {} — only 'pending' entries can be approved", id, entries[pos].status);
    }
    let order = entries[pos].order.clone();
    let order_type = match order.side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        other => anyhow::bail!("bad side {:?} on entry {}", other, id),
    };
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };

    let mut client = OrderbookClient::new(config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );
    let _ = rust_decimal::Decimal::from_str(&order.quantity).context("bad quantity")?;
    let _ = rust_decimal::Decimal::from_str(&order.price).context("bad price")?;

    let (signature, signed_data, nonce) =
        tracker.sign_order(&order.market, label, &order.price, &order.quantity);
    let order_ref = order.r#ref.clone().unwrap_or_else(|| format!("ha-{}", id));
    let resp = client
        .submit_order(
            &order.market,
            order_type,
            order.price.clone(),
            order.quantity.clone(),
            Some(order_ref),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
        .with_context(|| format!("submit_order for {}", id))?;

    let order_id = resp.order.as_ref().map(|o| o.order_id);
    entries[pos].status = "approved".into();
    entries[pos].decided_at = Some(Utc::now().to_rfc3339());
    entries[pos].decided_by = by;
    entries[pos].reason = reason;
    entries[pos].submit_order_id = order_id;
    write_queue(queue, &entries).await?;

    println!("approved {} → order_id={:?}", id, order_id);
    Ok(())
}

async fn reject(
    queue: &PathBuf,
    id: String,
    reason: Option<String>,
    by: Option<String>,
) -> Result<()> {
    let mut entries = read_queue(queue).await?;
    let pos = entries
        .iter()
        .position(|e| e.id == id)
        .ok_or_else(|| anyhow::anyhow!("no queue entry with id {}", id))?;
    if entries[pos].status != "pending" {
        anyhow::bail!("entry {} is in status {} — only 'pending' entries can be rejected", id, entries[pos].status);
    }
    entries[pos].status = "rejected".into();
    entries[pos].decided_at = Some(Utc::now().to_rfc3339());
    entries[pos].decided_by = by;
    entries[pos].reason = reason;
    write_queue(queue, &entries).await?;
    println!("rejected {}", id);
    Ok(())
}

async fn purge(queue: &PathBuf) -> Result<()> {
    let entries = read_queue(queue).await?;
    let kept: Vec<QueueEntry> = entries.into_iter().filter(|e| e.status == "pending").collect();
    write_queue(queue, &kept).await?;
    println!("queue purged, {} pending entries remain", kept.len());
    Ok(())
}

struct CloudBalanceProvider {
    client: TokioMutex<DAppProviderClient>,
}
#[async_trait]
impl BalanceProvider for CloudBalanceProvider {
    async fn fetch_balances(&self) -> Result<Vec<TokenBalance>> {
        self.client.lock().await.get_balances().await
    }
}

async fn settle(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
) -> Result<()> {
    info!("starting settlement worker for human-approval");
    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    let confirm_lock = agent_logic::confirm::new_confirm_lock();
    let runner_shutdown = agent_logic::shutdown::Shutdown::new();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
        runner_shutdown.clone(),
    );
    let ledger_client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await?;
    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only: true,
            orders_only: false,
            actionable_count: None,
            shutdown: Some(runner_shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(shutdown.clone())),
            state_file: Some(PathBuf::from("human-approval-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}
