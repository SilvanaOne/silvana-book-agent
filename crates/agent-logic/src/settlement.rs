//! Settlement execution for the orderbook agent
//!
//! Uses an event-driven architecture: polls GetSettlementStatus for each active
//! settlement to get the server-computed NextAction, then dispatches to the
//! appropriate handler.
//!
//! The executor is generic over `SettlementBackend`, allowing different
//! implementations for direct ledger access vs. cloud proxy.
//!
//! Settlements advance in parallel via tokio::spawn, bounded by a semaphore.
//! Each spawned task receives cloned deps and returns AdvanceResult via oneshot.
//! The main thread applies results to active_settlements.
//!
//! Flow: ProposalCreated → preconfirm → poll NextAction
//!       → PAY_DVP_FEE → pay fee
//!       → CREATE_DVP → propose DVP (buyer)
//!       → ACCEPT_DVP → accept DVP (seller)
//!       → PAY_ALLOC_FEE → pay fee
//!       → ALLOCATE → allocate tokens + save disclosed contracts
//!       → WAIT → sleep (counterparty's turn)
//!       → NONE → done (settled/failed/cancelled)

use anyhow::Result;
use async_trait::async_trait;
use futures::future::join_all;
use indexmap::IndexMap;
use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, error, info, warn};

use orderbook_proto::{
    orderbook::{SettlementProposal, SettlementUpdate, settlement_update::EventType},
    NextAction,
    RecordSettlementEventRequest, SettlementEventType, SettlementEventResult, RecordedByRole,
};

use crate::auth::generate_jwt;
use crate::client::OrderbookClient;
use crate::config::BaseConfig;
use crate::liquidity::{self, LiquidityManager};
use crate::order_tracker::{OrderTracker, VerifyResult};
use crate::rpc_client::OrderbookRpcClient;
use crate::runner::{AcceptedRfqTrade, QuotedTrade};
use crate::shutdown::Shutdown;
use crate::types::{AdvanceResult, CidWaitingType, FailedSettlement, SettlementStage, SettlementState};

/// Result from a settlement step operation
#[derive(Debug, Clone)]
pub struct StepResult {
    pub contract_id: String,
    pub update_id: String,
    pub traffic_total: u64,
}

/// Maximum number of spawn-loop iterations that may actually spawn a task in a
/// single `advance_all_settlements` call. Bounds worst-case spawn-loop cost
/// when `failed_settlements` is empty (post-restart) or when many cooldowns
/// expire simultaneously. Remaining proposals are picked up by the next 2s
/// `collect_and_readvance` tick.
const MAX_ADVANCE_SPAWNS_PER_CYCLE: usize = 50;

/// Advance cadence for proposals already past their deadline. Their retry/Wait
/// cooldowns are capped to this in `apply_result`, and the backoff bypass in
/// `advance_all_settlements` only cuts short cooldowns LONGER than this — so an
/// expired proposal is advanced (and its reservation released by the deadline
/// watchdog) within ~30s, without the 2s-tick hammering a full bypass would
/// cause, and without burning through retries in seconds during an RPC outage.
const EXPIRED_RETRY_SECS: u64 = 30;

/// Extract the 48-bit ms-since-epoch timestamp from a UUID-v7 string.
/// Used to sort the spawn queue freshest-first so a brand-new RFQ-driven
/// proposal is never starved behind stale buyer-abandoned ones.
/// Returns 0 on parse failure (sorts oldest — never starves valid work).
fn uuid_v7_ms(id: &str) -> u64 {
    let mut chars = id.chars().filter(|c| *c != '-');
    let mut hex = String::with_capacity(12);
    for _ in 0..12 {
        match chars.next() {
            Some(c) => hex.push(c),
            None => return 0,
        }
    }
    u64::from_str_radix(&hex, 16).unwrap_or(0)
}

/// A contract discovered via on-chain sync
#[derive(Debug, Clone)]
pub struct DiscoveredContract {
    pub settlement_id: String,
    pub contract_id: String,
    /// "DvpProposal" or "Dvp"
    pub contract_type: String,
}

/// Backend for executing settlement operations
///
/// Implementations provide the actual transaction execution:
/// - `DirectSettlementBackend` — calls Canton ledger API directly
/// - (future) `CloudSettlementBackend` — calls LedgerGatewayService via gRPC
#[async_trait]
pub trait SettlementBackend: Send + Sync {
    /// Pay DVP or allocation processing fee
    async fn pay_fee(&self, proposal_id: &str, fee_type: &str) -> Result<StepResult>;

    /// Propose DVP (buyer only)
    async fn propose_dvp(&self, proposal_id: &str) -> Result<StepResult>;

    /// Accept DVP proposal (seller only)
    async fn accept_dvp(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        expected_delivery_amount: &str,
        expected_payment_amount: &str,
        base_instrument: &str,
        quote_instrument: &str,
    ) -> Result<StepResult>;

    /// Allocate tokens for settlement.
    /// `allocation_cc`: Some(amount) if allocating CC amulets (needs amulet pre-selection),
    /// None if allocating CIP-56 tokens.
    async fn allocate(&self, proposal_id: &str, dvp_cid: &str, allocation_cc: Option<Decimal>) -> Result<StepResult>;

    /// Sync on-chain contracts for given settlement IDs
    async fn sync_contracts(&self, settlement_ids: &[String]) -> Result<Vec<DiscoveredContract>>;

    /// Get current payment queue depth: (allocations, fees).
    fn queue_depth(&self) -> (u64, u64);

    /// Get amulet cache stats: (available, consumed, reserved, selectable).
    /// Returns None if the backend doesn't use an amulet cache.
    fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        None
    }

    /// Get per-pool worker utilization:
    /// (alloc_active, alloc_max, fee_active, fee_max)
    fn worker_utilization(&self) -> Option<(u64, usize, u64, usize)> {
        None
    }

    /// Per-instrument RFQ V2 holdings histogram for the LIQUIDITY log, keyed by
    /// LiquidityManager token symbol. Returns None if the backend has no
    /// holdings cache. Computed in the backend (which owns the cache + USD
    /// prices) and returned as plain data.
    fn holdings_histogram(&self, _token: &str) -> Option<HoldingsHistogram> {
        None
    }

    /// Check if regular fees are paused (sequencer backpressure).
    /// Returns Some(remaining_secs) if paused, None otherwise.
    fn fee_pause_secs(&self) -> Option<u64> {
        None
    }

    /// Check if fees are paused due to low issuance forecast.
    /// LOW coefficient means heavy sequencer load — txs would hit
    /// SEQUENCER_BACKPRESSURE errors.
    fn forecast_paused(&self) -> bool {
        crate::forecast::is_fees_paused_by_overload()
    }

    /// Signal the payment queue to stop dispatching new work (for graceful shutdown).
    fn shutdown(&self) {}

    /// Get the liquidity manager (if available).
    fn liquidity_manager(&self) -> Option<Arc<crate::liquidity::LiquidityManager>> {
        None
    }
}

/// One instrument's RFQ V2 holdings, bucketed by USD value for the LIQUIDITY
/// heartbeat log. `total`/`reserved` are always meaningful; the USD buckets are
/// only populated when `priced` (a USD price was available). Buckets are
/// half-open: `under_10` = [0,10), `b10_20` = [10,20), … `over_100` = [100,∞).
#[derive(Clone, Copy, Default, Debug)]
pub struct HoldingsHistogram {
    pub total: usize,
    pub reserved: usize,
    pub under_10: usize,
    pub b10_20: usize,
    pub b20_50: usize,
    pub b50_100: usize,
    pub over_100: usize,
    pub priced: bool,
}

/// Settlement executor handles the DVP workflow
pub struct SettlementExecutor<B: SettlementBackend> {
    config: BaseConfig,
    backend: Arc<B>,
    active_settlements: IndexMap<String, SettlementState>,
    /// Proposals we already rejected — skip in polling to avoid re-discovery loop
    rejected_proposals: HashSet<String>,
    /// Proposals that completed successfully — skip in polling to avoid re-processing
    completed_proposals: HashSet<String>,
    /// Shared with the runner — same `AtomicBool` + `Notify`. Polled at the
    /// top of every loop iteration and used to cancel all bare jitter sleeps
    /// inside spawn_settlement_task.
    shutdown: Shutdown,
    tracker: Arc<Mutex<OrderTracker>>,
    /// Client for querying orders from server (user order lookup)
    query_client: Option<OrderbookClient>,
    // Parallel processing
    semaphore: Arc<Semaphore>,
    in_progress: HashMap<String, Instant>,
    failed_settlements: HashMap<String, FailedSettlement>,
    pending_results: Vec<(String, tokio::sync::oneshot::Receiver<(AdvanceResult, SettlementState)>)>,
    task_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Settlements that completed a step inline and need re-advancing on the next tick
    needs_readvance: HashSet<String>,
    /// Shared log for consolidating NextAction entries across parallel tasks
    action_log: Arc<Mutex<Vec<(String, &'static str)>>>,
    /// Shared counter of settlements where this agent must act (not waiting/terminal)
    actionable_count: Arc<AtomicUsize>,
    /// Buyer: accepted RFQ trades keyed by proposal_id (for settlement verification)
    accepted_rfq_trades: Option<Arc<Mutex<HashMap<String, AcceptedRfqTrade>>>>,
    /// Buyer: proposal_ids we've rejected — fill loop drains this to undo
    /// optimistic `filled_total` bookkeeping when a quote fails to settle.
    rejected_rfq_trades: Option<Arc<Mutex<HashSet<String>>>>,
    /// LP: trades we quoted on (for settlement verification by attribute matching)
    quoted_rfq_trades: Option<Arc<Mutex<Vec<QuotedTrade>>>>,
    /// Skip all verification and accept every proposal (migration from old worker without saved state)
    no_reject: bool,
    /// Liquidity manager for balance tracking and commitment gating
    liquidity_manager: Option<Arc<LiquidityManager>>,
}

impl<B: SettlementBackend + 'static> SettlementExecutor<B> {
    /// Create a new settlement executor with shared order tracker and backend
    pub fn new(config: &BaseConfig, tracker: Arc<Mutex<OrderTracker>>, backend: B) -> Self {
        let thread_count = config.settlement_thread_count;
        Self {
            config: config.clone(),
            backend: Arc::new(backend),
            active_settlements: IndexMap::new(),
            rejected_proposals: HashSet::new(),
            completed_proposals: HashSet::new(),
            shutdown: Shutdown::new(),
            tracker,
            query_client: None,
            semaphore: Arc::new(Semaphore::new(thread_count)),
            in_progress: HashMap::new(),
            failed_settlements: HashMap::new(),
            pending_results: Vec::new(),
            task_handles: Vec::new(),
            needs_readvance: HashSet::new(),
            action_log: Arc::new(Mutex::new(Vec::new())),
            actionable_count: Arc::new(AtomicUsize::new(0)),
            accepted_rfq_trades: None,
            rejected_rfq_trades: None,
            quoted_rfq_trades: None,
            no_reject: false,
            liquidity_manager: None,
        }
    }

    /// Get the shared actionable settlement counter
    pub fn actionable_count(&self) -> Arc<AtomicUsize> {
        self.actionable_count.clone()
    }

    /// Replace the actionable count Arc with an externally-provided one
    pub fn set_actionable_count(&mut self, count: Arc<AtomicUsize>) {
        self.actionable_count = count;
    }

    /// Set buyer RFQ trade tracking (for proposal verification)
    pub fn set_accepted_rfq_trades(&mut self, trades: Arc<Mutex<HashMap<String, AcceptedRfqTrade>>>) {
        self.accepted_rfq_trades = Some(trades);
    }

    /// Set buyer RFQ rejection feedback channel. Proposal ids are pushed here
    /// whenever the executor rejects a previously-accepted proposal so that
    /// the fill loop can undo its optimistic bookkeeping.
    pub fn set_rejected_rfq_trades(&mut self, trades: Arc<Mutex<HashSet<String>>>) {
        self.rejected_rfq_trades = Some(trades);
    }

    /// Set LP quoted trade tracking (for proposal verification)
    pub fn set_quoted_rfq_trades(&mut self, trades: Arc<Mutex<Vec<QuotedTrade>>>) {
        self.quoted_rfq_trades = Some(trades);
    }

    /// Set liquidity manager for balance gating
    pub fn set_liquidity_manager(&mut self, lm: Arc<LiquidityManager>) {
        self.liquidity_manager = Some(lm);
    }

    /// Get the liquidity manager (for heartbeat stats)
    pub fn liquidity_manager(&self) -> Option<&Arc<LiquidityManager>> {
        self.liquidity_manager.as_ref()
    }

    /// Release liquidity commitment for a proposal (helper for terminal states)
    fn release_commitment(&self, proposal_id: &str) {
        if let Some(ref lm) = self.liquidity_manager {
            let lm = lm.clone();
            let pid = proposal_id.to_string();
            tokio::spawn(async move { lm.release(&pid).await });
        }
    }

    /// True when a still-tracked settlement has not yet allocated on-chain (stage
    /// strictly before `Allocated`). Only such proposals are safe for the agent
    /// to cancel on the server: cETH locks on-chain only at the reserver's own
    /// `Allocate`, so before that the reservation is internal accounting and no
    /// live/settling DVP is torn down.
    fn is_pre_allocation(&self, proposal_id: &str) -> bool {
        matches!(
            self.active_settlements.get(proposal_id).map(|s| s.stage),
            Some(
                SettlementStage::ProposalReceived
                    | SettlementStage::DvpFeePaid
                    | SettlementStage::DvpProposed
                    | SettlementStage::DvpAccepted
                    | SettlementStage::AllocationFeePaid
            )
        )
    }

    /// Best-effort server-side cancel for a proposal the agent is permanently
    /// abandoning. Without this the server keeps the proposal `pending`, so
    /// `poll_pending_proposals` re-surfaces it (and after a restart the capped
    /// `rejected_proposals` dedup set can re-adopt it) — the rediscovery ratchet.
    /// Cancelling flips it to a terminal DB status so it drops out of
    /// `get_pending_proposals`.
    ///
    /// Spawned (non-blocking) so a slow/failed RPC never stalls the apply loop.
    /// The RPC targets the orderbook server (not the sequencer), so it still
    /// works during a ledger outage. Local cleanup (release + removal) is done by
    /// the caller regardless of whether this RPC succeeds. MUST only be called
    /// when `is_pre_allocation` — never cancel a DVP the agent already allocated
    /// for, which could still settle.
    fn notify_server_cancel(&self, proposal_id: &str, reason: &str) {
        let config = self.config.clone();
        let pid = proposal_id.to_string();
        let reason = reason.to_string();
        tokio::spawn(async move {
            let jwt = match generate_jwt(
                &config.party_id,
                &config.role,
                &config.private_key_bytes,
                config.token_ttl_secs,
                Some(config.node_name.as_str()),
            ) {
                Ok(j) => j,
                Err(e) => {
                    debug!("[{}] Best-effort cancel: JWT generation failed: {}", pid, e);
                    return;
                }
            };
            let mut client = match OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await {
                Ok(c) => c,
                Err(e) => {
                    debug!("[{}] Best-effort cancel: RPC connect failed: {}", pid, e);
                    return;
                }
            };
            match client.cancel_settlement(&pid, &reason).await {
                Ok(true) => info!("[{}] Server settlement cancelled (agent abandoned): {}", pid, reason),
                Ok(false) => debug!("[{}] Server declined cancel (already terminal?)", pid),
                Err(e) => debug!("[{}] Best-effort cancel RPC failed: {}", pid, e),
            }
        });
    }

    /// True when a tracked settlement has outlived its settlement window — or its
    /// allocation window while allocation is still pending per the locally-tracked
    /// stage. Used to bypass the retry backoff so the deadline watchdog in
    /// `advance_single` (which decides on fresh server data) runs promptly and the
    /// liquidity reservation is released at the deadline, not up to a full Wait
    /// cooldown later.
    fn past_deadline(&self, proposal_id: &str) -> bool {
        let Some(state) = self.active_settlements.get(proposal_id) else {
            return false;
        };
        let Some(created_at) = &state.proposal.created_at else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let age = now - created_at.seconds;
        let (allocate_window, settle_window) = expiry_windows(
            &state.proposal.origin,
            self.config.allocate_before_secs,
            self.config.settle_before_secs,
        );
        if age > settle_window as i64 {
            return true;
        }
        let pre_allocation = matches!(
            state.stage,
            SettlementStage::ProposalReceived
                | SettlementStage::DvpFeePaid
                | SettlementStage::DvpProposed
                | SettlementStage::DvpAccepted
                | SettlementStage::AllocationFeePaid
        );
        pre_allocation && age > allocate_window as i64
    }

    /// Record the inflow (token received from the counterparty) for a settling
    /// proposal — exactly once.
    ///
    /// Reads the still-present `active_settlements` state, so callers MUST invoke
    /// this BEFORE `shift_remove`. No-op if the proposal is no longer tracked
    /// (the other terminal path already removed it). `record_inflow` is `+=` and
    /// thus NOT idempotent, so this guard is what keeps the dominant stream
    /// terminal path and the self-driven `Terminal` path from double-counting.
    fn record_settlement_inflow(&self, proposal_id: &str) {
        if let (Some(lm), Some(state)) =
            (&self.liquidity_manager, self.active_settlements.get(proposal_id))
        {
            let received_token = if state.is_buyer {
                &state.proposal.base_instrument
            } else {
                &state.proposal.quote_instrument
            };
            let received_amount = if state.is_buyer {
                &state.proposal.base_quantity
            } else {
                &state.proposal.quote_quantity
            };
            let token_key = match &self.config.cc_token_id {
                Some(cc_id) if received_token == cc_id => liquidity::CC_TOKEN.to_string(),
                _ => received_token.clone(),
            };
            if let Ok(amount) = received_amount.parse::<f64>() {
                let lm = lm.clone();
                tokio::spawn(async move { lm.record_inflow(&token_key, amount).await });
            }
        }
    }

    /// Get the liquidity manager from the backend (for initial injection)
    pub fn backend_liquidity_manager(&self) -> Option<Arc<LiquidityManager>> {
        self.backend.liquidity_manager()
    }

    /// Enable no-reject mode: accept all proposals without verification
    pub fn set_no_reject(&mut self, no_reject: bool) {
        self.no_reject = no_reject;
    }

    /// Get current payment queue depth: (allocations, fees).
    pub fn queue_depth(&self) -> (u64, u64) {
        self.backend.queue_depth()
    }

    /// Get amulet cache stats: (available, consumed, reserved, selectable).
    pub fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        self.backend.cache_stats()
    }

    /// Get per-pool worker utilization (delegated to backend).
    pub fn worker_utilization(&self) -> Option<(u64, usize, u64, usize)> {
        self.backend.worker_utilization()
    }

    /// Per-instrument RFQ V2 holdings histogram (delegated to backend).
    pub fn holdings_histogram(&self, token: &str) -> Option<HoldingsHistogram> {
        self.backend.holdings_histogram(token)
    }

    /// Check if regular fees are paused (sequencer backpressure).
    pub fn fee_pause_secs(&self) -> Option<u64> {
        self.backend.fee_pause_secs()
    }

    /// Check if fees are paused due to low issuance forecast.
    pub fn forecast_paused(&self) -> bool {
        self.backend.forecast_paused()
    }

    /// Return (in_progress, max_threads, in_backoff, waiting) for thread utilization logging.
    ///
    /// `in_progress`, `in_backoff` and `waiting` partition the active set: a
    /// backoff entry is counted only when its proposal is still active AND not
    /// currently in-progress. Without those guards the counts would overlap (a
    /// cut-short proposal spawned while still holding a future `next_retry` is
    /// both in-progress and in backoff) or count stale `failed_settlements`
    /// entries for already-removed proposals — either of which understates
    /// `waiting` and could hide genuine permit starvation.
    pub fn thread_utilization(&self) -> (usize, usize, usize, usize) {
        let now = Instant::now();
        let in_progress = self.in_progress.len();
        let in_backoff = self.failed_settlements.iter()
            .filter(|(pid, f)| now < f.next_retry
                && self.active_settlements.contains_key(*pid)
                && !self.in_progress.contains_key(*pid))
            .count();
        let total = self.active_settlements.len();
        let waiting = total.saturating_sub(in_progress).saturating_sub(in_backoff);
        (in_progress, self.config.settlement_thread_count, in_backoff, waiting)
    }

    /// Log a one-line summary of proposals waiting for CIDs (called from heartbeat).
    pub fn log_cid_waiting_summary(&self) {
        let mut no_proposal_ids: Vec<&str> = Vec::new();
        let mut no_dvp_ids: Vec<&str> = Vec::new();
        let mut stuck_10m = 0u32;

        for (id, entry) in &self.failed_settlements {
            match entry.cid_waiting {
                Some(CidWaitingType::DvpProposal) => no_proposal_ids.push(id),
                Some(CidWaitingType::DvpContract) => no_dvp_ids.push(id),
                None => continue,
            }
            if entry.first_transient_at
                .map(|t| t.elapsed().as_secs() > 600)
                .unwrap_or(false)
            {
                stuck_10m += 1;
            }
        }

        let total = no_proposal_ids.len() + no_dvp_ids.len();
        if total > 0 {
            warn!(
                "CID waiting: {} proposals ({} no DvpProposal, {} no Dvp contract, {} stuck >10min)\n  \
                 no DvpProposal: {:?}\n  no Dvp contract: {:?}",
                total, no_proposal_ids.len(), no_dvp_ids.len(), stuck_10m,
                no_proposal_ids, no_dvp_ids,
            );
        }
    }

    /// Count settlements where this agent must act (not waiting or terminal)
    fn count_actionable_settlements(&self) -> usize {
        self.active_settlements.values()
            .filter(|s| !matches!(s.stage,
                SettlementStage::AwaitingSettlement |
                SettlementStage::Settled |
                SettlementStage::Failed |
                SettlementStage::Cancelled
            ))
            .count()
    }

    /// Update the shared actionable count
    fn update_actionable_count(&self) {
        self.actionable_count.store(self.count_actionable_settlements(), Ordering::Relaxed);
    }

    /// Lazily initialize the query client for server order lookups
    async fn get_query_client(&mut self) -> Result<&mut OrderbookClient> {
        if self.query_client.is_none() {
            self.query_client = Some(OrderbookClient::new(&self.config).await?);
        }
        Ok(self.query_client.as_mut().unwrap())
    }

    /// Signal that we are shutting down — reject new proposals, drain confirmed ones
    pub fn set_shutting_down(&mut self) {
        self.shutdown.signal();
    }

    /// Signal the backend's payment queue to stop dispatching new work.
    pub fn shutdown_backend(&self) {
        self.backend.shutdown();
    }

    /// Replace the shutdown signal with an externally-provided one (shares
    /// the runner's Ctrl-C `Shutdown`). Used so spawned settlement tasks
    /// observe Ctrl-C the moment it fires and so jitter sleeps inside this
    /// module wake up immediately on shutdown.
    pub fn set_shutdown(&mut self, shutdown: Shutdown) {
        self.shutdown = shutdown;
    }

    /// Reject all unconfirmed settlements (still at ProposalReceived stage)
    pub async fn reject_unconfirmed(&mut self) {
        let unconfirmed: Vec<String> = self.active_settlements.iter()
            .filter(|(_, s)| s.stage == SettlementStage::ProposalReceived)
            .map(|(id, _)| id.clone())
            .collect();

        for proposal_id in unconfirmed {
            // Release the CC reservation + pending tracker quantity before rejecting
            self.release_commitment(&proposal_id);
            {
                let mut tracker = self.tracker.lock().await;
                tracker.mark_failed(&proposal_id);
            }
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject unconfirmed proposal: {}", proposal_id, e);
                self.active_settlements.shift_remove(&proposal_id);
            }
        }
        self.update_actionable_count();
    }

    /// Cancel a settlement proactively (e.g., on timeout or strategy change)
    ///
    /// Calls the CancelSettlement RPC and removes from active settlements.
    /// The streaming event will also arrive via handle_settlement_update.
    pub async fn cancel_settlement(&mut self, proposal_id: &str, reason: &str, config: &BaseConfig) -> Result<()> {
        let jwt = generate_jwt(
            &config.party_id,
            &config.role,
            &config.private_key_bytes,
            config.token_ttl_secs,
            Some(config.node_name.as_str()),
        )?;
        let mut rpc_client = OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await?;
        let success = rpc_client.cancel_settlement(proposal_id, reason).await?;
        if success {
            info!("[{}] Settlement cancelled: {}", proposal_id, reason);
            self.release_commitment(proposal_id);
            self.active_settlements.shift_remove(proposal_id);
            self.in_progress.remove(proposal_id);
        }
        Ok(())
    }

    /// Handle a settlement update from the stream
    pub async fn handle_settlement_update(&mut self, update: SettlementUpdate) -> Result<()> {
        let event_type = EventType::try_from(update.event_type)
            .unwrap_or(EventType::Unspecified);

        match event_type {
            EventType::ProposalCreated => {
                if let Some(proposal) = update.proposal {
                    self.handle_proposal_created(proposal).await?;
                }
            }
            EventType::StatusChanged => {
                if let Some(proposal) = update.proposal {
                    self.handle_status_changed(&proposal).await?;
                }
            }
            EventType::Settled => {
                if let Some(proposal) = update.proposal {
                    info!("Settlement completed: {}", proposal.proposal_id);
                    {
                        let mut tracker = self.tracker.lock().await;
                        tracker.mark_settled(&proposal.proposal_id);
                    }
                    // Record the inflow and release the CC allocation + fee
                    // reservation. This is the dominant terminal path (~90% of
                    // settlements complete via the stream, not the self-driven
                    // advance loop), so without these the reservation leaks and
                    // `available_cc` decays to 0 over ~2 days (starving the RFQ
                    // handler), and the depletion EMA is biased high (over-widening
                    // spreads). Both helpers no-op if the self-driven path already
                    // finalized this proposal, so there is no double-counting.
                    self.record_settlement_inflow(&proposal.proposal_id);
                    self.release_commitment(&proposal.proposal_id);
                    self.completed_proposals.insert(proposal.proposal_id.clone());
                    self.active_settlements.shift_remove(&proposal.proposal_id);
                    self.in_progress.remove(&proposal.proposal_id);
                    self.failed_settlements.remove(&proposal.proposal_id);
                }
            }
            EventType::Failed | EventType::Cancelled => {
                if let Some(proposal) = update.proposal {
                    let status = if event_type == EventType::Failed { "failed" } else { "cancelled" };
                    warn!("Settlement {}: {}", status, proposal.proposal_id);
                    {
                        let mut tracker = self.tracker.lock().await;
                        tracker.mark_failed(&proposal.proposal_id);
                    }
                    // Release the CC reservation on the stream terminal path
                    // (see EventType::Settled above — same leak applies).
                    self.release_commitment(&proposal.proposal_id);
                    self.rejected_proposals.insert(proposal.proposal_id.clone());
                    self.active_settlements.shift_remove(&proposal.proposal_id);
                    self.in_progress.remove(&proposal.proposal_id);
                    self.failed_settlements.remove(&proposal.proposal_id);
                }
            }
            _ => {}
        }

        self.update_actionable_count();
        Ok(())
    }

    /// Handle a new settlement proposal
    async fn handle_proposal_created(&mut self, proposal: SettlementProposal) -> Result<()> {
        // Deduplicate: ignore if already processing, completed, or rejected (stream replay)
        if self.active_settlements.contains_key(&proposal.proposal_id) {
            debug!("[{}] Duplicate ProposalCreated, ignoring", proposal.proposal_id);
            return Ok(());
        }
        if self.completed_proposals.contains(&proposal.proposal_id) {
            debug!("[{}] Already completed, ignoring stream replay", proposal.proposal_id);
            return Ok(());
        }
        if self.rejected_proposals.contains(&proposal.proposal_id) {
            debug!("[{}] Already rejected, ignoring stream replay", proposal.proposal_id);
            return Ok(());
        }

        // Skip proposals that already settled on-chain — on restart the agent may
        // rediscover them via polling but should not attempt to reject them.
        if proposal.settled_at.is_some() {
            info!(
                "[{}] Already settled on-chain, adding to completed set",
                proposal.proposal_id
            );
            self.completed_proposals.insert(proposal.proposal_id.clone());
            return Ok(());
        }

        let is_buyer = proposal.buyer == self.config.party_id;
        let is_seller = proposal.seller == self.config.party_id;

        if !is_buyer && !is_seller {
            return Ok(());
        }

        let proposal_id = proposal.proposal_id.clone();

        // Check if this proposal was already verified and tracked before shutdown.
        // settlement_orders is restored from saved state — if present, the proposal
        // was previously accepted and pending_quantity is already accounted for.
        // Skip verification (RFQ trade may have been consumed) and mark_pending
        // (would double-count pending_quantity).
        {
            let tracker = self.tracker.lock().await;
            if tracker.has_settlement_order(&proposal_id) {
                drop(tracker);

                // Restart hygiene: don't re-adopt/re-advance a proposal already past
                // its settle deadline. It cannot complete (its on-chain settleBefore
                // has passed) and would otherwise sit in active_settlements churning
                // re-advances until the watchdog grinds it down — the mechanism that
                // ballooned active_settlements across restarts during the outage.
                // Best-effort cancel it on the server so it reaches a terminal DB
                // status; the server's own settled/in-flight guards make this safe
                // even if it had allocated (past settleBefore it can't settle anyway).
                if let Some(created_at) = &proposal.created_at {
                    let (_allocate_window, settle_window) = expiry_windows(
                        &proposal.origin,
                        self.config.allocate_before_secs,
                        self.config.settle_before_secs,
                    );
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                    if now - created_at.seconds > settle_window as i64 {
                        info!(
                            "[{}] Restored proposal past settle deadline ({}s old, max {}s) — abandoning instead of re-advancing",
                            proposal_id, now - created_at.seconds, settle_window
                        );
                        // Release the order's pending_quantity + drop the
                        // settlement_orders entry, exactly as the exhausted-abandon
                        // arms do. Without this, abandoning here (never re-adopting,
                        // so the deadline watchdog never runs and — if the server
                        // declines the cancel because the proposal already allocated
                        // — no terminal stream event ever fires) permanently strands
                        // the reserved pending_quantity, understating that order's
                        // remaining capacity across restarts.
                        {
                            let mut t = self.tracker.lock().await;
                            t.mark_failed(&proposal_id);
                        }
                        self.rejected_proposals.insert(proposal_id.clone());
                        self.notify_server_cancel(&proposal_id, "agent abandoned on restart: past settle deadline");
                        self.update_actionable_count();
                        return Ok(());
                    }
                }

                info!(
                    "[{}] Restored proposal from saved state, skipping re-verification (role: {})",
                    proposal_id,
                    if is_buyer { "buyer" } else { "seller" }
                );
                let state = SettlementState::new(proposal, is_buyer);
                self.active_settlements.insert(proposal_id.clone(), state);
                if self.config.auto_settle {
                    self.needs_readvance.insert(proposal_id.clone());
                }
                self.update_actionable_count();
                return Ok(());
            }
        }

        // --no-reject mode: skip all verification, accept every proposal.
        // Used when migrating from an old worker that didn't save state —
        // start_time_ms = now would cause all pre-existing orders to fail
        // the nonce check, and RFQ trade maps are empty.
        if self.no_reject {
            info!(
                "[{}] Accepting proposal without verification (--no-reject mode, role: {})",
                proposal_id,
                if is_buyer { "buyer" } else { "seller" }
            );
            let order_id = proposal.order_match.as_ref().map_or(0u64, |om| {
                if is_buyer { om.bid_order_id } else { om.offer_order_id }
            });
            {
                let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
                let mut tracker = self.tracker.lock().await;
                tracker.record_settlement_order(&proposal_id, order_id, base_quantity);
            }
            let state = SettlementState::new(proposal, is_buyer);
            self.active_settlements.insert(proposal_id.clone(), state);
            if self.config.auto_settle {
                self.needs_readvance.insert(proposal_id.clone());
            }
            self.update_actionable_count();
            return Ok(());
        }

        info!(
            "New settlement proposal: {} (role: {})",
            proposal_id,
            if is_buyer { "buyer" } else { "seller" }
        );

        // Reject new proposals during shutdown
        if self.shutdown.is_shutting_down() {
            info!("[{}] Rejecting proposal (shutting down)", proposal_id);
            let state = SettlementState::new(proposal, is_buyer);
            self.active_settlements.insert(proposal_id.clone(), state);
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject proposal during shutdown: {}", proposal_id, e);
                self.active_settlements.shift_remove(&proposal_id);
            }
            return Ok(());
        }

        // Per-counterparty cap: refuse new proposals from a counterparty that
        // already has too many pending settlements. Prevents a broken or
        // spamming counterparty (one that never preconfirms its side) from
        // piling up one-sided settlements that would otherwise sit until the
        // server's allocation-window timeout. Dedup above guarantees a live
        // already-adopted proposal is never re-checked here.
        let counterparty = if is_buyer { &proposal.seller } else { &proposal.buyer };
        let cp_pending = self
            .active_settlements
            .values()
            .filter(|s| {
                let cp = if s.is_buyer { &s.proposal.seller } else { &s.proposal.buyer };
                cp == counterparty
            })
            .count();
        if cp_pending >= self.config.max_pending_per_counterparty {
            warn!(
                "[{}] Rejecting proposal: counterparty {} has {} pending settlements (cap {})",
                proposal_id, counterparty, cp_pending, self.config.max_pending_per_counterparty
            );
            // Mirror the liquidity-gate reject below (incl. the RFQ feedback
            // insert — a cap-rejected buyer-RFQ proposal must revert the fill
            // loop's optimistic accounting).
            let state = SettlementState::new(proposal, is_buyer);
            self.active_settlements.insert(proposal_id.clone(), state);
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject: {}", proposal_id, e);
                self.active_settlements.shift_remove(&proposal_id);
            }
            if let Some(ref rejected) = self.rejected_rfq_trades {
                rejected.lock().await.insert(proposal_id.clone());
            }
            return Ok(());
        }

        // Liquidity gate (advisory): reject if agent lacks balance for
        // allocation + fees. Mirrors RFQ V2's indicative-phase availability
        // check — no commitment is made here. The actual reservation (LM
        // commitment + order pending_quantity + depletion outflow) is deferred
        // to `ensure_reserved`, which runs on the first post-preconfirm action,
        // i.e. once the counterparty has preconfirmed. One-sided proposals from
        // a counterparty that never commits therefore reserve nothing.
        // Don't reject based on zero balances at startup — wait for ACS worker to load them
        if let Some(ref lm) = self.liquidity_manager {
            if !lm.is_ready().await {
                info!("[{}] Balances not loaded yet, deferring preconfirmation", proposal_id);
                return Ok(());
            }
            let (allocation_token, allocation_amount, my_fees_usd) =
                reservation_inputs(&proposal, is_buyer, &self.config.cc_token_id);
            let fee_cc = lm.estimate_fee_cc(my_fees_usd).await;

            if let Err(reason) = lm.can_commit(&allocation_token, allocation_amount, fee_cc).await {
                warn!("[{}] Rejecting proposal: {}", proposal_id, reason);
                let state = SettlementState::new(proposal, is_buyer);
                self.active_settlements.insert(proposal_id.clone(), state);
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
                    self.active_settlements.shift_remove(&proposal_id);
                }
                if let Some(ref rejected) = self.rejected_rfq_trades {
                    rejected.lock().await.insert(proposal_id.clone());
                }
                return Ok(());
            }
        }

        // RFQ proposals (no order_match) — verify against agent's in-memory state
        if proposal.order_match.is_none() {
            let rfq_verified = self.verify_rfq_proposal(&proposal).await;
            if !rfq_verified {
                warn!("[{}] RFQ proposal rejected: not in agent's tracked RFQ state", proposal_id);
                let state = SettlementState::new(proposal, is_buyer);
                self.active_settlements.insert(proposal_id.clone(), state);
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
                    self.active_settlements.shift_remove(&proposal_id);
                }
                if let Some(ref rejected) = self.rejected_rfq_trades {
                    rejected.lock().await.insert(proposal_id.clone());
                }
                return Ok(());
            }
            // RFQ verified — order_id=0 (no orderbook order to track)
            let order_id = 0u64;
            {
                let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
                let mut tracker = self.tracker.lock().await;
                tracker.record_settlement_order(&proposal_id, order_id, base_quantity);
            }
            let state = SettlementState::new(proposal, is_buyer);
            self.active_settlements.insert(proposal_id.clone(), state);
            if self.config.auto_settle {
                self.needs_readvance.insert(proposal_id.clone());
            }
            self.update_actionable_count();
            return Ok(());
        }

        // Orderbook proposals — verify order via tracker
        let verify_result = {
            let tracker = self.tracker.lock().await;
            tracker.verify_settlement(&proposal, &self.config.party_id)
        };

        let order_id = match verify_result {
            VerifyResult::Accepted { order_id } => order_id,
            VerifyResult::Rejected { reason } => {
                warn!("[{}] Settlement rejected: {}", proposal_id, reason);
                let state = SettlementState::new(proposal, is_buyer);
                self.active_settlements.insert(proposal_id.clone(), state);
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
                    self.active_settlements.shift_remove(&proposal_id);
                }
                return Ok(());
            }
            VerifyResult::NeedServerLookup { order_id: lookup_id } => {
                // Path B: User order — fetch from server and verify
                info!("[{}] Order {} not in tracker, fetching from server", proposal_id, lookup_id);
                let market_id = proposal.market_id.clone();
                match self.verify_user_order(&proposal, lookup_id, &market_id).await {
                    Ok(oid) => oid,
                    Err(reason) => {
                        warn!("[{}] User order verification failed: {}", proposal_id, reason);
                        let state = SettlementState::new(proposal, is_buyer);
                        self.active_settlements.insert(proposal_id.clone(), state);
                        if let Err(e) = self.reject_proposal(&proposal_id).await {
                            warn!("[{}] Failed to reject: {}", proposal_id, e);
                            self.active_settlements.shift_remove(&proposal_id);
                        }
                        return Ok(());
                    }
                }
            }
        };

        // Order verified — record the local adoption decision and proceed.
        // (Capacity reservation is deferred to the counterparty's preconfirm.)
        {
            let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
            let mut tracker = self.tracker.lock().await;
            tracker.record_settlement_order(&proposal_id, order_id, base_quantity);
        }

        let state = SettlementState::new(proposal, is_buyer);
        self.active_settlements.insert(proposal_id.clone(), state);

        // Mark for advancement by the parallel thread pool
        if self.config.auto_settle {
            self.needs_readvance.insert(proposal_id.clone());
        }

        Ok(())
    }

    /// Handle status change for an existing settlement
    async fn handle_status_changed(&mut self, proposal: &SettlementProposal) -> Result<()> {
        if let Some(state) = self.active_settlements.get(&proposal.proposal_id) {
            // Skip if status hasn't actually changed (stream replays)
            if state.proposal.status == proposal.status {
                return Ok(());
            }
            let status_name = match proposal.status {
                0 => "Unspecified",
                1 => "Pending",
                2 => "Settled",
                3 => "Cancelled",
                4 => "Failed",
                _ => "Unknown",
            };
            info!("[{}] Settlement status changed: {}", proposal.proposal_id, status_name);
            self.needs_readvance.insert(proposal.proposal_id.clone());
        }
        Ok(())
    }

    /// Advance all active settlements in parallel (called by polling timer)
    ///
    /// Spawns each settlement as a tokio task bounded by the semaphore.
    /// Results are collected on the next call via `collect_results()`.
    pub async fn advance_all_settlements(&mut self) {
        // First, collect results from previously spawned tasks
        let _ = self.collect_results().await;

        // Drain needs_readvance: these settlements have actual work to do
        // (on-chain state change detected, step completed in-task, etc.).
        // Clear their cooldowns so they're immediately eligible, and spawn
        // them before the rest to prevent starvation by Wait polling.
        let mut priority_ids: Vec<String> = self.needs_readvance.drain()
            .filter(|pid| self.active_settlements.contains_key(pid))
            .collect();
        for pid in &priority_ids {
            self.failed_settlements.remove(pid);
        }
        // Within priority bucket, newest first.
        priority_ids.sort_by(|a, b| uuid_v7_ms(b).cmp(&uuid_v7_ms(a)));

        // Build spawn list: priority IDs first, then remaining active settlements
        // sorted newest-first by UUID-v7 timestamp. Fresh RFQ-driven proposals
        // must never queue behind stale buyer-abandoned flows.
        // HashSet for O(1) membership checks (avoids O(n²) Vec::contains at scale).
        let seen: HashSet<&str> = priority_ids.iter().map(|s| s.as_str()).collect();
        let mut tail: Vec<String> = self.active_settlements.keys()
            .filter(|k| !seen.contains(k.as_str()))
            .cloned()
            .collect();
        drop(seen);
        tail.sort_by(|a, b| uuid_v7_ms(b).cmp(&uuid_v7_ms(a)));

        let mut proposal_ids: Vec<String> = Vec::with_capacity(priority_ids.len() + tail.len());
        proposal_ids.extend(priority_ids);
        proposal_ids.extend(tail);

        let total_proposals = proposal_ids.len();
        let mut spawned: usize = 0;
        let mut skipped_in_progress: usize = 0;
        let mut skipped_backoff: usize = 0;
        let mut hit_cap = false;

        for proposal_id in proposal_ids {
            // Skip if already being processed by a spawned task
            if self.in_progress.contains_key(&proposal_id) {
                skipped_in_progress += 1;
                continue;
            }

            // Skip if in backoff from a previous failure. A proposal already past
            // its deadline cuts short a long PRE-expiry cooldown once (so the
            // deadline watchdog can release the reservation promptly instead of
            // after up to 10 min of Wait cooldown); cooldowns set AFTER expiry are
            // already capped at EXPIRED_RETRY_SECS in apply_result, bounding the
            // advance cadence rather than re-polling on every 2s tick.
            if let Some(f) = self.failed_settlements.get(&proposal_id) {
                if Instant::now() < f.next_retry {
                    let cut_short = f.next_retry
                        > Instant::now() + Duration::from_secs(EXPIRED_RETRY_SECS)
                        && self.past_deadline(&proposal_id);
                    if !cut_short {
                        skipped_backoff += 1;
                        continue;
                    }
                }
            }

            // Cap actual spawns per cycle. Remaining proposals get picked up
            // on the next 2s `collect_and_readvance` tick. Skipped items
            // (already in_progress / in backoff) don't count toward the cap.
            if spawned >= MAX_ADVANCE_SPAWNS_PER_CYCLE {
                hit_cap = true;
                break;
            }

            // Try to acquire a semaphore permit (non-blocking)
            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // Report waiting (runnable, blocked only on a permit) and
                    // backoff (cooling down, not runnable yet) separately —
                    // lumping them together made a burst of cooldown wake-ups
                    // read as a huge runnable queue. Reaching here always means
                    // the current proposal is runnable (it passed the
                    // in-progress and backoff skips above) yet no permit was
                    // free, so there is genuine permit contention → warn.
                    let (in_progress, max_threads, in_backoff, waiting) =
                        self.thread_utilization();
                    warn!(
                        "All {} settlement threads busy ({} in-progress, {} waiting, {} in backoff), will retry next cycle",
                        max_threads, in_progress, waiting, in_backoff,
                    );
                    break;
                }
            };

            self.spawn_settlement_task(proposal_id, permit, true);
            spawned += 1;

            // No spawn-site stagger: spawned tasks each do 0–2s jitter inside
            // before any Canton tx (see spawn_settlement_task initial_jitter),
            // and the semaphore caps RPC concurrency.
            if self.shutdown.is_shutting_down() {
                break;
            }
        }

        if hit_cap {
            let deferred = total_proposals
                .saturating_sub(spawned)
                .saturating_sub(skipped_in_progress)
                .saturating_sub(skipped_backoff);
            info!(
                "advance_all hit spawn cap ({}): {} deferred to next cycle (in_progress={}, backoff={})",
                MAX_ADVANCE_SPAWNS_PER_CYCLE, deferred, skipped_in_progress, skipped_backoff,
            );
        }
    }

    /// Advance a single settlement immediately (triggered by stream update).
    /// Clears backoff, checks preconditions, spawns task if eligible.
    /// Unlike advance_all_settlements(), does NOT iterate all settlements.
    pub async fn advance_proposal(&mut self, proposal_id: &str) {
        // Collect finished results first (frees semaphore permits)
        let _ = self.collect_results().await;

        // Must be active and not already in-progress
        if !self.active_settlements.contains_key(proposal_id) { return; }
        if self.in_progress.contains_key(proposal_id) { return; }

        // Clear backoff — stream update means new state to process
        self.failed_settlements.remove(proposal_id);
        self.needs_readvance.remove(proposal_id);

        // Try semaphore permit (non-blocking)
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => return, // All threads busy, poll cycle will pick it up
        };

        // Spawn without jitter — stream update means act NOW
        self.spawn_settlement_task(proposal_id.to_string(), permit, false);
    }

    /// Spawn a single settlement advancement task.
    /// Shared by advance_all_settlements() and advance_proposal().
    fn spawn_settlement_task(
        &mut self,
        proposal_id: String,
        permit: tokio::sync::OwnedSemaphorePermit,
        initial_jitter: bool,
    ) {
        let state = self.active_settlements.get(&proposal_id).unwrap().clone();
        let config = self.config.clone();
        let backend = Arc::clone(&self.backend);
        let tracker = Arc::clone(&self.tracker);
        let liquidity_manager = self.liquidity_manager.clone();
        let shutdown = self.shutdown.clone();
        let action_log = Arc::clone(&self.action_log);
        let pid = proposal_id.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();

        self.in_progress.insert(proposal_id.clone(), Instant::now());
        self.pending_results.push((proposal_id, rx));

        let handle = tokio::spawn(async move {
            let mut permit = Some(permit); // droppable before allocate
            let mut local_state = state;

            if initial_jitter {
                // Initial jitter to stagger threads (0-2s) — wakes early on shutdown
                let jitter = rand::thread_rng().gen_range(0..2000u64);
                shutdown.sleep(Duration::from_millis(jitter)).await;
            }

            loop {
                // Check shutdown flag before each step
                let is_shutting_down = shutdown.is_shutting_down();

                let result = tokio::time::timeout(Duration::from_secs(300), async {
                    advance_single(
                        pid.clone(),
                        local_state.clone(),
                        config.clone(),
                        backend.clone(),
                        tracker.clone(),
                        liquidity_manager.clone(),
                        is_shutting_down,
                        action_log.clone(),
                    )
                    .await
                })
                .await;

                let advance_result = match result {
                    Ok(r) => r,
                    Err(_) => {
                        error!(
                            "[{}] Settlement step timed out after 300s (role={}, stage={})",
                            pid,
                            if local_state.is_buyer { "buyer" } else { "seller" },
                            local_state.stage,
                        );
                        AdvanceResult::Timeout { proposal_id: pid.clone() }
                    }
                };

                // NeedsAllocate: release settlement permit before blocking on payment queue
                if let AdvanceResult::NeedsAllocate {
                    ref proposal_id, ref dvp_cid, allocation_cc, is_buyer,
                } = advance_result {
                    // Release settlement permit — allows other settlements to start
                    drop(permit.take());

                    let alloc_result = tokio::time::timeout(
                        Duration::from_secs(600),
                        backend.allocate(proposal_id, dvp_cid, allocation_cc),
                    ).await;
                    let alloc_result = match alloc_result {
                        Ok(r) => r,
                        Err(_) => Err(anyhow::anyhow!("Allocate timed out after 600s")),
                    };
                    match alloc_result {
                        Ok(step_result) => {
                            // Record allocation completed event
                            let event_type = if is_buyer {
                                SettlementEventType::AllocationBuyerCompleted
                            } else {
                                SettlementEventType::AllocationSellerCompleted
                            };
                            let jwt = match generate_jwt(
                                &config.party_id, &config.role, &config.private_key_bytes,
                                config.token_ttl_secs, Some(config.node_name.as_str()),
                            ) {
                                Ok(j) => j,
                                Err(e) => {
                                    warn!("[{}] JWT gen for alloc event failed: {}", proposal_id, e);
                                    String::new()
                                }
                            };
                            if !jwt.is_empty() {
                                if let Ok(mut rpc_client) = OrderbookRpcClient::connect(
                                    &config.orderbook_grpc_url, Some(jwt),
                                ).await {
                                    record_step_completed(
                                        &mut rpc_client, proposal_id, &config.party_id,
                                        is_buyer, event_type,
                                        &step_result.update_id, &step_result.contract_id,
                                    ).await;
                                }
                            }

                            let final_result = AdvanceResult::StepCompleted {
                                proposal_id: proposal_id.clone(),
                                stage: SettlementStage::Allocated,
                                dvp_proposal_cid: None,
                                dvp_cid: None,
                                allocation_cid: Some(step_result.contract_id),
                                pending_traffic: 0,
                            };
                            let _ = tx.send((final_result, local_state));
                        }
                        Err(e) => {
                            let err_result = AdvanceResult::Error {
                                proposal_id: proposal_id.clone(),
                                error: format!("Allocate failed: {:#}", e),
                            };
                            let _ = tx.send((err_result, local_state));
                        }
                    }
                    break;
                }

                if advance_result.should_readvance() && !shutdown.is_shutting_down() {
                    advance_result.apply_to_state(&mut local_state);
                    // Jitter between steps (200-1000ms) — wakes early on shutdown
                    let step_jitter = 200 + rand::thread_rng().gen_range(0..800u64);
                    shutdown.sleep(Duration::from_millis(step_jitter)).await;
                } else {
                    let _ = tx.send((advance_result, local_state));
                    break;
                }
            }
        });
        self.task_handles.push(handle);
    }


    /// Collect completed results from spawned tasks.
    ///
    /// Returns proposal IDs that completed a step and should be re-advanced.
    async fn collect_results(&mut self) -> Vec<String> {
        let mut still_pending = Vec::new();
        let mut completed = Vec::new();
        let mut readvance_ids = Vec::new();

        let mut orphaned: Vec<String> = Vec::new();
        for (proposal_id, mut rx) in self.pending_results.drain(..) {
            match rx.try_recv() {
                Ok((result, final_state)) => {
                    self.in_progress.remove(&proposal_id);
                    // Update active_settlements with accumulated state from the task
                    if let Some(state) = self.active_settlements.get_mut(&proposal_id) {
                        *state = final_state;
                    } else {
                        // Stream-terminal race: a Settled/Cancelled stream event
                        // removed this settlement while its task was mid-advance.
                        // The task may have made the reservation (ensure_reserved)
                        // AFTER the terminal handler's release ran against nothing.
                        // Release both halves — the LM half would self-heal via
                        // retain_commitments, but the tracker's pending_quantity
                        // has no reconciler and would leak permanently (and be
                        // persisted across restarts).
                        orphaned.push(proposal_id.clone());
                    }
                    completed.push(result);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still running
                    still_pending.push((proposal_id, rx));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    // Task panicked or was cancelled
                    warn!("[{}] Settlement task dropped without sending result", proposal_id);
                    self.in_progress.remove(&proposal_id);
                }
            }
        }

        self.pending_results = still_pending;

        // Release reservations for results that raced a stream terminal
        for proposal_id in orphaned {
            {
                let mut t = self.tracker.lock().await;
                t.mark_failed(&proposal_id);
            }
            self.release_commitment(&proposal_id);
        }

        // Emit consolidated NextAction summary
        let actions: Vec<(String, &'static str)> = self.action_log.lock().await.drain(..).collect();
        if !actions.is_empty() {
            let mut by_action: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (pid, action) in &actions {
                by_action.entry(action).or_default().push(pid.as_str());
            }
            let summary: Vec<String> = by_action.iter()
                .map(|(action, ids)| format!("{}({})", action, ids.join(", ")))
                .collect();
            info!("Settlement actions: {}", summary.join(", "));
        }

        // Apply all completed results
        for result in completed {
            if result.should_readvance() {
                readvance_ids.push(result.proposal_id().to_string());
            }
            self.apply_result(result).await;
        }

        // Warn about long-running tasks (may be stuck, holding semaphore permit)
        for (pid, started_at) in &self.in_progress {
            if started_at.elapsed() > Duration::from_secs(600) {
                warn!("[{}] Settlement task running for {:?} (may be stuck)", pid, started_at.elapsed());
            }
        }

        // Clean up finished JoinHandles
        self.task_handles.retain(|h| !h.is_finished());
        readvance_ids
    }

    /// Collect results from spawned tasks and re-advance any that completed a step.
    ///
    /// Called by the 2s result-collection timer in the runner. This ensures:
    /// 1. Spawned task results are collected quickly (not waiting for next poll cycle)
    /// 2. Settlements that completed a step are immediately re-advanced (looping)
    pub async fn collect_and_readvance(&mut self) {
        let readvance_ids = self.collect_results().await;

        // Merge spawned task results with needs_readvance
        for pid in readvance_ids {
            self.needs_readvance.insert(pid);
        }

        // Spawn parallel tasks for all settlements needing advancement.
        // advance_all_settlements iterates active_settlements and spawns
        // tasks for any not already in_progress or in backoff.
        self.advance_all_settlements().await;
    }

    /// Apply a single AdvanceResult to the executor state
    async fn apply_result(&mut self, result: AdvanceResult) {
        match result {
            AdvanceResult::StepCompleted {
                proposal_id, stage, dvp_proposal_cid, dvp_cid, allocation_cid, pending_traffic,
            } => {
                if let Some(state) = self.active_settlements.get_mut(&proposal_id) {
                    state.stage = stage;
                    if dvp_proposal_cid.is_some() {
                        state.dvp_proposal_cid = dvp_proposal_cid;
                    }
                    if dvp_cid.is_some() {
                        state.dvp_cid = dvp_cid;
                    }
                    if allocation_cid.is_some() {
                        state.allocation_cid = allocation_cid;
                    }
                    state.pending_traffic = pending_traffic;
                }
                self.failed_settlements.remove(&proposal_id);
            }
            AdvanceResult::Preconfirmed { proposal_id } => {
                if let Some(state) = self.active_settlements.get_mut(&proposal_id) {
                    state.stage = SettlementStage::ProposalReceived;
                }
                self.failed_settlements.remove(&proposal_id);
            }
            AdvanceResult::Rejected { proposal_id } => {
                self.release_commitment(&proposal_id);
                self.rejected_proposals.insert(proposal_id.clone());
                self.active_settlements.shift_remove(&proposal_id);
                self.failed_settlements.remove(&proposal_id);
                self.needs_readvance.remove(&proposal_id);
            }
            AdvanceResult::Terminal { proposal_id } => {
                // Record the inflow (token received from the counterparty) before
                // removing the entry, then release the CC reservation.
                self.record_settlement_inflow(&proposal_id);
                self.release_commitment(&proposal_id);
                self.completed_proposals.insert(proposal_id.clone());
                self.active_settlements.shift_remove(&proposal_id);
                self.failed_settlements.remove(&proposal_id);
                self.needs_readvance.remove(&proposal_id);
            }
            AdvanceResult::Wait { proposal_id } => {
                // Exponential cooldown for consecutive Waits: 30, 60, 120, 240,
                // 480, 600 (capped). Without this, Wait settlements are re-polled
                // every 2s, consuming semaphore permits and starving actionable
                // settlements. When counterparty acts, sync_on_chain_contracts
                // / stream-driven advance_proposal / step completion all
                // `.remove()` the entry — so `or_insert` creates a fresh one with
                // wait_count=0, resetting the backoff naturally.
                let expired = self.past_deadline(&proposal_id);
                let entry = self.failed_settlements.entry(proposal_id.clone())
                    .or_insert(FailedSettlement {
                        retry_count: 0,
                        wait_count: 0,
                        next_retry: Instant::now(),
                        first_transient_at: None,
                        cid_waiting: None,
                    });
                entry.retry_count = 0; // Not a failure — don't accumulate
                entry.wait_count = entry.wait_count.saturating_add(1);
                let mut delay = FailedSettlement::wait_delay(entry.wait_count);
                if expired {
                    // Past-deadline: keep advancing at a bounded cadence so the
                    // deadline watchdog can conclude and release the reservation.
                    delay = delay.min(Duration::from_secs(EXPIRED_RETRY_SECS));
                }
                entry.next_retry = Instant::now() + delay;
                entry.cid_waiting = None;
                debug!(
                    "[{}] Wait #{}: cooldown {}s",
                    proposal_id, entry.wait_count, delay.as_secs()
                );
            }
            AdvanceResult::Error { proposal_id, error } => {
                let is_permanent = error.contains("deadline-exceeded")
                    || error.contains("DA.Exception.PreconditionFailed")
                    || error.contains("PreconditionFailed")
                    || error.contains("PRECONDITION_FAILED");
                let is_inactive = error.contains("INACTIVE_CONTRACTS");
                let is_transient = !is_permanent && (is_inactive
                    || error.contains("No Dvp contract ID found")
                    || error.contains("No DvpProposal CID found"));
                let expired = self.past_deadline(&proposal_id);
                let entry = self.failed_settlements.entry(proposal_id.clone())
                    .or_insert(FailedSettlement {
                        retry_count: 0,
                        wait_count: 0,
                        next_retry: Instant::now(),
                        first_transient_at: None,
                        cid_waiting: None,
                    });
                if is_permanent {
                    entry.retry_count = FailedSettlement::max_retries();
                } else if !is_transient {
                    entry.retry_count += 1;
                }

                if entry.is_exhausted() {
                    error!(
                        "[{}] Settlement permanently failed after {} retries: {:#}",
                        proposal_id, entry.retry_count, error
                    );
                    // Tell the server we've abandoned it (pre-allocation only) so it
                    // reaches a terminal DB status and stops being re-surfaced by
                    // poll_pending_proposals. Best-effort; read stage before removal.
                    if self.is_pre_allocation(&proposal_id) {
                        self.notify_server_cancel(&proposal_id, "agent abandoned: deadline/error exhausted");
                    }
                    self.release_commitment(&proposal_id);
                    {
                        let mut t = self.tracker.lock().await;
                        t.mark_failed(&proposal_id);
                    }
                    // Record as terminal so polling won't re-discover and re-add it
                    // (and so a restart won't resume it). Mirrors the Rejected arm.
                    // Without this, an expired/permanently-failed proposal that the
                    // server still returns as pending loops forever.
                    self.rejected_proposals.insert(proposal_id.clone());
                    self.active_settlements.shift_remove(&proposal_id);
                    self.failed_settlements.remove(&proposal_id);
                    self.needs_readvance.remove(&proposal_id);
                    self.update_actionable_count();
                    return;
                }

                let mut delay = if is_transient {
                    Duration::from_secs(10)
                } else {
                    FailedSettlement::retry_delay(entry.retry_count)
                };
                if expired {
                    // Past-deadline: bounded cadence (see EXPIRED_RETRY_SECS) —
                    // fast enough for prompt release, slow enough that an RPC
                    // outage doesn't burn through retries in seconds.
                    delay = delay.min(Duration::from_secs(EXPIRED_RETRY_SECS));
                }
                entry.next_retry = Instant::now() + delay;
                if is_transient {
                    let is_cid_waiting = error.contains("No Dvp contract ID found")
                        || error.contains("No DvpProposal CID found");

                    if is_cid_waiting {
                        if entry.first_transient_at.is_none() {
                            entry.first_transient_at = Some(Instant::now());
                        }
                        entry.cid_waiting = Some(if error.contains("No DvpProposal CID found") {
                            CidWaitingType::DvpProposal
                        } else {
                            CidWaitingType::DvpContract
                        });
                        let waiting_secs = entry.first_transient_at
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        if waiting_secs > 600 {
                            warn!(
                                "[{}] Waiting {:?}: {} (stuck for {}s)",
                                proposal_id, delay, error, waiting_secs
                            );
                        } else {
                            info!(
                                "[{}] Waiting {:?}: {}",
                                proposal_id, delay, error
                            );
                        }
                    } else {
                        // INACTIVE_CONTRACTS and other transient: keep as info
                        entry.cid_waiting = None;
                        info!(
                            "[{}] Waiting {:?}: {}",
                            proposal_id, delay, error
                        );
                    }
                } else {
                    entry.cid_waiting = None;
                    warn!(
                        "[{}] Settlement error (retry {}/{} in {:?}): {:#}",
                        proposal_id, entry.retry_count, FailedSettlement::max_retries(), delay, error
                    );
                }
            }
            AdvanceResult::Timeout { proposal_id } => {
                let entry = self.failed_settlements.entry(proposal_id.clone())
                    .or_insert(FailedSettlement {
                        retry_count: 0,
                        wait_count: 0,
                        next_retry: Instant::now(),
                        first_transient_at: None,
                        cid_waiting: None,
                    });
                entry.retry_count += 1;

                if entry.is_exhausted() {
                    error!(
                        "[{}] Settlement permanently failed after {} timeouts",
                        proposal_id, entry.retry_count
                    );
                    // Tell the server we've abandoned it (pre-allocation only) so it
                    // reaches a terminal DB status and stops being re-surfaced. Best-
                    // effort; read stage before removal.
                    if self.is_pre_allocation(&proposal_id) {
                        self.notify_server_cancel(&proposal_id, "agent abandoned: settlement timed out");
                    }
                    // Release the CC reservation, mirroring the sibling terminal
                    // arms (Rejected/Terminal/Error-exhausted). Without this the
                    // orphan leaks until the next heartbeat reconcile.
                    self.release_commitment(&proposal_id);
                    {
                        let mut t = self.tracker.lock().await;
                        t.mark_failed(&proposal_id);
                    }
                    // Record as terminal so polling won't re-discover and re-add it
                    // (and so a restart won't resume it). Mirrors the Rejected arm.
                    self.rejected_proposals.insert(proposal_id.clone());
                    self.active_settlements.shift_remove(&proposal_id);
                    self.failed_settlements.remove(&proposal_id);
                    self.needs_readvance.remove(&proposal_id);
                    self.update_actionable_count();
                    return;
                }

                // Short backoff for timeouts (likely transient sequencer backpressure)
                entry.next_retry = Instant::now() + Duration::from_secs(10);
                warn!(
                    "[{}] Settlement timed out (retry {}/{} in 10s)",
                    proposal_id, entry.retry_count, FailedSettlement::max_retries()
                );
            }
            AdvanceResult::NeedsAllocate { .. } => {
                // Should never reach apply_result — handled in the spawned task loop
                warn!("Unexpected NeedsAllocate in apply_result");
            }
        }
        self.update_actionable_count();
    }

    /// Drain all in-progress tasks (for graceful shutdown)
    pub async fn drain_tasks(&mut self) -> usize {
        let handles: Vec<_> = self.task_handles.drain(..).collect();
        let count = handles.len();
        if count > 0 {
            info!("Waiting for {} in-progress settlement task(s)...", count);
            let _ = tokio::time::timeout(Duration::from_secs(300), join_all(handles)).await;
            self.collect_results().await;
        }
        count
    }

    /// Reset retry backoffs for all failed settlements.
    ///
    /// Called when connectivity is restored (stream reconnect or poll recovery)
    /// so that settlements stuck in long backoff retry immediately.
    pub fn reset_failed_backoffs(&mut self) {
        if self.failed_settlements.is_empty() {
            return;
        }
        let count = self.failed_settlements.len();
        for entry in self.failed_settlements.values_mut() {
            entry.next_retry = Instant::now();
        }
        info!(
            "Connectivity restored: reset backoff for {} failed settlement(s)",
            count
        );
    }

    /// Poll for pending settlement proposals and process any new ones
    ///
    /// This is a fallback for missed stream events — discovers proposals via
    /// GetSettlementProposals RPC and feeds them through the normal handler.
    ///
    /// Returns `true` if the RPC call succeeded, `false` on connection failure.
    pub async fn poll_pending_proposals(&mut self, client: &mut OrderbookClient) -> bool {
        let proposals = match client.get_pending_proposals().await {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to poll pending proposals: {}", e);
                return false;
            }
        };

        for proposal in proposals {
            if self.active_settlements.contains_key(&proposal.proposal_id) {
                continue;
            }
            if self.rejected_proposals.contains(&proposal.proposal_id) {
                continue;
            }
            if self.completed_proposals.contains(&proposal.proposal_id) {
                continue;
            }
            info!("Discovered pending proposal via polling: {}", proposal.proposal_id);
            let update = SettlementUpdate {
                event_type: EventType::ProposalCreated as i32,
                proposal: Some(proposal),
                timestamp: None,
            };
            if let Err(e) = self.handle_settlement_update(update).await {
                warn!("Error processing polled proposal: {}", e);
            }
        }
        true
    }

    /// Sync on-chain DvpProposal and Dvp contracts with local state.
    pub async fn sync_on_chain_contracts(&mut self) {
        if self.active_settlements.is_empty() {
            return;
        }

        let settlement_ids: Vec<String> = self.active_settlements.keys().cloned().collect();
        let contracts = match self.backend.sync_contracts(&settlement_ids).await {
            Ok(c) => c,
            Err(e) => {
                warn!("sync_on_chain_contracts: {} ({} active settlements)", e, settlement_ids.len());
                return;
            }
        };

        let mut found_proposals = 0u32;
        let mut found_dvps = 0u32;
        let mut found_allocations = 0u32;

        for contract in &contracts {
            let Some(state) = self.active_settlements.get_mut(&contract.settlement_id) else { continue };

            if contract.contract_type == "DvpProposal" && state.dvp_proposal_cid.is_none() {
                debug!("[{}] Discovered DvpProposal on-chain: {}", contract.settlement_id, contract.contract_id);
                state.dvp_proposal_cid = Some(contract.contract_id.clone());
                self.failed_settlements.remove(&contract.settlement_id);
                found_proposals += 1;
            } else if contract.contract_type == "Dvp" && state.dvp_cid.is_none() {
                debug!("[{}] Discovered Dvp on-chain: {}", contract.settlement_id, contract.contract_id);
                state.dvp_cid = Some(contract.contract_id.clone());
                self.failed_settlements.remove(&contract.settlement_id);
                found_dvps += 1;
            } else if contract.contract_type == "Allocation" && state.allocation_cid.is_none() {
                debug!("[{}] Discovered Allocation on-chain: {}", contract.settlement_id, contract.contract_id);
                state.allocation_cid = Some(contract.contract_id.clone());
                self.failed_settlements.remove(&contract.settlement_id);
                found_allocations += 1;
            }
        }

        // Identify settlements still waiting for CIDs (only those already flagged as cid_waiting)
        let missing_proposal_ids: Vec<&str> = self.failed_settlements.iter()
            .filter(|(_, f)| matches!(f.cid_waiting, Some(CidWaitingType::DvpProposal)))
            .map(|(id, _)| id.as_str())
            .collect();
        let missing_dvp_ids: Vec<&str> = self.failed_settlements.iter()
            .filter(|(_, f)| matches!(f.cid_waiting, Some(CidWaitingType::DvpContract)))
            .map(|(id, _)| id.as_str())
            .collect();

        if !missing_proposal_ids.is_empty() || !missing_dvp_ids.is_empty() {
            warn!(
                "sync_on_chain_contracts: gRPC returned {} contracts (new: {} DvpProposal, {} Dvp, {} Allocation). \
                 Still missing: {} DvpProposal {:?}, {} Dvp {:?}",
                contracts.len(), found_proposals, found_dvps, found_allocations,
                missing_proposal_ids.len(), missing_proposal_ids,
                missing_dvp_ids.len(), missing_dvp_ids,
            );
        } else if found_proposals > 0 || found_dvps > 0 || found_allocations > 0 {
            info!(
                "sync_on_chain_contracts: gRPC returned {} contracts (new: {} DvpProposal, {} Dvp, {} Allocation)",
                contracts.len(), found_proposals, found_dvps, found_allocations,
            );
        }
    }

    /// Verify a user order by fetching it from the server
    ///
    /// Returns the order_id on success, or an error message on failure.
    async fn verify_user_order(
        &mut self,
        proposal: &SettlementProposal,
        order_id: u64,
        market_id: &str,
    ) -> std::result::Result<u64, String> {
        let client = self.get_query_client().await
            .map_err(|e| format!("Failed to create query client: {}", e))?;

        let orders = client.get_active_orders(market_id).await
            .map_err(|e| format!("Failed to fetch orders: {}", e))?;

        let order = orders.into_iter()
            .find(|o| o.order_id == order_id)
            .ok_or_else(|| format!("Order {} not found on server", order_id))?;

        let mut tracker = self.tracker.lock().await;
        match tracker.verify_and_import_order(&order, proposal) {
            VerifyResult::Accepted { order_id } => Ok(order_id),
            VerifyResult::Rejected { reason } => Err(reason),
            VerifyResult::NeedServerLookup { .. } => Err("Unexpected NeedServerLookup".to_string()),
        }
    }

    /// Verify an RFQ proposal against agent's own in-memory state.
    ///
    /// Buyer path: match by proposal_id, then verify all trade parameters.
    /// LP path: match by (market_id, price, base_quantity, quote_quantity).
    /// Returns true if verified, false if rejected.
    async fn verify_rfq_proposal(&self, proposal: &SettlementProposal) -> bool {
        // Path 1: Buyer — match by proposal_id, verify all amounts
        if let Some(ref accepted) = self.accepted_rfq_trades {
            let mut map = accepted.lock().await;
            if let Some(trade) = map.remove(&proposal.proposal_id) {
                if trade.market_id != proposal.market_id {
                    warn!("[{}] RFQ verification failed: market_id mismatch (expected={}, got={})",
                        proposal.proposal_id, trade.market_id, proposal.market_id);
                    return false;
                }
                if trade.price != proposal.settlement_price {
                    warn!("[{}] RFQ verification failed: price mismatch (expected={}, got={})",
                        proposal.proposal_id, trade.price, proposal.settlement_price);
                    return false;
                }
                if trade.base_quantity != proposal.base_quantity {
                    warn!("[{}] RFQ verification failed: base_quantity mismatch (expected={}, got={})",
                        proposal.proposal_id, trade.base_quantity, proposal.base_quantity);
                    return false;
                }
                if trade.quote_quantity != proposal.quote_quantity {
                    warn!("[{}] RFQ verification failed: quote_quantity mismatch (expected={}, got={})",
                        proposal.proposal_id, trade.quote_quantity, proposal.quote_quantity);
                    return false;
                }
                info!("[{}] RFQ verified: buyer trade params match (market={}, price={}, qty={}, quote_qty={})",
                    proposal.proposal_id, trade.market_id, trade.price, trade.base_quantity, trade.quote_quantity);
                return true;
            }
        }

        // Path 2: LP — match by ALL trade parameters (exact string comparison)
        if let Some(ref trades) = self.quoted_rfq_trades {
            let mut trades = trades.lock().await;
            if let Some(idx) = trades.iter().position(|t| {
                t.market_id == proposal.market_id
                    && t.price == proposal.settlement_price
                    && t.base_quantity == proposal.base_quantity
                    && t.quote_quantity == proposal.quote_quantity
            }) {
                let matched = trades.swap_remove(idx);
                info!("[{}] RFQ verified: LP quoted matching trade (market={}, price={}, qty={}, quote_qty={})",
                    proposal.proposal_id, matched.market_id, matched.price,
                    matched.base_quantity, matched.quote_quantity);
                return true;
            }
        }

        false
    }

    // ========================================================================
    // Step handlers
    // ========================================================================

    /// Reject a proposal (send preconfirmation with accept=false) and remove it
    async fn reject_proposal(&mut self, proposal_id: &str) -> Result<()> {
        let jwt = self.create_jwt()?;
        let mut rpc_client = OrderbookRpcClient::connect(&self.config.orderbook_grpc_url, Some(jwt)).await?;
        rpc_client.submit_preconfirmation(
            proposal_id,
            proposal_id,
            &self.config.party_id,
            false,
        ).await?;
        self.rejected_proposals.insert(proposal_id.to_string());
        self.active_settlements.shift_remove(proposal_id);
        info!("[{}] Proposal rejected", proposal_id);
        Ok(())
    }

    // ========================================================================
    // Helper methods
    // ========================================================================

    fn create_jwt(&self) -> Result<String> {
        generate_jwt(
            &self.config.party_id,
            &self.config.role,
            &self.config.private_key_bytes,
            self.config.token_ttl_secs,
            Some(self.config.node_name.as_str()),
        )
    }

    /// Get list of active settlements
    pub fn active_settlements(&self) -> &IndexMap<String, SettlementState> {
        &self.active_settlements
    }

    /// Get completed proposals set (for state persistence)
    pub fn completed_proposals(&self) -> &HashSet<String> {
        &self.completed_proposals
    }

    /// Get rejected proposals set (for state persistence)
    pub fn rejected_proposals(&self) -> &HashSet<String> {
        &self.rejected_proposals
    }

    /// Inject previously saved completed proposals (for state restoration)
    pub fn inject_completed_proposals(&mut self, proposals: HashSet<String>) {
        self.completed_proposals = proposals;
    }

    /// Inject previously saved rejected proposals (for state restoration)
    pub fn inject_rejected_proposals(&mut self, proposals: HashSet<String>) {
        self.rejected_proposals = proposals;
    }
}

// ============================================================================
// Settlement event recording helper
// ============================================================================

/// Record a "Completed" settlement event via RPC.
///
/// This centralizes event recording so both direct and cloud backends
/// get events written to `settlement_proposal_history`. The direct backend
/// also records events internally, so duplicates are harmless.
///
/// Failures are logged but not propagated — the step itself succeeded,
/// and the event will be re-recorded on the next advance cycle if needed.
async fn record_step_completed(
    rpc_client: &mut OrderbookRpcClient,
    proposal_id: &str,
    party_id: &str,
    is_buyer: bool,
    event_type: SettlementEventType,
    update_id: &str,
    contract_id: &str,
) {
    let recorded_by_role = if is_buyer {
        RecordedByRole::Buyer as i32
    } else {
        RecordedByRole::Seller as i32
    };

    let request = RecordSettlementEventRequest {
        auth: None,
        proposal_id: proposal_id.to_string(),
        recorded_by: party_id.to_string(),
        recorded_by_role,
        event_type: event_type as i32,
        submission_id: None,
        update_id: Some(update_id.to_string()),
        contract_id: Some(contract_id.to_string()),
        template_id: None,
        result: SettlementEventResult::Success as i32,
        error_message: None,
        metadata: None,
    };

    match rpc_client.record_settlement_event(request).await {
        Ok(event_id) => {
            debug!(
                "[{}] Recorded settlement event {:?} (event_id={})",
                proposal_id, event_type, event_id
            );
        }
        Err(e) => {
            warn!(
                "[{}] Failed to record settlement event {:?}: {}",
                proposal_id, event_type, e
            );
        }
    }
}

/// Record a "Submitted" settlement event via RPC (fee queued for background payment).
///
/// Unlike record_step_completed, this records a Submitted event with Pending result
/// and no update_id/contract_id (payment hasn't happened yet).
async fn record_step_submitted(
    rpc_client: &mut OrderbookRpcClient,
    proposal_id: &str,
    party_id: &str,
    is_buyer: bool,
    event_type: SettlementEventType,
) {
    let recorded_by_role = if is_buyer {
        RecordedByRole::Buyer as i32
    } else {
        RecordedByRole::Seller as i32
    };

    let request = RecordSettlementEventRequest {
        auth: None,
        proposal_id: proposal_id.to_string(),
        recorded_by: party_id.to_string(),
        recorded_by_role,
        event_type: event_type as i32,
        submission_id: None,
        update_id: None,
        contract_id: None,
        template_id: None,
        result: SettlementEventResult::Pending as i32,
        error_message: None,
        metadata: None,
    };

    match rpc_client.record_settlement_event(request).await {
        Ok(event_id) => {
            debug!(
                "[{}] Recorded fee submitted event {:?} (event_id={})",
                proposal_id, event_type, event_id
            );
        }
        Err(e) => {
            warn!(
                "[{}] Failed to record fee submitted event {:?}: {}",
                proposal_id, event_type, e
            );
        }
    }
}

// ============================================================================
// Free function: advance a single settlement (runs in spawned task)
// ============================================================================

/// Compute this side's reservation inputs from locally-stored proposal terms:
/// `(allocation_token, allocation_amount, my_fees_usd)`.
///
/// Shared by the adoption-time advisory check (`can_commit`) and the
/// post-preconfirm reservation (`ensure_reserved` → `try_commit`) so the two
/// can never drift. The buyer allocates the quote leg, the seller the base leg.
fn reservation_inputs(
    proposal: &SettlementProposal,
    is_buyer: bool,
    cc_token_id: &Option<String>,
) -> (String, Decimal, Decimal) {
    let my_instrument = if is_buyer {
        &proposal.quote_instrument // buyer allocates quote
    } else {
        &proposal.base_instrument // seller allocates base
    };
    let allocation_amount = Decimal::from_str(
        if is_buyer { &proposal.quote_quantity } else { &proposal.base_quantity }
    ).unwrap_or(Decimal::ONE);

    let allocation_token = match cc_token_id {
        Some(cc_id) if my_instrument == cc_id => liquidity::CC_TOKEN.to_string(),
        _ => my_instrument.clone(),
    };

    let my_fees_usd = if is_buyer {
        Decimal::from_str(&proposal.dvp_processing_fee_buyer).unwrap_or_default()
            + Decimal::from_str(&proposal.allocation_processing_fee_buyer).unwrap_or_default()
    } else {
        Decimal::from_str(&proposal.dvp_processing_fee_seller).unwrap_or_default()
            + Decimal::from_str(&proposal.allocation_processing_fee_seller).unwrap_or_default()
    };

    (allocation_token, allocation_amount, my_fees_usd)
}

/// Reserve the resources for a settlement whose counterparty has committed
/// (server returned a post-preconfirm action). Idempotent; strictly on
/// locally-stored terms (`state.proposal` + the tracker's adoption record).
///
/// Two halves, each independently idempotent:
/// - Tracker: `try_reserve_pending` applies the order's `pending_quantity`
///   once (capacity re-checked — the atomic backstop for adoption-time
///   advisory checks that overlapped).
/// - LiquidityManager: `try_commit` — SKIPPED entirely when a commitment
///   already exists: `try_commit`'s availability check counts this proposal's
///   own commitment, so a bare re-commit under tight inventory would
///   spuriously fail a healthy, fully-reserved settlement. Restored-reserved
///   proposals (flag persisted, in-memory commitment lost with the process)
///   lazily re-commit here.
///
/// Depletion outflow is recorded only when the tracker reservation is NEW —
/// never per-step, never again after a restart restore.
async fn ensure_reserved(
    state: &SettlementState,
    config: &BaseConfig,
    liquidity_manager: &Option<Arc<LiquidityManager>>,
    tracker: &Arc<Mutex<OrderTracker>>,
    proposal_id: &str,
) -> Result<(), String> {
    // Precompute reservation inputs once (used for both the LM commit and the
    // depletion outflow) so the two can never diverge.
    let inputs = liquidity_manager
        .as_ref()
        .map(|_| reservation_inputs(&state.proposal, state.is_buyer, &config.cc_token_id));

    // LM commitment FIRST — before latching the tracker reservation. If the
    // commit fails (balance dipped since the adoption-time advisory check), we
    // return Err with NOTHING half-applied, so the Error-backoff retry re-runs
    // cleanly. Skipped when a commitment already exists (its own commitment
    // counts against availability, so a bare re-commit would spuriously fail;
    // and this is the restart lazy-recommit path). Idempotent.
    if let Some(lm) = liquidity_manager {
        if !lm.has_commitment(proposal_id).await {
            let (allocation_token, allocation_amount, my_fees_usd) = inputs.as_ref().unwrap();
            let fee_cc = lm.estimate_fee_cc(*my_fees_usd).await;
            lm.try_commit(proposal_id, allocation_token, *allocation_amount, fee_cc)
                .await?;
            info!(
                "[{}] Reserved {} {} + {:.4} CC fees (counterparty committed)",
                proposal_id, allocation_amount, allocation_token, fee_cc
            );
        }
    }

    // Tracker latch LAST. `try_reserve_pending` is the idempotent
    // once-per-settlement latch (Ok(true) exactly once ever; Ok(false) for a
    // restart-restored reserved entry). Because it runs only after the LM
    // commit succeeded, gating the depletion outflow on `newly_reserved` books
    // the outflow exactly once — never lost on a partial-failure retry (LM
    // failed first → tracker never latched → clean retry) and never
    // double-booked on a restart re-commit (restored entry → Ok(false)).
    let newly_reserved = {
        let mut t = tracker.lock().await;
        t.try_reserve_pending(proposal_id)?
    };

    if newly_reserved {
        if let (Some(lm), Some((allocation_token, allocation_amount, _))) =
            (liquidity_manager, &inputs)
        {
            lm.record_outflow(allocation_token, allocation_amount.to_f64().unwrap_or(0.0))
                .await;
        }
    }

    Ok(())
}

/// Deadline windows for orderbook-origin settlements. These proposals carry no
/// LP-quoted windows — the server's env defaults (ALLOCATE_DEADLINE_SECS /
/// SETTLE_DEADLINE_SECS, 6h/12h) are stamped into their on-chain DVP terms —
/// so the agent must NOT judge them by its own much tighter RFQ windows:
/// a human counterparty may legitimately take hours (frontend may be closed).
const ORDERBOOK_ALLOCATE_BEFORE_SECS: u64 = 21_600; // 6 hours
const ORDERBOOK_SETTLE_BEFORE_SECS: u64 = 43_200; // 12 hours

/// Resolve the (allocate, settle) expiry windows for a proposal by origin:
/// RFQ settlements use the agent's own quoted windows; orderbook (or unknown/
/// legacy origin) settlements use the server's 6h/12h defaults.
fn expiry_windows(origin: &str, rfq_allocate_secs: u64, rfq_settle_secs: u64) -> (u64, u64) {
    if origin == "rfq" {
        (rfq_allocate_secs, rfq_settle_secs)
    } else {
        (ORDERBOOK_ALLOCATE_BEFORE_SECS, ORDERBOOK_SETTLE_BEFORE_SECS)
    }
}

/// Deadline watchdog verdict for an in-flight settlement.
///
/// Returns the permanent `deadline-exceeded` error message when the proposal has
/// outlived its settlement window — or its allocation window while the server
/// still expects a pre-allocation action from us. Past `allocateBefore` the DVP
/// contract rejects further allocation steps on-chain, so such a settlement is
/// doomed even though the settle window is still open; abandoning it right away
/// releases the liquidity reservation instead of holding it until the (later)
/// settle deadline or a server-sent terminal event. `Wait` is excluded from the
/// allocation gate: it can mean "allocated, waiting for the operator", which
/// only the settle window covers. `None` never expires — an already-terminal
/// proposal is handled by the terminal arm of the state machine.
fn deadline_expiry_error(
    created_at_secs: i64,
    now_secs: i64,
    my_action: NextAction,
    allocate_before_secs: u64,
    settle_before_secs: u64,
) -> Option<String> {
    if my_action == NextAction::None {
        return None;
    }
    let age = now_secs - created_at_secs;
    if age > settle_before_secs as i64 {
        return Some(format!(
            "Settlement expired: deadline-exceeded (created {}s ago, max {}s)",
            age, settle_before_secs
        ));
    }
    let pre_allocation = matches!(
        my_action,
        NextAction::Preconfirm
            | NextAction::PayDvpFee
            | NextAction::CreateDvp
            | NextAction::AcceptDvp
            | NextAction::PayAllocFee
            | NextAction::Allocate
            | NextAction::MulticallAccept
    );
    if pre_allocation && age > allocate_before_secs as i64 {
        return Some(format!(
            "Settlement expired: deadline-exceeded (allocation window: created {}s ago, max {}s, pending action {:?})",
            age, allocate_before_secs, my_action
        ));
    }
    None
}

/// Advance a single settlement by checking NextAction from the server.
///
/// This is the core settlement state machine, extracted as a free function
/// so it can run in a spawned tokio task. It receives all dependencies as
/// parameters and returns an AdvanceResult for the main thread to apply.
///
/// Terminal state tracker updates (mark_settled/mark_failed) happen here
/// since we have Arc<Mutex<OrderTracker>>.
async fn advance_single<B: SettlementBackend>(
    proposal_id: String,
    state: SettlementState,
    config: BaseConfig,
    backend: Arc<B>,
    tracker: Arc<Mutex<OrderTracker>>,
    liquidity_manager: Option<Arc<LiquidityManager>>,
    shutting_down: bool,
    action_log: Arc<Mutex<Vec<(String, &'static str)>>>,
) -> AdvanceResult {
    debug!(
        "[{}] Advancing (role={}, stage={})",
        proposal_id,
        if state.is_buyer { "buyer" } else { "seller" },
        state.stage,
    );

    // Create RPC client (uses cached global channel — fast)
    let jwt = match generate_jwt(
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
    ) {
        Ok(j) => j,
        Err(e) => return AdvanceResult::Error {
            proposal_id,
            error: format!("JWT generation failed: {:#}", e),
        },
    };

    let mut rpc_client = match OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await {
        Ok(c) => c,
        Err(e) => return AdvanceResult::Error {
            proposal_id,
            error: format!("RPC connect failed: {:#}", e),
        },
    };

    // Get settlement status from server
    let status = match rpc_client.get_settlement_status(&proposal_id).await {
        Ok(s) => s,
        Err(e) => return AdvanceResult::Error {
            proposal_id,
            error: format!("GetSettlementStatus failed: {:#}", e),
        },
    };

    let my_action = if state.is_buyer {
        NextAction::try_from(status.buyer_next_action).unwrap_or(NextAction::None)
    } else {
        NextAction::try_from(status.seller_next_action).unwrap_or(NextAction::None)
    };

    // Abandon any still-in-flight proposal that has blown past its settlement
    // deadline (or its allocation deadline while allocation is still pending),
    // regardless of which action is pending. Without this, a proposal
    // stuck in `Wait` (e.g. the counterparty abandoned the flow and the server
    // emits no terminal event) is re-polled forever, never exhausts (the `Wait`
    // handler keeps resetting `retry_count`), and its CC reservation leaks for
    // the process lifetime — invisible to the heartbeat reconcile because the
    // proposal stays in `active_settlements`. Routing it through the
    // `deadline-exceeded` permanent-error path releases the reservation and
    // removes the entry. `None` is excluded so an already-terminal proposal is
    // handled by the terminal arm below instead of being marked failed.
    if let Some(created_at) = &state.proposal.created_at {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (allocate_window, settle_window) = expiry_windows(
            &state.proposal.origin,
            config.allocate_before_secs,
            config.settle_before_secs,
        );
        if let Some(error) = deadline_expiry_error(
            created_at.seconds,
            now,
            my_action,
            allocate_window,
            settle_window,
        ) {
            return AdvanceResult::Error { proposal_id, error };
        }
    }

    // Reserve on the first post-preconfirm action: any action other than
    // Preconfirm/Wait/None means the counterparty has committed its side
    // (the server only emits progress actions after both preconfirmations,
    // or — for the DVP acceptor — after the counterparty's on-chain DVP).
    // Until then nothing is reserved, so one-sided proposals cost no
    // inventory. Idempotent; a failure (balance dropped since the adoption
    // advisory check) routes through the Error backoff/retry path.
    if !matches!(
        my_action,
        NextAction::Preconfirm | NextAction::Wait | NextAction::None
    ) {
        if let Err(e) =
            ensure_reserved(&state, &config, &liquidity_manager, &tracker, &proposal_id).await
        {
            return AdvanceResult::Error {
                proposal_id,
                error: format!("reservation failed: {}", e),
            };
        }
    }

    match my_action {
        NextAction::Preconfirm => {
            if shutting_down {
                info!("[{}] Rejecting proposal (shutting down)", proposal_id);
                {
                    let mut t = tracker.lock().await;
                    t.mark_failed(&proposal_id);
                }
                // Submit rejection
                if let Err(e) = rpc_client.submit_preconfirmation(
                    &proposal_id, &proposal_id, &config.party_id, false,
                ).await {
                    warn!("[{}] Failed to reject during shutdown: {}", proposal_id, e);
                }
                AdvanceResult::Rejected { proposal_id }
            } else {
                action_log.lock().await.push((proposal_id.clone(), "Preconfirm"));
                match rpc_client.submit_preconfirmation(
                    &proposal_id, &proposal_id, &config.party_id, true,
                ).await {
                    Ok(()) => {
                        info!("[{}] Preconfirmation submitted", proposal_id);
                        AdvanceResult::Preconfirmed { proposal_id }
                    }
                    Err(e) => AdvanceResult::Error {
                        proposal_id,
                        error: format!("Preconfirmation failed: {:#}", e),
                    },
                }
            }
        }
        NextAction::PayDvpFee => {
            action_log.lock().await.push((proposal_id.clone(), "PayDvpFee"));
            let submitted_event = if state.is_buyer {
                SettlementEventType::DvpProcessingFeeBuyerSubmitted
            } else {
                SettlementEventType::DvpProcessingFeeSellerSubmitted
            };
            // Record _Submitted immediately — server allows next step for agents (LPs)
            record_step_submitted(
                &mut rpc_client, &proposal_id, &config.party_id,
                state.is_buyer, submitted_event,
            ).await;
            // Trigger the off-chain processing-fee debit via PreparePayFee /
            // ExecutePayFee. The ledger looks up our role from the proposal
            // and debits our share of the DVP processing fee. The
            // (source='pay_fee', external_id='dvp:<role>:<proposal_id>')
            // UNIQUE constraint dedupes per-role retries while letting
            // buyer and seller each pay their own DVP fee on the same
            // proposal.
            if let Err(e) = backend.pay_fee(&proposal_id, "dvp").await {
                warn!("[{}] DVP processing fee debit failed: {:#}", proposal_id, e);
            }
            AdvanceResult::StepCompleted {
                proposal_id,
                stage: SettlementStage::DvpFeePaid,
                dvp_proposal_cid: None,
                dvp_cid: None,
                allocation_cid: None,
                pending_traffic: 0,
            }
        }
        NextAction::CreateDvp => {
            action_log.lock().await.push((proposal_id.clone(), "CreateDvp"));
            match backend.propose_dvp(&proposal_id).await {
                Ok(result) => {
                    record_step_completed(
                        &mut rpc_client, &proposal_id, &config.party_id,
                        state.is_buyer, SettlementEventType::DvpRequestCompleted,
                        &result.update_id, &result.contract_id,
                    ).await;

                    AdvanceResult::StepCompleted {
                        proposal_id,
                        stage: SettlementStage::DvpProposed,
                        dvp_proposal_cid: Some(result.contract_id),
                        dvp_cid: None,
                        allocation_cid: None,
                        pending_traffic: 0,
                    }
                }
                Err(e) => AdvanceResult::Error {
                    proposal_id,
                    error: format!("CreateDvp failed: {:#}", e),
                },
            }
        }
        NextAction::AcceptDvp => {
            // (Deadline expiry is checked up-front for all in-flight actions.)
            action_log.lock().await.push((proposal_id.clone(), "AcceptDvp"));
            let dvp_proposal_cid = match state.dvp_proposal_cid {
                Some(ref cid) => cid.clone(),
                None => return AdvanceResult::Error {
                    proposal_id,
                    error: "No DvpProposal CID found (not yet proposed?)".into(),
                },
            };
            debug!("[{}] Using DvpProposal from on-chain sync: {}", proposal_id, dvp_proposal_cid);
            match backend.accept_dvp(
                &proposal_id,
                &dvp_proposal_cid,
                &state.proposal.base_quantity,
                &state.proposal.quote_quantity,
                &state.proposal.base_instrument,
                &state.proposal.quote_instrument,
            ).await {
                Ok(result) => {
                    record_step_completed(
                        &mut rpc_client, &proposal_id, &config.party_id,
                        state.is_buyer, SettlementEventType::DvpAcceptCompleted,
                        &result.update_id, &result.contract_id,
                    ).await;

                    AdvanceResult::StepCompleted {
                        proposal_id,
                        stage: SettlementStage::DvpAccepted,
                        dvp_proposal_cid: None,
                        dvp_cid: Some(result.contract_id),
                        allocation_cid: None,
                        pending_traffic: 0,
                    }
                }
                Err(e) => AdvanceResult::Error {
                    proposal_id,
                    error: format!("AcceptDvp failed: {:#}", e),
                },
            }
        }
        NextAction::PayAllocFee => {
            action_log.lock().await.push((proposal_id.clone(), "PayAllocFee"));
            let submitted_event = if state.is_buyer {
                SettlementEventType::AllocationProcessingFeeBuyerSubmitted
            } else {
                SettlementEventType::AllocationProcessingFeeSellerSubmitted
            };
            // Record _Submitted immediately — server allows next step for agents (LPs)
            record_step_submitted(
                &mut rpc_client, &proposal_id, &config.party_id,
                state.is_buyer, submitted_event,
            ).await;
            // Trigger the off-chain processing-fee debit via PreparePayFee /
            // ExecutePayFee. The (source='pay_fee',
            // external_id='allocate:<role>:<proposal_id>') UNIQUE
            // constraint dedupes per-role retries while letting buyer and
            // seller each pay their own allocation fee on the same proposal.
            if let Err(e) = backend.pay_fee(&proposal_id, "allocate").await {
                warn!("[{}] Allocation processing fee debit failed: {:#}", proposal_id, e);
            }
            AdvanceResult::StepCompleted {
                proposal_id,
                stage: SettlementStage::AllocationFeePaid,
                dvp_proposal_cid: None,
                dvp_cid: None,
                allocation_cid: None,
                pending_traffic: 0,
            }
        }
        NextAction::Allocate => {
            // LP allocates last — wait for operator to witness counterparty's
            // DVP fee, allocation fee, and allocation on-chain. Checking only
            // the allocation is not enough: a cheating user can record
            // *_completed events for their fees without actually transferring
            // them to the orderbook-fee party, leaving the orderbook short.
            let (cp_dvp_fee, cp_alloc_fee, cp_alloc) = if state.is_buyer {
                (
                    status.dvp_processing_fee_seller.as_ref(),
                    status.allocation_processing_fee_seller.as_ref(),
                    status.allocation_seller.as_ref(),
                )
            } else {
                (
                    status.dvp_processing_fee_buyer.as_ref(),
                    status.allocation_processing_fee_buyer.as_ref(),
                    status.allocation_buyer.as_ref(),
                )
            };
            const CONFIRMED: i32 = 4; // DVP_STEP_STATUS_CONFIRMED
            let cp_dvp_fee_status = cp_dvp_fee.map(|s| s.status).unwrap_or(0);
            let cp_alloc_fee_status = cp_alloc_fee.map(|s| s.status).unwrap_or(0);
            let cp_alloc_status = cp_alloc.map(|s| s.status).unwrap_or(0);
            if cp_dvp_fee_status != CONFIRMED
                || cp_alloc_fee_status != CONFIRMED
                || cp_alloc_status != CONFIRMED
            {
                info!(
                    "[{}] Allocate: waiting for counterparty witnesses — dvp_fee={} alloc_fee={} alloc={} (need 4 each)",
                    proposal_id, cp_dvp_fee_status, cp_alloc_fee_status, cp_alloc_status
                );
                return AdvanceResult::Wait { proposal_id };
            }

            action_log.lock().await.push((proposal_id.clone(), "Allocate"));
            let dvp_cid = match state.dvp_cid {
                Some(ref cid) => cid.clone(),
                None => return AdvanceResult::Error {
                    proposal_id,
                    error: "No Dvp contract ID found (not yet accepted?)".into(),
                },
            };

            // Determine if this is a CC allocation (needs amulet pre-selection)
            let my_instrument = if state.is_buyer {
                &state.proposal.quote_instrument  // buyer allocates quote (payment leg)
            } else {
                &state.proposal.base_instrument   // seller allocates base (delivery leg)
            };
            let allocation_cc = match &config.cc_token_id {
                Some(cc_id) if my_instrument == cc_id => {
                    let amount_str = if state.is_buyer {
                        &state.proposal.quote_quantity
                    } else {
                        &state.proposal.base_quantity
                    };
                    Some(Decimal::from_str(amount_str).unwrap_or(Decimal::ONE))
                }
                _ => None,
            };

            // Return NeedsAllocate so the caller can release the settlement
            // permit before blocking on the payment queue.
            AdvanceResult::NeedsAllocate {
                proposal_id,
                dvp_cid,
                allocation_cc,
                is_buyer: state.is_buyer,
            }
        }
        NextAction::Wait | NextAction::MulticallAccept => {
            debug!("[{}] NextAction: Wait (counterparty's turn or multicall)", proposal_id);
            AdvanceResult::Wait { proposal_id }
        }
        NextAction::None => {
            let is_settled = status.stage == orderbook_proto::SettlementStage::Settled as i32;
            if is_settled {
                info!("[{}] Settlement completed successfully", proposal_id);
                let mut t = tracker.lock().await;
                t.mark_settled(&proposal_id);
            } else {
                info!("[{}] Settlement terminal (stage={})", proposal_id, status.stage);
                let mut t = tracker.lock().await;
                t.mark_failed(&proposal_id);
            }
            AdvanceResult::Terminal { proposal_id }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Backend stub — none of its methods are exercised by the stream-update
    /// terminal-path tests below (they only touch tracker + liquidity state).
    struct MockBackend;

    #[async_trait]
    impl SettlementBackend for MockBackend {
        async fn pay_fee(&self, _: &str, _: &str) -> Result<StepResult> {
            Err(anyhow::anyhow!("mock backend: pay_fee not used in test"))
        }
        async fn propose_dvp(&self, _: &str) -> Result<StepResult> {
            Err(anyhow::anyhow!("mock backend: propose_dvp not used in test"))
        }
        async fn accept_dvp(
            &self, _: &str, _: &str, _: &str, _: &str, _: &str, _: &str,
        ) -> Result<StepResult> {
            Err(anyhow::anyhow!("mock backend: accept_dvp not used in test"))
        }
        async fn allocate(&self, _: &str, _: &str, _: Option<Decimal>) -> Result<StepResult> {
            Err(anyhow::anyhow!("mock backend: allocate not used in test"))
        }
        async fn sync_contracts(&self, _: &[String]) -> Result<Vec<DiscoveredContract>> {
            Ok(Vec::new())
        }
        fn queue_depth(&self) -> (u64, u64) {
            (0, 0)
        }
    }

    fn test_proposal(id: &str) -> SettlementProposal {
        SettlementProposal {
            proposal_id: id.to_string(),
            base_instrument: "USDCx".to_string(),
            base_quantity: "1000".to_string(),
            quote_instrument: "CCY".to_string(),
            quote_quantity: "500".to_string(),
            ..Default::default()
        }
    }

    /// Executor with one committed proposal "p1": seller (is_buyer=false)
    /// allocating 1000 USDCx + ~11 CC fees. CC available drops 95 -> 84.
    async fn committed_executor() -> (SettlementExecutor<MockBackend>, Arc<LiquidityManager>) {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert!(fee_cc > Decimal::ZERO);
        lm.try_commit("p1", "USDCx", Decimal::from(1000), fee_cc).await.unwrap();
        // Reservation in effect: allocation + fee both held.
        assert_eq!(lm.available("USDCx").await, Decimal::from(4000));
        assert!(lm.available_cc().await < Decimal::from(95));

        let config = BaseConfig::test_minimal();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend);
        exec.set_liquidity_manager(lm.clone());
        exec.active_settlements
            .insert("p1".to_string(), SettlementState::new(test_proposal("p1"), false));
        (exec, lm)
    }

    /// Spawned release/inflow tasks need a chance to run before asserting.
    async fn drain_spawned() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Regression for Fix 1: the dominant terminal path (stream EventType::Settled)
    // must release the CC reservation. Before the fix this arm removed the entry
    // from active_settlements but leaked the reservation, draining available_cc
    // to 0 over ~2 days.
    #[tokio::test]
    async fn test_stream_settled_releases_commitment() {
        let (mut exec, lm) = committed_executor().await;
        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Settled as i32,
            proposal: Some(test_proposal("p1")),
            ..Default::default()
        })
        .await
        .unwrap();
        drain_spawned().await;

        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
        assert_eq!(lm.available_cc().await, Decimal::from(95));
        assert!(!exec.active_settlements.contains_key("p1"));
        assert!(exec.completed_proposals.contains("p1"));
    }

    // The cancel-on-abandon safety gate: the agent may tell the server to cancel
    // a proposal it abandons only while it is pre-allocation (stage < Allocated).
    // cETH locks on-chain at the reserver's own Allocate, so cancelling at/after
    // Allocated could tear down a still-settling DVP.
    #[tokio::test]
    async fn test_is_pre_allocation_gate_by_stage() {
        let (mut exec, _lm) = committed_executor().await;
        for (stage, expected) in [
            (SettlementStage::ProposalReceived, true),
            (SettlementStage::DvpFeePaid, true),
            (SettlementStage::DvpProposed, true),
            (SettlementStage::DvpAccepted, true),
            (SettlementStage::AllocationFeePaid, true),
            (SettlementStage::Allocated, false),
            (SettlementStage::AwaitingSettlement, false),
            (SettlementStage::Settled, false),
        ] {
            exec.active_settlements.get_mut("p1").unwrap().stage = stage;
            assert_eq!(exec.is_pre_allocation("p1"), expected, "stage {stage:?}");
        }
        // Unknown proposal is never cancellable.
        assert!(!exec.is_pre_allocation("nope"));
    }

    #[tokio::test]
    async fn test_stream_cancelled_releases_commitment() {
        let (mut exec, lm) = committed_executor().await;
        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Cancelled as i32,
            proposal: Some(test_proposal("p1")),
            ..Default::default()
        })
        .await
        .unwrap();
        drain_spawned().await;

        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
        assert_eq!(lm.available_cc().await, Decimal::from(95));
        assert!(!exec.active_settlements.contains_key("p1"));
        assert!(exec.rejected_proposals.contains("p1"));
    }

    // Regression: a permanently-failed settlement (deadline-exceeded) must be
    // recorded in rejected_proposals so polling does not re-discover and re-add
    // it. Before the fix this arm removed the entry from active_settlements but
    // skipped the dedup insert, so an expired proposal the server still returned
    // as pending looped forever (re-failing every poll, across restarts).
    #[tokio::test]
    async fn test_deadline_exceeded_marks_rejected_and_removes_active() {
        let (mut exec, _lm) = committed_executor().await;
        assert!(exec.active_settlements.contains_key("p1"));

        exec.apply_result(AdvanceResult::Error {
            proposal_id: "p1".to_string(),
            error: "Settlement expired: deadline-exceeded (created 13078s ago, max 7200s)".to_string(),
        })
        .await;
        drain_spawned().await;

        // Terminal: gone from the active set AND recorded so polling skips it.
        assert!(!exec.active_settlements.contains_key("p1"));
        assert!(exec.rejected_proposals.contains("p1"));
        assert!(!exec.failed_settlements.contains_key("p1"));
    }

    // Same guarantee for the Timeout-exhausted terminal arm.
    #[tokio::test]
    async fn test_timeout_exhausted_marks_rejected_and_removes_active() {
        let (mut exec, _lm) = committed_executor().await;
        // Drive Timeout until retries are exhausted (each Timeout increments by 1).
        for _ in 0..FailedSettlement::max_retries() {
            exec.apply_result(AdvanceResult::Timeout {
                proposal_id: "p1".to_string(),
            })
            .await;
        }
        drain_spawned().await;

        assert!(!exec.active_settlements.contains_key("p1"));
        assert!(exec.rejected_proposals.contains("p1"));
        assert!(!exec.failed_settlements.contains_key("p1"));
    }

    // --- deadline_expiry_error: the watchdog verdict (pure) ---
    // Windows: allocate 900s, settle 1800s.

    #[test]
    fn test_expiry_settle_window_fires_for_any_pending_action() {
        for action in [NextAction::Wait, NextAction::Allocate, NextAction::Preconfirm] {
            let err = deadline_expiry_error(0, 1861, action, 900, 1800)
                .expect("settle window expired");
            assert!(err.contains("deadline-exceeded"), "got: {err}");
        }
        // Inside the settle window, Wait does not expire
        assert!(deadline_expiry_error(0, 1799, NextAction::Wait, 900, 1800).is_none());
    }

    #[test]
    fn test_expiry_allocation_window_fires_only_for_pre_allocation_actions() {
        // Past allocateBefore, still inside settleBefore
        let err = deadline_expiry_error(0, 901, NextAction::Allocate, 900, 1800)
            .expect("allocation window expired for pending Allocate");
        assert!(err.contains("allocation window"), "got: {err}");
        for action in [
            NextAction::Preconfirm,
            NextAction::PayDvpFee,
            NextAction::CreateDvp,
            NextAction::AcceptDvp,
            NextAction::PayAllocFee,
            NextAction::MulticallAccept,
        ] {
            assert!(
                deadline_expiry_error(0, 901, action, 900, 1800).is_some(),
                "expected allocation-window expiry for {action:?}"
            );
        }
        // Wait may mean "allocated, waiting for the operator" — only the settle
        // window applies to it.
        assert!(deadline_expiry_error(0, 901, NextAction::Wait, 900, 1800).is_none());
        // Inside the allocation window nothing expires
        assert!(deadline_expiry_error(0, 899, NextAction::Allocate, 900, 1800).is_none());
    }

    #[test]
    fn test_expiry_none_action_never_expires() {
        assert!(deadline_expiry_error(0, 1_000_000, NextAction::None, 900, 1800).is_none());
    }

    // past_deadline (backoff bypass): settle window applies to any stage; the
    // allocation window only to pre-allocation stages; windows are origin-aware.
    #[tokio::test]
    async fn test_past_deadline_bypasses_backoff_by_stage() {
        let (mut exec, _lm) = committed_executor().await;
        exec.config.allocate_before_secs = 900;
        exec.config.settle_before_secs = 1800;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Fresh RFQ proposal: not past any deadline
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.proposal.origin = "rfq".to_string();
            state.proposal.created_at = Some(prost_types::Timestamp { seconds: now, nanos: 0 });
            state.stage = SettlementStage::ProposalReceived;
        }
        assert!(!exec.past_deadline("p1"));

        // Un-allocated past the allocation window
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.proposal.created_at = Some(prost_types::Timestamp { seconds: now - 1000, nanos: 0 });
        }
        assert!(exec.past_deadline("p1"));

        // Allocated: allocation window no longer applies, settle window not hit
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.stage = SettlementStage::Allocated;
        }
        assert!(!exec.past_deadline("p1"));

        // Allocated but past the settle window
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.proposal.created_at = Some(prost_types::Timestamp { seconds: now - 2000, nanos: 0 });
        }
        assert!(exec.past_deadline("p1"));

        // Orderbook-origin proposal (empty/legacy origin too): agent RFQ windows
        // do NOT apply — the server's 6h/12h windows govern.
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.proposal.origin = "orderbook".to_string();
            state.stage = SettlementStage::ProposalReceived;
        }
        assert!(!exec.past_deadline("p1")); // 2000s: inside 6h allocate window
        {
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.proposal.created_at = Some(prost_types::Timestamp { seconds: now - 21_700, nanos: 0 });
        }
        assert!(exec.past_deadline("p1")); // past 6h allocate window, unallocated

        // Unknown proposal: never bypass
        assert!(!exec.past_deadline("unknown"));
    }

    #[test]
    fn test_expiry_windows_by_origin() {
        assert_eq!(expiry_windows("rfq", 900, 1800), (900, 1800));
        assert_eq!(
            expiry_windows("orderbook", 900, 1800),
            (ORDERBOOK_ALLOCATE_BEFORE_SECS, ORDERBOOK_SETTLE_BEFORE_SECS)
        );
        // Legacy servers send no origin — treat as orderbook (lenient)
        assert_eq!(
            expiry_windows("", 900, 1800),
            (ORDERBOOK_ALLOCATE_BEFORE_SECS, ORDERBOOK_SETTLE_BEFORE_SECS)
        );
    }

    /// Fresh LM: 100 CC + 5000 USDCx, rate 0.10, ready.
    async fn ready_lm() -> Arc<LiquidityManager> {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        lm
    }

    fn created_update(proposal: SettlementProposal) -> SettlementUpdate {
        SettlementUpdate {
            event_type: EventType::ProposalCreated as i32,
            proposal: Some(proposal),
            ..Default::default()
        }
    }

    // Per-counterparty cap: a counterparty at the cap gets refused; a different
    // counterparty (or the same one under the cap) still adopts.
    #[tokio::test]
    async fn test_per_counterparty_cap() {
        let mut config = BaseConfig::test_minimal();
        config.max_pending_per_counterparty = 2;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend);

        // Two active settlements with counterparty "cp-x" (we are the seller,
        // so the counterparty is the buyer).
        for pid in ["p1", "p2"] {
            let mut p = test_proposal(pid);
            p.buyer = "cp-x".to_string();
            p.seller = "test-party".to_string();
            exec.active_settlements
                .insert(pid.to_string(), SettlementState::new(p, false));
        }

        // Third proposal from cp-x: hits the cap → refused. (reject_proposal's
        // RPC fails in tests — empty URL — so the entry is removed instead of
        // landing in rejected_proposals; either way it is NOT adopted.)
        let mut p3 = test_proposal("p3");
        p3.buyer = "cp-x".to_string();
        p3.seller = "test-party".to_string();
        exec.handle_settlement_update(created_update(p3)).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p3"));
        assert!(!exec.tracker.lock().await.has_settlement_order("p3"));

        // Proposal from cp-y passes the cap and adopts via the quoted-RFQ path.
        let quoted = Arc::new(Mutex::new(vec![QuotedTrade {
            market_id: String::new(),
            price: String::new(),
            base_quantity: "1000".to_string(),
            quote_quantity: "500".to_string(),
        }]));
        exec.set_quoted_rfq_trades(quoted);
        let mut p4 = test_proposal("p4");
        p4.buyer = "cp-y".to_string();
        p4.seller = "test-party".to_string();
        exec.handle_settlement_update(created_update(p4)).await.unwrap();
        assert!(exec.active_settlements.contains_key("p4"));
        assert!(exec.tracker.lock().await.has_settlement_order("p4"));
    }

    // Deferred reservation: adoption records the local decision but commits
    // nothing; ensure_reserved (counterparty committed) reserves exactly once
    // and is safe to re-enter even under tight inventory.
    #[tokio::test]
    async fn test_adoption_defers_reservation_until_counterparty_commits() {
        let config = BaseConfig::test_minimal();
        let lm = ready_lm().await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend);
        exec.set_liquidity_manager(lm.clone());
        let quoted = Arc::new(Mutex::new(vec![QuotedTrade {
            market_id: String::new(),
            price: String::new(),
            base_quantity: "1000".to_string(),
            quote_quantity: "500".to_string(),
        }]));
        exec.set_quoted_rfq_trades(quoted);

        let mut p1 = test_proposal("p1");
        p1.seller = "test-party".to_string();
        p1.buyer = "cp-x".to_string();
        exec.handle_settlement_update(created_update(p1)).await.unwrap();

        // Adopted — but NOTHING reserved: no LM commitment, full availability.
        assert!(exec.active_settlements.contains_key("p1"));
        assert!(exec.tracker.lock().await.has_settlement_order("p1"));
        assert!(!lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));

        // Counterparty commits (server would now return a progress action):
        // ensure_reserved commits both halves.
        let state = exec.active_settlements.get("p1").unwrap().clone();
        let lm_opt = Some(lm.clone());
        ensure_reserved(&state, &exec.config, &lm_opt, &tracker, "p1")
            .await
            .unwrap();
        assert!(lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(4000));

        // Idempotent re-entry — nothing double-reserved.
        ensure_reserved(&state, &exec.config, &lm_opt, &tracker, "p1")
            .await
            .unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::from(4000));

        // Terminal releases both halves.
        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Settled as i32,
            proposal: Some(test_proposal("p1")),
            ..Default::default()
        })
        .await
        .unwrap();
        drain_spawned().await;
        assert!(!lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
    }

    // Tight-inventory re-entry: once this proposal's own commitment consumes
    // the remaining balance, a re-entered ensure_reserved must NOT fail (the
    // has_commitment gate skips try_commit, whose availability check counts
    // the proposal's own commitment).
    #[tokio::test]
    async fn test_ensure_reserved_reentry_under_tight_inventory() {
        let config = BaseConfig::test_minimal();
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(10)).await;
        lm.update_token_balance("USDCx", Decimal::from(1000)).await; // exactly the leg
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        tracker
            .lock()
            .await
            .record_settlement_order("p1", 0, Decimal::from(1000));

        let state = SettlementState::new(test_proposal("p1"), false);
        let lm_opt = Some(lm.clone());
        ensure_reserved(&state, &config, &lm_opt, &tracker, "p1").await.unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::ZERO);

        // Re-entry with zero remaining availability must still succeed.
        ensure_reserved(&state, &config, &lm_opt, &tracker, "p1").await.unwrap();

        // Unadopted proposal → invariant error, nothing reserved.
        let state2 = SettlementState::new(test_proposal("p2"), false);
        assert!(ensure_reserved(&state2, &config, &lm_opt, &tracker, "p2").await.is_err());
        assert!(!lm.has_commitment("p2").await);
    }

    // Stream-terminal race: a task result arriving for a proposal that a
    // Settled/Cancelled stream event already removed must release both the
    // tracker reservation and the LM commitment (the collect_results orphan
    // reconciler).
    #[tokio::test]
    async fn test_collect_results_releases_orphaned_reservation() {
        let config = BaseConfig::test_minimal();
        let lm = ready_lm().await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend);
        exec.set_liquidity_manager(lm.clone());

        // Task reserved both halves, then the stream terminal removed the
        // settlement (not in active_settlements) before the result landed.
        {
            let mut t = tracker.lock().await;
            t.record_settlement_order("p1", 0, Decimal::from(1000));
            t.try_reserve_pending("p1").unwrap();
        }
        lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::ZERO)
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();
        assert!(tx
            .send((
                AdvanceResult::Wait { proposal_id: "p1".to_string() },
                SettlementState::new(test_proposal("p1"), false),
            ))
            .is_ok());
        exec.in_progress.insert("p1".to_string(), Instant::now());
        exec.pending_results.push(("p1".to_string(), rx));

        exec.collect_results().await;
        drain_spawned().await;

        assert!(!exec.tracker.lock().await.has_settlement_order("p1"));
        assert!(!lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
    }

    // Review finding 1: a partial-failure re-entry must still reserve exactly
    // once. With the LM commit ordered BEFORE the tracker latch, a first
    // attempt that fails the LM commit latches NOTHING (no tracker reservation,
    // no commitment), so the retry runs cleanly — and the once-only outflow,
    // gated on the same tracker latch, is neither lost nor double-booked.
    #[tokio::test]
    async fn test_ensure_reserved_partial_failure_then_retry_reserves_once() {
        let config = BaseConfig::test_minimal();
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(50)).await;
        lm.update_token_balance("USDCx", Decimal::from(500)).await; // < the 1000 leg
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [7u8; 32])));
        tracker
            .lock()
            .await
            .record_settlement_order("p1", 0, Decimal::from(1000));
        // Seller allocates base = USDCx 1000.
        let mut proposal = test_proposal("p1");
        proposal.base_instrument = "USDCx".to_string();
        proposal.base_quantity = "1000".to_string();
        let state = SettlementState::new(proposal, false);
        let lm_opt = Some(lm.clone());

        // First attempt: LM commit fails (500 < 1000) → Err, NOTHING latched
        // (LM ordered first, so the tracker was never reached).
        assert!(ensure_reserved(&state, &config, &lm_opt, &tracker, "p1").await.is_err());
        assert!(!lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(500));

        // Balance recovers; retry succeeds and commits exactly the leg once.
        lm.update_token_balance("USDCx", Decimal::from(2000)).await;
        ensure_reserved(&state, &config, &lm_opt, &tracker, "p1").await.unwrap();
        assert!(lm.has_commitment("p1").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000)); // 2000 − 1000, once

        // Idempotent re-entry: no second commit, availability unchanged.
        ensure_reserved(&state, &config, &lm_opt, &tracker, "p1").await.unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
    }

    // Review finding 2: thread_utilization must partition the active set —
    // a backoff entry that is also in-progress (cut-short spawn) or references
    // a no-longer-active proposal must NOT be subtracted from `waiting`, else a
    // genuinely runnable-but-blocked proposal is hidden.
    #[tokio::test]
    async fn test_thread_utilization_partitions_active_set() {
        let config = BaseConfig::test_minimal();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, [0u8; 32])));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend);

        // active: a1 (in-progress + stale backoff entry), a2 (runnable/waiting)
        exec.active_settlements.insert("a1".to_string(), SettlementState::new(test_proposal("a1"), false));
        exec.active_settlements.insert("a2".to_string(), SettlementState::new(test_proposal("a2"), false));
        exec.in_progress.insert("a1".to_string(), Instant::now());
        // a1 also carries a future-dated backoff entry (cut-short case)...
        let future = Instant::now() + Duration::from_secs(300);
        exec.failed_settlements.insert("a1".to_string(), FailedSettlement {
            retry_count: 0, wait_count: 1, next_retry: future,
            first_transient_at: None, cid_waiting: None,
        });
        // ...and a stale backoff entry for a proposal no longer active.
        exec.failed_settlements.insert("ghost".to_string(), FailedSettlement {
            retry_count: 0, wait_count: 1, next_retry: future,
            first_transient_at: None, cid_waiting: None,
        });

        let (in_progress, _max, in_backoff, waiting) = exec.thread_utilization();
        assert_eq!(in_progress, 1);
        assert_eq!(in_backoff, 0); // a1 excluded (in-progress); ghost excluded (not active)
        assert_eq!(waiting, 1);    // a2 is genuinely runnable — must not be hidden
    }
}
