//! Parallel payment queue with amulet reservation
//!
//! Replaces the old single-threaded processor with a scheduler + parallel workers.
//! The scheduler picks payments from a priority heap, selects smallest-fit amulets
//! from the AmuletCache, reserves them, and spawns workers that execute concurrently.
//!
//! Priority order: Allocate (High) > PayFee (Normal) > TransferTrafficFee (Low).
//! Within the same priority, operations are processed FIFO.
//!
//! Workers pass pre-selected amulet CIDs via the proto `amulet_cids` field.
//! On success, consumed amulets are marked in the cache and newly created amulets
//! (from change/split) are added. On INACTIVE_CONTRACTS, consumed amulets are
//! marked and the payment is re-queued for retry with fresh amulet selection.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tracing::{debug, info, warn};

use orderbook_agent_logic::auth::generate_jwt;
use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::confirm::{confirm_transaction, ConfirmLock};
use orderbook_agent_logic::rpc_client::OrderbookRpcClient;
use orderbook_agent_logic::settlement::{PendingFee, PendingTrafficFee, StepResult};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AllocateParams, PayFeeParams,
    PrepareTransactionRequest, TransferCcParams, TransactionOperation,
};
use orderbook_proto::{
    RecordSettlementEventRequest, SettlementEventType, SettlementEventResult, RecordedByRole,
};
use tx_verifier::OperationExpectation;

use tonic::transport::Channel;

use crate::amulet_cache::{AmuletCache, CachedAmulet};
use crate::ledger_client::DAppProviderClient;

/// Default max concurrent allocation workers (critical path — highest priority)
const DEFAULT_MAX_ALLOCATION_WORKERS: usize = 20;

/// Default max concurrent fee payment workers
const DEFAULT_MAX_FEE_WORKERS: usize = 5;

/// Default max concurrent traffic fee workers
const DEFAULT_MAX_TRAFFIC_WORKERS: usize = 5;

/// CC margin added to estimated amount for amulet selection (covers fees/rounding)
const AMULET_SELECTION_MARGIN: &str = "1.0";

/// Fee payments avoid amulets larger than this threshold.
/// Large amulets are preserved for allocations.
const FEE_AMULET_MAX_CC: &str = "100";

/// Delay when no payment can be processed (insufficient amulets)
const SCHEDULER_BACKOFF_SECS: u64 = 5;

/// Maximum traffic fees in the scheduler heap at once (rest stay in backlog)
const MAX_TRAFFIC_FEES_IN_QUEUE: usize = 10;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaymentPriority {
    /// Allocate — critical path, highest priority
    High = 0,
    /// PayFee (DVP / Alloc) — normal priority
    Normal = 1,
    /// TransferTrafficFee — lowest priority
    Low = 2,
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
        fee_cc_estimate: f64,
    },
    Allocate {
        proposal_id: String,
        dvp_cid: String,
        allocation_cc: Option<Decimal>,  // Some(amount) for CC, None for CIP-56
    },
    TransferTrafficFee {
        traffic_bytes: u64,
        step_name: String,
        proposal_id: String,
    },
    /// Background fee payment — processor handles completion event + traffic fee
    PayFeeBackground {
        fee: PendingFee,
    },
}

enum PaymentResponse {
    Step(Result<StepResult>),
    TrafficFee(Result<()>),
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
    /// Pending background fees — tracked for state persistence on shutdown.
    pending_background_fees: Arc<Mutex<Vec<PendingFee>>>,
    /// Pending traffic fees — tracked for state persistence on shutdown.
    pending_traffic_fees: Arc<Mutex<Vec<PendingTrafficFee>>>,
    /// Traffic fee backlog — throttled to max 10 in the scheduler heap at a time.
    traffic_fee_backlog: Arc<Mutex<VecDeque<PendingTrafficFee>>>,
    /// Queue depth counters
    queued_allocations: Arc<AtomicU64>,
    queued_fees: Arc<AtomicU64>,
    queued_traffic: Arc<AtomicU64>,
    /// Per-pool active worker counters (for heartbeat visibility)
    active_alloc_workers: Arc<AtomicU64>,
    active_fee_workers: Arc<AtomicU64>,
    active_traffic_workers: Arc<AtomicU64>,
    /// Per-pool max worker counts
    max_alloc_workers: usize,
    max_fee_workers: usize,
    max_traffic_workers: usize,
    /// Shutdown flag — stops the scheduler from dispatching new work
    shutdown_flag: Arc<AtomicBool>,
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
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(Vec::new()));
        let pending_traffic = Arc::new(Mutex::new(Vec::new()));
        let traffic_backlog = Arc::new(Mutex::new(VecDeque::new()));
        let queued_allocations = Arc::new(AtomicU64::new(0));
        let queued_fees = Arc::new(AtomicU64::new(0));
        let queued_traffic = Arc::new(AtomicU64::new(0));
        let active_alloc_workers = Arc::new(AtomicU64::new(0));
        let active_fee_workers = Arc::new(AtomicU64::new(0));
        let active_traffic_workers = Arc::new(AtomicU64::new(0));
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let tx_for_scheduler = tx.clone();

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
        let max_traffic_workers = legacy_override.unwrap_or_else(|| {
            std::env::var("MAX_TRAFFIC_WORKERS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_MAX_TRAFFIC_WORKERS)
        });

        tokio::spawn(Self::scheduler(
            rx,
            tx_for_scheduler,
            config,
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock,
            cache,
            pending.clone(),
            pending_traffic.clone(),
            traffic_backlog.clone(),
            queued_allocations.clone(),
            queued_fees.clone(),
            queued_traffic.clone(),
            max_alloc_workers,
            max_fee_workers,
            max_traffic_workers,
            active_alloc_workers.clone(),
            active_fee_workers.clone(),
            active_traffic_workers.clone(),
            shutdown_flag.clone(),
        ));

        Self {
            tx,
            sequence: AtomicU64::new(0),
            pending_background_fees: pending,
            pending_traffic_fees: pending_traffic,
            traffic_fee_backlog: traffic_backlog,
            queued_allocations,
            queued_fees,
            queued_traffic,
            active_alloc_workers,
            active_fee_workers,
            active_traffic_workers,
            max_alloc_workers,
            max_fee_workers,
            max_traffic_workers,
            shutdown_flag,
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
        fee_cc_estimate: f64,
    ) -> Result<StepResult> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(QueuedPayment {
                priority: PaymentPriority::Normal,
                sequence: self.next_sequence(),
                request: PaymentRequest::PayFee {
                    proposal_id: proposal_id.to_string(),
                    fee_type: fee_type.to_string(),
                    fee_cc_estimate,
                },
                response_tx,
            })
            .map_err(|_| anyhow!("Payment queue closed"))?;

        match response_rx.await {
            Ok(PaymentResponse::Step(r)) => r,
            Ok(_) => Err(anyhow!("Unexpected response type for pay_fee")),
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
            Ok(_) => Err(anyhow!("Unexpected response type for allocate")),
            Err(_) => Err(anyhow!("Payment processor dropped without responding")),
        }
    }

    /// Submit a transfer_traffic_fee operation and await the result (lowest priority).
    pub async fn submit_transfer_traffic_fee(
        &self,
        traffic_bytes: u64,
        step_name: &str,
        proposal_id: &str,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(QueuedPayment {
                priority: PaymentPriority::Low,
                sequence: self.next_sequence(),
                request: PaymentRequest::TransferTrafficFee {
                    traffic_bytes,
                    step_name: step_name.to_string(),
                    proposal_id: proposal_id.to_string(),
                },
                response_tx,
            })
            .map_err(|_| anyhow!("Payment queue closed"))?;

        match response_rx.await {
            Ok(PaymentResponse::TrafficFee(r)) => r,
            Ok(_) => Err(anyhow!("Unexpected response type for transfer_traffic_fee")),
            Err(_) => Err(anyhow!("Payment processor dropped without responding")),
        }
    }

    /// Queue a fee payment for background processing (fire-and-forget).
    pub async fn queue_fee_background(&self, fee: PendingFee) {
        self.pending_background_fees.lock().await.push(fee.clone());
        let (response_tx, _response_rx) = oneshot::channel();
        let _ = self.tx.send(QueuedPayment {
            priority: PaymentPriority::Normal,
            sequence: self.next_sequence(),
            request: PaymentRequest::PayFeeBackground { fee },
            response_tx,
        });
    }

    /// Queue a traffic fee via the backlog (throttled to max 10 in scheduler heap).
    pub fn queue_traffic_fee(&self, traffic_bytes: u64, step_name: &str, proposal_id: &str) {
        if traffic_bytes == 0 {
            return;
        }
        let entry = PendingTrafficFee {
            traffic_bytes,
            step_name: step_name.to_string(),
            proposal_id: proposal_id.to_string(),
        };
        let pending = self.pending_traffic_fees.clone();
        let backlog = self.traffic_fee_backlog.clone();
        tokio::spawn(async move {
            pending.lock().await.push(entry.clone());
            backlog.lock().await.push_back(entry);
        });
    }

    /// Get all pending background fee payments (for state persistence on shutdown).
    pub async fn get_pending_fees(&self) -> Vec<PendingFee> {
        self.pending_background_fees.lock().await.clone()
    }

    /// Restore pending fee payments from saved state (re-queue on restart).
    pub async fn restore_pending_fees(&self, fees: Vec<PendingFee>) {
        for fee in fees {
            info!("[{}] Restoring pending {} fee from saved state", fee.proposal_id, fee.fee_type);
            self.queue_fee_background(fee).await;
        }
    }

    /// Get all pending traffic fee payments (for state persistence on shutdown).
    /// Includes both in-flight fees and backlog items.
    pub async fn get_pending_traffic_fees(&self) -> Vec<PendingTrafficFee> {
        self.pending_traffic_fees.lock().await.clone()
    }

    /// Get current backlog depth for heartbeat logging.
    pub async fn traffic_backlog_depth(&self) -> usize {
        self.traffic_fee_backlog.lock().await.len()
    }

    /// Restore pending traffic fee payments from saved state (pushed to backlog, throttled).
    pub async fn restore_pending_traffic_fees(&self, fees: Vec<PendingTrafficFee>) {
        let count = fees.len();
        let mut backlog = self.traffic_fee_backlog.lock().await;
        let mut pending = self.pending_traffic_fees.lock().await;
        for fee in fees {
            pending.push(fee.clone());
            backlog.push_back(fee);
        }
        info!("Restored {} traffic fees to backlog (will process max {} at a time)", count, MAX_TRAFFIC_FEES_IN_QUEUE);
    }

    /// Get current queue depth: (allocations, fees, traffic).
    pub fn queue_depth(&self) -> (u64, u64, u64) {
        (
            self.queued_allocations.load(AtomicOrdering::Relaxed),
            self.queued_fees.load(AtomicOrdering::Relaxed),
            self.queued_traffic.load(AtomicOrdering::Relaxed),
        )
    }

    /// Signal the scheduler to stop dispatching new work and exit.
    pub fn shutdown(&self) {
        self.shutdown_flag.store(true, AtomicOrdering::Relaxed);
    }

    /// Get per-pool worker utilization:
    /// (alloc_active, alloc_max, fee_active, fee_max, traffic_active, traffic_max)
    pub fn worker_utilization(&self) -> (u64, usize, u64, usize, u64, usize) {
        (
            self.active_alloc_workers.load(AtomicOrdering::Relaxed),
            self.max_alloc_workers,
            self.active_fee_workers.load(AtomicOrdering::Relaxed),
            self.max_fee_workers,
            self.active_traffic_workers.load(AtomicOrdering::Relaxed),
            self.max_traffic_workers,
        )
    }

    /// Check if regular fees are paused due to sequencer backpressure.
    /// Returns Some(remaining_secs) if paused, None if not.
    pub fn fee_pause_secs(&self) -> Option<u64> {
        crate::ledger_client::fee_pause_remaining()
    }

    /// Check if traffic fees are paused due to sequencer backpressure.
    /// Returns Some(remaining_secs) if paused, None if not.
    pub fn traffic_fee_pause_secs(&self) -> Option<u64> {
        crate::ledger_client::traffic_fee_pause_remaining()
    }

    /// Scheduler: picks payments, selects amulets, reserves, spawns workers
    #[allow(clippy::too_many_arguments)]
    async fn scheduler(
        mut rx: mpsc::UnboundedReceiver<QueuedPayment>,
        queue_tx: mpsc::UnboundedSender<QueuedPayment>,
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        cache: Arc<AmuletCache>,
        pending_background_fees: Arc<Mutex<Vec<PendingFee>>>,
        pending_traffic_fees: Arc<Mutex<Vec<PendingTrafficFee>>>,
        traffic_fee_backlog: Arc<Mutex<VecDeque<PendingTrafficFee>>>,
        queued_allocations: Arc<AtomicU64>,
        queued_fees: Arc<AtomicU64>,
        queued_traffic: Arc<AtomicU64>,
        max_alloc_workers: usize,
        max_fee_workers: usize,
        max_traffic_workers: usize,
        active_alloc_workers: Arc<AtomicU64>,
        active_fee_workers: Arc<AtomicU64>,
        active_traffic_workers: Arc<AtomicU64>,
        shutdown_flag: Arc<AtomicBool>,
    ) {
        let alloc_semaphore = Arc::new(Semaphore::new(max_alloc_workers));
        let fee_semaphore = Arc::new(Semaphore::new(max_fee_workers));
        let traffic_semaphore = Arc::new(Semaphore::new(max_traffic_workers));
        let mut heap: BinaryHeap<QueuedPayment> = BinaryHeap::new();
        let sequence = AtomicU64::new(u64::MAX / 2);

        // Shared gRPC channel — created lazily on first dispatch, then reused
        // by all workers via HTTP/2 multiplexing (avoids per-worker TCP+TLS overhead)
        let mut shared_channel: Option<Channel> = None;

        info!(
            "Payment scheduler started: {} allocation, {} fee, {} traffic workers",
            max_alloc_workers, max_fee_workers, max_traffic_workers
        );

        loop {
            // Check shutdown before dispatching new work
            if shutdown_flag.load(AtomicOrdering::Relaxed) {
                info!("Payment scheduler shutting down ({} items in heap)", heap.len());
                return;
            }

            // If the heap is empty, check backlog or block for new items
            if heap.is_empty() {
                // First check if backlog has items to drain
                {
                    let mut backlog = traffic_fee_backlog.lock().await;
                    let to_drain = MAX_TRAFFIC_FEES_IN_QUEUE.min(backlog.len());
                    for _ in 0..to_drain {
                        if let Some(fee) = backlog.pop_front() {
                            let (response_tx, _) = oneshot::channel();
                            heap.push(QueuedPayment {
                                priority: PaymentPriority::Low,
                                sequence: sequence.fetch_add(1, AtomicOrdering::Relaxed),
                                request: PaymentRequest::TransferTrafficFee {
                                    traffic_bytes: fee.traffic_bytes,
                                    step_name: fee.step_name,
                                    proposal_id: fee.proposal_id,
                                },
                                response_tx,
                            });
                        }
                    }
                    if to_drain > 0 {
                        debug!("Drained {} traffic fees from backlog ({} remaining)", to_drain, backlog.len());
                    }
                }

                // If still empty, block until a new item arrives or shutdown
                if heap.is_empty() {
                    tokio::select! {
                        item = rx.recv() => match item {
                            Some(item) => heap.push(item),
                            None => {
                                debug!("Payment queue channel closed, scheduler exiting");
                                return;
                            }
                        },
                        _ = async {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                if shutdown_flag.load(AtomicOrdering::Relaxed) { break; }
                            }
                        } => {
                            info!("Payment scheduler shutting down");
                            return;
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
                let (mut alloc, mut fees, mut traffic) = (0u64, 0u64, 0u64);
                for item in heap.iter() {
                    match &item.request {
                        PaymentRequest::Allocate { .. } => alloc += 1,
                        PaymentRequest::PayFee { .. } | PaymentRequest::PayFeeBackground { .. } => fees += 1,
                        PaymentRequest::TransferTrafficFee { .. } => traffic += 1,
                    }
                }
                queued_allocations.store(alloc, AtomicOrdering::Relaxed);
                queued_fees.store(fees, AtomicOrdering::Relaxed);
                queued_traffic.store(traffic, AtomicOrdering::Relaxed);
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
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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

                // Skip regular fees while sequencer backpressure pause is active
                if item_priority == PaymentPriority::Normal
                    && crate::ledger_client::fee_pause_remaining().is_some()
                {
                    deferred.push(item);
                    continue;
                }

                // Skip regular fees when predicted coefficient is below overload threshold
                // — extremely heavy sequencer load, fee txs would hit SEQUENCER_BACKPRESSURE
                if item_priority == PaymentPriority::Normal
                    && orderbook_agent_logic::forecast::is_fees_paused_by_overload()
                {
                    deferred.push(item);
                    continue;
                }

                // Skip traffic fees while sequencer backpressure pause is active
                if item_priority == PaymentPriority::Low
                    && crate::ledger_client::traffic_fee_pause_remaining().is_some()
                {
                    deferred.push(item);
                    continue;
                }

                // Skip traffic fees while issuance forecast is LOW — heavy sequencer load
                // means traffic txs would likely hit SEQUENCER_BACKPRESSURE errors
                if item_priority == PaymentPriority::Low
                    && orderbook_agent_logic::forecast::is_traffic_paused_by_forecast()
                {
                    deferred.push(item);
                    continue;
                }

                // Pause traffic fees when CC is critically low — preserve amulets for
                // allocations and regular fees (critical path)
                if item_priority == PaymentPriority::Low {
                    let selectable_total: Decimal = cache.get_selectable_amulets().await
                        .iter()
                        .map(|a| a.amount)
                        .sum();
                    if selectable_total < Decimal::from_f64_retain(config.fee_reserve_cc).unwrap_or(Decimal::from(5)) {
                        debug!("Pausing traffic fee — critical CC shortage ({:.2} selectable)", selectable_total);
                        deferred.push(item);
                        continue;
                    }
                }

                // Estimate CC needed for this payment
                let estimated_cc = estimate_cc_needed(&item.request);

                // Get selectable amulets
                let selectable = cache.get_selectable_amulets().await;

                // Select amulets using operation-aware strategy
                let selected = match &item.request {
                    PaymentRequest::Allocate { allocation_cc: Some(_), .. } => {
                        select_amulets_for_allocation(&selectable, estimated_cc)
                    }
                    _ => {
                        select_amulets_for_fee(&selectable, estimated_cc)
                    }
                };

                if selected.is_empty() && estimated_cc > Decimal::ZERO {
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
                    PaymentRequest::PayFee { .. } | PaymentRequest::PayFeeBackground { .. } => fee_semaphore.clone(),
                    PaymentRequest::TransferTrafficFee { .. } => traffic_semaphore.clone(),
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
                    PaymentRequest::PayFee { .. } | PaymentRequest::PayFeeBackground { .. } => active_fee_workers.clone(),
                    PaymentRequest::TransferTrafficFee { .. } => active_traffic_workers.clone(),
                };
                worker_counter.fetch_add(1, AtomicOrdering::Relaxed);

                // Spawn worker with shared channel clone
                let worker_channel = shared_channel.clone().unwrap(); // safe: initialized above
                let worker_config = config.clone();
                let worker_cache = cache.clone();
                let worker_queue_tx = queue_tx.clone();
                let worker_pending_fees = pending_background_fees.clone();
                let worker_pending_traffic = pending_traffic_fees.clone();
                let worker_confirm_lock = confirm_lock.clone();
                let worker_sequence = sequence.fetch_add(1, AtomicOrdering::Relaxed);

                tokio::spawn(async move {
                    let _permit = permit; // hold until worker completes
                    let cids_for_cleanup = selected_cids.clone();
                    let cache_for_cleanup = worker_cache.clone();

                    let worker_result = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        async {
                            match item.request {
                                PaymentRequest::PayFeeBackground { fee } => {
                                    let fee_type = fee.fee_type.clone();
                                    let proposal_id = fee.proposal_id.clone();
                                    let retry_count = fee.retry_count;

                                    let (success, traffic) = execute_background_fee(
                                        worker_channel.clone(), &worker_config, fee.clone(), &selected_cids,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                        &worker_pending_fees, &worker_cache,
                                    ).await;

                                    // Queue traffic fee after fee payment
                                    if traffic > 0 {
                                        let step_name = format!("{}-fee", fee_type);
                                        worker_pending_traffic.lock().await.push(PendingTrafficFee {
                                            traffic_bytes: traffic,
                                            step_name: step_name.clone(),
                                            proposal_id: proposal_id.clone(),
                                        });
                                        let (tx_resp, _) = oneshot::channel();
                                        let _ = worker_queue_tx.send(QueuedPayment {
                                            priority: PaymentPriority::Low,
                                            sequence: worker_sequence,
                                            request: PaymentRequest::TransferTrafficFee {
                                                traffic_bytes: traffic,
                                                step_name,
                                                proposal_id: proposal_id.clone(),
                                            },
                                            response_tx: tx_resp,
                                        });
                                    }

                                    // Retry failed background fees
                                    if !success && retry_count < 10 {
                                        let mut retry_fee = fee;
                                        retry_fee.retry_count += 1;
                                        info!("[{}] Scheduling background {} fee retry #{} in 5min",
                                            proposal_id, fee_type, retry_fee.retry_count);
                                        let retry_tx = worker_queue_tx.clone();
                                        tokio::spawn(async move {
                                            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                                            let (response_tx, _) = oneshot::channel();
                                            let _ = retry_tx.send(QueuedPayment {
                                                priority: PaymentPriority::Normal,
                                                sequence: u64::MAX / 2,
                                                request: PaymentRequest::PayFeeBackground { fee: retry_fee },
                                                response_tx,
                                            });
                                        });
                                    } else if !success {
                                        warn!("[{}] Background {} fee permanently failed after {} retries",
                                            proposal_id, fee_type, retry_count);
                                    }
                                }
                                PaymentRequest::Allocate { ref proposal_id, ref dvp_cid, .. } => {
                                    let result = execute_allocate(
                                        worker_channel.clone(), &worker_config, proposal_id, dvp_cid, &selected_cids,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                        &worker_cache,
                                    ).await;

                                    if let Ok(ref step_result) = result {
                                        if step_result.traffic_total > 0 {
                                            let step_name = format!("allocate-{}", proposal_id);
                                            worker_pending_traffic.lock().await.push(PendingTrafficFee {
                                                traffic_bytes: step_result.traffic_total,
                                                step_name: step_name.clone(),
                                                proposal_id: proposal_id.clone(),
                                            });
                                            let (tx_resp, _) = oneshot::channel();
                                            let _ = worker_queue_tx.send(QueuedPayment {
                                                priority: PaymentPriority::Low,
                                                sequence: worker_sequence,
                                                request: PaymentRequest::TransferTrafficFee {
                                                    traffic_bytes: step_result.traffic_total,
                                                    step_name,
                                                    proposal_id: proposal_id.clone(),
                                                },
                                                response_tx: tx_resp,
                                            });
                                        }
                                    }
                                    let _ = item.response_tx.send(PaymentResponse::Step(result));
                                }
                                PaymentRequest::TransferTrafficFee { traffic_bytes, ref step_name, ref proposal_id } => {
                                    let sn = step_name.clone();
                                    let pid = proposal_id.clone();
                                    let result = execute_transfer_traffic_fee(
                                        worker_channel.clone(), &worker_config, traffic_bytes, &sn, &pid, &selected_cids,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                        &worker_cache,
                                    ).await;
                                    if result.is_ok() {
                                        let mut pending = worker_pending_traffic.lock().await;
                                        pending.retain(|f| !(f.step_name == sn && f.proposal_id == pid));
                                    }
                                    let _ = item.response_tx.send(PaymentResponse::TrafficFee(result));
                                }
                                PaymentRequest::PayFee { ref proposal_id, ref fee_type, .. } => {
                                    let result = execute_pay_fee(
                                        worker_channel.clone(), &worker_config, proposal_id, fee_type, &selected_cids,
                                        verbose, dry_run, force, confirm, &worker_confirm_lock,
                                        &worker_cache,
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

            // Drip-feed traffic fees from backlog (max 10 in heap at a time)
            // but NOT when allocations are pending — allocations must take priority
            {
                let has_pending_allocations = allocation_deferred
                    || heap.iter().any(|i| matches!(&i.request, PaymentRequest::Allocate { .. }));
                let traffic_in_heap = heap.iter()
                    .filter(|i| matches!(&i.request, PaymentRequest::TransferTrafficFee { .. }))
                    .count();
                let traffic_paused = crate::ledger_client::traffic_fee_pause_remaining().is_some()
                    || orderbook_agent_logic::forecast::is_traffic_paused_by_forecast();
                if !has_pending_allocations && !traffic_paused && traffic_in_heap < MAX_TRAFFIC_FEES_IN_QUEUE {
                    let mut backlog = traffic_fee_backlog.lock().await;
                    let to_drain = MAX_TRAFFIC_FEES_IN_QUEUE - traffic_in_heap;
                    let mut drained = 0;
                    while drained < to_drain {
                        if let Some(fee) = backlog.pop_front() {
                            let (response_tx, _) = oneshot::channel();
                            heap.push(QueuedPayment {
                                priority: PaymentPriority::Low,
                                sequence: sequence.fetch_add(1, AtomicOrdering::Relaxed),
                                request: PaymentRequest::TransferTrafficFee {
                                    traffic_bytes: fee.traffic_bytes,
                                    step_name: fee.step_name,
                                    proposal_id: fee.proposal_id,
                                },
                                response_tx,
                            });
                            drained += 1;
                        } else {
                            break;
                        }
                    }
                    if drained > 0 {
                        debug!("Drained {} traffic fees from backlog ({} remaining)", drained, backlog.len());
                    }
                }
            }

            // If nothing was dispatched and heap is non-empty, backoff before retrying
            if !dispatched_any && !heap.is_empty() {
                debug!(
                    "Scheduler: {} payments deferred (insufficient selectable amulets), backing off {}s",
                    heap.len(),
                    SCHEDULER_BACKOFF_SECS
                );
                // While backing off, also drain new items
                tokio::select! {
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
        PaymentRequest::PayFee { fee_cc_estimate, .. } => {
            let fee_cc = Decimal::from_f64_retain(*fee_cc_estimate).unwrap_or(Decimal::ZERO);
            if fee_cc > Decimal::ZERO { fee_cc + margin } else { margin }
        }
        PaymentRequest::PayFeeBackground { fee } => {
            let fee_cc = Decimal::from_f64_retain(fee.fee_cc_estimate).unwrap_or(Decimal::ZERO);
            if fee_cc > Decimal::ZERO { fee_cc + margin } else { margin }
        }
        // CC allocation: need the full allocation amount + margin for amulet pre-selection
        // CIP-56 allocation: no amulets needed
        PaymentRequest::Allocate { allocation_cc, .. } => {
            allocation_cc.map(|cc| cc + margin).unwrap_or(Decimal::ZERO)
        }
        // Traffic fees are typically tiny
        PaymentRequest::TransferTrafficFee { .. } => margin,
    }
}

/// Select amulets for fee/traffic payments.
/// Prefers small amulets (<= FEE_AMULET_MAX_CC). Only uses large amulets as last resort.
/// Returns empty vec if no amulets available at all.
fn select_amulets_for_fee(selectable: &[CachedAmulet], estimated_cc: Decimal) -> Vec<CachedAmulet> {
    if estimated_cc <= Decimal::ZERO {
        return vec![];
    }
    let max_amulet_cc = Decimal::from_str(FEE_AMULET_MAX_CC).unwrap_or(Decimal::from(100));

    // First pass: only use amulets <= 100 CC (sorted ascending, smallest-fit)
    let small: Vec<_> = selectable.iter().filter(|a| a.amount <= max_amulet_cc).collect();
    let mut selected = Vec::new();
    let mut total = Decimal::ZERO;
    for amulet in &small {
        selected.push((*amulet).clone());
        total += amulet.amount;
        if total >= estimated_cc {
            return selected;
        }
    }

    // Fallback: no small amulets cover it — use smallest large amulet
    // (sorted ascending, so first amulet > max is the smallest large one)
    for amulet in selectable {
        if amulet.amount > max_amulet_cc {
            return vec![amulet.clone()];
        }
    }

    // Nothing available
    vec![]
}

/// Select amulets for CC allocations.
/// Prefers ONE amulet that covers the full amount (smallest-fit single).
/// Falls back to multiple amulets if no single one suffices.
/// Returns empty vec if insufficient amulets (scheduler will defer and retry).
fn select_amulets_for_allocation(selectable: &[CachedAmulet], estimated_cc: Decimal) -> Vec<CachedAmulet> {
    if estimated_cc <= Decimal::ZERO {
        return vec![];
    }

    // Prefer ONE amulet that covers the amount (smallest such amulet)
    // selectable is sorted ascending, so find first >= estimated_cc
    for amulet in selectable {
        if amulet.amount >= estimated_cc {
            return vec![amulet.clone()];
        }
    }

    // No single amulet suffices — use smallest-fit with multiple
    let mut selected = Vec::new();
    let mut total = Decimal::ZERO;
    for amulet in selectable {
        selected.push(amulet.clone());
        total += amulet.amount;
        if total >= estimated_cc {
            return selected;
        }
    }

    // Not enough even with all amulets — return empty to defer.
    // Scheduler will retry on next cycle (waiting for change from other operations).
    vec![]
}

/// Process amulet cache updates after a successful transaction
async fn process_tx_result(
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
async fn handle_inactive_contracts(cache: &Arc<AmuletCache>, input_cids: &[String]) {
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

/// Execute a background fee payment: pay fee, record _Completed event.
/// Returns (success, traffic_bytes).
async fn execute_background_fee(
    channel: Channel,
    config: &BaseConfig,
    fee: PendingFee,
    amulet_cids: &[String],
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
    pending_background_fees: &Arc<Mutex<Vec<PendingFee>>>,
    cache: &Arc<AmuletCache>,
) -> (bool, u64) {
    let proposal_id = &fee.proposal_id;
    let fee_type = &fee.fee_type;

    info!("[{}] Processing background {} fee (retry #{})", proposal_id, fee_type, fee.retry_count);

    // On retries, check if operator already confirmed receipt
    if fee.retry_count > 0 {
        if let Some(confirmed) = check_operator_confirmed_fee(config, proposal_id, fee_type, fee.is_buyer).await {
            if confirmed {
                info!("[{}] Background {} fee already confirmed by operator, skipping", proposal_id, fee_type);
                let mut pending = pending_background_fees.lock().await;
                pending.retain(|f| !(f.proposal_id == fee.proposal_id && f.fee_type == fee.fee_type));
                // Release reservations since we're not executing
                cache.release_reservations(&amulet_cids.to_vec()).await;
                return (true, 0);
            }
        }
    }

    let result = execute_pay_fee(channel, config, proposal_id, fee_type, amulet_cids, verbose, dry_run, force, confirm, confirm_lock, cache).await;

    match result {
        Ok(step_result) => {
            // Record _Completed event
            let completed_event = if fee_type == "dvp" {
                if fee.is_buyer {
                    SettlementEventType::DvpProcessingFeeBuyerCompleted
                } else {
                    SettlementEventType::DvpProcessingFeeSellerCompleted
                }
            } else {
                if fee.is_buyer {
                    SettlementEventType::AllocationProcessingFeeBuyerCompleted
                } else {
                    SettlementEventType::AllocationProcessingFeeSellerCompleted
                }
            };

            record_completed_event(
                config, proposal_id, fee.is_buyer, completed_event,
                &step_result.update_id, &step_result.contract_id,
            ).await;

            let traffic = step_result.traffic_total;

            // Remove from pending list
            let mut pending = pending_background_fees.lock().await;
            pending.retain(|f| !(f.proposal_id == fee.proposal_id && f.fee_type == fee.fee_type));

            info!("[{}] Background {} fee completed (update_id={})", proposal_id, fee_type, step_result.update_id);
            (true, traffic)
        }
        Err(e) => {
            warn!("[{}] Background {} fee failed: {:#}", proposal_id, fee_type, e);
            // Amulet cache is already handled by execute_pay_fee on error
            (false, 0)
        }
    }
}

/// Record a _Completed settlement event via RPC.
async fn record_completed_event(
    config: &BaseConfig,
    proposal_id: &str,
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
        recorded_by: config.party_id.clone(),
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

    let jwt = match generate_jwt(
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(&config.node_name),
    ) {
        Ok(token) => token,
        Err(e) => {
            warn!("[{}] Failed to generate JWT for event recording: {}", proposal_id, e);
            return;
        }
    };

    let mut rpc_client = match OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await {
        Ok(client) => client,
        Err(e) => {
            warn!("[{}] Failed to create RPC client for event recording: {}", proposal_id, e);
            return;
        }
    };

    match rpc_client.record_settlement_event(request).await {
        Ok(event_id) => {
            debug!("[{}] Recorded background fee completed event {:?} (event_id={})", proposal_id, event_type, event_id);
        }
        Err(e) => {
            warn!("[{}] Failed to record background fee completed event {:?}: {}", proposal_id, event_type, e);
        }
    }
}

/// Check if the operator has already confirmed receipt of a fee payment.
async fn check_operator_confirmed_fee(
    config: &BaseConfig,
    proposal_id: &str,
    fee_type: &str,
    is_buyer: bool,
) -> Option<bool> {
    let jwt = generate_jwt(
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(&config.node_name),
    ).ok()?;

    let mut rpc_client = OrderbookRpcClient::connect(&config.orderbook_grpc_url, Some(jwt)).await.ok()?;
    let status = rpc_client.get_settlement_status(proposal_id).await.ok()?;

    let step = match (fee_type, is_buyer) {
        ("dvp", true) => status.dvp_processing_fee_buyer,
        ("dvp", false) => status.dvp_processing_fee_seller,
        ("allocate", true) => status.allocation_processing_fee_buyer,
        ("allocate", false) => status.allocation_processing_fee_seller,
        _ => None,
    };

    let confirmed = step
        .map(|s| s.status == 4) // DVP_STEP_STATUS_CONFIRMED
        .unwrap_or(false);

    Some(confirmed)
}

async fn execute_pay_fee(
    channel: Channel,
    config: &BaseConfig,
    proposal_id: &str,
    fee_type: &str,
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
            &format!("Pay {} fee", fee_type),
            &format!("proposal: {}", proposal_id),
        )
        .await?;
    }

    let mut client = create_client_from_channel(channel, config)?;

    let operation = if fee_type == "dvp" {
        TransactionOperation::PayDvpFee
    } else {
        TransactionOperation::PayAllocFee
    };

    let expectation = OperationExpectation::PayFee {
        sender_party: config.party_id.clone(),
        fee_party: config.fee_party.clone(),
        proposal_id: proposal_id.to_string(),
        fee_type: fee_type.to_string(),
    };

    let result = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: operation as i32,
                params: Some(Params::PayFee(PayFeeParams {
                    proposal_id: proposal_id.to_string(),
                    fee_type: fee_type.to_string(),
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
            info!("{} fee paid for {}: update_id={}, traffic={}", fee_type, proposal_id, resp.update_id, traffic);
            Ok(StepResult {
                contract_id: resp.contract_id.clone().unwrap_or_default(),
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

async fn execute_transfer_traffic_fee(
    channel: Channel,
    config: &BaseConfig,
    traffic_bytes: u64,
    step_name: &str,
    proposal_id: &str,
    amulet_cids: &[String],
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
    cache: &Arc<AmuletCache>,
) -> Result<()> {
    if confirm && !dry_run {
        confirm_transaction(
            confirm_lock,
            "Transfer traffic fee",
            &format!(
                "proposal: {}, step: {}, bytes: {}",
                proposal_id, step_name, traffic_bytes
            ),
        )
        .await?;
    }

    let mut client = create_client_from_channel(channel, config)?;

    // Get CC/USD rate from server
    let rates = client.get_dso_rates().await?;
    let cc_usd_rate = Decimal::from_str(&rates.cc_usd_rate)
        .map_err(|e| anyhow!("Failed to parse cc_usd_rate: {}", e))?;

    if cc_usd_rate <= Decimal::ZERO {
        cache.release_reservations(&amulet_cids.to_vec()).await;
        return Err(anyhow!("Invalid CC/USD rate: {}", cc_usd_rate));
    }

    let fee_usd = Decimal::from(traffic_bytes)
        * Decimal::from_f64_retain(config.traffic_fee_usd_per_byte).unwrap_or(Decimal::ZERO);
    let fee_cc = (fee_usd / cc_usd_rate).round_dp(10);

    if fee_cc <= Decimal::ZERO {
        cache.release_reservations(&amulet_cids.to_vec()).await;
        return Ok(());
    }

    let command_id = format!("traffic-fee-{}-{}", step_name, proposal_id);
    let memo = format!("Traffic fee for {} bytes", traffic_bytes);

    info!(
        "[{}] Traffic fee: {:.4} CC ({:.4} USD) for {} bytes",
        proposal_id, fee_cc, fee_usd, traffic_bytes
    );

    let expectation = OperationExpectation::TransferCc {
        sender_party: config.party_id.clone(),
        receiver_party: config.traffic_fee_party.clone(),
        amount: fee_cc.to_string(),
        command_id: command_id.clone(),
    };

    let result = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::TransferCc as i32,
                params: Some(Params::TransferCc(TransferCcParams {
                    receiver_party: config.traffic_fee_party.clone(),
                    amount: fee_cc.to_string(),
                    description: Some(memo),
                    command_id,
                    settlement_proposal_id: Some(proposal_id.to_string()),
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
            info!("[{}] Traffic fee paid: {} CC (update_id={})", proposal_id, fee_cc, resp.update_id);
            Ok(())
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
