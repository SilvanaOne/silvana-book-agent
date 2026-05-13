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

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::confirm::{confirm_transaction, ConfirmLock};
use agent_logic::settlement::StepResult;
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AllocateParams,
    PrepareTransactionRequest, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use tonic::transport::Channel;

use crate::amulet_cache::{AmuletCache, CachedAmulet};
use crate::ledger_client::DAppProviderClient;

/// Default max concurrent allocation workers (critical path — highest priority)
const DEFAULT_MAX_ALLOCATION_WORKERS: usize = 20;

/// Default max concurrent fee payment workers
const DEFAULT_MAX_FEE_WORKERS: usize = 5;

/// CC margin added to estimated amount for amulet selection (covers fees/rounding)
const AMULET_SELECTION_MARGIN: &str = "1.0";

/// Delay when no payment can be processed (insufficient amulets)
const SCHEDULER_BACKOFF_SECS: u64 = 5;

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
    /// Create a new payment queue with parallel workers.
    pub fn new(
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        cache: Arc<AmuletCache>,
        shutdown: Shutdown,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued_allocations = Arc::new(AtomicU64::new(0));
        let queued_fees = Arc::new(AtomicU64::new(0));
        let active_alloc_workers = Arc::new(AtomicU64::new(0));
        let active_fee_workers = Arc::new(AtomicU64::new(0));

        // Backward compat: MAX_PAYMENT_WORKERS overrides all pools if set
        let legacy_override: Option<usize> = std::env::var("MAX_PAYMENT_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok());

        let max_alloc_workers = legacy_override.unwrap_or_else(|| {
            std::env::var("MAX_ALLOCATION_WORKERS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_MAX_ALLOCATION_WORKERS)
        });
        let max_fee_workers = legacy_override.unwrap_or_else(|| {
            std::env::var("MAX_FEE_WORKERS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_MAX_FEE_WORKERS)
        });

        tokio::spawn(Self::scheduler(
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
            max_alloc_workers,
            max_fee_workers,
            active_alloc_workers.clone(),
            active_fee_workers.clone(),
            shutdown.clone(),
        ));

        Self {
            tx,
            sequence: AtomicU64::new(0),
            queued_allocations,
            queued_fees,
            active_alloc_workers,
            active_fee_workers,
            max_alloc_workers,
            max_fee_workers,
            shutdown,
        }
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

        match response_rx.await {
            Ok(PaymentResponse::Step(r)) => r,
            Err(_) => Err(anyhow!("Payment processor dropped without responding")),
        }
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

        match response_rx.await {
            Ok(PaymentResponse::Step(r)) => r,
            Err(_) => Err(anyhow!("Payment processor dropped without responding")),
        }
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
        cache: Arc<AmuletCache>,
        queued_allocations: Arc<AtomicU64>,
        queued_fees: Arc<AtomicU64>,
        max_alloc_workers: usize,
        max_fee_workers: usize,
        active_alloc_workers: Arc<AtomicU64>,
        active_fee_workers: Arc<AtomicU64>,
        shutdown: Shutdown,
    ) {
        let alloc_semaphore = Arc::new(Semaphore::new(max_alloc_workers));
        let fee_semaphore = Arc::new(Semaphore::new(max_fee_workers));
        let mut heap: BinaryHeap<QueuedPayment> = BinaryHeap::new();
        let sequence = AtomicU64::new(u64::MAX / 2);

        // Shared gRPC channel — created lazily on first dispatch, then reused
        // by all workers via HTTP/2 multiplexing (avoids per-worker TCP+TLS overhead)
        let mut shared_channel: Option<Channel> = None;

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
                        PaymentRequest::Allocate { .. } => alloc += 1,
                        PaymentRequest::PayFee { .. } => fees += 1,
                    }
                }
                queued_allocations.store(alloc, AtomicOrdering::Relaxed);
                queued_fees.store(fees, AtomicOrdering::Relaxed);
            }

            // Ensure shared channel is initialized before dispatching
            if shared_channel.is_none() {
                match DAppProviderClient::create_channel(
                    &config.orderbook_grpc_url,
                    Some(config.connection_timeout_secs),
                    Some(config.request_timeout_secs),
                ).await {
                    Ok(ch) => {
                        info!("Shared gRPC channel created for payment workers");
                        shared_channel = Some(ch);
                    }
                    Err(e) => {
                        warn!("Failed to create shared gRPC channel: {:#} — will retry", e);
                        if shutdown.sleep(std::time::Duration::from_secs(5)).await {
                            info!("Payment scheduler shutting down (channel-create retry)");
                            return;
                        }
                        continue;
                    }
                }
            }

            // Try to dispatch as many payments as possible
            let mut deferred: Vec<QueuedPayment> = Vec::new();
            let mut dispatched_any = false;
            let mut allocation_deferred = false;

            while let Some(item) = heap.pop() {
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

                // Estimate CC needed for this payment
                let estimated_cc = estimate_cc_needed(&item.request);

                let selectable = cache.get_selectable_amulets().await;

                let is_allocation = matches!(
                    &item.request,
                    PaymentRequest::Allocate { allocation_cc: Some(_), .. }
                );
                // Off-chain PayFee needs no amulets — only CC allocations select.
                let selected = if is_allocation {
                    select_amulets_for_allocation(&selectable, estimated_cc)
                } else {
                    Vec::new()
                };

                // Defense in depth: never submit an allocation whose selected amulets
                // sum below the requested amount — the on-chain transaction would fail
                // with ITR_InsufficientFunds. The selector already guards this; this
                // catches any future regression.
                let insufficient_allocation = is_allocation
                    && !selected.is_empty()
                    && selected.iter().map(|a| a.amount).sum::<Decimal>() < estimated_cc;
                if insufficient_allocation {
                    let total: Decimal = selected.iter().map(|a| a.amount).sum();
                    warn!(
                        "Allocation deferred: selected {} amulets totalling {:.4} CC, need {:.4} CC",
                        selected.len(), total, estimated_cc
                    );
                }

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
                if !selected_cids.is_empty() {
                    if !cache.reserve(&selected_cids, &payment_id).await {
                        // Reservation failed (race condition) — defer
                        deferred.push(item);
                        continue;
                    }
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
                worker_counter.fetch_add(1, AtomicOrdering::Relaxed);

                // Spawn worker with shared channel clone
                let worker_channel = shared_channel.clone().unwrap(); // safe: initialized above
                let worker_config = config.clone();
                let worker_cache = cache.clone();
                let worker_confirm_lock = confirm_lock.clone();

                tokio::spawn(async move {
                    let _permit = permit; // hold until worker completes
                    let cids_for_cleanup = selected_cids.clone();
                    let cache_for_cleanup = worker_cache.clone();

                    let worker_result = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        async {
                            match item.request {
                                PaymentRequest::Allocate { ref proposal_id, ref dvp_cid, .. } => {
                                    let result = execute_allocate(
                                        worker_channel.clone(), &worker_config, proposal_id, dvp_cid, &selected_cids,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                        &worker_cache,
                                    ).await;
                                    let _ = item.response_tx.send(PaymentResponse::Step(result));
                                }
                                PaymentRequest::PayFee { ref proposal_id, ref fee_type, .. } => {
                                    let result = execute_pay_fee(
                                        worker_channel.clone(), &worker_config, proposal_id, fee_type,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                    ).await;
                                    let _ = item.response_tx.send(PaymentResponse::Step(result));
                                }
                            }
                        }
                    ).await;

                    if worker_result.is_err() {
                        warn!("Payment worker timed out after 120s, releasing {} amulet reservations", cids_for_cleanup.len());
                        cache_for_cleanup.release_reservations(&cids_for_cleanup).await;
                    }

                    // Decrement per-pool active worker counter
                    worker_counter.fetch_sub(1, AtomicOrdering::Relaxed);
                });
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
                    _ = tokio::time::sleep(std::time::Duration::from_secs(SCHEDULER_BACKOFF_SECS)) => {}
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

/// Estimate CC needed for a payment (amount + margin)
fn estimate_cc_needed(request: &PaymentRequest) -> Decimal {
    let margin = Decimal::from_str(AMULET_SELECTION_MARGIN).unwrap_or(Decimal::ONE);
    match request {
        // Off-chain pay-fee: no on-chain CC transfer, no amulet selection.
        PaymentRequest::PayFee { .. } => Decimal::ZERO,
        // CC allocation: need the full allocation amount + margin for amulet pre-selection
        // CIP-56 allocation: no amulets needed
        PaymentRequest::Allocate { allocation_cc, .. } => {
            allocation_cc.map(|cc| cc + margin).unwrap_or(Decimal::ZERO)
        }
    }
}

/// Select amulets for CC allocations.
/// Prefers ONE amulet that covers the full amount (smallest-fit single).
/// Falls back to multiple amulets if no single one suffices.
/// Returns empty vec if insufficient amulets (scheduler will defer and retry).
pub(crate) fn select_amulets_for_allocation(selectable: &[CachedAmulet], estimated_cc: Decimal) -> Vec<CachedAmulet> {
    if estimated_cc <= Decimal::ZERO {
        return vec![];
    }

    // Add 2% margin to account for CC holding fee decay between selection and execution
    let target = estimated_cc * Decimal::new(102, 2); // 1.02

    // Prefer ONE amulet that covers the amount (smallest such amulet)
    // selectable is sorted ascending, so find first >= target
    for amulet in selectable {
        if amulet.amount >= target {
            return vec![amulet.clone()];
        }
    }

    // Multi-amulet fallback: take the LARGEST amulets first (selectable is sorted
    // ascending, so iterate in reverse). This maximises coverage with the 100-input
    // limit. Picking smallest-first would often produce a partial set whose total
    // is below the target, causing the on-chain transaction to fail with
    // ITR_InsufficientFunds.
    const MAX_INPUTS: usize = 100;
    let mut selected = Vec::new();
    let mut total = Decimal::ZERO;
    for amulet in selectable.iter().rev() {
        if selected.len() >= MAX_INPUTS {
            break;
        }
        selected.push(amulet.clone());
        total += amulet.amount;
        if total >= target {
            return selected;
        }
    }

    // 100 largest amulets still not enough — return empty so the scheduler defers.
    // Submitting a partial set would be guaranteed to fail on chain.
    vec![]
}

/// Process amulet cache updates after a successful transaction
pub(crate) async fn process_tx_result(
    cache: &Arc<AmuletCache>,
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
pub(crate) async fn handle_inactive_contracts(cache: &Arc<AmuletCache>, input_cids: &[String]) {
    if !input_cids.is_empty() {
        cache.mark_consumed(input_cids, "inactive").await;
    }
}

// ============================================================================
// Payment execution
// ============================================================================

/// Create a DAppProviderClient from a shared channel (no TCP+TLS overhead)
fn create_client_from_channel(channel: Channel, config: &BaseConfig) -> Result<DAppProviderClient> {
    DAppProviderClient::from_channel(
        channel,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
    )
}

async fn execute_pay_fee(
    channel: Channel,
    config: &BaseConfig,
    proposal_id: &str,
    fee_type: &str,
    _verbose: bool,
    dry_run: bool,
    _force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
) -> Result<StepResult> {
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

async fn execute_allocate(
    channel: Channel,
    config: &BaseConfig,
    proposal_id: &str,
    dvp_cid: &str,
    amulet_cids: &[String],
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
    cache: &Arc<AmuletCache>,
) -> Result<StepResult> {
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
                    amulet_cids: amulet_cids.to_vec(),
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await;

    match result {
        Ok(ref resp) => {
            process_tx_result(cache, amulet_cids, resp).await;
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
                handle_inactive_contracts(cache, amulet_cids).await;
            } else {
                cache.release_reservations(&amulet_cids.to_vec()).await;
            }
            Err(e)
        }
    }
}

// `execute_transfer_traffic_fee` removed: traffic billing is now handled
// off-chain by the ledger via the prepaid traffic pool (debited inside
// `execute.rs` after each successful Canton tx). The cloud-agent no longer
// pays sequencer traffic fees on-chain.
