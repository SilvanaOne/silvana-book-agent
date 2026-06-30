//! Block Execution Agent — large private trade with controlled signaling
//!
//! Combines TWAP (time-slicing) and Iceberg (per-slice hidden chunks) to walk
//! a large parent order through the book without leaking its full size:
//!
//! - Parent quantity `--total` is divided into `--time-slices` equal slices.
//! - Each slice runs for `duration_secs / time_slices` seconds.
//! - Within a slice the agent maintains at most `--visible` quantity live on
//!   the book at the configured price. As the visible child order fills (or
//!   leaves the active set), it is replaced with the next chunk until the
//!   slice's quantity is exhausted or the slice's window ends.
//! - When the slice window expires, any remaining quantity rolls into the
//!   next slice — this avoids leaving big chunks on the book at end-of-slice.
//!
//! This is essentially `agent-iceberg-execution` time-bounded per slice, so
//! you can guarantee a worst-case fill schedule while still hiding child
//! order size.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::{OrderStatus, OrderType};


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-block-execution")]
#[command(about = "TWAP × Iceberg hybrid for large block trades")]
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
        #[arg(long)]
        market: String,

        /// "buy" or "sell"
        #[arg(long)]
        side: String,

        /// Total parent quantity
        #[arg(long)]
        total: String,

        /// Limit price for every chunk
        #[arg(long)]
        price: String,

        /// How many time-slices to split the parent into
        #[arg(long)]
        time_slices: u32,

        /// Total duration over which to spread time-slices, in seconds
        #[arg(long)]
        duration_secs: u64,

        /// Maximum quantity visible on the book at any one moment
        #[arg(long)]
        visible: String,

        /// Order status poll interval
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        #[arg(long)]
        no_restore: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_block_execution", "agent_logic", "tx_verifier"],
        "agent-block-execution",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            side,
            total,
            price,
            time_slices,
            duration_secs,
            visible,
            poll_secs,
            no_restore,
        } => {
            run_block(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                market, side, total, price, time_slices, duration_secs, visible, poll_secs,
            )
            .await
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
async fn run_block(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    side: String,
    total: String,
    price: String,
    time_slices: u32,
    duration_secs: u64,
    visible: String,
    poll_secs: u64,
) -> Result<()> {
    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        other => anyhow::bail!("--side must be 'buy' or 'sell', got {}", other),
    };
    if time_slices == 0 {
        anyhow::bail!("--time-slices must be >= 1");
    }
    if duration_secs == 0 {
        anyhow::bail!("--duration-secs must be >= 1");
    }
    let total_dec = Decimal::from_str(&total).context("Invalid --total")?;
    let price_dec = Decimal::from_str(&price).context("Invalid --price")?;
    let visible_dec = Decimal::from_str(&visible).context("Invalid --visible")?;
    if visible_dec > total_dec {
        anyhow::bail!("--visible must be <= --total");
    }
    let slice_qty = total_dec / Decimal::from(time_slices);
    let slice_window = duration_secs / time_slices as u64;

    info!("Starting Block Execution");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} total={} price={} time_slices={} (slice_qty={}, window={}s) visible={}",
        market, side, total_dec, price_dec, time_slices, slice_qty, slice_window, visible_dec
    );

    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    {
        let mut client = OrderbookClient::new(&config).await?;
        if let Ok(markets) = client.get_markets().await {
            for m in &markets {
                let parts: Vec<&str> = m.market_id.split('-').collect();
                if parts.len() == 2 {
                    liquidity_manager.register_alias(parts[0], &m.base_instrument).await;
                    liquidity_manager.register_alias(parts[1], &m.quote_instrument).await;
                }
            }
        }
    }
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
    let loop_cfg = config.clone();
    let loop_sd = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = block_loop(
            loop_cfg, market, order_type, total_dec, price_dec, time_slices, slice_qty,
            slice_window, visible_dec, poll_secs, loop_sd,
        )
        .await
        {
            error!("Block loop failed: {:#}", e);
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
            shutdown: Some(runner_shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(shutdown.clone())),
            state_file: Some(PathBuf::from("block-execution-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn block_loop(
    config: BaseConfig,
    market: String,
    order_type: OrderType,
    total: Decimal,
    price: Decimal,
    time_slices: u32,
    slice_qty: Decimal,
    slice_window: u64,
    visible: Decimal,
    poll_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );
    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Block loop started, label={}", label);

    let mut total_filled = Decimal::ZERO;
    let mut carry_over = Decimal::ZERO;

    for slice in 1..=time_slices {
        if shutdown.load(Ordering::Relaxed) {
            info!("block loop: shutdown");
            return Ok(());
        }
        let slice_target = slice_qty + carry_over;
        let mut slice_filled = Decimal::ZERO;
        carry_over = Decimal::ZERO;
        let slice_start = Instant::now();
        info!(
            "slice {}/{}: target={} window={}s carry_over={}",
            slice, time_slices, slice_target, slice_window, carry_over
        );

        while slice_filled < slice_target {
            if shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            if slice_start.elapsed().as_secs() >= slice_window && slice < time_slices {
                // Roll the unfilled remainder into the next slice rather than slamming it now.
                carry_over = slice_target - slice_filled;
                info!(
                    "slice {} window expired with {} unfilled — rolling to next slice",
                    slice, carry_over
                );
                break;
            }

            let remaining = slice_target - slice_filled;
            let chunk_qty = if remaining < visible { remaining } else { visible };

            let (signature, signed_data, nonce) =
                tracker.sign_order(&market, label, &price.to_string(), &chunk_qty.to_string());
            info!(
                "slice {} chunk: {} {} @ {} (slice_filled {}/{})",
                slice, chunk_qty, label, price, slice_filled, slice_target
            );
            let order_id = match client
                .submit_order(
                    &market,
                    order_type,
                    price.to_string(),
                    chunk_qty.to_string(),
                    Some(format!("block-s{}-{}", slice, chrono::Utc::now().timestamp_millis())),
                    Some(signature),
                    signed_data,
                    nonce,
                )
                .await
            {
                Ok(r) => match r.order.as_ref() {
                    Some(o) => o.order_id,
                    None => {
                        warn!("submit returned no order — waiting before retry");
                        sleep_or_break(poll_secs, &shutdown).await;
                        continue;
                    }
                },
                Err(e) => {
                    warn!("submit chunk failed: {:#}", e);
                    sleep_or_break(poll_secs, &shutdown).await;
                    continue;
                }
            };
            info!("  → order_id={} placed; waiting for it to clear", order_id);

            let actual_filled =
                wait_clear(&mut client, &market, order_id, poll_secs, &shutdown).await;
            slice_filled += actual_filled;
            total_filled += actual_filled;
            info!(
                "  chunk done: filled={} slice_filled={}/{} parent_filled={}/{}",
                actual_filled, slice_filled, slice_target, total_filled, total
            );
        }
    }

    info!("Block execution complete: filled={}/{}", total_filled, total);
    Ok(())
}

async fn wait_clear(
    client: &mut OrderbookClient,
    market: &str,
    order_id: u64,
    poll_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Decimal {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Decimal::ZERO;
        }
        sleep_or_break(poll_secs, shutdown).await;
        let orders = match client.get_active_orders(market).await {
            Ok(o) => o,
            Err(_) => continue,
        };
        match orders.iter().find(|o| o.order_id == order_id) {
            Some(o) => {
                let st = OrderStatus::try_from(o.status).unwrap_or(OrderStatus::Unspecified);
                if !matches!(st, OrderStatus::Active | OrderStatus::Partial) {
                    return Decimal::from_str(&o.filled_quantity).unwrap_or(Decimal::ZERO);
                }
            }
            None => {
                // No longer in active set — treat as cleared with full visible quantity.
                return Decimal::ZERO;
            }
        }
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
