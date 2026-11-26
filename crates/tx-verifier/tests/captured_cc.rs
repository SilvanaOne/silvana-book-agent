//! Captured devnet records: CC transfers, traffic top-ups, splits and batch payments.

mod common;

use common::Captured;
use prost::Message;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
use tx_verifier::{BatchPayTarget, OperationExpectation};
use v2::interactive::daml_transaction::{node::VersionedNode, Node};
use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::{Create, Exercise, Node as V1Node};
use v2::interactive::PreparedTransaction;
use v2::value::Sum;
use v2::{Identifier, Record, RecordField, Value};

const OTHER: &str = "intruder::1220ff";

/// A named change to a prepared transaction.
type TxMutation = (&'static str, fn(&mut PreparedTransaction));
const DAY_MICROS: i64 = 86_400_000_000;

/// The request fields the agent fills its expectations from.
#[derive(Clone, PartialEq, Message)]
struct Request {
    #[prost(message, optional, tag = "21")]
    split_cc: Option<SplitCcParams>,
}

#[derive(Clone, PartialEq, Message)]
struct SplitCcParams {
    #[prost(string, repeated, tag = "2")]
    amulet_cids: Vec<String>,
}

/// The record's expectation as the agent builds it now: pins and request coins filled.
fn as_built(c: &Captured) -> OperationExpectation {
    let pins = common::pins();
    match c.expectation.clone() {
        OperationExpectation::TransferCc { sender_party, receiver_party, amount, command_id, .. } => {
            OperationExpectation::TransferCc { sender_party, receiver_party, amount, command_id, dso_party: Some(pins.dso) }
        }
        OperationExpectation::PrepayTraffic { sender_party, amount, command_id, .. } => OperationExpectation::PrepayTraffic {
            sender_party,
            amount,
            command_id,
            receiver_party: Some(pins.party_prepaid_traffic),
            dso_party: Some(pins.dso),
        },
        OperationExpectation::SplitCc { party, output_amounts, .. } => {
            let request = Request::decode(c.request.as_slice()).expect("request");
            let amulet_cids = request.split_cc.expect("split params").amulet_cids;
            OperationExpectation::SplitCc { party, output_amounts, amulet_cids, dso_party: Some(pins.dso) }
        }
        other => panic!("{}: unexpected {other:?}", c.fixture_id),
    }
}

fn assert_clean(id: &str, bytes: &[u8], exp: &OperationExpectation) {
    let (r, observed) = common::inspect_bytes(bytes, exp);
    assert!(r.accepted, "{id}: refused: {:?}", r.rejection_reason);
    assert!(r.warnings.is_empty(), "{id}: warnings {:?}", r.warnings);
    assert!(observed.is_empty(), "{id}: enforce would refuse {observed:?}");
}

/// The refusal under enforce, if any.
fn refusal(bytes: &[u8], exp: &OperationExpectation) -> Option<String> {
    let (r, observed) = common::inspect_bytes(bytes, exp);
    if !r.accepted {
        return Some(r.rejection_reason.unwrap_or(r.summary));
    }
    observed.into_iter().next()
}

// ---------------------------------------------------------------------------
// Mutation helpers
// ---------------------------------------------------------------------------

fn val(sum: Sum) -> Value {
    Value { sum: Some(sum) }
}
fn party(s: &str) -> Value {
    val(Sum::Party(s.into()))
}
fn numeric(s: &str) -> Value {
    val(Sum::Numeric(s.into()))
}
fn cid(s: &str) -> Value {
    val(Sum::ContractId(s.into()))
}
fn list(elements: Vec<Value>) -> Value {
    val(Sum::List(v2::List { elements }))
}
fn optional(inner: Option<Value>) -> Value {
    val(Sum::Optional(Box::new(v2::Optional { value: inner.map(Box::new) })))
}
fn variant(constructor: &str, inner: Value) -> Value {
    val(Sum::Variant(Box::new(v2::Variant {
        variant_id: None,
        constructor: constructor.into(),
        value: Some(Box::new(inner)),
    })))
}
fn record(fields: Vec<(&str, Value)>) -> Value {
    val(Sum::Record(Record {
        record_id: None,
        fields: fields
            .into_iter()
            .map(|(label, value)| RecordField { label: label.into(), value: Some(value) })
            .collect(),
    }))
}

fn node_type_mut(n: &mut Node) -> &mut NodeType {
    match n.versioned_node.as_mut() {
        Some(VersionedNode::V1(v)) => v.node_type.as_mut().expect("node type"),
        None => panic!("unversioned node"),
    }
}
fn node_type(n: &Node) -> &NodeType {
    match n.versioned_node.as_ref() {
        Some(VersionedNode::V1(v)) => v.node_type.as_ref().expect("node type"),
        None => panic!("unversioned node"),
    }
}
fn nodes_mut(p: &mut PreparedTransaction) -> &mut Vec<Node> {
    &mut p.transaction.as_mut().expect("transaction").nodes
}
fn exercise_mut<'a>(p: &'a mut PreparedTransaction, choice: &str) -> &'a mut Exercise {
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| match node_type_mut(n) {
            NodeType::Exercise(e) if e.choice_id == choice => Some(e),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {choice}"))
}
fn root_mut(p: &mut PreparedTransaction) -> &mut Exercise {
    let root = p.transaction.as_ref().expect("transaction").roots[0].clone();
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| {
            let id = n.node_id.clone();
            match node_type_mut(n) {
                NodeType::Exercise(e) if id == root => Some(e),
                _ => None,
            }
        })
        .expect("root exercise")
}
fn rec_mut(v: &mut Value) -> &mut Record {
    match v.sum.as_mut() {
        Some(Sum::Record(r)) => r,
        _ => panic!("not a record"),
    }
}
fn list_mut(v: &mut Value) -> &mut Vec<Value> {
    match v.sum.as_mut() {
        Some(Sum::List(l)) => &mut l.elements,
        _ => panic!("not a list"),
    }
}
fn field_mut<'a>(r: &'a mut Record, label: &str) -> &'a mut Value {
    r.fields
        .iter_mut()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_mut())
        .unwrap_or_else(|| panic!("no field {label}"))
}
fn path_mut<'a>(mut r: &'a mut Record, path: &[&str]) -> &'a mut Value {
    let (last, init) = path.split_last().expect("path");
    for label in init {
        r = rec_mut(field_mut(r, label));
    }
    field_mut(r, last)
}
fn chosen_mut(e: &mut Exercise) -> &mut Record {
    rec_mut(e.chosen_value.as_mut().expect("chosen"))
}
fn party_of(r: &Record, label: &str) -> String {
    match r.fields.iter().find(|f| f.label == label).and_then(|f| f.value.as_ref()).and_then(|v| v.sum.as_ref()) {
        Some(Sum::Party(p)) => p.clone(),
        _ => panic!("no party {label}"),
    }
}
/// The node id of the first create of `entity` whose `owner` is `owner`.
fn create_id(p: &PreparedTransaction, entity: &str, owner: &str) -> String {
    p.transaction
        .as_ref()
        .expect("transaction")
        .nodes
        .iter()
        .find(|n| match node_type(n) {
            NodeType::Create(c) => {
                c.template_id.as_ref().is_some_and(|t| t.entity_name == entity)
                    && match c.argument.as_ref().and_then(|a| a.sum.as_ref()) {
                        Some(Sum::Record(r)) => party_of(r, "owner") == owner,
                        _ => false,
                    }
            }
            _ => false,
        })
        .map(|n| n.node_id.clone())
        .unwrap_or_else(|| panic!("no {entity} create for {owner}"))
}
fn create_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut Create {
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| {
            let this = n.node_id == id;
            match node_type_mut(n) {
                NodeType::Create(c) if this => Some(c),
                _ => None,
            }
        })
        .expect("create")
}
/// Adds a copy of create `id`, owned by `owner`, beside it.
fn add_copy(p: &mut PreparedTransaction, id: &str, owner: &str) {
    let mut copy = create_mut(p, id).clone();
    *field_mut(rec_mut(copy.argument.as_mut().expect("argument")), "owner") = party(owner);
    let new_id = "900".to_string();
    nodes_mut(p).push(Node {
        node_id: new_id.clone(),
        versioned_node: Some(VersionedNode::V1(V1Node { node_type: Some(NodeType::Create(copy)) })),
    });
    for n in nodes_mut(p) {
        if let NodeType::Exercise(e) = node_type_mut(n)
            && e.children.iter().any(|c| c == id)
        {
            e.children.push(new_id.clone());
            return;
        }
    }
    panic!("no parent of {id}");
}
fn set_result_output(e: &mut Exercise, constructor: &str) {
    let result = rec_mut(e.exercise_result.as_mut().expect("result"));
    match field_mut(result, "output").sum.as_mut() {
        Some(Sum::Variant(v)) => v.constructor = constructor.into(),
        _ => panic!("output is not a variant"),
    }
}
fn timestamp_of(v: &Value) -> i64 {
    match v.sum.as_ref() {
        Some(Sum::Timestamp(t)) => *t,
        _ => panic!("not a timestamp"),
    }
}
fn mutated(c: &Captured, f: impl FnOnce(&mut PreparedTransaction)) -> Vec<u8> {
    let mut p = common::prepared_transaction(c);
    f(&mut p);
    common::encode(&p)
}

struct Parties {
    sender: String,
    receiver: String,
}

fn parties(p: &PreparedTransaction) -> Parties {
    let mut p = p.clone();
    let transfer = rec_mut(field_mut(chosen_mut(root_mut(&mut p)), "transfer")).clone();
    Parties { sender: party_of(&transfer, "sender"), receiver: party_of(&transfer, "receiver") }
}

type Mutation = (&'static str, fn(&mut PreparedTransaction, &Parties));

const CC_MUTATIONS: &[Mutation] = &[
    ("the transfer receiver", |p, _| *path_mut(chosen_mut(root_mut(p)), &["transfer", "receiver"]) = party(OTHER)),
    ("the receiver amulet owner", |p, s| {
        let id = create_id(p, "Amulet", &s.receiver);
        *field_mut(rec_mut(create_mut(p, &id).argument.as_mut().expect("arg")), "owner") = party(OTHER);
    }),
    ("the transfer amount", |p, _| *path_mut(chosen_mut(root_mut(p)), &["transfer", "amount"]) = numeric("999.0000000000")),
    ("the receiver amulet amount", |p, s| {
        let id = create_id(p, "Amulet", &s.receiver);
        let arg = rec_mut(create_mut(p, &id).argument.as_mut().expect("arg"));
        *path_mut(arg, &["amount", "initialAmount"]) = numeric("999.0000000000");
    }),
    ("change to a third party", |p, s| {
        let id = create_id(p, "Amulet", &s.sender);
        *field_mut(rec_mut(create_mut(p, &id).argument.as_mut().expect("arg")), "owner") = party(OTHER);
    }),
    ("an extra amulet", |p, s| {
        let id = create_id(p, "Amulet", &s.receiver);
        add_copy(p, &id, OTHER);
    }),
    ("a fake expectedAdmin", |p, _| *field_mut(chosen_mut(root_mut(p)), "expectedAdmin") = party(OTHER)),
    ("a fake instrument admin", |p, _| {
        *path_mut(chosen_mut(root_mut(p)), &["transfer", "instrumentId", "admin"]) = party(OTHER)
    }),
    ("Send in place of SendV2", |p, _| exercise_mut(p, "TransferPreapproval_SendV2").choice_id = "TransferPreapproval_Send".into()),
    ("a pending result", |p, _| set_result_output(root_mut(p), "TransferInstructionResult_Pending")),
    ("executeBefore 30 days later", |p, _| {
        let v = path_mut(chosen_mut(root_mut(p)), &["transfer", "executeBefore"]);
        *v = val(Sum::Timestamp(timestamp_of(v) + 30 * DAY_MICROS));
    }),
    ("a second submitter", |p, _| {
        p.metadata.as_mut().expect("metadata").submitter_info.as_mut().expect("submitter").act_as.push(OTHER.into())
    }),
    ("a marker without a featured context", |p, s| {
        let root = root_mut(p);
        if let Some(Sum::TextMap(m)) = path_mut(chosen_mut(root), &["extraArgs", "context", "values"]).sum.as_mut() {
            m.entries.retain(|e| e.key != "featured-app-right");
        }
        let id = create_id(p, "Amulet", &s.receiver);
        add_copy(p, &id, OTHER);
        let marker = create_mut(p, "900");
        marker.template_id.as_mut().expect("template").entity_name = "FeaturedAppActivityMarker".into();
        marker.argument = Some(record(vec![("dso", party(OTHER)), ("provider", party(OTHER)), ("beneficiary", party(OTHER))]));
    }),
];

fn assert_cc_mutations_refuse(kind: &str) {
    for c in common::load(kind) {
        let exp = as_built(&c);
        let who = parties(&common::prepared_transaction(&c));
        for (name, mutation) in CC_MUTATIONS {
            let bytes = mutated(&c, |p| mutation(p, &who));
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Transfers and top-ups
// ---------------------------------------------------------------------------

#[test]
fn transfer_cc_hash_and_facts() {
    common::assert_hash_and_facts("transfer_cc");
}

#[test]
fn transfer_cc_inspects_clean() {
    common::assert_inspects_clean("transfer_cc");
}

#[test]
fn transfer_cc_inspects_clean_with_the_dso_pinned() {
    for c in common::load("transfer_cc") {
        assert_clean(&c.fixture_id, &c.prepared, &as_built(&c));
    }
}

#[test]
fn transfer_cc_with_another_dso_pinned_refuses() {
    for c in common::load("transfer_cc") {
        let OperationExpectation::TransferCc { sender_party, receiver_party, amount, command_id, .. } = c.expectation.clone() else {
            panic!("not a transfer");
        };
        let exp = OperationExpectation::TransferCc { sender_party, receiver_party, amount, command_id, dso_party: Some(OTHER.into()) };
        assert!(refusal(&c.prepared, &exp).is_some(), "{}", c.fixture_id);
    }
}

#[test]
fn transfer_cc_mutations_refuse() {
    assert_cc_mutations_refuse("transfer_cc");
}

#[test]
fn prepay_traffic_hash_and_facts() {
    common::assert_hash_and_facts("prepay_traffic");
}

#[test]
fn prepay_traffic_inspects_clean() {
    for c in common::load("prepay_traffic") {
        assert_clean(&c.fixture_id, &c.prepared, &as_built(&c));
    }
}

#[test]
fn prepay_traffic_without_the_pin_only_warns() {
    for c in common::load("prepay_traffic") {
        let (r, observed) = common::inspect_observing(&c);
        assert!(r.accepted, "{}: {:?}", c.fixture_id, r.rejection_reason);
        assert_eq!(r.warnings.len(), 1, "{}: {:?}", c.fixture_id, r.warnings);
        assert!(observed.is_empty(), "{}: {observed:?}", c.fixture_id);
    }
}

#[test]
fn prepay_traffic_to_another_receiver_refuses() {
    for c in common::load("prepay_traffic") {
        let OperationExpectation::PrepayTraffic { sender_party, amount, command_id, .. } = c.expectation.clone() else {
            panic!("not a top-up");
        };
        let exp = OperationExpectation::PrepayTraffic {
            sender_party,
            amount,
            command_id,
            receiver_party: Some(OTHER.into()),
            dso_party: None,
        };
        assert!(refusal(&c.prepared, &exp).is_some(), "{}", c.fixture_id);
    }
}

#[test]
fn prepay_traffic_mutations_refuse() {
    assert_cc_mutations_refuse("prepay_traffic");
}

// ---------------------------------------------------------------------------
// Splits and CC merges
// ---------------------------------------------------------------------------

#[test]
fn split_cc_hash_and_facts() {
    common::assert_hash_and_facts("split_cc");
}

#[test]
fn split_cc_inspects_clean() {
    for c in common::load("split_cc") {
        assert_clean(&c.fixture_id, &c.prepared, &as_built(&c));
    }
}

#[test]
fn split_cc_without_the_coins_only_warns() {
    for c in common::load("split_cc") {
        let (r, observed) = common::inspect_observing(&c);
        assert!(r.accepted, "{}: {:?}", c.fixture_id, r.rejection_reason);
        assert_eq!(r.warnings.len(), 1, "{}: {:?}", c.fixture_id, r.warnings);
        assert!(observed.is_empty(), "{}: {observed:?}", c.fixture_id);
    }
}

const SPLIT_MUTATIONS: &[TxMutation] = &[
    ("an output to another party", |p| {
        let outputs = list_mut(path_mut(chosen_mut(root_mut(p)), &["transfer", "outputs"]));
        *field_mut(rec_mut(&mut outputs[0]), "receiver") = party(OTHER);
    }),
    ("a locked output", |p| {
        let outputs = list_mut(path_mut(chosen_mut(root_mut(p)), &["transfer", "outputs"]));
        rec_mut(&mut outputs[0]).fields.push(RecordField {
            label: "lock".into(),
            value: Some(optional(Some(record(vec![("holders", list(vec![party(OTHER)]))])))),
        });
    }),
    ("an input that was not requested", |p| {
        let inputs = list_mut(path_mut(chosen_mut(root_mut(p)), &["transfer", "inputs"]));
        inputs[0] = variant("InputAmulet", cid("00ff"));
    }),
    ("another provider", |p| *path_mut(chosen_mut(root_mut(p)), &["transfer", "provider"]) = party(OTHER)),
    ("an output amount", |p| {
        let outputs = list_mut(path_mut(chosen_mut(root_mut(p)), &["transfer", "outputs"]));
        *field_mut(rec_mut(&mut outputs[0]), "amount") = numeric("6.0000000000");
    }),
    ("a created amulet for another party", |p| {
        let owner = parties_split(p);
        let id = create_id(p, "Amulet", &owner);
        *field_mut(rec_mut(create_mut(p, &id).argument.as_mut().expect("arg")), "owner") = party(OTHER);
    }),
    ("a second submitter", |p| {
        p.metadata.as_mut().expect("metadata").submitter_info.as_mut().expect("submitter").act_as.push(OTHER.into())
    }),
];

fn parties_split(p: &mut PreparedTransaction) -> String {
    let transfer = rec_mut(field_mut(chosen_mut(root_mut(p)), "transfer")).clone();
    party_of(&transfer, "sender")
}

#[test]
fn split_cc_mutations_refuse() {
    for c in common::load("split_cc") {
        let exp = as_built(&c);
        for (name, mutation) in SPLIT_MUTATIONS {
            let bytes = mutated(&c, mutation);
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Batch payments: one Execute_MultiCall over two captured transfer subtrees
// ---------------------------------------------------------------------------

fn renumbered(p: &PreparedTransaction, offset: usize) -> (String, Vec<Node>) {
    let shift = |id: &str| (id.parse::<usize>().expect("numeric node id") + offset).to_string();
    let tx = p.transaction.as_ref().expect("transaction");
    let nodes = tx
        .nodes
        .iter()
        .map(|n| {
            let mut n = n.clone();
            n.node_id = shift(&n.node_id);
            if let NodeType::Exercise(e) = node_type_mut(&mut n) {
                e.children = e.children.iter().map(|c| shift(c)).collect();
            }
            n
        })
        .collect();
    (shift(&tx.roots[0]), nodes)
}

fn batch_op(tf: &Exercise) -> Value {
    let mut tf = tf.clone();
    let chosen = chosen_mut(&mut tf).clone();
    let get = |r: &Record, label: &str| r.fields.iter().find(|f| f.label == label).and_then(|f| f.value.clone()).expect(label);
    let transfer = match get(&chosen, "transfer").sum {
        Some(Sum::Record(r)) => r,
        _ => panic!("transfer"),
    };
    variant(
        "Op_BatchTransfer",
        record(vec![
            ("transferFactoryCid", cid(&tf.contract_id)),
            ("expectedAdmin", get(&chosen, "expectedAdmin")),
            ("instrumentId", get(&transfer, "instrumentId")),
            ("requestedAt", get(&transfer, "requestedAt")),
            ("executeBefore", get(&transfer, "executeBefore")),
            ("extraArgs", get(&chosen, "extraArgs")),
            (
                "transferTargets",
                list(vec![record(vec![
                    ("receiver", get(&transfer, "receiver")),
                    ("amount", get(&transfer, "amount")),
                    ("description", optional(None)),
                ])]),
            ),
        ]),
    )
}

/// A batch payment built from the CLI transfer and the CLI top-up of the same agent.
fn batch_pay() -> (PreparedTransaction, OperationExpectation) {
    let cc = common::load("transfer_cc").remove(1);
    let prepay = common::load("prepay_traffic").remove(0);
    let (mut a, mut b) = (common::prepared_transaction(&cc), common::prepared_transaction(&prepay));
    let (sa, sb) = (parties(&a), parties(&b));
    assert_eq!(sa.sender, sb.sender, "one sender");
    let (tf_a, tf_b) = (root_mut(&mut a).clone(), root_mut(&mut b).clone());
    let inputs = |tf: &Exercise| -> Vec<Value> {
        let mut tf = tf.clone();
        list_mut(path_mut(chosen_mut(&mut tf), &["transfer", "inputHoldingCids"])).clone()
    };
    let coins: Vec<Value> = inputs(&tf_a).into_iter().chain(inputs(&tf_b)).collect();
    let holding_cids = coins
        .iter()
        .map(|v| match v.sum.as_ref() {
            Some(Sum::ContractId(c)) => c.clone(),
            _ => panic!("coin"),
        })
        .collect();
    let (root_a, mut nodes) = renumbered(&a, 1);
    let (root_b, nodes_b) = renumbered(&b, 101);
    nodes.extend(nodes_b);
    let root = Exercise {
        lf_version: tf_a.lf_version.clone(),
        contract_id: "00aa".into(),
        package_name: "multicall-v1".into(),
        template_id: Some(Identifier {
            package_id: "pkg".into(),
            module_name: "MultiCall".into(),
            entity_name: "MultiCall".into(),
        }),
        acting_parties: vec![sa.sender.clone()],
        choice_id: "Execute_MultiCall".into(),
        chosen_value: Some(record(vec![
            ("sender", party(&sa.sender)),
            ("inputHoldings", list(coins)),
            ("operations", list(vec![batch_op(&tf_a), batch_op(&tf_b)])),
        ])),
        consuming: false,
        children: vec![root_a, root_b],
        ..Default::default()
    };
    nodes.push(Node {
        node_id: "0".into(),
        versioned_node: Some(VersionedNode::V1(V1Node { node_type: Some(NodeType::Exercise(root)) })),
    });
    let extra: Vec<_> = b.metadata.take().expect("metadata").input_contracts;
    let tx = a.transaction.as_mut().expect("transaction");
    tx.roots = vec!["0".into()];
    tx.nodes = nodes;
    tx.node_seeds.clear();
    let metadata = a.metadata.as_mut().expect("metadata");
    for ic in extra {
        let id = |ic: &v2::interactive::metadata::InputContract| match ic.contract.as_ref() {
            Some(InputContractOneof::V1(c)) => c.contract_id.clone(),
            None => String::new(),
        };
        if !metadata.input_contracts.iter().any(|m| id(m) == id(&ic)) {
            metadata.input_contracts.push(ic);
        }
    }
    let amount = |c: &Captured| match &c.expectation {
        OperationExpectation::TransferCc { amount, .. } | OperationExpectation::PrepayTraffic { amount, .. } => amount.clone(),
        other => panic!("{other:?}"),
    };
    let exp = OperationExpectation::BatchPayCc {
        party: sa.sender.clone(),
        targets: vec![
            BatchPayTarget { receiver: sa.receiver, amount: amount(&cc) },
            BatchPayTarget { receiver: sb.receiver, amount: amount(&prepay) },
        ],
        holding_cids,
        dso_party: Some(common::pins().dso),
    };
    (a, exp)
}

fn tf_mut<'a>(p: &'a mut PreparedTransaction, node_id: &str) -> &'a mut Exercise {
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| {
            let this = n.node_id == node_id;
            match node_type_mut(n) {
                NodeType::Exercise(e) if this => Some(e),
                _ => None,
            }
        })
        .expect("transfer node")
}

#[test]
fn batch_pay_inspects_clean() {
    let (p, exp) = batch_pay();
    assert_clean("batch-pay", &common::encode(&p), &exp);
    let OperationExpectation::BatchPayCc { party, targets, holding_cids, .. } = exp else { panic!() };
    let unpinned = OperationExpectation::BatchPayCc { party, targets, holding_cids, dso_party: None };
    assert_clean("batch-pay unpinned", &common::encode(&p), &unpinned);
}

#[test]
fn batch_pay_expectation_mismatches_refuse() {
    let (p, exp) = batch_pay();
    let bytes = common::encode(&p);
    let OperationExpectation::BatchPayCc { party, targets, holding_cids, dso_party } = exp else { panic!() };
    let with = |targets: Vec<BatchPayTarget>, holding_cids: Vec<String>| OperationExpectation::BatchPayCc {
        party: party.clone(),
        targets,
        holding_cids,
        dso_party: dso_party.clone(),
    };
    let mut amount = targets.clone();
    amount[1].amount = "6".into();
    let cases = [
        ("an unrequested coin", with(targets.clone(), holding_cids[..1].to_vec())),
        ("a target amount", with(amount, holding_cids.clone())),
        ("a missing target", with(targets[..1].to_vec(), holding_cids.clone())),
    ];
    for (name, exp) in cases {
        assert!(refusal(&bytes, &exp).is_some(), "{name} is accepted");
    }
}

#[test]
fn batch_pay_mutations_refuse() {
    let (p, exp) = batch_pay();
    let mutations: &[TxMutation] = &[
        ("a window over 310 s", |p| {
            let v = path_mut(chosen_mut(tf_mut(p, "101")), &["transfer", "executeBefore"]);
            *v = val(Sum::Timestamp(timestamp_of(v) + 400_000_000));
        }),
        ("a pending transfer", |p| set_result_output(tf_mut(p, "101"), "TransferInstructionResult_Pending")),
        ("a transfer to another receiver", |p| {
            *path_mut(chosen_mut(tf_mut(p, "101")), &["transfer", "receiver"]) = party(OTHER)
        }),
        ("a second submitter", |p| {
            p.metadata.as_mut().expect("metadata").submitter_info.as_mut().expect("submitter").act_as.push(OTHER.into())
        }),
        ("another operation", |p| {
            let ops = list_mut(field_mut(chosen_mut(root_mut(p)), "operations"));
            if let Some(Sum::Variant(v)) = ops[1].sum.as_mut() {
                v.constructor = "Op_Transfer".into();
            }
        }),
        ("an op admin", |p| {
            let ops = list_mut(field_mut(chosen_mut(root_mut(p)), "operations"));
            if let Some(Sum::Variant(v)) = ops[1].sum.as_mut() {
                *field_mut(rec_mut(v.value.as_mut().expect("op")), "expectedAdmin") = party(OTHER);
            }
        }),
        ("the preapproval of another receiver", |p| {
            let other = tf_mut(p, "1").children.clone();
            let send_a = other
                .iter()
                .find_map(|id| {
                    let id = id.clone();
                    let n = nodes_mut(p).iter_mut().find(|n| n.node_id == id)?;
                    match node_type_mut(n) {
                        NodeType::Exercise(e) if e.choice_id == "TransferPreapproval_SendV2" => Some(e.contract_id.clone()),
                        _ => None,
                    }
                })
                .expect("first SendV2");
            let ids = tf_mut(p, "101").children.clone();
            for id in ids {
                let n = nodes_mut(p).iter_mut().find(|n| n.node_id == id).expect("child");
                if let NodeType::Exercise(e) = node_type_mut(n)
                    && e.choice_id == "TransferPreapproval_SendV2"
                {
                    e.contract_id = send_a.clone();
                }
            }
        }),
        ("an extra amulet", |p| {
            let receiver = party_of(rec_mut(field_mut(chosen_mut(tf_mut(p, "101")), "transfer")), "receiver");
            let id = create_id(p, "Amulet", &receiver);
            add_copy(p, &id, OTHER);
        }),
    ];
    for (name, mutation) in mutations {
        let mut q = p.clone();
        mutation(&mut q);
        assert!(refusal(&common::encode(&q), &exp).is_some(), "{name} is accepted");
    }
}

#[test]
fn cc_transfers_and_top_ups_name_their_memo_once() {
    let memos = |kind: &str| -> Vec<Vec<String>> {
        common::load(kind).iter().map(|c| common::facts(c).memos).collect()
    };
    assert_eq!(memos("transfer_cc"), [vec!["test3"], vec!["cc-transfer"]]);
    assert_eq!(memos("prepay_traffic"), [vec!["cc-transfer"], vec!["auto-topup"]]);
}
