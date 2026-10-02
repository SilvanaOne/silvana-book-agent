//! PreparedTransaction fixture builders for the unit tests.

use prost::Message;

use crate::decode::canton_proto::com::daml::ledger::api::v2 as v2;
use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
use v2::interactive::metadata::{InputContract, SubmitterInfo};
use v2::interactive::transaction::v1::{node::NodeType, Create, Exercise, Node as V1Node, Rollback};
use v2::interactive::{daml_transaction, DamlTransaction, Metadata, PreparedTransaction};
use v2::value::Sum;
use v2::{Identifier, Record, RecordField, Value};

pub(crate) use daml_transaction::{Node, NodeSeed};

pub(crate) fn ident(module: &str, entity: &str) -> Identifier {
    Identifier {
        package_id: "pkg".into(),
        module_name: module.into(),
        entity_name: entity.into(),
    }
}

fn val(sum: Sum) -> Value {
    Value { sum: Some(sum) }
}

pub(crate) fn boolean(b: bool) -> Value {
    val(Sum::Bool(b))
}

pub(crate) fn party(s: &str) -> Value {
    val(Sum::Party(s.into()))
}

pub(crate) fn text(s: &str) -> Value {
    val(Sum::Text(s.into()))
}

pub(crate) fn numeric(s: &str) -> Value {
    val(Sum::Numeric(s.into()))
}

pub(crate) fn contract_id(s: &str) -> Value {
    val(Sum::ContractId(s.into()))
}

pub(crate) fn list(elements: Vec<Value>) -> Value {
    val(Sum::List(v2::List { elements }))
}

pub(crate) fn optional(inner: Option<Value>) -> Value {
    val(Sum::Optional(Box::new(v2::Optional {
        value: inner.map(Box::new),
    })))
}

pub(crate) fn record(fields: Vec<(&str, Value)>) -> Value {
    val(Sum::Record(Record {
        record_id: None,
        fields: fields
            .into_iter()
            .map(|(label, value)| RecordField {
                label: label.into(),
                value: Some(value),
            })
            .collect(),
    }))
}

/// Exercise with every field the hasher requires; `contract_id` "00" is valid hex.
pub(crate) fn exercise(choice: &str, acting: &[&str], children: Vec<String>) -> Exercise {
    Exercise {
        lf_version: "2.1".into(),
        contract_id: "00".into(),
        package_name: "pkg".into(),
        template_id: Some(ident("M", "T")),
        acting_parties: acting.iter().map(|p| p.to_string()).collect(),
        choice_id: choice.into(),
        chosen_value: Some(boolean(true)),
        consuming: true,
        children,
        ..Default::default()
    }
}

/// Create with every field the hasher requires.
pub(crate) fn create(template: Identifier, argument: Value, signatories: &[&str]) -> Create {
    Create {
        lf_version: "2.1".into(),
        contract_id: "00".into(),
        package_name: "pkg".into(),
        template_id: Some(template),
        argument: Some(argument),
        signatories: signatories.iter().map(|p| p.to_string()).collect(),
        ..Default::default()
    }
}

fn node(id: &str, node_type: NodeType) -> Node {
    Node {
        node_id: id.into(),
        versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node {
            node_type: Some(node_type),
        })),
    }
}

pub(crate) fn exercise_node(id: &str, ex: Exercise) -> Node {
    node(id, NodeType::Exercise(ex))
}

pub(crate) fn create_node(id: &str, c: Create) -> Node {
    node(id, NodeType::Create(c))
}

pub(crate) fn rollback_node(id: &str, children: Vec<String>) -> Node {
    node(id, NodeType::Rollback(Rollback { children }))
}

pub(crate) fn seed(node_id: i32, byte: u8) -> NodeSeed {
    NodeSeed {
        node_id,
        seed: vec![byte; 32],
    }
}

pub(crate) fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

pub(crate) fn prepared(
    roots: &[&str],
    nodes: Vec<Node>,
    node_seeds: Vec<NodeSeed>,
    act_as: &[&str],
) -> PreparedTransaction {
    PreparedTransaction {
        transaction: Some(DamlTransaction {
            version: "2.1".into(),
            roots: ids(roots),
            nodes,
            node_seeds,
        }),
        metadata: Some(Metadata {
            submitter_info: Some(SubmitterInfo {
                act_as: ids(act_as),
                command_id: "cmd".into(),
            }),
            ..Default::default()
        }),
    }
}

/// Adds a disclosed contract to the metadata.
pub(crate) fn with_input_contract(mut p: PreparedTransaction, c: Create) -> PreparedTransaction {
    if let Some(m) = p.metadata.as_mut() {
        m.input_contracts.push(InputContract {
            created_at: 1,
            event_blob: vec![1, 2, 3],
            contract: Some(InputContractOneof::V1(c)),
        });
    }
    p
}

pub(crate) fn encode(p: &PreparedTransaction) -> Vec<u8> {
    p.encode_to_vec()
}
