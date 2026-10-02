//! Parallel payment queue with amulet reservation
//!
//! Replaces the old single-threaded processor with a scheduler + parallel workers.
//! The scheduler picks payments from a priority heap, selects smallest-fit amulets
//! from the AmuletCache, reserves them, and spawns workers that execute concurrently.
//!
//! Priority order: Allocate (High) > PayFee (Normal). Traffic billing is
//! off-chain (handled by the ledger via the prepaid traffic pool); the
//! cloud-agent does not pay traffic fees on-chain.
//! Within the same priority, operations are processed FIFO.
//!
//! Workers pass pre-selected amulet CIDs via the proto `amulet_cids` field.
//! On success, consumed amulets are marked in the cache and newly created amulets
//! (from change/split) are added. On INACTIVE_CONTRACTS, consumed amulets are
//! marked as such; retry happens at the settlement-event layer above this queue.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::confirm::{confirm_transaction, ConfirmLock};
use agent_logic::num::{dec_sum, Dp};
use agent_logic::settlement::StepResult;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AllocateParams,
    PrepareTransactionRequest, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use tonic::transport::Channel;

use crate::holdings_cache::{CachedAmulet, CcView, ReservationGuard};
use crate::ledger_client::DAppProviderClient;

/// Default max concurrent allocation workers (critical path — highest priority)
const DEFAULT_MAX_ALLOCATION_WORKERS: usize = 20;

/// Default max concurrent fee payment workers
const DEFAULT_MAX_FEE_WORKERS: usize = 5;

/// Largest accepted size of either payment worker pool.
pub const MAX_PAYMENT_WORKERS: usize = 256;

/// Payment worker pool sizes, validated at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentLimits {
    pub allocation_workers: usize,
    pub fee_workers: usize,
}

impl PaymentLimits {
    /// Read `MAX_PAYMENT_WORKERS` (sets both pools), `MAX_ALLOCATION_WORKERS` and
    /// `MAX_FEE_WORKERS`. Unset or blank means the default; other values must be 1..=256.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| match std::env::var(name) {
            Ok(raw) => Ok(Some(raw)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(e) => Err(anyhow!("{name}: {e}")),
        })
    }

    pub(crate) fn from_lookup(get: impl Fn(&str) -> Result<Option<String>>) -> Result<Self> {
        let read = |name: &str| -> Result<Option<usize>> {
            let Some(raw) = get(name)? else { return Ok(None) };
            let raw = raw.trim();
            if raw.is_empty() {
                return Ok(None);
            }
            let n: usize = raw
                .parse()
                .with_context(|| format!("{name}={raw:?} is not a whole number"))?;
            if !(1..=MAX_PAYMENT_WORKERS).contains(&n) {
                bail!("{name}={n} is outside 1..={MAX_PAYMENT_WORKERS}");
            }
            Ok(Some(n))
        };
        let both = read("MAX_PAYMENT_WORKERS")?;
        let allocation = read("MAX_ALLOCATION_WORKERS")?;
        let fee = read("MAX_FEE_WORKERS")?;
        Ok(Self {
            allocation_workers: both.or(allocation).unwrap_or(DEFAULT_MAX_ALLOCATION_WORKERS),
            fee_workers: both.or(fee).unwrap_or(DEFAULT_MAX_FEE_WORKERS),
        })
    }
}

/// CC margin added to estimated amount for amulet selection (covers fees/rounding)
const AMULET_SELECTION_MARGIN: Decimal = Decimal::ONE;

/// Longest wait for a queued payment's result. Only an abandoned allocation is
/// dropped afterwards; an abandoned fee stays queued until it is paid.
const PAYMENT_WAIT: Duration = Duration::from_secs(600);

/// Worker timeouts in a row after which the shared channel is rebuilt.
const CHANNEL_REBUILD_AFTER: u32 = 3;

/// Delay when no payment can be processed (insufficient amulets)
const SCHEDULER_BACKOFF_SECS: u64 = 5;

/// A payment worker running longer than this (the submit budget plus room for its
/// last attempt) is logged and counted as a timeout; it is never cancelled.
const PAYMENT_WORKER_TIMEOUT: Duration =
    crate::ledger_client::SUBMIT_BUDGET.saturating_add(Duration::from_secs(60));

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaymentPriority {
    /// Allocate — critical path, highest priority
    High = 0,
    /// PayFee (DVP / Alloc) — normal priority
    Normal = 1,
}

impl Ord for PaymentPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        // Lower number = higher priority → reverse comparison for max-heap
        (*other as u8).cmp(&(*self as u8))
    }
}

impl PartialOrd for PaymentPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

enum PaymentRequest {
    PayFee {
        proposal_id: String,
        fee_type: String,
    },
    Allocate {
        proposal_id: String,
        dvp_cid: String,
        allocation_cc: Option<Decimal>,  // Some(amount) for CC, None for CIP-56
    },
}

impl PaymentRequest {
    fn proposal_id(&self) -> &str {
        match self {
            PaymentRequest::PayFee { proposal_id, .. } | PaymentRequest::Allocate { proposal_id, .. } => proposal_id,
        }
    }
}

enum PaymentResponse {
    Step(Result<StepResult>),
}

struct QueuedPayment {
    priority: PaymentPriority,
    sequence: u64,
    request: PaymentRequest,
    response_tx: oneshot::Sender<PaymentResponse>,
}

impl Eq for QueuedPayment {}

impl PartialEq for QueuedPayment {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Ord for QueuedPayment {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, then lower sequence (FIFO)
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedPayment {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ============================================================================
// Batch pay types
// ============================================================================

// ============================================================================
// PaymentQueue
// ============================================================================

/// Parallel payment queue with amulet reservation.
///
/// Payments are submitted via `submit_*` methods (blocking) or `queue_*` (fire-and-forget).
/// A scheduler task picks payments from the priority heap, selects amulets from the cache,
/// reserves them, and spawns worker tasks that execute concurrently (up to max_workers).
pub struct PaymentQueue {
    tx: mpsc::UnboundedSender<QueuedPayment>,
    sequence: AtomicU64,
    /// Queue depth counters
    queued_allocations: Arc<AtomicU64>,
    queued_fees: Arc<AtomicU64>,
    /// Per-pool active worker counters (for heartbeat visibility)
    active_alloc_workers: Arc<AtomicU64>,
    active_fee_workers: Arc<AtomicU64>,
    /// Per-pool max worker counts
    max_alloc_workers: usize,
    max_fee_workers: usize,
    /// Shared shutdown signal — wakes the scheduler's `rx.recv()` and
    /// `sleep` arms instantly so it exits without waiting on idle backoff.
    shutdown: Shutdown,
}

impl PaymentQueue {
    /// Create a new payment queue with parallel workers. Fails on invalid
    /// worker limits or when called outside a tokio runtime.
    pub fn new(
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        cache: CcView,
        shutdown: Shutdown,
    ) -> Result<Self> {
        let limits = PaymentLimits::from_env()?;
        let max_alloc_workers = limits.allocation_workers;
        let max_fee_workers = limits.fee_workers;
        let alloc_semaphore = Arc::new(agent_logic::sync::semaphore(max_alloc_workers)?);
        let fee_semaphore = Arc::new(agent_logic::sync::semaphore(max_fee_workers)?);

        let (tx, rx) = mpsc::unbounded_channel();
        let queued_allocations = Arc::new(AtomicU64::new(0));
        let queued_fees = Arc::new(AtomicU64::new(0));
        let active_alloc_workers = Arc::new(AtomicU64::new(0));
        let active_fee_workers = Arc::new(AtomicU64::new(0));

        let scheduler = Self::scheduler(
            rx,
            config,
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock,
            cache,
            queued_allocations.clone(),
            queued_fees.clone(),
            alloc_semaphore,
            fee_semaphore,
            active_alloc_workers.clone(),
            active_fee_workers.clone(),
            shutdown.clone(),
        );
        // It owns the queue's receiver, so it runs once; a failure stops the agent
        let mut once = Some(scheduler);
        let s = shutdown.clone();
        supervise::spawn_supervised("payment scheduler", shutdown.clone(), Policy::Escalate, move || {
            let (run, s) = (once.take(), s.clone());
            async move {
                if let Some(run) = run {
                    run.await;
                }
                // A dropped queue ends the scheduler; that is not a failure
                s.wait().await;
            }
        })?;

        Ok(Self {
            tx,
            sequence: AtomicU64::new(0),
            queued_allocations,
            queued_fees,
            active_alloc_workers,
            active_fee_workers,
            max_alloc_workers,
            max_fee_workers,
            shutdown,
        })
    }

    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, AtomicOrdering::Relaxed)
    }

    /// Submit a pay_fee operation and await the result.
    pub async fn submit_pay_fee(
        &self,
        proposal_id: &str,
        fee_type: &str,
    ) -> Result<StepResult> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(QueuedPayment {
                priority: PaymentPriority::Normal,
                sequence: self.next_sequence(),
                request: PaymentRequest::PayFee {
                    proposal_id: proposal_id.to_string(),
                    fee_type: fee_type.to_string(),
                },
                response_tx,
            })
            .map_err(|_| anyhow!("Payment queue closed"))?;

        await_response(response_rx, PAYMENT_WAIT).await
    }

    /// Submit an allocate operation and await the result (highest priority).
    /// `allocation_cc`: Some(amount) if allocating CC amulets (needs amulet pre-selection),
    /// None if allocating CIP-56 tokens.
    pub async fn submit_allocate(
        &self,
        proposal_id: &str,
        dvp_cid: &str,
        allocation_cc: Option<Decimal>,
    ) -> Result<StepResult> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(QueuedPayment {
                priority: PaymentPriority::High,
                sequence: self.next_sequence(),
                request: PaymentRequest::Allocate {
                    proposal_id: proposal_id.to_string(),
                    dvp_cid: dvp_cid.to_string(),
                    allocation_cc,
                },
                response_tx,
            })
            .map_err(|_| anyhow!("Payment queue closed"))?;

        await_response(response_rx, PAYMENT_WAIT).await
    }

    /// Get current queue depth: (allocations, fees).
    pub fn queue_depth(&self) -> (u64, u64) {
        (
            self.queued_allocations.load(AtomicOrdering::Relaxed),
            self.queued_fees.load(AtomicOrdering::Relaxed),
        )
    }

    /// Signal the scheduler to stop dispatching new work and exit.
    pub fn shutdown(&self) {
        self.shutdown.signal();
    }

    /// Get per-pool worker utilization:
    /// (alloc_active, alloc_max, fee_active, fee_max)
    pub fn worker_utilization(&self) -> (u64, usize, u64, usize) {
        (
            self.active_alloc_workers.load(AtomicOrdering::Relaxed),
            self.max_alloc_workers,
            self.active_fee_workers.load(AtomicOrdering::Relaxed),
            self.max_fee_workers,
        )
    }

    /// Check if regular fees are paused due to sequencer backpressure.
    /// Returns Some(remaining_secs) if paused, None if not.
    pub fn fee_pause_secs(&self) -> Option<u64> {
        crate::ledger_client::fee_pause_remaining()
    }

    /// Scheduler: picks payments, selects amulets, reserves, spawns workers
    #[allow(clippy::too_many_arguments)]
    async fn scheduler(
        mut rx: mpsc::UnboundedReceiver<QueuedPayment>,
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        cache: CcView,
        queued_allocations: Arc<AtomicU64>,
        queued_fees: Arc<AtomicU64>,
        alloc_semaphore: Arc<Semaphore>,
        fee_semaphore: Arc<Semaphore>,
        active_alloc_workers: Arc<AtomicU64>,
        active_fee_workers: Arc<AtomicU64>,
        shutdown: Shutdown,
    ) {
        let (max_alloc_workers, max_fee_workers) =
            (alloc_semaphore.available_permits(), fee_semaphore.available_permits());
        let mut heap: BinaryHeap<QueuedPayment> = BinaryHeap::new();
        let sequence = AtomicU64::new(u64::MAX / 2);

        // Shared gRPC channel — created lazily on first dispatch, then reused
        // by all workers via HTTP/2 multiplexing (avoids per-worker TCP+TLS overhead)
        let mut shared = SharedChannel::default();

        info!(
            "Payment scheduler started: {} allocation, {} fee workers",
            max_alloc_workers, max_fee_workers,
        );

        loop {
            // Check shutdown before dispatching new work
            if shutdown.is_shutting_down() {
                info!("Payment scheduler shutting down ({} items in heap)", heap.len());
                return;
            }

            // If the heap is empty, block until a new item arrives — but
            // also wake on shutdown so we don't sit on an empty channel.
            if heap.is_empty() {
                tokio::select! {
                    biased;
                    _ = shutdown.wait() => {
                        info!("Payment scheduler shutting down (idle, 0 items in heap)");
                        return;
                    }
                    item = rx.recv() => {
                        match item {
                            Some(item) => heap.push(item),
                            None => {
                                debug!("Payment queue channel closed, scheduler exiting");
                                return;
                            }
                        }
                    }
                }
            }

            // Non-blocking drain of any additional items
            while let Ok(item) = rx.try_recv() {
                heap.push(item);
            }

            // Update queue depth counters
            {
                let (mut alloc, mut fees) = (0u64, 0u64);
                for item in heap.iter() {
                    match &item.request {
                        PaymentRequest::Allocate { .. } => alloc = alloc.saturating_add(1),
                        PaymentRequest::PayFee { .. } => fees = fees.saturating_add(1),
                    }
                }
                queued_allocations.store(alloc, AtomicOrdering::Relaxed);
                queued_fees.store(fees, AtomicOrdering::Relaxed);
            }

            // Ensure shared channel is initialized before dispatching
            let channel = match shared.get(&config).await {
                Ok(ch) => ch,
                Err(e) => {
                    warn!("Failed to create shared gRPC channel: {:#} — will retry", e);
                    if shutdown.sleep(Duration::from_secs(5)).await {
                        info!("Payment scheduler shutting down (channel-create retry)");
                        return;
                    }
                    continue;
                }
            };

            // Try to dispatch as many payments as possible
            let mut deferred: Vec<QueuedPayment> = Vec::new();
            let mut dispatched_any = false;
            let mut allocation_deferred = false;

            while let Some(item) = heap.pop() {
                let Some((item, estimated_cc)) = screen(item) else {
                    continue;
                };
                let item_priority = item.priority;

                // Skip non-allocations if an allocation was already deferred —
                // don't let fees consume amulets that allocations need
                if allocation_deferred && item_priority != PaymentPriority::High {
                    deferred.push(item);
                    continue;
                }

                // Check if fees are paused (sequencer backpressure or forecast)
                let fees_paused = crate::ledger_client::fee_pause_remaining().is_some()
                    || agent_logic::forecast::is_fees_paused_by_overload();

                // Skip sync PayFee while fees paused
                if item_priority == PaymentPriority::Normal && fees_paused {
                    if matches!(&item.request, PaymentRequest::PayFee { .. }) {
                        deferred.push(item);
                        continue;
                    }
                }

                let selectable = cache.get_selectable_amulets().await;

                let is_allocation = matches!(
                    &item.request,
                    PaymentRequest::Allocate { allocation_cc: Some(_), .. }
                );
                // Off-chain PayFee needs no amulets — only CC allocations select.
                let selected = if is_allocation {
                    let mut sel = select_amulets_for_allocation(&selectable, estimated_cc);
                    if sel.is_empty() {
                        // Splitter-reserve fail-open: v1 settle success outranks
                        // reserve preservation — retry including the reserve.
                        let with_reserve = cache.get_selectable_amulets_incl_reserve().await;
                        sel = select_amulets_for_allocation(&with_reserve, estimated_cc);
                    }
                    sel
                } else {
                    Vec::new()
                };

                // Defense in depth: never submit an allocation whose selected amulets
                // sum below the requested amount — the on-chain transaction would fail
                // with ITR_InsufficientFunds. The selector already guards this; this
                // catches any future regression.
                let insufficient_allocation = is_allocation
                    && !selected.is_empty()
                    && selection_short(&selected, estimated_cc);

                if (selected.is_empty() || insufficient_allocation) && estimated_cc > Decimal::ZERO {
                    // Not enough amulets — defer this payment (kept until success)
                    deferred.push(item);
                    // If allocation can't get amulets, block lower-priority items
                    // so freed amulets go to allocations first
                    if item_priority == PaymentPriority::High {
                        allocation_deferred = true;
                    }
                    continue;
                }

                let selected_cids: Vec<String> = selected.iter().map(|a| a.contract_id.clone()).collect();
                let payment_id = format!("payment-{}", sequence.fetch_add(1, AtomicOrdering::Relaxed));

                // Reserve amulets (skip for zero-CC operations)
                if !selected_cids.is_empty() && !cache.reserve(&selected_cids, &payment_id).await {
                    // Reservation failed (race condition) — defer
                    deferred.push(item);
                    continue;
                }

                // Select the per-type semaphore (separate pools prevent starvation)
                let target_semaphore = match &item.request {
                    PaymentRequest::Allocate { .. } => alloc_semaphore.clone(),
                    PaymentRequest::PayFee { .. } => fee_semaphore.clone(),
                };

                // Acquire worker permit from the type-specific pool (non-blocking)
                let permit = match target_semaphore.try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        // This pool is full — defer this item but CONTINUE trying
                        // others (different pools may have capacity)
                        if !selected_cids.is_empty() {
                            cache.release_reservations(&selected_cids).await;
                        }
                        deferred.push(item);
                        continue;
                    }
                };

                dispatched_any = true;

                // Track active workers per pool
                let worker_counter = match &item.request {
                    PaymentRequest::Allocate { .. } => active_alloc_workers.clone(),
                    PaymentRequest::PayFee { .. } => active_fee_workers.clone(),
                };
                let job = WorkerJob {
                    channel: channel.clone(),
                    config: config.clone(),
                    cache: cache.clone(),
                    confirm_lock: confirm_lock.clone(),
                    flags: Flags { verbose, dry_run, force, confirm },
                    reservation: ReservationGuard::new(Arc::clone(cache.inner()), selected_cids),
                    _active: ActiveWorker::start(worker_counter),
                    _permit: permit,
                    timeouts: Arc::clone(&shared.timeouts),
                    slow_after: PAYMENT_WORKER_TIMEOUT,
                };
                let _ = supervise::try_spawn("payment worker", job.run(item));
            }

            // Put deferred items back into the heap
            for item in deferred {
                heap.push(item);
            }

            // If nothing was dispatched and heap is non-empty, backoff before retrying
            if !dispatched_any && !heap.is_empty() {
                debug!(
                    "Scheduler: {} payments deferred (insufficient selectable amulets), backing off {}s",
                    heap.len(),
                    SCHEDULER_BACKOFF_SECS
                );
                // While backing off, also drain new items and observe shutdown
                tokio::select! {
                    biased;
                    _ = shutdown.wait() => {
                        info!("Payment scheduler shutting down (backoff)");
                        return;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(SCHEDULER_BACKOFF_SECS)) => {}
                    item = rx.recv() => {
                        if let Some(item) = item {
                            heap.push(item);
                        } else {
                            return; // channel closed
                        }
                    }
                }
            }
        }
    }
}


// ============================================================================
// Amulet selection helpers
// ============================================================================

/// CC to select for a payment (amount + margin); `None` when it, or its fee
/// margin, is out of range.
fn payment_target(request: &PaymentRequest) -> Option<Decimal> {
    let target = match request {
        // Off-chain pay-fee: no on-chain CC transfer, no amulet selection.
        PaymentRequest::PayFee { .. } => Decimal::ZERO,
        // CC allocation: need the full allocation amount + margin for amulet pre-selection
        // CIP-56 allocation: no amulets needed
        PaymentRequest::Allocate { allocation_cc, .. } => match allocation_cc {
            Some(cc) => cc.checked_add(AMULET_SELECTION_MARGIN)?,
            None => Decimal::ZERO,
        },
    };
    with_fee_margin(target)?;
    Some(target)
}

/// The payment and its selection target, or `None` once it has left the queue: an
/// allocation nobody waits for, or an amount out of range (answered with an error).
fn screen(item: QueuedPayment) -> Option<(QueuedPayment, Decimal)> {
    if item.response_tx.is_closed() && matches!(item.request, PaymentRequest::Allocate { .. }) {
        info!("Payment for {} dropped: its caller stopped waiting", item.request.proposal_id());
        return None;
    }
    match payment_target(&item.request) {
        Some(target) => Some((item, target)),
        None => {
            warn!("Payment for {} rejected: allocation amount out of range", item.request.proposal_id());
            let _ = item
                .response_tx
                .send(PaymentResponse::Step(Err(anyhow!("allocation amount out of range"))));
            None
        }
    }
}

/// Whether the selected amulets sum below `need`; a sum beyond the Decimal range covers it.
fn selection_short(selected: &[CachedAmulet], need: Decimal) -> bool {
    match dec_sum(selected.iter().map(|a| a.amount)) {
        Some(total) if total < need => {
            warn!(
                "Allocation deferred: selected {} amulets totalling {} CC, need {} CC",
                selected.len(),
                Dp(total, 4),
                Dp(need, 4)
            );
            true
        }
        Some(_) => false,
        None => {
            warn!("Selected {} amulets total beyond the Decimal range; treating it as covered", selected.len());
            false
        }
    }
}

/// A queued payment's result, waited for at most `wait`.
async fn await_response(response_rx: oneshot::Receiver<PaymentResponse>, wait: Duration) -> Result<StepResult> {
    match tokio::time::timeout(wait, response_rx).await {
        Ok(Ok(PaymentResponse::Step(r))) => r,
        Ok(Err(_)) => Err(anyhow!("Payment processor dropped without responding")),
        Err(_) => Err(anyhow!("Payment not completed within {}s", wait.as_secs())),
    }
}

/// Splice rejects amulet transfers above `transferConfig.maxNumInputs`.
pub(crate) const MAX_AMULET_INPUTS: usize = 100;

/// CC holding fees decay an amulet between selection and execution, so select
/// against a slightly larger target than the amount actually being moved.
/// `None` when the result is out of range.
pub(crate) fn with_fee_margin(amount: Decimal) -> Option<Decimal> {
    amount.checked_mul(Decimal::new(102, 2)) // 1.02
}

/// Indices into an ASCENDING-sorted amount list that cover `target`, one
/// covering amulet if there is one, else largest-first. Empty means the target
/// is unreachable within [`MAX_AMULET_INPUTS`]; a partial set never settles.
pub(crate) fn select_amulet_indices(ascending: &[Decimal], target: Decimal) -> Vec<usize> {
    if target <= Decimal::ZERO {
        return vec![];
    }

    // Prefer ONE amulet that covers the amount (smallest such amulet).
    if let Some(i) = ascending.iter().position(|amt| *amt >= target) {
        return vec![i];
    }

    // Multi-amulet fallback: take the LARGEST first. Picking smallest-first
    // would often produce a partial set whose total is below the target,
    // causing the on-chain transaction to fail with ITR_InsufficientFunds.
    let mut selected = Vec::new();
    let mut total = Decimal::ZERO;
    for (i, amount) in ascending.iter().enumerate().rev() {
        if selected.len() >= MAX_AMULET_INPUTS {
            break;
        }
        selected.push(i);
        // A total beyond the Decimal range is above any target
        let Some(next) = total.checked_add(*amount) else {
            return selected;
        };
        total = next;
        if total >= target {
            return selected;
        }
    }

    vec![]
}

/// Select amulets for CC allocations.
/// Prefers ONE amulet that covers the full amount (smallest-fit single).
/// Falls back to multiple amulets if no single one suffices.
/// Returns empty vec if insufficient amulets (scheduler will defer and retry).
pub(crate) fn select_amulets_for_allocation(selectable: &[CachedAmulet], estimated_cc: Decimal) -> Vec<CachedAmulet> {
    if estimated_cc <= Decimal::ZERO {
        return vec![];
    }
    let Some(target) = with_fee_margin(estimated_cc) else {
        return vec![];
    };
    let amounts: Vec<Decimal> = selectable.iter().map(|a| a.amount).collect();
    select_amulet_indices(&amounts, target)
        .into_iter()
        .filter_map(|i| selectable.get(i).cloned())
        .collect()
}

/// Process amulet cache updates after a successful transaction
pub(crate) async fn process_tx_result(
    cache: &CcView,
    input_cids: &[String],
    result: &orderbook_proto::ledger::ExecuteTransactionResponse,
) {
    // Mark input amulets as consumed
    if !input_cids.is_empty() {
        cache.mark_consumed(input_cids, &result.update_id).await;
    }

    // Add newly created amulets from change/split
    let new_amulets: Vec<CachedAmulet> = result
        .created_contracts
        .iter()
        .filter(|c| c.template_id.contains("Amulet") && !c.template_id.contains("Rules"))
        .filter_map(|c| {
            let amount = c.amount.parse::<Decimal>().ok()?;
            if amount <= Decimal::ZERO {
                return None;
            }
            Some(CachedAmulet {
                contract_id: c.contract_id.clone(),
                amount,
                discovered_at: std::time::Instant::now(),
            })
        })
        .collect();

    if !new_amulets.is_empty() {
        debug!(
            "Transaction created {} new amulets from change/split",
            new_amulets.len()
        );
        cache.add_created_amulets(new_amulets).await;
    }
}

/// Handle INACTIVE_CONTRACTS error — mark amulets as consumed and release
pub(crate) async fn handle_inactive_contracts(cache: &CcView, input_cids: &[String]) {
    if !input_cids.is_empty() {
        cache.mark_consumed(input_cids, "inactive").await;
    }
}

// ============================================================================
// Payment workers
// ============================================================================

#[derive(Clone, Copy)]
struct Flags {
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
}

/// Counts one active worker in its pool until dropped, also on unwind.
struct ActiveWorker(Arc<AtomicU64>);

impl ActiveWorker {
    fn start(counter: Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, AtomicOrdering::Relaxed);
        Self(counter)
    }
}

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.fetch_sub(1, AtomicOrdering::Relaxed);
    }
}

/// Consecutive worker timeouts.
#[derive(Default)]
struct TimeoutStreak(AtomicU32);

impl TimeoutStreak {
    fn record(&self, timed_out: bool) {
        if timed_out {
            let mut n = self.0.load(AtomicOrdering::Relaxed);
            while let Err(actual) = self.0.compare_exchange_weak(
                n,
                n.saturating_add(1),
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            ) {
                n = actual;
            }
        } else {
            self.0.store(0, AtomicOrdering::Relaxed);
        }
    }

    /// The streak, reset, once it reached `limit`.
    fn take_if_at_least(&self, limit: u32) -> Option<u32> {
        let n = self.0.load(AtomicOrdering::Relaxed);
        (n >= limit).then(|| {
            self.0.store(0, AtomicOrdering::Relaxed);
            n
        })
    }
}

/// The scheduler's shared gRPC channel and its workers' timeout streak.
#[derive(Default)]
struct SharedChannel {
    channel: Option<Channel>,
    timeouts: Arc<TimeoutStreak>,
}

impl SharedChannel {
    /// The channel for new workers: created when there is none, and again
    /// once `CHANNEL_REBUILD_AFTER` workers in a row timed out on it.
    async fn get(&mut self, config: &BaseConfig) -> Result<Channel> {
        if let Some(n) = self.timeouts.take_if_at_least(CHANNEL_REBUILD_AFTER) {
            if self.channel.take().is_some() {
                warn!("{} payment workers in a row timed out; rebuilding the shared gRPC channel", n);
            }
        }
        if let Some(ch) = &self.channel {
            return Ok(ch.clone());
        }
        let ch = DAppProviderClient::create_channel(
            &config.orderbook_grpc_url,
            Some(config.connection_timeout_secs),
            Some(config.request_timeout_secs),
        )
        .await?;
        info!("Shared gRPC channel created for payment workers");
        self.channel = Some(ch.clone());
        // Outcomes of workers on an earlier channel no longer count.
        self.timeouts = Arc::default();
        Ok(ch)
    }
}

/// Whether an error is a call or connect running out of time.
fn is_timeout(e: &anyhow::Error) -> bool {
    let msg = format!("{e:#}");
    ["client deadline", "Timeout expired", "timed out"]
        .iter()
        .any(|needle| msg.contains(needle))
}

/// One dispatched payment and what its worker holds until it ends.
struct WorkerJob {
    channel: Channel,
    config: BaseConfig,
    cache: CcView,
    confirm_lock: ConfirmLock,
    flags: Flags,
    reservation: ReservationGuard,
    _active: ActiveWorker,
    _permit: OwnedSemaphorePermit,
    timeouts: Arc<TimeoutStreak>,
    /// `PAYMENT_WORKER_TIMEOUT` outside tests.
    slow_after: Duration,
}

impl WorkerJob {
    /// Run the payment and answer its caller. A worker past
    /// `slow_after` is logged and counted, never cancelled.
    async fn run(self, item: QueuedPayment) {
        let WorkerJob { channel, config, cache, confirm_lock, flags, reservation, timeouts, slow_after, .. } = self;
        let QueuedPayment { request, response_tx, .. } = item;
        let work = async {
            match &request {
                PaymentRequest::Allocate { proposal_id, dvp_cid, .. } => {
                    execute_allocate(
                        channel, &config, proposal_id, dvp_cid, reservation, flags, &confirm_lock, &cache,
                    )
                    .await
                }
                PaymentRequest::PayFee { proposal_id, fee_type } => {
                    drop(reservation);
                    execute_pay_fee(channel, &config, proposal_id, fee_type, flags, &confirm_lock).await
                }
            }
        };
        let (result, slow) = run_to_end(work, slow_after, request.proposal_id()).await;
        timeouts.record(slow || result.as_ref().is_err_and(is_timeout));
        answer(response_tx, request.proposal_id(), result);
    }
}

/// Send a payment's result; a failure nobody waits for any more is logged here.
fn answer(response_tx: oneshot::Sender<PaymentResponse>, proposal_id: &str, result: Result<StepResult>) {
    if let Err(PaymentResponse::Step(Err(e))) = response_tx.send(PaymentResponse::Step(result)) {
        warn!("Payment for {} failed after its caller stopped waiting: {:#}", proposal_id, e);
    }
}

/// Await `work` to its end, warning once it runs past `slow_after`. Returns
/// its output and whether it was slow.
async fn run_to_end<T>(work: impl std::future::Future<Output = T>, slow_after: Duration, proposal_id: &str) -> (T, bool) {
    let on_slow = || {
        warn!(
            "Payment worker for {} still running after {}s; waiting for it to finish",
            proposal_id,
            slow_after.as_secs()
        );
    };
    agent_logic::supervise::run_to_end(work, slow_after, on_slow).await
}

// ============================================================================
// Payment execution
// ============================================================================

/// Create a DAppProviderClient from a shared channel (no TCP+TLS overhead)
fn create_client_from_channel(channel: Channel, config: &BaseConfig) -> Result<DAppProviderClient> {
    let client = DAppProviderClient::from_channel(
        channel,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
    )?;
    Ok(client.with_request_timeout(std::time::Duration::from_secs(config.request_timeout_secs)))
}

async fn execute_pay_fee(
    channel: Channel,
    config: &BaseConfig,
    proposal_id: &str,
    fee_type: &str,
    flags: Flags,
    confirm_lock: &ConfirmLock,
) -> Result<StepResult> {
    let Flags { dry_run, confirm, .. } = flags;
    if confirm && !dry_run {
        confirm_transaction(
            confirm_lock,
            &format!("Pay {} fee (off-chain debit)", fee_type),
            &format!("proposal: {}", proposal_id),
        )
        .await?;
    }

    if dry_run {
        info!("[dry-run] would pay {} fee for {}", fee_type, proposal_id);
        return Ok(StepResult {
            contract_id: String::new(),
            update_id: format!("{}:{}", fee_type, proposal_id),
            traffic_total: 0,
        });
    }

    let mut client = create_client_from_channel(channel, config)?;

    // Off-chain processing-fee payment via PreparePayFee / ExecutePayFee.
    // The fee is debited from the cloud-agent's prepaid traffic balance.
    match client.pay_processing_fee(proposal_id, fee_type).await {
        Ok(()) => {
            info!(
                "{} fee paid off-chain for {} (debited from prepaid traffic balance)",
                fee_type, proposal_id
            );
            Ok(StepResult {
                contract_id: String::new(),
                update_id: format!("{}:{}", fee_type, proposal_id),
                traffic_total: 0,
            })
        }
        Err(e) => Err(e),
    }
}

/// Allocate with the amulets `reservation` holds; it is released if this ends
/// before the submit does.
#[allow(clippy::too_many_arguments)]
async fn execute_allocate(
    channel: Channel,
    config: &BaseConfig,
    proposal_id: &str,
    dvp_cid: &str,
    reservation: ReservationGuard,
    flags: Flags,
    confirm_lock: &ConfirmLock,
    cache: &CcView,
) -> Result<StepResult> {
    let Flags { verbose, dry_run, force, confirm } = flags;
    if confirm && !dry_run {
        confirm_transaction(
            confirm_lock,
            "Allocate",
            &format!("proposal: {}, dvp: {}", proposal_id, dvp_cid),
        )
        .await?;
    }

    let mut client = create_client_from_channel(channel, config)?;

    let expectation = OperationExpectation::Allocate {
        party: config.party_id.clone(),
        proposal_id: proposal_id.to_string(),
        dvp_cid: dvp_cid.to_string(),
    };

    let result = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::Allocate as i32,
                params: Some(Params::Allocate(AllocateParams {
                    proposal_id: proposal_id.to_string(),
                    dvp_cid: dvp_cid.to_string(),
                    amulet_cids: reservation.cids().to_vec(),
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await;
    let amulet_cids = reservation.disarm();

    match result {
        Ok(ref resp) => {
            process_tx_result(cache, &amulet_cids, resp).await;
            let traffic = resp.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
            let cid = resp.contract_id.clone().unwrap_or_default();
            info!("Allocation complete for {}: CID={} (update: {}, traffic={})", proposal_id, cid, resp.update_id, traffic);
            Ok(StepResult {
                contract_id: cid,
                update_id: resp.update_id.clone(),
                traffic_total: traffic,
            })
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("INACTIVE_CONTRACTS") {
                handle_inactive_contracts(cache, &amulet_cids).await;
            } else {
                cache.release_reservations(&amulet_cids).await;
            }
            Err(e)
        }
    }
}

// `execute_transfer_traffic_fee` removed: traffic billing is now handled
// off-chain by the ledger via the prepaid traffic pool (debited inside
// `execute.rs` after each successful Canton tx). The cloud-agent no longer
// pays sequencer traffic fees on-chain.

#[cfg(test)]
mod limit_tests {
    use super::*;
    use std::collections::HashMap;

    fn limits(vars: &[(&str, &str)]) -> Result<PaymentLimits> {
        let vars: HashMap<String, String> =
            vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        PaymentLimits::from_lookup(|name| Ok(vars.get(name).cloned()))
    }

    fn pools(allocation_workers: usize, fee_workers: usize) -> PaymentLimits {
        PaymentLimits { allocation_workers, fee_workers }
    }

    #[test]
    fn unset_or_blank_limits_use_the_defaults() {
        assert_eq!(limits(&[]).unwrap(), pools(20, 5));
        assert_eq!(limits(&[("MAX_FEE_WORKERS", " "), ("MAX_PAYMENT_WORKERS", "")]).unwrap(), pools(20, 5));
    }

    #[test]
    fn limits_are_read_per_pool_and_the_shared_value_overrides_both() {
        assert_eq!(limits(&[("MAX_ALLOCATION_WORKERS", "3"), ("MAX_FEE_WORKERS", " 3 ")]).unwrap(), pools(3, 3));
        let both = [("MAX_PAYMENT_WORKERS", "7"), ("MAX_ALLOCATION_WORKERS", "3")];
        assert_eq!(limits(&both).unwrap(), pools(7, 7));
        assert_eq!(limits(&[("MAX_FEE_WORKERS", "256")]).unwrap(), pools(20, 256));
    }

    // Values that would stall the pool or overflow the semaphore are startup errors
    #[test]
    fn out_of_range_or_garbage_limits_are_errors() {
        for (name, value) in [
            ("MAX_ALLOCATION_WORKERS", "0"),
            ("MAX_FEE_WORKERS", "257"),
            ("MAX_PAYMENT_WORKERS", "-1"),
            ("MAX_FEE_WORKERS", "lots"),
            ("MAX_ALLOCATION_WORKERS", "18446744073709551615"),
        ] {
            let err = limits(&[(name, value)]).unwrap_err().to_string();
            assert!(err.starts_with(name), "{err}");
        }
    }

    // Pooled clients share a channel built with the configured request timeout
    #[tokio::test]
    async fn pooled_clients_follow_the_configured_request_timeout() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.request_timeout_secs = 300;
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let client = create_client_from_channel(channel, &config).unwrap();
        assert_eq!(client.call_deadline(), std::time::Duration::from_secs(305));
    }

    #[test]
    fn a_worker_outlives_the_submit_budget() {
        assert!(PAYMENT_WORKER_TIMEOUT > crate::ledger_client::SUBMIT_BUDGET);
    }

    #[test]
    fn a_queue_outside_a_runtime_is_an_error() {
        let config = BaseConfig::test_minimal().unwrap();
        let cache = crate::holdings_cache::HoldingsCache::new(false).cc();
        let queue = PaymentQueue::new(
            config,
            false,
            false,
            false,
            false,
            agent_logic::confirm::new_confirm_lock(),
            cache,
            Shutdown::new(),
        );
        assert!(queue.is_err());
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    use std::str::FromStr;
    use std::time::Instant;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn allocate(cc: Option<Decimal>) -> PaymentRequest {
        PaymentRequest::Allocate { proposal_id: "p1".into(), dvp_cid: "d1".into(), allocation_cc: cc }
    }

    fn queued(request: PaymentRequest) -> (QueuedPayment, oneshot::Receiver<PaymentResponse>) {
        let (response_tx, rx) = oneshot::channel();
        (QueuedPayment { priority: PaymentPriority::High, sequence: 0, request, response_tx }, rx)
    }

    fn amulet(cid: &str, amount: &str) -> CachedAmulet {
        CachedAmulet { contract_id: cid.into(), amount: dec(amount), discovered_at: Instant::now() }
    }

    const BIG: &str = "50000000000000000000000000000";

    #[test]
    fn payment_targets_add_the_margin_and_reject_out_of_range_amounts() {
        assert_eq!(payment_target(&allocate(Some(dec("10")))), Some(dec("11")));
        assert_eq!(payment_target(&allocate(None)), Some(Decimal::ZERO));
        let fee = PaymentRequest::PayFee { proposal_id: "p".into(), fee_type: "dvp".into() };
        assert_eq!(payment_target(&fee), Some(Decimal::ZERO));
        assert_eq!(payment_target(&allocate(Some(Decimal::MAX))), None);
        assert_eq!(payment_target(&allocate(Some(dec("78000000000000000000000000000")))), None, "the margin overflows");
    }

    // An out-of-range amount is answered with an error instead of waiting in the queue
    #[test]
    fn an_out_of_range_payment_is_answered_and_dropped() {
        let (item, mut rx) = queued(allocate(Some(Decimal::MAX)));
        assert!(screen(item).is_none());
        match rx.try_recv() {
            Ok(PaymentResponse::Step(Err(e))) => assert_eq!(e.to_string(), "allocation amount out of range"),
            _ => panic!("expected an error reply"),
        }
    }

    #[test]
    fn a_payment_nobody_waits_for_is_dropped() {
        let (item, rx) = queued(allocate(Some(dec("10"))));
        drop(rx);
        assert!(screen(item).is_none());
        let (item, _rx) = queued(allocate(Some(dec("10"))));
        let (_item, target) = screen(item).unwrap();
        assert_eq!(target, dec("11"));
    }

    // A fee whose caller stopped waiting is still paid once fees resume
    #[test]
    fn an_abandoned_fee_stays_queued_but_an_abandoned_allocation_is_dropped() {
        let (item, rx) = queued(PaymentRequest::PayFee { proposal_id: "p1".into(), fee_type: "dvp".into() });
        drop(rx);
        let (_item, target) = screen(item).expect("the fee is still paid");
        assert_eq!(target, Decimal::ZERO);
        let (item, rx) = queued(allocate(Some(dec("10"))));
        drop(rx);
        assert!(screen(item).is_none());
    }

    // A failure nobody waits for any more is logged; an answered or successful one is not
    #[test]
    fn a_late_payment_failure_is_logged() {
        let logs = crate::test_util::LogBuf::default();
        let _capture = logs.capture(tracing::Level::WARN);
        let late = "failed after its caller stopped waiting";
        let (tx, rx) = oneshot::channel();
        drop(rx);
        answer(tx, "p1", Err(anyhow!("fee service down")));
        assert_eq!(logs.count("Payment for p1 failed after its caller stopped waiting: fee service down"), 1);

        let (tx, rx) = oneshot::channel();
        drop(rx);
        let paid = StepResult { contract_id: String::new(), update_id: "dvp:p2".into(), traffic_total: 0 };
        answer(tx, "p2", Ok(paid));
        let (tx, mut rx) = oneshot::channel();
        answer(tx, "p3", Err(anyhow!("rejected")));
        assert!(matches!(rx.try_recv(), Ok(PaymentResponse::Step(Err(_)))));
        assert_eq!(logs.count(late), 1, "only the unanswered failure is logged");
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_result_is_bounded_and_abandons_the_payment() {
        let (tx, rx) = oneshot::channel::<PaymentResponse>();
        let err = await_response(rx, PAYMENT_WAIT).await.unwrap_err();
        assert_eq!(err.to_string(), "Payment not completed within 600s");
        assert!(tx.is_closed(), "the scheduler then sees the payment as abandoned");
    }

    // A running total past the Decimal range covers any target
    #[test]
    fn selection_overflow_counts_as_covered() {
        let big = dec(BIG);
        assert_eq!(select_amulet_indices(&[big, big, big], Decimal::MAX), vec![2, 1]);
        assert!(select_amulets_for_allocation(&[amulet("a", "1")], Decimal::MAX).is_empty());
        assert!(!selection_short(&[amulet("a", BIG), amulet("b", BIG)], Decimal::MAX));
        assert!(selection_short(&[amulet("a", "5")], dec("10")));
    }

    // `{:.4}` panics on a total this large when WARN is enabled
    #[test]
    fn a_short_selection_is_logged_at_any_magnitude() {
        let _logs = crate::test_util::warn_logging();
        let big = "5000000000000000000000000000";
        assert!(selection_short(&[amulet("a", big)], dec("6000000000000000000000000000")));
    }

    #[test]
    fn the_timeout_streak_counts_consecutive_timeouts() {
        let streak = TimeoutStreak::default();
        streak.record(true);
        streak.record(true);
        streak.record(false);
        streak.record(true);
        streak.record(true);
        assert_eq!(streak.take_if_at_least(CHANNEL_REBUILD_AFTER), None);
        streak.record(true);
        assert_eq!(streak.take_if_at_least(CHANNEL_REBUILD_AFTER), Some(3));
        assert_eq!(streak.take_if_at_least(CHANNEL_REBUILD_AFTER), None, "the streak restarts");
        assert!(is_timeout(&anyhow!("Prepare RPC failed (Unavailable): client deadline 125s exceeded")));
        assert!(is_timeout(&anyhow!("Execute RPC failed (Cancelled): Timeout expired")));
        assert!(is_timeout(&anyhow!("(Unavailable) connect to http://x timed out after 25s")));
        assert!(!is_timeout(&anyhow!("Transaction failed: INACTIVE_CONTRACTS")));
    }

    /// Run one PayFee worker against a fake ledger in `mode`; returns its error.
    async fn pay_fee_worker(mode: crate::test_util::Fake, timeouts: &Arc<TimeoutStreak>) -> String {
        pay_fee_worker_with(mode, timeouts, 1, PAYMENT_WORKER_TIMEOUT).await
    }

    /// [`pay_fee_worker`] with the ledger request timeout and the worker's slow mark.
    async fn pay_fee_worker_with(
        mode: crate::test_util::Fake,
        timeouts: &Arc<TimeoutStreak>,
        request_timeout_secs: u64,
        slow_after: Duration,
    ) -> String {
        let fake = crate::test_util::FakeLedger::start(mode).await;
        let mut config = BaseConfig::test_minimal().unwrap();
        config.request_timeout_secs = request_timeout_secs;
        let channel = DAppProviderClient::create_channel(&fake.url, Some(5), Some(request_timeout_secs)).await.unwrap();
        let cache = crate::holdings_cache::HoldingsCache::new(false).cc();
        let permit = Arc::new(agent_logic::sync::semaphore(1).unwrap()).try_acquire_owned().unwrap();
        let job = WorkerJob {
            channel,
            config,
            cache: cache.clone(),
            confirm_lock: agent_logic::confirm::new_confirm_lock(),
            flags: Flags { verbose: false, dry_run: false, force: false, confirm: false },
            reservation: ReservationGuard::new(Arc::clone(cache.inner()), vec![]),
            _active: ActiveWorker::start(Arc::new(AtomicU64::new(0))),
            _permit: permit,
            timeouts: Arc::clone(timeouts),
            slow_after,
        };
        let (item, rx) = queued(PaymentRequest::PayFee { proposal_id: "p1".into(), fee_type: "dvp".into() });
        tokio::time::timeout(Duration::from_secs(30), job.run(item)).await.expect("the worker ends");
        fake.stop().await;
        match rx.await {
            Ok(PaymentResponse::Step(Err(e))) => format!("{e:#}"),
            _ => panic!("the worker answers with an error"),
        }
    }

    // A worker's timeout feeds the rebuild streak; any other outcome resets it
    #[tokio::test]
    async fn worker_timeouts_feed_the_rebuild_streak() {
        let timeouts = Arc::new(TimeoutStreak::default());
        let err = pay_fee_worker(crate::test_util::Fake::Stall, &timeouts).await;
        assert!(err.ends_with("Timeout expired"), "{err}");
        assert_eq!(timeouts.take_if_at_least(1), Some(1));

        pay_fee_worker(crate::test_util::Fake::Stall, &timeouts).await;
        let err = pay_fee_worker(crate::test_util::Fake::Answer, &timeouts).await;
        assert!(err.contains("Missing response_signature"), "{err}");
        assert_eq!(timeouts.take_if_at_least(1), None, "an answered call ends the streak");
    }

    // The worker's own wait: a payment past its slow mark answers with its real result
    #[tokio::test]
    async fn a_slow_payment_worker_answers_with_its_own_result() {
        let timeouts = Arc::new(TimeoutStreak::default());
        let slow = crate::test_util::Fake::Slow(Duration::from_millis(600));
        let err = pay_fee_worker_with(slow, &timeouts, 5, Duration::from_millis(100)).await;
        assert!(err.contains("Missing response_signature") && !err.contains("timed out"), "{err}");
        assert_eq!(timeouts.take_if_at_least(1), Some(1), "the slow run was counted");
    }

    async fn connections_reach(fake: &crate::test_util::FakeLedger, n: usize) {
        let reached = tokio::time::timeout(Duration::from_secs(5), async {
            while fake.connections() < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        reached.await.unwrap_or_else(|_| panic!("{} connection(s), expected {n}", fake.connections()));
    }

    // Three worker timeouts in a row rebuild the shared channel once; an answer in between keeps it
    #[tokio::test]
    async fn three_worker_timeouts_in_a_row_rebuild_the_shared_channel() {
        let fake = crate::test_util::FakeLedger::start(crate::test_util::Fake::Answer).await;
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = fake.url.clone();
        let mut shared = SharedChannel::default();
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 1).await;

        for timed_out in [true, true, false, true, true] {
            shared.timeouts.record(timed_out);
        }
        shared.get(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.connections(), 1, "an answer in between keeps the channel");

        shared.timeouts.record(true);
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 2).await;
        shared.get(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.connections(), 2, "the new channel is then reused");
        fake.stop().await;
    }

    // Timeouts from workers on a replaced channel do not rebuild its successor
    #[tokio::test]
    async fn timeouts_on_a_replaced_channel_keep_its_successor() {
        let fake = crate::test_util::FakeLedger::start(crate::test_util::Fake::Answer).await;
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = fake.url.clone();
        let mut shared = SharedChannel::default();
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 1).await;
        let old_job = Arc::clone(&shared.timeouts);
        for _ in 0..CHANNEL_REBUILD_AFTER {
            old_job.record(true);
        }
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 2).await;
        for _ in 0..CHANNEL_REBUILD_AFTER {
            old_job.record(true);
        }
        shared.get(&config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.connections(), 2, "late timeouts on the old channel keep the new one");
        fake.stop().await;
    }

    // A late answer on a replaced channel does not reset its successor's streak
    #[tokio::test]
    async fn an_answer_on_a_replaced_channel_keeps_the_new_streak() {
        let fake = crate::test_util::FakeLedger::start(crate::test_util::Fake::Answer).await;
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = fake.url.clone();
        let mut shared = SharedChannel::default();
        shared.get(&config).await.unwrap();
        let old_job = Arc::clone(&shared.timeouts);
        for _ in 0..CHANNEL_REBUILD_AFTER {
            old_job.record(true);
        }
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 2).await;
        let new_job = Arc::clone(&shared.timeouts);
        new_job.record(true);
        new_job.record(true);
        old_job.record(false);
        new_job.record(true);
        shared.get(&config).await.unwrap();
        connections_reach(&fake, 3).await;
        fake.stop().await;
    }

    // A slow payment is waited for, never cancelled
    #[tokio::test(start_paused = true)]
    async fn a_slow_worker_runs_to_its_end() {
        let work = async {
            tokio::time::sleep(PAYMENT_WORKER_TIMEOUT * 2).await;
            7u8
        };
        assert_eq!(run_to_end(work, PAYMENT_WORKER_TIMEOUT, "p1").await, (7, true));
        assert_eq!(run_to_end(async { 8u8 }, PAYMENT_WORKER_TIMEOUT, "p1").await, (8, false));
    }

    #[tokio::test]
    async fn the_active_count_drops_when_a_worker_unwinds() {
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let joined = tokio::spawn(async move {
            let _active = ActiveWorker::start(c);
            panic!("worker bug");
        })
        .await;
        assert!(joined.is_err());
        assert_eq!(counter.load(AtomicOrdering::Relaxed), 0);
    }

    // The scheduler ends with its queue; only a failure stops the agent
    #[tokio::test]
    async fn dropping_the_queue_does_not_stop_the_agent() {
        let shutdown = Shutdown::new();
        let queue = PaymentQueue::new(
            BaseConfig::test_minimal().unwrap(),
            false,
            false,
            false,
            false,
            agent_logic::confirm::new_confirm_lock(),
            crate::holdings_cache::HoldingsCache::new(false).cc(),
            shutdown.clone(),
        )
        .unwrap();
        drop(queue);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!shutdown.is_shutting_down());
        shutdown.signal();
    }
}
