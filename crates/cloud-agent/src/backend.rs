//! Cloud settlement backend using DAppProviderService (CIP-0103)
//!
//! Implements `SettlementBackend` by calling the DAppProviderService gRPC proxy
//! instead of accessing Canton ledger directly.
//!
//! Allocations (amulet selection) and the off-chain processing fees go through a
//! PaymentQueue; propose_dvp and accept_dvp run concurrently.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::confirm::ConfirmLock;
use agent_logic::liquidity::LiquidityManager;
use agent_logic::settlement::{DiscoveredContract, HoldingsHistogram, SettlementBackend, StepResult};
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, AcceptDvpParams,
    PrepareTransactionRequest, ProposeDvpParams, TransactionOperation,
};

use orderbook_proto::orderbook::SettlementProposal;
use tx_verifier::{ExpectedDvpTerms, ExpectedQuantity, OperationExpectation};

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
    mid_prices: &RwLock<HashMap<String, agent_logic::pool_impact::MarketMid>>,
    token: &str,
) -> Option<f64> {
    if token.starts_with("USDC") {
        return Some(1.0);
    }
    let mids = mid_prices.read().await;
    for stable in ["USDCx", "USDC"] {
        if let Some(m) = mids.get(&format!("{token}-{stable}")) {
            if m.mid > 0.0 {
                return Some(m.mid);
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
        let bucket = if usd < 10.0 {
            &mut h.under_10
        } else if usd < 20.0 {
            &mut h.b10_20
        } else if usd < 50.0 {
            &mut h.b20_50
        } else if usd < 100.0 {
            &mut h.b50_100
        } else {
            &mut h.over_100
        };
        *bucket = bucket.saturating_add(1);
    }
    h
}

/// The DvP terms of the agent's own record of a match, each instrument resolved to its
/// on-chain id and registry admin as for AcceptDvp.
pub(crate) fn recorded_terms(config: &BaseConfig, proposal: &SettlementProposal) -> Result<ExpectedDvpTerms> {
    let (delivery_id, delivery_admin) = config.try_resolve_instrument(&proposal.base_instrument)?;
    let (payment_id, payment_admin) = config.try_resolve_instrument(&proposal.quote_instrument)?;
    Ok(ExpectedDvpTerms {
        proposal_id: proposal.proposal_id.clone(),
        buyer: proposal.buyer.clone(),
        seller: proposal.seller.clone(),
        delivery: ExpectedQuantity {
            instrument_admin: delivery_admin,
            instrument_id: delivery_id,
            amount: proposal.base_quantity.clone(),
        },
        payment: ExpectedQuantity {
            instrument_admin: payment_admin,
            instrument_id: payment_id,
            amount: proposal.quote_quantity.clone(),
        },
    })
}

/// [`recorded_terms`], or `None` with a warning: the verifier then only warns, and a
/// linked agent's content check refuses an allocation without them.
fn recorded_terms_or_warn(config: &BaseConfig, proposal: &SettlementProposal) -> Option<ExpectedDvpTerms> {
    recorded_terms(config, proposal)
        .map_err(|e| warn!("[{}] DvP terms not recorded for verification: {:#}", proposal.proposal_id, e))
        .ok()
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
    /// Shared market mids owned by the RfqHandler, used only to bucket the
    /// holdings histogram by USD. `None` when RFQ V2 is disabled.
    mid_prices: Option<Arc<RwLock<HashMap<String, agent_logic::pool_impact::MarketMid>>>>,
    /// Shared shutdown signal for background tasks (ACS / merge / payment queue).
    /// Cloned from the runner's `Shutdown` so Ctrl-C reaches every worker
    /// the moment it fires, not only after the main loop has exited.
    shutdown: Shutdown,
    /// Prepaid top-up nudge, fired after every settlement step.
    topup: Option<crate::topup::TopupTrigger>,
}

impl CloudSettlementBackend {
    /// Fails on invalid payment worker limits or outside a tokio runtime.
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
    ) -> Result<Self> {
        // Built first so invalid worker limits fail before any task starts
        let payment_queue = PaymentQueue::new(
            config.clone(),
            verbose,
            dry_run,
            force,
            confirm,
            confirm_lock.clone(),
            cache.cc(),
            shutdown.clone(),
        )?;

        // Spawn ACS worker to refresh the holdings cache and update liquidity manager
        spawn_acs_worker(config.clone(), cache.clone(), liquidity_manager.clone(), shutdown.clone())?;

        // NOTE: the merge worker is spawned by the caller (lib.rs), not here — it
        // needs the per-instrument SplitInstrument set (CC + the utility ladders),
        // which is only built after this constructor runs. The LP/RFQ-v2 path
        // spawns an all-instrument merge; the fill path spawns a CC-only merge.

        let amulet_cache = cache.cc();
        Ok(Self { config, verbose, dry_run, force, confirm, confirm_lock, payment_queue, holdings_cache: cache, amulet_cache, liquidity_manager, mid_prices: None, shutdown, topup: None })
    }

    /// Nudge the prepaid top-up runner after each settlement step, paid or refused.
    pub fn with_topup_trigger(mut self, trigger: crate::topup::TopupTrigger) -> Self {
        self.topup = Some(trigger);
        self
    }

    fn nudge_topup(&self) {
        if let Some(t) = &self.topup {
            t.nudge();
        }
    }

    async fn propose_dvp_inner(&self, proposal: &SettlementProposal) -> Result<StepResult> {
        let proposal_id = proposal.proposal_id.as_str();
        if self.confirm && !self.dry_run {
            agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Propose DVP",
                &format!("proposal: {}", proposal_id),
            ).await?;
        }

        let mut client = self.create_client().await?;

        let counterparty =
            if proposal.buyer == self.config.party_id { &proposal.seller } else { &proposal.buyer };
        let expectation = OperationExpectation::ProposeDvp {
            proposer: self.config.party_id.clone(),
            counterparty: counterparty.clone(),
            proposal_id: proposal_id.to_string(),
            synchronizer_id: self.config.synchronizer_id.clone(),
            terms: recorded_terms_or_warn(&self.config, proposal),
        };

        let result = client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::ProposeDvp as i32,
                    params: Some(Params::ProposeDvp(ProposeDvpParams {
                        proposal_id: proposal_id.to_string(),
                    })),
                    request_signature: None,
                    controller_command: None,
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
            fee_cc: None,
        })
    }

    async fn accept_dvp_inner(&self, proposal: &SettlementProposal, dvp_proposal_cid: &str) -> Result<StepResult> {
        let proposal_id = proposal.proposal_id.as_str();
        // Both legs need a registry admin from local config; the verifier refuses an
        // empty one, so nothing is prepared without them.
        let terms = recorded_terms(&self.config, proposal)?;

        if self.confirm && !self.dry_run {
            agent_logic::confirm::confirm_transaction(
                &self.confirm_lock,
                "Accept DVP",
                &format!("proposal: {}, dvp_proposal: {}", proposal_id, dvp_proposal_cid),
            ).await?;
        }

        let mut client = self.create_client().await?;

        let expectation = OperationExpectation::AcceptDvp {
            acceptor: self.config.party_id.clone(),
            proposal_id: proposal_id.to_string(),
            dvp_proposal_cid: dvp_proposal_cid.to_string(),
            expected_delivery_amount: terms.delivery.amount.clone(),
            expected_payment_amount: terms.payment.amount.clone(),
            expected_delivery_instrument_id: terms.delivery.instrument_id.clone(),
            expected_delivery_instrument_admin: terms.delivery.instrument_admin.clone(),
            expected_payment_instrument_id: terms.payment.instrument_id.clone(),
            expected_payment_instrument_admin: terms.payment.instrument_admin.clone(),
            terms: Some(terms),
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
                    controller_command: None,
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
            fee_cc: None,
        })
    }

    /// Wire the RfqHandler's shared mid-price map so the LIQUIDITY heartbeat can
    /// bucket the holdings histogram by USD. Call only when RFQ V2 is enabled.
    pub fn with_mid_prices(
        mut self,
        mid_prices: Arc<RwLock<HashMap<String, agent_logic::pool_impact::MarketMid>>>,
    ) -> Self {
        self.mid_prices = Some(mid_prices);
        self
    }

    /// Create a new DAppProviderClient for this request
    async fn create_client(&self) -> Result<DAppProviderClient> {
        DAppProviderClient::new(
            &self.config.orderbook_grpc_url,
            &self.config.party_id,
            &self.config.role,
            &self.config.private_key,
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
        let paid = self.payment_queue.submit_pay_fee(proposal_id, fee_type).await;
        self.nudge_topup();
        paid
    }

    async fn propose_dvp(&self, proposal: &SettlementProposal) -> Result<StepResult> {
        let proposed = self.propose_dvp_inner(proposal).await;
        self.nudge_topup();
        proposed
    }

    async fn accept_dvp(&self, proposal: &SettlementProposal, dvp_proposal_cid: &str) -> Result<StepResult> {
        let accepted = self.accept_dvp_inner(proposal, dvp_proposal_cid).await;
        self.nudge_topup();
        accepted
    }

    async fn allocate(
        &self,
        proposal: &SettlementProposal,
        dvp_cid: &str,
        allocation_cc: Option<Decimal>,
    ) -> Result<StepResult> {
        let terms = recorded_terms_or_warn(&self.config, proposal);
        let allocated = self.payment_queue.submit_allocate(&proposal.proposal_id, dvp_cid, allocation_cc, terms).await;
        self.nudge_topup();
        allocated
    }

    fn queue_depth(&self) -> (u64, u64) {
        self.payment_queue.queue_depth()
    }

    fn shutdown(&self) {
        self.shutdown.signal();
        self.payment_queue.shutdown();
    }

    async fn cache_stats(&self) -> Option<(usize, usize, usize, usize)> {
        Some(self.amulet_cache.stats().await)
    }

    async fn holdings_histogram(&self, token: &str) -> Option<HoldingsHistogram> {
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
        let (total, reserved, holdings) = self.holdings_cache.holdings_for_instrument(&cache_key).await;
        let usd_price = token_usd_price(&mid_prices, token).await;
        Some(bucket_by_usd(total, reserved, &holdings, usd_price))
    }

    fn worker_utilization(&self) -> Option<(u64, usize, u64, usize)> {
        Some(self.payment_queue.worker_utilization())
    }

    fn background_pause_secs(&self) -> Option<u64> {
        crate::ledger_client::background_pause_remaining()
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

#[cfg(test)]
mod runtime_tests {
    use super::*;

    // Heartbeat stats work on a current-thread runtime, where block_in_place panics
    #[tokio::test]
    async fn heartbeat_stats_work_on_a_current_thread_runtime() {
        let shutdown = Shutdown::new();
        let mids = Arc::new(RwLock::new(HashMap::from([(
            "CC-USDCx".to_string(),
            agent_logic::pool_impact::MarketMid { mid: 0.1, pool_depth: None },
        )])));
        let backend = CloudSettlementBackend::new(
            BaseConfig::test_minimal().unwrap(),
            false,
            false,
            false,
            false,
            agent_logic::confirm::new_confirm_lock(),
            LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0),
            shutdown.clone(),
            HoldingsCache::new(false),
        )
        .unwrap()
        .with_mid_prices(mids);
        assert_eq!(backend.cache_stats().await, Some((0, 0, 0, 0)));
        let histogram = backend.holdings_histogram(CC_INSTRUMENT).await.unwrap();
        assert!(histogram.priced);
        assert_eq!(histogram.total, 0);
        shutdown.signal();
    }

    fn proposal(base: &str, quote: &str) -> SettlementProposal {
        SettlementProposal {
            proposal_id: "p1".into(),
            buyer: "buyer::1220aa".into(),
            seller: "seller::1220bb".into(),
            base_instrument: base.into(),
            quote_instrument: quote.into(),
            base_quantity: "1".into(),
            quote_quantity: "2".into(),
            ..Default::default()
        }
    }

    // The terms of the agent's own record, resolved as AcceptDvp resolves its legs
    #[test]
    fn recorded_terms_follow_the_proposal() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.dso_party = "dso::1220aa".to_string();
        config.populate_instruments_from_rpc(vec![
            orderbook_proto::orderbook::Instrument {
                instrument_id: "CC".to_string(),
                instrument_type: "token".to_string(),
                symbol: "CC".to_string(),
                registry: Some("dso::1220aa".to_string()),
                ..Default::default()
            },
            orderbook_proto::orderbook::Instrument {
                instrument_id: "TOK".to_string(),
                symbol: "TOK".to_string(),
                registry: Some("reg::1220cc".to_string()),
                ..Default::default()
            },
        ]);
        let terms = recorded_terms(&config, &proposal("TOK", "CC")).unwrap();
        assert_eq!(
            terms,
            ExpectedDvpTerms {
                proposal_id: "p1".into(),
                buyer: "buyer::1220aa".into(),
                seller: "seller::1220bb".into(),
                delivery: ExpectedQuantity { instrument_admin: "reg::1220cc".into(), instrument_id: "TOK".into(), amount: "1".into() },
                payment: ExpectedQuantity { instrument_admin: "dso::1220aa".into(), instrument_id: "Amulet".into(), amount: "2".into() },
            }
        );
        assert!(recorded_terms(&config, &proposal("UNLISTED", "CC")).is_err());
        assert_eq!(recorded_terms_or_warn(&config, &proposal("CC", "UNLISTED")), None);
    }

    // Each settlement step nudges the prepaid top-up runner, paid or refused
    #[tokio::test]
    async fn settlement_steps_nudge_the_topup_runner() {
        let ledger = crate::test_util::FakeLedger::start(crate::test_util::Fake::Answer).await;
        let shutdown = Shutdown::new();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = ledger.url.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let backend = CloudSettlementBackend::new(
            config,
            false,
            false,
            false,
            false,
            agent_logic::confirm::new_confirm_lock(),
            LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0),
            shutdown.clone(),
            HoldingsCache::new(false),
        )
        .unwrap()
        .with_topup_trigger(crate::topup::TopupTrigger::from_sender(tx));
        let bounded = std::time::Duration::from_secs(10);
        assert!(tokio::time::timeout(bounded, backend.pay_fee("p1", "dvp")).await.unwrap().is_err());
        assert!(tokio::time::timeout(bounded, backend.propose_dvp(&proposal("UNLISTED", "CC"))).await.unwrap().is_err());
        let unlisted = proposal("UNLISTED", "CC");
        let refused = backend.accept_dvp(&unlisted, "00dvp");
        assert!(tokio::time::timeout(bounded, refused).await.unwrap().is_err());
        // No CC leg, so the allocation is not held back for coins
        let allocated = backend.allocate(&unlisted, "00dvp", None);
        assert!(tokio::time::timeout(bounded, allocated).await.unwrap().is_err());
        let mut nudges = 0;
        while rx.try_recv().is_ok() {
            nudges += 1;
        }
        assert_eq!(nudges, 4);
        shutdown.signal();
        ledger.stop().await;
    }

    // A leg the registry lacked used to be verified against an empty admin
    #[tokio::test]
    async fn accept_dvp_refuses_an_unresolvable_leg_before_preparing() {
        let ledger = crate::test_util::FakeLedger::start(crate::test_util::Fake::Answer).await;
        let shutdown = Shutdown::new();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = ledger.url.clone();
        config.dso_party = "dso::1220aa".to_string();
        config.populate_instruments_from_rpc(vec![orderbook_proto::orderbook::Instrument {
            instrument_id: "CC".to_string(),
            instrument_type: "token".to_string(),
            symbol: "CC".to_string(),
            registry: Some("dso::1220aa".to_string()),
            ..Default::default()
        }]);
        let backend = CloudSettlementBackend::new(
            config,
            false,
            false,
            true,
            false,
            agent_logic::confirm::new_confirm_lock(),
            LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0),
            shutdown.clone(),
            HoldingsCache::new(false),
        )
        .unwrap();
        for (base, quote) in [("UNLISTED", "CC"), ("CC", "UNLISTED")] {
            let err = backend
                .accept_dvp(&proposal(base, quote), "00dvp")
                .await
                .unwrap_err();
            let text = format!("{err:#}");
            assert!(
                text.starts_with(agent_logic::config::UNRESOLVED_INSTRUMENT)
                    && text.contains("'UNLISTED'"),
                "{text}"
            );
        }
        assert_eq!(
            ledger.calls("PrepareTransaction"),
            0,
            "nothing was prepared, even with --force"
        );
        shutdown.signal();
        ledger.stop().await;
    }
}

