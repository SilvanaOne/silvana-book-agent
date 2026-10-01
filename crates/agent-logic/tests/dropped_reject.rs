//! A proposal reject cut off mid-flight must leave nothing that a later advance could accept.
//! Its own test binary: the settlement RPC channel is cached process-wide.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use agent_logic::config::BaseConfig;
use agent_logic::liquidity::LiquidityManager;
use agent_logic::order_tracker::OrderTracker;
use agent_logic::secret::Secret;
use agent_logic::settlement::{
    DiscoveredContract, SettlementBackend, SettlementExecutor, StepResult,
};
use anyhow::Result;
use orderbook_proto::orderbook::{
    OrderMatch, OrderType, SettlementProposal, SettlementUpdate, settlement_update::EventType,
};
use rust_decimal::Decimal;
use tokio::sync::Mutex;

/// Long enough for a fast-failing reject to finish; a black-holed one is cut off.
const PER_UPDATE: Duration = Duration::from_millis(300);

struct UnusedBackend;

#[async_trait::async_trait]
impl SettlementBackend for UnusedBackend {
    async fn pay_fee(&self, _: &str, _: &str) -> Result<StepResult> {
        Err(anyhow::anyhow!("unused"))
    }
    async fn propose_dvp(&self, _: &str) -> Result<StepResult> {
        Err(anyhow::anyhow!("unused"))
    }
    async fn accept_dvp(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<StepResult> {
        Err(anyhow::anyhow!("unused"))
    }
    async fn allocate(&self, _: &str, _: &str, _: Option<Decimal>) -> Result<StepResult> {
        Err(anyhow::anyhow!("unused"))
    }
    async fn sync_contracts(&self, _: &[String]) -> Result<Vec<DiscoveredContract>> {
        Ok(Vec::new())
    }
    fn queue_depth(&self) -> (u64, u64) {
        (0, 0)
    }
}

fn sale(id: &str) -> SettlementProposal {
    SettlementProposal {
        proposal_id: id.to_string(),
        market_id: "USDCx-CCY".to_string(),
        seller: "test-party".to_string(),
        buyer: "cp-x".to_string(),
        base_instrument: "USDCx".to_string(),
        base_quantity: "1000".to_string(),
        quote_instrument: "CCY".to_string(),
        quote_quantity: "500".to_string(),
        ..Default::default()
    }
}

fn created(proposal: SettlementProposal) -> SettlementUpdate {
    SettlementUpdate {
        event_type: EventType::ProposalCreated as i32,
        proposal: Some(proposal),
        ..Default::default()
    }
}

fn executor(config: &BaseConfig) -> SettlementExecutor<UnusedBackend> {
    let tracker = Arc::new(Mutex::new(OrderTracker::new(
        0,
        Secret::seal(&mut [0u8; 32]),
    )));
    SettlementExecutor::new(config, tracker, UnusedBackend)
}

/// Deliver the proposal twice through the stream path; each reject is dropped mid-flight.
async fn assert_dropped_reject_leaves_nothing(
    exec: &mut SettlementExecutor<UnusedBackend>,
    proposal: SettlementProposal,
) {
    let id = proposal.proposal_id.clone();
    for _ in 0..2 {
        let mut backlog = VecDeque::from([created(proposal.clone())]);
        let outcome = exec
            .apply_stream_batch(&mut backlog, PER_UPDATE, Duration::from_secs(5))
            .await;
        assert_eq!(outcome.handled, 1);
        assert_eq!(
            outcome.touched, 0,
            "{id}: the reject was still in flight when dropped"
        );
        assert!(
            !exec.active_settlements().contains_key(&id),
            "{id}: left active"
        );
        assert!(
            !exec.rejected_proposals().contains(&id),
            "{id}: a later delivery retries it"
        );
    }
}

#[tokio::test]
async fn dropped_reject_leaves_the_proposal_inactive_and_retryable() {
    // Takes connections but never answers, so every reject stays in flight
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = BaseConfig::test_minimal();
    config.orderbook_grpc_url = format!("http://{}", listener.local_addr().unwrap());

    // Per-counterparty cap
    let mut capped = config.clone();
    capped.max_pending_per_counterparty = 0;
    assert_dropped_reject_leaves_nothing(&mut executor(&capped), sale("p-cap")).await;

    // Not enough liquidity
    let mut exec = executor(&config);
    let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
    lm.update_cc_balance(Decimal::from(100)).await;
    lm.update_token_balance("USDCx", Decimal::from(10)).await;
    exec.set_liquidity_manager(lm);
    assert_dropped_reject_leaves_nothing(&mut exec, sale("p-liquidity")).await;

    // RFQ proposal the agent never quoted
    assert_dropped_reject_leaves_nothing(&mut executor(&config), sale("p-rfq")).await;

    // Tracked order that fails verification
    let mut proposal = sale("p-order");
    proposal.order_match = Some(OrderMatch {
        settlement_proposal_id: "p-order".to_string(),
        bid_order_id: 7,
        offer_order_id: 42,
        matched_quantity: "1000".to_string(),
        matched_price: "0.5".to_string(),
        created_at: None,
    });
    let tracker = Arc::new(Mutex::new(OrderTracker::new(
        0,
        Secret::seal(&mut [0u8; 32]),
    )));
    tracker.lock().await.track_order(
        42,
        "USDCx-CCY",
        OrderType::Offer as i32,
        "0.5",
        "1000",
        1,
        "bad",
        b"bad",
    );
    let mut exec = SettlementExecutor::new(&config, tracker, UnusedBackend);
    assert_dropped_reject_leaves_nothing(&mut exec, proposal).await;

    // Shutting down
    let mut exec = executor(&config);
    exec.set_shutting_down();
    let handled = tokio::time::timeout(
        PER_UPDATE,
        exec.handle_settlement_update(created(sale("p-stop"))),
    )
    .await;
    assert!(
        handled.is_err(),
        "the reject was still in flight when dropped"
    );
    assert!(!exec.active_settlements().contains_key("p-stop"));
    assert!(!exec.rejected_proposals().contains("p-stop"));
    drop(listener);
}
