//! Batch Order Management Agent — bulk submit / bulk cancel CLI
//!
//! Three one-shot commands. None of them is a long-running daemon — they
//! finish, print a summary, and exit.
//!
//! - `submit-batch --file orders.jsonl` — read a JSONL file of orders and
//!   submit each via `OrderbookService.SubmitOrder`. Empty lines and lines
//!   starting with `#` are ignored. Errors are logged but do not stop the
//!   batch (use `--abort-on-error` to opt in to fail-fast).
//! - `cancel-batch [--market <id>] [--side buy|sell]` — fetch own active
//!   orders, filter, cancel each via `CancelOrder`.
//! - `cancel-all` — alias for `cancel-batch` with no filter.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
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
#[command(name = "agent-batch-orders-bulk-cli")]
#[command(about = "Bulk submit / cancel orders")]
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
    /// Submit a JSONL file of orders
    SubmitBatch {
        #[arg(long)]
        file: PathBuf,

        /// Stop on the first failed submission (default: continue)
        #[arg(long)]
        abort_on_error: bool,
    },
    /// Cancel a filtered subset of own active orders
    CancelBatch {
        #[arg(long)]
        market: Option<String>,

        /// Restrict cancel to one side: "buy" or "sell"
        #[arg(long)]
        side: Option<String>,
    },
    /// Cancel every own active order (alias of cancel-batch with no filter)
    CancelAll,
    /// Settlement worker (long-running, optional — needed if batch caused fills)
    Settle {
        #[arg(long)]
        no_restore: bool,
    },
}

#[derive(Deserialize)]
struct OrderSpec {
    market: String,
    side: String,
    quantity: String,
    price: String,
    #[serde(default)]
    r#ref: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_batch_orders", "agent_logic", "tx_verifier"],
        "agent-batch-orders-bulk-cli",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::SubmitBatch { file, abort_on_error } => submit_batch(config, file, abort_on_error).await,
        Commands::CancelBatch { market, side } => cancel_batch(config, market, side).await,
        Commands::CancelAll => cancel_batch(config, None, None).await,
        Commands::Settle { no_restore } => settle(config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore).await,
    }
}

async fn submit_batch(config: BaseConfig, file: PathBuf, abort_on_error: bool) -> Result<()> {
    info!("Submitting batch from {:?}", file);
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let f = tokio::fs::File::open(&file).await.with_context(|| format!("open {:?}", file))?;
    let reader = BufReader::new(f);
    let mut lines = reader.lines();

    let mut accepted = 0u32;
    let mut rejected = 0u32;
    let mut line_no = 0u32;
    while let Some(line) = lines.next_line().await? {
        line_no += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let spec: OrderSpec = match serde_json::from_str(trimmed) {
            Ok(s) => s,
            Err(e) => {
                warn!("line {}: malformed: {} ({})", line_no, e, trimmed);
                rejected += 1;
                if abort_on_error {
                    anyhow::bail!("abort_on_error: malformed line {}", line_no);
                }
                continue;
            }
        };
        let order_type = match spec.side.to_lowercase().as_str() {
            "buy" => OrderType::Bid,
            "sell" => OrderType::Offer,
            other => {
                warn!("line {}: bad side {:?}", line_no, other);
                rejected += 1;
                if abort_on_error {
                    anyhow::bail!("abort_on_error: bad side at line {}", line_no);
                }
                continue;
            }
        };
        let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };
        let (signature, signed_data, nonce) =
            tracker.sign_order(&spec.market, label, &spec.price, &spec.quantity);
        let order_ref = spec
            .r#ref
            .clone()
            .unwrap_or_else(|| format!("batch-{}-{}", line_no, chrono::Utc::now().timestamp_millis()));
        match client
            .submit_order(
                &spec.market,
                order_type,
                spec.price.clone(),
                spec.quantity.clone(),
                Some(order_ref),
                Some(signature),
                signed_data,
                nonce,
            )
            .await
        {
            Ok(resp) => {
                let id = resp.order.as_ref().map(|o| o.order_id).unwrap_or(0);
                info!("line {}: OK id={}", line_no, id);
                accepted += 1;
            }
            Err(e) => {
                error!("line {}: submit failed: {:#}", line_no, e);
                rejected += 1;
                if abort_on_error {
                    anyhow::bail!("abort_on_error: submit failed at line {}", line_no);
                }
            }
        }
    }
    println!("batch done — accepted {}, rejected {}", accepted, rejected);
    if rejected > 0 {
        std::process::exit(2);
    }
    Ok(())
}

async fn cancel_batch(
    config: BaseConfig,
    market: Option<String>,
    side: Option<String>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let orders = match market.as_deref() {
        Some(m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };
    let side_filter: Option<OrderType> = match side.as_deref().map(|s| s.to_lowercase()).as_deref() {
        Some("buy") => Some(OrderType::Bid),
        Some("sell") => Some(OrderType::Offer),
        Some(other) => anyhow::bail!("--side must be 'buy' or 'sell', got {:?}", other),
        None => None,
    };
    let filtered: Vec<_> = orders
        .into_iter()
        .filter(|o| {
            side_filter
                .map(|s| o.order_type == s as i32)
                .unwrap_or(true)
        })
        .collect();
    info!("cancelling {} order(s)", filtered.len());

    let mut cancelled = 0u32;
    let mut failed = 0u32;
    for o in filtered {
        match client.cancel_order(o.order_id).await {
            Ok(r) if r.success => {
                info!("cancelled order_id={} market={}", o.order_id, o.market_id);
                cancelled += 1;
            }
            Ok(r) => {
                warn!("cancel order_id={} !success: {}", o.order_id, r.message);
                failed += 1;
            }
            Err(e) => {
                error!("cancel order_id={} error: {:#}", o.order_id, e);
                failed += 1;
            }
        }
    }
    println!("cancel-batch — cancelled {}, failed {}", cancelled, failed);
    if failed > 0 {
        std::process::exit(2);
    }
    Ok(())
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

async fn settle(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
) -> Result<()> {
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
            state_file: Some(PathBuf::from("batch-orders-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}
