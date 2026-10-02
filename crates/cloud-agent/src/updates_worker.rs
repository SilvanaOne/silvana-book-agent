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

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Bounded, Policy};
use agent_logic::sync;
use orderbook_proto::ledger::{get_updates_response, ledger_event, GetUpdatesResponse};

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

/// Added to twice the request timeout to bound one GetUpdates call or venue
/// refresh; above the client's own open deadline plus stream bound.
const BACKSTOP_SLACK: Duration = Duration::from_secs(60);

fn backstop(config: &BaseConfig) -> Duration {
    Duration::from_secs(config.request_timeout_secs)
        .saturating_mul(2)
        .saturating_add(BACKSTOP_SLACK)
}

/// Restarts allowed at one unchanged cursor before the batch after it is skipped.
const RESUME_RETRIES: u32 = 2;

/// Where a restarted watcher picks up; shared by every run of the task.
#[derive(Debug, Default)]
struct Resume {
    cursor: Option<i64>,
    /// Restarts at `cursor` since it last advanced.
    retries_at_cursor: u32,
}

type SharedResume = Arc<std::sync::Mutex<Resume>>;

impl Resume {
    /// The cursor a new run starts from; `None` seeds it at the ledger end.
    fn seed(&mut self) -> Option<i64> {
        match self.cursor {
            Some(c) if self.retries_at_cursor < RESUME_RETRIES => {
                self.retries_at_cursor = self.retries_at_cursor.saturating_add(1);
                Some(c)
            }
            Some(c) => {
                error!(
                    "Updates watcher: skipping the updates after offset {} after {} failed restarts there",
                    c, self.retries_at_cursor
                );
                *self = Resume::default();
                None
            }
            None => None,
        }
    }

    /// Record the cursor a run has reached; moving on resets the retry count.
    fn record(&mut self, cursor: i64) {
        if self.cursor != Some(cursor) {
            self.retries_at_cursor = 0;
        }
        self.cursor = Some(cursor);
    }
}

/// Spawn the updates watcher background task (LP + rfq_v2 mode only); it
/// restarts if it fails, resuming from the last applied offset.
#[allow(clippy::too_many_arguments)]
pub fn spawn_updates_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    ticket_pool: Option<Arc<TicketPool>>,
    venue_registry: Arc<VenueRegistry>,
    settle_tx: mpsc::UnboundedSender<SettleObserved>,
    poll_interval_secs: u64,
    shutdown: Shutdown,
) -> anyhow::Result<()> {
    let s = shutdown.clone();
    let resume = SharedResume::default();
    supervise::spawn_supervised("updates watcher", shutdown, Policy::Restart, move || {
        run(
            config.clone(),
            cache.clone(),
            ticket_pool.clone(),
            venue_registry.clone(),
            settle_tx.clone(),
            poll_interval_secs,
            s.clone(),
            resume.clone(),
        )
    })?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    ticket_pool: Option<Arc<TicketPool>>,
    venue_registry: Arc<VenueRegistry>,
    settle_tx: mpsc::UnboundedSender<SettleObserved>,
    poll_interval_secs: u64,
    shutdown: Shutdown,
    resume: SharedResume,
) {
    info!("Updates watcher started (poll every {}s)", poll_interval_secs);

    let filters: Vec<String> = vec![
        TEMPLATE_AMULET.to_string(),
        TEMPLATE_HOLDING.to_string(),
        TEMPLATE_SETTLEMENT_TICKET.to_string(),
        TEMPLATE_ATOMIC_DVP.to_string(),
    ];
    let limit = backstop(&config);

    let mut client: Option<DAppProviderClient> = None;
    let mut cursor: Option<i64> = sync::lock(&resume).seed();
    if let Some(c) = cursor {
        info!("Updates watcher: resuming at offset {}", c);
    }

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
                        sync::lock(&resume).record(offset);
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
            let Some(fetched) = fetch(&shutdown, limit, c.get_updates(cur, None, &filters)).await else {
                info!("Updates watcher shutting down");
                return;
            };
            let step = apply_fetch(fetched, cur, limit, &config.party_id, &cache, &ticket_pool, &settle_tx).await;
            sync::lock(&resume).record(step.cursor);
            cursor = Some(step.cursor);
            let mut reconnect = step.reconnect;
            if step.venue_changed {
                info!("Updates watcher: AtomicDVP venue change observed — refreshing registry");
                match supervise::bounded(&shutdown, limit, venue_registry.refresh(c)).await {
                    Bounded::Done(Ok(())) => {}
                    Bounded::Done(Err(e)) => warn!("Updates watcher: venue refresh failed: {:#}", e),
                    Bounded::Elapsed => {
                        warn!(
                            "Updates watcher: venue refresh failed: no result within {}s",
                            limit.as_secs()
                        );
                        reconnect = true;
                    }
                    Bounded::Shutdown => {
                        info!("Updates watcher shutting down");
                        return;
                    }
                }
            }
            if reconnect {
                client = None; // reconnect next tick; cursor is kept
            }
        }

        if shutdown.sleep(Duration::from_secs(poll_interval_secs)).await {
            info!("Updates watcher shutting down");
            return;
        }
    }
}

/// What one GetUpdates poll returned.
enum Fetch {
    Batch {
        updates: Vec<GetUpdatesResponse>,
        complete: bool,
    },
    Failed(anyhow::Error),
    TimedOut,
}

/// One GetUpdates call within `limit`; `None` on shutdown.
async fn fetch<F>(shutdown: &Shutdown, limit: Duration, call: F) -> Option<Fetch>
where
    F: Future<Output = anyhow::Result<(Vec<GetUpdatesResponse>, bool)>>,
{
    match supervise::bounded(shutdown, limit, call).await {
        Bounded::Done(Ok((updates, complete))) => Some(Fetch::Batch { updates, complete }),
        Bounded::Done(Err(e)) => Some(Fetch::Failed(e)),
        Bounded::Elapsed => Some(Fetch::TimedOut),
        Bounded::Shutdown => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Step {
    cursor: i64,
    venue_changed: bool,
    /// Drop the client so the next poll reconnects.
    reconnect: bool,
}

/// Apply one poll. Any batch advances the cursor, as a partial one is an
/// ordered prefix; anything short of a complete batch also reconnects.
async fn apply_fetch(
    fetched: Fetch,
    cur: i64,
    limit: Duration,
    party_id: &str,
    cache: &Arc<HoldingsCache>,
    ticket_pool: &Option<Arc<TicketPool>>,
    settle_tx: &mpsc::UnboundedSender<SettleObserved>,
) -> Step {
    match fetched {
        Fetch::Batch { updates, complete } => {
            let mut max_offset = cur;
            let mut venue_changed = false;
            for resp in &updates {
                match &resp.update {
                    Some(get_updates_response::Update::Transaction(tx)) => {
                        max_offset = max_offset.max(tx.offset);
                        venue_changed |=
                            process_transaction(tx, party_id, cache, ticket_pool, settle_tx).await;
                    }
                    Some(get_updates_response::Update::OffsetCheckpoint(cp)) => {
                        max_offset = max_offset.max(cp.offset);
                    }
                    None => {}
                }
            }
            if !complete {
                debug!(
                    "Updates watcher: partial batch of {} update(s) applied up to offset {}; reconnecting",
                    updates.len(),
                    max_offset
                );
            }
            Step { cursor: max_offset, venue_changed, reconnect: !complete }
        }
        Fetch::Failed(e) => {
            warn!("Updates watcher: get_updates failed: {:#}", e);
            Step { cursor: cur, venue_changed: false, reconnect: true }
        }
        Fetch::TimedOut => {
            warn!(
                "Updates watcher: get_updates failed: no result within {}s",
                limit.as_secs()
            );
            Step { cursor: cur, venue_changed: false, reconnect: true }
        }
    }
}

async fn create_client(config: &BaseConfig) -> anyhow::Result<DAppProviderClient> {
    DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{Fake, FakeLedger};
    use orderbook_proto::ledger::{LedgerArchivedEvent, LedgerEvent, LedgerOffsetCheckpoint, LedgerTransaction};

    fn tx_at(offset: i64, events: Vec<LedgerEvent>) -> GetUpdatesResponse {
        GetUpdatesResponse {
            update: Some(get_updates_response::Update::Transaction(LedgerTransaction {
                update_id: format!("u{offset}"),
                offset,
                events,
                ..Default::default()
            })),
        }
    }

    fn checkpoint(offset: i64) -> GetUpdatesResponse {
        GetUpdatesResponse {
            update: Some(get_updates_response::Update::OffsetCheckpoint(LedgerOffsetCheckpoint { offset })),
        }
    }

    fn archived(cid: &str) -> LedgerEvent {
        LedgerEvent {
            event: Some(ledger_event::Event::Archived(LedgerArchivedEvent {
                contract_id: cid.to_string(),
                template_id: TEMPLATE_HOLDING.to_string(),
            })),
        }
    }

    async fn apply(fetched: Fetch, cache: &Arc<HoldingsCache>, tx: &mpsc::UnboundedSender<SettleObserved>) -> Step {
        apply_fetch(fetched, 3, Duration::from_secs(60), "lp::1220", cache, &None, tx).await
    }

    // A batch cut short is still applied and moves the cursor; the client reconnects
    #[tokio::test]
    async fn a_partial_batch_is_applied_and_reconnects() {
        let cache = HoldingsCache::new(false);
        let far = std::time::Instant::now() + Duration::from_secs(600);
        assert!(cache.reserve_v2(&["h1".to_string()], "q1", far).await);
        let (tx, mut rx) = mpsc::unbounded_channel();

        let partial = Fetch::Batch { updates: vec![tx_at(5, vec![archived("h1")]), checkpoint(9)], complete: false };
        let step = apply(partial, &cache, &tx).await;
        assert_eq!(step, Step { cursor: 9, venue_changed: false, reconnect: true });
        let seen = rx.try_recv().unwrap();
        assert_eq!((seen.quote_id.as_str(), seen.update_id.as_str()), ("q1", "u5"));

        let complete = Fetch::Batch { updates: vec![checkpoint(12)], complete: true };
        assert_eq!(apply(complete, &cache, &tx).await, Step { cursor: 12, venue_changed: false, reconnect: false });
    }

    #[tokio::test]
    async fn a_failed_or_unanswered_poll_keeps_the_cursor_and_reconnects() {
        let cache = HoldingsCache::new(false);
        let (tx, _rx) = mpsc::unbounded_channel();
        let kept = Step { cursor: 3, venue_changed: false, reconnect: true };
        assert_eq!(apply(Fetch::Failed(anyhow::anyhow!("boom")), &cache, &tx).await, kept);
        assert_eq!(apply(Fetch::TimedOut, &cache, &tx).await, kept);
        let empty_partial = Fetch::Batch { updates: Vec::new(), complete: false };
        assert_eq!(apply(empty_partial, &cache, &tx).await, kept);
    }

    #[tokio::test(start_paused = true)]
    async fn a_get_updates_call_that_never_answers_is_bounded() {
        let shutdown = Shutdown::new();
        let hung = std::future::pending::<anyhow::Result<(Vec<GetUpdatesResponse>, bool)>>();
        let fetched = tokio::time::timeout(Duration::from_secs(3600), fetch(&shutdown, Duration::from_secs(60), hung))
            .await
            .expect("the backstop ends the call");
        assert!(matches!(fetched, Some(Fetch::TimedOut)));
        shutdown.signal();
        let hung = std::future::pending::<anyhow::Result<(Vec<GetUpdatesResponse>, bool)>>();
        assert!(fetch(&shutdown, Duration::MAX, hung).await.is_none());
    }

    // The backstop never cuts a call the client itself still bounds
    #[test]
    fn the_backstop_is_above_the_client_bounds() {
        let mut config = BaseConfig::test_minimal().unwrap();
        for request in [1u64, 120, 3600] {
            config.request_timeout_secs = request;
            let open_and_stream = Duration::from_secs(request + 5 + request + 30);
            assert!(backstop(&config) > open_and_stream, "request {request}");
        }
    }

    // A restarted run used to re-seed at the ledger end and miss everything in between
    #[test]
    fn a_restart_resumes_from_the_last_applied_offset() {
        let mut resume = Resume::default();
        assert_eq!(resume.seed(), None, "the first run seeds at the ledger end");
        resume.record(10);
        resume.record(15);
        assert_eq!(resume.seed(), Some(15));
    }

    #[test]
    fn repeated_restarts_at_one_cursor_fall_back_to_the_ledger_end() {
        let mut resume = Resume::default();
        resume.record(15);
        for _ in 0..RESUME_RETRIES {
            assert_eq!(resume.seed(), Some(15));
            resume.record(15);
        }
        assert_eq!(resume.seed(), None, "a batch that keeps failing is skipped");
        assert_eq!(resume.cursor, None);
        assert_eq!(resume.seed(), None);
    }

    #[test]
    fn advancing_the_cursor_resets_the_retry_count() {
        let mut resume = Resume::default();
        resume.record(15);
        assert_eq!(resume.seed(), Some(15));
        resume.record(20);
        assert_eq!(resume.retries_at_cursor, 0);
        for _ in 0..RESUME_RETRIES {
            assert_eq!(resume.seed(), Some(20));
        }
    }

    /// Run the watcher against `ledger` until `done` holds; returns the saved cursor.
    async fn run_until(
        ledger: &FakeLedger,
        resume: SharedResume,
        done: impl Fn(&FakeLedger, &SharedResume) -> bool,
    ) -> Option<i64> {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = ledger.url.clone();
        let registry = Arc::new(VenueRegistry::new("lp::1220".into(), String::new(), Default::default()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let shutdown = Shutdown::new();
        let task = tokio::spawn(run(config, HoldingsCache::new(false), None, registry, tx, 1, shutdown.clone(), resume.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !done(ledger, &resume) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the watcher gets there");
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
        let cursor = sync::lock(&resume).cursor;
        cursor
    }

    /// The fake's GetUpdates answer: a checkpoint at this offset.
    const POLLED_TO: i64 = 50;

    fn saved_poll(_: &FakeLedger, resume: &SharedResume) -> bool {
        sync::lock(resume).cursor == Some(POLLED_TO)
    }

    fn polled(ledger: &FakeLedger, _: &SharedResume) -> bool {
        ledger.calls("GetUpdates") > 0
    }

    // A poll's progress used to be lost on a restart unless it was saved as it was applied
    #[tokio::test]
    async fn a_run_resumes_from_the_saved_cursor_and_saves_each_poll() {
        let ledger = FakeLedger::start(Fake::Progress).await;
        let resume = SharedResume::default();
        sync::lock(&resume).record(42);
        assert_eq!(run_until(&ledger, resume, saved_poll).await, Some(POLLED_TO));
        assert_eq!(ledger.calls("GetLedgerEnd"), 0, "a saved cursor is not re-seeded");
        ledger.stop().await;
    }

    #[tokio::test]
    async fn a_fresh_run_seeds_at_the_ledger_end_and_saves_each_poll() {
        let ledger = FakeLedger::start(Fake::Progress).await;
        assert_eq!(run_until(&ledger, SharedResume::default(), saved_poll).await, Some(POLLED_TO));
        assert_eq!(ledger.calls("GetLedgerEnd"), 1);
        ledger.stop().await;
    }

    // The seed is saved before the first poll ends, so a restart resumes there
    #[tokio::test]
    async fn the_seed_is_saved_before_the_first_poll_ends() {
        let ledger = FakeLedger::start(Fake::StreamStall).await;
        assert_eq!(run_until(&ledger, SharedResume::default(), polled).await, Some(0));
        assert_eq!(ledger.calls("GetLedgerEnd"), 1);
        ledger.stop().await;
    }

    #[test]
    fn spawning_outside_a_runtime_is_an_error() {
        let config = BaseConfig::test_minimal().unwrap();
        let registry = Arc::new(VenueRegistry::new("lp::1220".into(), String::new(), Default::default()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let spawned = spawn_updates_worker(config, HoldingsCache::new(false), None, registry, tx, 2, Shutdown::new());
        assert!(spawned.is_err());
    }
}
