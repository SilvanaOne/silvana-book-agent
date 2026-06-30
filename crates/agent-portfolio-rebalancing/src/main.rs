//! Portfolio Rebalancing Agent — weight-based multi-asset balancer
//!
//! Configure a target weight per instrument with `--target inst=weight`
//! (weights are normalized so they don't have to sum to 1). The agent:
//!
//! 1. Fetches unlocked balances per instrument and the live mid for the
//!    configured market (per instrument) → values each balance in the quote
//!    currency.
//! 2. Computes `current_weight = inst_value / portfolio_value` and the
//!    deviation `Δ = current_weight − target_weight`.
//! 3. If `|Δ| > --threshold-pct`, places a market-side order on the configured
//!    market to push the instrument back toward its target — selling when
//!    over-weight, buying when under-weight. Sized so the trade alone closes a
//!    fraction `--rebalance-fraction` of the gap (default 1.0).
//!
//! Targets use a flat `INSTRUMENT@MARKET=WEIGHT` syntax to associate each
//! instrument with the market it should be rebalanced on (e.g.
//! `--target Amulet@CC-USDC=0.4`).

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
#[command(name = "agent-portfolio-rebalancing")]
#[command(about = "Maintain target portfolio weights across multiple instruments")]
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
        /// Targets: repeat for each tracked instrument as `INSTRUMENT@MARKET=WEIGHT`,
        /// e.g. `--target Amulet@CC-USDC=0.4 --target CBTC@CBTC-CC=0.6`.
        #[arg(long, value_parser = parse_target)]
        target: Vec<TargetSpec>,

        /// Trigger rebalance when |Δweight| × 100 exceeds this many percentage points
        #[arg(long, default_value = "2.0")]
        threshold_pct: f64,

        /// Fraction of the deviation to close per cycle (0 < x ≤ 1)
        #[arg(long, default_value = "1.0")]
        rebalance_fraction: f64,

        #[arg(long, default_value = "60")]
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
    weight: f64,
}

fn parse_target(s: &str) -> Result<TargetSpec, String> {
    let (lhs, weight_str) = s.split_once('=').ok_or_else(|| format!("expected INST@MARKET=WEIGHT, got {s}"))?;
    let (instrument, market) = lhs.split_once('@').ok_or_else(|| format!("expected INST@MARKET, got {lhs}"))?;
    let weight: f64 = weight_str.parse().map_err(|e| format!("bad weight in {s}: {e}"))?;
    if weight < 0.0 {
        return Err(format!("weight must be >= 0, got {weight}"));
    }
    Ok(TargetSpec {
        instrument: instrument.to_string(),
        market: market.to_string(),
        weight,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_portfolio_rebalancing", "agent_logic", "tx_verifier"],
        "agent-portfolio-rebalancing",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            target,
            threshold_pct,
            rebalance_fraction,
            check_interval,
            no_restore,
        } => {
            if target.is_empty() {
                anyhow::bail!("provide at least one --target INSTRUMENT@MARKET=WEIGHT");
            }
            if !(0.0..=1.0).contains(&rebalance_fraction) || rebalance_fraction == 0.0 {
                anyhow::bail!("--rebalance-fraction must be in (0, 1]");
            }
            run_balance(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                target,
                threshold_pct,
                rebalance_fraction,
                check_interval,
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
async fn run_balance(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    targets: Vec<TargetSpec>,
    threshold_pct: f64,
    rebalance_fraction: f64,
    check_interval: u64,
) -> Result<()> {
    // Normalize weights
    let sum: f64 = targets.iter().map(|t| t.weight).sum();
    if sum <= 0.0 {
        anyhow::bail!("target weights sum to 0");
    }
    let targets_norm: Vec<TargetSpec> = targets
        .into_iter()
        .map(|t| TargetSpec {
            weight: t.weight / sum,
            ..t
        })
        .collect();

    info!("Starting Portfolio Rebalancing");
    info!("Party: {}", config.party_id);
    for t in &targets_norm {
        info!("  target: {}@{} = {:.4}", t.instrument, t.market, t.weight);
    }
    info!(
        "threshold_pct={} rebalance_fraction={} check_interval={}s",
        threshold_pct, rebalance_fraction, check_interval
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
        if let Err(e) = balance_loop(
            loop_cfg, targets_norm, threshold_pct, rebalance_fraction, check_interval, loop_sd,
        )
        .await
        {
            error!("Rebalance loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("portfolio-rebalance-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn balance_loop(
    config: BaseConfig,
    targets: Vec<TargetSpec>,
    threshold_pct: f64,
    rebalance_fraction: f64,
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

    let threshold = Decimal::from_str(&format!("{}", threshold_pct / 100.0)).unwrap_or(Decimal::ZERO);
    let frac = Decimal::from_str(&format!("{}", rebalance_fraction)).unwrap_or(Decimal::ONE);

    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Rebalance loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Fetch balances + mid prices
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

        let mut mids: HashMap<String, Decimal> = HashMap::new();
        for t in &targets {
            let m = match ob.get_price(&t.market).await {
                Ok(p) => mid_decimal(&p),
                Err(_) => Decimal::ZERO,
            };
            mids.insert(t.market.clone(), m);
        }

        // Portfolio value in quote currency, summed across all targeted instruments
        let mut portfolio_value = Decimal::ZERO;
        let mut values: HashMap<&str, Decimal> = HashMap::new();
        for t in &targets {
            let bal = by_inst.get(t.instrument.as_str()).copied().unwrap_or(Decimal::ZERO);
            let mid = mids.get(&t.market).copied().unwrap_or(Decimal::ZERO);
            let val = bal * mid;
            values.insert(t.instrument.as_str(), val);
            portfolio_value += val;
        }

        info!("portfolio_value={} ({} instruments)", portfolio_value, targets.len());
        if portfolio_value <= Decimal::ZERO {
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }

        for t in &targets {
            let mid = mids.get(&t.market).copied().unwrap_or(Decimal::ZERO);
            if mid <= Decimal::ZERO {
                warn!("{}@{}: no mid — skipping", t.instrument, t.market);
                continue;
            }
            let value = values.get(t.instrument.as_str()).copied().unwrap_or(Decimal::ZERO);
            let current_weight = value / portfolio_value;
            let target_weight = Decimal::from_str(&format!("{}", t.weight)).unwrap_or(Decimal::ZERO);
            let delta_weight = current_weight - target_weight;

            info!(
                "{}@{}: cur={:.4} tgt={:.4} delta={:+.4}",
                t.instrument, t.market, current_weight, target_weight, delta_weight
            );

            if delta_weight.abs() <= threshold {
                continue;
            }

            // Notional to push: fraction of deviation * portfolio_value
            let notional = (delta_weight * frac).abs() * portfolio_value;
            let qty = notional / mid;
            if qty <= Decimal::ZERO {
                continue;
            }

            // Over-weight (delta > 0) → SELL. Under-weight → BUY.
            let (order_type, label) = if delta_weight > Decimal::ZERO {
                (OrderType::Offer, "OFFER")
            } else {
                (OrderType::Bid, "BID")
            };
            info!(
                "REBAL {} {} on {}: qty={} @ mid={}",
                label, t.instrument, t.market, qty, mid
            );
            place(&mut ob, &tracker, &t.market, order_type, label, &mid, &qty).await;
        }

        sleep_or_break(check_interval, &shutdown).await;
    }
}

async fn place(
    ob: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: &Decimal,
    qty: &Decimal,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    match ob
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("rebal-{}-{}", label, chrono::Utc::now().timestamp_millis())),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
    {
        Ok(resp) => info!(
            "  {} placed id={}",
            label,
            resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
        ),
        Err(e) => warn!("  {} submit failed: {:#}", label, e),
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
