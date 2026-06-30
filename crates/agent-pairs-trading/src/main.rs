//! Pairs Trading Agent — dual-market spread divergence
//!
//! Watches two markets A and B simultaneously and computes their price ratio
//! `r = mid_A / mid_B`. The user supplies the equilibrium target ratio and a
//! divergence threshold (in percent). When the live ratio moves beyond the
//! threshold in either direction, the agent places a single limit order on
//! each leg:
//!
//! - r > target × (1 + threshold/100) → A is rich, B is cheap:
//!     SELL `--quantity-a` of A at mid_A, BUY  `--quantity-b` of B at mid_B
//! - r < target × (1 − threshold/100) → A is cheap, B is rich:
//!     BUY  `--quantity-a` of A at mid_A, SELL `--quantity-b` of B at mid_B
//!
//! At most one open position per direction (i.e. skip when an order in that
//! leg is already live). This is a minimal "stat-arb" skeleton — no rolling
//! covariance, no z-score, no per-side hedge sizing. Use it as a base to
//! extend with a real mean/variance model.

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
#[command(name = "agent-pairs-trading")]
#[command(about = "Trade two markets against each other when their ratio diverges")]
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
        market_a: String,

        #[arg(long)]
        market_b: String,

        /// Equilibrium ratio of mid_A / mid_B
        #[arg(long)]
        target_ratio: f64,

        /// Divergence trigger in percent
        #[arg(long)]
        threshold_pct: f64,

        #[arg(long)]
        quantity_a: String,

        #[arg(long)]
        quantity_b: String,

        #[arg(long, default_value = "30")]
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
        &["agent_pairs_trading", "agent_logic", "tx_verifier"],
        "agent-pairs-trading",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market_a,
            market_b,
            target_ratio,
            threshold_pct,
            quantity_a,
            quantity_b,
            poll_secs,
            no_restore,
        } => {
            run_pairs(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market_a,
                market_b,
                target_ratio,
                threshold_pct,
                quantity_a,
                quantity_b,
                poll_secs,
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
async fn run_pairs(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_a: String,
    market_b: String,
    target_ratio: f64,
    threshold_pct: f64,
    quantity_a: String,
    quantity_b: String,
    poll_secs: u64,
) -> Result<()> {
    if target_ratio <= 0.0 {
        anyhow::bail!("--target-ratio must be > 0");
    }
    if threshold_pct <= 0.0 {
        anyhow::bail!("--threshold-pct must be > 0");
    }
    let qty_a = Decimal::from_str(&quantity_a).context("Invalid --quantity-a")?;
    let qty_b = Decimal::from_str(&quantity_b).context("Invalid --quantity-b")?;
    if qty_a <= Decimal::ZERO || qty_b <= Decimal::ZERO {
        anyhow::bail!("--quantity-a and --quantity-b must be > 0");
    }

    info!("Starting Pairs Trading");
    info!("Party: {}", config.party_id);
    info!(
        "A={} B={} target_ratio={} threshold={}% qty_a={} qty_b={} poll={}s",
        market_a, market_b, target_ratio, threshold_pct, qty_a, qty_b, poll_secs
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
        if let Err(e) = pairs_loop(
            loop_cfg, market_a, market_b, target_ratio, threshold_pct, qty_a, qty_b, poll_secs,
            loop_sd,
        )
        .await
        {
            error!("Pairs loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("pairs-trading-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn pairs_loop(
    config: BaseConfig,
    market_a: String,
    market_b: String,
    target: f64,
    threshold_pct: f64,
    qty_a: Decimal,
    qty_b: Decimal,
    poll_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );
    let threshold = threshold_pct / 100.0;

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Pairs loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let pa = client.get_price(&market_a).await;
        let pb = client.get_price(&market_b).await;
        let (mid_a, mid_b) = match (pa, pb) {
            (Ok(a), Ok(b)) => (mid_value(&a), mid_value(&b)),
            (a, b) => {
                if let Err(e) = a {
                    warn!("get_price({}): {:#}", market_a, e);
                }
                if let Err(e) = b {
                    warn!("get_price({}): {:#}", market_b, e);
                }
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        if mid_a <= 0.0 || mid_b <= 0.0 {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }
        let ratio = mid_a / mid_b;
        let dev = (ratio - target) / target;
        info!(
            "mid_a={:.6} mid_b={:.6} ratio={:.6} target={:.6} dev={:+.4}%",
            mid_a,
            mid_b,
            ratio,
            target,
            dev * 100.0
        );

        let (side_a, label_a, side_b, label_b) = if dev > threshold {
            (OrderType::Offer, "OFFER", OrderType::Bid, "BID") // A rich → sell A, buy B
        } else if dev < -threshold {
            (OrderType::Bid, "BID", OrderType::Offer, "OFFER") // A cheap → buy A, sell B
        } else {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        };

        // skip if an order on either leg is already live in the intended direction
        if has_open(&mut client, &market_a, side_a).await {
            info!("leg A already has open {} — skipping", label_a);
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }
        if has_open(&mut client, &market_b, side_b).await {
            info!("leg B already has open {} — skipping", label_b);
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let price_a_dec = Decimal::from_str(&format!("{}", mid_a)).unwrap_or(Decimal::ZERO);
        let price_b_dec = Decimal::from_str(&format!("{}", mid_b)).unwrap_or(Decimal::ZERO);

        info!(
            "PAIR ENTRY: {} {} {} @ {}  +  {} {} {} @ {}",
            label_a, qty_a, market_a, price_a_dec, label_b, qty_b, market_b, price_b_dec
        );
        place(&mut client, &tracker, &market_a, side_a, label_a, &price_a_dec, &qty_a).await;
        place(&mut client, &tracker, &market_b, side_b, label_b, &price_b_dec, &qty_b).await;

        sleep_or_break(poll_secs, &shutdown).await;
    }
}

async fn has_open(client: &mut OrderbookClient, market: &str, side: OrderType) -> bool {
    match client.get_active_orders(market).await {
        Ok(orders) => orders.iter().any(|o| {
            o.order_type == side as i32
                && (o.status == OrderStatus::Active as i32 || o.status == OrderStatus::Partial as i32)
        }),
        Err(_) => false,
    }
}

async fn place(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: &Decimal,
    qty: &Decimal,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("pair-{}-{}", label, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  {} on {} placed id={}",
            label,
            market,
            resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
        ),
        Err(e) => warn!("  {} on {} submit failed: {:#}", label, market, e),
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

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
