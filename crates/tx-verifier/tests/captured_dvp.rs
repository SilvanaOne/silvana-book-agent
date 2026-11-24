//! Captured devnet records: DvP propose, accept and allocate.

mod common;

use common::Captured;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
use tx_verifier::{ExpectedDvpTerms, OperationExpectation};
use v2::interactive::daml_transaction::node::VersionedNode;
use v2::interactive::daml_transaction::Node;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::Create;
use v2::interactive::PreparedTransaction;
use v2::value::Sum;
use v2::Value;

const OTHER: &str = "other::1220eeeeeeee";

/// The terms the agent recorded for `proposal_id`, as the allocate records carry them.
fn recorded_terms(proposal_id: &str) -> ExpectedDvpTerms {
    common::load("allocate")
        .into_iter()
        .filter_map(|c| c.recorded_terms)
        .find(|t| t.proposal_id == proposal_id)
        .unwrap_or_else(|| panic!("no recorded terms for {proposal_id}"))
}

/// The record's expectation with the agent's recorded terms and, to allocate, the pinned operator.
fn with_terms(c: &Captured) -> OperationExpectation {
    let mut e = c.expectation.clone();
    match &mut e {
        OperationExpectation::Allocate { terms, settlement_operator, .. } => {
            *terms = c.recorded_terms.clone();
            *settlement_operator = Some(common::pins().settlement_operator);
        }
        OperationExpectation::ProposeDvp { proposal_id, terms, .. }
        | OperationExpectation::AcceptDvp { proposal_id, terms, .. } => {
            *terms = Some(recorded_terms(proposal_id));
        }
        other => panic!("{}: not a DvP expectation: {other:?}", c.fixture_id),
    }
    e
}

fn altered(e: &OperationExpectation, change: impl FnOnce(&mut ExpectedDvpTerms)) -> OperationExpectation {
    let mut e = e.clone();
    match &mut e {
        OperationExpectation::ProposeDvp { terms: Some(t), .. }
        | OperationExpectation::AcceptDvp { terms: Some(t), .. }
        | OperationExpectation::Allocate { terms: Some(t), .. } => change(t),
        other => panic!("no terms to alter: {other:?}"),
    }
    e
}

#[track_caller]
fn assert_clean(c: &Captured, e: &OperationExpectation) {
    let (r, observed) = common::inspect_bytes(&c.prepared, e);
    assert!(r.accepted, "{} ({}): refused: {:?}", c.fixture_id, c.source, r.rejection_reason);
    assert!(r.warnings.is_empty(), "{} ({}): warnings {:?}", c.fixture_id, c.source, r.warnings);
    assert!(observed.is_empty(), "{} ({}): enforce would refuse {:?}", c.fixture_id, c.source, observed);
}

/// Refused, or observed so that enforce refuses, with `needle` in the reason.
#[track_caller]
fn assert_flagged(bytes: &[u8], e: &OperationExpectation, needle: &str, what: &str) {
    let (r, observed) = common::inspect_bytes(bytes, e);
    let all = format!("{:?} {observed:?}", r.rejection_reason);
    assert!((!r.accepted || !observed.is_empty()) && all.contains(needle), "{what}: {all}");
}

fn nodes_mut(p: &mut PreparedTransaction) -> &mut Vec<Node> {
    &mut p.transaction.as_mut().expect("transaction").nodes
}

fn node_type(node: &Node) -> &NodeType {
    let VersionedNode::V1(n) = node.versioned_node.as_ref().expect("versioned node");
    n.node_type.as_ref().expect("node type")
}

fn node_type_mut(node: &mut Node) -> &mut NodeType {
    let VersionedNode::V1(n) = node.versioned_node.as_mut().expect("versioned node");
    n.node_type.as_mut().expect("node type")
}

fn is_allocation(c: &Create) -> bool {
    c.template_id.as_ref().is_some_and(|t| t.entity_name == "DvpLegAllocation")
}

/// The transaction's DvpLegAllocation create.
fn allocation_mut(p: &mut PreparedTransaction) -> &mut Create {
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| match node_type_mut(n) {
            NodeType::Create(c) if is_allocation(c) => Some(c),
            _ => None,
        })
        .expect("a DvpLegAllocation create")
}

/// The value at `path` inside a record value.
fn field_mut<'a>(mut v: &'a mut Value, path: &[&str]) -> &'a mut Value {
    for label in path {
        let Some(Sum::Record(r)) = v.sum.as_mut() else { panic!("{label}: not in a record") };
        v = r
            .fields
            .iter_mut()
            .find(|f| f.label == *label)
            .and_then(|f| f.value.as_mut())
            .unwrap_or_else(|| panic!("no field {label}"));
    }
    v
}

fn set_leg(p: &mut PreparedTransaction, path: &[&str], sum: Sum) {
    let arg = allocation_mut(p).argument.as_mut().expect("argument");
    field_mut(arg, path).sum = Some(sum);
}

/// A copy of the allocation create, next to it under the same parent.
fn add_second_allocation(p: &mut PreparedTransaction) {
    let nodes = nodes_mut(p);
    let original = nodes
        .iter()
        .find(|n| matches!(node_type(n), NodeType::Create(c) if is_allocation(c)))
        .expect("a DvpLegAllocation node")
        .clone();
    let mut copy = original.clone();
    copy.node_id = "copy-1".into();
    if let NodeType::Create(c) = node_type_mut(&mut copy) {
        c.contract_id = format!("00{}", "ab".repeat(32));
    }
    for n in nodes.iter_mut() {
        if let NodeType::Exercise(ex) = node_type_mut(n)
            && ex.children.contains(&original.node_id)
        {
            ex.children.push(copy.node_id.clone());
        }
    }
    nodes.push(copy);
}

/// Every settlement record's `settleBefore` set to `days` after its `requestedAt`.
fn stretch_settle_before(p: &mut PreparedTransaction, days: i64) {
    fn walk(v: &mut Value, days: i64) {
        let Some(Sum::Record(r)) = v.sum.as_mut() else { return };
        let requested = r.fields.iter().find(|f| f.label == "requestedAt").and_then(|f| match f.value.as_ref()?.sum {
            Some(Sum::Timestamp(t)) => Some(t),
            _ => None,
        });
        for f in r.fields.iter_mut() {
            let Some(value) = f.value.as_mut() else { continue };
            match requested {
                Some(t) if f.label == "settleBefore" => value.sum = Some(Sum::Timestamp(t + days * 86_400_000_000)),
                _ => walk(value, days),
            }
        }
    }
    for n in nodes_mut(p) {
        match node_type_mut(n) {
            NodeType::Create(c) => c.argument.iter_mut().for_each(|a| walk(a, days)),
            NodeType::Exercise(ex) => ex.chosen_value.iter_mut().for_each(|a| walk(a, days)),
            _ => {}
        }
    }
}

#[test]
fn an_allocation_settling_long_after_its_request_is_flagged() {
    for c in common::load("allocate") {
        let e = with_terms(&c);
        let mut p = common::prepared_transaction(&c);
        stretch_settle_before(&mut p, 365);
        let (r, observed) = common::inspect_bytes(&common::encode(&p), &e);
        assert!(r.accepted, "{}: {:?}", c.fixture_id, r.rejection_reason);
        let late: Vec<_> = observed.iter().filter(|o| o.contains("more than 72 h after the request")).collect();
        assert_eq!(late.len(), 1, "{}: {observed:?}", c.fixture_id);
    }
}

/// The accept record's transaction as a multicall that accepts and allocates it.
fn as_multicall(c: &Captured, terms: Option<ExpectedDvpTerms>) -> OperationExpectation {
    let OperationExpectation::AcceptDvp { acceptor, .. } = &c.expectation else { panic!("{}: not an accept", c.fixture_id) };
    OperationExpectation::ExecuteMulticall { party: acceptor.clone(), op_count: 1, terms }
}

#[test]
fn a_multicall_compares_its_dvp_terms_with_the_recorded_terms() {
    for c in common::load("accept_dvp") {
        let OperationExpectation::AcceptDvp { proposal_id, .. } = &c.expectation else { panic!("not an accept") };
        let recorded = recorded_terms(proposal_id);
        let (r, observed) = common::inspect_bytes(&c.prepared, &as_multicall(&c, Some(recorded.clone())));
        assert!(r.accepted && observed.is_empty(), "{}: {:?} {observed:?}", c.fixture_id, r.rejection_reason);
        let (_, unrecorded) = common::inspect_bytes(&c.prepared, &as_multicall(&c, None));
        assert!(unrecorded.is_empty(), "{}: {unrecorded:?}", c.fixture_id);
        let mut more = recorded.clone();
        more.payment.amount = format!("{}1", more.payment.amount);
        assert_flagged(&c.prepared, &as_multicall(&c, Some(more)), "not the recorded terms", &c.fixture_id);
        let mut other = recorded;
        other.proposal_id = "another-proposal".into();
        assert_flagged(&c.prepared, &as_multicall(&c, Some(other)), "not the recorded terms", &c.fixture_id);
    }
}

#[test]
fn propose_dvp_hash_and_facts() {
    common::assert_hash_and_facts("propose_dvp");
}

#[test]
fn propose_dvp_inspects_clean() {
    for c in common::load("propose_dvp") {
        assert_clean(&c, &with_terms(&c));
    }
}

#[test]
fn accept_dvp_hash_and_facts() {
    common::assert_hash_and_facts("accept_dvp");
}

#[test]
fn accept_dvp_inspects_clean() {
    for c in common::load("accept_dvp") {
        assert_clean(&c, &with_terms(&c));
    }
}

#[test]
fn allocate_hash_and_facts() {
    common::assert_hash_and_facts("allocate");
}

#[test]
fn allocate_inspects_clean() {
    for c in common::load("allocate") {
        assert!(c.recorded_terms.is_some(), "{}: no recorded terms", c.fixture_id);
        assert_clean(&c, &with_terms(&c));
    }
}

#[test]
fn allocate_facts_carry_the_allocation() {
    for c in common::load("allocate") {
        let terms = c.recorded_terms.clone().expect("recorded terms");
        let f = common::facts(&c);
        let [a] = f.allocations.as_slice() else { panic!("{}: allocations {:?}", c.fixture_id, f.allocations) };
        assert_eq!(a.settlement_ref_id, terms.proposal_id, "{}", c.fixture_id);
        assert_eq!(a.sender, c.party, "{}", c.fixture_id);
        assert_eq!(a.executor, common::pins().settlement_operator, "{}", c.fixture_id);
        let buyer = terms.buyer == c.party;
        assert_eq!(a.transfer_leg_id, if buyer { "2" } else { "1" }, "{}", c.fixture_id);
        assert!(a.allocate_before_micros > 0 && a.settle_before_micros > a.allocate_before_micros, "{a:?}");
    }
}

#[test]
fn dvp_records_without_terms_warn_and_are_never_refused_by_default() {
    for kind in ["propose_dvp", "accept_dvp", "allocate"] {
        for c in common::load(kind) {
            let (r, observed) = common::inspect_observing(&c);
            assert!(r.accepted, "{}: {:?}", c.fixture_id, r.rejection_reason);
            assert!(observed.is_empty(), "{}: {observed:?}", c.fixture_id);
            assert!(r.warnings.iter().any(|w| w.contains("no recorded DvP terms")), "{}: {:?}", c.fixture_id, r.warnings);
            let v = common::verify_default(&c);
            assert!(v.accepted, "{}: {:?}", c.fixture_id, v.rejection_reason);
        }
    }
}

#[test]
fn propose_and_accept_compare_the_recorded_terms() {
    for kind in ["propose_dvp", "accept_dvp"] {
        for c in common::load(kind) {
            let good = with_terms(&c);
            let cases = [
                ("buyer", altered(&good, |t| t.buyer = OTHER.into())),
                ("seller", altered(&good, |t| t.seller = OTHER.into())),
                ("delivery amount", altered(&good, |t| t.delivery.amount = "1".into())),
                ("payment amount", altered(&good, |t| t.payment.amount = "99999".into())),
                ("delivery instrument", altered(&good, |t| t.delivery.instrument_id = "OTHER".into())),
                ("payment admin", altered(&good, |t| t.payment.instrument_admin = OTHER.into())),
                ("terms id", altered(&good, |t| t.proposal_id = "another-proposal".into())),
            ];
            for (what, e) in cases {
                assert_flagged(&c.prepared, &e, "terms", &format!("{} {what}", c.fixture_id));
            }
        }
    }
    for c in common::load("propose_dvp") {
        let mut e = with_terms(&c);
        if let OperationExpectation::ProposeDvp { counterparty, .. } = &mut e {
            *counterparty = OTHER.into();
        }
        assert_flagged(&c.prepared, &e, "counterparty", &c.fixture_id);
    }
}

#[test]
fn allocate_mutations_are_flagged() {
    for c in common::load("allocate") {
        let e = with_terms(&c);
        let mutated = |f: &dyn Fn(&mut PreparedTransaction)| {
            let mut p = common::prepared_transaction(&c);
            f(&mut p);
            common::encode(&p)
        };
        let cases: [(&str, &str, Vec<u8>); 6] = [
            ("receiver", "leg", mutated(&|p| set_leg(p, &["allocation", "transferLeg", "receiver"], Sum::Party(OTHER.into())))),
            ("amount", "leg", mutated(&|p| set_leg(p, &["allocation", "transferLeg", "amount"], Sum::Numeric("1.0000000000".into())))),
            ("instrument", "leg", mutated(&|p| set_leg(p, &["allocation", "transferLeg", "instrumentId", "id"], Sum::Text("OTHER".into())))),
            ("settlementRef id", "settlementRef", mutated(&|p| set_leg(p, &["allocation", "settlement", "settlementRef", "id"], Sum::Text("another-proposal".into())))),
            ("second allocation", "allocation", mutated(&|p| add_second_allocation(p))),
            ("sender", "sender", mutated(&|p| set_leg(p, &["allocation", "transferLeg", "sender"], Sum::Party(OTHER.into())))),
        ];
        for (what, needle, bytes) in cases {
            assert_flagged(&bytes, &e, needle, &format!("{} {what}", c.fixture_id));
        }
        let mut other_operator = e.clone();
        if let OperationExpectation::Allocate { settlement_operator, .. } = &mut other_operator {
            *settlement_operator = Some(OTHER.into());
        }
        assert_flagged(&c.prepared, &other_operator, "executor", &c.fixture_id);
        let other_leg = altered(&e, |t| t.delivery.amount = "1".into());
        let other_leg = altered(&other_leg, |t| t.payment.amount = "1".into());
        assert_flagged(&c.prepared, &other_leg, "leg", &c.fixture_id);
    }
}
