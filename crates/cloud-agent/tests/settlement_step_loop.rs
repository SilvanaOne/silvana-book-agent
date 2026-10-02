use std::time::Duration;

use agent_logic::settlement::step_until_done;
use agent_logic::shutdown::Shutdown;
use agent_logic::types::{AdvanceResult, SettlementStage, SettlementState};
use orderbook_proto::orderbook::SettlementProposal;

// A step is awaited to its end however long it runs, and the next step sees its result
#[tokio::test(start_paused = true)]
async fn a_settlement_step_is_never_cut_short() {
    let shutdown = Shutdown::new();
    let mut calls = 0u32;
    let step = |state: SettlementState, _: bool| {
        calls += 1;
        let first = calls == 1;
        async move {
            if first {
                tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
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
    let proposal = SettlementProposal { proposal_id: "p1".to_string(), ..Default::default() };
    let state = SettlementState::new(proposal, false);
    let (result, state) = step_until_done("p1", state, &shutdown, step).await;
    assert_eq!(state.dvp_proposal_cid.as_deref(), Some("cid-from-the-slow-step"));
    let AdvanceResult::Error { error, .. } = result else { panic!("the second step's result") };
    assert_eq!(error, "next step saw Some(\"cid-from-the-slow-step\")");
    assert_eq!(calls, 2);
}
