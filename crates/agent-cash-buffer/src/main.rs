//! Cash Buffer Agent — keep CC balance within a target band
//!
//! Polls the Canton Coin balance on a schedule. If unlocked CC exceeds
//! `--max-cc`, the excess is transferred to `--sink-party` via `TransferCc`
//! (the agent has approval to PUSH but cannot PULL — so under-balance only
//! logs a warning rather than refilling).
//!
//! Uses the same CIP-0103 two-phase signing pipeline as the other ledger-
//! writing agents (Spot DCA, TP/SL): boilerplate `ledger_client.rs`,
//! `amulet_cache.rs`, `acs_worker.rs`, `payment_queue.rs` are copied as-is.

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest, TokenBalance,
    TransactionOperation, TransferCcParams,
};
use tx_verifier::OperationExpectation;

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use backend::CloudSettlementBackend;
use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-cash-buffer")]
#[command(about = "Maintain Canton Coin balance within a target band")]
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
    /// Run the buffer loop until shutdown
    Run {
        /// Minimum unlocked CC (warn if balance drops below this — agent cannot pull)
        #[arg(long)]
        min_cc: String,

        /// Maximum unlocked CC (push excess to sink party once exceeded)
        #[arg(long)]
        max_cc: String,

        /// Party ID that receives excess CC
        #[arg(long)]
        sink_party: String,

        /// Poll interval in seconds
        #[arg(long, default_value = "60")]
        check_interval: u64,

        /// Optional description placed on the TransferCc record
        #[arg(long, default_value = "cash-buffer-flush")]
        description: String,

        #[arg(long)]
        no_restore: bool,
    },
    /// Print current CC balance
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_cash_buffer", "orderbook_agent_logic", "tx_verifier"],
        "agent-cash-buffer",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            min_cc,
            max_cc,
            sink_party,
            check_interval,
            description,
            no_restore,
        } => {
            run_buffer(
                config,
                cli.verbose,
                cli.dry_run,
                cli.force,
                cli.confirm,
                no_restore,
                min_cc,
                max_cc,
                sink_party,
                check_interval,
                description,
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

async fn run_buffer(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
    min_cc: String,
    max_cc: String,
    sink_party: String,
    check_interval: u64,
    description: String,
) -> Result<()> {
    let min = Decimal::from_str(&min_cc).context("--min-cc not a decimal")?;
    let max = Decimal::from_str(&max_cc).context("--max-cc not a decimal")?;
    if min > max {
        anyhow::bail!("--min-cc must be <= --max-cc");
    }
    let target = (min + max) / Decimal::from(2);

    info!("Starting Cash Buffer agent");
    info!("Party: {}", config.party_id);
    info!("Band: min={} target={} max={}", min, target, max);
    info!("Sink party: {}", sink_party);
    info!("Check interval: {}s  dry_run={}", check_interval, dry_run);

    // Settlement backend + ledger client (same wiring as spot-dca, even though we
    // don't actively settle — run_agent expects a backend).
    let liquidity_manager = orderbook_agent_logic::liquidity::LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
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
    .await
    .context("Failed to create ledger client")?;

    let balance_provider = CloudBalanceProvider {
        client: TokioMutex::new(ledger_client),
    };

    // Background buffer loop. run_agent runs in the foreground for shutdown coordination
    // and settlement-related ledger plumbing.
    let buffer_shutdown = Arc::new(AtomicBool::new(false));
    let loop_config = config.clone();
    let loop_shutdown = buffer_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = buffer_loop(
            loop_config,
            verbose,
            dry_run,
            force,
            confirm,
            min,
            target,
            max,
            sink_party,
            description,
            check_interval,
            loop_shutdown,
        )
        .await
        {
            error!("Buffer loop failed: {:#}", e);
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
            lp_shutdown: Some(buffer_shutdown),
            state_file: Some(PathBuf::from("cash-buffer-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
}

async fn buffer_loop(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    _confirm: bool,
    min: Decimal,
    target: Decimal,
    max: Decimal,
    sink_party: String,
    description: String,
    check_interval: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let mut client = DAppProviderClient::new(
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

    // Give run_agent a few seconds to come up.
    tokio::time::sleep(Duration::from_secs(3)).await;
    info!("Buffer loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("Buffer loop: shutdown received");
            return Ok(());
        }

        match client.get_balances().await {
            Ok(balances) => {
                let cc = cc_unlocked(&balances, config.cc_token_id.as_deref());
                info!("cc_unlocked={} (target band {} ≤ x ≤ {})", cc, min, max);
                if cc < min {
                    warn!(
                        "CC balance {} below floor {} — agent cannot pull, please refill manually",
                        cc, min
                    );
                } else if cc > max {
                    let excess = cc - target;
                    if excess > Decimal::ZERO {
                        info!("flushing excess {} CC → {}", excess, sink_party);
                        if let Err(e) = flush(
                            &mut client,
                            &config,
                            &sink_party,
                            excess.to_string(),
                            description.clone(),
                            verbose,
                            dry_run,
                            force,
                        )
                        .await
                        {
                            error!("flush failed: {:#}", e);
                        }
                    }
                }
            }
            Err(e) => warn!("get_balances failed: {:#}", e),
        }

        for _ in 0..check_interval {
            if shutdown.load(Ordering::Relaxed) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

fn cc_unlocked(balances: &[TokenBalance], cc_token_id: Option<&str>) -> Decimal {
    let amulet_match = |b: &TokenBalance| -> bool {
        if let Some(id) = cc_token_id {
            b.instrument_id == id
        } else {
            // Heuristic: CC tokens are surfaced as "Amulet" by the ledger service.
            b.instrument_id == "Amulet"
        }
    };
    balances
        .iter()
        .find(|b| amulet_match(b))
        .and_then(|b| Decimal::from_str(&b.unlocked_amount).ok())
        .unwrap_or(Decimal::ZERO)
}

async fn flush(
    client: &mut DAppProviderClient,
    config: &BaseConfig,
    receiver: &str,
    amount: String,
    description: String,
    verbose: bool,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let command_id = format!("cash-buffer-{}", chrono::Utc::now().timestamp_millis());
    let expectation = OperationExpectation::TransferCc {
        sender_party: config.party_id.clone(),
        receiver_party: receiver.to_string(),
        amount: amount.clone(),
        command_id: command_id.clone(),
    };
    let req = PrepareTransactionRequest {
        operation: TransactionOperation::TransferCc as i32,
        params: Some(Params::TransferCc(TransferCcParams {
            receiver_party: receiver.to_string(),
            amount,
            description: Some(description),
            command_id,
            settlement_proposal_id: None,
            amulet_cids: vec![],
        })),
        request_signature: None,
    };
    let result = client
        .submit_transaction(req, &expectation, verbose, dry_run, force)
        .await?;
    info!("TransferCc submitted: update_id={}", result.update_id);
    Ok(())
}

async fn run_status(config: BaseConfig) -> Result<()> {
    let mut client = DAppProviderClient::new(
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
    let balances = client.get_balances().await?;
    println!("Party: {}", config.party_id);
    println!("Balances:");
    for b in &balances {
        println!(
            "  {} — total: {}  locked: {}  unlocked: {}",
            b.instrument_id, b.total_amount, b.locked_amount, b.unlocked_amount
        );
    }
    let cc = cc_unlocked(&balances, config.cc_token_id.as_deref());
    println!("\nUnlocked CC: {}", cc);
    Ok(())
}
