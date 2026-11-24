//! The facts types the directed binding takes, named through this crate.
#![cfg(feature = "facts")]

use agent_control::tx_verifier::facts::{FactsError, TxFacts, extract_facts};
use agent_control::{BindingError, Coverage, OperationKind, PartyId, swap_intent, transfer_intent};

#[test]
fn the_binding_takes_the_facts_this_crate_re_exports() {
    let facts: TxFacts = TxFacts::default();
    let intent = transfer_intent(&facts, OperationKind::TransferCc, Coverage::Fields).unwrap();
    assert!(intent.outgoing.is_empty() && intent.instruments.is_empty());
    let agent = PartyId::parse(&format!("agent::1220{}", "ab".repeat(32))).unwrap();
    assert_eq!(
        swap_intent(&facts, &agent, Coverage::Fields),
        Err(BindingError::NoSettle)
    );
    let read = extract_facts(&[], agent.as_str());
    assert!(matches!(read, Err(FactsError::Malformed(_))), "{read:?}");
}
