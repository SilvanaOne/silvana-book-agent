//! Spot Grid Agent — minimal grid market-maker
//!
//! Strict subset of `orderbook-cloud-agent`: places passive bid/offer ladders
//! around the mid price for every market listed in `agent.toml`, but does NOT
//! run RFQ handlers or fill loops. Grid management is delegated entirely to the
//! shared `OrderManager` inside `run_agent`.
//!
//! The grid is configured via the same `[[markets]]` / `[[markets.bid_levels]]`
//! / `[[markets.offer_levels]]` blocks as `orderbook-cloud-agent` (see README).

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use agent_logic::runner::{run_agent, AgentOptions, BalanceProvider};
use orderbook_proto::ledger::TokenBalance;
use orderbook_proto::orderbook::OrderType;


use cloud_agent::CloudSettlementBackend;
use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-spot-grid")]
#[command(about = "Minimal spot grid market-maker (no RFQ)")]
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
    /// Run the grid agent until shutdown
    Run {
        #[arg(long)]
        no_restore: bool,
    },
    /// Show balances and active orders
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_spot_grid", "agent_logic", "tx_verifier"],
        "agent-spot-grid",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run { no_restore } => {
            run_grid(config, cli.verbose, cli.dry_run, cli.force, cli.confirm, no_restore).await
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

async fn run_grid(
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    no_restore: bool,
) -> Result<()> {
    let enabled_markets: Vec<&str> = config
        .enabled_markets()
        .iter()
        .map(|m| m.market_id.as_str())
        .collect();
    if enabled_markets.is_empty() {
        anyhow::bail!("agent.toml has no enabled [[markets]] blocks — grid has nothing to do");
    }

    info!("Starting Spot Grid agent");
    info!("Party: {}", config.party_id);
    info!("Enabled markets: {}", enabled_markets.join(", "));

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

    // settlement_only=false + orders_only=false → the shared OrderManager picks up
    // the [[markets]] blocks from agent.toml and runs the grid for us.
    run_agent(
        config,
        backend,
        balance_provider,
        AgentOptions {
            settlement_only: false,
            orders_only: false,
            actionable_count: None,
            shutdown: Some(shutdown.clone()),
            rejected_rfq_trades: None,
            accepted_rfq_trades: None,
            quoted_rfq_trades: None,
            lp_shutdown: None,
            state_file: Some(PathBuf::from("spot-grid-state.json")),
            no_restore,
            fill_state: None,
            no_reject: false,
        },
    )
    .await
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
    println!("\nActive grid orders ({}):", orders.len());
    for o in &orders {
        println!(
            "  #{} {} {} @ {} qty={}",
            o.order_id,
            if o.order_type == OrderType::Bid as i32 { "BID" } else { "OFFER" },
            o.market_id,
            o.price,
            o.quantity
        );
    }
    Ok(())
}
