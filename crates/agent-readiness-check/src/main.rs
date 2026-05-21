//! Readiness Check Agent — pre-trade gate
//!
//! Before launching a long-running trading agent, run this check to verify:
//! - Required balances are present (`--required <instrument>=<amount>` repeatable)
//! - No stuck pending settlements above a configurable limit
//! - No `FAILED` settlement proposals (or below a limit)
//! - At least one `TransferPreapproval` exists (optional via `--require-preapproval`)
//!
//! Exits with non-zero status if any check fails — wire it into deployment
//! scripts as a hard gate before starting the production agent.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::SettlementStatus;

mod acs_worker;
mod amulet_cache;
mod backend;
mod ledger_client;
mod payment_queue;

use ledger_client::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-readiness-check")]
#[command(about = "Pre-trade gate: balances, preapprovals, stuck settlements")]
struct Cli {
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Check {
        /// Required balance entries, repeat: --required Amulet=50 --required CC-USDC=100
        #[arg(long, value_parser = parse_balance_req)]
        required: Vec<(String, Decimal)>,

        /// Fail if FAILED settlement proposals exceed this many (default 0)
        #[arg(long, default_value = "0")]
        max_failed_settlements: u32,

        /// Fail if PENDING settlement proposals exceed this many (default unlimited)
        #[arg(long)]
        max_pending_settlements: Option<u32>,

        /// Require at least one active TransferPreapproval contract
        #[arg(long)]
        require_preapproval: bool,
    },
}

fn parse_balance_req(s: &str) -> Result<(String, Decimal), String> {
    let (k, v) = s.split_once('=').ok_or_else(|| format!("Expected key=value, got {s}"))?;
    let dec = Decimal::from_str(v).map_err(|e| format!("Invalid amount in {s}: {e}"))?;
    Ok((k.to_string(), dec))
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_readiness_check", "orderbook_agent_logic"],
        "agent-readiness-check",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Check {
            required,
            max_failed_settlements,
            max_pending_settlements,
            require_preapproval,
        } => check(
            config,
            required,
            max_failed_settlements,
            max_pending_settlements,
            require_preapproval,
        )
        .await,
    }
}

async fn check(
    config: BaseConfig,
    required: Vec<(String, Decimal)>,
    max_failed: u32,
    max_pending: Option<u32>,
    require_preapproval: bool,
) -> Result<()> {
    println!("Party: {}", config.party_id);
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

    let mut failures: Vec<String> = Vec::new();

    // Balances
    let balances = ledger.get_balances().await.context("get_balances failed")?;
    let by_instrument: HashMap<&str, Decimal> = balances
        .iter()
        .map(|b| (b.instrument_id.as_str(), Decimal::from_str(&b.unlocked_amount).unwrap_or(Decimal::ZERO)))
        .collect();

    println!("\n=== Balances (unlocked) ===");
    for b in &balances {
        println!(
            "  {} total={} unlocked={} locked={}",
            b.instrument_id, b.total_amount, b.unlocked_amount, b.locked_amount
        );
    }

    println!("\n=== Required balances ===");
    if required.is_empty() {
        println!("  (none specified)");
    }
    for (inst, need) in &required {
        let have = by_instrument.get(inst.as_str()).copied().unwrap_or(Decimal::ZERO);
        let ok = have >= *need;
        let mark = if ok { "OK  " } else { "FAIL" };
        println!("  [{}] {} need={} have={}", mark, inst, need, have);
        if !ok {
            failures.push(format!("balance {}: need {}, have {}", inst, need, have));
        }
    }

    // Settlements
    let proposals = ob.get_pending_proposals().await.context("get_pending_proposals failed")?;
    let pending_count = proposals
        .iter()
        .filter(|p| p.status == SettlementStatus::Pending as i32)
        .count() as u32;
    let failed_count = proposals
        .iter()
        .filter(|p| p.status == SettlementStatus::Failed as i32)
        .count() as u32;
    println!("\n=== Settlements ===");
    println!("  pending={} failed={}", pending_count, failed_count);
    if failed_count > max_failed {
        let mark = "FAIL";
        println!("  [{}] failed_settlements {} > {}", mark, failed_count, max_failed);
        failures.push(format!("failed_settlements {} > {}", failed_count, max_failed));
    } else {
        println!("  [OK  ] failed_settlements {} <= {}", failed_count, max_failed);
    }
    if let Some(limit) = max_pending {
        if pending_count > limit {
            println!("  [FAIL] pending_settlements {} > {}", pending_count, limit);
            failures.push(format!("pending_settlements {} > {}", pending_count, limit));
        } else {
            println!("  [OK  ] pending_settlements {} <= {}", pending_count, limit);
        }
    }

    // Preapproval
    if require_preapproval {
        info!("Querying TransferPreapproval contracts…");
        match ledger.get_preapprovals().await {
            Ok(p) => {
                println!("\n=== TransferPreapproval ===");
                println!("  count={}", p.len());
                if p.is_empty() {
                    println!("  [FAIL] no active TransferPreapproval");
                    failures.push("missing TransferPreapproval".into());
                } else {
                    println!("  [OK  ] at least one active preapproval");
                }
            }
            Err(e) => {
                println!("  [FAIL] get_preapprovals errored: {:#}", e);
                failures.push(format!("get_preapprovals: {e}"));
            }
        }
    }

    println!("\n=== Result ===");
    if failures.is_empty() {
        println!("READY");
        Ok(())
    } else {
        for f in &failures {
            println!("  - {}", f);
        }
        anyhow::bail!("{} check(s) failed", failures.len());
    }
}
