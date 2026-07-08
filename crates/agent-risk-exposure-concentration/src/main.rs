//! Risk Exposure Dashboard
//!
//! Periodic snapshot of the party's risk posture across all configured
//! markets. Each cycle pulls:
//!
//! - Unlocked balances per instrument (ledger)
//! - Active orders aggregated by market and side (open notional)
//! - Pending settlement proposals (pending notional)
//! - Live mid for each configured market (for valuation)
//!
//! Computes:
//! - Portfolio value in quote currency
//! - Concentration: each instrument's share of portfolio value
//! - Top-concentration flag if any single instrument exceeds
//!   `--concentration-warn-pct`
//! - Open notional / portfolio ratio (leverage proxy)
//!
//! Emits one JSONL record per cycle to stdout / file / webhook. Read-only:
//! does not place or cancel orders.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{OrderType, SettlementStatus};


use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-risk-exposure-concentration")]
#[command(about = "Per-instrument concentration + open exposure dashboard")]
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
        /// Markets to value (any instrument whose ID is the BASE side of one of
        /// these markets contributes value × mid to portfolio_value)
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,

        /// Snapshot interval in seconds
        #[arg(long, default_value = "60")]
        snapshot_secs: u64,

        /// Warn flag set if any instrument's share exceeds this
        #[arg(long, default_value = "50.0")]
        concentration_warn_pct: f64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// One-off snapshot then exit
    Snapshot {
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        markets: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_risk_exposure", "agent_logic"],
        "agent-risk-exposure-concentration",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            markets,
            snapshot_secs,
            concentration_warn_pct,
            stdout,
            log_file,
        } => {
            if markets.is_empty() {
                anyhow::bail!("--markets is required");
            }
            if !stdout && log_file.is_none() {
                anyhow::bail!("provide at least one sink (--stdout / --log-file)");
            }
            let sink = FileSink::open(log_file, stdout).await?;
            run(config, markets, snapshot_secs, concentration_warn_pct, sink).await
        }
        Commands::Snapshot { markets } => {
            if markets.is_empty() {
                anyhow::bail!("--markets is required");
            }
            let sink = FileSink::open(None, true).await?;
            snapshot_once(config, markets, 50.0, sink).await
        }
    }
}

struct FileSink {
    file: Option<Mutex<tokio::fs::File>>,
    stdout: bool,
}
impl FileSink {
    async fn open(log_file: Option<PathBuf>, stdout: bool) -> Result<Arc<Self>> {
        let file = match log_file {
            Some(p) => Some(Mutex::new(
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .await
                    .with_context(|| format!("open {:?}", p))?,
            )),
            None => None,
        };
        Ok(Arc::new(Self { file, stdout }))
    }
    async fn emit(&self, line: &str) {
        if self.stdout {
            println!("{}", line);
        }
        if let Some(f) = &self.file {
            let mut f = f.lock().await;
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
        }
    }
}

async fn run(
    config: BaseConfig,
    markets: Vec<String>,
    snapshot_secs: u64,
    warn_pct: f64,
    sink: Arc<FileSink>,
) -> Result<()> {
    info!(
        "Starting risk-exposure: markets={:?} snapshot={}s warn_pct={}",
        markets, snapshot_secs, warn_pct
    );

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(snapshot_secs)) => {
                if let Err(e) = snapshot_once(config.clone(), markets.clone(), warn_pct, sink.clone()).await {
                    error!("snapshot failed: {:#}", e);
                }
            }
        }
    }
}

async fn snapshot_once(
    config: BaseConfig,
    markets: Vec<String>,
    warn_pct: f64,
    sink: Arc<FileSink>,
) -> Result<()> {
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

    // Balances → values via mids
    let balances = ledger.get_balances().await.context("get_balances failed")?;
    let known_markets = ob.get_markets().await.unwrap_or_default();
    let mut mids: HashMap<String, Decimal> = HashMap::new();
    let mut base_to_market: HashMap<String, String> = HashMap::new();
    for m in &known_markets {
        if markets.contains(&m.market_id) {
            base_to_market.insert(m.base_instrument.clone(), m.market_id.clone());
        }
    }
    for m in &markets {
        let mid = match ob.get_price(m).await {
            Ok(p) => mid_decimal(&p),
            Err(_) => Decimal::ZERO,
        };
        mids.insert(m.clone(), mid);
    }

    let mut by_inst_value: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut by_inst_balance: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut portfolio_value = Decimal::ZERO;
    for b in &balances {
        let bal = Decimal::from_str(&b.total_amount).unwrap_or(Decimal::ZERO);
        by_inst_balance.insert(b.instrument_id.clone(), bal);
        let value = base_to_market
            .get(b.instrument_id.as_str())
            .and_then(|mid_market| mids.get(mid_market).copied())
            .map(|mid| bal * mid)
            .unwrap_or(Decimal::ZERO);
        by_inst_value.insert(b.instrument_id.clone(), value);
        portfolio_value += value;
    }

    // Active orders → open notional per market
    let orders = ob.get_all_active_orders().await.unwrap_or_default();
    let mut open_notional_per_market: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut open_total = Decimal::ZERO;
    for o in &orders {
        let p = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
        let q = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
        let notional = p * q;
        *open_notional_per_market.entry(o.market_id.clone()).or_default() += notional;
        open_total += notional;
    }

    // Pending settlements
    let proposals = ob.get_pending_proposals().await.unwrap_or_default();
    let mut pending_total = Decimal::ZERO;
    let mut pending_count: u32 = 0;
    let mut failed_count: u32 = 0;
    for p in &proposals {
        if p.status == SettlementStatus::Pending as i32 {
            pending_total += Decimal::from_str(&p.quote_quantity).unwrap_or(Decimal::ZERO);
            pending_count += 1;
        } else if p.status == SettlementStatus::Failed as i32 {
            failed_count += 1;
        }
    }

    // Concentration: highest share + warn
    let mut top_inst: Option<(String, Decimal, Decimal)> = None; // (instrument, value, share)
    let mut concentration_warn = false;
    let warn_decimal = Decimal::from_str(&format!("{}", warn_pct)).unwrap_or(Decimal::ZERO);
    for (inst, val) in &by_inst_value {
        if portfolio_value > Decimal::ZERO && *val > Decimal::ZERO {
            let share = *val / portfolio_value * Decimal::from(100);
            if top_inst.as_ref().map(|(_, _, s)| share > *s).unwrap_or(true) {
                top_inst = Some((inst.clone(), *val, share));
            }
            if share > warn_decimal {
                concentration_warn = true;
            }
        }
    }

    let leverage_proxy = if portfolio_value > Decimal::ZERO {
        open_total / portfolio_value
    } else {
        Decimal::ZERO
    };

    let payload = json!({
        "kind": "risk.exposure",
        "ts": Utc::now().to_rfc3339(),
        "party": config.party_id,
        "portfolio_value_quote": portfolio_value.to_string(),
        "open_notional_total": open_total.to_string(),
        "leverage_proxy": leverage_proxy.to_string(),
        "pending_settlements_count": pending_count,
        "pending_settlements_notional": pending_total.to_string(),
        "failed_settlements_count": failed_count,
        "top_instrument": top_inst.as_ref().map(|(i, v, s)| json!({
            "instrument": i,
            "value": v.to_string(),
            "share_pct": s.to_string(),
        })),
        "concentration_warn": concentration_warn,
        "concentration_warn_pct": warn_pct,
        "balances": by_inst_value.iter().map(|(inst, val)| {
            let bal = by_inst_balance.get(inst).copied().unwrap_or(Decimal::ZERO);
            let share = if portfolio_value > Decimal::ZERO && *val > Decimal::ZERO {
                (*val / portfolio_value * Decimal::from(100)).to_string()
            } else { "0".to_string() };
            json!({
                "instrument": inst,
                "balance": bal.to_string(),
                "value_quote": val.to_string(),
                "share_pct": share,
            })
        }).collect::<Vec<_>>(),
        "open_orders_by_market": open_notional_per_market.iter().map(|(m, n)| json!({
            "market_id": m,
            "open_notional": n.to_string(),
            "order_count": orders.iter().filter(|o| &o.market_id == m).count(),
            "bids": orders.iter().filter(|o| &o.market_id == m && o.order_type == OrderType::Bid as i32).count(),
            "offers": orders.iter().filter(|o| &o.market_id == m && o.order_type == OrderType::Offer as i32).count(),
        })).collect::<Vec<_>>(),
    });
    let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    sink.emit(&line).await;
    if concentration_warn {
        warn!("concentration warning fired");
    }
    Ok(())
}

fn mid_decimal(p: &orderbook_proto::pricing::GetPriceResponse) -> Decimal {
    let mid = match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    };
    if mid > 0.0 {
        Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO)
    } else {
        Decimal::ZERO
    }
}
