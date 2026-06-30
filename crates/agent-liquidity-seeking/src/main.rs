//! Liquidity-Seeking Execution Agent
//!
//! Like `agent-iceberg-execution`, but the visible chunk size is **adaptive**:
//! each cycle the agent polls `GetOrderbookDepth` and sizes the next child
//! order to the quantity that the top-of-book on the relevant side can absorb
//! without slipping past `--max-slippage-bps` from the live mid. When the book
//! is shallow, child orders shrink; when it's deep, they grow up to
//! `--max-chunk`.
//!
//! For each cycle:
//! 1. Get depth + mid.
//! 2. Walk the offer (buys) or bid (sells) side accumulating qty until VWAP
//!    deviates from mid by more than `--max-slippage-bps`. That's the max
//!    safe size for this cycle.
//! 3. Place a limit order of `min(remaining, max_safe, --max-chunk)` at the
//!    last acceptable price level.
//! 4. Wait for the order to clear, then loop until `--total` is filled.

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
use orderbook_proto::orderbook::{OrderStatus, OrderType, OrderbookLevel};


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-liquidity-seeking")]
#[command(about = "Adaptive execution sized to live orderbook depth")]
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

        #[arg(long)]
        total: String,

        /// Max basis points of VWAP-vs-mid slippage tolerated per cycle
        #[arg(long, default_value = "20")]
        max_slippage_bps: u32,

        /// Hard ceiling on per-child quantity even if depth supports more
        #[arg(long)]
        max_chunk: String,

        /// Depth snapshot depth (number of price levels to walk)
        #[arg(long, default_value = "20")]
        depth: u32,

        /// Status poll interval while waiting for a child to clear
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        /// Abort after this many seconds even if total isn't filled
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
        &["agent_liquidity_seeking", "agent_logic", "tx_verifier"],
        "agent-liquidity-seeking",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            side,
            total,
            max_slippage_bps,
            max_chunk,
            depth,
            poll_secs,
            max_runtime_secs,
            no_restore,
        } => {
            run_seek(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                market, side, total, max_slippage_bps, max_chunk, depth, poll_secs, max_runtime_secs,
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
async fn run_seek(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    side: String,
    total: String,
    max_slippage_bps: u32,
    max_chunk: String,
    depth: u32,
    poll_secs: u64,
    max_runtime_secs: Option<u64>,
) -> Result<()> {
    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        other => anyhow::bail!("--side must be 'buy' or 'sell', got {}", other),
    };
    let total_dec = Decimal::from_str(&total).context("Invalid --total")?;
    let max_chunk_dec = Decimal::from_str(&max_chunk).context("Invalid --max-chunk")?;
    if total_dec <= Decimal::ZERO || max_chunk_dec <= Decimal::ZERO {
        anyhow::bail!("--total and --max-chunk must be > 0");
    }

    info!("Starting Liquidity-Seeking");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} total={} max_slippage_bps={} max_chunk={} depth={} poll={}s max_runtime={:?}",
        market, side, total_dec, max_slippage_bps, max_chunk_dec, depth, poll_secs, max_runtime_secs
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
        if let Err(e) = seek_loop(
            loop_cfg, market, order_type, total_dec, max_slippage_bps, max_chunk_dec,
            depth, poll_secs, max_runtime_secs, loop_sd,
        )
        .await
        {
            error!("Liquidity-seeking loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("liquidity-seeking-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn seek_loop(
    config: BaseConfig,
    market: String,
    order_type: OrderType,
    total: Decimal,
    max_slippage_bps: u32,
    max_chunk: Decimal,
    depth: u32,
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

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Liquidity-seeking loop started, label={}", label);

    let mut filled = Decimal::ZERO;
    let mut cycle: u32 = 0;

    while filled < total {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(limit) = max_runtime_secs {
            if started.elapsed().as_secs() > limit {
                warn!("max_runtime_secs {} reached — filled {}/{}", limit, filled, total);
                return Ok(());
            }
        }

        cycle += 1;
        let dep = match client.get_orderbook_depth(&market, Some(depth)).await? {
            Some(d) => d,
            None => {
                warn!("no depth — sleeping");
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        let mid_dec = match (dep.bids.first().and_then(level_price), dep.offers.first().and_then(level_price)) {
            (Some(b), Some(o)) => Some((b + o) / Decimal::from(2)),
            _ => None,
        };
        let Some(mid) = mid_dec else {
            warn!("no mid — sleeping");
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        };

        // Walk relevant side accumulating qty until VWAP-vs-mid slippage breaches threshold
        let levels: &[OrderbookLevel] = if order_type == OrderType::Bid { &dep.offers } else { &dep.bids };
        let bps_limit = Decimal::from(max_slippage_bps);
        let walk = max_safe_qty(levels, mid, bps_limit, order_type);
        if walk.qty <= Decimal::ZERO {
            info!(
                "cycle {}: no usable depth (mid={} levels_examined={}) — waiting",
                cycle, mid, walk.levels_examined
            );
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }
        let remaining = total - filled;
        let chunk = walk.qty.min(max_chunk).min(remaining);
        if chunk <= Decimal::ZERO {
            info!("cycle {}: chunk computed as 0 — sleeping", cycle);
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }
        let order_price = walk.boundary_price;

        let (signature, signed_data, nonce) =
            tracker.sign_order(&market, label, &order_price.to_string(), &chunk.to_string());
        info!(
            "cycle {}: {} {} @ {} (mid={}, safe_qty={}, max_chunk={}, remaining={})",
            cycle, label, chunk, order_price, mid, walk.qty, max_chunk, remaining
        );
        let order_id = match client
            .submit_order(
                &market,
                order_type,
                order_price.to_string(),
                chunk.to_string(),
                Some(format!("liq-{}-{}", cycle, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(r) => match r.order.as_ref() {
                Some(o) => o.order_id,
                None => {
                    warn!("no order returned — sleeping");
                    sleep_or_break(poll_secs, &shutdown).await;
                    continue;
                }
            },
            Err(e) => {
                warn!("submit failed: {:#}", e);
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        info!("  → order_id={} placed, waiting for it to clear", order_id);

        let actual = wait_clear(&mut client, &market, order_id, poll_secs, &shutdown).await;
        filled += actual;
        info!("  chunk done: filled={} parent_filled={}/{}", actual, filled, total);
    }

    info!("Liquidity-seeking complete: filled={}/{}", filled, total);
    Ok(())
}

struct WalkResult {
    qty: Decimal,
    boundary_price: Decimal,
    levels_examined: u32,
}

fn max_safe_qty(
    levels: &[OrderbookLevel],
    mid: Decimal,
    max_bps: Decimal,
    side: OrderType,
) -> WalkResult {
    let mut accum_qty = Decimal::ZERO;
    let mut accum_cost = Decimal::ZERO;
    let mut last_price = mid;
    let mut examined: u32 = 0;
    let bps_unit = Decimal::from(10000);
    for l in levels {
        examined += 1;
        let lvl_qty = Decimal::from_str(&l.total_quantity).unwrap_or(Decimal::ZERO);
        let lvl_px = Decimal::from_str(&l.price).unwrap_or(Decimal::ZERO);
        if lvl_qty <= Decimal::ZERO || lvl_px <= Decimal::ZERO {
            continue;
        }
        // Tentatively include this level fully and check VWAP
        let cand_qty = accum_qty + lvl_qty;
        let cand_cost = accum_cost + lvl_qty * lvl_px;
        let vwap = cand_cost / cand_qty;
        let dev = match side {
            OrderType::Bid => (vwap - mid),  // walking offers ↑
            OrderType::Offer => (mid - vwap), // walking bids ↓
            _ => Decimal::ZERO,
        };
        let bps = dev.abs() / mid * bps_unit;
        if bps > max_bps {
            // This level pushes us past the slippage budget — return what we accumulated so far
            break;
        }
        accum_qty = cand_qty;
        accum_cost = cand_cost;
        last_price = lvl_px;
    }
    WalkResult {
        qty: accum_qty,
        boundary_price: last_price,
        levels_examined: examined,
    }
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
            None => return Decimal::ZERO,
        }
    }
}

fn level_price(l: &OrderbookLevel) -> Option<Decimal> {
    Decimal::from_str(&l.price).ok()
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
