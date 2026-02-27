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
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use crate::order_tracker::{OrderTracker, VerifyResult};
use crate::rpc_client::OrderbookRpcClient;
use crate::runner::{AcceptedRfqTrade, QuotedTrade};
use crate::types::{AdvanceResult, CidWaitingType, FailedSettlement, SettlementStage, SettlementState};

/// Result from a settlement step operation
#[derive(Debug, Clone)]
pub struct StepResult {
    pub contract_id: String,
    pub update_id: String,
    pub traffic_total: u64,
}

/// A contract discovered via on-chain sync
#[derive(Debug, Clone)]
pub struct DiscoveredContract {
    pub settlement_id: String,
    pub contract_id: String,
    /// "DvpProposal" or "Dvp"
    pub contract_type: String,
}

/// A fee payment queued for background processing (fire-and-forget).
/// Persisted to state file on shutdown, restored on restart.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingFee {
    pub proposal_id: String,
    /// "dvp" or "allocate"
    pub fee_type: String,
    pub is_buyer: bool,
    pub pending_traffic: u64,
    #[serde(default)]
    pub retry_count: u32,
    /// USD fee amount from proposal (for CC conversion at queue time)
    #[serde(default)]
    pub fee_amount_usd: String,
    /// Estimated CC needed (computed by backend using current CC/USD rate)
    #[serde(default)]
    pub fee_cc_estimate: f64,
}

/// A traffic fee queued for background processing (fire-and-forget).
/// Persisted to state file on shutdown, restored on restart.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingTrafficFee {
    pub traffic_bytes: u64,
    pub step_name: String,
    pub proposal_id: String,
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
    async fn accept_dvp(&self, proposal_id: &str, dvp_proposal_cid: &str) -> Result<StepResult>;

    /// Allocate tokens for settlement.
    /// `allocation_cc`: Some(amount) if allocating CC amulets (needs amulet pre-selection),
    /// None if allocating CIP-56 tokens.
    async fn allocate(&self, proposal_id: &str, dvp_cid: &str, allocation_cc: Option<Decimal>) -> Result<StepResult>;

    /// Transfer traffic fee to PARTY_TRAFFIC_FEE
    async fn transfer_traffic_fee(&self, traffic_bytes: u64, step_name: &str, proposal_id: &str) -> Result<()>;

    /// Queue a traffic fee at lowest priority (fire-and-forget).
    /// The fee will be processed by the payment queue without blocking the caller.
    fn queue_traffic_fee(&self, traffic_bytes: u64, step_name: &str, proposal_id: &str);

    /// Sync on-chain contracts for given settlement IDs
    async fn sync_contracts(&self, settlement_ids: &[String]) -> Result<Vec<DiscoveredContract>>;

    /// Queue a fee payment for background processing (fire-and-forget).
    /// The fee will be processed by the payment queue without blocking the caller.
    async fn queue_fee_payment(&self, fee: PendingFee);

    /// Get all pending background fee payments (for state persistence on shutdown).
    fn get_pending_fees(&self) -> Vec<PendingFee>;

    /// Restore pending fee payments from saved state (re-queue on restart).
    async fn restore_pending_fees(&self, fees: Vec<PendingFee>);

    /// Get all pending traffic fee payments (for state persistence on shutdown).
    fn get_pending_traffic_fees(&self) -> Vec<PendingTrafficFee>;

    /// Restore pending traffic fee payments from saved state (re-queue on restart).
    fn restore_pending_traffic_fees(&self, fees: Vec<PendingTrafficFee>);

    /// Get current payment queue depth: (allocations, fees, traffic).
    fn queue_depth(&self) -> (u64, u64, u64);

    /// Get amulet cache stats: (available, consumed, reserved, selectable).
    /// Returns None if the backend doesn't use an amulet cache.
    fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        None
    }

    /// Get traffic fee backlog depth (fees waiting beyond the active queue).
    fn traffic_backlog_depth(&self) -> usize {
        0
    }

    /// Get per-pool worker utilization:
    /// (alloc_active, alloc_max, fee_active, fee_max, traffic_active, traffic_max)
    fn worker_utilization(&self) -> Option<(u64, usize, u64, usize, u64, usize)> {
        None
    }

    /// Check if traffic fees are paused (sequencer backpressure).
    /// Returns Some(remaining_secs) if paused, None otherwise.
    fn traffic_fee_pause_secs(&self) -> Option<u64> {
        None
    }

    /// Signal the payment queue to stop dispatching new work (for graceful shutdown).
    fn shutdown(&self) {}
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
    shutting_down: Arc<AtomicBool>,
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
    /// LP: trades we quoted on (for settlement verification by attribute matching)
    quoted_rfq_trades: Option<Arc<Mutex<Vec<QuotedTrade>>>>,
    /// Skip all verification and accept every proposal (migration from old worker without saved state)
    no_reject: bool,
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
            shutting_down: Arc::new(AtomicBool::new(false)),
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
            quoted_rfq_trades: None,
            no_reject: false,
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

    /// Set LP quoted trade tracking (for proposal verification)
    pub fn set_quoted_rfq_trades(&mut self, trades: Arc<Mutex<Vec<QuotedTrade>>>) {
        self.quoted_rfq_trades = Some(trades);
    }

    /// Enable no-reject mode: accept all proposals without verification
    pub fn set_no_reject(&mut self, no_reject: bool) {
        self.no_reject = no_reject;
    }

    /// Get pending background fee payments from the backend (for state persistence).
    pub fn get_pending_fees(&self) -> Vec<PendingFee> {
        self.backend.get_pending_fees()
    }

    /// Get pending traffic fee payments from the backend (for state persistence).
    pub fn get_pending_traffic_fees(&self) -> Vec<PendingTrafficFee> {
        self.backend.get_pending_traffic_fees()
    }

    /// Get current payment queue depth: (allocations, fees, traffic).
    pub fn queue_depth(&self) -> (u64, u64, u64) {
        self.backend.queue_depth()
    }

    /// Get amulet cache stats: (available, consumed, reserved, selectable).
    pub fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        self.backend.cache_stats()
    }

    /// Get traffic fee backlog depth (fees waiting beyond the active queue).
    pub fn traffic_backlog_depth(&self) -> usize {
        self.backend.traffic_backlog_depth()
    }

    /// Get per-pool worker utilization (delegated to backend).
    pub fn worker_utilization(&self) -> Option<(u64, usize, u64, usize, u64, usize)> {
        self.backend.worker_utilization()
    }

    /// Check if traffic fees are paused (sequencer backpressure).
    pub fn traffic_fee_pause_secs(&self) -> Option<u64> {
        self.backend.traffic_fee_pause_secs()
    }

    /// Return (in_progress, max_threads, in_backoff, waiting) for thread utilization logging
    pub fn thread_utilization(&self) -> (usize, usize, usize, usize) {
        let in_progress = self.in_progress.len();
        let in_backoff = self.failed_settlements.values()
            .filter(|f| Instant::now() < f.next_retry)
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
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Signal the backend's payment queue to stop dispatching new work.
    pub fn shutdown_backend(&self) {
        self.backend.shutdown();
    }

    /// Replace the shutdown flag with an externally-provided one (shares runner's Ctrl-C flag)
    pub fn set_shutdown_flag(&mut self, flag: Arc<AtomicBool>) {
        self.shutting_down = flag;
    }

    /// Reject all unconfirmed settlements (still at ProposalReceived stage)
    pub async fn reject_unconfirmed(&mut self) {
        let unconfirmed: Vec<String> = self.active_settlements.iter()
            .filter(|(_, s)| s.stage == SettlementStage::ProposalReceived)
            .map(|(id, _)| id.clone())
            .collect();

        for proposal_id in unconfirmed {
            // Release pending tracker quantity before rejecting
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
                info!(
                    "[{}] Restored proposal from saved state, skipping re-verification (role: {})",
                    proposal_id,
                    if is_buyer { "buyer" } else { "seller" }
                );
                drop(tracker);
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
                tracker.mark_pending(&proposal_id, order_id, base_quantity);
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
        if self.shutting_down.load(Ordering::Relaxed) {
            info!("[{}] Rejecting proposal (shutting down)", proposal_id);
            let state = SettlementState::new(proposal, is_buyer);
            self.active_settlements.insert(proposal_id.clone(), state);
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject proposal during shutdown: {}", proposal_id, e);
                self.active_settlements.shift_remove(&proposal_id);
            }
            return Ok(());
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
                return Ok(());
            }
            // RFQ verified — order_id=0 (no orderbook order to track)
            let order_id = 0u64;
            {
                let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
                let mut tracker = self.tracker.lock().await;
                tracker.mark_pending(&proposal_id, order_id, base_quantity);
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

        // Order verified — mark pending and proceed
        {
            let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
            let mut tracker = self.tracker.lock().await;
            tracker.mark_pending(&proposal_id, order_id, base_quantity);
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
        let priority_ids: Vec<String> = self.needs_readvance.drain()
            .filter(|pid| self.active_settlements.contains_key(pid))
            .collect();
        for pid in &priority_ids {
            self.failed_settlements.remove(pid);
        }

        // Build spawn list: priority IDs first, then remaining active settlements
        let mut proposal_ids = priority_ids;
        for key in self.active_settlements.keys() {
            if !proposal_ids.contains(key) {
                proposal_ids.push(key.clone());
            }
        }

        for proposal_id in proposal_ids {
            // Skip if already being processed by a spawned task
            if self.in_progress.contains_key(&proposal_id) {
                continue;
            }

            // Skip if in backoff from a previous failure
            if let Some(f) = self.failed_settlements.get(&proposal_id) {
                if Instant::now() < f.next_retry {
                    continue;
                }
            }

            // Try to acquire a semaphore permit (non-blocking)
            let permit = match self.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    let total = self.active_settlements.len();
                    let in_progress = self.in_progress.len();
                    warn!(
                        "All {} settlement threads busy ({} in-progress, {} waiting), will retry next cycle",
                        self.config.settlement_thread_count,
                        in_progress,
                        total.saturating_sub(in_progress),
                    );
                    break;
                }
            };

            let state = self.active_settlements.get(&proposal_id).unwrap().clone();
            let config = self.config.clone();
            let backend = Arc::clone(&self.backend);
            let tracker = Arc::clone(&self.tracker);
            let shutting_down = self.shutting_down.clone();
            let action_log = Arc::clone(&self.action_log);
            let pid = proposal_id.clone();
            let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();

            self.in_progress.insert(proposal_id.clone(), Instant::now());
            self.pending_results.push((proposal_id, rx));

            let handle = tokio::spawn(async move {
                let mut permit = Some(permit); // droppable before allocate
                let mut local_state = state;

                // Initial jitter to stagger threads (0-2s) — avoids thundering herd
                let jitter = rand::thread_rng().gen_range(0..2000u64);
                tokio::time::sleep(Duration::from_millis(jitter)).await;

                loop {
                    // Check shutdown flag before each step
                    let is_shutting_down = shutting_down.load(Ordering::Relaxed);

                    let result = tokio::time::timeout(Duration::from_secs(300), async {
                        advance_single(
                            pid.clone(),
                            local_state.clone(),
                            config.clone(),
                            backend.clone(),
                            tracker.clone(),
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

                    if advance_result.should_readvance() && !shutting_down.load(Ordering::Relaxed) {
                        advance_result.apply_to_state(&mut local_state);
                        // Jitter between steps (200-1000ms) to spread ledger load
                        let step_jitter = 200 + rand::thread_rng().gen_range(0..800u64);
                        tokio::time::sleep(Duration::from_millis(step_jitter)).await;
                    } else {
                        let _ = tx.send((advance_result, local_state));
                        break;
                    }
                }
            });
            self.task_handles.push(handle);

            // Stagger spawns to avoid thundering-herd on Canton ledger
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }


    /// Collect completed results from spawned tasks.
    ///
    /// Returns proposal IDs that completed a step and should be re-advanced.
    async fn collect_results(&mut self) -> Vec<String> {
        let mut still_pending = Vec::new();
        let mut completed = Vec::new();
        let mut readvance_ids = Vec::new();

        for (proposal_id, mut rx) in self.pending_results.drain(..) {
            match rx.try_recv() {
                Ok((result, final_state)) => {
                    self.in_progress.remove(&proposal_id);
                    // Update active_settlements with accumulated state from the task
                    if let Some(state) = self.active_settlements.get_mut(&proposal_id) {
                        *state = final_state;
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
                self.rejected_proposals.insert(proposal_id.clone());
                self.active_settlements.shift_remove(&proposal_id);
                self.failed_settlements.remove(&proposal_id);
                self.needs_readvance.remove(&proposal_id);
            }
            AdvanceResult::Terminal { proposal_id } => {
                self.completed_proposals.insert(proposal_id.clone());
                self.active_settlements.shift_remove(&proposal_id);
                self.failed_settlements.remove(&proposal_id);
                self.needs_readvance.remove(&proposal_id);
            }
            AdvanceResult::Wait { proposal_id } => {
                // Cooldown: don't re-poll for 30s. Without this, Wait settlements
                // are re-polled every 2s, consuming semaphore permits and starving
                // actionable settlements (CreateDvp, AcceptDvp) that are later in
                // the iteration order. When counterparty acts, sync_on_chain_contracts
                // adds to needs_readvance which clears the cooldown immediately.
                let entry = self.failed_settlements.entry(proposal_id.clone())
                    .or_insert(FailedSettlement {
                        retry_count: 0,
                        next_retry: Instant::now(),
                        first_transient_at: None,
                        cid_waiting: None,
                    });
                entry.retry_count = 0; // Not a failure — don't accumulate
                entry.next_retry = Instant::now() + Duration::from_secs(30);
                entry.cid_waiting = None;
            }
            AdvanceResult::Error { proposal_id, error } => {
                let is_permanent = error.contains("deadline-exceeded");
                let is_inactive = error.contains("INACTIVE_CONTRACTS");
                let is_transient = !is_permanent && (is_inactive
                    || error.contains("No Dvp contract ID found")
                    || error.contains("No DvpProposal CID found"));
                let entry = self.failed_settlements.entry(proposal_id.clone())
                    .or_insert(FailedSettlement {
                        retry_count: 0,
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
                    {
                        let mut t = self.tracker.lock().await;
                        t.mark_failed(&proposal_id);
                    }
                    self.active_settlements.shift_remove(&proposal_id);
                    self.failed_settlements.remove(&proposal_id);
                    self.needs_readvance.remove(&proposal_id);
                    self.update_actionable_count();
                    return;
                }

                let delay = if is_transient {
                    Duration::from_secs(10)
                } else {
                    FailedSettlement::retry_delay(entry.retry_count)
                };
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
                            debug!(
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
                    {
                        let mut t = self.tracker.lock().await;
                        t.mark_failed(&proposal_id);
                    }
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
            // Queue fee for background processing (fire-and-forget)
            let fee_usd = if state.is_buyer {
                state.proposal.dvp_processing_fee_buyer.clone()
            } else {
                state.proposal.dvp_processing_fee_seller.clone()
            };
            backend.queue_fee_payment(PendingFee {
                proposal_id: proposal_id.clone(),
                fee_type: "dvp".to_string(),
                is_buyer: state.is_buyer,
                pending_traffic: 0,
                retry_count: 0,
                fee_amount_usd: fee_usd,
                fee_cc_estimate: 0.0,
            }).await;
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

                    // Queue traffic fee at lowest priority (fire-and-forget)
                    backend.queue_traffic_fee(result.traffic_total, "propose", &proposal_id);
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
            // Check if proposal has expired before attempting on-chain transaction
            if let Some(created_at) = &state.proposal.created_at {
                let deadline = created_at.seconds + config.settle_before_secs as i64;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                if now > deadline {
                    return AdvanceResult::Error {
                        proposal_id,
                        error: format!(
                            "DvpProposal expired: deadline-exceeded (created {}s ago, max {}s)",
                            now - created_at.seconds, config.settle_before_secs
                        ),
                    };
                }
            }
            action_log.lock().await.push((proposal_id.clone(), "AcceptDvp"));
            let dvp_proposal_cid = match state.dvp_proposal_cid {
                Some(ref cid) => cid.clone(),
                None => return AdvanceResult::Error {
                    proposal_id,
                    error: "No DvpProposal CID found (not yet proposed?)".into(),
                },
            };
            debug!("[{}] Using DvpProposal from on-chain sync: {}", proposal_id, dvp_proposal_cid);
            match backend.accept_dvp(&proposal_id, &dvp_proposal_cid).await {
                Ok(result) => {
                    record_step_completed(
                        &mut rpc_client, &proposal_id, &config.party_id,
                        state.is_buyer, SettlementEventType::DvpAcceptCompleted,
                        &result.update_id, &result.contract_id,
                    ).await;

                    // Queue traffic fee at lowest priority (fire-and-forget)
                    backend.queue_traffic_fee(result.traffic_total, "accept", &proposal_id);
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
            // Queue fee for background processing (fire-and-forget)
            let fee_usd = if state.is_buyer {
                state.proposal.allocation_processing_fee_buyer.clone()
            } else {
                state.proposal.allocation_processing_fee_seller.clone()
            };
            backend.queue_fee_payment(PendingFee {
                proposal_id: proposal_id.clone(),
                fee_type: "allocate".to_string(),
                is_buyer: state.is_buyer,
                pending_traffic: 0,
                retry_count: 0,
                fee_amount_usd: fee_usd,
                fee_cc_estimate: 0.0,
            }).await;
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
            // LP allocates last — wait for operator to witness counterparty's allocation
            let counterparty_alloc = if state.is_buyer {
                status.allocation_seller.as_ref()
            } else {
                status.allocation_buyer.as_ref()
            };
            let counterparty_witnessed = counterparty_alloc
                .map(|s| s.status == 4) // DVP_STEP_STATUS_CONFIRMED (operator witnessed)
                .unwrap_or(false);
            if !counterparty_witnessed {
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
        NextAction::Wait => {
            debug!("[{}] NextAction: Wait (counterparty's turn)", proposal_id);
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
