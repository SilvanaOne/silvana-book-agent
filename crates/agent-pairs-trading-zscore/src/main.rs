//! Pairs Trading Agent — z-score variant
//!
//! Watches two markets A and B simultaneously and computes their price ratio
//! `r = mid_A / mid_B`. Instead of comparing `r` to a fixed target, this agent
//! maintains a rolling window of length `--window` and evaluates:
//!
//! ```text
//! μ = mean(window)
//! σ = sample_stddev(window)
//! z = (r_now − μ) / σ
//! ```
//!
//! Entry (opposite-direction orders per leg):
//!
//! - `z > +entry_z`  → A rich, B cheap:  OFFER `--quantity-a` of A at mid_A,
//!                                       BID   `--quantity-b` of B at mid_B
//! - `z < −entry_z`  → A cheap, B rich:  BID   `--quantity-a` of A at mid_A,
//!                                       OFFER `--quantity-b` of B at mid_B
//!
//! When `|z| ≤ exit_z` the agent clears its in-memory `position_open` flag but
//! does not actively cancel — existing orders live/expire naturally. Warmup is
//! `--warmup-samples` (defaults to `--window`); σ must be > 0 with ≥ 3 samples
//! before any signal is emitted. Skips entry if an open own-order already
//! exists in the intended direction on either leg.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::collections::VecDeque;
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
#[command(name = "agent-pairs-trading-zscore")]
#[command(about = "Trade two markets against each other on rolling-window z-score dislocations")]
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

        /// Rolling-window size for μ and σ of the ratio
        #[arg(long, default_value = "60")]
        window: usize,

        /// Entry threshold: enter when |z| > entry_z
        #[arg(long, default_value = "2.0")]
        entry_z: f64,

        /// Exit threshold: below |z| ≤ exit_z the agent won't open new pairs
        #[arg(long, default_value = "0.5")]
        exit_z: f64,

        #[arg(long)]
        quantity_a: String,

        #[arg(long)]
        quantity_b: String,

        #[arg(long, default_value = "30")]
        poll_secs: u64,

        /// Minimum window fill required before any signal (defaults to --window)
        #[arg(long)]
        warmup_samples: Option<usize>,

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
        "agent-pairs-trading-zscore",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market_a,
            market_b,
            window,
            entry_z,
            exit_z,
            quantity_a,
            quantity_b,
            poll_secs,
            warmup_samples,
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
                window,
                entry_z,
                exit_z,
                quantity_a,
                quantity_b,
                poll_secs,
                warmup_samples,
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
    window: usize,
    entry_z: f64,
    exit_z: f64,
    quantity_a: String,
    quantity_b: String,
    poll_secs: u64,
    warmup_samples: Option<usize>,
) -> Result<()> {
    if window < 3 {
        anyhow::bail!("--window must be >= 3");
    }
    if entry_z <= 0.0 {
        anyhow::bail!("--entry-z must be > 0");
    }
    if exit_z < 0.0 {
        anyhow::bail!("--exit-z must be >= 0");
    }
    if exit_z >= entry_z {
        anyhow::bail!("--exit-z must be < --entry-z");
    }
    let qty_a = Decimal::from_str(&quantity_a).context("Invalid --quantity-a")?;
    let qty_b = Decimal::from_str(&quantity_b).context("Invalid --quantity-b")?;
    if qty_a <= Decimal::ZERO || qty_b <= Decimal::ZERO {
        anyhow::bail!("--quantity-a and --quantity-b must be > 0");
    }
    let warmup = warmup_samples.unwrap_or(window).max(3);

    info!("Starting Pairs Trading (Z-Score)");
    info!("Party: {}", config.party_id);
    info!(
        "A={} B={} window={} entry_z={} exit_z={} qty_a={} qty_b={} poll={}s warmup={}",
        market_a, market_b, window, entry_z, exit_z, qty_a, qty_b, poll_secs, warmup
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
            loop_cfg, market_a, market_b, window, entry_z, exit_z, qty_a, qty_b, poll_secs, warmup,
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
            state_file: Some(PathBuf::from("pairs-trading-zscore-state.json")),
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
    window: usize,
    entry_z: f64,
    exit_z: f64,
    qty_a: Decimal,
    qty_b: Decimal,
    poll_secs: u64,
    warmup: usize,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let mut buf: VecDeque<f64> = VecDeque::with_capacity(window);
    let mut position_open = false;

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Pairs loop (z-score) started");

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

        if buf.len() == window {
            buf.pop_front();
        }
        buf.push_back(ratio);

        if buf.len() < warmup {
            info!(
                "mid_a={:.6} mid_b={:.6} ratio={:.6} samples={}/{} (warming up)",
                mid_a, mid_b, ratio, buf.len(), warmup
            );
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let n = buf.len() as f64;
        let mean: f64 = buf.iter().sum::<f64>() / n;
        let var: f64 = buf.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let sigma = var.sqrt();

        if !(sigma > 0.0) || buf.len() < 3 {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let z = (ratio - mean) / sigma;
        info!(
            "mid_a={:.6} mid_b={:.6} ratio={:.6} μ={:.6} σ={:.6} z={:+.3} samples={}",
            mid_a, mid_b, ratio, mean, sigma, z, buf.len()
        );

        if z.abs() <= exit_z {
            if position_open {
                info!("z={:+.3} inside exit band ±{} — clearing position_open flag", z, exit_z);
                position_open = false;
            }
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        if z.abs() < entry_z {
            sleep_or_break(poll_secs, &shutdown).await;
            continue;
        }

        let (side_a, label_a, side_b, label_b) = if z > 0.0 {
            (OrderType::Offer, "OFFER", OrderType::Bid, "BID") // A rich → sell A, buy B
        } else {
            (OrderType::Bid, "BID", OrderType::Offer, "OFFER") // A cheap → buy A, sell B
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
            "PAIR ENTRY (z={:+.3}): {} {} {} @ {}  +  {} {} {} @ {}",
            z, label_a, qty_a, market_a, price_a_dec, label_b, qty_b, market_b, price_b_dec
        );
        place(&mut client, &tracker, &market_a, side_a, label_a, &price_a_dec, &qty_a).await;
        place(&mut client, &tracker, &market_b, side_b, label_b, &price_b_dec, &qty_b).await;
        position_open = true;

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
            Some(format!("pair-z-{}-{}", label, chrono::Utc::now().timestamp_millis())),
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
