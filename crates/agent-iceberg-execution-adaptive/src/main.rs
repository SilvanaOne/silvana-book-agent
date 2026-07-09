//! Iceberg Execution Agent — adaptive visible size driven by fill velocity
//!
//! Same fill-driven iceberg pattern as the hidden-parent sibling — at most one
//! child chunk on the book at a time, and chunk N+1 lands only after chunk N
//! leaves the active book. Unlike the fixed `--visible` sibling, this variant
//! resizes the next chunk based on how fast the previous one cleared:
//!
//!   - elapsed < `--fast-secs`  → next visible = min(current × 2, --max-visible)
//!   - elapsed > `--slow-secs`  → next visible = max(current / 2, --min-visible)
//!   - otherwise unchanged
//!
//! Implementation:
//! 1. Start with `--initial-visible`. Place one chunk at `--price`.
//! 2. Poll the order's status via `get_active_orders` at `--poll-secs`.
//! 3. When the order is no longer ACTIVE/PARTIAL, mark `filled_qty +=
//!    (visible − remaining)`, resize visible from the elapsed window, and
//!    place the next chunk.
//! 4. Stop when `filled_qty >= total`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
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
#[command(name = "agent-iceberg-execution-adaptive")]
#[command(about = "Adaptive iceberg: chunk size follows fill velocity")]
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

        #[arg(long)]
        side: String,

        /// Total parent quantity
        #[arg(long)]
        total: String,

        /// Starting visible quantity for the first chunk
        #[arg(long)]
        initial_visible: String,

        /// Floor for the adaptive resizer
        #[arg(long)]
        min_visible: String,

        /// Ceiling for the adaptive resizer
        #[arg(long)]
        max_visible: String,

        /// Chunks completing in less than this grow next visible (× 2)
        #[arg(long, default_value = "15")]
        fast_secs: u64,

        /// Chunks taking longer than this shrink next visible (÷ 2)
        #[arg(long, default_value = "90")]
        slow_secs: u64,

        /// Limit price for every chunk
        #[arg(long)]
        price: String,

        /// Status poll interval
        #[arg(long, default_value = "10")]
        poll_secs: u64,

        /// Abort if the parent isn't finished after this many seconds
        #[arg(long)]
        max_runtime_secs: Option<u64>,

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
        &["agent_iceberg_execution", "agent_logic", "tx_verifier"],
        "agent-iceberg-execution-adaptive",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            side,
            total,
            initial_visible,
            min_visible,
            max_visible,
            fast_secs,
            slow_secs,
            price,
            poll_secs,
            max_runtime_secs,
            no_restore,
        } => {
            run_iceberg(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                side,
                total,
                initial_visible,
                min_visible,
                max_visible,
                fast_secs,
                slow_secs,
                price,
                poll_secs,
                max_runtime_secs,
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
async fn run_iceberg(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    side: String,
    total: String,
    initial_visible: String,
    min_visible: String,
    max_visible: String,
    fast_secs: u64,
    slow_secs: u64,
    price: String,
    poll_secs: u64,
    max_runtime_secs: Option<u64>,
) -> Result<()> {
    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        other => anyhow::bail!("--side must be 'buy' or 'sell', got {other}"),
    };
    let total_dec = Decimal::from_str(&total).context("Invalid --total")?;
    let initial_dec = Decimal::from_str(&initial_visible).context("Invalid --initial-visible")?;
    let min_dec = Decimal::from_str(&min_visible).context("Invalid --min-visible")?;
    let max_dec = Decimal::from_str(&max_visible).context("Invalid --max-visible")?;
    let price_dec = Decimal::from_str(&price).context("Invalid --price")?;
    if total_dec <= Decimal::ZERO || initial_dec <= Decimal::ZERO {
        anyhow::bail!("--total and --initial-visible must be > 0");
    }
    if min_dec <= Decimal::ZERO || max_dec <= Decimal::ZERO {
        anyhow::bail!("--min-visible and --max-visible must be > 0");
    }
    if min_dec > max_dec {
        anyhow::bail!("--min-visible must be <= --max-visible");
    }
    if initial_dec < min_dec || initial_dec > max_dec {
        anyhow::bail!("--initial-visible must be within [--min-visible, --max-visible]");
    }
    if initial_dec > total_dec {
        anyhow::bail!("--initial-visible must be <= --total");
    }
    if slow_secs <= fast_secs {
        anyhow::bail!("--slow-secs must be > --fast-secs");
    }

    info!("Starting Iceberg Execution — Adaptive");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} total={} initial_visible={} min={} max={} fast={}s slow={}s price={} poll={}s max_runtime={:?}",
        market,
        side,
        total_dec,
        initial_dec,
        min_dec,
        max_dec,
        fast_secs,
        slow_secs,
        price_dec,
        poll_secs,
        max_runtime_secs
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
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
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
        if let Err(e) = iceberg_loop(
            loop_cfg,
            market,
            order_type,
            total_dec,
            initial_dec,
            min_dec,
            max_dec,
            fast_secs,
            slow_secs,
            price_dec,
            poll_secs,
            max_runtime_secs,
            loop_sd,
        )
        .await
        {
            error!("Iceberg loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("iceberg-adaptive-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn iceberg_loop(
    config: BaseConfig,
    market: String,
    order_type: OrderType,
    total: Decimal,
    initial_visible: Decimal,
    min_visible: Decimal,
    max_visible: Decimal,
    fast_secs: u64,
    slow_secs: u64,
    price: Decimal,
    poll_secs: u64,
    max_runtime_secs: Option<u64>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };
    let started = std::time::Instant::now();
    let mut filled = Decimal::ZERO;
    let mut chunk_no: u32 = 0;
    let mut visible = initial_visible;

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!(
        "Adaptive iceberg loop started, label={} initial_visible={}",
        label, visible
    );

    while filled < total {
        if shutdown.load(Ordering::Relaxed) {
            info!("iceberg loop: shutdown");
            return Ok(());
        }
        if let Some(limit) = max_runtime_secs {
            if started.elapsed().as_secs() > limit {
                warn!(
                    "max_runtime_secs {} reached — aborting with filled={}/{}",
                    limit, filled, total
                );
                return Ok(());
            }
        }

        let remaining = total - filled;
        let this_qty = if remaining < visible { remaining } else { visible };

        chunk_no += 1;
        let (signature, signed_data, nonce) =
            tracker.sign_order(&market, label, &price.to_string(), &this_qty.to_string());
        info!(
            "iceberg chunk #{}: {} {} @ {} (visible={} filled {}/{})",
            chunk_no, this_qty, label, price, visible, filled, total
        );
        let chunk_start = std::time::Instant::now();
        let order_id = match client
            .submit_order(
                &market,
                order_type,
                price.to_string(),
                this_qty.to_string(),
                Some(format!("iceadapt-{}-{}", chunk_no, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => match resp.order.as_ref() {
                Some(o) => o.order_id,
                None => {
                    warn!("submit returned no order — skipping chunk and sleeping");
                    sleep_or_break(poll_secs, &shutdown).await;
                    continue;
                }
            },
            Err(e) => {
                warn!("submit chunk #{}: {:#}", chunk_no, e);
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        info!("  → order id={} placed; waiting for fill", order_id);

        // Poll until the order is no longer active
        let chunk_filled =
            wait_for_completion(&mut client, &market, order_id, this_qty, poll_secs, &shutdown).await;
        let elapsed_secs = chunk_start.elapsed().as_secs();
        filled += chunk_filled;
        info!(
            "chunk #{} done: chunk_filled={} elapsed={}s parent_filled={}/{}",
            chunk_no, chunk_filled, elapsed_secs, filled, total
        );

        // Adaptive resize for the NEXT chunk based on this chunk's elapsed time.
        let prev_visible = visible;
        if elapsed_secs < fast_secs {
            let grown = visible * Decimal::from(2);
            visible = if grown > max_visible { max_visible } else { grown };
        } else if elapsed_secs > slow_secs {
            let shrunk = visible / Decimal::from(2);
            visible = if shrunk < min_visible { min_visible } else { shrunk };
        }
        if visible != prev_visible {
            info!(
                "adaptive resize: elapsed={}s → visible {} → {}",
                elapsed_secs, prev_visible, visible
            );
        }

        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
    }

    info!("Adaptive iceberg complete: filled={}", filled);
    Ok(())
}

async fn wait_for_completion(
    client: &mut OrderbookClient,
    market: &str,
    order_id: u64,
    expected_qty: Decimal,
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
            Err(e) => {
                warn!("get_active_orders: {:#}", e);
                continue;
            }
        };
        match orders.iter().find(|o| o.order_id == order_id) {
            Some(o) => {
                let status = OrderStatus::try_from(o.status).unwrap_or(OrderStatus::Unspecified);
                if !matches!(status, OrderStatus::Active | OrderStatus::Partial) {
                    let fq = Decimal::from_str(&o.filled_quantity).unwrap_or(Decimal::ZERO);
                    return fq;
                }
                // still working
            }
            None => {
                // Disappeared from active set — order fully consumed by the matcher and
                // DvP already settled (or was cancelled externally). Return the chunk's
                // expected quantity so the parent-order fill count advances instead of
                // looping forever.
                info!("order_id={} no longer active — chunk of {} treated as fully filled", order_id, expected_qty);
                return expected_qty;
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
