//! Signal Bot — execute trades from a JSONL signal file
//!
//! Tails a JSONL file. Each line is one signal:
//!
//! ```json
//! {"market":"CC-USDC","side":"buy","quantity":"0.5","price":"1.02","ref":"my-strategy-1"}
//! ```
//!
//! Fields:
//! - `market` — market id (required)
//! - `side`   — "buy" | "sell" (required)
//! - `quantity` — base instrument quantity (required, decimal string)
//! - `price` — limit price (required, decimal string)
//! - `ref` — optional trader_order_ref echoed back in the order
//!
//! The bot persists the byte offset it has read in a sibling file
//! `<signals>.cursor` so it doesn't replay signals across restarts.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader, SeekFrom};
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::order_tracker::OrderTracker;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-signal-bot")]
#[command(about = "Read signals from a JSONL file and execute them")]
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
        /// Path to the signals JSONL file. The bot persists its byte offset in
        /// a sibling `<path>.cursor` file.
        #[arg(long)]
        signals_file: PathBuf,

        /// Poll interval for file changes
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        /// Start from the current end of file (skip historical signals)
        #[arg(long)]
        from_end: bool,

        #[arg(long)]
        no_restore: bool,
    },
}

#[derive(Deserialize)]
struct Signal {
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
    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_signal_bot", "orderbook_agent_logic", "tx_verifier"],
        "agent-signal-bot",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            signals_file,
            poll_secs,
            from_end,
            no_restore,
        } => {
            run_bot(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                signals_file,
                poll_secs,
                from_end,
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
async fn run_bot(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    signals_file: PathBuf,
    poll_secs: u64,
    from_end: bool,
) -> Result<()> {
    info!("Starting Signal Bot");
    info!("Party: {}", config.party_id);
    info!(
        "signals_file={:?} poll={}s from_end={} dry_run={}",
        signals_file, poll_secs, from_end, dry_run
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

    let bot_shutdown = Arc::new(AtomicBool::new(false));
    let loop_config = config.clone();
    let loop_shutdown = bot_shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = signal_loop(loop_config, signals_file, poll_secs, from_end, dry_run, loop_shutdown).await {
            error!("Signal loop failed: {:#}", e);
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
            lp_shutdown: Some(bot_shutdown),
            state_file: Some(PathBuf::from("signal-bot-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn signal_loop(
    config: BaseConfig,
    signals_file: PathBuf,
    poll_secs: u64,
    from_end: bool,
    dry_run: bool,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let tracker = OrderTracker::new(
        chrono::Utc::now().timestamp_millis() as u64,
        config.private_key_bytes,
    );

    let cursor_path = signals_file.with_extension({
        let mut s = signals_file
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !s.is_empty() {
            s.push('.');
        }
        s.push_str("cursor");
        s
    });
    let mut offset: u64 = read_cursor(&cursor_path).await.unwrap_or(0);
    if from_end && offset == 0 {
        if let Ok(meta) = tokio::fs::metadata(&signals_file).await {
            offset = meta.len();
            let _ = write_cursor(&cursor_path, offset).await;
            info!("--from-end: starting at offset {}", offset);
        }
    }
    info!(
        "tailing {:?} from offset {} (cursor: {:?})",
        signals_file, offset, cursor_path
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("signal loop: shutdown received");
            return Ok(());
        }

        match read_new_lines(&signals_file, &mut offset).await {
            Ok(lines) => {
                for line in lines {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Signal>(&line) {
                        Ok(sig) => {
                            if let Err(e) = execute(&mut client, &tracker, &sig, dry_run).await {
                                warn!("execute signal failed: {:#}", e);
                            }
                        }
                        Err(e) => warn!("malformed signal {:?}: {}", line, e),
                    }
                }
                if let Err(e) = write_cursor(&cursor_path, offset).await {
                    warn!("cursor write failed: {:#}", e);
                }
            }
            Err(e) => warn!("read signals failed: {:#}", e),
        }

        sleep_or_break(poll_secs, &shutdown).await;
    }
}

async fn execute(
    client: &mut OrderbookClient,
    tracker: &OrderTracker,
    sig: &Signal,
    dry_run: bool,
) -> Result<()> {
    let order_type = match sig.side.to_lowercase().as_str() {
        "buy" => OrderType::Bid,
        "sell" => OrderType::Offer,
        other => anyhow::bail!("unknown side {:?}", other),
    };
    let _ = Decimal::from_str(&sig.quantity).context("invalid quantity")?;
    let _ = Decimal::from_str(&sig.price).context("invalid price")?;

    let label = if order_type == OrderType::Bid { "BID" } else { "OFFER" };
    info!(
        "SIGNAL: {} {} {} @ {} ref={:?}{}",
        label,
        sig.quantity,
        sig.market,
        sig.price,
        sig.r#ref,
        if dry_run { " (dry-run)" } else { "" }
    );
    if dry_run {
        return Ok(());
    }

    let (signature, signed_data, nonce) =
        tracker.sign_order(&sig.market, label, &sig.price, &sig.quantity);
    let resp = client
        .submit_order(
            &sig.market,
            order_type,
            sig.price.clone(),
            sig.quantity.clone(),
            Some(
                sig.r#ref
                    .clone()
                    .unwrap_or_else(|| format!("sig-{}", chrono::Utc::now().timestamp_millis())),
            ),
            Some(signature),
            signed_data,
            nonce,
        )
        .await?;
    info!(
        "  → order id={}",
        resp.order.as_ref().map(|o| o.order_id).unwrap_or(0)
    );
    Ok(())
}

async fn read_new_lines(path: &PathBuf, offset: &mut u64) -> Result<Vec<String>> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(Vec::new());
    }
    let mut file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    // If file was truncated (rotated), restart from beginning.
    if meta.len() < *offset {
        *offset = 0;
    }
    file.seek(SeekFrom::Start(*offset)).await?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            break;
        }
        // Only consider complete lines (terminated with '\n'). If the last
        // chunk has no '\n', leave it for the next poll.
        if !buf.ends_with('\n') {
            break;
        }
        *offset += n as u64;
        lines.push(buf.trim_end_matches('\n').to_string());
    }
    Ok(lines)
}

async fn read_cursor(path: &PathBuf) -> Result<u64> {
    let s = tokio::fs::read_to_string(path).await?;
    Ok(s.trim().parse::<u64>().unwrap_or(0))
}
async fn write_cursor(path: &PathBuf, offset: u64) -> Result<()> {
    tokio::fs::write(path, offset.to_string()).await?;
    Ok(())
}

async fn sleep_or_break(secs: u64, shutdown: &Arc<AtomicBool>) {
    for _ in 0..secs {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
