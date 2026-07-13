//! Updates watcher (design §5.2) — "LP analyzes all updates".
//!
//! Cursor-poll loop over `GetUpdates` (a bounded offset-range stream, not a
//! tail): every `updates_poll_interval_secs` (default 2 s) fetch updates for
//! the Amulet / Holding / SettlementTicket / AtomicDVP templates and keep the
//! shared caches truthful in ~2 s:
//!
//! - consumed Amulet/Holding → `mark_consumed`; a V2Quote-reserved consumption
//!   emits `SettleObserved{quote_id, update_id}` to the rfq_v2 state
//! - created Amulet/Holding owned by the party → cache add, blob-pending
//!   (update events carry no createdEventBlob; the 30 s ACS refresh backfills)
//! - archived SettlementTicket → ticket pool Spent
//! - created/archived AtomicDVP → venue registry refresh (key rotation)

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{get_updates_response, ledger_event};

use crate::holdings_cache::{
    instrument_key, CachedHolding, HoldingsCache, CC_INSTRUMENT, TEMPLATE_AMULET, TEMPLATE_HOLDING,
};
use crate::ledger_client::DAppProviderClient;
use crate::rfq_v2::SettleObserved;
use crate::ticket_pool::TicketPool;
use crate::venue_registry::{
    VenueRegistry, TEMPLATE_ATOMIC_DVP, TEMPLATE_SETTLEMENT_TICKET,
};

enum TemplateKind {
    Amulet,
    Holding,
    Ticket,
    Venue,
    Other,
}

fn classify(template_id: &str) -> TemplateKind {
    if template_id.contains("Splice.Amulet:Amulet") && !template_id.contains("Locked") {
        TemplateKind::Amulet
    } else if template_id.contains("Utility.Registry.Holding.V0.Holding:Holding") {
        TemplateKind::Holding
    } else if template_id.contains("AtomicDVP:SettlementTicket") {
        TemplateKind::Ticket
    } else if template_id.contains("AtomicDVP:AtomicDVP") {
        TemplateKind::Venue
    } else {
        TemplateKind::Other
    }
}

/// Spawn the updates watcher background task (LP + rfq_v2 mode only).
#[allow(clippy::too_many_arguments)]
pub fn spawn_updates_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    ticket_pool: Option<Arc<TicketPool>>,
    venue_registry: Arc<VenueRegistry>,
    settle_tx: mpsc::UnboundedSender<SettleObserved>,
    poll_interval_secs: u64,
    shutdown: Shutdown,
) {
    tokio::spawn(async move {
        info!("Updates watcher started (poll every {}s)", poll_interval_secs);

        let filters: Vec<String> = vec![
            TEMPLATE_AMULET.to_string(),
            TEMPLATE_HOLDING.to_string(),
            TEMPLATE_SETTLEMENT_TICKET.to_string(),
            TEMPLATE_ATOMIC_DVP.to_string(),
        ];

        let mut client: Option<DAppProviderClient> = None;
        let mut cursor: Option<i64> = None;

        loop {
            if shutdown.is_shutting_down() {
                info!("Updates watcher shutting down");
                return;
            }

            // (Re)create the client + seed the cursor as needed
            if client.is_none() {
                match create_client(&config).await {
                    Ok(c) => client = Some(c),
                    Err(e) => {
                        warn!("Updates watcher: client create failed: {:#}", e);
                    }
                }
            }
            if let Some(c) = client.as_mut() {
                if cursor.is_none() {
                    match c.get_ledger_end().await {
                        Ok(offset) => {
                            info!("Updates watcher: cursor seeded at offset {}", offset);
                            cursor = Some(offset);
                        }
                        Err(e) => {
                            warn!("Updates watcher: get_ledger_end failed: {:#}", e);
                            client = None;
                        }
                    }
                }
            }

            if let (Some(c), Some(cur)) = (client.as_mut(), cursor) {
                match c.get_updates(cur, None, &filters).await {
                    Ok(updates) => {
                        let mut max_offset = cur;
                        let mut venue_changed = false;
                        for resp in &updates {
                            match &resp.update {
                                Some(get_updates_response::Update::Transaction(tx)) => {
                                    max_offset = max_offset.max(tx.offset);
                                    venue_changed |= process_transaction(
                                        tx,
                                        &config.party_id,
                                        &cache,
                                        &ticket_pool,
                                        &settle_tx,
                                    )
                                    .await;
                                }
                                Some(get_updates_response::Update::OffsetCheckpoint(cp)) => {
                                    max_offset = max_offset.max(cp.offset);
                                }
                                None => {}
                            }
                        }
                        cursor = Some(max_offset);
                        if venue_changed {
                            info!("Updates watcher: AtomicDVP venue change observed — refreshing registry");
                            if let Err(e) = venue_registry.refresh(c).await {
                                warn!("Updates watcher: venue refresh failed: {:#}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Updates watcher: get_updates failed: {:#}", e);
                        client = None; // reconnect next tick; cursor is kept
                    }
                }
            }

            if shutdown.sleep(Duration::from_secs(poll_interval_secs)).await {
                info!("Updates watcher shutting down");
                return;
            }
        }
    });
}

async fn create_client(config: &BaseConfig) -> anyhow::Result<DAppProviderClient> {
    DAppProviderClient::new(
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
}

/// Process one ledger transaction's events in order. Returns true when an
/// AtomicDVP venue create/archive was seen (registry refresh needed).
async fn process_transaction(
    tx: &orderbook_proto::ledger::LedgerTransaction,
    party_id: &str,
    cache: &Arc<HoldingsCache>,
    ticket_pool: &Option<Arc<TicketPool>>,
    settle_tx: &mpsc::UnboundedSender<SettleObserved>,
) -> bool {
    let mut venue_changed = false;
    // One SettleObserved per quote_id per update, even when several reserved
    // holdings are consumed together.
    let mut observed_quotes: HashSet<String> = HashSet::new();

    for event in &tx.events {
        match &event.event {
            Some(ledger_event::Event::Created(created)) => {
                match classify(&created.template_id) {
                    TemplateKind::Venue => venue_changed = true,
                    TemplateKind::Amulet | TemplateKind::Holding => {
                        if let Some(h) = parse_created_holding(created, party_id, &tx.synchronizer_id) {
                            debug!(
                                "Updates watcher: created {} {} = {} ({})",
                                h.instrument, h.contract_id, h.amount, tx.update_id
                            );
                            cache.add_created(vec![h]).await;
                        }
                    }
                    // Created tickets are adopted by the pool's ACS reconcile
                    // (blobs are needed anyway and update events carry none).
                    TemplateKind::Ticket | TemplateKind::Other => {}
                }
            }
            Some(ledger_event::Event::Archived(archived)) => {
                handle_consumed(
                    &archived.contract_id,
                    &archived.template_id,
                    tx,
                    cache,
                    ticket_pool,
                    settle_tx,
                    &mut observed_quotes,
                    &mut venue_changed,
                )
                .await;
            }
            Some(ledger_event::Event::Exercised(exercised)) => {
                if exercised.consuming {
                    handle_consumed(
                        &exercised.contract_id,
                        &exercised.template_id,
                        tx,
                        cache,
                        ticket_pool,
                        settle_tx,
                        &mut observed_quotes,
                        &mut venue_changed,
                    )
                    .await;
                }
            }
            None => {}
        }
    }

    venue_changed
}

#[allow(clippy::too_many_arguments)]
async fn handle_consumed(
    contract_id: &str,
    template_id: &str,
    tx: &orderbook_proto::ledger::LedgerTransaction,
    cache: &Arc<HoldingsCache>,
    ticket_pool: &Option<Arc<TicketPool>>,
    settle_tx: &mpsc::UnboundedSender<SettleObserved>,
    observed_quotes: &mut HashSet<String>,
    venue_changed: &mut bool,
) {
    match classify(template_id) {
        TemplateKind::Amulet | TemplateKind::Holding => {
            // Reservation kind BEFORE mark_consumed (which drops the entry)
            let v2_quote = cache.v2_reservation_quote(contract_id).await;
            cache
                .mark_consumed(&[contract_id.to_string()], &tx.update_id)
                .await;
            if let Some(quote_id) = v2_quote {
                if observed_quotes.insert(quote_id.clone()) {
                    let _ = settle_tx.send(SettleObserved {
                        quote_id,
                        update_id: tx.update_id.clone(),
                    });
                }
            }
        }
        TemplateKind::Ticket => {
            if let Some(pool) = ticket_pool {
                pool.on_archived(contract_id);
            }
        }
        TemplateKind::Venue => *venue_changed = true,
        TemplateKind::Other => {}
    }
}

/// Parse a created Amulet/Holding event into a blob-pending cache entry.
fn parse_created_holding(
    created: &orderbook_proto::ledger::LedgerCreatedEvent,
    party_id: &str,
    synchronizer_id: &str,
) -> Option<CachedHolding> {
    let args = created.create_arguments.as_ref()?;
    let json = crate::prost_struct_to_json(args);
    let owner_ok = json.get("owner").and_then(|o| o.as_str()) == Some(party_id);
    if !owner_ok {
        return None;
    }

    let (instrument, amount) = match classify(&created.template_id) {
        TemplateKind::Amulet => {
            let amount: Decimal = json
                .pointer("/amount/initialAmount")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())?;
            (CC_INSTRUMENT.to_string(), amount)
        }
        TemplateKind::Holding => {
            let lock = json.pointer("/lock");
            if !(lock.is_none() || lock.is_some_and(|l| l.is_null())) {
                return None;
            }
            let amount: Decimal = json
                .get("amount")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())?;
            let admin = json.pointer("/instrument/source").and_then(|v| v.as_str())?;
            let id = json.pointer("/instrument/id").and_then(|v| v.as_str())?;
            (instrument_key(admin, id), amount)
        }
        _ => return None,
    };

    Some(CachedHolding {
        contract_id: created.contract_id.clone(),
        template_id: created.template_id.clone(),
        instrument,
        amount,
        created_event_blob: None, // blob-pending: update events carry no blob
        synchronizer_id: synchronizer_id.to_string(),
        discovered_at: std::time::Instant::now(),
    })
}
