//! Inventory Risk Prevention Agent
//!
//! Watches the unlocked balance of `--instrument` against a target band
//! `[target − soft_tolerance, target + soft_tolerance]`. Two layers:
//!
//! - **Soft band**: when balance crosses `soft_tolerance` an `inventory.risk`
//!   signal is emitted (suggested hedge direction + size) but no order is
//!   placed.
//! - **Hard band**: when balance crosses `hard_tolerance` and `--auto-hedge`
//!   is set, the agent places an opposite-side limit order on the hedge
//!   market sized to bring balance back to target.
//!
//! Difference vs `agent-hedging`: hedging fires on *every* deviation; this
//! agent has two thresholds, signals first, only forces a hedge at the hard
//! band — appropriate when the directional risk is the symptom of a separate
//! strategy that you'd rather let work itself out under normal conditions.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Mutex as TokioMutex};
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
#[command(name = "agent-inventory-risk")]
#[command(about = "Soft/hard band inventory risk signal + optional auto-hedge")]
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
        instrument: String,

        #[arg(long)]
        hedge_market: String,

        #[arg(long)]
        target_balance: String,

        #[arg(long)]
        soft_tolerance: String,

        #[arg(long)]
        hard_tolerance: String,

        /// Place the corrective order when the hard band is breached
        #[arg(long)]
        auto_hedge: bool,

        #[arg(long, default_value = "60")]
        check_interval: u64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,

        #[arg(long)]
        webhook: Option<String>,

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
        &["agent_inventory_risk", "agent_logic", "tx_verifier"],
        "agent-inventory-risk",
    );
    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            instrument,
            hedge_market,
            target_balance,
            soft_tolerance,
            hard_tolerance,
            auto_hedge,
            check_interval,
            stdout,
            log_file,
            webhook,
            no_restore,
        } => {
            if !stdout && log_file.is_none() && webhook.is_none() {
                anyhow::bail!("provide at least one sink");
            }
            let sinks = Sinks::open(webhook, log_file, stdout).await?;
            run_inv_risk(
                config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore,
                instrument, hedge_market, target_balance, soft_tolerance, hard_tolerance,
                auto_hedge, check_interval, sinks,
            )
            .await
        }
    }
}

struct Sinks {
    webhook: Option<String>,
    http: Option<reqwest::Client>,
    log_file: Option<Mutex<tokio::fs::File>>,
    stdout: bool,
}
impl Sinks {
    async fn open(
        webhook: Option<String>,
        log_file: Option<PathBuf>,
        stdout: bool,
    ) -> Result<Arc<Self>> {
        let http = if webhook.is_some() {
            Some(reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?)
        } else {
            None
        };
        let file = match log_file {
            Some(p) => Some(Mutex::new(
                tokio::fs::OpenOptions::new().create(true).append(true).open(&p).await
                    .with_context(|| format!("open {:?}", p))?,
            )),
            None => None,
        };
        Ok(Arc::new(Self { webhook, http, log_file: file, stdout }))
    }
    async fn dispatch(&self, payload: Value) {
        let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
        if self.stdout {
            println!("{}", line);
        }
        if let Some(file) = &self.log_file {
            let mut f = file.lock().await;
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
        }
        if let (Some(url), Some(http)) = (&self.webhook, &self.http) {
            match http.post(url).json(&payload).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => warn!("webhook returned {}", r.status()),
                Err(e) => warn!("webhook POST failed: {e}"),
            }
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
async fn run_inv_risk(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    instrument: String,
    hedge_market: String,
    target_balance: String,
    soft_tolerance: String,
    hard_tolerance: String,
    auto_hedge: bool,
    check_interval: u64,
    sinks: Arc<Sinks>,
) -> Result<()> {
    let target = Decimal::from_str(&target_balance).context("Invalid --target-balance")?;
    let soft = Decimal::from_str(&soft_tolerance).context("Invalid --soft-tolerance")?;
    let hard = Decimal::from_str(&hard_tolerance).context("Invalid --hard-tolerance")?;
    if soft <= Decimal::ZERO || hard <= Decimal::ZERO || hard <= soft {
        anyhow::bail!("require 0 < --soft-tolerance < --hard-tolerance");
    }

    info!("Starting Inventory Risk Prevention");
    info!("Party: {}", config.party_id);
    info!(
        "instrument={} hedge_market={} target={} soft=±{} hard=±{} auto_hedge={} interval={}s",
        instrument, hedge_market, target, soft, hard, auto_hedge, check_interval
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
        if let Err(e) = inv_risk_loop(
            loop_cfg, instrument, hedge_market, target, soft, hard, auto_hedge, check_interval, sinks, loop_sd,
        )
        .await
        {
            error!("inventory-risk loop failed: {:#}", e);
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
            state_file: Some(PathBuf::from("inventory-risk-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn inv_risk_loop(
    config: BaseConfig,
    instrument: String,
    hedge_market: String,
    target: Decimal,
    soft: Decimal,
    hard: Decimal,
    auto_hedge: bool,
    check_interval: u64,
    sinks: Arc<Sinks>,
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
    info!("inventory-risk loop started");

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
        let bal = balances
            .iter()
            .find(|b| b.instrument_id == instrument)
            .and_then(|b| Decimal::from_str(&b.unlocked_amount).ok())
            .unwrap_or(Decimal::ZERO);
        let delta = bal - target;
        let abs = delta.abs();
        let zone = if abs <= soft {
            "ok"
        } else if abs <= hard {
            "soft_band"
        } else {
            "hard_band"
        };
        info!(
            "balance({}) = {}  target = {}  delta = {:+}  zone = {}",
            instrument, bal, target, delta, zone
        );

        let label = if delta > Decimal::ZERO { "OFFER" } else { "BID" };
        sinks.dispatch(json!({
            "kind": "inventory.risk",
            "ts": Utc::now().to_rfc3339(),
            "instrument": instrument,
            "balance": bal.to_string(),
            "target": target.to_string(),
            "delta": delta.to_string(),
            "zone": zone,
            "suggested_hedge_side": label,
            "suggested_hedge_qty": abs.to_string(),
        })).await;

        if zone == "hard_band" && auto_hedge {
            // Place corrective hedge order to bring balance back to target
            let mid = match ob.get_price(&hedge_market).await {
                Ok(p) => mid_decimal(&p),
                Err(e) => { warn!("get_price({}): {:#}", hedge_market, e); Decimal::ZERO }
            };
            if mid <= Decimal::ZERO {
                warn!("no mid for hedge market — skipping auto-hedge");
            } else {
                let order_type = if delta > Decimal::ZERO { OrderType::Offer } else { OrderType::Bid };
                let qty = abs;
                let (signature, signed_data, nonce) =
                    tracker.sign_order(&hedge_market, label, &mid.to_string(), &qty.to_string());
                warn!(
                    "HARD BAND BREACH — placing hedge: {} {} @ {} on {}",
                    label, qty, mid, hedge_market
                );
                match ob
                    .submit_order(
                        &hedge_market,
                        order_type,
                        mid.to_string(),
                        qty.to_string(),
                        Some(format!("invrisk-{}-{}", label, chrono::Utc::now().timestamp_millis())),
                        Some(signature),
                        signed_data,
                        nonce,
                    )
                    .await
                {
                    Ok(resp) => info!("  hedge order id={}", resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)),
                    Err(e) => warn!("  hedge submit failed: {:#}", e),
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
