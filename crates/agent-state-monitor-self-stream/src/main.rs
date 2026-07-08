//! State Monitoring Agent — observe this party's own orders and settlements
//!
//! Read-only. Subscribes to `SubscribeOrders` and `SubscribeSettlements` and
//! prints every event (created/filled/cancelled/etc., proposal/settled/failed)
//! as a formatted log line. Useful for ops dashboards, audit trails, and
//! debugging settlement issues. No ledger writes.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{
    order_update::EventType as OrderEvent,
    settlement_update::EventType as SettlementEvent,
    OrderStatus, OrderUpdate, SettlementUpdate,
};

#[derive(Parser)]
#[command(name = "agent-state-monitor-self-stream")]
#[command(about = "Observe own order + settlement state via streaming subscriptions")]
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
    /// Stream order + settlement updates until shutdown
    Run {
        /// Optional market filter; if omitted, all markets are streamed
        #[arg(long)]
        market: Option<String>,

        /// Skip the order stream
        #[arg(long)]
        no_orders: bool,

        /// Skip the settlement stream
        #[arg(long)]
        no_settlements: bool,
    },
    /// One-off snapshot: active orders + pending settlement proposals
    Snapshot {
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
        &["agent_state_monitor", "agent_logic"],
        "agent-state-monitor-self-stream",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Run {
            market,
            no_orders,
            no_settlements,
        } => run(config, market, no_orders, no_settlements).await,
        Commands::Snapshot { market } => snapshot(config, market).await,
    }
}

async fn run(
    config: BaseConfig,
    market: Option<String>,
    no_orders: bool,
    no_settlements: bool,
) -> Result<()> {
    if no_orders && no_settlements {
        anyhow::bail!("--no-orders and --no-settlements together leave nothing to monitor");
    }

    info!("Starting State Monitor");
    info!("Party: {}", config.party_id);
    info!(
        "market={:?} orders={} settlements={}",
        market, !no_orders, !no_settlements
    );

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Shutdown signal received");
            shutdown_signal.notify_waiters();
        }
    });

    let mut handles = Vec::new();

    if !no_orders {
        let cfg = config.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            stream_orders(cfg, m, sd).await
        }));
    }
    if !no_settlements {
        let cfg = config.clone();
        let m = market.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            stream_settlements(cfg, m, sd).await
        }));
    }

    for h in handles {
        if let Err(e) = h.await {
            error!("monitor task panicked: {e}");
        }
    }
    info!("State Monitor exited");
    Ok(())
}

async fn stream_orders(
    config: BaseConfig,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_orders(market.clone())
        .await
        .context("subscribe_orders failed")?;

    info!("[orders] stream opened market={:?}", market);

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                info!("[orders] shutdown — closing");
                return Ok(());
            }
            next = stream.next() => match next {
                Some(Ok(u)) => log_order_update(&u),
                Some(Err(s)) => { error!("[orders] stream error: {} — exiting", s); return Ok(()); }
                None => { warn!("[orders] stream ended by server"); return Ok(()); }
            }
        }
    }
}

async fn stream_settlements(
    config: BaseConfig,
    market: Option<String>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let mut stream = client
        .subscribe_settlements(market.clone())
        .await
        .context("subscribe_settlements failed")?;

    info!("[settlements] stream opened market={:?}", market);

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                info!("[settlements] shutdown — closing");
                return Ok(());
            }
            next = stream.next() => match next {
                Some(Ok(u)) => log_settlement_update(&u),
                Some(Err(s)) => { error!("[settlements] stream error: {} — exiting", s); return Ok(()); }
                None => { warn!("[settlements] stream ended by server"); return Ok(()); }
            }
        }
    }
}

fn log_order_update(u: &OrderUpdate) {
    let kind = match OrderEvent::try_from(u.event_type).unwrap_or(OrderEvent::Unspecified) {
        OrderEvent::Created => "CREATED",
        OrderEvent::Updated => "UPDATED",
        OrderEvent::Filled => "FILLED",
        OrderEvent::PartiallyFilled => "PARTIAL",
        OrderEvent::Cancelled => "CANCELLED",
        OrderEvent::Expired => "EXPIRED",
        OrderEvent::Unspecified => "?",
    };
    let ts = format_proto_ts(u.timestamp.as_ref());
    if let Some(o) = &u.order {
        let status = OrderStatus::try_from(o.status).unwrap_or(OrderStatus::Unspecified);
        let match_part = u
            .r#match
            .as_ref()
            .map(|m| format!(" matched(qty={} px={})", m.matched_quantity, m.matched_price))
            .unwrap_or_default();
        info!(
            "[orders] {} ts={} id={} market={} side={:?} status={:?} price={} qty={} filled={} remaining={}{}",
            kind,
            ts,
            o.order_id,
            o.market_id,
            o.order_type,
            status,
            o.price,
            o.quantity,
            o.filled_quantity,
            o.remaining_quantity,
            match_part,
        );
    } else {
        info!("[orders] {} ts={} (no order payload)", kind, ts);
    }
}

fn log_settlement_update(u: &SettlementUpdate) {
    let kind = match SettlementEvent::try_from(u.event_type).unwrap_or(SettlementEvent::Unspecified) {
        SettlementEvent::ProposalCreated => "PROPOSAL_CREATED",
        SettlementEvent::StatusChanged => "STATUS_CHANGED",
        SettlementEvent::Settled => "SETTLED",
        SettlementEvent::Failed => "FAILED",
        SettlementEvent::Cancelled => "CANCELLED",
        SettlementEvent::Unspecified => "?",
    };
    let ts = format_proto_ts(u.timestamp.as_ref());
    if let Some(p) = &u.proposal {
        let err = p
            .error_message
            .as_ref()
            .map(|m| format!(" err=\"{}\"", m))
            .unwrap_or_default();
        info!(
            "[settlements] {} ts={} id={} market={} buyer={} seller={} qty={} px={} status={}{}",
            kind,
            ts,
            p.proposal_id,
            p.market_id,
            short_party(&p.buyer),
            short_party(&p.seller),
            p.base_quantity,
            p.settlement_price,
            p.status,
            err,
        );
    } else {
        info!("[settlements] {} ts={} (no proposal payload)", kind, ts);
    }
}

async fn snapshot(config: BaseConfig, market: Option<String>) -> Result<()> {
    let mut client = OrderbookClient::new(&config).await?;
    let orders = match market.as_deref() {
        Some(m) => client.get_active_orders(m).await?,
        None => client.get_all_active_orders().await?,
    };
    println!("=== Active orders ({}) ===", orders.len());
    for o in orders {
        let status = OrderStatus::try_from(o.status).unwrap_or(OrderStatus::Unspecified);
        println!(
            "  id={} market={} side={:?} status={:?} price={} qty={} filled={} remaining={}",
            o.order_id,
            o.market_id,
            o.order_type,
            status,
            o.price,
            o.quantity,
            o.filled_quantity,
            o.remaining_quantity,
        );
    }

    let proposals = client.get_pending_proposals().await?;
    let filtered: Vec<_> = match market.as_deref() {
        Some(m) => proposals.into_iter().filter(|p| p.market_id == m).collect(),
        None => proposals,
    };
    println!("\n=== Pending settlement proposals ({}) ===", filtered.len());
    for p in filtered {
        println!(
            "  id={} market={} buyer={} seller={} qty={} px={} status={}",
            p.proposal_id,
            p.market_id,
            short_party(&p.buyer),
            short_party(&p.seller),
            p.base_quantity,
            p.settlement_price,
            p.status,
        );
    }
    Ok(())
}

fn format_proto_ts(ts: Option<&prost_types::Timestamp>) -> String {
    let Some(ts) = ts else { return "-".to_string() };
    let nanos: i64 = ts.nanos.into();
    DateTime::<Utc>::from_timestamp(ts.seconds, nanos.max(0) as u32)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{}.{}", ts.seconds, ts.nanos))
}

fn short_party(p: &str) -> String {
    if p.len() > 18 {
        format!("{}…{}", &p[..8], &p[p.len() - 6..])
    } else {
        p.to_string()
    }
}
