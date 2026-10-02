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
use indexmap::{IndexMap, IndexSet};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, error, info, warn};

use orderbook_proto::{
    orderbook::{SettlementProposal, SettlementUpdate, settlement_update::EventType},
    DvpStepStatusEnum, GetSettlementStatusResponse, NextAction,
    RecordSettlementEventRequest, SettlementEventType, SettlementEventResult, RecordedByRole,
};

use crate::auth::generate_jwt;
use crate::clock;
use crate::client::OrderbookClient;
use crate::config::BaseConfig;
use crate::liquidity::{self, LiquidityManager};
use crate::order_tracker::{OrderTracker, VerifyResult};
use crate::rpc_client::OrderbookRpcClient;
use crate::runner::{AcceptedRfqTrade, QuotedTrade};
use crate::shutdown::Shutdown;
use crate::supervise::try_spawn;
use crate::sync;
use crate::types::{AdvanceResult, CidWaitingType, FailedSettlement, SettlementStage, SettlementState};

/// Outcome of the server-side user-order verification (Path B of proposal
/// verification). `LookupFailed` is deliberately distinct from `Rejected`:
/// an infrastructure failure (server unreachable, or the market gated as
/// not_found by the inactive-market invisibility change) carries NO verdict
/// about the trade and must never trigger a proposal reject — the proposal is
/// held instead, to be retried or cleaned up by the server's expiry cancel.
#[derive(Debug)]
enum UserOrderVerdict {
    Verified(u64),
    Rejected(String),
    LookupFailed(String),
}

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

/// Stream-touched proposals spawned without start jitter; the rest are staggered.
const STREAM_IMMEDIATE_SPAWNS: usize = 4;

/// Upper bound for one on-chain contract sync.
const SYNC_CONTRACTS_TIMEOUT: Duration = Duration::from_secs(60);

/// Minimum spacing of on-chain syncs while no proposal is waiting for a CID.
const SYNC_MIN_INTERVAL: Duration = Duration::from_secs(15);

/// Poll re-feed delay for a held proposal; doubles per hold up to `HOLD_MAX`.
const HOLD_INITIAL: Duration = Duration::from_secs(30);
const HOLD_MAX: Duration = Duration::from_secs(300);

/// Bound on waiting for in-flight settlement tasks at shutdown; the rest are aborted.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(300);

/// A settlement step still running after this long is logged; it is never cut short.
#[cfg(not(test))]
const STEP_SLOW_AFTER: Duration = Duration::from_secs(300);
/// Short in unit tests, so a step can outlive it in real time.
#[cfg(test)]
const STEP_SLOW_AFTER: Duration = Duration::from_millis(100);

/// Most stream updates taken into one batch.
pub const STREAM_BATCH_MAX: usize = 64;

/// Time budget for handling one stream batch; the remainder stays queued.
pub const STREAM_BATCH_BUDGET: Duration = Duration::from_secs(2);

/// Summary of one `apply_stream_batch` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamBatchOutcome {
    /// Updates taken from the backlog, whatever their result
    pub handled: usize,
    /// Distinct proposals the batch tried to advance
    pub touched: usize,
    /// Advancement tasks spawned for them
    pub spawned: usize,
    /// True when any handled update can change the order grid
    pub affects_grid: bool,
}

/// True for stream events that can free inventory or fill grid orders.
pub fn affects_grid(update: &SettlementUpdate) -> bool {
    match EventType::try_from(update.event_type) {
        Ok(EventType::Settled | EventType::Failed | EventType::Cancelled) => true,
        Ok(EventType::ProposalCreated) => update
            .proposal
            .as_ref()
            .is_some_and(|p| p.order_match.is_some()),
        _ => false,
    }
}

/// A fresh JWT for the configured party.
fn config_jwt(config: &BaseConfig) -> Result<String> {
    generate_jwt(
        &config.party_id,
        &config.role,
        &*config.private_key.expose()?,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
    )
}

/// Wall-clock seconds since the Unix epoch; 0 if the clock is before it.
fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Seconds from `created` to `now`, saturating on out-of-range server values.
fn age_secs(now: i64, created: i64) -> i64 {
    now.saturating_sub(created)
}

/// Stages before the agent's own allocation.
fn is_pre_allocation_stage(stage: SettlementStage) -> bool {
    matches!(
        stage,
        SettlementStage::ProposalReceived
            | SettlementStage::DvpFeePaid
            | SettlementStage::DvpProposed
            | SettlementStage::DvpAccepted
            | SettlementStage::AllocationFeePaid
    )
}

/// True when `result` is the transient missing-CID error for a CID merged in since.
fn cid_since_found(result: &AdvanceResult, gained_proposal_cid: bool, gained_dvp_cid: bool) -> bool {
    let AdvanceResult::Error { error, .. } = result else {
        return false;
    };
    (gained_proposal_cid && error.contains("No DvpProposal CID found"))
        || (gained_dvp_cid && error.contains("No Dvp contract ID found"))
}

/// rfq_v2_only disposition for an encountered V1 settlement: true = leave it
/// alone (do NOT cancel). The server's CancelSettlement guard only refuses
/// after the settlement transaction is submitted — it does NOT protect a
/// proposal this agent has already allocated for, which can still settle via
/// the operator without any further agent action. So the agent self-gates on
/// its OWN allocation step (Submitted/Completed/Confirmed); Failed/Rejected/
/// Withdrawn/Cancelled/Timeout allocation attempts left nothing standing and
/// are safe to cancel. The settlement-step and terminal-stage checks are
/// belt-and-braces against races with the operator between the status read
/// and the cancel. `stage` alone is party-agnostic (Allocating/Allocated can
/// mean only the COUNTERPARTY allocated), so it never triggers leave-alone
/// below Settling.
fn v1_settlement_leave_alone(status: &GetSettlementStatusResponse, is_buyer: bool) -> bool {
    fn at_least_submitted(step: Option<&orderbook_proto::DvpStepStatus>) -> bool {
        matches!(
            step.map(|s| s.status),
            Some(s) if s == DvpStepStatusEnum::DvpStepStatusSubmitted as i32
                || s == DvpStepStatusEnum::DvpStepStatusCompleted as i32
                || s == DvpStepStatusEnum::DvpStepStatusConfirmed as i32
        )
    }
    let my_alloc = if is_buyer {
        status.allocation_buyer.as_ref()
    } else {
        status.allocation_seller.as_ref()
    };
    // Settling=10, Settled=11, Failed=12, Cancelled=13 (contiguous tail).
    at_least_submitted(my_alloc)
        || at_least_submitted(status.settlement.as_ref())
        || status.stage >= orderbook_proto::SettlementStage::Settling as i32
}

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
    async fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
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
    async fn holdings_histogram(&self, _token: &str) -> Option<HoldingsHistogram> {
        None
    }

    /// Check if regular fees are paused (sequencer backpressure).
    /// Returns Some(remaining_secs) if paused, None otherwise.
    fn fee_pause_secs(&self) -> Option<u64> {
        None
    }

    /// Check if background housekeeping (DvpProposal GC) is paused by sequencer
    /// backpressure. Returns Some(remaining_secs) if paused, None otherwise.
    ///
    /// Separate from [`Self::fee_pause_secs`] because it runs several times
    /// longer: deadline-free work is the last thing that should resume
    /// competing for sequencer slots.
    fn background_pause_secs(&self) -> Option<u64> {
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
    /// Stream-touched while a task was running; a Wait result re-advances them
    rearm_on_result: HashSet<String>,
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
    /// Held proposals the poll skips until the instant; the delay doubles per hold
    held_until: HashMap<String, (Instant, Duration)>,
    /// Start of the last on-chain contract sync
    last_sync: Option<Instant>,
    /// User-order lookups attempted against the server
    #[cfg(test)]
    server_lookups: usize,
    /// Answers user-order lookups in place of the server
    #[cfg(test)]
    stub_orders: Option<Vec<orderbook_proto::orderbook::Order>>,
    /// User-order lookups never answer
    #[cfg(test)]
    stub_lookup_hangs: bool,
    /// Rejects never complete, as against a server that never answers
    #[cfg(test)]
    reject_black_hole: bool,
}

impl<B: SettlementBackend + 'static> SettlementExecutor<B> {
    /// Create a new settlement executor with shared order tracker and backend.
    /// Fails when `settlement_thread_count` is outside the semaphore's range.
    pub fn new(config: &BaseConfig, tracker: Arc<Mutex<OrderTracker>>, backend: B) -> Result<Self> {
        let semaphore = sync::semaphore(config.settlement_thread_count)?;
        Ok(Self {
            config: config.clone(),
            backend: Arc::new(backend),
            active_settlements: IndexMap::new(),
            rejected_proposals: HashSet::new(),
            completed_proposals: HashSet::new(),
            shutdown: Shutdown::new(),
            tracker,
            query_client: None,
            semaphore: Arc::new(semaphore),
            in_progress: HashMap::new(),
            failed_settlements: HashMap::new(),
            pending_results: Vec::new(),
            task_handles: Vec::new(),
            needs_readvance: HashSet::new(),
            rearm_on_result: HashSet::new(),
            action_log: Arc::new(Mutex::new(Vec::new())),
            actionable_count: Arc::new(AtomicUsize::new(0)),
            accepted_rfq_trades: None,
            rejected_rfq_trades: None,
            quoted_rfq_trades: None,
            no_reject: false,
            liquidity_manager: None,
            held_until: HashMap::new(),
            last_sync: None,
            #[cfg(test)]
            server_lookups: 0,
            #[cfg(test)]
            stub_orders: None,
            #[cfg(test)]
            stub_lookup_hangs: false,
            #[cfg(test)]
            reject_black_hole: false,
        })
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
            try_spawn("liquidity release", async move { lm.release(&pid).await });
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
        try_spawn("best-effort settlement cancel", async move {
            let jwt = match config_jwt(&config) {
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

    /// rfq_v2_only: actively abort an encountered V1 settlement instead of
    /// adopting it (server-side cancel via the unary CancelSettlement RPC —
    /// no V1 LP stream needed). Proposals this agent already allocated for —
    /// or whose settlement transaction is already in flight / terminal — are
    /// left alone: they can still settle via the operator without agent
    /// action (see `v1_settlement_leave_alone`). Failures do NOT insert into
    /// `rejected_proposals`, so `poll_pending_proposals` re-surfaces the
    /// proposal and retries at poll cadence; the server cancel is idempotent
    /// (already-terminal → success=true).
    async fn abort_v1_settlement(&mut self, proposal: &SettlementProposal, is_buyer: bool) {
        let proposal_id = proposal.proposal_id.clone();

        let jwt = match self.create_jwt() {
            Ok(j) => j,
            Err(e) => {
                warn!("[{}] rfq_v2_only abort: JWT generation failed: {:#} — retrying next poll", proposal_id, e);
                return;
            }
        };
        let mut rpc_client =
            match OrderbookRpcClient::connect(&self.config.orderbook_grpc_url, Some(jwt)).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("[{}] rfq_v2_only abort: RPC connect failed: {:#} — retrying next poll", proposal_id, e);
                    return;
                }
            };
        let status = match rpc_client.get_settlement_status(&proposal_id).await {
            Ok(s) => s,
            Err(e) => {
                warn!("[{}] rfq_v2_only abort: GetSettlementStatus failed: {:#} — retrying next poll", proposal_id, e);
                return;
            }
        };

        if v1_settlement_leave_alone(&status, is_buyer) {
            info!(
                "[{}] rfq_v2_only: leaving in-flight V1 settlement to complete/expire \
                 (stage={}, own allocation or settlement tx already submitted)",
                proposal_id, status.stage
            );
            // Do NOT mark_failed here: a later Settled stream event needs the
            // preserved settlement_orders entry for tracker accounting.
            self.rejected_proposals.insert(proposal_id);
            return;
        }

        match rpc_client
            .cancel_settlement(
                &proposal_id,
                "rfq_v2_only: agent is RFQ V2 (AtomicDVP) only; aborting V1 settlement",
            )
            .await
        {
            Ok(true) => {
                info!(
                    "[{}] rfq_v2_only: V1 settlement aborted server-side (stage={})",
                    proposal_id, status.stage
                );
                // Release a restored order reservation, if any (no-op when
                // untracked) — mirrors the restored-abandon arm above.
                {
                    let mut tracker = self.tracker.lock().await;
                    tracker.mark_failed(&proposal_id);
                }
                // Buyer fill-loop feedback parity with the reject arms.
                if let Some(ref rejected) = self.rejected_rfq_trades {
                    rejected.lock().await.insert(proposal_id.clone());
                }
                self.rejected_proposals.insert(proposal_id);
            }
            Ok(false) => {
                // Settlement tx submitted between the status read and the
                // cancel — next poll's status check classifies it leave-alone.
                warn!("[{}] rfq_v2_only: server declined cancel — re-checking next poll", proposal_id);
            }
            Err(e) => {
                warn!("[{}] rfq_v2_only: cancel RPC failed: {:#} — retrying next poll", proposal_id, e);
            }
        }
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
        let age = age_secs(unix_now_secs(), created_at.seconds);
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
                try_spawn("liquidity inflow record", async move { lm.record_inflow(&token_key, amount).await });
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
    pub async fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        self.backend.cache_stats().await
    }

    /// Get per-pool worker utilization (delegated to backend).
    pub fn worker_utilization(&self) -> Option<(u64, usize, u64, usize)> {
        self.backend.worker_utilization()
    }

    /// Per-instrument RFQ V2 holdings histogram (delegated to backend).
    pub async fn holdings_histogram(&self, token: &str) -> Option<HoldingsHistogram> {
        self.backend.holdings_histogram(token).await
    }

    /// Check if regular fees are paused (sequencer backpressure).
    pub fn fee_pause_secs(&self) -> Option<u64> {
        self.backend.fee_pause_secs()
    }

    /// Check if background housekeeping is paused (sequencer backpressure).
    pub fn background_pause_secs(&self) -> Option<u64> {
        self.backend.background_pause_secs()
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
                stuck_10m = stuck_10m.saturating_add(1);
            }
        }

        let total = no_proposal_ids.len().saturating_add(no_dvp_ids.len());
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
        self.query_client
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("query client unavailable"))
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
        let jwt = config_jwt(config)?;
        let mut rpc_client = OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await?;
        let success = rpc_client.cancel_settlement(proposal_id, reason).await?;
        if success {
            info!("[{}] Settlement cancelled: {}", proposal_id, reason);
            self.release_commitment(proposal_id);
            self.active_settlements.shift_remove(proposal_id);
            self.in_progress.remove(proposal_id);
            self.rearm_on_result.remove(proposal_id);
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
                    self.rearm_on_result.remove(&proposal.proposal_id);
                    self.held_until.remove(&proposal.proposal_id);
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
                    self.rearm_on_result.remove(&proposal.proposal_id);
                    self.held_until.remove(&proposal.proposal_id);
                }
            }
            _ => {}
        }

        self.update_actionable_count();
        Ok(())
    }

    /// Handle a new settlement proposal
    async fn handle_proposal_created(&mut self, proposal: SettlementProposal) -> Result<()> {
        // Only the hold arms below re-arm a hold; every other outcome ends it
        let prior_hold = self.held_until.remove(&proposal.proposal_id).map(|(_, delay)| delay);

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

        // Amounts no real trade carries are refused before any path can adopt them
        if let Some((field, raw)) = implausible_amount(&proposal) {
            warn!("[{}] Rejecting proposal: implausible {} {}", proposal_id, field, raw);
            // A proposal restored from saved state releases what it reserved
            self.tracker.lock().await.mark_failed(&proposal_id);
            self.release_commitment(&proposal_id);
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject: {}", proposal_id, e);
            }
            if let Some(ref rejected) = self.rejected_rfq_trades {
                rejected.lock().await.insert(proposal_id.clone());
            }
            return Ok(());
        }

        // rfq_v2_only: never adopt V1 settlements — every proposal reaching
        // this pipeline is V1 by construction (RFQ V2 / AtomicDVP settles
        // atomically and never creates settlement proposals). Actively abort
        // it server-side unless this agent already allocated (it can still
        // settle via the operator) or the settlement tx is already in flight.
        // Must precede the restore / --no-reject / liquidity-not-ready
        // bypasses below, all of which would otherwise adopt.
        if self.config.rfq_v2_only {
            self.abort_v1_settlement(&proposal, is_buyer).await;
            return Ok(());
        }

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
                    let age = age_secs(unix_now_secs(), created_at.seconds);
                    if age > settle_window as i64 {
                        info!(
                            "[{}] Restored proposal past settle deadline ({}s old, max {}s) — abandoning instead of re-advancing",
                            proposal_id, age, settle_window
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
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject proposal during shutdown: {}", proposal_id, e);
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
            if let Err(e) = self.reject_proposal(&proposal_id).await {
                warn!("[{}] Failed to reject: {}", proposal_id, e);
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
        if let Some(lm) = self.liquidity_manager.clone() {
            if !lm.is_ready().await {
                info!("[{}] Balances not loaded yet, deferring preconfirmation", proposal_id);
                self.hold(&proposal_id, prior_hold);
                return Ok(());
            }
            let (allocation_token, allocation_amount, my_fees_usd) =
                reservation_inputs(&proposal, is_buyer, &self.config.cc_token_id);
            // A stale balance defers preconfirmation the same way an unloaded
            // one does — never commit to a settlement from an aged number.
            if lm.is_stale(&allocation_token).await.is_some() {
                info!("[{}] Balances stale, deferring preconfirmation", proposal_id);
                self.hold(&proposal_id, prior_hold);
                return Ok(());
            }
            let fee_cc = lm.estimate_fee_cc(my_fees_usd).await;

            if let Err(reason) = lm.can_commit(&allocation_token, allocation_amount, fee_cc).await {
                warn!("[{}] Rejecting proposal: {}", proposal_id, reason);
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
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
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
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
                if let Err(e) = self.reject_proposal(&proposal_id).await {
                    warn!("[{}] Failed to reject: {}", proposal_id, e);
                }
                return Ok(());
            }
            VerifyResult::PlacementInFlight { order_id: pending_id } => {
                // Neither adopted nor rejected; the next poll re-feeds it once tracked
                info!("[{proposal_id}] Order {pending_id} placement in flight, holding proposal");
                return Ok(());
            }
            VerifyResult::NeedServerLookup { order_id: lookup_id } => {
                // Path B: User order — fetch from server and verify
                info!("[{}] Order {} not in tracker, fetching from server", proposal_id, lookup_id);
                // Held across the lookup, so a cancelled handler keeps its backoff
                self.hold(&proposal_id, prior_hold);
                let market_id = proposal.market_id.clone();
                match self.verify_user_order(&proposal, lookup_id, &market_id).await {
                    UserOrderVerdict::Verified(oid) => {
                        self.held_until.remove(&proposal_id);
                        oid
                    }
                    UserOrderVerdict::Rejected(reason) => {
                        self.held_until.remove(&proposal_id);
                        warn!("[{}] User order verification failed: {}", proposal_id, reason);
                        if let Err(e) = self.reject_proposal(&proposal_id).await {
                            warn!("[{}] Failed to reject: {}", proposal_id, e);
                        }
                        return Ok(());
                    }
                    UserOrderVerdict::LookupFailed(reason) => {
                        // No verdict — HOLD, never reject. The proposal stays
                        // pending: a later notification retries it, and if none
                        // comes the server's expiry cancel releases it cleanly.
                        // Rejecting here burned legitimate settlements when the
                        // lookup failed for infra reasons (e.g. the market
                        // deactivated after the match — GetOrders answers
                        // not_found under the invisibility invariant).
                        warn!(
                            "[{}] User order lookup failed, holding proposal (no verdict): {}",
                            proposal_id, reason
                        );
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

    /// Keep a held proposal out of poll re-feeds: `HOLD_INITIAL`, doubling up to `HOLD_MAX`.
    fn hold(&mut self, proposal_id: &str, prior: Option<Duration>) {
        let delay = prior.map_or(HOLD_INITIAL, |d| d.saturating_mul(2).min(HOLD_MAX));
        let now = Instant::now();
        let until = now.checked_add(delay).unwrap_or(now);
        self.held_until.insert(proposal_id.to_string(), (until, delay));
    }

    /// Proposals currently held (neither adopted nor rejected yet).
    pub fn held_count(&self) -> usize {
        self.held_until.len()
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
        let readvance_ids = self.collect_results().await;
        self.needs_readvance.extend(readvance_ids);

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

        let mut proposal_ids: Vec<String> = Vec::with_capacity(priority_ids.len().saturating_add(tail.len()));
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
                skipped_in_progress = skipped_in_progress.saturating_add(1);
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
                        > crate::order_manager::deadline_after(Duration::from_secs(EXPIRED_RETRY_SECS))
                        && self.past_deadline(&proposal_id);
                    if !cut_short {
                        skipped_backoff = skipped_backoff.saturating_add(1);
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

            if self.spawn_settlement_task(proposal_id, permit, true) {
                spawned = spawned.saturating_add(1);
            }

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

    /// Handle queued stream updates in order within `budget`, then advance
    /// each proposal they touched once. Unhandled updates stay in `backlog`.
    pub async fn apply_stream_batch(
        &mut self,
        backlog: &mut VecDeque<SettlementUpdate>,
        per_update: Duration,
        budget: Duration,
    ) -> StreamBatchOutcome {
        let started = Instant::now();
        let mut outcome = StreamBatchOutcome::default();
        let mut touched: IndexSet<String> = IndexSet::new();

        while !self.shutdown.is_shutting_down() {
            // The first update always runs, so a zero budget still progresses
            if outcome.handled > 0 && started.elapsed() >= budget {
                break;
            }
            let Some(update) = backlog.pop_front() else { break };
            outcome.handled = outcome.handled.saturating_add(1);
            outcome.affects_grid |= affects_grid(&update);

            let update_proposal_id = update.proposal.as_ref().map(|p| p.proposal_id.clone());
            let update_desc = format!(
                "{} event={} market={}",
                update.proposal.as_ref().map(|p| p.proposal_id.as_str()).unwrap_or("?"),
                update.event_type,
                update.proposal.as_ref().map(|p| p.market_id.as_str()).unwrap_or("?"),
            );
            match tokio::time::timeout(per_update, self.handle_settlement_update(update)).await {
                Ok(Ok(())) => {
                    if let Some(pid) = update_proposal_id {
                        touched.insert(pid);
                    }
                }
                Ok(Err(e)) => error!("[{}] Error handling settlement update: {:#}", update_desc, e),
                Err(_) => warn!(
                    "[{}] Settlement update handling timed out after {}s",
                    update_desc,
                    per_update.as_secs()
                ),
            }
        }

        outcome.touched = touched.len();
        outcome.spawned = self.advance_proposals(touched).await;
        outcome
    }

    /// Advance the given proposals now (stream-driven), each at most once.
    /// Busy permits or the spawn cap defer the rest to `needs_readvance`.
    pub async fn advance_proposals(&mut self, proposal_ids: IndexSet<String>) -> usize {
        // Collect finished results first (frees semaphore permits)
        let readvance_ids = self.collect_results().await;
        self.needs_readvance.extend(readvance_ids);

        let mut spawned = 0usize;
        for proposal_id in proposal_ids {
            // Must be active and not already in-progress
            if !self.active_settlements.contains_key(&proposal_id) {
                continue;
            }
            if self.in_progress.contains_key(&proposal_id) {
                // The running task may return a Wait from state older than this event
                self.rearm_on_result.insert(proposal_id);
                continue;
            }

            // Clear backoff — stream update means new state to process
            self.failed_settlements.remove(&proposal_id);
            self.needs_readvance.remove(&proposal_id);

            if spawned >= MAX_ADVANCE_SPAWNS_PER_CYCLE || self.shutdown.is_shutting_down() {
                self.needs_readvance.insert(proposal_id);
                continue;
            }
            let Ok(permit) = self.semaphore.clone().try_acquire_owned() else {
                self.needs_readvance.insert(proposal_id);
                continue;
            };

            // Act now for the first few; stagger the rest of a large batch
            let jitter = spawned >= STREAM_IMMEDIATE_SPAWNS;
            if self.spawn_settlement_task(proposal_id, permit, jitter) {
                spawned = spawned.saturating_add(1);
            }
        }
        spawned
    }

    /// Spawn a single settlement advancement task; false if the proposal is gone.
    /// Shared by advance_all_settlements() and advance_proposals().
    fn spawn_settlement_task(
        &mut self,
        proposal_id: String,
        permit: tokio::sync::OwnedSemaphorePermit,
        initial_jitter: bool,
    ) -> bool {
        let Some(state) = self.active_settlements.get(&proposal_id).cloned() else {
            drop(permit);
            return false;
        };
        let config = self.config.clone();
        let backend = Arc::clone(&self.backend);
        let tracker = Arc::clone(&self.tracker);
        let liquidity_manager = self.liquidity_manager.clone();
        let shutdown = self.shutdown.clone();
        let action_log = Arc::clone(&self.action_log);
        let pid = proposal_id.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();

        let handle = try_spawn("settlement task", async move {
            let mut permit = Some(permit); // droppable before allocate

            if initial_jitter {
                // Initial jitter to stagger threads (0-2s) — wakes early on shutdown
                let jitter = clock::jitter_ms(2000);
                shutdown.sleep(Duration::from_millis(jitter)).await;
            }

            let step = |local_state: SettlementState, is_shutting_down: bool| {
                advance_single(
                    pid.clone(),
                    local_state,
                    config.clone(),
                    backend.clone(),
                    tracker.clone(),
                    liquidity_manager.clone(),
                    is_shutting_down,
                    action_log.clone(),
                )
            };
            let (advance_result, local_state) = step_until_done(&pid, state, &shutdown, step).await;
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
                        let jwt = match config_jwt(&config) {
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
                return;
            }

            let _ = tx.send((advance_result, local_state));
        });
        let Some(handle) = handle else { return false };
        self.in_progress.insert(proposal_id.clone(), Instant::now());
        self.pending_results.push((proposal_id, rx));
        self.task_handles.push(handle);
        true
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
                Ok((result, mut final_state)) => {
                    self.in_progress.remove(&proposal_id);
                    let mut gained = (false, false);
                    // Update active_settlements with accumulated state from the task
                    if let Some(state) = self.active_settlements.get_mut(&proposal_id) {
                        gained = (
                            final_state.dvp_proposal_cid.is_none() && state.dvp_proposal_cid.is_some(),
                            final_state.dvp_cid.is_none() && state.dvp_cid.is_some(),
                        );
                        // Keep CIDs that sync discovered while the task was in flight
                        final_state.dvp_proposal_cid =
                            final_state.dvp_proposal_cid.or(state.dvp_proposal_cid.take());
                        final_state.dvp_cid = final_state.dvp_cid.or(state.dvp_cid.take());
                        final_state.allocation_cid =
                            final_state.allocation_cid.or(state.allocation_cid.take());
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
                    completed.push((result, gained));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still running
                    still_pending.push((proposal_id, rx));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    // Task panicked or was cancelled
                    warn!("[{}] Settlement task dropped without sending result", proposal_id);
                    self.in_progress.remove(&proposal_id);
                    self.rearm_on_result.remove(&proposal_id);
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
        for (result, (gained_proposal_cid, gained_dvp_cid)) in completed {
            let pid = result.proposal_id().to_string();
            let is_wait = matches!(result, AdvanceResult::Wait { .. });
            let cid_found = cid_since_found(&result, gained_proposal_cid, gained_dvp_cid);
            if result.should_readvance() {
                readvance_ids.push(pid.clone());
            }
            self.apply_result(result).await;
            // Touched while running: drop the cooldown a stale Wait just set
            if self.rearm_on_result.remove(&pid) && is_wait && self.active_settlements.contains_key(&pid) {
                self.failed_settlements.remove(&pid);
                readvance_ids.push(pid);
            } else if cid_found && self.active_settlements.contains_key(&pid) {
                // Sync merged the missing CID while the task ran: retry without the CID backoff
                self.failed_settlements.remove(&pid);
                readvance_ids.push(pid);
            }
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
                // / stream-driven advance_proposals / step completion all
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
                entry.next_retry = crate::order_manager::deadline_after(delay);
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
                    entry.retry_count = entry.retry_count.saturating_add(1);
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
                entry.next_retry = crate::order_manager::deadline_after(delay);
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
                entry.retry_count = entry.retry_count.saturating_add(1);

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
                entry.next_retry = crate::order_manager::deadline_after(Duration::from_secs(10));
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
        self.drain_tasks_within(DRAIN_TIMEOUT).await
    }

    /// Wait up to `limit` for in-progress tasks, then abort the ones still running.
    async fn drain_tasks_within(&mut self, limit: Duration) -> usize {
        let handles: Vec<_> = self.task_handles.drain(..).collect();
        let count = handles.len();
        if count > 0 {
            info!("Waiting for {} in-progress settlement task(s)...", count);
            let aborts: Vec<_> = handles.iter().map(|h| h.abort_handle()).collect();
            if tokio::time::timeout(limit, join_all(handles)).await.is_err() {
                let left = aborts.iter().filter(|a| !a.is_finished()).count();
                warn!("{} settlement task(s) still running after {}s; aborting them", left, limit.as_secs());
                for a in &aborts {
                    a.abort();
                }
            }
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
        self.feed_polled_proposals(proposals).await;
        true
    }

    /// Feed polled proposals through the normal handler, skipping held ones;
    /// holds on ids the server no longer returns are dropped.
    async fn feed_polled_proposals(&mut self, proposals: Vec<SettlementProposal>) {
        let returned: HashSet<&str> = proposals.iter().map(|p| p.proposal_id.as_str()).collect();
        self.held_until.retain(|id, _| returned.contains(id.as_str()));

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
            let now = Instant::now();
            if self.held_until.get(&proposal.proposal_id).is_some_and(|(until, _)| now < *until) {
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
    }

    /// True when a sync can help: every call while a proposal waits for a CID, else
    /// at most every `SYNC_MIN_INTERVAL` while a pre-allocation settlement lacks its Dvp CID.
    fn sync_due(&self, now: Instant) -> bool {
        let cid_waiting = self.failed_settlements.iter().any(|(id, f)| {
            f.cid_waiting.is_some() && self.active_settlements.contains_key(id)
        });
        if cid_waiting {
            return true;
        }
        let lacks_cid = self.active_settlements.values().any(|s| {
            is_pre_allocation_stage(s.stage) && s.dvp_cid.is_none()
        });
        lacks_cid
            && self
                .last_sync
                .is_none_or(|t| now.saturating_duration_since(t) >= SYNC_MIN_INTERVAL)
    }

    /// Sync on-chain DvpProposal and Dvp contracts with local state.
    pub async fn sync_on_chain_contracts(&mut self) {
        let now = Instant::now();
        if self.active_settlements.is_empty() || !self.sync_due(now) {
            return;
        }
        self.last_sync = Some(now);

        let settlement_ids: Vec<String> = self.active_settlements.keys().cloned().collect();
        let sync = self.backend.sync_contracts(&settlement_ids);
        let contracts = match tokio::time::timeout(SYNC_CONTRACTS_TIMEOUT, sync).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                warn!("sync_on_chain_contracts: {} ({} active settlements)", e, settlement_ids.len());
                return;
            }
            Err(_) => {
                warn!(
                    "sync_on_chain_contracts: timed out after {}s ({} active settlements)",
                    SYNC_CONTRACTS_TIMEOUT.as_secs(),
                    settlement_ids.len()
                );
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
                found_proposals = found_proposals.saturating_add(1);
            } else if contract.contract_type == "Dvp" && state.dvp_cid.is_none() {
                debug!("[{}] Discovered Dvp on-chain: {}", contract.settlement_id, contract.contract_id);
                state.dvp_cid = Some(contract.contract_id.clone());
                self.failed_settlements.remove(&contract.settlement_id);
                found_dvps = found_dvps.saturating_add(1);
            } else if contract.contract_type == "Allocation" && state.allocation_cid.is_none() {
                debug!("[{}] Discovered Allocation on-chain: {}", contract.settlement_id, contract.contract_id);
                state.allocation_cid = Some(contract.contract_id.clone());
                self.failed_settlements.remove(&contract.settlement_id);
                found_allocations = found_allocations.saturating_add(1);
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

    /// Verify a user order by fetching it from the server.
    ///
    /// Distinguishes a definitive verdict from a failed lookup — only the
    /// former may drive a proposal reject (see the call site).
    async fn verify_user_order(
        &mut self,
        proposal: &SettlementProposal,
        order_id: u64,
        market_id: &str,
    ) -> UserOrderVerdict {
        // Infrastructure failures are NOT verdicts. In particular, GetOrders
        // with a market filter returns NOT_FOUND for a market deactivated
        // after the match — rejecting on that would burn a legitimate
        // in-flight settlement on what is an infra/lookup condition. Held
        // proposals are retried on the next server notification or cleaned up
        // by the server's expiry cancel; both are recoverable, a reject is not.
        #[cfg(test)]
        {
            self.server_lookups = self.server_lookups.saturating_add(1);
        }
        let orders = match self.lookup_active_orders(market_id).await {
            Ok(o) => o,
            Err(reason) => return UserOrderVerdict::LookupFailed(reason),
        };

        // The server answered — an absent order is a definitive verdict.
        let Some(order) = orders.into_iter().find(|o| o.order_id == order_id) else {
            return UserOrderVerdict::Rejected(format!("Order {} not found on server", order_id));
        };

        let mut tracker = self.tracker.lock().await;
        // Our own order booked by a submit that errored: capacity starts fresh, as for any agent order
        if tracker.adopt_failed_submit(&order, Instant::now()) {
            return match tracker.verify_settlement(proposal, &self.config.party_id) {
                VerifyResult::Accepted { order_id } => UserOrderVerdict::Verified(order_id),
                VerifyResult::Rejected { reason } => UserOrderVerdict::Rejected(reason),
                VerifyResult::NeedServerLookup { .. } | VerifyResult::PlacementInFlight { .. } => {
                    UserOrderVerdict::LookupFailed("adopted order not tracked".to_string())
                }
            };
        }
        match tracker.verify_and_import_order(&order, proposal) {
            VerifyResult::Accepted { order_id } => UserOrderVerdict::Verified(order_id),
            VerifyResult::Rejected { reason } => UserOrderVerdict::Rejected(reason),
            VerifyResult::NeedServerLookup { .. } => {
                UserOrderVerdict::Rejected("Unexpected NeedServerLookup".to_string())
            }
            VerifyResult::PlacementInFlight { .. } => {
                UserOrderVerdict::LookupFailed("order placement still in flight".to_string())
            }
        }
    }

    /// Live orders in `market_id` for a user-order lookup; Err carries the failure reason.
    async fn lookup_active_orders(
        &mut self,
        market_id: &str,
    ) -> Result<Vec<orderbook_proto::orderbook::Order>, String> {
        #[cfg(test)]
        if self.stub_lookup_hangs {
            std::future::pending::<()>().await;
        }
        #[cfg(test)]
        if let Some(orders) = self.stub_orders.clone() {
            return Ok(orders);
        }
        let client = self
            .get_query_client()
            .await
            .map_err(|e| format!("Failed to create query client: {e}"))?;
        client
            .get_active_orders(market_id)
            .await
            .map_err(|e| format!("Failed to fetch orders: {e}"))
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

    /// Reject a proposal (send preconfirmation with accept=false) and remove it.
    /// New proposals are never made active before this, so a dropped reject leaves nothing to accept.
    async fn reject_proposal(&mut self, proposal_id: &str) -> Result<()> {
        #[cfg(test)]
        if self.reject_black_hole {
            std::future::pending::<()>().await;
        }
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
        config_jwt(&self.config)
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

    let (dvp_fee, allocation_fee) = if is_buyer {
        (&proposal.dvp_processing_fee_buyer, &proposal.allocation_processing_fee_buyer)
    } else {
        (&proposal.dvp_processing_fee_seller, &proposal.allocation_processing_fee_seller)
    };
    // An overflowing sum reads as unaffordable
    let my_fees_usd = Decimal::from_str(dvp_fee)
        .unwrap_or_default()
        .checked_add(Decimal::from_str(allocation_fee).unwrap_or_default())
        .unwrap_or(Decimal::MAX);

    (allocation_token, allocation_amount, my_fees_usd)
}

/// Largest amount a proposal may carry in any quantity or fee field.
const MAX_PROPOSAL_AMOUNT: u64 = 1_000_000_000_000_000_000;

/// First quantity or fee that parses but is negative or above [`MAX_PROPOSAL_AMOUNT`].
fn implausible_amount(proposal: &SettlementProposal) -> Option<(&'static str, &str)> {
    let max = Decimal::from(MAX_PROPOSAL_AMOUNT);
    [
        ("base_quantity", proposal.base_quantity.as_str()),
        ("quote_quantity", proposal.quote_quantity.as_str()),
        ("dvp_processing_fee_buyer", proposal.dvp_processing_fee_buyer.as_str()),
        ("dvp_processing_fee_seller", proposal.dvp_processing_fee_seller.as_str()),
        ("allocation_processing_fee_buyer", proposal.allocation_processing_fee_buyer.as_str()),
        ("allocation_processing_fee_seller", proposal.allocation_processing_fee_seller.as_str()),
    ]
    .into_iter()
    .find(|(_, raw)| Decimal::from_str(raw).is_ok_and(|v| v < Decimal::ZERO || v > max))
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
    if let (Some(lm), Some((allocation_token, allocation_amount, my_fees_usd))) =
        (liquidity_manager, inputs.as_ref())
    {
        if !lm.has_commitment(proposal_id).await {
            let fee_cc = lm.estimate_fee_cc(*my_fees_usd).await;
            lm.try_commit(proposal_id, allocation_token, *allocation_amount, fee_cc)
                .await?;
            info!(
                "[{}] Reserved {} {} + {:.4} CC fees (counterparty committed)",
                proposal_id, allocation_amount, allocation_token, liquidity::shown(fee_cc)
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
    let age = age_secs(now_secs, created_at_secs);
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

/// Run `step` until a result is not re-advanced or needs an allocation; every
/// step runs to its end. Returns that result and the state it ended with.
#[doc(hidden)]
pub async fn step_until_done<S, F>(
    pid: &str,
    mut local_state: SettlementState,
    shutdown: &Shutdown,
    mut step: S,
) -> (AdvanceResult, SettlementState)
where
    S: FnMut(SettlementState, bool) -> F,
    F: std::future::Future<Output = AdvanceResult>,
{
    loop {
        // Check shutdown flag before each step
        let is_shutting_down = shutdown.is_shutting_down();
        let next = step(local_state.clone(), is_shutting_down);
        let advance_result = await_step(next, STEP_SLOW_AFTER, pid, &local_state).await;

        let again = advance_result.should_readvance() && !shutdown.is_shutting_down();
        if matches!(advance_result, AdvanceResult::NeedsAllocate { .. }) || !again {
            return (advance_result, local_state);
        }
        advance_result.apply_to_state(&mut local_state);
        // Jitter between steps (200-1000ms) — wakes early on shutdown
        let step_jitter = 200u64.saturating_add(clock::jitter_ms(800));
        shutdown.sleep(Duration::from_millis(step_jitter)).await;
    }
}

/// Await one settlement step to its end, warning once it runs past `slow_after`.
async fn await_step<T>(
    step: impl std::future::Future<Output = T>,
    slow_after: Duration,
    pid: &str,
    state: &SettlementState,
) -> T {
    let on_slow = || {
        warn!(
            "[{}] Settlement step still running after {}s (role={}, stage={}); waiting for it to finish",
            pid,
            slow_after.as_secs(),
            state.role(),
            state.stage,
        );
    };
    crate::supervise::run_to_end(step, slow_after, on_slow).await.0
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
    let jwt = match config_jwt(&config) {
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
        let now = unix_now_secs();
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

        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
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

    // A step past 300s used to be cut, possibly mid-submit, and re-advanced blind
    #[tokio::test]
    async fn a_slow_settlement_step_runs_to_its_end() {
        let state = SettlementState::new(test_proposal("p1"), false);
        let step = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            AdvanceResult::Wait { proposal_id: "p1".to_string() }
        };
        let result = await_step(step, Duration::from_millis(50), "p1", &state).await;
        assert!(matches!(result, AdvanceResult::Wait { .. }), "the step's own result comes back");
    }

    // The settlement task's own step loop: a step past the slow mark keeps its result
    #[tokio::test]
    async fn the_settlement_task_keeps_a_slow_steps_result() {
        let shutdown = Shutdown::new();
        let mut calls = 0u32;
        let step = |state: SettlementState, _: bool| {
            calls += 1;
            let first = calls == 1;
            async move {
                if first {
                    tokio::time::sleep(STEP_SLOW_AFTER * 4).await;
                    return AdvanceResult::StepCompleted {
                        proposal_id: "p1".to_string(),
                        stage: SettlementStage::DvpProposed,
                        dvp_proposal_cid: Some("cid-from-the-slow-step".to_string()),
                        dvp_cid: None,
                        allocation_cid: None,
                        pending_traffic: 0,
                    };
                }
                let error = format!("next step saw {:?}", state.dvp_proposal_cid);
                AdvanceResult::Error { proposal_id: "p1".to_string(), error }
            }
        };
        let state = SettlementState::new(test_proposal("p1"), false);
        let (result, state) = step_until_done("p1", state, &shutdown, step).await;
        assert_eq!(state.dvp_proposal_cid.as_deref(), Some("cid-from-the-slow-step"));
        let AdvanceResult::Error { error, .. } = result else { panic!("the second step's result") };
        assert_eq!(error, "next step saw Some(\"cid-from-the-slow-step\")");
        assert_eq!(calls, 2);
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
        let mut config = BaseConfig::test_minimal().unwrap();
        config.max_pending_per_counterparty = 2;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();

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
        // RPC fails in tests — empty URL — so it never lands in
        // rejected_proposals; either way it is NOT adopted.)
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

    // rfq_v2_only disposition classifier: only the agent's OWN allocation step
    // (or an in-flight/terminal settlement) protects a V1 settlement from the
    // active abort. Counterparty progress and party-agnostic Allocating/
    // Allocated stages must NOT protect it.
    #[test]
    fn test_v1_leave_alone_disposition() {
        use orderbook_proto::DvpStepStatus;
        fn step(status: i32) -> Option<DvpStepStatus> {
            Some(DvpStepStatus { status, ..Default::default() })
        }

        // Fresh proposal: cancel (both roles).
        let fresh = GetSettlementStatusResponse::default();
        assert!(!v1_settlement_leave_alone(&fresh, true));
        assert!(!v1_settlement_leave_alone(&fresh, false));

        // My allocation Submitted/Completed/Confirmed → leave alone; the same
        // step on the COUNTERPARTY's side must not protect.
        for s in [2, 3, 4] {
            let mut st = GetSettlementStatusResponse::default();
            st.allocation_buyer = step(s);
            assert!(v1_settlement_leave_alone(&st, true), "buyer alloc status {s}");
            assert!(!v1_settlement_leave_alone(&st, false), "counterparty alloc status {s}");
            let mut st = GetSettlementStatusResponse::default();
            st.allocation_seller = step(s);
            assert!(v1_settlement_leave_alone(&st, false), "seller alloc status {s}");
            assert!(!v1_settlement_leave_alone(&st, true), "counterparty alloc status {s}");
        }

        // Pending or failed-flavour allocation attempts left nothing standing.
        for s in [0, 1, 5, 6, 7, 8, 9] {
            let mut st = GetSettlementStatusResponse::default();
            st.allocation_buyer = step(s);
            assert!(!v1_settlement_leave_alone(&st, true), "alloc status {s}");
        }

        // Settlement tx in flight/done → leave alone regardless of role.
        for s in [2, 3, 4] {
            let mut st = GetSettlementStatusResponse::default();
            st.settlement = step(s);
            assert!(v1_settlement_leave_alone(&st, true), "settlement status {s}");
            assert!(v1_settlement_leave_alone(&st, false), "settlement status {s}");
        }

        // Terminal-ish stages (Settling=10..Cancelled=13) → leave alone;
        // Allocating/Allocated (8/9) alone do not (party-agnostic).
        for stage in [10, 11, 12, 13] {
            let mut st = GetSettlementStatusResponse::default();
            st.stage = stage;
            assert!(v1_settlement_leave_alone(&st, true), "stage {stage}");
        }
        for stage in [8, 9] {
            let mut st = GetSettlementStatusResponse::default();
            st.stage = stage;
            assert!(!v1_settlement_leave_alone(&st, true), "stage {stage}");
        }
    }

    // rfq_v2_only: a V1 proposal is never adopted — even with a matching
    // quoted trade that would adopt it with the switch off (see
    // test_adoption_defers_reservation_until_counterparty_commits). In-harness
    // the abort's GetSettlementStatus RPC fails (empty URL), so the pid must
    // stay OUT of rejected_proposals — retry-able on the next poll — and a
    // replayed delivery is equally inert.
    #[tokio::test]
    async fn test_rfq_v2_only_never_adopts_and_stays_retryable() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.rfq_v2_only = true;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
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
        exec.handle_settlement_update(created_update(p1.clone())).await.unwrap();

        assert!(!exec.active_settlements.contains_key("p1"));
        assert!(!exec.tracker.lock().await.has_settlement_order("p1"));
        assert!(!exec.rejected_proposals.contains("p1"), "RPC failed → must stay retry-able");

        // Replay (poll re-synthesizes ProposalCreated) — equally inert.
        exec.handle_settlement_update(created_update(p1)).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p1"));
    }

    // rfq_v2_only precedes BOTH adoption bypasses: the restored-tracker path
    // and --no-reject would otherwise adopt without verification.
    #[tokio::test]
    async fn test_rfq_v2_only_branch_precedes_restore_and_no_reject() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.rfq_v2_only = true;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();

        // Case A: restored settlement order (state restore) — would re-adopt.
        {
            let mut t = exec.tracker.lock().await;
            t.record_settlement_order("p1", 0, Decimal::from(1000));
        }
        let mut p1 = test_proposal("p1");
        p1.seller = "test-party".to_string();
        p1.buyer = "cp-x".to_string();
        exec.handle_settlement_update(created_update(p1)).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p1"));
        // Tracker entry preserved: the abort's RPC failed before any
        // disposition, so no mark_failed ran.
        assert!(exec.tracker.lock().await.has_settlement_order("p1"));

        // Case B: --no-reject — would adopt everything.
        exec.set_no_reject(true);
        let mut p2 = test_proposal("p2");
        p2.seller = "test-party".to_string();
        p2.buyer = "cp-x".to_string();
        exec.handle_settlement_update(created_update(p2)).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p2"));
        assert!(!exec.tracker.lock().await.has_settlement_order("p2"));
    }

    // Deferred reservation: adoption records the local decision but commits
    // nothing; ensure_reserved (counterparty committed) reserves exactly once
    // and is safe to re-enter even under tight inventory.
    #[tokio::test]
    async fn test_adoption_defers_reservation_until_counterparty_commits() {
        let config = BaseConfig::test_minimal().unwrap();
        let lm = ready_lm().await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();
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
        let config = BaseConfig::test_minimal().unwrap();
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(10)).await;
        lm.update_token_balance("USDCx", Decimal::from(1000)).await; // exactly the leg
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
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
        let config = BaseConfig::test_minimal().unwrap();
        let lm = ready_lm().await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();
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
        let config = BaseConfig::test_minimal().unwrap();
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(50)).await;
        lm.update_token_balance("USDCx", Decimal::from(500)).await; // < the 1000 leg
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [7u8; 32]).unwrap())));
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
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();

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

    fn status_update(pid: &str) -> SettlementUpdate {
        SettlementUpdate {
            event_type: EventType::StatusChanged as i32,
            proposal: Some(test_proposal(pid)),
            ..Default::default()
        }
    }

    fn executor_with_active(threads: usize, pids: &[&str]) -> SettlementExecutor<MockBackend> {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.settlement_thread_count = threads;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
        for pid in pids {
            exec.active_settlements
                .insert(pid.to_string(), SettlementState::new(test_proposal(pid), false));
        }
        exec
    }

    const LONG: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn test_stream_batch_advances_each_proposal_once() {
        let mut exec = executor_with_active(8, &["p1", "p2"]);
        let mut backlog: VecDeque<SettlementUpdate> =
            ["p1", "p1", "p2", "p1"].into_iter().map(status_update).collect();

        let outcome = exec.apply_stream_batch(&mut backlog, LONG, LONG).await;

        assert!(backlog.is_empty());
        assert_eq!(
            outcome,
            StreamBatchOutcome { handled: 4, touched: 2, spawned: 2, affects_grid: false }
        );
        // One task per proposal, each started after the whole batch was handled
        let spawns = |id: &str| exec.pending_results.iter().filter(|(pid, _)| pid == id).count();
        assert_eq!((spawns("p1"), spawns("p2")), (1, 1));
        assert_eq!(exec.pending_results.len(), 2);
        assert!(exec.rearm_on_result.is_empty(), "no update was handled while its task ran");
        assert!(exec.in_progress.contains_key("p1"));
        assert!(exec.in_progress.contains_key("p2"));
    }

    // A stuck update is cut off at `per_update`; the rest of the batch still runs
    #[tokio::test]
    async fn test_stream_batch_times_out_a_stuck_update() {
        let mut exec = executor_with_active(8, &["p1", "p2"]);
        let cancelled = SettlementUpdate {
            event_type: EventType::Cancelled as i32,
            proposal: Some(test_proposal("p1")),
            ..Default::default()
        };
        let mut backlog = VecDeque::from([cancelled, status_update("p2")]);

        // A terminal event needs the tracker lock, held here for the whole batch
        let tracker = Arc::clone(&exec.tracker);
        let held = tracker.lock().await;
        let batch = exec.apply_stream_batch(&mut backlog, Duration::from_millis(100), LONG);
        let outcome = tokio::time::timeout(Duration::from_secs(10), batch)
            .await
            .expect("each update is bounded");
        drop(held);

        assert!(backlog.is_empty());
        assert_eq!(
            outcome,
            StreamBatchOutcome { handled: 2, touched: 1, spawned: 1, affects_grid: true }
        );
        // The cut-off handler removed nothing and its proposal was not advanced
        assert!(exec.active_settlements.contains_key("p1"));
        assert!(!exec.rejected_proposals.contains("p1"));
        assert!(!exec.in_progress.contains_key("p1"));
        assert!(exec.in_progress.contains_key("p2"));
    }

    #[tokio::test]
    async fn test_stream_batch_respects_budget() {
        let mut exec = executor_with_active(8, &["p1", "p2", "p3"]);
        let mut backlog: VecDeque<SettlementUpdate> =
            ["p1", "p2", "p3"].into_iter().map(status_update).collect();

        // An exhausted budget still handles one update; the rest stay queued in order
        let outcome = exec.apply_stream_batch(&mut backlog, LONG, Duration::ZERO).await;
        assert_eq!(outcome.handled, 1);
        assert_eq!(outcome.spawned, 1);
        let queued: Vec<String> = backlog
            .iter()
            .filter_map(|u| u.proposal.as_ref().map(|p| p.proposal_id.clone()))
            .collect();
        assert_eq!(queued, ["p2", "p3"]);

        let outcome = exec.apply_stream_batch(&mut backlog, LONG, LONG).await;
        assert_eq!(outcome.handled, 2);
        assert!(backlog.is_empty());
    }

    #[tokio::test]
    async fn test_stream_batch_defers_to_needs_readvance_without_permit() {
        let mut exec = executor_with_active(1, &["p1", "p2"]);
        // p2 sits in a long backoff; the stream event must clear it
        exec.failed_settlements.insert("p2".to_string(), FailedSettlement {
            retry_count: 0, wait_count: 3, next_retry: Instant::now() + Duration::from_secs(300),
            first_transient_at: None, cid_waiting: None,
        });
        let mut backlog: VecDeque<SettlementUpdate> =
            ["p1", "p2"].into_iter().map(status_update).collect();

        let outcome = exec.apply_stream_batch(&mut backlog, LONG, LONG).await;

        assert_eq!(outcome.touched, 2);
        assert_eq!(outcome.spawned, 1);
        assert!(exec.in_progress.contains_key("p1"));
        assert!(!exec.needs_readvance.contains("p1"));
        // No free permit: deferred, not dropped, and its backoff is cleared
        assert!(!exec.in_progress.contains_key("p2"));
        assert!(exec.needs_readvance.contains("p2"));
        assert!(!exec.failed_settlements.contains_key("p2"));
    }

    #[tokio::test]
    async fn test_affects_grid_by_event() {
        use orderbook_proto::orderbook::OrderMatch;
        let event = |event_type: EventType, proposal: SettlementProposal| SettlementUpdate {
            event_type: event_type as i32,
            proposal: Some(proposal),
            ..Default::default()
        };
        let mut matched = test_proposal("p1");
        matched.order_match = Some(OrderMatch::default());

        for terminal in [EventType::Settled, EventType::Failed, EventType::Cancelled] {
            assert!(affects_grid(&event(terminal, test_proposal("p1"))), "{terminal:?}");
        }
        assert!(affects_grid(&event(EventType::ProposalCreated, matched)));
        // RFQ proposals carry no order match and never touch the grid
        assert!(!affects_grid(&event(EventType::ProposalCreated, test_proposal("p1"))));
        assert!(!affects_grid(&event(EventType::StatusChanged, test_proposal("p1"))));
        assert!(!affects_grid(&event(EventType::Unspecified, test_proposal("p1"))));

        // The batch outcome carries the flag for a terminal event
        let mut exec = executor_with_active(8, &["p1", "p2"]);
        let mut backlog = VecDeque::from([
            status_update("p2"),
            event(EventType::Cancelled, test_proposal("p1")),
        ]);
        let outcome = exec.apply_stream_batch(&mut backlog, LONG, LONG).await;
        assert!(outcome.affects_grid);
        assert_eq!(outcome.touched, 2);
        assert_eq!(outcome.spawned, 1); // p1 is terminal and no longer active
        assert!(!exec.active_settlements.contains_key("p1"));
    }

    #[tokio::test]
    async fn test_placement_in_flight_proposal_held_then_adopted() {
        use orderbook_proto::orderbook::{OrderMatch, OrderType};
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [3u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();

        let mut proposal = test_proposal("p-grid");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.seller = "test-party".to_string();
        proposal.buyer = "cp-x".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-grid".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });

        // Order 42 matched before the placing task tracked it
        let placement = tracker.lock().await.begin_placement("USDCx-CCY");
        exec.handle_settlement_update(created_update(proposal.clone())).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p-grid"));
        assert!(!exec.rejected_proposals.contains("p-grid"));
        assert!(!exec.tracker.lock().await.has_settlement_order("p-grid"));
        assert_eq!(exec.server_lookups, 0, "held without a server lookup");
        assert_eq!(exec.held_count(), 0, "no poll backoff while placement is in flight");

        // The guard covers its own market only; elsewhere the order is looked up
        let mut elsewhere = proposal.clone();
        elsewhere.proposal_id = "p-other".to_string();
        elsewhere.market_id = "OTHER-CCY".to_string();
        exec.handle_settlement_update(created_update(elsewhere)).await.unwrap();
        assert_eq!(exec.server_lookups, 1);

        // Placement completes: tracked, then the guard is released under the same lock
        {
            let mut t = tracker.lock().await;
            let (signature, signed_data, nonce) =
                t.sign_order("USDCx-CCY", "offer", "0.5", "1000").unwrap();
            t.track_order(42, "USDCx-CCY", OrderType::Offer as i32, "0.5", "1000", nonce, &signature, &signed_data);
            drop(placement);
        }

        // The next poll re-feeds the held proposal, which now adopts
        exec.handle_settlement_update(created_update(proposal)).await.unwrap();
        assert!(exec.active_settlements.contains_key("p-grid"));
        assert!(exec.tracker.lock().await.has_settlement_order("p-grid"));
        assert!(!exec.rejected_proposals.contains("p-grid"));
        assert_eq!(exec.server_lookups, 1, "adopted from the tracker, not the server");
    }

    // A user-order reject cut off mid-flight leaves nothing a later advance could accept
    #[tokio::test]
    async fn test_dropped_user_order_reject_leaves_nothing_active() {
        use orderbook_proto::orderbook::OrderMatch;
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [3u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
        // The server answers without the order: a definitive verdict
        exec.stub_orders = Some(Vec::new());
        exec.reject_black_hole = true;

        let mut proposal = our_sale("p-user");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-user".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });
        let handled =
            tokio::time::timeout(Duration::from_millis(200), exec.handle_settlement_update(created_update(proposal)))
                .await;
        assert!(handled.is_err(), "the reject was still in flight when dropped");
        assert_eq!(exec.server_lookups, 1);
        assert!(!exec.active_settlements.contains_key("p-user"), "left active");
        assert!(!exec.rejected_proposals.contains("p-user"), "a later delivery retries it");
    }

    #[tokio::test]
    async fn test_collect_results_keeps_cid_found_during_task() {
        let (mut exec, _lm) = committed_executor().await;
        {
            // Sync found these while the task was in flight
            let state = exec.active_settlements.get_mut("p1").unwrap();
            state.dvp_cid = Some("dvp-sync".to_string());
            state.dvp_proposal_cid = Some("proposal-sync".to_string());
        }
        // The task ran from an older snapshot and found its own CIDs
        let mut task_state = SettlementState::new(test_proposal("p1"), false);
        task_state.dvp_proposal_cid = Some("proposal-task".to_string());
        task_state.allocation_cid = Some("alloc-task".to_string());

        let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();
        assert!(tx
            .send((AdvanceResult::Wait { proposal_id: "p1".to_string() }, task_state))
            .is_ok());
        exec.in_progress.insert("p1".to_string(), Instant::now());
        exec.pending_results.push(("p1".to_string(), rx));

        exec.collect_results().await;

        let state = exec.active_settlements.get("p1").unwrap();
        assert_eq!(state.dvp_cid.as_deref(), Some("dvp-sync"));
        assert_eq!(state.dvp_proposal_cid.as_deref(), Some("proposal-task"));
        assert_eq!(state.allocation_cid.as_deref(), Some("alloc-task"));
        assert!(!exec.in_progress.contains_key("p1"));
    }

    /// Marks `pid` as running with a result channel that has not been sent yet.
    fn park_in_flight(
        exec: &mut SettlementExecutor<MockBackend>,
        pid: &str,
    ) -> tokio::sync::oneshot::Sender<(AdvanceResult, SettlementState)> {
        let (tx, rx) = tokio::sync::oneshot::channel::<(AdvanceResult, SettlementState)>();
        exec.in_progress.insert(pid.to_string(), Instant::now());
        exec.pending_results.push((pid.to_string(), rx));
        tx
    }

    fn send_result(tx: tokio::sync::oneshot::Sender<(AdvanceResult, SettlementState)>, result: AdvanceResult) {
        let pid = result.proposal_id().to_string();
        assert!(tx.send((result, SettlementState::new(test_proposal(&pid), false))).is_ok());
    }

    #[tokio::test]
    async fn test_stream_touch_during_task_rearms_after_wait() {
        let mut exec = executor_with_active(8, &["p1"]);
        let tx = park_in_flight(&mut exec, "p1");

        let mut backlog = VecDeque::from([status_update("p1")]);
        let outcome = exec.apply_stream_batch(&mut backlog, LONG, LONG).await;
        assert_eq!(outcome.touched, 1);
        assert_eq!(outcome.spawned, 0);
        assert!(exec.rearm_on_result.contains("p1"));

        // The task finishes with a Wait from older state; the touch undoes its cooldown
        send_result(tx, AdvanceResult::Wait { proposal_id: "p1".to_string() });
        let ids = exec.collect_results().await;
        assert!(ids.iter().any(|id| id == "p1"));
        assert!(!exec.failed_settlements.contains_key("p1"));
        assert!(exec.rearm_on_result.is_empty());

        // Control: an untouched Wait keeps its cooldown
        let tx = park_in_flight(&mut exec, "p1");
        send_result(tx, AdvanceResult::Wait { proposal_id: "p1".to_string() });
        let ids = exec.collect_results().await;
        assert!(!ids.iter().any(|id| id == "p1"));
        assert!(exec.failed_settlements.contains_key("p1"));
    }

    #[tokio::test]
    async fn test_stream_touch_during_task_keeps_error_backoff() {
        let mut exec = executor_with_active(8, &["p1"]);
        let tx = park_in_flight(&mut exec, "p1");

        let touched: IndexSet<String> = ["p1".to_string()].into_iter().collect();
        assert_eq!(exec.advance_proposals(touched).await, 0);
        assert!(exec.rearm_on_result.contains("p1"));

        let error = AdvanceResult::Error { proposal_id: "p1".to_string(), error: "boom".to_string() };
        send_result(tx, error);
        let ids = exec.collect_results().await;
        assert!(!ids.iter().any(|id| id == "p1"));
        assert!(exec.failed_settlements.get("p1").is_some_and(|f| f.retry_count == 1));
        assert!(exec.rearm_on_result.is_empty());
    }

    #[tokio::test]
    async fn test_stream_terminal_clears_rearm() {
        let mut exec = executor_with_active(8, &["p1"]);
        let _tx = park_in_flight(&mut exec, "p1");
        exec.rearm_on_result.insert("p1".to_string());

        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Cancelled as i32,
            proposal: Some(test_proposal("p1")),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(exec.rearm_on_result.is_empty());
    }

    #[tokio::test]
    async fn test_advance_all_prioritises_collected_readvance() {
        // A UUIDv7-shaped id sorts newest in the tail; "p1" (ms 0) sorts oldest
        let newer = "ffffffff-ffff-7fff-8fff-ffffffffffff";
        let mut exec = executor_with_active(1, &["p1", newer]);
        let tx = park_in_flight(&mut exec, "p1");
        let step = AdvanceResult::Preconfirmed { proposal_id: "p1".to_string() };
        assert!(step.should_readvance());
        send_result(tx, step);

        exec.advance_all_settlements().await;

        // The collected readvance id takes the only permit ahead of the newer tail entry
        assert!(exec.in_progress.contains_key("p1"));
        assert!(!exec.in_progress.contains_key(newer));
    }

    #[tokio::test]
    async fn test_advance_proposals_defers_past_spawn_cap() {
        let ids: Vec<String> =
            (0..MAX_ADVANCE_SPAWNS_PER_CYCLE + 2).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut exec = executor_with_active(64, &refs);

        let spawned = exec.advance_proposals(ids.iter().cloned().collect()).await;

        assert_eq!(spawned, MAX_ADVANCE_SPAWNS_PER_CYCLE);
        for id in ids.iter().skip(MAX_ADVANCE_SPAWNS_PER_CYCLE) {
            assert!(exec.needs_readvance.contains(id));
            assert!(!exec.in_progress.contains_key(id));
        }
    }

    #[tokio::test]
    async fn test_age_saturates_on_extreme_created_at() {
        assert_eq!(age_secs(0, i64::MIN), i64::MAX);
        assert_eq!(age_secs(i64::MIN, i64::MAX), i64::MIN);

        let (mut exec, _lm) = committed_executor().await;
        exec.active_settlements.get_mut("p1").unwrap().proposal.created_at =
            Some(prost_types::Timestamp { seconds: i64::MIN, nanos: 0 });
        assert!(exec.past_deadline("p1"));
        exec.active_settlements.get_mut("p1").unwrap().proposal.created_at =
            Some(prost_types::Timestamp { seconds: i64::MAX, nanos: 0 });
        assert!(!exec.past_deadline("p1"));

        assert!(deadline_expiry_error(i64::MIN, 0, NextAction::Wait, 900, 1800).is_some());
        assert!(deadline_expiry_error(i64::MAX, i64::MIN, NextAction::Allocate, 900, 1800).is_none());

        // A restored proposal with such a timestamp is abandoned, not re-adopted
        exec.tracker.lock().await.record_settlement_order("p9", 0, Decimal::from(1000));
        let mut p9 = test_proposal("p9");
        p9.seller = "test-party".to_string();
        p9.created_at = Some(prost_types::Timestamp { seconds: i64::MIN, nanos: 0 });
        exec.handle_settlement_update(created_update(p9)).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p9"));
        assert!(exec.rejected_proposals.contains("p9"));
    }

    #[tokio::test]
    async fn test_collect_results_retries_cid_error_once_sync_found_it() {
        let (mut exec, _lm) = committed_executor().await;
        // Sync found the Dvp while the task ran from a snapshot without it
        exec.active_settlements.get_mut("p1").unwrap().dvp_cid = Some("dvp-sync".to_string());
        let tx = park_in_flight(&mut exec, "p1");
        let error = "No Dvp contract ID found (not yet accepted?)".to_string();
        send_result(tx, AdvanceResult::Error { proposal_id: "p1".to_string(), error });

        let ids = exec.collect_results().await;

        assert!(ids.iter().any(|id| id == "p1"), "re-advanced at once");
        assert!(!exec.failed_settlements.contains_key("p1"), "no CID backoff left");
        assert_eq!(exec.active_settlements.get("p1").unwrap().dvp_cid.as_deref(), Some("dvp-sync"));

        // Control: an error for a CID still unknown keeps its backoff
        let tx = park_in_flight(&mut exec, "p1");
        let error = "No DvpProposal CID found (not yet proposed?)".to_string();
        send_result(tx, AdvanceResult::Error { proposal_id: "p1".to_string(), error });
        let ids = exec.collect_results().await;
        assert!(!ids.iter().any(|id| id == "p1"));
        assert!(exec.failed_settlements.get("p1").is_some_and(|f| f.cid_waiting.is_some()));
    }

    fn our_sale(id: &str) -> SettlementProposal {
        let mut p = test_proposal(id);
        p.seller = "test-party".to_string();
        p.buyer = "cp-x".to_string();
        p
    }

    fn hold_delay(exec: &SettlementExecutor<MockBackend>, id: &str) -> Option<Duration> {
        exec.held_until.get(id).map(|(_, delay)| *delay)
    }

    fn expire_hold(exec: &mut SettlementExecutor<MockBackend>, id: &str) {
        let entry = exec.held_until.get_mut(id).unwrap();
        entry.0 = Instant::now();
    }

    #[tokio::test]
    async fn test_held_proposal_backs_off_poll_refeeds() {
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
        // No balances yet: every proposal is held
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        exec.set_liquidity_manager(lm.clone());

        exec.feed_polled_proposals(vec![our_sale("p1")]).await;
        assert_eq!(hold_delay(&exec, "p1"), Some(HOLD_INITIAL));
        assert_eq!(exec.held_count(), 1);

        // Still held: the poll skips it, so the hold is not re-armed
        exec.feed_polled_proposals(vec![our_sale("p1")]).await;
        assert_eq!(hold_delay(&exec, "p1"), Some(HOLD_INITIAL));

        // Each expired hold is re-verified and doubles, up to the cap
        for secs in [60, 120, 240, 300, 300] {
            expire_hold(&mut exec, "p1");
            exec.feed_polled_proposals(vec![our_sale("p1")]).await;
            assert_eq!(hold_delay(&exec, "p1"), Some(Duration::from_secs(secs)));
        }

        // The server no longer returns it: the hold is dropped
        exec.feed_polled_proposals(Vec::new()).await;
        assert_eq!(exec.held_count(), 0);

        // A terminal stream event ends a hold
        exec.feed_polled_proposals(vec![our_sale("p2")]).await;
        assert_eq!(exec.held_count(), 1);
        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Cancelled as i32,
            proposal: Some(our_sale("p2")),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(exec.held_count(), 0);
        exec.feed_polled_proposals(vec![our_sale("p4")]).await;
        assert_eq!(exec.held_count(), 1);
        exec.handle_settlement_update(SettlementUpdate {
            event_type: EventType::Settled as i32,
            proposal: Some(our_sale("p4")),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(exec.held_count(), 0);

        // A reject ends a hold
        exec.feed_polled_proposals(vec![our_sale("p3")]).await;
        assert_eq!(exec.held_count(), 1);
        exec.config.max_pending_per_counterparty = 0;
        expire_hold(&mut exec, "p3");
        exec.feed_polled_proposals(vec![our_sale("p3")]).await;
        assert_eq!(exec.held_count(), 0);
        exec.config.max_pending_per_counterparty = 1000;

        // Balances load: the next re-feed adopts and ends the hold
        exec.feed_polled_proposals(vec![our_sale("p1")]).await;
        assert_eq!(hold_delay(&exec, "p1"), Some(HOLD_INITIAL));
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        exec.set_quoted_rfq_trades(Arc::new(Mutex::new(vec![QuotedTrade {
            market_id: String::new(),
            price: String::new(),
            base_quantity: "1000".to_string(),
            quote_quantity: "500".to_string(),
        }])));

        // Loaded but stale balances hold it as well
        lm.set_stale_after(Duration::ZERO);
        expire_hold(&mut exec, "p1");
        exec.feed_polled_proposals(vec![our_sale("p1")]).await;
        assert_eq!(hold_delay(&exec, "p1"), Some(HOLD_INITIAL * 2));
        lm.set_stale_after(Duration::from_secs(120));

        expire_hold(&mut exec, "p1");
        exec.feed_polled_proposals(vec![our_sale("p1")]).await;
        assert!(exec.active_settlements.contains_key("p1"));
        assert_eq!(exec.held_count(), 0);
    }

    #[tokio::test]
    async fn test_failed_user_order_lookup_is_held_with_backoff() {
        use orderbook_proto::orderbook::OrderMatch;
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
        let mut proposal = our_sale("p-user");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-user".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });

        // Order 42 is not tracked and the server lookup cannot run: held, not rejected
        exec.feed_polled_proposals(vec![proposal.clone()]).await;
        assert!(!exec.active_settlements.contains_key("p-user"));
        assert!(!exec.rejected_proposals.contains("p-user"));
        assert_eq!(hold_delay(&exec, "p-user"), Some(HOLD_INITIAL));

        exec.feed_polled_proposals(vec![proposal.clone()]).await;
        assert_eq!(hold_delay(&exec, "p-user"), Some(HOLD_INITIAL), "skipped while held");
        expire_hold(&mut exec, "p-user");
        exec.feed_polled_proposals(vec![proposal]).await;
        assert_eq!(hold_delay(&exec, "p-user"), Some(HOLD_INITIAL * 2));
    }

    // A handler dropped during the lookup (a caller's timeout) keeps the proposal held
    #[tokio::test]
    async fn test_hold_survives_a_lookup_cut_short_by_a_timeout() {
        use orderbook_proto::orderbook::OrderMatch;
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, MockBackend).unwrap();
        let mut proposal = our_sale("p-user");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-user".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });

        exec.feed_polled_proposals(vec![proposal.clone()]).await;
        assert_eq!(hold_delay(&exec, "p-user"), Some(HOLD_INITIAL));

        expire_hold(&mut exec, "p-user");
        exec.stub_lookup_hangs = true;
        let fed = tokio::time::timeout(Duration::from_millis(50), exec.feed_polled_proposals(vec![proposal])).await;
        assert!(fed.is_err(), "the lookup should still be pending");
        assert_eq!(hold_delay(&exec, "p-user"), Some(HOLD_INITIAL * 2));
        assert!(!exec.active_settlements.contains_key("p-user"));
        assert!(!exec.rejected_proposals.contains("p-user"));
    }

    // A submit that errored may still book the order; its match is adopted, not rejected
    #[tokio::test]
    async fn test_order_booked_by_a_failed_submit_is_adopted() {
        use orderbook_proto::orderbook::{Order, OrderMatch, OrderStatus, OrderType};
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [3u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();

        // The submit fails at the transport, so the order id never comes back
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut grid_config = BaseConfig::test_minimal().unwrap();
        grid_config.orderbook_grpc_url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let client = OrderbookClient::lazy_for_tests(&grid_config).unwrap();
        let mut om = crate::order_manager::OrderManager::new(grid_config, client, tracker.clone());
        assert!(om.place_offer("USDCx-CCY", "0.5", "1000", None).await.is_err());
        let submit = tracker.lock().await.failed_submits().pop().unwrap();

        // Yet the server booked it as order 42, with all of it matched and pending
        exec.stub_orders = Some(vec![Order {
            order_id: 42,
            market_id: "USDCx-CCY".to_string(),
            order_type: OrderType::Offer as i32,
            price: "0.5".to_string(),
            quantity: "1000".to_string(),
            filled_quantity: "0".to_string(),
            pending_quantity: "1000".to_string(),
            status: OrderStatus::Partial as i32,
            nonce: submit.nonce,
            signature: Some(submit.signature.clone()),
            signed_data: submit.signed_data.clone(),
            ..Default::default()
        }]);
        let mut proposal = our_sale("p-booked");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-booked".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });

        // Held while the failure is fresh
        exec.handle_settlement_update(created_update(proposal.clone())).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p-booked"));
        assert_eq!(exec.server_lookups, 0);

        // Past the hold the lookup finds our own order and adopts it once
        tracker.lock().await.expire_submit_holds();
        exec.handle_settlement_update(created_update(proposal)).await.unwrap();
        assert_eq!(exec.server_lookups, 1);
        assert!(exec.active_settlements.contains_key("p-booked"));
        assert_eq!(exec.held_count(), 0, "a lookup adoption ends the hold");
        assert!(tracker.lock().await.has_settlement_order("p-booked"));
        assert!(tracker.lock().await.failed_submits().is_empty());
    }

    // The grid lists a booked failed submit before it can cancel it; its match is accepted after the hold
    #[tokio::test]
    async fn test_failed_submit_listed_by_the_grid_is_adopted_before_its_cancel() {
        use crate::config::{MarketConfig, PriceLevel};
        use orderbook_proto::ledger::TokenBalance;
        use orderbook_proto::orderbook::{Order, OrderMatch, OrderStatus, OrderType};
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [3u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut grid_config = BaseConfig::test_minimal().unwrap();
        grid_config.orderbook_grpc_url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let level = |delta_percent: f64| PriceLevel { delta_percent, quantity: "1000".to_string() };
        grid_config.markets = vec![MarketConfig {
            market_id: "USDCx-CCY".to_string(),
            enabled: true,
            base_order_size: None,
            bid_levels: vec![level(-1.0), level(-2.0)],
            offer_levels: vec![level(1.0), level(2.0)],
            price_change_threshold_percent: 1.0,
            rfq: None,
        }];
        let client = OrderbookClient::lazy_for_tests(&grid_config).unwrap();
        let mut om = crate::order_manager::OrderManager::new(grid_config, client, tracker.clone());
        assert!(om.place_offer("USDCx-CCY", "0.5", "1000", None).await.is_err());
        let submit = tracker.lock().await.failed_submits().pop().unwrap();
        let booked = Order {
            order_id: 42,
            market_id: "USDCx-CCY".to_string(),
            order_type: OrderType::Offer as i32,
            price: "0.5".to_string(),
            quantity: "1000".to_string(),
            filled_quantity: "0".to_string(),
            pending_quantity: "1000".to_string(),
            status: OrderStatus::Partial as i32,
            nonce: submit.nonce,
            signature: Some(submit.signature.clone()),
            signed_data: submit.signed_data.clone(),
            ..Default::default()
        };
        let mut proposal = our_sale("p-booked");
        proposal.market_id = "USDCx-CCY".to_string();
        proposal.order_match = Some(OrderMatch {
            settlement_proposal_id: "p-booked".to_string(),
            bid_order_id: 7,
            offer_order_id: 42,
            matched_quantity: "1000".to_string(),
            matched_price: "0.5".to_string(),
            created_at: None,
        });
        exec.handle_settlement_update(created_update(proposal.clone())).await.unwrap();
        assert!(!exec.active_settlements.contains_key("p-booked"));

        // A grid visit lists order 42 and re-places the side, cancelling it
        let balance = |id: &str, amount: &str, cc: bool| TokenBalance {
            instrument_id: id.to_string(),
            unlocked_amount: amount.to_string(),
            is_canton_coin: cc,
            ..Default::default()
        };
        om.set_balances(vec![balance("Amulet", "100", true), balance("USDCx", "10000", false), balance("CCY", "10000", false)]);
        om.stub_book(0.5, vec![booked]);
        let stop = Shutdown::new();
        let report = om.update_cycle(&stop, Instant::now() + Duration::from_secs(30)).await.unwrap();
        assert_eq!(report.refreshed, 1);
        assert!(matches!(
            tracker.lock().await.verify_settlement(&proposal, &config.party_id),
            VerifyResult::Accepted { order_id: 42 }
        ));

        // Cancelled, the order is gone from the live listing; the tracked order is still accepted
        exec.stub_orders = Some(Vec::new());
        tracker.lock().await.expire_submit_holds();
        exec.handle_settlement_update(created_update(proposal)).await.unwrap();
        assert_eq!(exec.server_lookups, 0);
        assert!(exec.active_settlements.contains_key("p-booked"));
        assert!(!exec.rejected_proposals.contains("p-booked"));
    }

    // Amounts that would overflow the fee or capacity arithmetic are refused, never adopted
    #[tokio::test]
    async fn test_implausible_amounts_are_rejected_without_panic() {
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.15").unwrap()).await;
        exec.set_liquidity_manager(lm);
        let rejected = Arc::new(Mutex::new(HashSet::new()));
        exec.set_rejected_rfq_trades(rejected.clone());

        let mut fees = our_sale("p-fees");
        fees.dvp_processing_fee_seller = Decimal::MAX.to_string();
        fees.allocation_processing_fee_seller = Decimal::MAX.to_string();
        assert_eq!(reservation_inputs(&fees, false, &None).2, Decimal::MAX);

        // A negative CC sale that a quoted trade would otherwise adopt
        let mut negative = our_sale("p-negative");
        negative.base_instrument = liquidity::CC_TOKEN.to_string();
        negative.base_quantity = "-5".to_string();
        exec.set_quoted_rfq_trades(Arc::new(Mutex::new(vec![QuotedTrade {
            market_id: String::new(),
            price: String::new(),
            base_quantity: "-5".to_string(),
            quote_quantity: "500".to_string(),
        }])));

        for proposal in [fees, negative] {
            let id = proposal.proposal_id.clone();
            exec.handle_settlement_update(created_update(proposal)).await.unwrap();
            assert!(!exec.active_settlements.contains_key(&id), "{id}");
            assert!(!tracker.lock().await.has_settlement_order(&id), "{id}");
            assert!(rejected.lock().await.contains(&id), "{id}");
        }
    }

    // A restored proposal refused as implausible gives back what it reserved
    #[tokio::test]
    async fn test_implausible_restored_proposal_releases_its_reservation() {
        use crate::state::{SavedSettlementOrder, SavedTrackedOrder};
        use orderbook_proto::orderbook::OrderType;
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker.clone(), MockBackend).unwrap();
        let lm = ready_lm().await;
        exec.set_liquidity_manager(lm.clone());
        tracker.lock().await.import_state(
            vec![SavedTrackedOrder {
                order_id: 42,
                market_id: "USDCx-CCY".to_string(),
                order_type: OrderType::Offer as i32,
                price: "0.5".to_string(),
                quantity: "5000".to_string(),
                settled_quantity: "0".to_string(),
                pending_quantity: "1500".to_string(),
                nonce: 1,
                signature: String::new(),
                signed_data: String::new(),
                placed_by: "test-party".to_string(),
                is_active: true,
            }],
            vec![SavedSettlementOrder {
                proposal_id: "p-restored".to_string(),
                order_id: 42,
                quantity: "1000".to_string(),
                reserved: true,
            }],
        );
        lm.try_commit("p-restored", "USDCx", Decimal::from(1000), Decimal::ZERO).await.unwrap();

        let mut proposal = our_sale("p-restored");
        proposal.dvp_processing_fee_seller = "-1".to_string();
        exec.feed_polled_proposals(vec![proposal]).await;
        drain_spawned().await;

        assert!(!exec.active_settlements.contains_key("p-restored"));
        let (_, orders, settlement_orders) = tracker.lock().await.export_state();
        assert!(settlement_orders.is_empty());
        assert_eq!(orders[0].pending_quantity, "500");
        assert!(!lm.has_commitment("p-restored").await);
    }

    // A fee too large for fixed-precision formatting is logged capped
    #[tokio::test]
    async fn test_reserving_a_huge_fee_logs_without_panicking() {
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::INFO);
        let config = BaseConfig::test_minimal().unwrap();
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::MAX).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.0000000001").unwrap()).await;
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        tracker.lock().await.record_settlement_order("p1", 0, Decimal::from(1000));
        let mut proposal = test_proposal("p1");
        proposal.dvp_processing_fee_seller = "100000000000000000".to_string();
        let state = SettlementState::new(proposal, false);
        assert!(lm.estimate_fee_cc(Decimal::from(100_000_000_000_000_000u64)).await >= Decimal::from_scientific("1e27").unwrap());

        ensure_reserved(&state, &config, &Some(lm.clone()), &tracker, "p1").await.unwrap();
        assert!(lm.has_commitment("p1").await);
        assert_eq!(logs.count("18446744073709551615.0000 CC fees (counterparty committed)"), 1);
    }

    /// Counts on-chain syncs; every other method is unused.
    struct SyncCounter(Arc<AtomicUsize>);

    #[async_trait]
    impl SettlementBackend for SyncCounter {
        async fn pay_fee(&self, _: &str, _: &str) -> Result<StepResult> {
            Err(anyhow::anyhow!("unused"))
        }
        async fn propose_dvp(&self, _: &str) -> Result<StepResult> {
            Err(anyhow::anyhow!("unused"))
        }
        async fn accept_dvp(
            &self, _: &str, _: &str, _: &str, _: &str, _: &str, _: &str,
        ) -> Result<StepResult> {
            Err(anyhow::anyhow!("unused"))
        }
        async fn allocate(&self, _: &str, _: &str, _: Option<Decimal>) -> Result<StepResult> {
            Err(anyhow::anyhow!("unused"))
        }
        async fn sync_contracts(&self, _: &[String]) -> Result<Vec<DiscoveredContract>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn queue_depth(&self) -> (u64, u64) {
            (0, 0)
        }
    }

    fn cid_wait(cid: CidWaitingType) -> FailedSettlement {
        FailedSettlement {
            retry_count: 0,
            wait_count: 0,
            next_retry: Instant::now() + Duration::from_secs(10),
            first_transient_at: Some(Instant::now()),
            cid_waiting: Some(cid),
        }
    }

    #[tokio::test]
    async fn test_sync_runs_only_while_a_cid_is_missing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let config = BaseConfig::test_minimal().unwrap();
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        let mut exec = SettlementExecutor::new(&config, tracker, SyncCounter(Arc::clone(&calls))).unwrap();
        let syncs = || calls.load(Ordering::SeqCst);

        // Allocated, or holding both CIDs: nothing left to discover
        let mut allocated = SettlementState::new(test_proposal("p-alloc"), false);
        allocated.stage = SettlementStage::Allocated;
        exec.active_settlements.insert("p-alloc".to_string(), allocated);
        let mut both = SettlementState::new(test_proposal("p-both"), true);
        both.stage = SettlementStage::AllocationFeePaid;
        both.dvp_proposal_cid = Some("proposal".to_string());
        both.dvp_cid = Some("dvp".to_string());
        exec.active_settlements.insert("p-both".to_string(), both);
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 0);

        // Restored past the accept: the Dvp is known and its proposal is gone
        let mut restored = SettlementState::new(test_proposal("p-restored"), false);
        restored.dvp_cid = Some("dvp".to_string());
        exec.active_settlements.insert("p-restored".to_string(), restored);
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 0);

        // A pre-allocation settlement lacking a CID: at most once per interval
        exec.active_settlements
            .insert("p1".to_string(), SettlementState::new(test_proposal("p1"), false));
        exec.sync_on_chain_contracts().await;
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 1);
        exec.last_sync = Instant::now().checked_sub(SYNC_MIN_INTERVAL);
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 2);

        // A CID-waiting entry for a proposal no longer active changes nothing
        exec.failed_settlements.insert("ghost".to_string(), cid_wait(CidWaitingType::DvpContract));
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 2);

        // A proposal waiting for a CID syncs on every call
        exec.failed_settlements.insert("p1".to_string(), cid_wait(CidWaitingType::DvpProposal));
        exec.sync_on_chain_contracts().await;
        exec.sync_on_chain_contracts().await;
        assert_eq!(syncs(), 4);
    }

    fn plain_executor(config: &BaseConfig) -> Result<SettlementExecutor<MockBackend>> {
        let tracker = Arc::new(Mutex::new(OrderTracker::new(0, crate::secret::Secret::seal(&mut [0u8; 32]).unwrap())));
        SettlementExecutor::new(config, tracker, MockBackend)
    }

    // A thread count the semaphore cannot hold is an error, not a panic or a stalled executor
    #[test]
    fn executor_rejects_a_thread_count_outside_the_semaphore_range() {
        let mut config = BaseConfig::test_minimal().unwrap();
        for bad in [0, crate::sync::MAX_PERMITS + 1, usize::MAX] {
            config.settlement_thread_count = bad;
            assert!(plain_executor(&config).is_err(), "count {bad}");
        }
        config.settlement_thread_count = crate::sync::MAX_PERMITS;
        let exec = plain_executor(&config).unwrap();
        assert_eq!(exec.semaphore.available_permits(), crate::sync::MAX_PERMITS);
    }

    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    // Tasks still running when the drain bound passes are aborted, not left behind
    #[tokio::test]
    async fn drain_aborts_tasks_still_running_after_the_limit() {
        let mut exec = plain_executor(&BaseConfig::test_minimal().unwrap()).unwrap();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = SetOnDrop(dropped.clone());
        exec.task_handles.push(tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }));
        exec.task_handles.push(tokio::spawn(async {}));
        let started = Instant::now();
        assert_eq!(exec.drain_tasks_within(Duration::from_millis(100)).await, 2);
        assert!(started.elapsed() < Duration::from_secs(5));
        for _ in 0..100 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(dropped.load(Ordering::SeqCst), "the stuck task must be aborted");
        assert!(exec.task_handles.is_empty());
    }

    // Outside a runtime these spawns are skipped instead of panicking
    #[test]
    fn spawning_helpers_do_not_panic_without_a_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (mut exec, _lm) = rt.block_on(committed_executor());
        drop(rt);
        exec.release_commitment("p1");
        exec.record_settlement_inflow("p1");
        exec.notify_server_cancel("p1", "test");
        let permit = exec.semaphore.clone().try_acquire_owned().unwrap();
        assert!(!exec.spawn_settlement_task("p1".to_string(), permit, false));
        assert!(exec.in_progress.is_empty() && exec.pending_results.is_empty() && exec.task_handles.is_empty());
        assert_eq!(exec.semaphore.available_permits(), 1, "the permit is returned");
    }

    // A signing key that cannot be opened fails the JWT instead of panicking
    #[test]
    fn a_key_that_cannot_be_opened_fails_the_jwt() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.private_key = crate::secret::Secret::corrupt_for_tests();
        let exec = plain_executor(&config).unwrap();
        assert_eq!(exec.create_jwt().unwrap_err().to_string(), "sealed secret is corrupt");
        assert!(config_jwt(&BaseConfig::test_minimal().unwrap()).is_ok());
    }

    #[tokio::test]
    async fn backend_stats_are_awaited_and_default_to_none() {
        let exec = plain_executor(&BaseConfig::test_minimal().unwrap()).unwrap();
        assert!(exec.cache_stats().await.is_none());
        assert!(exec.holdings_histogram("CC").await.is_none());
    }
}
