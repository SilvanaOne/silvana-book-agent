//! Captured devnet records: atomic DvP settlements.

mod common;

use common::Captured;
use prost::Message;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
use tx_verifier::{ExpectedFee, OperationExpectation, VerifyMode, WouldRefuse};
use v2::interactive::daml_transaction::node::VersionedNode;
use v2::interactive::daml_transaction::Node;
use v2::interactive::metadata::input_contract::Contract as InputContract;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::{Create, Exercise};
use v2::interactive::PreparedTransaction;
use v2::value::Sum;
use v2::Value;

const OTHER: &str = "other::1220eeeeeeee";

/// The fee fields of the prepare request, read with its field numbers.
#[derive(Clone, PartialEq, Message)]
struct AtomicRequest {
    #[prost(message, optional, tag = "10")]
    settle: Option<SettleParams>,
}

#[derive(Clone, PartialEq, Message)]
struct SettleParams {
    #[prost(message, repeated, tag = "18")]
    lp_fees: Vec<FeeSpec>,
}

#[derive(Clone, PartialEq, Message)]
struct FeeSpec {
    #[prost(string, tag = "1")]
    receiver: String,
    #[prost(string, tag = "2")]
    instrument_admin: String,
    #[prost(string, tag = "3")]
    instrument_id: String,
    #[prost(string, tag = "4")]
    amount: String,
}

/// The record's expectation with the request's signed fees and, unless `unpinned`, the fee pin.
fn settle_expectation(c: &Captured, unpinned: bool) -> OperationExpectation {
    let request = AtomicRequest::decode(c.request.as_slice()).expect("atomic request");
    let fees = request.settle.expect("settle params").lp_fees;
    assert!(!fees.is_empty(), "{}: no signed fee", c.fixture_id);
    let mut e = c.expectation.clone();
    let OperationExpectation::AtomicDvpSettle { expected_fees, fee_party, .. } = &mut e else {
        panic!("{}: not a settle", c.fixture_id)
    };
    *expected_fees = fees
        .into_iter()
        .map(|f| ExpectedFee {
            receiver: f.receiver,
            instrument_admin: f.instrument_admin,
            instrument_id: f.instrument_id,
            amount: f.amount,
        })
        .collect();
    *fee_party = (!unpinned).then(|| common::pins().party_fee);
    e
}

#[track_caller]
fn assert_flagged(bytes: &[u8], e: &OperationExpectation, needle: &str, what: &str) {
    let (r, observed) = common::inspect_bytes(bytes, e);
    let all = format!("{:?} {observed:?}", r.rejection_reason);
    assert!((!r.accepted || !observed.is_empty()) && all.contains(needle), "{what}: {all}");
}

fn nodes_mut(p: &mut PreparedTransaction) -> &mut Vec<Node> {
    &mut p.transaction.as_mut().expect("transaction").nodes
}

fn node_type_mut(node: &mut Node) -> &mut NodeType {
    let VersionedNode::V1(n) = node.versioned_node.as_mut().expect("versioned node");
    n.node_type.as_mut().expect("node type")
}

fn root_id(p: &PreparedTransaction) -> String {
    p.transaction.as_ref().and_then(|t| t.roots.first()).cloned().expect("a root")
}

fn settle_mut(p: &mut PreparedTransaction) -> &mut Exercise {
    let root = root_id(p);
    let node = nodes_mut(p).iter_mut().find(|n| n.node_id == root).expect("root node");
    match node_type_mut(node) {
        NodeType::Exercise(ex) => ex,
        _ => panic!("the root is not an exercise"),
    }
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

/// The settle's `fees` list.
fn fees_mut(p: &mut PreparedTransaction) -> &mut Vec<Value> {
    let chosen = settle_mut(p).chosen_value.as_mut().expect("chosen value");
    let Some(Sum::Optional(o)) = field_mut(chosen, &["fees"]).sum.as_mut() else { panic!("fees is not optional") };
    let Some(Sum::List(l)) = o.value.as_mut().and_then(|v| v.sum.as_mut()) else { panic!("fees is not a list") };
    &mut l.elements
}

fn venue_mut<'a>(p: &'a mut PreparedTransaction, venue_cid: &str) -> &'a mut Create {
    p.metadata
        .as_mut()
        .expect("metadata")
        .input_contracts
        .iter_mut()
        .find_map(|ic| match ic.contract.as_mut() {
            Some(InputContract::V1(c)) if c.contract_id == venue_cid => Some(c),
            _ => None,
        })
        .expect("the venue input")
}

/// A copy of the Amulet created for `owner`, owned by `new_owner`, with a fresh id.
fn amulet_copy(p: &PreparedTransaction, owner: &str, new_owner: &str, n: u8) -> Node {
    let owned_by = |c: &Create| {
        c.template_id.as_ref().is_some_and(|t| t.entity_name == "Amulet" && t.module_name == "Splice.Amulet")
            && c.argument.as_ref().is_some_and(|a| format!("{a:?}").contains(owner))
    };
    let mut node = p
        .transaction
        .as_ref()
        .expect("transaction")
        .nodes
        .iter()
        .find(|n| {
            let Some(VersionedNode::V1(v1)) = n.versioned_node.as_ref() else { return false };
            matches!(v1.node_type.as_ref(), Some(NodeType::Create(c)) if owned_by(c))
        })
        .expect("an Amulet for the owner")
        .clone();
    node.node_id = format!("copy-{n}");
    if let NodeType::Create(c) = node_type_mut(&mut node) {
        c.contract_id = format!("00{}", format!("{n:02x}").repeat(32));
        let arg = c.argument.as_mut().expect("argument");
        field_mut(arg, &["owner"]).sum = Some(Sum::Party(new_owner.into()));
    }
    node
}

/// The created contract `cid`, handed to `new_owner` in place.
fn reown(p: &mut PreparedTransaction, cid: &str, new_owner: &str) {
    let node = nodes_mut(p)
        .iter_mut()
        .find(|n| {
            let Some(VersionedNode::V1(v1)) = n.versioned_node.as_ref() else { return false };
            matches!(v1.node_type.as_ref(), Some(NodeType::Create(c)) if c.contract_id == cid)
        })
        .expect("the created contract");
    if let NodeType::Create(c) = node_type_mut(node) {
        let arg = c.argument.as_mut().expect("argument");
        field_mut(arg, &["owner"]).sum = Some(Sum::Party(new_owner.into()));
    }
}

#[test]
fn atomic_dvp_settle_hash_and_facts() {
    common::assert_hash_and_facts("atomic_dvp_settle");
}

#[test]
fn atomic_dvp_settle_inspects_clean() {
    for c in common::load("atomic_dvp_settle") {
        let (r, observed) = common::inspect_bytes(&c.prepared, &settle_expectation(&c, false));
        assert!(r.accepted, "{} ({}): refused: {:?}", c.fixture_id, c.source, r.rejection_reason);
        assert!(!r.summary.starts_with("[STUB]"), "{}: {}", c.fixture_id, r.summary);
        assert!(r.warnings.is_empty(), "{} ({}): warnings {:?}", c.fixture_id, c.source, r.warnings);
        assert!(observed.is_empty(), "{} ({}): enforce would refuse {:?}", c.fixture_id, c.source, observed);
    }
}

#[test]
fn an_unpinned_fee_party_is_a_warning_never_a_refusal() {
    for c in common::load("atomic_dvp_settle") {
        let e = settle_expectation(&c, true);
        let (r, observed) = common::inspect_bytes(&c.prepared, &e);
        assert!(r.accepted && observed.is_empty(), "{}: {:?} {observed:?}", c.fixture_id, r.rejection_reason);
        assert!(r.warnings.iter().any(|w| w.contains("PARTY_FEE")), "{}: {:?}", c.fixture_id, r.warnings);
        // Observed or enforced, the missing pin alone is no inspector refusal.
        for strict_all in [false, true] {
            let mode = VerifyMode { strict_all, ..VerifyMode::default() };
            let v = tx_verifier::verify_and_hash_with(&c.prepared, &c.server_hash_base64, &c.hashing_scheme_version, &e, mode, false)
                .expect("verification");
            assert!(v.accepted, "{} strict_all={strict_all}: {:?}", c.fixture_id, v.rejection_reason);
            assert!(!v.would_refuse.iter().any(|w| matches!(w, WouldRefuse::Inspector(_))), "{}: {:?}", c.fixture_id, v.would_refuse);
            assert!(v.warnings.iter().any(|w| w.contains("PARTY_FEE")), "{}: {:?}", c.fixture_id, v.warnings);
        }
        let v = common::verify_default(&c);
        assert!(v.accepted, "{}: {:?}", c.fixture_id, v.rejection_reason);
    }
}

#[test]
fn atomic_settle_mutations_are_flagged() {
    let pins = common::pins();
    for c in common::load("atomic_dvp_settle") {
        let e = settle_expectation(&c, false);
        let OperationExpectation::AtomicDvpSettle { venue_cid, .. } = &e else { panic!("not a settle") };
        let mutated = |f: &dyn Fn(&mut PreparedTransaction)| {
            let mut p = common::prepared_transaction(&c);
            f(&mut p);
            common::encode(&p)
        };
        let fee_field = |path: &'static [&'static str], sum: Sum| {
            move |p: &mut PreparedTransaction| {
                let fee = fees_mut(p).first_mut().expect("a fee");
                field_mut(fee, path).sum = Some(sum.clone());
            }
        };
        let cases: [(&str, &str, Vec<u8>); 7] = [
            ("LP", "LP", mutated(&|p| {
                let arg = venue_mut(p, venue_cid).argument.as_mut().expect("venue argument");
                field_mut(arg, &["lp"]).sum = Some(Sum::Party(OTHER.into()));
            })),
            ("amounts", "amount", mutated(&|p| {
                let chosen = settle_mut(p).chosen_value.as_mut().expect("chosen value");
                field_mut(chosen, &["quote", "baseAmount"]).sum = Some(Sum::Numeric("1.0000000000".into()));
            })),
            ("fee receiver", "fee", mutated(&fee_field(&["receiver"], Sum::Party(OTHER.into())))),
            ("extra fee", "fee", mutated(&|p| {
                let fees = fees_mut(p);
                let first = fees.first().cloned().expect("a fee");
                fees.push(first);
            })),
            ("non-CC fee", "fee", mutated(&fee_field(&["instrumentId", "id"], Sum::Text("USDC".into())))),
            ("extra outflow", "outflow", mutated(&|p| {
                let copy = amulet_copy(p, &pins.party_fee, OTHER, 1);
                settle_mut(p).children.push(copy.node_id.clone());
                nodes_mut(p).push(copy);
            })),
            ("second root", "root", mutated(&|p| {
                let copy = amulet_copy(p, &pins.party_fee, &c.party, 2);
                if let Some(tx) = p.transaction.as_mut() {
                    tx.roots.push(copy.node_id.clone());
                }
                nodes_mut(p).push(copy);
            })),
        ];
        for (what, needle, bytes) in cases {
            assert_flagged(&bytes, &e, needle, &format!("{} {what}", c.fixture_id));
        }
    }
}


#[test]
fn lp_change_never_absorbs_the_users_own_holdings() {
    let mut judged = 0;
    for c in common::load("atomic_dvp_settle") {
        let e = settle_expectation(&c, false);
        let OperationExpectation::AtomicDvpSettle { lp_party, side, base_instrument_id, quote_instrument_id, .. } = &e else {
            panic!("not a settle")
        };
        // The LP's change is in CC where the user receives CC: hand the user's CC to the LP.
        let proceeds = if side == "Buy" { base_instrument_id } else { quote_instrument_id };
        if proceeds != "Amulet" {
            continue;
        }
        let facts = common::facts(&c);
        let cc = facts.inflows.iter().find(|h| h.instrument_id == "Amulet").expect("the user's CC");
        let mut p = common::prepared_transaction(&c);
        reown(&mut p, &cc.contract_id, lp_party);
        assert_flagged(&common::encode(&p), &e, "net outflow", &format!("{} user CC to the LP", c.fixture_id));
        judged += 1;
    }
    assert!(judged > 0, "no settle pays the user in CC");
}
