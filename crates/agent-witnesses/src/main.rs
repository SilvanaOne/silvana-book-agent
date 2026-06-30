//! Witnesses Agent — event-driven external command launcher
//!
//! Subscribes to this party's own settlement and order events. For each event
//! whose type matches a configured trigger, spawns an external shell command,
//! passing key fields via environment variables:
//!
//! - SILVANA_EVENT = "settlement.settled" / "order.filled" / etc.
//! - SILVANA_EVENT_TS = ISO timestamp
//! - SILVANA_PROPOSAL_ID / SILVANA_ORDER_ID
//! - SILVANA_MARKET_ID
//! - SILVANA_STATUS (settlement only)
//!
//! Run a script per event-type with `--on-<event>=<command>`. Commands are
//! executed asynchronously; failures are logged but do not abort the agent.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::Notify;
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
#[command(name = "agent-witnesses")]
#[command(about = "Run external commands on settlement / order events")]
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
        /// Command to run when a settlement reaches SETTLED
        #[arg(long)]
        on_settled: Option<String>,

        /// Command to run when a settlement reaches FAILED
        #[arg(long)]
        on_failed: Option<String>,

        /// Command to run when a settlement reaches CANCELLED
        #[arg(long)]
        on_cancelled: Option<String>,

        /// Command to run when a settlement PROPOSAL is created
        #[arg(long)]
        on_proposal_created: Option<String>,

        /// Command to run when an order is FILLED
        #[arg(long)]
        on_order_filled: Option<String>,

        /// Command to run when an order is CANCELLED
        #[arg(long)]
        on_order_cancelled: Option<String>,

        /// Optional market filter
        #[arg(long)]
        market: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_witnesses", "agent_logic"],
        "agent-witnesses",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            on_settled,
            on_failed,
            on_cancelled,
            on_proposal_created,
            on_order_filled,
            on_order_cancelled,
            market,
        } => {
            let mut handlers: HashMap<&'static str, String> = HashMap::new();
            if let Some(c) = on_settled { handlers.insert("settlement.settled", c); }
            if let Some(c) = on_failed { handlers.insert("settlement.failed", c); }
            if let Some(c) = on_cancelled { handlers.insert("settlement.cancelled", c); }
            if let Some(c) = on_proposal_created { handlers.insert("settlement.proposal_created", c); }
            if let Some(c) = on_order_filled { handlers.insert("order.filled", c); }
            if let Some(c) = on_order_cancelled { handlers.insert("order.cancelled", c); }

            if handlers.is_empty() {
                anyhow::bail!("provide at least one --on-<event>=<command> trigger");
            }
            run(config, handlers, market).await
        }
    }
}

async fn run(
    config: BaseConfig,
    handlers: HashMap<&'static str, String>,
    market: Option<String>,
) -> Result<()> {
    info!("Starting Witnesses");
    info!("Party: {}", config.party_id);
    for (k, v) in &handlers {
        info!("  trigger {} -> {}", k, v);
    }

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Shutdown signal received");
            shutdown_signal.notify_waiters();
        }
    });

    let handlers = Arc::new(handlers);

    let want_settlements = handlers.keys().any(|k| k.starts_with("settlement."));
    let want_orders = handlers.keys().any(|k| k.starts_with("order."));

    let mut tasks = Vec::new();
    if want_settlements {
        let cfg = config.clone();
        let h = handlers.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        tasks.push(tokio::spawn(async move { settlement_stream(cfg, h, m, sd).await }));
    }
    if want_orders {
        let cfg = config.clone();
        let h = handlers.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        tasks.push(tokio::spawn(async move { order_stream(cfg, h, m, sd).await }));
    }
    for t in tasks {
        if let Err(e) = t.await {
            error!("task panicked: {e}");
        }
    }
    info!("Witnesses exited");
    Ok(())
}

async fn settlement_stream(
    config: BaseConfig,
    handlers: Arc<HashMap<&'static str, String>>,
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
                Some(Ok(u)) => handle_settlement(&u, &handlers).await,
                Some(Err(s)) => { error!("[settlements] stream error: {s}"); return Ok(()); }
                None => { warn!("[settlements] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn order_stream(
    config: BaseConfig,
    handlers: Arc<HashMap<&'static str, String>>,
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
                Some(Ok(u)) => handle_order(&u, &handlers).await,
                Some(Err(s)) => { error!("[orders] stream error: {s}"); return Ok(()); }
                None => { warn!("[orders] stream ended"); return Ok(()); }
            }
        }
    }
}

async fn handle_settlement(u: &SettlementUpdate, handlers: &HashMap<&'static str, String>) {
    let kind = match SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified)
    {
        SettlementEvent::Settled => "settlement.settled",
        SettlementEvent::Failed => "settlement.failed",
        SettlementEvent::Cancelled => "settlement.cancelled",
        SettlementEvent::ProposalCreated => "settlement.proposal_created",
        SettlementEvent::StatusChanged => return, // not exposed as a separate trigger
        SettlementEvent::Unspecified => return,
    };
    let Some(cmd) = handlers.get(kind) else { return };
    let mut env: Vec<(String, String)> = vec![
        ("SILVANA_EVENT".into(), kind.into()),
        ("SILVANA_EVENT_TS".into(), now_iso()),
    ];
    if let Some(p) = &u.proposal {
        env.push(("SILVANA_PROPOSAL_ID".into(), p.proposal_id.clone()));
        env.push(("SILVANA_MARKET_ID".into(), p.market_id.clone()));
        env.push(("SILVANA_STATUS".into(), p.status.to_string()));
    }
    spawn_command(cmd.clone(), env, kind);
}

async fn handle_order(u: &OrderUpdate, handlers: &HashMap<&'static str, String>) {
    let kind = match OrderEvent::try_from(u.event_type).unwrap_or(OrderEvent::Unspecified) {
        OrderEvent::Filled => "order.filled",
        OrderEvent::Cancelled => "order.cancelled",
        OrderEvent::PartiallyFilled => return,
        OrderEvent::Created => return,
        OrderEvent::Updated => return,
        OrderEvent::Expired => return,
        OrderEvent::Unspecified => return,
    };
    let Some(cmd) = handlers.get(kind) else { return };
    let mut env: Vec<(String, String)> = vec![
        ("SILVANA_EVENT".into(), kind.into()),
        ("SILVANA_EVENT_TS".into(), now_iso()),
    ];
    if let Some(o) = &u.order {
        env.push(("SILVANA_ORDER_ID".into(), o.order_id.to_string()));
        env.push(("SILVANA_MARKET_ID".into(), o.market_id.clone()));
    }
    spawn_command(cmd.clone(), env, kind);
}

fn spawn_command(cmd: String, env: Vec<(String, String)>, kind: &'static str) {
    tokio::spawn(async move {
        info!("fire {} -> {}", kind, cmd);
        // Use the shell so users can pass pipelines and quoted strings.
        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
        let mut command = tokio::process::Command::new(shell);
        command
            .arg(flag)
            .arg(&cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        for (k, v) in env {
            command.env(k, v);
        }
        match command.status().await {
            Ok(s) if s.success() => {}
            Ok(s) => warn!("command exited with {} (kind={})", s, kind),
            Err(e) => warn!("command spawn failed (kind={}): {e}", kind),
        }
    });
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}
