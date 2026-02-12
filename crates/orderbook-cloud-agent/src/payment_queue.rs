//! Priority queue for serializing amulet (CC) operations
//!
//! All operations that touch amulet contracts (pay_fee, allocate, transfer_traffic_fee)
//! are submitted to this queue and executed one at a time by a background task.
//! This prevents LOCKED_CONTRACTS / INACTIVE_CONTRACTS errors from concurrent usage.
//!
//! Priority order: Allocate (High) > PayFee (Normal) > TransferTrafficFee (Low).
//! Within the same priority, operations are processed FIFO.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::confirm::{confirm_transaction, ConfirmLock};
use orderbook_agent_logic::settlement::StepResult;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AllocateParams, PayFeeParams,
    PrepareTransactionRequest, TransferCcParams, TransactionOperation,
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
pub struct PaymentQueue {
    tx: mpsc::UnboundedSender<QueuedPayment>,
    sequence: AtomicU64,
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
        tokio::spawn(Self::processor(
            rx,
            config,
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock,
        ));
        Self {
            tx,
            sequence: AtomicU64::new(0),
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

    /// Background processor: executes queued payments one at a time.
    async fn processor(
        mut rx: mpsc::UnboundedReceiver<QueuedPayment>,
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
    ) {
        let mut heap: BinaryHeap<QueuedPayment> = BinaryHeap::new();

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

            // Pop the highest priority item and execute it
            if let Some(item) = heap.pop() {
                let response = execute_payment(
                    &config,
                    item.request,
                    verbose,
                    dry_run,
                    force,
                    confirm,
                    &confirm_lock,
                )
                .await;
                // Receiver may have been dropped (caller timed out) — ignore send error
                let _ = item.response_tx.send(response);
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
    }
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
