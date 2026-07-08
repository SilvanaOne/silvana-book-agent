//! PnL Screening Agent
//!
//! Subscribes to this party's settlement stream and accumulates per-market
//! position and weighted-average cost basis. After each settled trade it
//! recomputes:
//!
//! - **position** (signed): + when the party was the buyer, − when seller.
//! - **cost basis (WAVG)** maintained only for the "long" side of a market —
//!   when position is positive, cost basis tracks the WAVG price paid for the
//!   current open inventory; when position is closed (=0) cost basis resets.
//! - **realized PnL** accumulated when selling out of a long position
//!   (`(sell_price − cost_basis) × matched_qty`). The mirror case for short
//!   positions is symmetrical and supported.
//! - **unrealized PnL** = `position × (mid − cost_basis)` recomputed every
//!   `--snapshot-secs` using the live mid price.
//!
//! Read-only: no orders, no ledger writes. Snapshots logged to stdout or a
//! JSONL file.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{settlement_update::EventType as SettlementEvent, SettlementUpdate};

#[derive(Parser)]
#[command(name = "agent-pnl-screening-settlement-stream")]
#[command(about = "Track realized/unrealized PnL via the settlement stream")]
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
        /// Optional market filter (defaults to all)
        #[arg(long)]
        market: Option<String>,

        /// How often to recompute unrealized PnL and print a snapshot
        #[arg(long, default_value = "30")]
        snapshot_secs: u64,

        #[arg(long)]
        stdout: bool,

        #[arg(long)]
        log_file: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_pnl_screening", "agent_logic"],
        "agent-pnl-screening-settlement-stream",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            snapshot_secs,
            stdout,
            log_file,
        } => {
            if !stdout && log_file.is_none() {
                anyhow::bail!("provide at least one sink (--stdout / --log-file)");
            }
            let sink = FileSink::open(log_file, stdout).await?;
            run(config, market, snapshot_secs, sink).await
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
                    .with_context(|| format!("open log file {:?}", p))?,
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

#[derive(Default, Clone)]
struct PositionState {
    position: Decimal, // signed: + long, − short
    cost_basis: Decimal, // WAVG over current open inventory; reset to 0 on flatten
    realized: Decimal,
}

async fn run(
    config: BaseConfig,
    market_filter: Option<String>,
    snapshot_secs: u64,
    sink: Arc<FileSink>,
) -> Result<()> {
    let party = config.party_id.clone();
    info!("Starting pnl-screening for party {}", party);
    info!("market_filter={:?} snapshot_secs={}", market_filter, snapshot_secs);

    let mut client = OrderbookClient::new(&config).await?;
    let positions: Arc<Mutex<HashMap<String, PositionState>>> = Arc::new(Mutex::new(HashMap::new()));

    // Background task: subscribe settlements and update positions on SETTLED events
    let positions_for_stream = positions.clone();
    let party_for_stream = party.clone();
    let market_for_stream = market_filter.clone();
    let stream_config = config.clone();
    let sink_for_stream = sink.clone();
    tokio::spawn(async move {
        if let Err(e) = settlement_stream(
            stream_config,
            party_for_stream,
            market_for_stream,
            positions_for_stream,
            sink_for_stream,
        )
        .await
        {
            error!("settlement stream failed: {:#}", e);
        }
    });

    // Foreground: snapshot loop using live mid for unrealized PnL
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(snapshot_secs)) => {
                let snap = positions.lock().await.clone();
                if snap.is_empty() {
                    info!("no positions yet");
                    continue;
                }
                let mut total_unrealized = Decimal::ZERO;
                let mut total_realized = Decimal::ZERO;
                let mut entries = Vec::new();
                for (market, state) in &snap {
                    let mid = match client.get_price(market).await {
                        Ok(p) => mid_decimal(&p),
                        Err(_) => Decimal::ZERO,
                    };
                    let unrealized = if mid > Decimal::ZERO {
                        state.position * (mid - state.cost_basis)
                    } else {
                        Decimal::ZERO
                    };
                    total_unrealized += unrealized;
                    total_realized += state.realized;
                    entries.push(json!({
                        "market_id": market,
                        "position": state.position.to_string(),
                        "cost_basis": state.cost_basis.to_string(),
                        "mid": mid.to_string(),
                        "realized_pnl": state.realized.to_string(),
                        "unrealized_pnl": unrealized.to_string(),
                    }));
                }
                let payload = json!({
                    "kind": "pnl.snapshot",
                    "ts": Utc::now().to_rfc3339(),
                    "party": party,
                    "total_realized_pnl": total_realized.to_string(),
                    "total_unrealized_pnl": total_unrealized.to_string(),
                    "positions": entries,
                });
                let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
                sink.emit(&line).await;
            }
        }
    }
}

async fn settlement_stream(
    config: BaseConfig,
    party: String,
    market_filter: Option<String>,
    positions: Arc<Mutex<HashMap<String, PositionState>>>,
    sink: Arc<FileSink>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client.subscribe_settlements(market_filter).await?;
    info!("[settlements] stream opened");
    loop {
        match stream.next().await {
            Some(Ok(u)) => apply_event(&u, &party, &positions, &sink).await,
            Some(Err(s)) => {
                error!("[settlements] stream error: {} — exiting", s);
                return Ok(());
            }
            None => {
                warn!("[settlements] stream ended");
                return Ok(());
            }
        }
    }
}

async fn apply_event(
    u: &SettlementUpdate,
    party: &str,
    positions: &Arc<Mutex<HashMap<String, PositionState>>>,
    sink: &Arc<FileSink>,
) {
    let evt = SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified);
    if evt != SettlementEvent::Settled {
        return;
    }
    let Some(p) = &u.proposal else { return };

    let qty = Decimal::from_str(&p.base_quantity).unwrap_or(Decimal::ZERO);
    let price = Decimal::from_str(&p.settlement_price).unwrap_or(Decimal::ZERO);
    if qty <= Decimal::ZERO || price <= Decimal::ZERO {
        return;
    }
    // Determine our side: +qty if we were buyer, -qty if seller
    let signed_qty = if p.buyer == party {
        qty
    } else if p.seller == party {
        -qty
    } else {
        return;
    };

    let mut map = positions.lock().await;
    let state = map.entry(p.market_id.clone()).or_default();
    let prev_pos = state.position;
    let new_pos = prev_pos + signed_qty;

    // Realised PnL accrues whenever the trade reduces the absolute position
    // (i.e. closes part of the existing inventory).
    let mut realized_delta = Decimal::ZERO;
    if prev_pos > Decimal::ZERO && signed_qty < Decimal::ZERO {
        // Selling out of a long position
        let closed = signed_qty.abs().min(prev_pos);
        realized_delta = (price - state.cost_basis) * closed;
    } else if prev_pos < Decimal::ZERO && signed_qty > Decimal::ZERO {
        // Buying back a short position
        let closed = signed_qty.min(prev_pos.abs());
        realized_delta = (state.cost_basis - price) * closed;
    }

    // Update WAVG cost basis only on the side that grows the position
    if (prev_pos >= Decimal::ZERO && signed_qty > Decimal::ZERO)
        || (prev_pos <= Decimal::ZERO && signed_qty < Decimal::ZERO)
    {
        let old_abs = prev_pos.abs();
        let added = signed_qty.abs();
        let new_abs = old_abs + added;
        if new_abs > Decimal::ZERO {
            state.cost_basis = (state.cost_basis * old_abs + price * added) / new_abs;
        }
    }

    state.position = new_pos;
    state.realized += realized_delta;
    if new_pos == Decimal::ZERO {
        state.cost_basis = Decimal::ZERO;
    }

    let payload = json!({
        "kind": "pnl.fill",
        "ts": Utc::now().to_rfc3339(),
        "proposal_id": p.proposal_id,
        "market_id": p.market_id,
        "side": if p.buyer == party { "buy" } else { "sell" },
        "qty": qty.to_string(),
        "price": price.to_string(),
        "realized_pnl_delta": realized_delta.to_string(),
        "position_after": state.position.to_string(),
        "cost_basis_after": state.cost_basis.to_string(),
        "realized_pnl_total": state.realized.to_string(),
    });
    let line = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    sink.emit(&line).await;
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
