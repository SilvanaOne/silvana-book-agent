//! Priority queue for serializing amulet (CC) operations
//!
//! All operations that touch amulet contracts (pay_fee, allocate, transfer_traffic_fee)
//! are submitted to this queue and executed one at a time by a background task.
//! This prevents LOCKED_CONTRACTS / INACTIVE_CONTRACTS errors from concurrent usage.
//!
//! Priority order: Allocate (High) > PayFee (Normal) > TransferTrafficFee (Low).
//! Within the same priority, operations are processed FIFO.
//!
//! Background fee payments (fire-and-forget) are queued via `queue_fee_background()`.
//! The processor handles them self-contained: executes the fee, records _Completed
//! event via RPC, and transfers traffic fee — all without a caller waiting.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot, Mutex};
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

use crate::ledger_client::DAppProviderClient;

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
    },
    Allocate {
        proposal_id: String,
        dvp_cid: String,
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

/// Serializes amulet-touching operations through a priority queue.
///
/// All calls to `submit_*` return only after the operation has been executed
/// by the single-threaded background processor.
///
/// Background fee payments (`queue_fee_background`) are fire-and-forget:
/// the processor executes the fee, records the _Completed event, and handles
/// traffic fee transfer — all without a caller waiting.
pub struct PaymentQueue {
    tx: mpsc::UnboundedSender<QueuedPayment>,
    sequence: AtomicU64,
    /// Pending background fees — tracked for state persistence on shutdown.
    /// Entries are added by `queue_fee_background()` and removed by the processor
    /// after successful completion.
    pending_background_fees: Arc<Mutex<Vec<PendingFee>>>,
    /// Pending traffic fees — tracked for state persistence on shutdown.
    /// Entries are added by `queue_traffic_fee()` and removed by the processor
    /// after successful completion.
    pending_traffic_fees: Arc<Mutex<Vec<PendingTrafficFee>>>,
    /// Queue depth counters — written by processor after each drain cycle
    queued_allocations: Arc<AtomicU64>,
    queued_fees: Arc<AtomicU64>,
    queued_traffic: Arc<AtomicU64>,
}

impl PaymentQueue {
    /// Create a new payment queue and spawn the background processor.
    pub fn new(
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(Vec::new()));
        let pending_traffic = Arc::new(Mutex::new(Vec::new()));
        let queued_allocations = Arc::new(AtomicU64::new(0));
        let queued_fees = Arc::new(AtomicU64::new(0));
        let queued_traffic = Arc::new(AtomicU64::new(0));
        let tx_for_processor = tx.clone();
        tokio::spawn(Self::processor(
            rx,
            tx_for_processor,
            config,
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock,
            pending.clone(),
            pending_traffic.clone(),
            queued_allocations.clone(),
            queued_fees.clone(),
            queued_traffic.clone(),
        ));
        Self {
            tx,
            sequence: AtomicU64::new(0),
            pending_background_fees: pending,
            pending_traffic_fees: pending_traffic,
            queued_allocations,
            queued_fees,
            queued_traffic,
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
            Ok(_) => Err(anyhow!("Unexpected response type for pay_fee")),
            Err(_) => Err(anyhow!("Payment processor dropped without responding")),
        }
    }

    /// Submit an allocate operation and await the result (highest priority).
    pub async fn submit_allocate(
        &self,
        proposal_id: &str,
        dvp_cid: &str,
    ) -> Result<StepResult> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(QueuedPayment {
                priority: PaymentPriority::High,
                sequence: self.next_sequence(),
                request: PaymentRequest::Allocate {
                    proposal_id: proposal_id.to_string(),
                    dvp_cid: dvp_cid.to_string(),
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
    /// The caller does NOT wait for the result — the processor handles everything.
    pub async fn queue_fee_background(&self, fee: PendingFee) {
        // Track for state persistence (must happen before sending to processor
        // to avoid race where processor completes before entry is in the list)
        self.pending_background_fees.lock().await.push(fee.clone());

        // Send to processor — drop the response receiver (fire-and-forget)
        let (response_tx, _response_rx) = oneshot::channel();
        let _ = self.tx.send(QueuedPayment {
            priority: PaymentPriority::Normal,
            sequence: self.next_sequence(),
            request: PaymentRequest::PayFeeBackground { fee },
            response_tx,
        });
    }

    /// Queue a traffic fee at lowest priority (fire-and-forget).
    /// The caller does NOT wait for the result.
    pub fn queue_traffic_fee(&self, traffic_bytes: u64, step_name: &str, proposal_id: &str) {
        if traffic_bytes == 0 {
            return;
        }
        // Track for state persistence (before sending to processor)
        {
            let pending = self.pending_traffic_fees.clone();
            let entry = PendingTrafficFee {
                traffic_bytes,
                step_name: step_name.to_string(),
                proposal_id: proposal_id.to_string(),
            };
            tokio::spawn(async move { pending.lock().await.push(entry) });
        }
        let (response_tx, _) = oneshot::channel(); // receiver dropped = fire-and-forget
        let _ = self.tx.send(QueuedPayment {
            priority: PaymentPriority::Low,
            sequence: self.next_sequence(),
            request: PaymentRequest::TransferTrafficFee {
                traffic_bytes,
                step_name: step_name.to_string(),
                proposal_id: proposal_id.to_string(),
            },
            response_tx,
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
    pub async fn get_pending_traffic_fees(&self) -> Vec<PendingTrafficFee> {
        self.pending_traffic_fees.lock().await.clone()
    }

    /// Restore pending traffic fee payments from saved state (re-queue on restart).
    pub fn restore_pending_traffic_fees(&self, fees: Vec<PendingTrafficFee>) {
        for fee in fees {
            info!("[{}] Restoring pending traffic fee from saved state (step={}, bytes={})",
                fee.proposal_id, fee.step_name, fee.traffic_bytes);
            self.queue_traffic_fee(fee.traffic_bytes, &fee.step_name, &fee.proposal_id);
        }
    }

    /// Get current queue depth: (allocations, fees, traffic).
    /// Values are snapshots updated by the processor after each drain cycle.
    pub fn queue_depth(&self) -> (u64, u64, u64) {
        (
            self.queued_allocations.load(AtomicOrdering::Relaxed),
            self.queued_fees.load(AtomicOrdering::Relaxed),
            self.queued_traffic.load(AtomicOrdering::Relaxed),
        )
    }

    /// Background processor: executes queued payments one at a time.
    async fn processor(
        mut rx: mpsc::UnboundedReceiver<QueuedPayment>,
        queue_tx: mpsc::UnboundedSender<QueuedPayment>,
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        pending_background_fees: Arc<Mutex<Vec<PendingFee>>>,
        pending_traffic_fees: Arc<Mutex<Vec<PendingTrafficFee>>>,
        queued_allocations: Arc<AtomicU64>,
        queued_fees: Arc<AtomicU64>,
        queued_traffic: Arc<AtomicU64>,
    ) {
        let mut heap: BinaryHeap<QueuedPayment> = BinaryHeap::new();
        let sequence = AtomicU64::new(u64::MAX / 2); // separate sequence space for processor-queued items

        loop {
            // If the heap is empty, block until at least one item arrives
            if heap.is_empty() {
                match rx.recv().await {
                    Some(item) => heap.push(item),
                    None => {
                        debug!("Payment queue channel closed, processor exiting");
                        return;
                    }
                }
            }

            // Non-blocking drain of any additional items that arrived
            while let Ok(item) = rx.try_recv() {
                heap.push(item);
            }

            // Update queue depth counters (after drain, before pop)
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

            // Pop the highest priority item and execute it
            if let Some(item) = heap.pop() {
                match item.request {
                    PaymentRequest::PayFeeBackground { fee } => {
                        let fee_type = fee.fee_type.clone();
                        let proposal_id = fee.proposal_id.clone();
                        let retry_count = fee.retry_count;
                        let (success, traffic) = execute_background_fee(
                            &config, fee.clone(), verbose, dry_run, force, confirm, &confirm_lock,
                            &pending_background_fees,
                        ).await;
                        // Queue traffic fee after fee payment
                        if traffic > 0 {
                            let step_name = format!("{}-fee", fee_type);
                            // Track for state persistence
                            pending_traffic_fees.lock().await.push(PendingTrafficFee {
                                traffic_bytes: traffic,
                                step_name: step_name.clone(),
                                proposal_id: proposal_id.clone(),
                            });
                            let (tx_resp, _) = oneshot::channel();
                            let _ = queue_tx.send(QueuedPayment {
                                priority: PaymentPriority::Low,
                                sequence: sequence.fetch_add(1, AtomicOrdering::Relaxed),
                                request: PaymentRequest::TransferTrafficFee {
                                    traffic_bytes: traffic,
                                    step_name,
                                    proposal_id: proposal_id.clone(),
                                },
                                response_tx: tx_resp,
                            });
                        }
                        // Retry failed background fees with delay
                        if !success && retry_count < 10 {
                            let mut retry_fee = fee;
                            retry_fee.retry_count += 1;
                            info!("[{}] Scheduling background {} fee retry #{} in 5min",
                                proposal_id, fee_type, retry_fee.retry_count);
                            let retry_tx = queue_tx.clone();
                            let seq = sequence.fetch_add(1, AtomicOrdering::Relaxed);
                            tokio::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                                let (response_tx, _) = oneshot::channel();
                                let _ = retry_tx.send(QueuedPayment {
                                    priority: PaymentPriority::Low,
                                    sequence: seq,
                                    request: PaymentRequest::PayFeeBackground { fee: retry_fee },
                                    response_tx,
                                });
                            });
                        } else if !success {
                            warn!("[{}] Background {} fee permanently failed after {} retries",
                                proposal_id, fee_type, retry_count);
                        }
                    }
                    PaymentRequest::Allocate { ref proposal_id, ref dvp_cid } => {
                        let pid = proposal_id.clone();
                        let cid = dvp_cid.clone();
                        let result = execute_allocate(
                            &config, &pid, &cid, verbose, dry_run, force, confirm, &confirm_lock,
                        ).await;
                        // Queue traffic fee after allocate
                        if let Ok(ref step_result) = result {
                            if step_result.traffic_total > 0 {
                                let step_name = format!("allocate-{}", pid);
                                // Track for state persistence
                                pending_traffic_fees.lock().await.push(PendingTrafficFee {
                                    traffic_bytes: step_result.traffic_total,
                                    step_name: step_name.clone(),
                                    proposal_id: pid.clone(),
                                });
                                let (tx_resp, _) = oneshot::channel();
                                let _ = queue_tx.send(QueuedPayment {
                                    priority: PaymentPriority::Low,
                                    sequence: sequence.fetch_add(1, AtomicOrdering::Relaxed),
                                    request: PaymentRequest::TransferTrafficFee {
                                        traffic_bytes: step_result.traffic_total,
                                        step_name,
                                        proposal_id: pid,
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
                            &config, traffic_bytes, &sn, &pid, verbose, dry_run, force, confirm, &confirm_lock,
                        ).await;
                        if result.is_ok() {
                            // Remove from pending list on success
                            let mut pending = pending_traffic_fees.lock().await;
                            pending.retain(|f| !(f.step_name == sn && f.proposal_id == pid));
                        }
                        let _ = item.response_tx.send(PaymentResponse::TrafficFee(result));
                    }
                    request => {
                        let response = execute_payment(
                            &config, request, verbose, dry_run, force, confirm, &confirm_lock,
                        ).await;
                        // Receiver may have been dropped (caller timed out) — ignore send error
                        let _ = item.response_tx.send(response);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Payment execution (extracted from CloudSettlementBackend)
// ============================================================================

/// Create a new DAppProviderClient from config
async fn create_client(config: &BaseConfig) -> Result<DAppProviderClient> {
    DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
    )
    .await
}

/// Execute a single payment operation (called by the processor).
async fn execute_payment(
    config: &BaseConfig,
    request: PaymentRequest,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
) -> PaymentResponse {
    match request {
        PaymentRequest::PayFee {
            proposal_id,
            fee_type,
        } => PaymentResponse::Step(
            execute_pay_fee(config, &proposal_id, &fee_type, verbose, dry_run, force, confirm, confirm_lock).await,
        ),
        PaymentRequest::Allocate {
            proposal_id,
            dvp_cid,
        } => PaymentResponse::Step(
            execute_allocate(config, &proposal_id, &dvp_cid, verbose, dry_run, force, confirm, confirm_lock).await,
        ),
        PaymentRequest::TransferTrafficFee {
            traffic_bytes,
            step_name,
            proposal_id,
        } => PaymentResponse::TrafficFee(
            execute_transfer_traffic_fee(config, traffic_bytes, &step_name, &proposal_id, verbose, dry_run, force, confirm, confirm_lock).await,
        ),
        PaymentRequest::PayFeeBackground { .. } => {
            // Should not reach here — handled separately in processor
            PaymentResponse::Step(Err(anyhow!("PayFeeBackground should not go through execute_payment")))
        }
    }
}

/// Execute a background fee payment: pay fee, record _Completed event.
/// Returns (success, traffic_bytes) — traffic fee is queued by the processor.
async fn execute_background_fee(
    config: &BaseConfig,
    fee: PendingFee,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
    pending_background_fees: &Arc<Mutex<Vec<PendingFee>>>,
) -> (bool, u64) {
    let proposal_id = &fee.proposal_id;
    let fee_type = &fee.fee_type;

    info!("[{}] Processing background {} fee (retry #{})", proposal_id, fee_type, fee.retry_count);

    // 0. On retries, check if operator already confirmed receipt before re-sending
    if fee.retry_count > 0 {
        if let Some(confirmed) = check_operator_confirmed_fee(config, proposal_id, fee_type, fee.is_buyer).await {
            if confirmed {
                info!("[{}] Background {} fee already confirmed by operator, skipping payment", proposal_id, fee_type);
                let mut pending = pending_background_fees.lock().await;
                pending.retain(|f| !(f.proposal_id == fee.proposal_id && f.fee_type == fee.fee_type));
                return (true, 0);
            }
        }
    }

    // 1. Execute the fee payment
    let result = execute_pay_fee(config, proposal_id, fee_type, verbose, dry_run, force, confirm, confirm_lock).await;

    match result {
        Ok(step_result) => {
            // 2. Record _Completed event via RPC
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

            // 3. Remove from pending list
            let mut pending = pending_background_fees.lock().await;
            pending.retain(|f| !(f.proposal_id == fee.proposal_id && f.fee_type == fee.fee_type));

            info!("[{}] Background {} fee completed", proposal_id, fee_type);
            (true, traffic)
        }
        Err(e) => {
            // Don't remove from pending — will be retried
            warn!("[{}] Background {} fee failed: {:#}", proposal_id, fee_type, e);
            (false, 0)
        }
    }
}

/// Record a _Completed settlement event via RPC (called by background fee processor).
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

    // Create RPC client for event recording
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
/// Returns Some(true) if confirmed (status == 4), Some(false) if not, None on RPC error.
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
        .map(|s| s.status == 4) // DVP_STEP_STATUS_CONFIRMED (operator witnessed)
        .unwrap_or(false);

    Some(confirmed)
}

async fn execute_pay_fee(
    config: &BaseConfig,
    proposal_id: &str,
    fee_type: &str,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
) -> Result<StepResult> {
    if confirm && !dry_run {
        confirm_transaction(
            confirm_lock,
            &format!("Pay {} fee", fee_type),
            &format!("proposal: {}", proposal_id),
        )
        .await?;
    }

    let mut client = create_client(config).await?;

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
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await?;

    let traffic = result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
    info!(
        "{} fee paid for {}: update_id={}, traffic={}",
        fee_type, proposal_id, result.update_id, traffic
    );

    Ok(StepResult {
        contract_id: result.contract_id.unwrap_or_default(),
        update_id: result.update_id,
        traffic_total: traffic,
    })
}

async fn execute_allocate(
    config: &BaseConfig,
    proposal_id: &str,
    dvp_cid: &str,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
) -> Result<StepResult> {
    if confirm && !dry_run {
        confirm_transaction(
            confirm_lock,
            "Allocate",
            &format!("proposal: {}, dvp: {}", proposal_id, dvp_cid),
        )
        .await?;
    }

    let mut client = create_client(config).await?;

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
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await?;

    let traffic = result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
    let cid = result.contract_id.clone().unwrap_or_default();
    info!(
        "Allocation complete for {}: CID={} (update: {}, traffic={})",
        proposal_id, cid, result.update_id, traffic
    );

    Ok(StepResult {
        contract_id: cid,
        update_id: result.update_id,
        traffic_total: traffic,
    })
}

async fn execute_transfer_traffic_fee(
    config: &BaseConfig,
    traffic_bytes: u64,
    step_name: &str,
    proposal_id: &str,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: &ConfirmLock,
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

    let mut client = create_client(config).await?;

    // Get CC/USD rate from server
    let rates = client.get_dso_rates().await?;
    let cc_usd_rate = Decimal::from_str(&rates.cc_usd_rate)
        .map_err(|e| anyhow!("Failed to parse cc_usd_rate: {}", e))?;

    if cc_usd_rate <= Decimal::ZERO {
        return Err(anyhow!("Invalid CC/USD rate: {}", cc_usd_rate));
    }

    let fee_usd = Decimal::from(traffic_bytes)
        * Decimal::from_f64_retain(config.traffic_fee_usd_per_byte).unwrap_or(Decimal::ZERO);
    let fee_cc = (fee_usd / cc_usd_rate).round_dp(10);

    if fee_cc <= Decimal::ZERO {
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

    client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::TransferCc as i32,
                params: Some(Params::TransferCc(TransferCcParams {
                    receiver_party: config.traffic_fee_party.clone(),
                    amount: fee_cc.to_string(),
                    description: Some(memo),
                    command_id,
                    settlement_proposal_id: Some(proposal_id.to_string()),
                })),
                request_signature: None,
            },
            &expectation,
            verbose,
            dry_run,
            force,
        )
        .await?;

    info!("[{}] Traffic fee paid: {} CC", proposal_id, fee_cc);

    Ok(())
}
