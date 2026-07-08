//! Failure Recovery Agent
//!
//! Periodically sweeps the party's settlement proposals and surfaces:
//! - PENDING settlements older than `--max-pending-age-secs`
//! - FAILED settlements (always reported)
//!
//! The deep retry/rollback flow already lives inside `settlement.rs` (the
//! `SettlementExecutor` retries failed steps and the payment queue retries
//! fee payments). This agent is a *separate* watchdog meant to run alongside
//! a trading agent: it surfaces problems an operator should act on, and can
//! optionally call `CancelOrder` for stuck orders associated with a stale
//! proposal (matched by `bid_order_id` / `offer_order_id`).
//!
//! Exits cleanly on Ctrl-C. No ledger writes (cancel_order is JWT-only).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{SettlementProposal, SettlementStatus};

#[derive(Parser)]
#[command(name = "agent-failure-recovery-settlement-watchdog")]
#[command(about = "Watchdog for stale / failed settlements")]
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
        /// Pending settlements older than this trigger a warning
        #[arg(long, default_value = "3600")]
        max_pending_age_secs: u64,

        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Cancel any active orders linked to a stale/failed proposal
        #[arg(long)]
        cancel_related_orders: bool,

        /// Don't actually cancel — just log what would be cancelled (only meaningful with --cancel-related-orders)
        #[arg(long)]
        dry_run: bool,
    },
    /// One-off snapshot and exit
    Snapshot {
        #[arg(long, default_value = "3600")]
        max_pending_age_secs: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_failure_recovery", "agent_logic"],
        "agent-failure-recovery-settlement-watchdog",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            max_pending_age_secs,
            check_interval,
            cancel_related_orders,
            dry_run,
        } => {
            run(
                config,
                max_pending_age_secs,
                check_interval,
                cancel_related_orders,
                dry_run,
            )
            .await
        }
        Commands::Snapshot {
            max_pending_age_secs,
        } => {
            let mut client = OrderbookClient::new(&config).await?;
            sweep(&mut client, max_pending_age_secs, false, false).await?;
            Ok(())
        }
    }
}

async fn run(
    config: BaseConfig,
    max_pending_age_secs: u64,
    check_interval: u64,
    cancel_related_orders: bool,
    dry_run: bool,
) -> Result<()> {
    info!(
        "Starting failure-recovery: max_pending_age={}s interval={}s cancel_related_orders={} dry_run={}",
        max_pending_age_secs, check_interval, cancel_related_orders, dry_run
    );
    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown signal received");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(check_interval)) => {
                if let Err(e) = sweep(&mut client, max_pending_age_secs, cancel_related_orders, dry_run).await {
                    error!("sweep failed: {:#}", e);
                }
            }
        }
    }
}

async fn sweep(
    client: &mut OrderbookClient,
    max_pending_age_secs: u64,
    cancel_related_orders: bool,
    dry_run: bool,
) -> Result<()> {
    let proposals = client.get_pending_proposals().await?;
    let now = Utc::now();

    let mut stale: Vec<&SettlementProposal> = Vec::new();
    let mut failed: Vec<&SettlementProposal> = Vec::new();

    for p in &proposals {
        match SettlementStatus::try_from(p.status).unwrap_or(SettlementStatus::Unspecified) {
            SettlementStatus::Pending => {
                if let Some(age) = age_secs(p, now) {
                    if age > max_pending_age_secs {
                        stale.push(p);
                    }
                }
            }
            SettlementStatus::Failed => failed.push(p),
            _ => {}
        }
    }

    info!(
        "scanned {} proposals — stale_pending={} failed={}",
        proposals.len(),
        stale.len(),
        failed.len()
    );

    if stale.is_empty() && failed.is_empty() {
        return Ok(());
    }

    for p in &stale {
        let age = age_secs(p, now).unwrap_or(0);
        warn!(
            "STALE PENDING age={}s id={} market={} qty={} px={}",
            age, p.proposal_id, p.market_id, p.base_quantity, p.settlement_price
        );
    }
    for p in &failed {
        warn!(
            "FAILED id={} market={} err={:?}",
            p.proposal_id, p.market_id, p.error_message
        );
    }

    if cancel_related_orders {
        let mut order_ids: HashSet<u64> = HashSet::new();
        for p in stale.iter().chain(failed.iter()) {
            if let Some(m) = &p.order_match {
                order_ids.insert(m.bid_order_id);
                order_ids.insert(m.offer_order_id);
            }
        }
        if order_ids.is_empty() {
            return Ok(());
        }

        // Build a set of order IDs from the agent's own active book so we don't try to
        // cancel orders that belong to the counterparty.
        let active = client.get_all_active_orders().await.unwrap_or_default();
        let active_ids: HashSet<u64> = active.iter().map(|o| o.order_id).collect();

        for id in order_ids.intersection(&active_ids).copied() {
            if dry_run {
                info!("[dry-run] would cancel related order_id={}", id);
                continue;
            }
            match client.cancel_order(id).await {
                Ok(r) if r.success => info!("cancelled related order_id={}", id),
                Ok(r) => warn!("cancel order_id={} !success: {}", id, r.message),
                Err(e) => warn!("cancel order_id={} failed: {:#}", id, e),
            }
        }
    }

    Ok(())
}

fn age_secs(p: &SettlementProposal, now: DateTime<Utc>) -> Option<u64> {
    let ts = p.created_at.as_ref()?;
    let nanos: i64 = ts.nanos.into();
    let created = DateTime::<Utc>::from_timestamp(ts.seconds, nanos.max(0) as u32)?;
    let delta = now.signed_duration_since(created).num_seconds();
    if delta < 0 {
        Some(0)
    } else {
        Some(delta as u64)
    }
}
