//! TWAP VWAP Execution Agent — volume-weighted slice sizing across time
//!
//! Splits `--total` quantity into `--slices` time slots spaced
//! `duration / slices` seconds apart (same fixed cadence as the Linear
//! sibling). The *size* of each slice is proportional to a normalized
//! volume-curve weight rather than a flat `total / slices`:
//!
//!   slice_i_qty = total × weight_i / Σ weight
//!
//! Weights come from `--volume-curve <preset|comma-list>`. Presets:
//!   - `u-shaped`     — classic U profile (heavy open/close)
//!   - `linear`       — uniform (degenerates to plain TWAP)
//!   - `front-loaded` — monotonically decreasing weights
//! Or a comma list of positive numbers whose length must equal `--slices`.
//!
//! Price rule is identical to Linear: `mid × (1 + price_offset_pct/100)`,
//! rounded to the market's tick size, optionally clamped by `--limit-price`
//! (BIDs skip when price > limit, OFFERs skip when price < limit).

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-twap-vwap")]
#[command(about = "TWAP VWAP execution — volume-weighted slice sizing across time")]
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
    /// Execute one VWAP plan and exit when all slices are placed
    Run {
        #[arg(long)]
        market: String,

        /// "buy" or "sell"
        #[arg(long)]
        side: String,

        /// Total quantity (base currency)
        #[arg(long)]
        total: String,

        /// Number of slices to split into
        #[arg(long)]
        slices: u32,

        /// Total duration across which to spread slices, in seconds
        #[arg(long)]
        duration_secs: u64,

        /// Volume curve: preset (`u-shaped` | `linear` | `front-loaded`) or comma-list of weights
        #[arg(long, default_value = "u-shaped")]
        volume_curve: String,

        /// Optional price offset from mid in percent (e.g. -0.1 = 10 bps below mid for buys)
        #[arg(long, default_value = "0.0")]
        price_offset_pct: f64,

        /// Optional worst-acceptable price: buys skip when computed price > limit, sells when <
        #[arg(long)]
        limit_price: Option<String>,

        #[arg(long)]
        no_restore: bool,
    },
    /// Show party balances and active orders
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_twap", "agent_logic", "tx_verifier"],
        "agent-twap-vwap",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            side,
            total,
            slices,
            duration_secs,
            volume_curve,
            price_offset_pct,
            limit_price,
            no_restore,
        } => {
            run_twap(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                side,
                total,
                slices,
                duration_secs,
                volume_curve,
                price_offset_pct,
                limit_price,
            )
            .await
        }
        Commands::Status => run_status(config).await,
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

/// Build a normalized weight vector (sums to 1.0) of length `slices` from
/// either a named preset or a comma-separated list of positive numbers.
fn build_volume_curve(spec: &str, slices: u32) -> Result<Vec<Decimal>> {
    if slices == 0 {
        anyhow::bail!("cannot build a volume curve with 0 slices");
    }
    let n = slices as usize;
    let raw: Vec<Decimal> = match spec.trim().to_lowercase().as_str() {
        "u-shaped" | "u_shaped" | "u" => {
            // Symmetric parabola centered at (n-1)/2: heavier at both ends.
            // w_i = 1 + a × ((i - c) / c)^2 with a=3 to give a ~4×
            // open/close vs the trough. For n=1 falls back to [1].
            if n == 1 {
                vec![Decimal::ONE]
            } else {
                let center = (n as f64 - 1.0) / 2.0;
                (0..n)
                    .map(|i| {
                        let d = (i as f64 - center) / center.max(1.0);
                        let w = 1.0 + 3.0 * d * d;
                        Decimal::from_str(&format!("{}", w)).unwrap_or(Decimal::ONE)
                    })
                    .collect()
            }
        }
        "linear" | "flat" | "uniform" => vec![Decimal::ONE; n],
        "front-loaded" | "front_loaded" | "front" | "decreasing" => (0..n)
            .map(|i| {
                let w = (n - i) as f64;
                Decimal::from_str(&format!("{}", w)).unwrap_or(Decimal::ONE)
            })
            .collect(),
        other => {
            let parts: Vec<&str> = other
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() != n {
                anyhow::bail!(
                    "--volume-curve length {} does not match --slices {}",
                    parts.len(),
                    n
                );
            }
            let mut out = Vec::with_capacity(n);
            for p in parts {
                let d = Decimal::from_str(p)
                    .with_context(|| format!("--volume-curve weight `{}` is not a number", p))?;
                if d <= Decimal::ZERO {
                    anyhow::bail!("--volume-curve weights must be > 0, got `{}`", p);
                }
                out.push(d);
            }
            out
        }
    };

    let sum: Decimal = raw.iter().copied().sum();
    if sum <= Decimal::ZERO {
        anyhow::bail!("volume curve weights sum to zero");
    }
    Ok(raw.into_iter().map(|w| w / sum).collect())
}

#[allow(clippy::too_many_arguments)]
async fn run_twap(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_id: String,
    side: String,
    total: String,
    slices: u32,
    duration_secs: u64,
    volume_curve: String,
    price_offset_pct: f64,
    limit_price: Option<String>,
) -> Result<()> {
    if slices == 0 {
        anyhow::bail!("--slices must be >= 1");
    }
    if duration_secs == 0 {
        anyhow::bail!("--duration-secs must be >= 1");
    }

    let order_type = match side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        _ => anyhow::bail!("--side must be 'buy' or 'sell'"),
    };
    let total_dec = Decimal::from_str(&total).context("Invalid --total")?;
    let weights = build_volume_curve(&volume_curve, slices)?;
    // Materialize slice sizes now so a bad --total surfaces before any RPC.
    let slice_sizes: Vec<Decimal> = weights.iter().map(|w| total_dec * *w).collect();
    let slice_interval = duration_secs / slices as u64;
    let limit_dec = limit_price
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .context("Invalid --limit-price")?;

    info!("Starting TWAP VWAP agent");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} total={} slices={} curve=`{}` duration={}s interval={}s offset={}% limit={:?}",
        market_id,
        side,
        total_dec,
        slices,
        volume_curve,
        duration_secs,
        slice_interval,
        price_offset_pct,
        limit_dec
    );
    info!(
        "normalized weights = [{}]",
        weights
            .iter()
            .map(|w| format!("{:.4}", w))
            .collect::<Vec<_>>()
            .join(", ")
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
    let shutdown = agent_logic::shutdown::Shutdown::new();
    let backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
        shutdown.clone(),
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

    let twap_shutdown = Arc::new(AtomicBool::new(false));
    let loop_config = config.clone();
    let loop_shutdown = twap_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = twap_loop(
            loop_config,
            market_id,
            order_type,
            slice_sizes,
            slice_interval,
            price_offset_pct,
            limit_dec,
            order_type_label(order_type),
            dry_run,
            loop_shutdown,
        )
        .await
        {
            error!("TWAP VWAP loop failed: {:#}", e);
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
            shutdown: Some(shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(twap_shutdown.clone())),
            state_file: Some(PathBuf::from("twap-vwap-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

fn order_type_label(o: OrderType) -> &'static str {
    match o {
        OrderType::Bid => "BID",
        OrderType::Offer => "OFFER",
        _ => "BID",
    }
}

#[allow(clippy::too_many_arguments)]
async fn twap_loop(
    config: BaseConfig,
    market_id: String,
    order_type: OrderType,
    slice_sizes: Vec<Decimal>,
    slice_interval_secs: u64,
    price_offset_pct: f64,
    limit_price: Option<Decimal>,
    side_label: &'static str,
    dry_run: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );
    let tick_size = client.get_tick_size(&market_id).await;
    let slices = slice_sizes.len() as u32;

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!(
        "TWAP VWAP loop started: {} slices @ {}s apart (tick={})",
        slices, slice_interval_secs, tick_size
    );

    for (idx, slice_size) in slice_sizes.iter().enumerate() {
        let i = (idx as u32) + 1;
        if shutdown.load(Ordering::Relaxed) {
            info!("TWAP VWAP loop: shutdown during execution at slice {}/{}", i, slices);
            return Ok(());
        }

        let price = match client.get_price(&market_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!("get_price failed for slice {}: {:#}", i, e);
                sleep_or_break(slice_interval_secs, &shutdown).await;
                continue;
            }
        };
        let mid = mid_from_price(&price);
        if mid <= Decimal::ZERO {
            warn!("no valid mid price for slice {}, skipping", i);
            sleep_or_break(slice_interval_secs, &shutdown).await;
            continue;
        }

        let raw_price = mid
            * (Decimal::ONE
                + Decimal::from_str(&format!("{}", price_offset_pct / 100.0))
                    .unwrap_or(Decimal::ZERO));
        let order_price = agent_logic::tick::round_to_tick(raw_price, tick_size);

        if let Some(limit) = limit_price {
            let bad = match order_type {
                OrderType::Bid => order_price > limit,
                OrderType::Offer => order_price < limit,
                _ => false,
            };
            if bad {
                warn!(
                    "slice {}/{} skipped: price {} outside limit {} (side={})",
                    i, slices, order_price, limit, side_label
                );
                sleep_or_break(slice_interval_secs, &shutdown).await;
                continue;
            }
        }

        let (signature, signed_data, nonce) = tracker.sign_order(
            &market_id,
            side_label,
            &order_price.to_string(),
            &slice_size.to_string(),
        );

        info!(
            "TWAP VWAP slice {}/{}: {} {} {} @ {} (mid={})",
            i, slices, side_label, slice_size, market_id, order_price, mid
        );

        if dry_run {
            info!("  [dry-run] would submit {} {} @ {}", side_label, slice_size, order_price);
        } else {
            match client
                .submit_order(
                    &market_id,
                    order_type,
                    order_price.to_string(),
                    slice_size.to_string(),
                    Some(format!("twap-vwap-{}-{}", i, chrono::Utc::now().timestamp_millis())),
                    Some(signature),
                    signed_data,
                    nonce,
                )
                .await
            {
                Ok(resp) => info!(
                    "  → order placed id={}",
                    resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
                ),
                Err(e) => warn!("slice {} submit failed: {:#}", i, e),
            }
        }

        if i < slices {
            sleep_or_break(slice_interval_secs, &shutdown).await;
        }
    }

    info!("TWAP VWAP complete: {} slices placed", slices);
    Ok(())
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn mid_from_price(p: &orderbook_proto::pricing::GetPriceResponse) -> Decimal {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => {
            Decimal::from_str(&format!("{}", (b + a) / 2.0)).unwrap_or(Decimal::ZERO)
        }
        _ if p.last > 0.0 => Decimal::from_str(&format!("{}", p.last)).unwrap_or(Decimal::ZERO),
        _ => Decimal::ZERO,
    }
}

async fn run_status(config: BaseConfig) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
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

    println!("Party: {}", config.party_id);
    let balances = ledger.get_balances().await?;
    println!("\nBalances:");
    for b in &balances {
        println!(
            "  {} total={} locked={} unlocked={}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount
        );
    }

    let orders = client.get_all_active_orders().await?;
    println!("\nActive orders ({}):", orders.len());
    for o in &orders {
        println!(
            "  #{} {} {} @ {} qty={}",
            o.order_id,
            if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
            o.market_id,
            o.price,
            o.quantity
        );
    }
    Ok(())
}
