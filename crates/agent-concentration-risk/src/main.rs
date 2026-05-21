//! Concentration Risk Prevention Agent
//!
//! Enforcement variant of `agent-risk-exposure`. Each cycle:
//!
//! 1. Values the portfolio in quote currency via balances × live mid for the
//!    configured markets.
//! 2. Computes the share-of-portfolio for each instrument.
//! 3. If any instrument exceeds `--max-share-pct`, cancels the agent's own
//!    open BIDs on that instrument's market (buying more would worsen the
//!    concentration). Symmetric guardrail at `--min-share-pct` cancels OFFERs
//!    on under-concentrated instruments (selling more would deplete it).
//! 4. `--dry-run` only logs what would be cancelled.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-concentration-risk")]
#[command(about = "Enforce per-instrument concentration limits by cancelling own orders")]
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

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        /// Markets used for valuation. The BASE of each market is the controlled instrument.
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        /// Cancel BIDs on the market when share-of-portfolio exceeds this percentage
        #[arg(long, default_value = "50.0")]
        max_share_pct: f64,

        /// Cancel OFFERs on the market when share-of-portfolio is below this percentage
        #[arg(long, default_value = "0.0")]
        min_share_pct: f64,

        #[arg(long, default_value = "60")]
        check_interval: u64,

        #[arg(long)]
        no_restore: bool,
    },
    /// One-off snapshot — print shares + would-be-cancellations and exit
    Snapshot {
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        #[arg(long, default_value = "50.0")]
        max_share_pct: f64,

        #[arg(long, default_value = "0.0")]
        min_share_pct: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_concentration_risk", "orderbook_agent_logic", "tx_verifier"],
        "agent-concentration-risk",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            max_share_pct,
            min_share_pct,
            check_interval,
            no_restore,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            if max_share_pct <= min_share_pct {
                anyhow::bail!("--max-share-pct must be > --min-share-pct");
            }
            run_enforce(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                markets, max_share_pct, min_share_pct, check_interval,
            )
            .await
        }
        Commands::Snapshot {
            markets,
            max_share_pct,
            min_share_pct,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets required");
            }
            let mut ob = OrderbookClient::new(&config).await?;
            let mut ledger = DAppProviderClient::new(
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
            sweep(&mut ob, &mut ledger, &markets, max_share_pct, min_share_pct, true).await?;
            Ok(())
        }
    }
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

#[allow(clippy::too_many_arguments)]
async fn run_enforce(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    markets: Vec<String>,
    max_share_pct: f64,
    min_share_pct: f64,
    check_interval: u64,
) -> Result<()> {
    info!("Starting concentration-risk");
    info!("Party: {}", config.party_id);
    info!(
        "markets={:?} max_share={}% min_share={}% interval={}s dry_run={}",
        markets, max_share_pct, min_share_pct, check_interval, dry_run
    );

    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(
        config.clone(), verbose, dry_run, force, confirm, confirm_lock, liquidity_manager,
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
    let loop_cfg = config.clone();
    let loop_sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = enforce_loop(
            loop_cfg, markets, max_share_pct, min_share_pct, check_interval, dry_run, loop_sd,
        )
        .await
        {
            error!("enforce loop failed: {:#}", e);
        }
    });

    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only: true,
            orders_only: false,
            actionable_count: None,
            shutdown_notify: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(shutdown),
            state_file: Some(PathBuf::from("concentration-risk-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn enforce_loop(
    config: BaseConfig,
    markets: Vec<String>,
    max_share_pct: f64,
    min_share_pct: f64,
    check_interval: u64,
    dry_run: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut ob = OrderbookClient::new(&config).await?;
    let mut ledger = DAppProviderClient::new(
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

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Concentration enforce loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Err(e) = sweep(&mut ob, &mut ledger, &markets, max_share_pct, min_share_pct, dry_run).await {
            warn!("sweep failed: {:#}", e);
        }
        sleep_or_break(check_interval, &shutdown).await;
    }
}

async fn sweep(
    ob: &mut OrderbookClient,
    ledger: &mut DAppProviderClient,
    markets: &[String],
    max_share_pct: f64,
    min_share_pct: f64,
    dry_run: bool,
) -> Result<()> {
    let balances = ledger.get_balances().await?;
    let known = ob.get_markets().await.unwrap_or_default();
    let mut base_to_market: HashMap<String, String> = HashMap::new();
    for m in &known {
        if markets.contains(&m.market_id) {
            base_to_market.insert(m.base_instrument.clone(), m.market_id.clone());
        }
    }

    let mut values: HashMap<String, Decimal> = HashMap::new();
    let mut portfolio = Decimal::ZERO;
    for b in &balances {
        if let Some(market) = base_to_market.get(b.instrument_id.as_str()) {
            let bal = Decimal::from_str(&b.total_amount).unwrap_or(Decimal::ZERO);
            let mid = match ob.get_price(market).await {
                Ok(p) => mid_decimal(&p),
                Err(_) => Decimal::ZERO,
            };
            let value = bal * mid;
            values.insert(b.instrument_id.clone(), value);
            portfolio += value;
        }
    }
    if portfolio <= Decimal::ZERO {
        info!("portfolio_value=0 — nothing to enforce");
        return Ok(());
    }

    let max_share = Decimal::from_str(&format!("{}", max_share_pct / 100.0)).unwrap_or(Decimal::ZERO);
    let min_share = Decimal::from_str(&format!("{}", min_share_pct / 100.0)).unwrap_or(Decimal::ZERO);

    for (instrument, market) in &base_to_market {
        let v = values.get(instrument).copied().unwrap_or(Decimal::ZERO);
        let share = v / portfolio;
        info!(
            "{}@{}: value={} share={:.4} (max={:.4} min={:.4})",
            instrument, market, v, share, max_share, min_share
        );
        let cancel_side: Option<OrderType> = if share > max_share {
            Some(OrderType::Bid) // can't accumulate more
        } else if share < min_share {
            Some(OrderType::Offer) // can't depllete further
        } else {
            None
        };
        let Some(side) = cancel_side else { continue };
        let label = if side == OrderType::Bid { "BID" } else { "OFFER" };
        let active = ob.get_active_orders(market).await.unwrap_or_default();
        let to_cancel: Vec<_> = active
            .into_iter()
            .filter(|o| o.order_type == side as i32)
            .collect();
        if to_cancel.is_empty() {
            info!("  → breach but no own {} orders to cancel", label);
            continue;
        }
        warn!(
            "BREACH on {}@{}: cancelling {} {} orders (dry_run={})",
            instrument, market, to_cancel.len(), label, dry_run
        );
        for o in to_cancel {
            if dry_run {
                info!("  [dry-run] would cancel order_id={}", o.order_id);
                continue;
            }
            match ob.cancel_order(o.order_id).await {
                Ok(r) if r.success => info!("  cancelled order_id={}", o.order_id),
                Ok(r) => warn!("  cancel order_id={} !success: {}", o.order_id, r.message),
                Err(e) => warn!("  cancel order_id={} failed: {:#}", o.order_id, e),
            }
        }
    }

    Ok(())
}

fn mid_decimal(p: &orderbook_proto::pricing::GetPriceResponse) -> Decimal {
    let mid = match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    };
    if mid > 0.0 {
        Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO)
    } else {
        Decimal::ZERO
    }
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
