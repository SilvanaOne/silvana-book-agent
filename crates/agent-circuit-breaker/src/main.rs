//! Circuit Breaker Agent
//!
//! Tracks a market's mid price. When the price moves more than
//! `--max-deviation-pct` from a baseline within the `--window-secs` lookback
//! window, the breaker TRIPS: cancel-all this party's orders on that market and
//! sleep for `--pause-secs` before re-arming. On re-arm the baseline resets to
//! the current price.
//!
//! Use it as a safety net in front of `agent-spot-grid` or any market-making
//! strategy: a sudden spike or crash pulls your quotes off the book until the
//! market settles.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use orderbook_agent_logic::client::OrderbookClient;
use orderbook_agent_logic::config::BaseConfig;

#[derive(Parser)]
#[command(name = "agent-circuit-breaker")]
#[command(about = "Halt trading on a market when price deviates beyond a threshold")]
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
    Run {
        #[arg(long)]
        market: String,

        /// Trip when |price - baseline| / baseline * 100 >= this value
        #[arg(long)]
        max_deviation_pct: f64,

        /// Lookback window for the baseline: the oldest mid price within this many seconds
        #[arg(long, default_value = "60")]
        window_secs: u64,

        /// Sleep this long after a trip before re-arming
        #[arg(long, default_value = "300")]
        pause_secs: u64,

        /// Poll interval
        #[arg(long, default_value = "5")]
        poll_secs: u64,

        /// Don't actually cancel — just log
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    orderbook_agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_circuit_breaker", "orderbook_agent_logic"],
        "agent-circuit-breaker",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            max_deviation_pct,
            window_secs,
            pause_secs,
            poll_secs,
            dry_run,
        } => {
            if poll_secs == 0 {
                anyhow::bail!("--poll-secs must be >= 1");
            }
            run(
                config,
                market,
                max_deviation_pct,
                window_secs,
                pause_secs,
                poll_secs,
                dry_run,
            )
            .await
        }
    }
}

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    mid: f64,
}

async fn run(
    config: BaseConfig,
    market: String,
    max_deviation_pct: f64,
    window_secs: u64,
    pause_secs: u64,
    poll_secs: u64,
    dry_run: bool,
) -> Result<()> {
    info!(
        "Starting circuit-breaker on {}: max_dev={}% window={}s pause={}s poll={}s dry_run={}",
        market, max_deviation_pct, window_secs, pause_secs, poll_secs, dry_run
    );
    let mut client = OrderbookClient::new(&config).await?;
    let mut history: VecDeque<Sample> = VecDeque::new();

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown signal received");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(poll_secs)) => {
                let mid = match client.get_price(&market).await {
                    Ok(p) => mid_value(&p),
                    Err(e) => { warn!("get_price failed: {:#}", e); continue; }
                };
                if mid <= 0.0 { continue; }

                let now = Instant::now();
                history.push_back(Sample { at: now, mid });
                while let Some(front) = history.front() {
                    if now.duration_since(front.at).as_secs() > window_secs {
                        history.pop_front();
                    } else {
                        break;
                    }
                }

                // Baseline = the oldest sample still inside the window
                let Some(baseline) = history.front().copied() else { continue };
                if baseline.mid <= 0.0 { continue; }

                let dev_pct = (mid - baseline.mid) / baseline.mid * 100.0;
                info!(
                    "mid={:.6} baseline={:.6} dev={:+.4}% (window {}s, samples={})",
                    mid, baseline.mid, dev_pct, window_secs, history.len()
                );

                if dev_pct.abs() >= max_deviation_pct {
                    error!(
                        "CIRCUIT BREAKER TRIPPED on {}: dev {:+.4}% >= {:.4}% — cancelling orders",
                        market, dev_pct, max_deviation_pct
                    );
                    let cancelled = cancel_market(&mut client, &market, dry_run).await
                        .unwrap_or_else(|e| { error!("cancel failed: {:#}", e); 0 });
                    error!("cancel-all on {}: {} orders affected, pausing {}s", market, cancelled, pause_secs);
                    tokio::time::sleep(Duration::from_secs(pause_secs)).await;
                    history.clear();
                    info!("re-armed — baseline reset");
                }
            }
        }
    }
}

async fn cancel_market(client: &mut OrderbookClient, market: &str, dry_run: bool) -> Result<usize> {
    let orders = client.get_active_orders(market).await?;
    let mut affected = 0;
    for o in orders {
        if dry_run {
            info!("[dry-run] would cancel order_id={}", o.order_id);
            affected += 1;
            continue;
        }
        match client.cancel_order(o.order_id).await {
            Ok(r) if r.success => {
                info!("cancelled order_id={}", o.order_id);
                affected += 1;
            }
            Ok(r) => warn!("cancel order_id={} !success: {}", o.order_id, r.message),
            Err(e) => warn!("cancel order_id={} failed: {:#}", o.order_id, e),
        }
    }
    Ok(affected)
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
