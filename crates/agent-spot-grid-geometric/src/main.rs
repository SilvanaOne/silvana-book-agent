//! Spot Grid Agent — geometric level spacing
//!
//! Sibling of `agent-spot-grid-static` but the ladder is *generated* around the
//! current mid using a **geometric progression** rather than read from static
//! `[[markets.bid_levels]]` blocks. Every refresh cycle:
//!
//! - Level i (1..=N) offset (fraction) = `base_step / 100 × ratio^(i-1)`
//! - Bid   price at level i = `mid × (1 − offset_i)`
//! - Offer price at level i = `mid × (1 + offset_i)`
//!
//! With `ratio > 1` the outer levels get exponentially wider — dense near mid,
//! sparse in the tails. With `ratio == 1` this collapses to an arithmetic grid.
//! Existing own-orders on the market are cancelled and the ladder is re-quoted
//! every `--refresh-secs`.

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
#[command(name = "agent-spot-grid-geometric")]
#[command(about = "Spot grid with geometric level spacing")]
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

        /// Base step for level 1 in percent (e.g. 0.25 = 0.25%)
        #[arg(long)]
        base_step_pct: f64,

        /// Geometric multiplier applied per additional level (>= 1.0)
        #[arg(long, default_value = "1.5")]
        ratio: f64,

        /// Number of levels per side
        #[arg(long)]
        levels: u32,

        /// Quantity per level (uniform across the ladder)
        #[arg(long)]
        quantity_per_level: String,

        /// How often to recompute and re-quote the ladder
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
        "agent-spot-grid-geometric",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            base_step_pct,
            ratio,
            levels,
            quantity_per_level,
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
                base_step_pct,
                ratio,
                levels,
                quantity_per_level,
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
    base_step_pct: f64,
    ratio: f64,
    levels: u32,
    quantity_per_level: String,
    refresh_secs: u64,
) -> Result<()> {
    if levels == 0 {
        anyhow::bail!("--levels must be >= 1");
    }
    if base_step_pct <= 0.0 {
        anyhow::bail!("--base-step-pct must be > 0");
    }
    if ratio < 1.0 {
        anyhow::bail!("--ratio must be >= 1.0 (use 1.0 for arithmetic)");
    }
    let qty = Decimal::from_str(&quantity_per_level).context("Invalid --quantity-per-level")?;
    if qty <= Decimal::ZERO {
        anyhow::bail!("--quantity-per-level must be > 0");
    }

    info!("Starting Spot Grid (geometric)");
    info!("Party: {}", config.party_id);
    info!(
        "market={} base_step_pct={} ratio={} levels={} qty={} refresh={}s",
        market, base_step_pct, ratio, levels, qty, refresh_secs
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
            base_step_pct,
            ratio,
            levels,
            qty,
            refresh_secs,
            loop_sd,
        )
        .await
        {
            error!("Geometric grid loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("spot-grid-geometric-state.json")),
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
    base_step_pct: f64,
    ratio: f64,
    levels: u32,
    quantity: Decimal,
    refresh_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let tick_size = client.get_tick_size(&market).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Geometric grid loop started (tick={})", tick_size);

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

        // Cancel existing own orders on this market before rebuilding
        if let Ok(orders) = client.get_active_orders(&market).await {
            for o in orders {
                if let Err(e) = client.cancel_order(o.order_id).await {
                    warn!("cancel order_id={}: {:#}", o.order_id, e);
                }
            }
        }

        info!("rebuilding grid: mid={:.6}", mid);
        let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);

        // Geometric offsets: level i offset (fraction) = (base_step_pct/100) * ratio^(i-1)
        let mut compounded = base_step_pct / 100.0;
        for i in 1..=levels {
            let offset = Decimal::from_str(&format!("{}", compounded)).unwrap_or(Decimal::ZERO);
            let bid_price =
                agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE - offset), tick_size);
            let offer_price =
                agent_logic::tick::round_to_tick(mid_dec * (Decimal::ONE + offset), tick_size);

            place(
                &mut client,
                &tracker,
                &market,
                OrderType::Bid,
                "BID",
                &bid_price,
                &quantity,
                i,
            )
            .await;
            place(
                &mut client,
                &tracker,
                &market,
                OrderType::Offer,
                "OFFER",
                &offer_price,
                &quantity,
                i,
            )
            .await;

            if i < levels {
                compounded *= ratio;
            }
        }

        sleep_or_break(refresh_secs, &shutdown).await;
    }
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
                "geo-{}-{}-{}",
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
            "  L{} {} @ {} id={}",
            level,
            label,
            price,
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
