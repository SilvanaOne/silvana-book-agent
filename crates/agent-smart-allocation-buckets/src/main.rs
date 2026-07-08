//! Smart Allocation Agent — two-level bucket portfolio
//!
//! Generalization of `agent-portfolio-rebalancing`: instruments are grouped
//! into *buckets* (e.g. "stables", "btc-themed", "long-duration"), each
//! bucket has a target weight in the overall portfolio, and inside a bucket
//! each instrument has its own within-bucket weight. Each cycle the agent:
//!
//! 1. Values every instrument at mid.
//! 2. Aggregates per bucket; compares each bucket's current weight vs target.
//! 3. Where the bucket itself is out of band, walks its instruments and places
//!    rebalance orders weighted by the within-bucket targets.
//!
//! Policy TOML:
//!
//! ```toml
//! [[bucket]]
//! name = "stables"
//! weight = 0.5
//!   [[bucket.instrument]]
//!   instrument = "Amulet"
//!   market = "CC-USDC"
//!   weight = 1.0
//!
//! [[bucket]]
//! name = "btc-theme"
//! weight = 0.5
//!   [[bucket.instrument]]
//!   instrument = "CBTC"
//!   market = "CBTC-CC"
//!   weight = 1.0
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
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
#[command(name = "agent-smart-allocation-buckets")]
#[command(about = "Two-level bucket portfolio allocator")]
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
        policy: PathBuf,

        #[arg(long, default_value = "2.0")]
        bucket_threshold_pct: f64,

        #[arg(long, default_value = "0.5")]
        rebalance_fraction: f64,

        #[arg(long, default_value = "300")]
        check_interval: u64,

        #[arg(long)]
        no_restore: bool,
    },
    Check {
        #[arg(long)]
        policy: PathBuf,
    },
}

#[derive(Deserialize, Clone, Debug)]
struct Policy {
    #[serde(rename = "bucket")]
    buckets: Vec<Bucket>,
}

#[derive(Deserialize, Clone, Debug)]
struct Bucket {
    name: String,
    weight: f64,
    #[serde(rename = "instrument", default)]
    instruments: Vec<InstrumentSlot>,
}

#[derive(Deserialize, Clone, Debug)]
struct InstrumentSlot {
    instrument: String,
    market: String,
    weight: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_smart_allocation", "agent_logic", "tx_verifier"],
        "agent-smart-allocation-buckets",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run { policy, bucket_threshold_pct, rebalance_fraction, check_interval, no_restore } => {
            if !(0.0..=1.0).contains(&rebalance_fraction) || rebalance_fraction == 0.0 {
                anyhow::bail!("--rebalance-fraction must be in (0, 1]");
            }
            let pol = load_policy(&policy).await?;
            run_alloc(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                pol, bucket_threshold_pct, rebalance_fraction, check_interval,
            ).await
        }
        Commands::Check { policy } => {
            let p = load_policy(&policy).await?;
            let bucket_sum: f64 = p.buckets.iter().map(|b| b.weight).sum();
            println!("Loaded {} buckets (weight sum = {:.4})", p.buckets.len(), bucket_sum);
            for b in &p.buckets {
                let in_sum: f64 = b.instruments.iter().map(|i| i.weight).sum();
                println!("  bucket {} weight={:.4} instruments={} (within-sum={:.4})", b.name, b.weight, b.instruments.len(), in_sum);
                for i in &b.instruments {
                    println!("    - {}@{} = {:.4}", i.instrument, i.market, i.weight);
                }
            }
            Ok(())
        }
    }
}

async fn load_policy(path: &PathBuf) -> Result<Policy> {
    let body = tokio::fs::read_to_string(path).await.with_context(|| format!("read {:?}", path))?;
    Ok(toml::from_str(&body).with_context(|| format!("parse {:?}", path))?)
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
    policy: Policy,
    bucket_threshold_pct: f64,
    rebalance_fraction: f64,
    check_interval: u64,
) -> Result<()> {
    // Normalize bucket weights
    let bsum: f64 = policy.buckets.iter().map(|b| b.weight).sum();
    if bsum <= 0.0 {
        anyhow::bail!("bucket weights sum to 0");
    }
    let buckets_norm: Vec<Bucket> = policy
        .buckets
        .into_iter()
        .map(|b| {
            let isum: f64 = b.instruments.iter().map(|i| i.weight).sum();
            let inorm: Vec<InstrumentSlot> = b
                .instruments
                .into_iter()
                .map(|i| InstrumentSlot {
                    weight: if isum > 0.0 { i.weight / isum } else { 0.0 },
                    ..i
                })
                .collect();
            Bucket { weight: b.weight / bsum, instruments: inorm, ..b }
        })
        .collect();

    info!("Starting Smart Allocation");
    info!("Party: {}", config.party_id);
    for b in &buckets_norm {
        info!("  bucket {} weight={:.4}", b.name, b.weight);
    }

    let liquidity_manager = agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
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
    let frac = Decimal::from_str(&format!("{}", rebalance_fraction)).unwrap_or(Decimal::ONE);
    let threshold = Decimal::from_str(&format!("{}", bucket_threshold_pct / 100.0)).unwrap_or(Decimal::ZERO);

    tokio::spawn(async move {
        if let Err(e) = alloc_loop(loop_cfg, buckets_norm, threshold, frac, check_interval, loop_sd).await {
            error!("smart-allocation loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("smart-allocation-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn alloc_loop(
    config: BaseConfig,
    buckets: Vec<Bucket>,
    bucket_threshold: Decimal,
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
    info!("smart-allocation loop started");

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

        // Cache mids per market
        let mut mids: HashMap<String, Decimal> = HashMap::new();
        for b in &buckets {
            for ins in &b.instruments {
                if !mids.contains_key(&ins.market) {
                    let m = match ob.get_price(&ins.market).await {
                        Ok(p) => mid_decimal(&p),
                        Err(_) => Decimal::ZERO,
                    };
                    mids.insert(ins.market.clone(), m);
                }
            }
        }

        // Bucket values
        let mut portfolio = Decimal::ZERO;
        let mut bucket_values: HashMap<String, Decimal> = HashMap::new();
        for b in &buckets {
            let mut v = Decimal::ZERO;
            for ins in &b.instruments {
                let bal = by_inst.get(ins.instrument.as_str()).copied().unwrap_or(Decimal::ZERO);
                let mid = mids.get(&ins.market).copied().unwrap_or(Decimal::ZERO);
                v += bal * mid;
            }
            bucket_values.insert(b.name.clone(), v);
            portfolio += v;
        }
        if portfolio <= Decimal::ZERO {
            sleep_or_break(check_interval, &shutdown).await;
            continue;
        }

        for b in &buckets {
            let target_weight = Decimal::from_str(&format!("{}", b.weight)).unwrap_or(Decimal::ZERO);
            let value = bucket_values.get(&b.name).copied().unwrap_or(Decimal::ZERO);
            let current = value / portfolio;
            let delta = current - target_weight;
            info!(
                "bucket {}: cur={:.4} tgt={:.4} delta={:+.4}",
                b.name, current, target_weight, delta
            );
            if delta.abs() <= bucket_threshold {
                continue;
            }
            // Notional to move across the bucket as a whole
            let notional = (delta * fraction).abs() * portfolio;
            // Spread across instruments by within-bucket weight
            for ins in &b.instruments {
                if ins.weight <= 0.0 { continue; }
                let mid = mids.get(&ins.market).copied().unwrap_or(Decimal::ZERO);
                if mid <= Decimal::ZERO { continue; }
                let slot_notional = notional * Decimal::from_str(&format!("{}", ins.weight)).unwrap_or(Decimal::ZERO);
                let qty = slot_notional / mid;
                if qty <= Decimal::ZERO { continue; }
                let (order_type, label) = if delta > Decimal::ZERO {
                    (OrderType::Offer, "OFFER") // bucket overweight → sell its instruments
                } else {
                    (OrderType::Bid, "BID")    // bucket underweight → buy its instruments
                };
                let (signature, signed_data, nonce) =
                    tracker.sign_order(&ins.market, label, &mid.to_string(), &qty.to_string());
                info!(
                    "  REBAL {} {}@{} qty={} (bucket={} delta={:+.4})",
                    label, ins.instrument, ins.market, qty, b.name, delta
                );
                match ob
                    .submit_order(
                        &ins.market,
                        order_type,
                        mid.to_string(),
                        qty.to_string(),
                        Some(format!("smart-{}-{}", b.name, chrono::Utc::now().timestamp_millis())),
                        Some(signature),
                        signed_data,
                        nonce,
                    )
                    .await
                {
                    Ok(resp) => info!("    → id={}", resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)),
                    Err(e) => warn!("    submit failed: {:#}", e),
                }
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
