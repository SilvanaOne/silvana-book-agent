//! Test Run Agent — pre-flight healthcheck for the orderbook stack
//!
//! Read-only validation. Exercises every gRPC service the trading agents
//! depend on (Orderbook + Pricing via JWT, plus optionally a specific market)
//! and reports per-check pass/fail. Intended as a quick sanity check before
//! running a ledger-writing agent (`agent-spot-grid`, `agent-spot-dca`, etc.):
//! if `agent-test-run` fails, the others will too.
//!
//! NOTE: Two-phase `PrepareTransaction` dry-runs are already covered by the
//! `--dry-run` flag on each ledger-writing agent (spot-dca, tpsl, cash-buffer,
//! cloud-agent, etc.). This binary stays focused on the read-only RPCs that
//! every agent shares.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-test-run")]
#[command(about = "Pre-flight healthcheck for the Silvana orderbook stack")]
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
    /// Run all checks and exit non-zero if any fail
    Validate {
        /// Specific market to probe with GetPrice + GetOrderbookDepth + GetMarketData
        #[arg(long)]
        market: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_test_run", "agent_logic"],
        "agent-test-run",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Validate { market } => validate(config, market).await,
    }
}

async fn validate(config: BaseConfig, market: Option<String>) -> Result<()> {
    println!("Party:    {}", config.party_id);
    println!("Endpoint: {}", config.orderbook_grpc_url);
    println!("Node:     {}", config.node_name);
    println!();

    let mut report: Vec<(&'static str, Outcome)> = Vec::new();

    // Connection / auth
    let t0 = Instant::now();
    let client_result = OrderbookClient::new(&config).await;
    let mut client = match client_result {
        Ok(c) => {
            report.push(("connect+jwt", Outcome::ok(t0.elapsed())));
            c
        }
        Err(e) => {
            report.push(("connect+jwt", Outcome::fail(t0.elapsed(), format!("{:#}", e))));
            print_report(&report);
            anyhow::bail!("cannot continue without a connection");
        }
    };

    // GetMarkets
    let t0 = Instant::now();
    let markets_count = match client.get_markets().await {
        Ok(m) => {
            report.push(("get_markets", Outcome::detail(t0.elapsed(), format!("{} markets", m.len()))));
            m.len()
        }
        Err(e) => {
            report.push(("get_markets", Outcome::fail(t0.elapsed(), format!("{:#}", e))));
            0
        }
    };

    // GetAllActiveOrders (validates JWT scope works for party-specific queries)
    let t0 = Instant::now();
    match client.get_all_active_orders().await {
        Ok(orders) => report.push(("get_active_orders", Outcome::detail(t0.elapsed(), format!("{} orders", orders.len())))),
        Err(e) => report.push(("get_active_orders", Outcome::fail(t0.elapsed(), format!("{:#}", e)))),
    }

    // GetPendingProposals
    let t0 = Instant::now();
    match client.get_pending_proposals().await {
        Ok(p) => report.push(("get_pending_proposals", Outcome::detail(t0.elapsed(), format!("{} pending", p.len())))),
        Err(e) => report.push(("get_pending_proposals", Outcome::fail(t0.elapsed(), format!("{:#}", e)))),
    }

    // GetMarketData (all)
    let t0 = Instant::now();
    match client.get_market_data(vec![]).await {
        Ok(md) => report.push(("get_market_data", Outcome::detail(t0.elapsed(), format!("{} entries", md.len())))),
        Err(e) => report.push(("get_market_data", Outcome::fail(t0.elapsed(), format!("{:#}", e)))),
    }

    if let Some(m) = market.as_deref() {
        let t0 = Instant::now();
        match client.get_price(m).await {
            Ok(p) => report.push(("get_price", Outcome::detail(t0.elapsed(), format!("last={} src={}", p.last, p.source)))),
            Err(e) => report.push(("get_price", Outcome::fail(t0.elapsed(), format!("{:#}", e)))),
        }
        let t0 = Instant::now();
        match client.get_orderbook_depth(m, Some(5)).await {
            Ok(Some(d)) => report.push(("get_orderbook_depth", Outcome::detail(t0.elapsed(), format!("bids={} offers={}", d.bids.len(), d.offers.len())))),
            Ok(None) => report.push(("get_orderbook_depth", Outcome::detail(t0.elapsed(), "no depth".into()))),
            Err(e) => report.push(("get_orderbook_depth", Outcome::fail(t0.elapsed(), format!("{:#}", e)))),
        }
    } else if markets_count > 0 {
        println!("(no --market provided, skipping per-market checks)\n");
    }

    print_report(&report);
    if report.iter().any(|(_, o)| !o.ok) {
        anyhow::bail!("one or more checks failed");
    }
    println!("\nAll checks passed.");
    Ok(())
}

struct Outcome {
    ok: bool,
    duration_ms: u128,
    detail: String,
}
impl Outcome {
    fn ok(d: std::time::Duration) -> Self {
        Self { ok: true, duration_ms: d.as_millis(), detail: String::new() }
    }
    fn detail(d: std::time::Duration, msg: String) -> Self {
        Self { ok: true, duration_ms: d.as_millis(), detail: msg }
    }
    fn fail(d: std::time::Duration, msg: String) -> Self {
        Self { ok: false, duration_ms: d.as_millis(), detail: msg }
    }
}

fn print_report(report: &[(&'static str, Outcome)]) {
    for (name, out) in report {
        let mark = if out.ok { "OK  " } else { "FAIL" };
        if out.detail.is_empty() {
            println!("  [{}] {:<22} ({} ms)", mark, name, out.duration_ms);
        } else {
            println!("  [{}] {:<22} ({} ms)  {}", mark, name, out.duration_ms, out.detail);
        }
    }
}
