//! Example: Buy CC tokens via RFQ on the CC-USDC market.
//!
//! Demonstrates importing `orderbook-cloud-agent` as a library to build
//! a custom agent. This example creates a taker that buys a specified
//! amount of CC, retrying at `--poll-period` intervals until filled.
//!
//! Prerequisites:
//!   - `.env` file with PARTY_AGENT, PARTY_AGENT_PRIVATE_KEY, ORDERBOOK_GRPC_URL, etc.
//!   - `agent.toml` (optional — serde defaults used if missing)
//!
//! Usage:
//!   cargo run -p buy-cc-example -- --amount 10.0 --max-price 1.05
//!   cargo run -p buy-cc-example -- --amount 5.0 --poll-period 300

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

use agent_logic::config::BaseConfig;
use agent_logic::confirm::new_confirm_lock;
use agent_logic::liquidity::LiquidityManager;
use cloud_agent::accept_settle::MulticallSettler;
use cloud_agent::backend::CloudSettlementBackend;
use cloud_agent::fill_loop::{self, FillDirection, FillParams};
use cloud_agent::populate_instruments;

#[derive(Parser)]
#[command(name = "buy-cc")]
#[command(about = "Buy CC tokens via RFQ on the CC-USDC market")]
struct Args {
    /// Total amount of CC to buy
    #[arg(long)]
    amount: f64,

    /// Maximum price per CC in USDC (reject quotes above this)
    #[arg(long)]
    max_price: Option<f64>,

    /// Seconds between retries when no quote is available (default: 10 min)
    #[arg(long, default_value = "600")]
    poll_period: u64,

    /// Minimum settlement size per round
    #[arg(long, default_value = "5.0")]
    min_settlement: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    // Load config from .env + agent.toml (lenient — serde defaults if missing)
    let mut config = BaseConfig::load_or_defaults("agent.toml")
        .context("Failed to load config")?;

    // Populate instrument registry from orderbook-rpc
    populate_instruments(&mut config).await
        .context("Failed to populate instruments")?;

    info!(
        "Buying {} CC on CC-USDC (max_price={}, poll={}s)",
        args.amount,
        args.max_price.map_or("market".to_string(), |p| format!("{:.4}", p)),
        args.poll_period,
    );

    // Create backend (spawns ACS worker to keep amulet cache fresh)
    let confirm_lock = new_confirm_lock();
    let lm = LiquidityManager::new(
        config.fee_reserve_cc,
        config.liquidity_margin,
        config.flow_ema_window_hours,
        config.depletion_max_hours,
        config.depletion_min_hours,
    );
    let backend = CloudSettlementBackend::new(
        config.clone(),
        false, // verbose
        false, // dry_run
        false, // force
        false, // confirm
        confirm_lock.clone(),
        lm,
        agent_logic::shutdown::Shutdown::new(),
    );

    // Create settler for atomic multicall settlements
    let settler = Arc::new(MulticallSettler {
        config: config.clone(),
        amulet_cache: backend.amulet_cache().clone(),
        verbose: false,
        dry_run: false,
        force: false,
        confirm: false,
        confirm_lock,
    });

    let params = FillParams {
        direction: FillDirection::Buy,
        market_id: "CC-USDC".to_string(),
        total_amount: args.amount,
        price_limit: args.max_price,
        min_settlement: args.min_settlement,
        max_settlement: args.amount,
        interval_secs: args.poll_period,
    };

    // Keep backend alive so its ACS worker keeps refreshing amulets
    let _backend_guard = backend;

    fill_loop::run_fill_loop(config, settler, params, None, None).await
}
