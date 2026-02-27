//! Cloud settlement backend using DAppProviderService (CIP-0103)
//!
//! Implements `SettlementBackend` by calling the DAppProviderService gRPC proxy
//! instead of accessing Canton ledger directly.
//!
//! All amulet-touching operations (pay_fee, allocate, transfer_traffic_fee) are
//! serialized through a PaymentQueue to avoid LOCKED_CONTRACTS race conditions.
//! Operations that don't use amulets (propose_dvp, accept_dvp) run concurrently.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use tracing::{debug, info};

use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::confirm::ConfirmLock;
use orderbook_agent_logic::settlement::{DiscoveredContract, PendingFee, PendingTrafficFee, SettlementBackend, StepResult};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AcceptDvpParams,
    PrepareTransactionRequest, ProposeDvpParams, TransactionOperation,
};

use tx_verifier::OperationExpectation;

use crate::amulet_cache::AmuletCache;
use crate::acs_worker::spawn_acs_worker;
use crate::ledger_client::DAppProviderClient;
use crate::payment_queue::PaymentQueue;

/// Settlement backend that uses DAppProviderService gRPC (no direct ledger access)
pub struct CloudSettlementBackend {
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: ConfirmLock,
    /// Parallel payment queue with amulet cache
    payment_queue: PaymentQueue,
    /// Shared amulet cache for heartbeat stats
    amulet_cache: Arc<AmuletCache>,
}

impl CloudSettlementBackend {
    pub fn new(config: BaseConfig, verbose: bool, dry_run: bool, force: bool, confirm: bool, confirm_lock: ConfirmLock) -> Self {
        let cache = AmuletCache::new();

        // Spawn ACS worker to refresh amulet cache periodically
        spawn_acs_worker(config.clone(), cache.clone());

        let payment_queue = PaymentQueue::new(
            config.clone(),
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock.clone(),
            cache.clone(),
        );
        Self { config, verbose, dry_run, force, confirm, confirm_lock, payment_queue, amulet_cache: cache }
    }

    /// Create a new DAppProviderClient for this request
    async fn create_client(&self) -> Result<DAppProviderClient> {
        DAppProviderClient::new(
            &self.config.orderbook_grpc_url,
            &self.config.party_id,
            &self.config.role,
            &self.config.private_key_bytes,
            self.config.token_ttl_secs,
            Some(self.config.node_name.as_str()),
            &self.config.ledger_service_public_key,
            Some(self.config.connection_timeout_secs),
        )
        .await
    }
}

#[async_trait]
impl SettlementBackend for CloudSettlementBackend {
    async fn pay_fee(&self, proposal_id: &str, fee_type: &str) -> Result<StepResult> {
        self.payment_queue.submit_pay_fee(proposal_id, fee_type, 0.0).await
    }

    async fn propose_dvp(&self, proposal_id: &str) -> Result<StepResult> {
        if self.confirm && !self.dry_run {
            orderbook_agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Propose DVP",
                &format!("proposal: {}", proposal_id),
            ).await?;
        }

        let mut client = self.create_client().await?;

        let expectation = OperationExpectation::ProposeDvp {
            buyer_party: self.config.party_id.clone(),
            seller_party: String::new(), // not known at this level
            proposal_id: proposal_id.to_string(),
            synchronizer_id: self.config.synchronizer_id.clone(),
        };

        let result = client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::ProposeDvp as i32,
                    params: Some(Params::ProposeDvp(ProposeDvpParams {
                        proposal_id: proposal_id.to_string(),
                    })),
                    request_signature: None,
                },
                &expectation,
                self.verbose,
                self.dry_run,
                self.force,
            )
            .await?;

        let traffic = result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
        let cid = result.contract_id.clone().unwrap_or_default();
        info!("Created DvpProposal for {}: CID={}", proposal_id, cid);

        Ok(StepResult {
            contract_id: cid,
            update_id: result.update_id,
            traffic_total: traffic,
        })
    }

    async fn accept_dvp(&self, proposal_id: &str, dvp_proposal_cid: &str) -> Result<StepResult> {
        if self.confirm && !self.dry_run {
            orderbook_agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Accept DVP",
                &format!("proposal: {}, dvp_proposal: {}", proposal_id, dvp_proposal_cid),
            ).await?;
        }

        let mut client = self.create_client().await?;

        let expectation = OperationExpectation::AcceptDvp {
            seller_party: self.config.party_id.clone(),
            proposal_id: proposal_id.to_string(),
            dvp_proposal_cid: dvp_proposal_cid.to_string(),
        };

        let result = client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::AcceptDvp as i32,
                    params: Some(Params::AcceptDvp(AcceptDvpParams {
                        proposal_id: proposal_id.to_string(),
                        dvp_proposal_cid: dvp_proposal_cid.to_string(),
                    })),
                    request_signature: None,
                },
                &expectation,
                self.verbose,
                self.dry_run,
                self.force,
            )
            .await?;

        let traffic = result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
        let cid = result.contract_id.clone().unwrap_or_default();
        info!("Accepted DVP for {}: CID={}", proposal_id, cid);

        Ok(StepResult {
            contract_id: cid,
            update_id: result.update_id,
            traffic_total: traffic,
        })
    }

    async fn allocate(&self, proposal_id: &str, dvp_cid: &str, allocation_cc: Option<Decimal>) -> Result<StepResult> {
        self.payment_queue.submit_allocate(proposal_id, dvp_cid, allocation_cc).await
    }

    async fn transfer_traffic_fee(
        &self,
        traffic_bytes: u64,
        step_name: &str,
        proposal_id: &str,
    ) -> Result<()> {
        if traffic_bytes == 0 {
            return Ok(());
        }
        self.payment_queue
            .submit_transfer_traffic_fee(traffic_bytes, step_name, proposal_id)
            .await
    }

    fn queue_traffic_fee(&self, traffic_bytes: u64, step_name: &str, proposal_id: &str) {
        self.payment_queue.queue_traffic_fee(traffic_bytes, step_name, proposal_id);
    }

    async fn queue_fee_payment(&self, mut fee: PendingFee) {
        // Compute fee CC estimate from USD amount using current CC/USD rate
        if !fee.fee_amount_usd.is_empty() {
            if let Ok(fee_usd) = fee.fee_amount_usd.parse::<f64>() {
                if fee_usd > 0.0 {
                    if let Ok(mut client) = self.create_client().await {
                        if let Ok(rates) = client.get_dso_rates().await {
                            if let Ok(rate) = rates.cc_usd_rate.parse::<f64>() {
                                if rate > 0.0 {
                                    fee.fee_cc_estimate = fee_usd / rate;
                                    debug!(
                                        "[{}] Fee CC estimate: {} USD / {} rate = {} CC",
                                        fee.proposal_id, fee_usd, rate, fee.fee_cc_estimate
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        self.payment_queue.queue_fee_background(fee).await;
    }

    fn get_pending_fees(&self) -> Vec<PendingFee> {
        // Block on the async lock — this is called during shutdown (sync context)
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.payment_queue.get_pending_fees())
        })
    }

    async fn restore_pending_fees(&self, fees: Vec<PendingFee>) {
        self.payment_queue.restore_pending_fees(fees).await;
    }

    fn get_pending_traffic_fees(&self) -> Vec<PendingTrafficFee> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.payment_queue.get_pending_traffic_fees())
        })
    }

    fn restore_pending_traffic_fees(&self, fees: Vec<PendingTrafficFee>) {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.payment_queue.restore_pending_traffic_fees(fees))
        });
    }

    fn queue_depth(&self) -> (u64, u64, u64) {
        self.payment_queue.queue_depth()
    }

    fn shutdown(&self) {
        self.payment_queue.shutdown();
    }

    fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        // Use block_in_place since stats() is async (holds read locks)
        Some(tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.amulet_cache.stats())
        }))
    }

    fn traffic_backlog_depth(&self) -> usize {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.payment_queue.traffic_backlog_depth())
        })
    }

    fn worker_utilization(&self) -> Option<(u64, usize, u64, usize, u64, usize)> {
        Some(self.payment_queue.worker_utilization())
    }

    fn fee_pause_secs(&self) -> Option<u64> {
        self.payment_queue.fee_pause_secs()
    }

    fn traffic_fee_pause_secs(&self) -> Option<u64> {
        self.payment_queue.traffic_fee_pause_secs()
    }

    async fn sync_contracts(
        &self,
        settlement_ids: &[String],
    ) -> Result<Vec<DiscoveredContract>> {
        if settlement_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut client = self.create_client().await?;
        let contracts = client.get_settlement_contracts(settlement_ids).await?;

        debug!(
            "sync_contracts: found {} contracts for {} settlements",
            contracts.len(),
            settlement_ids.len()
        );

        Ok(contracts
            .into_iter()
            .map(|c| DiscoveredContract {
                settlement_id: c.settlement_id,
                contract_id: c.contract_id,
                contract_type: c.contract_type,
            })
            .collect())
    }
}
