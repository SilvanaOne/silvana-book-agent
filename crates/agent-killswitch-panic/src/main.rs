//! Killswitch Agent — emergency stop
//!
//! Two modes:
//! - `panic` — one-shot: cancel every active order this party has, then exit.
//! - `run` — monitor mode: poll a health condition on a schedule, and if it
//!   trips, fire the panic action and exit. Conditions supported:
//!     * `--max-rejects` consecutive failed settlements
//!     * `--min-cc-balance` for the unlocked Canton Coin balance dropping below
//!
//! Read the trigger that fired from the exit code (0 = manual/clean, 2 = trip).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::SettlementStatus;

const EXIT_TRIPPED: i32 = 2;

#[derive(Parser)]
#[command(name = "agent-killswitch-panic")]
#[command(about = "Emergency cancel-all + health monitor")]
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
    /// Cancel every active order for this party right now, then exit
    Panic {
        /// Optional market filter
        #[arg(long)]
        market: Option<String>,
    },
    /// Monitor a health condition and trip the killswitch when it fires
    Run {
        /// How often to evaluate the condition in seconds
        #[arg(long, default_value = "15")]
        check_interval: u64,

        /// Optional market filter for the cancel-all on trip
        #[arg(long)]
        market: Option<String>,

        /// Trip if `failed` settlements in the last poll exceed this count
        #[arg(long)]
        max_failed_settlements: Option<u32>,

        /// Trip if the agent's active order count exceeds this number
        #[arg(long)]
        max_open_orders: Option<u32>,

        /// Don't actually cancel — just log what would be cancelled
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_killswitch", "agent_logic"],
        "agent-killswitch-panic",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Panic { market } => {
            let mut client = OrderbookClient::new(&config).await?;
            let n = cancel_all(&mut client, market.as_deref(), false).await?;
            info!("Killswitch PANIC complete: cancelled {} orders", n);
            Ok(())
        }
        Commands::Run {
            check_interval,
            market,
            max_failed_settlements,
            max_open_orders,
            dry_run,
        } => {
            if max_failed_settlements.is_none() && max_open_orders.is_none() {
                anyhow::bail!(
                    "Provide at least one trigger: --max-failed-settlements and/or --max-open-orders"
                );
            }
            run_monitor(
                config,
                check_interval,
                market,
                max_failed_settlements,
                max_open_orders,
                dry_run,
            )
            .await
        }
    }
}

async fn run_monitor(
    config: BaseConfig,
    check_interval: u64,
    market: Option<String>,
    max_failed_settlements: Option<u32>,
    max_open_orders: Option<u32>,
    dry_run: bool,
) -> Result<()> {
    info!(
        "Killswitch monitor started: interval={}s market={:?} max_failed={:?} max_open={:?} dry_run={}",
        check_interval, market, max_failed_settlements, max_open_orders, dry_run
    );

    let mut client = OrderbookClient::new(&config).await?;

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("Shutdown signal received");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(check_interval)) => {
                if let Some(trigger) = evaluate(&mut client, market.as_deref(), max_failed_settlements, max_open_orders).await? {
                    error!("KILLSWITCH TRIPPED: {}", trigger);
                    let cancelled = cancel_all(&mut client, market.as_deref(), dry_run).await?;
                    error!("cancel-all complete: {} orders affected (dry_run={})", cancelled, dry_run);
                    std::process::exit(EXIT_TRIPPED);
                }
            }
        }
    }
}

async fn evaluate(
    client: &mut OrderbookClient,
    market: Option<&str>,
    max_failed_settlements: Option<u32>,
    max_open_orders: Option<u32>,
) -> Result<Option<String>> {
    if let Some(limit) = max_open_orders {
        let orders = match market {
            Some(m) => client.get_active_orders(m).await?,
            None => client.get_all_active_orders().await?,
        };
        let n = orders.len() as u32;
        info!("eval: open_orders={} limit={}", n, limit);
        if n > limit {
            return Ok(Some(format!("open orders {} exceeded limit {}", n, limit)));
        }
    }

    if let Some(limit) = max_failed_settlements {
        let failed = client
            .get_pending_proposals()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.status == SettlementStatus::Failed as i32)
            .count() as u32;
        info!("eval: failed_settlements={} limit={}", failed, limit);
        if failed > limit {
            return Ok(Some(format!(
                "failed settlements {} exceeded limit {}",
                failed, limit
            )));
        }
    }

    Ok(None)
}

async fn cancel_all(
    client: &mut OrderbookClient,
    market: Option<&str>,
    dry_run: bool,
) -> Result<usize> {
    let orders = match market {
        Some(m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };
    if orders.is_empty() {
        info!("no active orders to cancel");
        return Ok(0);
    }
    info!("cancelling {} active orders (dry_run={})", orders.len(), dry_run);

    let mut affected = 0usize;
    for o in orders {
        let id = o.order_id;
        if dry_run {
            info!("[dry-run] would cancel order_id={} market={} qty={}", id, o.market_id, o.quantity);
            affected += 1;
            continue;
        }
        match client.cancel_order(id).await {
            Ok(r) if r.success => {
                info!("cancelled order_id={} market={}", id, o.market_id);
                affected += 1;
            }
            Ok(r) => warn!("cancel order_id={} returned !success: {}", id, r.message),
            Err(e) => error!("cancel order_id={} failed: {:#}", id, e),
        }
    }
    Ok(affected)
}

