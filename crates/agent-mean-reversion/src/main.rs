//! Mean Reversion Agent
//!
//! Tracks the exponential moving average (EMA) of a market's mid price over a
//! rolling window of polls. When price moves more than `--deviation-pct` away
//! from the EMA, it places a single limit order betting on the snap-back:
//!
//! - price > EMA × (1 + deviation/100)  →  SELL  at price = EMA  (price too
//!   high, expect to fall back)
//! - price < EMA × (1 − deviation/100)  →  BUY   at price = EMA  (price too
//!   low, expect to rise)
//!
//! At most one open mean-reversion order per direction at any time (to avoid
//! stacking risk). When that order fills or is cancelled, the agent becomes
//! eligible to signal again.

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
use orderbook_proto::orderbook::{OrderStatus, OrderType};


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-mean-reversion")]
#[command(about = "Mean reversion — trade snap-back when price diverges from EMA")]
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

        /// EMA window in number of poll samples (e.g. 20 = ~10 min at 30s polls)
        #[arg(long, default_value = "20")]
        ema_window: u32,

        /// Trigger when |price - ema| / ema * 100 >= this value (percent)
        #[arg(long, default_value = "1.0")]
        deviation_pct: f64,

        /// Order quantity (base currency) per reversion entry
        #[arg(long)]
        quantity: String,

        /// Price poll interval in seconds
        #[arg(long, default_value = "30")]
        poll_secs: u64,

        /// Number of samples to warm up the EMA before any signal is emitted
        #[arg(long, default_value = "5")]
        warmup_samples: u32,

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
        &["agent_mean_reversion", "agent_logic", "tx_verifier"],
        "agent-mean-reversion",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            ema_window,
            deviation_pct,
            quantity,
            poll_secs,
            warmup_samples,
            no_restore,
        } => {
            run_mr(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                ema_window,
                deviation_pct,
                quantity,
                poll_secs,
                warmup_samples,
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
async fn run_mr(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    ema_window: u32,
    deviation_pct: f64,
    quantity: String,
    poll_secs: u64,
    warmup_samples: u32,
) -> Result<()> {
    if ema_window < 2 {
        anyhow::bail!("--ema-window must be >= 2");
    }
    let qty = Decimal::from_str(&quantity).context("Invalid --quantity")?;

    info!("Starting Mean Reversion agent");
    info!("Party: {}", config.party_id);
    info!(
        "market={} ema_window={} deviation_pct={} qty={} poll_secs={} warmup={}",
        market, ema_window, deviation_pct, qty, poll_secs, warmup_samples
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

    let mr_shutdown = Arc::new(AtomicBool::new(false));
    let loop_config = config.clone();
    let loop_shutdown = mr_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = mr_loop(
            loop_config,
            market,
            ema_window,
            deviation_pct,
            qty,
            poll_secs,
            warmup_samples,
            loop_shutdown,
        )
        .await
        {
            error!("Mean reversion loop failed: {:#}", e);
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
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(mr_shutdown.clone())),
            state_file: Some(PathBuf::from("mean-reversion-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn mr_loop(
    config: BaseConfig,
    market_id: String,
    ema_window: u32,
    deviation_pct: f64,
    quantity: Decimal,
    poll_secs: u64,
    warmup_samples: u32,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    // EMA smoothing factor α = 2 / (N + 1)
    let alpha = 2.0 / (ema_window as f64 + 1.0);
    let mut ema: Option<f64> = None;
    let mut samples: u32 = 0;
    let dev = deviation_pct / 100.0;

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Mean reversion loop started (α={:.4})", alpha);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let price_resp = match client.get_price(&market_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!("get_price failed: {:#}", e);
                sleep_or_break(poll_secs, &shutdown).await;
                continue;
            }
        };
        let mid = mid_value(&price_resp);
        if mid <= 0.0 {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        ema = Some(match ema {
            None => mid,
            Some(prev) => alpha * mid + (1.0 - alpha) * prev,
        });
        samples += 1;
        let ema_val = ema.unwrap();

        if samples < warmup_samples {
            info!(
                "warmup {}/{}: mid={:.6} ema={:.6}",
                samples, warmup_samples, mid, ema_val
            );
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let diff = (mid - ema_val) / ema_val;
        info!(
            "mid={:.6} ema={:.6} diff={:+.4}% (threshold ±{:.2}%)",
            mid,
            ema_val,
            diff * 100.0,
            deviation_pct
        );

        // Direction of signal
        let signal: Option<(OrderType, &'static str)> = if diff > dev {
            Some((OrderType::Offer, "OFFER")) // price too high → sell at EMA
        } else if diff < -dev {
            Some((OrderType::Bid, "BID")) // price too low → buy at EMA
        } else {
            None
        };

        let Some((order_type, label)) = signal else {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        };

        // Skip if we already have an open reversion order in this direction
        if has_open_order(&mut client, &market_id, order_type).await {
            info!("signal {} but an open order already exists — skipping", label);
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let order_price = Decimal::from_str(&format!("{}", ema_val)).unwrap_or(Decimal::ZERO);
        if order_price <= Decimal::ZERO {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let (signature, signed_data, nonce) = tracker.sign_order(
            &market_id,
            label,
            &order_price.to_string(),
            &quantity.to_string(),
        );
        info!(
            "SIGNAL {}: {} {} @ {} (mid={:.6}, ema={:.6}, diff={:+.4}%)",
            label, quantity, market_id, order_price, mid, ema_val, diff * 100.0
        );
        match client
            .submit_order(
                &market_id,
                order_type,
                order_price.to_string(),
                quantity.to_string(),
                Some(format!("mr-{}-{}", label, chrono::Utc::now().timestamp_millis())),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => info!(
                "  → order id={}",
                resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
            ),
            Err(e) => warn!("submit_order failed: {:#}", e),
        }

        sleep_or_break(poll_secs, &shutdown).await;
    }
}

async fn has_open_order(client: &mut OrderbookClient, market: &str, side: OrderType) -> bool {
    match client.get_active_orders(market).await {
        Ok(orders) => orders.iter().any(|o| {
            o.order_type == side as i32
                && (o.status == OrderStatus::Active as i32 || o.status == OrderStatus::Partial as i32)
        }),
        Err(_) => false,
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
