//! Bracket TP/SL Agent — OCO (one-cancels-other) resting orders
//!
//! Sibling of `agent-tpsl-classic`. Rather than polling price and firing a
//! *market* exit when a threshold is crossed, this variant places **both**
//! take-profit and stop-loss as resting limit orders immediately after
//! entering the position. The exchange decides which one fills; the agent
//! then cancels the other and shuts down.
//!
//! Advantages over the classic (polling) TP/SL:
//! - No missed exits during network hiccups — orders live on the book.
//! - No slippage from the poll cadence — TP/SL trigger at exactly the
//!   configured price.
//! - Symmetric handling of long / short positions.
//!
//! Trade-offs:
//! - No trailing stop (that would require re-quoting the SL on every peak).
//! - Both orders lock inventory until one fills.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;

use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Clone, ValueEnum)]
enum PositionSide {
    Long,
    Short,
}

#[derive(Parser)]
#[command(name = "agent-tpsl-bracket")]
#[command(about = "Bracket TP/SL — OCO resting orders")]
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
    /// Place bracket TP + SL and wait for one to fill
    Run {
        /// Market ID (e.g. "CC-USDC")
        #[arg(long)]
        market: String,

        /// Position side
        #[arg(long)]
        side: PositionSide,

        /// Quantity to exit
        #[arg(long)]
        quantity: String,

        /// Take-profit price (required)
        #[arg(long)]
        tp: String,

        /// Stop-loss price (required)
        #[arg(long)]
        sl: String,

        /// Poll interval for checking which side of the bracket filled
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        #[arg(long)]
        no_restore: bool,
    },
    /// Show open orders + balances
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_tpsl", "agent_logic", "tx_verifier"],
        "agent-tpsl-bracket",
    );

    let base_config = agent_logic::config::BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            side,
            quantity,
            tp,
            sl,
            poll_secs,
            no_restore,
        } => {
            run_bracket(
                base_config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                side,
                quantity,
                tp,
                sl,
                poll_secs,
            )
            .await
        }
        Commands::Status => run_status(base_config).await,
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
async fn run_bracket(
    config: agent_logic::config::BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market_id: String,
    side: PositionSide,
    quantity: String,
    tp_str: String,
    sl_str: String,
    poll_secs: u64,
) -> Result<()> {
    let qty = Decimal::from_str(&quantity).context("Invalid --quantity")?;
    let tp = Decimal::from_str(&tp_str).context("Invalid --tp")?;
    let sl = Decimal::from_str(&sl_str).context("Invalid --sl")?;
    if qty <= Decimal::ZERO {
        anyhow::bail!("--quantity must be > 0");
    }

    // Sanity checks — TP profits, SL cuts loss
    match side {
        PositionSide::Long => {
            if !(sl < tp) {
                anyhow::bail!("For long positions --sl must be < --tp");
            }
        }
        PositionSide::Short => {
            if !(tp < sl) {
                anyhow::bail!("For short positions --tp must be < --sl");
            }
        }
    }

    let side_label = match side {
        PositionSide::Long => "LONG",
        PositionSide::Short => "SHORT",
    };
    // Exit direction: long exits with OFFER, short exits with BID
    let exit_type = match side {
        PositionSide::Long => OrderType::Offer,
        PositionSide::Short => OrderType::Bid,
    };
    let exit_label = match exit_type {
        OrderType::Offer => "OFFER",
        OrderType::Bid => "BID",
        _ => "OFFER",
    };

    info!("Starting Bracket TP/SL");
    info!("Party: {}", config.party_id);
    info!(
        "market={} side={} qty={} tp={} sl={} poll={}s",
        market_id, side_label, qty, tp, sl, poll_secs
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
    .await
    .context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    let br_shutdown = Arc::new(AtomicBool::new(false));
    let br_config = config.clone();
    let br_shutdown_clone = br_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = bracket_loop(
            br_config,
            market_id,
            exit_type,
            exit_label,
            qty,
            tp,
            sl,
            poll_secs,
            br_shutdown_clone,
        )
        .await
        {
            error!("Bracket loop failed: {:#}", e);
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
            lp_shutdown: Some(agent_logic::shutdown::Shutdown::from_flag(br_shutdown.clone())),
            state_file: Some(PathBuf::from("tpsl-bracket-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn bracket_loop(
    config: agent_logic::config::BaseConfig,
    market_id: String,
    exit_type: OrderType,
    exit_label: &'static str,
    quantity: Decimal,
    tp_price: Decimal,
    sl_price: Decimal,
    poll_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let tick_size = client.get_tick_size(&market_id).await;
    let tp_rounded = agent_logic::tick::round_to_tick(tp_price, tick_size);
    let sl_rounded = agent_logic::tick::round_to_tick(sl_price, tick_size);
    if tp_rounded == sl_rounded {
        anyhow::bail!(
            "After tick rounding TP and SL collapsed to the same price ({}), aborting",
            tp_rounded
        );
    }

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    info!("Placing bracket orders (tick={})", tick_size);

    // Place TP first
    let tp_id = place_and_wait(&mut client, &tracker, &market_id, exit_type, exit_label, tp_rounded, quantity, "TP")
        .await?;
    // Then SL
    let sl_id = place_and_wait(&mut client, &tracker, &market_id, exit_type, exit_label, sl_rounded, quantity, "SL")
        .await?;

    info!("Bracket active — waiting for one leg to fill (TP id={}, SL id={})", tp_id, sl_id);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("Bracket monitor: shutdown");
            break;
        }

        let active = client.get_active_orders(&market_id).await.unwrap_or_default();
        let tp_open = active.iter().any(|o| o.order_id == tp_id);
        let sl_open = active.iter().any(|o| o.order_id == sl_id);

        match (tp_open, sl_open) {
            (true, true) => {
                // Both still resting
            }
            (false, true) => {
                info!("🎯 TP filled (id={}) — cancelling SL (id={})", tp_id, sl_id);
                if let Err(e) = client.cancel_order(sl_id).await {
                    warn!("Failed to cancel SL {}: {:#}", sl_id, e);
                }
                break;
            }
            (true, false) => {
                info!("🛑 SL filled (id={}) — cancelling TP (id={})", sl_id, tp_id);
                if let Err(e) = client.cancel_order(tp_id).await {
                    warn!("Failed to cancel TP {}: {:#}", tp_id, e);
                }
                break;
            }
            (false, false) => {
                info!("Both bracket legs are gone — nothing to reconcile");
                break;
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
    }

    // Let settlement finish, then signal shutdown
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    shutdown.store(true, Ordering::SeqCst);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn place_and_wait(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: Decimal,
    qty: Decimal,
    tag: &'static str,
) -> Result<u64> {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    let resp = client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!(
                "tpsl-bracket-{}-{}",
                tag.to_lowercase(),
                chrono::Utc::now().timestamp_millis()
            )),
            Some(signature),
            signed_data,
            nonce,
        )
        .await
        .with_context(|| format!("submit {} order failed", tag))?;
    let order_id = resp.order.as_ref().map(|o| o.order_id).unwrap_or(0);
    info!("  {} {} @ {} id={}", tag, label, price, order_id);
    Ok(order_id)
}

async fn run_status(config: agent_logic::config::BaseConfig) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut ledger_client = DAppProviderClient::new(
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
    println!();

    let balances = ledger_client.get_balances().await?;
    println!("Balances:");
    for b in &balances {
        println!(
            "  {} — total: {}, locked: {}, unlocked: {}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount
        );
    }
    println!();

    let orders = client.get_all_active_orders().await?;
    if orders.is_empty() {
        println!("No active orders.");
    } else {
        println!("Active orders ({}):", orders.len());
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
    }

    Ok(())
}
