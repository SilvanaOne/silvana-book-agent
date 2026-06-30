//! Portfolio Health Check Agent — aggregated balance + exposure report
//!
//! Pulls balances from the ledger, active orders and pending settlement
//! proposals from the orderbook, current market mid-prices from the pricing
//! service, and renders a single human-readable snapshot:
//!
//! - Balances per instrument (total / unlocked / locked)
//! - Open exposure per market (bid notional vs offer notional)
//! - Pending settlement count + total notional
//! - Approximate portfolio NAV in the quote currency of each market

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::str::FromStr;

use agent_logic::client::OrderbookClient;
use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::{OrderType, SettlementStatus};


use cloud_agent::DAppProviderClient;

#[derive(Parser)]
#[command(name = "agent-portfolio-health")]
#[command(about = "Aggregated balance + exposure report")]
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
    Report {
        /// Optional list of markets to value with mid-price; empty = skip valuation
        #[arg(long, value_delimiter = ',', num_args = 0..)]
        markets: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    agent_logic::logging::init_logging(
        cli.verbose,
        &["agent_portfolio_health", "agent_logic"],
        "agent-portfolio-health",
    );

    let config = BaseConfig::load(&cli.config)
        .with_context(|| format!("Failed to load config from {:?}", cli.config))?;

    match cli.command {
        Commands::Report { markets } => report(config, markets).await,
    }
}

#[derive(Default)]
struct MarketExposure {
    bid_qty: Decimal,
    bid_notional: Decimal,
    offer_qty: Decimal,
    offer_notional: Decimal,
    order_count: u32,
}

async fn report(config: BaseConfig, markets: Vec<String>) -> Result<()> {
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

    println!("Party: {}", config.party_id);

    // Balances
    let balances = ledger.get_balances().await.context("get_balances failed")?;
    let mut total_by_instrument: BTreeMap<String, Decimal> = BTreeMap::new();
    println!("\n=== Balances ===");
    for b in &balances {
        let total = Decimal::from_str(&b.total_amount).unwrap_or(Decimal::ZERO);
        total_by_instrument.insert(b.instrument_id.clone(), total);
        println!(
            "  {:<20} total={:<18} unlocked={:<18} locked={}",
            b.instrument_id, b.total_amount, b.unlocked_amount, b.locked_amount
        );
    }

    // Active orders aggregated by market and side
    let orders = ob.get_all_active_orders().await.context("get_all_active_orders failed")?;
    let mut exposures: HashMap<String, MarketExposure> = HashMap::new();
    for o in &orders {
        let price = Decimal::from_str(&o.price).unwrap_or(Decimal::ZERO);
        let qty = Decimal::from_str(&o.remaining_quantity).unwrap_or(Decimal::ZERO);
        let notional = price * qty;
        let e = exposures.entry(o.market_id.clone()).or_default();
        e.order_count += 1;
        if o.order_type == OrderType::Bid as i32 {
            e.bid_qty += qty;
            e.bid_notional += notional;
        } else if o.order_type == OrderType::Offer as i32 {
            e.offer_qty += qty;
            e.offer_notional += notional;
        }
    }

    println!("\n=== Open exposure ({} orders across {} markets) ===", orders.len(), exposures.len());
    if exposures.is_empty() {
        println!("  (no active orders)");
    }
    let mut market_keys: Vec<&String> = exposures.keys().collect();
    market_keys.sort();
    for m in &market_keys {
        let e = &exposures[*m];
        println!(
            "  {:<16} orders={:<3} bid_qty={:<14} bid_notional={:<16} offer_qty={:<14} offer_notional={}",
            m, e.order_count, e.bid_qty, e.bid_notional, e.offer_qty, e.offer_notional
        );
    }

    // Pending / failed settlements
    let proposals = ob.get_pending_proposals().await.context("get_pending_proposals failed")?;
    let mut pending = 0u32;
    let mut failed = 0u32;
    let mut pending_notional = Decimal::ZERO;
    for p in &proposals {
        let n = Decimal::from_str(&p.quote_quantity).unwrap_or(Decimal::ZERO);
        if p.status == SettlementStatus::Pending as i32 {
            pending += 1;
            pending_notional += n;
        } else if p.status == SettlementStatus::Failed as i32 {
            failed += 1;
        }
    }
    println!("\n=== Settlements ===");
    println!("  pending={} failed={} pending_notional={}", pending, failed, pending_notional);

    // Optional mid-price valuation
    if !markets.is_empty() {
        println!("\n=== Mid-price valuation ===");
        for m in &markets {
            let mid = match ob.get_price(m).await {
                Ok(p) => mid_value(&p),
                Err(e) => {
                    println!("  {}: get_price error: {:#}", m, e);
                    continue;
                }
            };
            if mid <= 0.0 {
                println!("  {}: no mid-price", m);
                continue;
            }
            let mid_dec = Decimal::from_str(&format!("{}", mid)).unwrap_or(Decimal::ZERO);
            let exposure = exposures.get(m);
            let net_inventory = match exposure {
                Some(e) => e.bid_qty - e.offer_qty,
                None => Decimal::ZERO,
            };
            println!(
                "  {:<16} mid={:.6}  net_open_inventory={}  inv_value={}",
                m,
                mid,
                net_inventory,
                net_inventory * mid_dec
            );
        }
    }

    Ok(())
}

fn mid_value(p: &orderbook_proto::pricing::GetPriceResponse) -> f64 {
    match (p.bid, p.ask) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => (b + a) / 2.0,
        _ if p.last > 0.0 => p.last,
        _ => 0.0,
    }
}
