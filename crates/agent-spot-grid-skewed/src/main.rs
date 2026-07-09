//! Spot Grid Agent — inventory-skewed sizing
//!
//! Sibling of `agent-spot-grid-static` and `agent-spot-grid-geometric` but
//! sizes each level according to current **net inventory**. The grid layout
//! (arithmetic spacing around mid) is the same as the infinite grid; what
//! changes is the per-level quantity:
//!
//! - Compute `net_base` = unlocked balance of the base instrument (long),
//!   converted to a signed quantity around a target holdings level.
//! - Define `skew = clamp(net_base / target, -1, +1)`.
//! - Long inventory (`skew > 0`) → bids shrink by `(1 − alpha·skew)` and
//!   offers grow by `(1 + alpha·skew)`. Short → opposite.
//! - This unwinds inventory faster on the "heavy" side without cancelling
//!   liquidity on the other.
//!
//! `alpha ∈ (0, 1]` is how aggressive the skew is. `alpha = 0` recovers the
//! symmetric grid. `alpha = 1` fully removes one side at maximum imbalance.

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
#[command(name = "agent-spot-grid-skewed")]
#[command(about = "Spot grid whose per-level size is skewed by current inventory")]
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
        /// Market ID (e.g. "CC-USDC")
        #[arg(long)]
        market: String,

        /// Base instrument alias (e.g. "CC"). If omitted, taken from market_id prefix.
        #[arg(long)]
        base_instrument: Option<String>,

        /// Arithmetic spacing between adjacent levels in percent
        #[arg(long)]
        step_pct: f64,

        /// Number of levels per side
        #[arg(long)]
        levels: u32,

        /// Baseline quantity per level (before skew)
        #[arg(long)]
        base_quantity: String,

        /// Target base balance. Skew = clamp((balance - target) / target, -1, +1)
        #[arg(long)]
        target_balance: String,

        /// Skew intensity in [0, 1]. 0 disables skew, 1 fully removes the heavy side at limit.
        #[arg(long, default_value = "0.5")]
        alpha: f64,

        /// How often to poll balance + recompute ladder
        #[arg(long, default_value = "60")]
        refresh_secs: u64,

        #[arg(long)]
        no_restore: bool,
    },
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_spot_grid", "agent_logic", "tx_verifier"],
        "agent-spot-grid-skewed",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            base_instrument,
            step_pct,
            levels,
            base_quantity,
            target_balance,
            alpha,
            refresh_secs,
            no_restore,
        } => {
            run_grid(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                base_instrument,
                step_pct,
                levels,
                base_quantity,
                target_balance,
                alpha,
                refresh_secs,
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

#[allow(clippy::too_many_arguments)]
async fn run_grid(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    base_instrument: Option<String>,
    step_pct: f64,
    levels: u32,
    base_quantity: String,
    target_balance: String,
    alpha: f64,
    refresh_secs: u64,
) -> Result<()> {
    if levels == 0 {
        anyhow::bail!("--levels must be >= 1");
    }
    if step_pct <= 0.0 {
        anyhow::bail!("--step-pct must be > 0");
    }
    if !(0.0..=1.0).contains(&alpha) {
        anyhow::bail!("--alpha must be in [0, 1]");
    }
    let base_qty = Decimal::from_str(&base_quantity).context("Invalid --base-quantity")?;
    let target = Decimal::from_str(&target_balance).context("Invalid --target-balance")?;
    if base_qty <= Decimal::ZERO {
        anyhow::bail!("--base-quantity must be > 0");
    }
    if target <= Decimal::ZERO {
        anyhow::bail!("--target-balance must be > 0");
    }

    let base_alias = base_instrument.unwrap_or_else(|| {
        market
            .split('-')
            .next()
            .unwrap_or(&market)
            .to_string()
    });

    info!("Starting Spot Grid (skewed)");
    info!("Party: {}", config.party_id);
    info!(
        "market={} base={} step_pct={} levels={} base_qty={} target={} alpha={} refresh={}s",
        market, base_alias, step_pct, levels, base_qty, target, alpha, refresh_secs
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
                    liquidity_manager
                        .register_alias(parts[0], &m.base_instrument)
                        .await;
                    liquidity_manager
                        .register_alias(parts[1], &m.quote_instrument)
                        .await;
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
        if let Err(e) = grid_loop(
            loop_cfg,
            market,
            base_alias,
            step_pct,
            levels,
            base_qty,
            target,
            alpha,
            refresh_secs,
            loop_sd,
        )
        .await
        {
            error!("Skewed grid loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("spot-grid-skewed-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn grid_loop(
    config: BaseConfig,
    market: String,
    base_alias: String,
    step_pct: f64,
    levels: u32,
    base_qty: Decimal,
    target: Decimal,
    alpha: f64,
    refresh_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
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

    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let tick_size = client.get_tick_size(&market).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Skewed grid loop started (tick={})", tick_size);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let price = match client.get_price(&market).await {
            Ok(p) => p,
            Err(e) => {
                warn!("get_price failed: {:#}", e);
                sleep_or_break(refresh_secs, &shutdown).await;
                continue;
            }
        };
        let mid = mid_value(&price);
        if mid <= 0.0 {
            sleep_or_break(refresh_secs, &shutdown).await;
            continue;
        }

        // Look up current balance of the base instrument. Balance lookup is a
        // best-effort — if it fails we treat inventory as at-target.
        let balance = match ledger.get_balances().await {
            Ok(bs) => bs
                .into_iter()
                .find(|b| {
                    b.instrument_id.eq_ignore_ascii_case(&base_alias)
                        || b.instrument_id.contains(&base_alias)
                })
                .and_then(|b| Decimal::from_str(&b.unlocked_amount).ok())
                .unwrap_or(target),
            Err(e) => {
                warn!("get_balances failed: {:#} — treating inventory as at-target", e);
                target
            }
        };

        // skew ∈ [-1, +1]; positive = long inventory
        let deviation = balance - target;
        let ratio = deviation / target;
        let skew_f = f64::from_str(&ratio.to_string()).unwrap_or(0.0);
        let skew = skew_f.clamp(-1.0, 1.0);

        // bid multiplier shrinks when long (skew > 0), grows when short
        let bid_mult = (1.0 - alpha * skew).max(0.0);
        let offer_mult = (1.0 + alpha * skew).max(0.0);
        let bid_qty = base_qty * decimal(bid_mult);
        let offer_qty = base_qty * decimal(offer_mult);

        info!(
            "balance={} target={} skew={:+.3} bid_mult={:.3} offer_mult={:.3}",
            balance, target, skew, bid_mult, offer_mult
        );

        // Cancel existing own orders on this market
        if let Ok(orders) = client.get_active_orders(&market).await {
            for o in orders {
                if let Err(e) = client.cancel_order(o.order_id).await {
                    warn!("cancel order_id={}: {:#}", o.order_id, e);
                }
            }
        }

        info!("rebuilding grid: mid={:.6}", mid);
        let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);

        for i in 1..=levels {
            let offset =
                Decimal::from_str(&format!("{}", step_pct * i as f64 / 100.0)).unwrap_or(Decimal::ZERO);
            let bid_price =
                agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE - offset), tick_size);
            let offer_price =
                agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE + offset), tick_size);

            if bid_qty > Decimal::ZERO {
                place(
                    &mut client,
                    &tracker,
                    &market,
                    OrderType::Bid,
                    "BID",
                    &bid_price,
                    &bid_qty,
                    i,
                )
                .await;
            } else {
                info!("  L{} BID skipped (skew fully removed)", i);
            }
            if offer_qty > Decimal::ZERO {
                place(
                    &mut client,
                    &tracker,
                    &market,
                    OrderType::Offer,
                    "OFFER",
                    &offer_price,
                    &offer_qty,
                    i,
                )
                .await;
            } else {
                info!("  L{} OFFER skipped (skew fully removed)", i);
            }
        }

        sleep_or_break(refresh_secs, &shutdown).await;
    }
}

fn decimal(f: f64) -> Decimal {
    Decimal::from_str(&format!("{}", f)).unwrap_or(Decimal::ZERO)
}

#[allow(clippy::too_many_arguments)]
async fn place(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: &Decimal,
    qty: &Decimal,
    level: u32,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!(
                "skew-{}-{}-{}",
                label,
                level,
                chrono::Utc::now().timestamp_millis()
            )),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  L{} {} @ {} qty={} id={}",
            level,
            label,
            price,
            qty,
            resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
        ),
        Err(e) => warn!("  L{} {} submit failed: {:#}", level, label, e),
    }
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
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
    println!("\nActive grid orders ({}):", orders.len());
    for o in &orders {
        println!(
            "  #{} {} {} @ {} qty={}",
            o.order_id,
            if o.order_type == OrderType::Bid as i32 {
                "BID"
            } else {
                "OFFER"
            },
            o.market_id,
            o.price,
            o.quantity
        );
    }
    Ok(())
}
