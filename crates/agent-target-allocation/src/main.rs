//! Target Allocation Agent — absolute-value target balancer
//!
//! Sibling of `agent-portfolio-rebalancing` parameterized by **absolute
//! quote-currency targets** rather than weights. Each `--target` says: keep
//! this instrument valued at this many quote-currency units (e.g. "hold 10
//! USDC worth of Amulet at any time"):
//!
//! ```
//! --target Amulet@CC-USDC=10000   # 10 000 USDC of Amulet via CC-USDC
//! --target CBTC@CBTC-CC=20000     # 20 000 CC of CBTC via CBTC-CC
//! ```
//!
//! Each cycle values each instrument at the live mid for its market and
//! computes the deviation from target. If `|deviation| > --threshold-quote`,
//! places a rebalancing order on that instrument's market for a quantity that
//! closes `--rebalance-fraction` of the deviation.

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

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-target-allocation")]
#[command(about = "Maintain absolute quote-currency value targets per instrument")]
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
        /// Targets in absolute quote value: `INSTRUMENT@MARKET=VALUE_IN_QUOTE`
        #[arg(long, value_parser = parse_target)]
        target: Vec<TargetSpec>,

        /// Don't trade if deviation is below this absolute quote value
        #[arg(long, default_value = "10")]
        threshold_quote: String,

        /// Fraction of the gap to close per cycle (0 < x ≤ 1)
        #[arg(long, default_value = "1.0")]
        rebalance_fraction: f64,

        #[arg(long, default_value = "120")]
        check_interval: u64,

        #[arg(long)]
        no_restore: bool,
    },
    Status,
}

#[derive(Clone, Debug)]
struct TargetSpec {
    instrument: String,
    market: String,
    target_quote_value: Decimal,
}

fn parse_target(s: &str) -> Result<TargetSpec, String> {
    let (lhs, val_str) = s.split_once('=').ok_or_else(|| format!("expected INST@MARKET=VALUE, got {s}"))?;
    let (instrument, market) = lhs.split_once('@').ok_or_else(|| format!("expected INST@MARKET, got {lhs}"))?;
    let value = Decimal::from_str(val_str).map_err(|e| format!("bad value in {s}: {e}"))?;
    if value < Decimal::ZERO {
        return Err(format!("value must be >= 0, got {value}"));
    }
    Ok(TargetSpec {
        instrument: instrument.to_string(),
        market: market.to_string(),
        target_quote_value: value,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_target_allocation", "agent_logic", "tx_verifier"],
        "agent-target-allocation",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            target,
            threshold_quote,
            rebalance_fraction,
            check_interval,
            no_restore,
        } => {
            if target.is_empty() {
                anyhow::bail!("provide at least one --target");
            }
            if !(0.0..=1.0).contains(&rebalance_fraction) || rebalance_fraction == 0.0 {
                anyhow::bail!("--rebalance-fraction must be in (0, 1]");
            }
            let threshold = Decimal::from_str(&threshold_quote).context("Invalid --threshold-quote")?;
            run_alloc(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                target, threshold, rebalance_fraction, check_interval,
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
async fn run_alloc(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    targets: Vec<TargetSpec>,
    threshold_quote: Decimal,
    rebalance_fraction: f64,
    check_interval: u64,
) -> Result<()> {
    info!("Starting Target Allocation");
    info!("Party: {}", config.party_id);
    for t in &targets {
        info!("  target: {}@{} = {} (quote value)", t.instrument, t.market, t.target_quote_value);
    }
    info!("threshold_quote={} fraction={} interval={}s", threshold_quote, rebalance_fraction, check_interval);

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
        if let Err(e) = alloc_loop(
            loop_cfg, targets, threshold_quote,
            Decimal::from_str(&format!("{}", rebalance_fraction)).unwrap_or(Decimal::ONE),
            check_interval, loop_sd,
        ).await {
            error!("alloc loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("target-allocation-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn alloc_loop(
    config: BaseConfig,
    targets: Vec<TargetSpec>,
    threshold_quote: Decimal,
    fraction: Decimal,
    check_interval: u64,
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
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Target allocation loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        let balances = match ledger.get_balances().await {
            Ok(b) => b,
            Err(e) => {
                warn!("get_balances failed: {:#}", e);
                sleep_or_break(check_interval, &shutdown).await;
                continue;
            }
        };
        let by_inst: HashMap<&str, Decimal> = balances
            .iter()
            .map(|b| (b.instrument_id.as_str(), Decimal::from_str(&b.unlocked_amount).unwrap_or(Decimal::ZERO)))
            .collect();

        for t in &targets {
            let mid = match ob.get_price(&t.market).await {
                Ok(p) => mid_decimal(&p),
                Err(e) => { warn!("get_price({}): {:#}", t.market, e); continue; }
            };
            if mid <= Decimal::ZERO {
                continue;
            }
            let bal = by_inst.get(t.instrument.as_str()).copied().unwrap_or(Decimal::ZERO);
            let value = bal * mid;
            let delta_value = value - t.target_quote_value;
            info!(
                "{}@{}: bal={} mid={} value={} target={} delta={:+}",
                t.instrument, t.market, bal, mid, value, t.target_quote_value, delta_value
            );

            if delta_value.abs() <= threshold_quote {
                continue;
            }

            let close_value = delta_value.abs() * fraction;
            let qty = close_value / mid;
            if qty <= Decimal::ZERO {
                continue;
            }
            // Over target → too much instrument → SELL. Under → BUY.
            let (order_type, label) = if delta_value > Decimal::ZERO {
                (OrderType::Offer, "OFFER")
            } else {
                (OrderType::Bid, "BID")
            };
            let price = mid.round_dp(8);
            let (signature, signed_data, nonce) = tracker.sign_order(&t.market, label, &price.to_string(), &qty.to_string());
            info!("REBAL {} {} on {}: qty={} @ {}", label, t.instrument, t.market, qty, price);
            match ob
                .submit_order(
                    &t.market,
                    order_type,
                    price.to_string(),
                    qty.to_string(),
                    Some(format!("alloc-{}-{}", label, chrono::Utc::now().timestamp_millis())),
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
                Err(e) => warn!("  submit failed: {:#}", e),
            }
        }

        sleep_or_break(check_interval, &shutdown).await;
    }
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
            o.market_id, o.price, o.quantity
        );
    }
    Ok(())
}
