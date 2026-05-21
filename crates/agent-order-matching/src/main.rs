//! Order Matching Agent — snipe orders crossing a configured trigger
//!
//! Subscribes to the orderbook depth stream for a single market and watches
//! the best bid / best offer. When:
//!
//! - `best_offer <= --buy-trigger` → places a BID at `best_offer` for
//!   `--quantity` (we are willing to buy at or below the trigger).
//! - `best_bid  >= --sell-trigger` → places an OFFER at `best_bid` for
//!   `--quantity` (we are willing to sell at or above the trigger).
//!
//! At most one open snipe per direction at a time — if a previous snipe is
//! still open, we skip until it fills or is cancelled.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::order_tracker::OrderTracker;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::{OrderStatus, OrderType, OrderbookLevel, OrderbookUpdate};

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-order-matching")]
#[command(about = "Snipe trades when best bid/offer crosses configured triggers")]
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

        /// Buy when best_offer <= this price (optional — omit to disable buy side)
        #[arg(long)]
        buy_trigger: Option<String>,

        /// Sell when best_bid >= this price (optional)
        #[arg(long)]
        sell_trigger: Option<String>,

        #[arg(long)]
        quantity: String,

        /// Depth subscription window
        #[arg(long, default_value = "5")]
        depth: u32,

        #[arg(long)]
        no_restore: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_order_matching", "orderbook_agent_logic", "tx_verifier"],
        "agent-order-matching",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            buy_trigger,
            sell_trigger,
            quantity,
            depth,
            no_restore,
        } => {
            run_match(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                market,
                buy_trigger,
                sell_trigger,
                quantity,
                depth,
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
async fn run_match(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    market: String,
    buy_trigger: Option<String>,
    sell_trigger: Option<String>,
    quantity: String,
    depth: u32,
) -> Result<()> {
    if buy_trigger.is_none() && sell_trigger.is_none() {
        anyhow::bail!("provide at least one of --buy-trigger or --sell-trigger");
    }
    let buy_trig = buy_trigger
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .context("Invalid --buy-trigger")?;
    let sell_trig = sell_trigger
        .as_ref()
        .map(|s| Decimal::from_str(s))
        .transpose()
        .context("Invalid --sell-trigger")?;
    let qty = Decimal::from_str(&quantity).context("Invalid --quantity")?;

    info!("Starting Order Matching");
    info!("Party: {}", config.party_id);
    info!(
        "market={} buy_trigger={:?} sell_trigger={:?} qty={} depth={}",
        market, buy_trig, sell_trig, qty, depth
    );

    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
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
    let confirm_lock = orderbook_agent_logic::confirm::new_confirm_lock();
    let backend = CloudSettlementBackend::new(
        config.clone(),
        verbose,
        dry_run,
        force,
        confirm,
        confirm_lock,
        liquidity_manager,
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
        if let Err(e) = match_loop(
            loop_cfg, market, buy_trig, sell_trig, qty, depth, loop_sd,
        )
        .await
        {
            error!("Match loop failed: {:#}", e);
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
            shutdown_notify: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: Some(shutdown),
            state_file: Some(PathBuf::from("order-matching-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn match_loop(
    config: BaseConfig,
    market: String,
    buy_trigger: Option<Decimal>,
    sell_trigger: Option<Decimal>,
    quantity: Decimal,
    depth: u32,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let mut stream = client
        .subscribe_orderbook_depth(&market, Some(depth))
        .await
        .with_context(|| format!("subscribe_orderbook_depth({})", market))?;
    info!("depth stream opened for {}", market);

    // Maintain top-of-book between snapshot/delta events
    let mut top_bid: Option<Decimal> = None;
    let mut top_offer: Option<Decimal> = None;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }
        tokio::select! {
            next = stream.next() => {
                match next {
                    Some(Ok(update)) => {
                        let (b, o) = top_of_book(&update);
                        if let Some(b) = b { top_bid = Some(b); }
                        if let Some(o) = o { top_offer = Some(o); }
                        info!("top_of_book bid={:?} offer={:?}", top_bid, top_offer);

                        if let (Some(trig), Some(offer)) = (buy_trigger, top_offer) {
                            if offer <= trig {
                                // Sanity-check no existing open BID, then place
                                if !has_open_in_side(&mut client, &market, OrderType::Bid).await {
                                    fire(&mut client, &tracker, &market, OrderType::Bid, "BID", offer, quantity).await;
                                }
                            }
                        }
                        if let (Some(trig), Some(bid)) = (sell_trigger, top_bid) {
                            if bid >= trig {
                                if !has_open_in_side(&mut client, &market, OrderType::Offer).await {
                                    fire(&mut client, &tracker, &market, OrderType::Offer, "OFFER", bid, quantity).await;
                                }
                            }
                        }
                    }
                    Some(Err(s)) => {
                        error!("depth stream error: {} — exiting", s);
                        return Ok(());
                    }
                    None => {
                        warn!("depth stream ended");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn top_of_book(u: &OrderbookUpdate) -> (Option<Decimal>, Option<Decimal>) {
    // Take the best price from updates if present. For snapshots this is the
    // best bid / best offer; for deltas it's the latest level reported.
    let best_bid = u.bid_updates.iter().filter_map(level_price).max();
    let best_offer = u.offer_updates.iter().filter_map(level_price).min();
    (best_bid, best_offer)
}

fn level_price(l: &OrderbookLevel) -> Option<Decimal> {
    Decimal::from_str(&l.price).ok()
}

async fn has_open_in_side(client: &mut OrderbookClient, market: &str, side: OrderType) -> bool {
    match client.get_active_orders(market).await {
        Ok(orders) => orders.iter().any(|o| {
            o.order_type == side as i32
                && (o.status == OrderStatus::Active as i32
                    || o.status == OrderStatus::Partial as i32)
        }),
        Err(_) => false,
    }
}

async fn fire(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    market: &str,
    order_type: OrderType,
    label: &'static str,
    price: Decimal,
    qty: Decimal,
) {
    let (signature, signed_data, nonce) =
        tracker.sign_order(market, label, &price.to_string(), &qty.to_string());
    info!("SNIPE {}: {} {} @ {}", label, qty, market, price);
    match client
        .submit_order(
            market,
            order_type,
            price.to_string(),
            qty.to_string(),
            Some(format!("match-{}-{}", label, chrono::Utc::now().timestamp_millis())),
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
