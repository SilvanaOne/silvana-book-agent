//! Captured devnet records: CIP-56 transfers and preapproval requests.

mod common;

use base64::Engine;
use common::Captured;
use prost::Message;
use rust_decimal::Decimal;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2 as v2;
use tx_verifier::{OperationExpectation, VerifyMode};
use v2::interactive::daml_transaction::{node::VersionedNode, Node};
use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
use v2::interactive::transaction::v1::node::NodeType;
use v2::interactive::transaction::v1::{Create, Exercise, Node as V1Node};
use v2::interactive::PreparedTransaction;
use v2::value::Sum;
use v2::{Identifier, Record, RecordField, Value};

const OTHER: &str = "intruder::1220ff";
const FACTORY_MODULE: &str = "Utility.Registry.App.V0.Service.AllocationFactory";
const FACTORY_CID: &str = "00a110ca7ef0c7";
const SECOND_INPUT: &str = "00b2d1e5ec0de1";
const DAY_MICROS: i64 = 86_400 * 1_000_000;

/// A named change to a prepared transaction.
type TxMutation = (&'static str, fn(&mut PreparedTransaction));

/// The request fields the agent fills its expectations from.
#[derive(Clone, PartialEq, Message)]
struct Request {
    #[prost(message, optional, tag = "15")]
    request_preapproval: Option<PreapprovalParams>,
    #[prost(message, optional, tag = "19")]
    transfer_cip56: Option<Cip56Params>,
}

#[derive(Clone, PartialEq, Message)]
struct PreapprovalParams {
    #[prost(string, tag = "1")]
    instrument_admin: String,
    #[prost(string, tag = "3")]
    operator: String,
}

#[derive(Clone, PartialEq, Message)]
struct Cip56Params {
    #[prost(string, repeated, tag = "6")]
    input_holding_cids: Vec<String>,
}

/// The record's expectation as the agent builds it now.
fn as_built(c: &Captured) -> OperationExpectation {
    let request = Request::decode(c.request.as_slice()).expect("request");
    match c.expectation.clone() {
        OperationExpectation::TransferCip56 { sender_party, receiver_party, instrument_id, instrument_admin, amount, .. } => {
            OperationExpectation::TransferCip56 {
                sender_party,
                receiver_party,
                instrument_id,
                instrument_admin,
                amount,
                // Directed transfers name their command id; the CLI's is the server's.
                command_id: c.command_id.starts_with("ctl-").then(|| c.command_id.clone()),
                input_holding_cids: request.transfer_cip56.expect("cip56 params").input_holding_cids,
            }
        }
        OperationExpectation::RequestPreapproval { party, .. } => {
            let params = request.request_preapproval.expect("preapproval params");
            OperationExpectation::RequestPreapproval {
                party,
                operator: Some(params.operator),
                instrument_admin: Some(params.instrument_admin),
            }
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

fn party(s: &str) -> Value {
    Value { sum: Some(Sum::Party(s.into())) }
}
fn numeric(s: &str) -> Value {
    Value { sum: Some(Sum::Numeric(s.into())) }
}
fn text(s: &str) -> Value {
    Value { sum: Some(Sum::Text(s.into())) }
}
fn node_type_mut(n: &mut Node) -> &mut NodeType {
    match n.versioned_node.as_mut() {
        Some(VersionedNode::V1(v)) => v.node_type.as_mut().expect("node type"),
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
    exercise_mut(p, "TransferFactory_Transfer")
}
fn rec_mut(v: &mut Value) -> &mut Record {
    match v.sum.as_mut() {
        Some(Sum::Record(r)) => r,
        _ => panic!("not a record"),
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
fn creates_mut(p: &mut PreparedTransaction) -> impl Iterator<Item = (String, &mut Create)> {
    nodes_mut(p).iter_mut().filter_map(|n| {
        let id = n.node_id.clone();
        match node_type_mut(n) {
            NodeType::Create(c) => Some((id, c)),
            _ => None,
        }
    })
}
fn owner_of(c: &Create) -> String {
    match c.argument.as_ref().and_then(|a| a.sum.as_ref()) {
        Some(Sum::Record(r)) => party_of(r, "owner"),
        _ => panic!("no owner"),
    }
}
/// The node id of the Holding create owned by `owner`.
fn holding_id(p: &mut PreparedTransaction, owner: &str) -> String {
    creates_mut(p)
        .find(|(_, c)| owner_of(c) == owner)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no holding of {owner}"))
}
fn holding_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut Record {
    let (_, c) = creates_mut(p).find(|(i, _)| i == id).expect("holding");
    rec_mut(c.argument.as_mut().expect("arg"))
}
fn set_result_output(e: &mut Exercise, constructor: &str) {
    let result = rec_mut(e.exercise_result.as_mut().expect("result"));
    match field_mut(result, "output").sum.as_mut() {
        Some(Sum::Variant(v)) => v.constructor = constructor.into(),
        _ => panic!("output is not a variant"),
    }
}
fn sender_receiver(p: &mut PreparedTransaction) -> (String, String) {
    let transfer = rec_mut(field_mut(chosen_mut(root_mut(p)), "transfer")).clone();
    (party_of(&transfer, "sender"), party_of(&transfer, "receiver"))
}
fn mutated(c: &Captured, f: impl FnOnce(&mut PreparedTransaction)) -> Vec<u8> {
    let mut p = common::prepared_transaction(c);
    f(&mut p);
    common::encode(&p)
}
fn cid(s: &str) -> Value {
    Value { sum: Some(Sum::ContractId(s.into())) }
}
fn rec(fields: Vec<(&str, Value)>) -> Value {
    let fields = fields.into_iter().map(|(label, v)| RecordField { label: label.into(), value: Some(v) }).collect();
    Value { sum: Some(Sum::Record(Record { record_id: None, fields })) }
}
/// `id` with another module and entity, in the same package.
fn renamed(id: &Option<Identifier>, module: &str, entity: &str) -> Option<Identifier> {
    id.clone().map(|t| Identifier { module_name: module.into(), entity_name: entity.into(), ..t })
}
fn is_entity(id: &Option<Identifier>, entity: &str) -> bool {
    id.as_ref().is_some_and(|t| t.entity_name == entity)
}
fn sum_of<'a>(r: &'a Record, label: &str) -> &'a Sum {
    r.fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
        .and_then(|v| v.sum.as_ref())
        .unwrap_or_else(|| panic!("no field {label}"))
}
fn numeric_of(r: &Record, label: &str) -> String {
    match sum_of(r, label) {
        Sum::Numeric(n) => n.clone(),
        _ => panic!("{label} is not a number"),
    }
}
fn node_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut NodeType {
    node_type_mut(nodes_mut(p).iter_mut().find(|n| n.node_id == id).unwrap_or_else(|| panic!("no node {id}")))
}
/// The id of the first node `f` picks.
fn node_id_of(p: &mut PreparedTransaction, f: impl Fn(&NodeType) -> bool) -> String {
    nodes_mut(p)
        .iter_mut()
        .find_map(|n| {
            let id = n.node_id.clone();
            f(node_type_mut(n)).then_some(id)
        })
        .expect("node")
}
fn push_node(p: &mut PreparedTransaction, id: &str, node_type: NodeType) {
    nodes_mut(p).push(Node {
        node_id: id.into(),
        versioned_node: Some(VersionedNode::V1(V1Node { node_type: Some(node_type) })),
    });
}
fn create_of_mut<'a>(p: &'a mut PreparedTransaction, entity: &str) -> &'a mut Create {
    creates_mut(p).map(|(_, c)| c).find(|c| is_entity(&c.template_id, entity)).unwrap_or_else(|| panic!("no {entity}"))
}
fn input_mut<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut Create {
    p.metadata
        .as_mut()
        .expect("metadata")
        .input_contracts
        .iter_mut()
        .find_map(|ic| match ic.contract.as_mut() {
            Some(InputContractOneof::V1(c)) if c.contract_id == id => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no input {id}"))
}
fn input_terms<'a>(p: &'a mut PreparedTransaction, id: &str) -> &'a mut Record {
    rec_mut(input_mut(p, id).argument.as_mut().expect("arg"))
}
/// A copy of input contract `like` under contract id `id`.
fn add_input(p: &mut PreparedTransaction, like: &str, id: &str) {
    let inputs = &mut p.metadata.as_mut().expect("metadata").input_contracts;
    let mut copy = inputs
        .iter()
        .find(|ic| matches!(ic.contract.as_ref(), Some(InputContractOneof::V1(c)) if c.contract_id == like))
        .expect("input")
        .clone();
    if let Some(InputContractOneof::V1(c)) = copy.contract.as_mut() {
        c.contract_id = id.into();
    }
    inputs.push(copy);
}
fn first_input(p: &mut PreparedTransaction) -> String {
    match path_mut(chosen_mut(root_mut(p)), &["transfer", "inputHoldingCids"]).sum.as_ref() {
        Some(Sum::List(l)) => match l.elements.first().and_then(|v| v.sum.as_ref()) {
            Some(Sum::ContractId(id)) => id.clone(),
            _ => panic!("input"),
        },
        _ => panic!("inputs"),
    }
}
fn cip56(sender: &str, receiver: &str, id: &str, admin: &str, amount: &str, inputs: &[&str]) -> OperationExpectation {
    OperationExpectation::TransferCip56 {
        sender_party: sender.into(),
        receiver_party: receiver.into(),
        instrument_id: id.into(),
        instrument_admin: admin.into(),
        amount: amount.into(),
        command_id: None,
        input_holding_cids: inputs.iter().map(|s| s.to_string()).collect(),
    }
}
/// The verification a linked agent runs on `bytes`, against their own hash.
fn verify_linked(c: &Captured, bytes: &[u8], exp: &OperationExpectation) -> tx_verifier::VerificationResult {
    let hash = tx_verifier::hasher::try_compute_hash(bytes, &c.hashing_scheme_version).expect("hash");
    let hash = base64::engine::general_purpose::STANDARD.encode(hash);
    let mode = VerifyMode { strict_all: true, ..VerifyMode::default() };
    tx_verifier::verify_and_hash_with(bytes, &hash, &c.hashing_scheme_version, exp, mode, false).expect("verification")
}

// ---------------------------------------------------------------------------
// Transfers
// ---------------------------------------------------------------------------

#[test]
fn transfer_cip56_hash_and_facts() {
    common::assert_hash_and_facts("transfer_cip56");
}

#[test]
fn transfer_cip56_inspects_clean() {
    common::assert_inspects_clean("transfer_cip56");
    for c in common::load("transfer_cip56") {
        assert_clean(&c.fixture_id, &c.prepared, &as_built(&c));
    }
}

const CIP56_MUTATIONS: &[TxMutation] = &[
    ("the receiver", |p| *path_mut(chosen_mut(root_mut(p)), &["transfer", "receiver"]) = party(OTHER)),
    ("the amount", |p| *path_mut(chosen_mut(root_mut(p)), &["transfer", "amount"]) = numeric("999.0000000000")),
    ("the instrument", |p| *path_mut(chosen_mut(root_mut(p)), &["transfer", "instrumentId", "id"]) = text("FAKE")),
    ("the admin", |p| {
        let chosen = chosen_mut(root_mut(p));
        *field_mut(chosen, "expectedAdmin") = party(OTHER);
        *path_mut(chosen, &["transfer", "instrumentId", "admin"]) = party(OTHER);
    }),
    ("change to a third party", |p| {
        let (sender, _) = sender_receiver(p);
        let id = holding_id(p, &sender);
        *field_mut(holding_mut(p, &id), "owner") = party(OTHER);
    }),
    ("an extra holding", |p| {
        let (_, receiver) = sender_receiver(p);
        let id = holding_id(p, &receiver);
        let (_, c) = creates_mut(p).find(|(i, _)| *i == id).expect("holding");
        let mut copy = c.clone();
        *field_mut(rec_mut(copy.argument.as_mut().expect("arg")), "owner") = party(OTHER);
        nodes_mut(p).push(Node {
            node_id: "900".into(),
            versioned_node: Some(VersionedNode::V1(V1Node { node_type: Some(NodeType::Create(copy)) })),
        });
        exercise_mut(p, "TransferRule_DirectTransfer").children.push("900".into());
    }),
    ("a child record that differs", |p| {
        let child = chosen_mut(exercise_mut(p, "TransferRule_DirectTransfer"));
        *path_mut(child, &["transfer", "amount"]) = numeric("2.0000000000");
    }),
    ("a pending result", |p| set_result_output(root_mut(p), "TransferInstructionResult_Pending")),
    ("a second submitter", |p| {
        p.metadata.as_mut().expect("metadata").submitter_info.as_mut().expect("submitter").act_as.push(OTHER.into())
    }),
];

#[test]
fn transfer_cip56_mutations_refuse() {
    for c in common::load("transfer_cip56") {
        let exp = as_built(&c);
        for (name, mutation) in CIP56_MUTATIONS {
            let bytes = mutated(&c, mutation);
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

#[test]
fn transfer_cip56_with_another_command_id_refuses() {
    let c = common::load("transfer_cip56").remove(0);
    let OperationExpectation::TransferCip56 { sender_party, receiver_party, instrument_id, instrument_admin, amount, .. } = as_built(&c) else {
        panic!("not a transfer");
    };
    let exp = OperationExpectation::TransferCip56 {
        sender_party,
        receiver_party,
        instrument_id,
        instrument_admin,
        amount,
        command_id: Some("ctl-other".into()),
        input_holding_cids: vec![],
    };
    assert!(refusal(&c.prepared, &exp).is_some());
}

#[test]
fn transfer_cip56_change_that_loses_value_refuses() {
    for c in common::load("transfer_cip56") {
        let exp = as_built(&c);
        let bytes = mutated(&c, |p| {
            let (sender, _) = sender_receiver(p);
            let id = holding_id(p, &sender);
            *field_mut(holding_mut(p, &id), "amount") = numeric("1.0000000000");
        });
        let reason = refusal(&bytes, &exp);
        assert!(reason.as_deref().is_some_and(|r| r.contains("inputs total")), "{}: {reason:?}", c.fixture_id);
    }
}

/// The nodes other registry versions add under the direct transfer: a credential read, an
/// activity marker and a transfer receipt.
fn add_registry_extras(p: &mut PreparedTransaction) {
    let (sender, receiver) = sender_receiver(p);
    let log = exercise_mut(p, "EventLog_HoldingsChange").clone();
    let paid = holding_id(p, &receiver);
    let NodeType::Create(holding) = node_mut(p, &paid).clone() else { panic!("holding") };
    let terms = match holding.argument.as_ref().and_then(|a| a.sum.as_ref()) {
        Some(Sum::Record(r)) => r.clone(),
        _ => panic!("terms"),
    };
    let value_of = |label: &str| Value { sum: Some(sum_of(&terms, label).clone()) };
    let mut credential = log.clone();
    credential.template_id = renamed(&log.template_id, "Utility.Credential.V0.Credential", "Credential");
    credential.interface_id = None;
    credential.choice_id = "Credential_Get".into();
    credential.contract_id = "00c4ed".into();
    credential.chosen_value = Some(rec(vec![("actor", value_of("registrar"))]));
    credential.children = vec![];
    let mut right = log.clone();
    right.template_id = renamed(&log.template_id, "Splice.Amulet", "FeaturedAppRight");
    right.interface_id = renamed(&log.interface_id, "Splice.Api.FeaturedAppRightV1", "FeaturedAppRight");
    right.choice_id = "FeaturedAppRight_CreateActivityMarker".into();
    right.contract_id = "00fa12".into();
    right.children = vec!["912".into()];
    let mut marker = holding.clone();
    marker.template_id = renamed(&holding.template_id, "Splice.Amulet", "FeaturedAppActivityMarker");
    marker.argument = Some(rec(vec![
        ("dso", party(OTHER)),
        ("provider", party(OTHER)),
        ("beneficiary", party(OTHER)),
        ("weight", numeric("1.0")),
    ]));
    let mut receipt = holding.clone();
    receipt.template_id = renamed(&holding.template_id, "Utility.Registry.V0.Holding.Transfer", "ExecutedTransfer");
    let transfer = rec(vec![
        ("operator", party(OTHER)),
        ("provider", party(OTHER)),
        ("registrar", value_of("registrar")),
        ("sender", party(&sender)),
        ("receiver", party(&receiver)),
        ("instrumentIdentifier", value_of("instrument")),
        ("amount", value_of("amount")),
        ("reference", text("")),
    ]);
    receipt.argument = Some(rec(vec![("transfer", transfer), ("senderLabel", text("")), ("receiverLabel", text(""))]));
    push_node(p, "910", NodeType::Exercise(credential));
    push_node(p, "911", NodeType::Exercise(right));
    push_node(p, "912", NodeType::Create(marker));
    push_node(p, "913", NodeType::Create(receipt));
    exercise_mut(p, "TransferRule_DirectTransfer").children.extend(["910", "911", "913"].map(String::from));
}

#[test]
fn transfer_cip56_with_other_registry_nodes_inspects_clean() {
    for c in common::load("transfer_cip56") {
        let bytes = mutated(&c, add_registry_extras);
        assert_clean(&c.fixture_id, &bytes, &as_built(&c));
    }
}

const REGISTRY_EXTRA_MUTATIONS: &[TxMutation] = &[
    ("a consuming credential read", |p| exercise_mut(p, "Credential_Get").consuming = true),
    ("a receipt of another transfer", |p| {
        let receipt = rec_mut(create_of_mut(p, "ExecutedTransfer").argument.as_mut().expect("arg"));
        *path_mut(receipt, &["transfer", "amount"]) = numeric("2.0000000000");
    }),
    ("a second receipt", |p| {
        let copy = create_of_mut(p, "ExecutedTransfer").clone();
        push_node(p, "914", NodeType::Create(copy));
        exercise_mut(p, "TransferRule_DirectTransfer").children.push("914".into());
    }),
    ("another create under the marker", |p| {
        let marker = create_of_mut(p, "FeaturedAppActivityMarker");
        marker.template_id = renamed(&marker.template_id, "Splice.Amulet", "Amulet");
    }),
];

#[test]
fn transfer_cip56_with_other_registry_nodes_mutations_refuse() {
    for c in common::load("transfer_cip56") {
        let exp = as_built(&c);
        for (name, mutation) in REGISTRY_EXTRA_MUTATIONS {
            let bytes = mutated(&c, |p| {
                add_registry_extras(p);
                mutation(p);
            });
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

/// A merge as the registry's allocation factory runs it, built from the CLI transfer: the
/// sender's input and a second one merged into one holding of their total.
fn merge(keep_change: bool) -> (Captured, Vec<u8>, OperationExpectation) {
    let c = common::load("transfer_cip56").remove(1);
    let mut p = common::prepared_transaction(&c);
    let (sender, receiver) = sender_receiver(&mut p);
    let input = first_input(&mut p);
    let first = numeric_of(input_terms(&mut p, &input), "amount");
    add_input(&mut p, &input, SECOND_INPUT);
    *field_mut(input_terms(&mut p, SECOND_INPUT), "amount") = numeric("2.5000000000");
    let total = (Decimal::from_str_exact(&first).expect("amount") + Decimal::new(25, 1)).to_string();
    // The factory has the parties of the registry's transfer rule.
    let rule = exercise_mut(&mut p, "TransferRule_DirectTransfer").contract_id.clone();
    add_input(&mut p, &rule, FACTORY_CID);
    let factory = input_mut(&mut p, FACTORY_CID);
    factory.template_id = renamed(&factory.template_id, FACTORY_MODULE, "AllocationFactory");
    let terms = input_terms(&mut p, FACTORY_CID).clone();
    {
        let root = root_mut(&mut p);
        root.template_id = renamed(&root.template_id, FACTORY_MODULE, "AllocationFactory");
        root.contract_id = FACTORY_CID.into();
        let transfer = rec_mut(field_mut(chosen_mut(root), "transfer"));
        *field_mut(transfer, "receiver") = party(&sender);
        *field_mut(transfer, "amount") = numeric(&total);
        if let Some(Sum::List(l)) = field_mut(transfer, "inputHoldingCids").sum.as_mut() {
            l.elements.push(cid(SECOND_INPUT));
        }
    }
    let root = root_mut(&mut p).clone();
    {
        let internal = exercise_mut(&mut p, "TransferRule_DirectTransfer");
        internal.template_id = root.template_id.clone();
        internal.contract_id = FACTORY_CID.into();
        internal.choice_id = "AllocationFactory_TransferInternal".into();
        internal.chosen_value = Some(rec(vec![("payload", root.chosen_value.clone().expect("chosen"))]));
        internal.acting_parties = vec![party_of(&terms, "provider"), party_of(&terms, "registrar"), sender.clone()];
        internal.exercise_result = root.exercise_result.clone();
    }
    // No instrument configuration fetch; the second input is fetched and archived too.
    let config = node_id_of(&mut p, |n| matches!(n, NodeType::Fetch(f) if is_entity(&f.template_id, "InstrumentConfiguration")));
    let fetch = node_id_of(&mut p, |n| matches!(n, NodeType::Fetch(f) if is_entity(&f.template_id, "Holding")));
    let archive = node_id_of(&mut p, |n| matches!(n, NodeType::Exercise(x) if x.choice_id == "Archive"));
    let mut fetch2 = node_mut(&mut p, &fetch).clone();
    if let NodeType::Fetch(f) = &mut fetch2 {
        f.contract_id = SECOND_INPUT.into();
    }
    let mut archive2 = node_mut(&mut p, &archive).clone();
    if let NodeType::Exercise(x) = &mut archive2 {
        x.contract_id = SECOND_INPUT.into();
    }
    push_node(&mut p, "901", fetch2);
    push_node(&mut p, "902", archive2);
    {
        let seeds = &mut p.transaction.as_mut().expect("transaction").node_seeds;
        let from: i32 = archive.parse().expect("node id");
        let mut seed = seeds.iter().find(|s| s.node_id == from).expect("seed").clone();
        seed.node_id = 902;
        seed.seed[0] ^= 1;
        seeds.push(seed);
    }
    nodes_mut(&mut p).retain(|n| n.node_id != config);
    {
        let internal = exercise_mut(&mut p, "AllocationFactory_TransferInternal");
        internal.children.retain(|id| *id != config);
        internal.children.insert(0, "901".into());
        internal.children.push("902".into());
    }
    let to_receiver = holding_id(&mut p, &receiver);
    {
        let h = holding_mut(&mut p, &to_receiver);
        *field_mut(h, "owner") = party(&sender);
        *field_mut(h, "amount") = numeric(&total);
    }
    if !keep_change {
        let change = creates_mut(&mut p)
            .find(|(id, c)| *id != to_receiver && owner_of(c) == sender)
            .map(|(id, _)| id)
            .expect("change");
        nodes_mut(&mut p).retain(|n| n.node_id != change);
        exercise_mut(&mut p, "AllocationFactory_TransferInternal").children.retain(|id| *id != change);
    }
    let OperationExpectation::TransferCip56 { instrument_id, instrument_admin, .. } = c.expectation.clone() else {
        panic!("not a transfer");
    };
    let exp = cip56(&sender, &sender, &instrument_id, &instrument_admin, &total, &[&input, SECOND_INPUT]);
    let bytes = common::encode(&p);
    (c, bytes, exp)
}

#[test]
fn cip56_merge_inspects_clean() {
    let (_, bytes, exp) = merge(false);
    assert_clean("merge", &bytes, &exp);
}

#[test]
fn cip56_merge_on_allocation_factory_passes_linked() {
    let (c, bytes, exp) = merge(false);
    let v = verify_linked(&c, &bytes, &exp);
    assert!(v.accepted, "{:?}", v.rejection_reason);
}

#[test]
fn cip56_merge_with_change_refuses() {
    let (_, bytes, exp) = merge(true);
    assert!(refusal(&bytes, &exp).is_some());
}

#[test]
fn cip56_merge_of_other_coins_refuses() {
    let (_, bytes, exp) = merge(false);
    let OperationExpectation::TransferCip56 { sender_party, instrument_id, instrument_admin, amount, .. } = exp else {
        panic!("not a transfer");
    };
    let exp = cip56(&sender_party, &sender_party, &instrument_id, &instrument_admin, &amount, &["00ff"]);
    assert!(refusal(&bytes, &exp).is_some());
}

const MERGE_MUTATIONS: &[TxMutation] = &[
    ("a factory of another registrar", |p| *field_mut(input_terms(p, FACTORY_CID), "registrar") = party(OTHER)),
    ("a factory that is not listed", |p| {
        let inputs = &mut p.metadata.as_mut().expect("metadata").input_contracts;
        inputs.retain(|ic| !matches!(ic.contract.as_ref(), Some(InputContractOneof::V1(c)) if c.contract_id == FACTORY_CID));
    }),
    ("an internal transfer of another payload", |p| {
        let internal = chosen_mut(exercise_mut(p, "AllocationFactory_TransferInternal"));
        *path_mut(internal, &["payload", "transfer", "amount"]) = numeric("1.0000000000");
    }),
    ("an internal transfer by a third party", |p| {
        exercise_mut(p, "AllocationFactory_TransferInternal").acting_parties.push(OTHER.into())
    }),
    ("a direct transfer under the factory", |p| {
        exercise_mut(p, "AllocationFactory_TransferInternal").choice_id = "TransferRule_DirectTransfer".into()
    }),
    ("a merged holding that loses value", |p| {
        let (sender, _) = sender_receiver(p);
        let id = holding_id(p, &sender);
        *field_mut(holding_mut(p, &id), "amount") = numeric("1.0000000000");
    }),
];

#[test]
fn cip56_merge_mutations_refuse() {
    let (_, base, exp) = merge(false);
    for (name, mutation) in MERGE_MUTATIONS {
        let mut p = PreparedTransaction::decode(base.as_slice()).expect("transaction");
        mutation(&mut p);
        assert!(refusal(&common::encode(&p), &exp).is_some(), "{name} is accepted");
    }
}

#[test]
fn cip56_send_on_allocation_factory_refuses() {
    let (_, bytes, exp) = merge(false);
    let OperationExpectation::TransferCip56 { sender_party, instrument_id, instrument_admin, amount, .. } = exp else {
        panic!("not a transfer");
    };
    // A factory transfer to another party is a two-step offer, not a merge.
    let exp = cip56(&sender_party, OTHER, &instrument_id, &instrument_admin, &amount, &[]);
    assert!(refusal(&bytes, &exp).is_some());
}

// ---------------------------------------------------------------------------
// Amulet through the CIP-56 path
// ---------------------------------------------------------------------------

/// The CC transfer as the CIP-56 builder sends Amulet: `executeBefore` at `window` after
/// `requestedAt`.
fn amulet_cip56(c: &Captured, window: i64) -> (Vec<u8>, OperationExpectation) {
    let mut p = common::prepared_transaction(c);
    let (sender, receiver) = sender_receiver(&mut p);
    let chosen = chosen_mut(root_mut(&mut p));
    let dso = party_of(chosen, "expectedAdmin");
    let transfer = rec_mut(field_mut(chosen, "transfer"));
    let requested = match sum_of(transfer, "requestedAt") {
        Sum::Timestamp(t) => *t,
        _ => panic!("requestedAt"),
    };
    *field_mut(transfer, "executeBefore") = Value { sum: Some(Sum::Timestamp(requested + window)) };
    let amount = numeric_of(transfer, "amount");
    (common::encode(&p), cip56(&sender, &receiver, "Amulet", &dso, &amount, &[]))
}

#[test]
fn transfer_cip56_of_amulet_inspects_clean() {
    for c in common::load("transfer_cc") {
        for window in [7 * DAY_MICROS, 7 * DAY_MICROS + 10_000_000] {
            let (bytes, exp) = amulet_cip56(&c, window);
            assert_clean(&c.fixture_id, &bytes, &exp);
        }
    }
}

#[test]
fn transfer_cip56_of_amulet_mutations_refuse() {
    for c in common::load("transfer_cc") {
        let (bytes, exp) = amulet_cip56(&c, 8 * DAY_MICROS);
        assert!(refusal(&bytes, &exp).is_some(), "{}: an 8-day window is accepted", c.fixture_id);
        let (bytes, exp) = amulet_cip56(&c, 7 * DAY_MICROS);
        let OperationExpectation::TransferCip56 { sender_party, receiver_party, instrument_admin, amount, .. } = exp else {
            panic!("not a transfer");
        };
        for (name, admin, inputs, to) in [
            ("another admin", OTHER, vec![], receiver_party.as_str()),
            ("other coins", instrument_admin.as_str(), vec!["00ff"], receiver_party.as_str()),
            ("another receiver", instrument_admin.as_str(), vec![], OTHER),
        ] {
            let exp = cip56(&sender_party, to, "Amulet", admin, &amount, &inputs);
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Preapproval requests
// ---------------------------------------------------------------------------

#[test]
fn request_preapproval_hash_and_facts() {
    common::assert_hash_and_facts("request_preapproval");
}

#[test]
fn request_preapproval_inspects_clean() {
    common::assert_inspects_clean("request_preapproval");
    for c in common::load("request_preapproval") {
        assert_clean(&c.fixture_id, &c.prepared, &as_built(&c));
    }
}

fn preapproval_mut(p: &mut PreparedTransaction) -> &mut Create {
    creates_mut(p).next().map(|(_, c)| c).expect("create")
}

const PREAPPROVAL_MUTATIONS: &[TxMutation] = &[
    ("the operator", |p| *field_mut(rec_mut(preapproval_mut(p).argument.as_mut().expect("arg")), "operator") = party(OTHER)),
    ("the admin", |p| {
        *field_mut(rec_mut(preapproval_mut(p).argument.as_mut().expect("arg")), "instrumentAdmin") = party(OTHER)
    }),
    ("an allowance", |p| {
        let arg = rec_mut(preapproval_mut(p).argument.as_mut().expect("arg"));
        if let Some(Sum::List(l)) = field_mut(arg, "instrumentAllowances").sum.as_mut() {
            l.elements.push(text("USDC"));
        }
    }),
    ("an extra signatory", |p| preapproval_mut(p).signatories.push(OTHER.into())),
    ("an extra node", |p| {
        let mut n = nodes_mut(p)[0].clone();
        n.node_id = "1".into();
        nodes_mut(p).push(n);
    }),
    ("a second submitter", |p| {
        p.metadata.as_mut().expect("metadata").submitter_info.as_mut().expect("submitter").act_as.push(OTHER.into())
    }),
];

#[test]
fn request_preapproval_mutations_refuse() {
    for c in common::load("request_preapproval") {
        let exp = as_built(&c);
        for (name, mutation) in PREAPPROVAL_MUTATIONS {
            let bytes = mutated(&c, mutation);
            assert!(refusal(&bytes, &exp).is_some(), "{}: {name} is accepted", c.fixture_id);
        }
    }
}

#[test]
fn a_cip56_transfer_names_its_reason_as_its_memo() {
    for c in common::load("transfer_cip56") {
        assert!(common::facts(&c).memos.is_empty(), "{}: an empty reason is no memo", c.fixture_id);
        let mut p = common::prepared_transaction(&c);
        let values = path_mut(chosen_mut(root_mut(&mut p)), &["transfer", "meta", "values"]);
        let Some(Sum::TextMap(map)) = values.sum.as_mut() else {
            panic!("{}: meta values are not a text map", c.fixture_id);
        };
        let entry = map
            .entries
            .iter_mut()
            .find(|e| e.key == tx_verifier::facts::REASON_META_KEY)
            .unwrap_or_else(|| panic!("{}: no reason", c.fixture_id));
        entry.value = Some(text("buy-01"));
        let facts = tx_verifier::facts::extract_facts(&common::encode(&p), &c.party)
            .unwrap_or_else(|e| panic!("{}: facts: {e}", c.fixture_id));
        assert_eq!(facts.memos, ["buy-01"], "{}", c.fixture_id);
    }
}
