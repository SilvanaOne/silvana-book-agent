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

use agent_logic::config::BaseConfig;
use agent_logic::confirm::ConfirmLock;
use agent_logic::liquidity::LiquidityManager;
use agent_logic::settlement::{DiscoveredContract, HoldingsHistogram, SettlementBackend, StepResult};
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AcceptDvpParams,
    PrepareTransactionRequest, ProposeDvpParams, TransactionOperation,
};

use tx_verifier::OperationExpectation;

use crate::acs_worker::spawn_acs_worker;
use crate::holdings_cache::{instrument_key, CcView, HoldingsCache, CC_INSTRUMENT};
use crate::ledger_client::DAppProviderClient;
use crate::payment_queue::PaymentQueue;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// USD price of a token from the shared mid-price map (same logic as
/// `RfqHandler::token_usd_price`): USDC*/USDCx = $1; else the mid of
/// `{token}-USDCx` or `{token}-USDC`. `None` when no market mid is loaded.
async fn token_usd_price(
    mid_prices: &RwLock<HashMap<String, f64>>,
    token: &str,
) -> Option<f64> {
    if token.starts_with("USDC") {
        return Some(1.0);
    }
    let mids = mid_prices.read().await;
    for stable in ["USDCx", "USDC"] {
        if let Some(&p) = mids.get(&format!("{token}-{stable}")) {
            if p > 0.0 {
                return Some(p);
            }
        }
    }
    None
}

/// Tally per-holding amounts into a `HoldingsHistogram` by USD value. Buckets
/// are half-open ([0,10), [10,20), [20,50), [50,100), [100,∞)). When
/// `usd_price` is None (no market mid), only total/reserved are populated and
/// `priced` stays false.
pub fn bucket_by_usd(
    total: usize,
    reserved: usize,
    holdings: &[(rust_decimal::Decimal, bool)],
    usd_price: Option<f64>,
) -> HoldingsHistogram {
    let mut h = HoldingsHistogram { total, reserved, ..Default::default() };
    let Some(price) = usd_price else {
        return h;
    };
    h.priced = true;
    for (amount, _reserved) in holdings {
        let usd = amount.to_f64().unwrap_or(0.0) * price;
        if usd < 10.0 {
            h.under_10 += 1;
        } else if usd < 20.0 {
            h.b10_20 += 1;
        } else if usd < 50.0 {
            h.b20_50 += 1;
        } else if usd < 100.0 {
            h.b50_100 += 1;
        } else {
            h.over_100 += 1;
        }
    }
    h
}

/// Settlement backend that uses DAppProviderService gRPC (no direct ledger access)
pub struct CloudSettlementBackend {
    config: BaseConfig,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    confirm_lock: ConfirmLock,
    /// Parallel payment queue with holdings cache
    payment_queue: PaymentQueue,
    /// Shared multi-instrument holdings cache (created by the caller so RFQ V2
    /// components can share it)
    holdings_cache: Arc<HoldingsCache>,
    /// CC view of the cache for heartbeat stats / multicall settlement
    amulet_cache: CcView,
    /// Liquidity manager for balance tracking and commitment gating
    liquidity_manager: Arc<LiquidityManager>,
    /// Shared market mid-prices (`market_id` → mid), owned by the RfqHandler.
    /// Used only to bucket the holdings histogram by USD in the LIQUIDITY log.
    /// `None` when RFQ V2 is disabled (no histogram then).
    mid_prices: Option<Arc<RwLock<HashMap<String, f64>>>>,
    /// Shared shutdown signal for background tasks (ACS / merge / payment queue).
    /// Cloned from the runner's `Shutdown` so Ctrl-C reaches every worker
    /// the moment it fires, not only after the main loop has exited.
    shutdown: Shutdown,
}

impl CloudSettlementBackend {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: BaseConfig,
        verbose: bool,
        dry_run: bool,
        force: bool,
        confirm: bool,
        confirm_lock: ConfirmLock,
        liquidity_manager: Arc<LiquidityManager>,
        shutdown: Shutdown,
        cache: Arc<HoldingsCache>,
    ) -> Self {
        // Spawn ACS worker to refresh the holdings cache and update liquidity manager
        spawn_acs_worker(config.clone(), cache.clone(), liquidity_manager.clone(), shutdown.clone());

        // Spawn merge worker if threshold is configured
        if config.merge_threshold.is_some() {
            crate::merge_worker::spawn_merge_worker(
                config.clone(), cache.cc(), shutdown.clone(),
            );
        }

        let payment_queue = PaymentQueue::new(
            config.clone(),
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock.clone(),
            cache.cc(),
            shutdown.clone(),
        );
        let amulet_cache = cache.cc();
        Self { config, verbose, dry_run, force, confirm, confirm_lock, payment_queue, holdings_cache: cache, amulet_cache, liquidity_manager, mid_prices: None, shutdown }
    }

    /// Wire the RfqHandler's shared mid-price map so the LIQUIDITY heartbeat can
    /// bucket the holdings histogram by USD. Call only when RFQ V2 is enabled.
    pub fn with_mid_prices(mut self, mid_prices: Arc<RwLock<HashMap<String, f64>>>) -> Self {
        self.mid_prices = Some(mid_prices);
        self
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
            Some(self.config.request_timeout_secs),
        )
        .await
    }

    /// Public accessor for the CC cache view (used by multicall settlement path).
    pub fn amulet_cache(&self) -> &CcView {
        &self.amulet_cache
    }

    /// Public accessor for the shared multi-instrument holdings cache.
    pub fn holdings_cache(&self) -> &Arc<HoldingsCache> {
        &self.holdings_cache
    }
}

#[async_trait]
impl SettlementBackend for CloudSettlementBackend {
    async fn pay_fee(&self, proposal_id: &str, fee_type: &str) -> Result<StepResult> {
        self.payment_queue.submit_pay_fee(proposal_id, fee_type).await
    }

    async fn propose_dvp(&self, proposal_id: &str) -> Result<StepResult> {
        if self.confirm && !self.dry_run {
            agent_logic::confirm::confirm_transaction(
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

    async fn accept_dvp(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        expected_delivery_amount: &str,
        expected_payment_amount: &str,
        base_instrument: &str,
        quote_instrument: &str,
    ) -> Result<StepResult> {
        if self.confirm && !self.dry_run {
            agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Accept DVP",
                &format!("proposal: {}, dvp_proposal: {}", proposal_id, dvp_proposal_cid),
            ).await?;
        }

        let mut client = self.create_client().await?;

        // Resolve instrument IDs and registries from local config (not RPC)
        let (del_id, del_admin) = self.config.resolve_instrument(base_instrument);
        let (pay_id, pay_admin) = self.config.resolve_instrument(quote_instrument);

        let expectation = OperationExpectation::AcceptDvp {
            seller_party: self.config.party_id.clone(),
            proposal_id: proposal_id.to_string(),
            dvp_proposal_cid: dvp_proposal_cid.to_string(),
            expected_delivery_amount: expected_delivery_amount.to_string(),
            expected_payment_amount: expected_payment_amount.to_string(),
            expected_delivery_instrument_id: del_id,
            expected_delivery_instrument_admin: del_admin,
            expected_payment_instrument_id: pay_id,
            expected_payment_instrument_admin: pay_admin,
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

    fn queue_depth(&self) -> (u64, u64) {
        self.payment_queue.queue_depth()
    }

    fn shutdown(&self) {
        self.shutdown.signal();
        self.payment_queue.shutdown();
    }

    fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        // Use block_in_place since stats() is async (holds read locks)
        Some(tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.amulet_cache.stats())
        }))
    }

    fn holdings_histogram(&self, token: &str) -> Option<HoldingsHistogram> {
        let mid_prices = self.mid_prices.clone()?;
        // Map the LiquidityManager token symbol to the HoldingsCache key: "CC"
        // for Amulet, else instrument_key(registry, on_chain_id).
        let cache_key = if token == CC_INSTRUMENT {
            CC_INSTRUMENT.to_string()
        } else {
            let (on_chain_id, registry) = self.config.resolve_instrument(token);
            if registry.is_empty() {
                return None;
            }
            instrument_key(&registry, &on_chain_id)
        };
        // block_in_place: the cache reads + price read are async (hold locks).
        let (total, reserved, holdings, usd_price) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let (t, r, hs) = self.holdings_cache.holdings_for_instrument(&cache_key).await;
                let price = token_usd_price(&mid_prices, token).await;
                (t, r, hs, price)
            })
        });
        Some(bucket_by_usd(total, reserved, &holdings, usd_price))
    }

    fn worker_utilization(&self) -> Option<(u64, usize, u64, usize)> {
        Some(self.payment_queue.worker_utilization())
    }

    fn fee_pause_secs(&self) -> Option<u64> {
        self.payment_queue.fee_pause_secs()
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

    fn liquidity_manager(&self) -> Option<Arc<LiquidityManager>> {
        Some(self.liquidity_manager.clone())
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::bucket_by_usd;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn h(amt: &str, reserved: bool) -> (Decimal, bool) {
        (Decimal::from_str(amt).unwrap(), reserved)
    }

    #[test]
    fn buckets_by_usd_value_half_open() {
        // USDC-like: price $1, amounts land directly in the USD buckets.
        let holdings = vec![
            h("9.99", false),  // <10
            h("10", false),    // 10-20 (lower bound inclusive)
            h("19.99", true),  // 10-20
            h("20", false),    // 20-50
            h("49.99", false), // 20-50
            h("50", false),    // 50-100
            h("100", false),   // >100 (100 is not <100)
        ];
        let hist = bucket_by_usd(holdings.len(), 1, &holdings, Some(1.0));
        assert!(hist.priced);
        assert_eq!(hist.total, 7);
        assert_eq!(hist.reserved, 1);
        assert_eq!(hist.under_10, 1);
        assert_eq!(hist.b10_20, 2);
        assert_eq!(hist.b20_50, 2);
        assert_eq!(hist.b50_100, 1);
        assert_eq!(hist.over_100, 1);
    }

    #[test]
    fn applies_the_price_multiplier() {
        // cETH-like: 0.012 cETH * $1874 = $22.5 -> 20-50 bucket.
        let holdings = vec![h("0.012", false), h("0.006", false)]; // $22.5, $11.2
        let hist = bucket_by_usd(2, 0, &holdings, Some(1874.0));
        assert_eq!(hist.b20_50, 1);
        assert_eq!(hist.b10_20, 1);
    }

    #[test]
    fn no_price_leaves_buckets_empty() {
        let holdings = vec![h("20", false), h("40", true)];
        let hist = bucket_by_usd(2, 1, &holdings, None);
        assert!(!hist.priced);
        assert_eq!(hist.total, 2);
        assert_eq!(hist.reserved, 1);
        assert_eq!(hist.under_10 + hist.b10_20 + hist.b20_50 + hist.b50_100 + hist.over_100, 0);
    }
}
