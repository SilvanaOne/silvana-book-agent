//! Drills of the direct-command executor on a fake ledger and a fake rpc: what fails its
//! binding is never signed, and a signed command is never signed again.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};

use agent_control::policy::{LimitItem, PolicyInstrument};
use agent_control::{
    CodeSource, ControllerProof, ControllerType, DEFAULT_LINKED_RULES, EntryBuilder, EthAddress,
    HeadRef, Network, PolicyDocument, Receipt, ReceiptOutcome, ReceiptSubject, Rules, Text,
    controller_id_for,
};
use agent_logic::control_store::testing::TestChain;
use ed25519_dalek::{Signer, SigningKey};
use orderbook_proto::orderbook::ListPendingControlResponse;
use rust_decimal::Decimal;
use tx_verifier::VerifyMode;

use super::*;
use crate::control::poller::{self, Memo};
use crate::control::prompt::Scripted;
use crate::control::test_support::{FakeRpc, TestAgent};
use crate::ledger_client::EXECUTE_OUTCOME_UNKNOWN;

/// Prepared transactions in the shapes the SDK's transfers take, hashed and read as facts.
pub(crate) mod fixture {
    use prost::Message;
    use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2;
    use v2::interactive::metadata::input_contract::Contract as InputContractOneof;
    use v2::interactive::metadata::{InputContract, SubmitterInfo};
    use v2::interactive::transaction::v1::{
        Create, Exercise, Fetch, Node as V1Node, node::NodeType,
    };
    use v2::interactive::{DamlTransaction, Metadata, PreparedTransaction, daml_transaction};
    use v2::value::Sum;
    use v2::{Identifier, Record, RecordField, Value};

    pub(crate) const PKG: &str = "5f6c3e7d9a1b2c4d8e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d";

    /// `requestedAt` of every transfer; `executeBefore` follows 40 s later, as the SDK sets it.
    const REQUESTED_AT: i64 = 1_795_489_110_609_724;
    const EXECUTE_WITHIN: i64 = 40_000_000;

    const AMULET: (&str, &str) = ("Splice.Amulet", "Amulet");
    const AMULET_RULES: (&str, &str) = ("Splice.AmuletRules", "AmuletRules");
    const RULES: (&str, &str) = (
        "Splice.ExternalPartyAmuletRules",
        "ExternalPartyAmuletRules",
    );
    const CONFIG: (&str, &str) = (
        "Splice.ExternalPartyConfigState",
        "ExternalPartyConfigState",
    );
    const PREAPPROVAL: (&str, &str) = ("Splice.AmuletRules", "TransferPreapproval");
    const FACTORY: (&str, &str) = ("Splice.Api.Token.TransferInstructionV1", "TransferFactory");
    const EVENT_LOG: (&str, &str) = ("Splice.Api.Token.TransferEventsV2", "EventLog");
    const HOLDING: (&str, &str) = ("Utility.Registry.Holding.V0.Holding", "Holding");
    const TOKEN_PREAPPROVAL: (&str, &str) = (
        "Utility.Registry.App.V0.Model.TransferPreapproval",
        "TransferPreapproval",
    );
    const TRANSFER_RULE: (&str, &str) = ("Utility.Registry.V0.Rule.Transfer", "TransferRule");
    const ALLOCATION_FACTORY: (&str, &str) = (
        "Utility.Registry.App.V0.Service.AllocationFactory",
        "AllocationFactory",
    );

    /// The spent coin or holding, the payment and the change of every transfer.
    const INPUT: &str = "00d101";
    const PAID: &str = "00e101";
    const CHANGE: &str = "00e201";

    pub(crate) fn party(hint: &str, byte: u8) -> String {
        format!("{hint}::1220{}", hex::encode([byte; 32]))
    }

    pub(crate) fn dso() -> String {
        party("dso", 0xdd)
    }

    pub(crate) fn bob() -> String {
        party("bob", 0xbb)
    }

    pub(crate) fn eve() -> String {
        party("eve", 0xee)
    }

    pub(crate) fn registry() -> String {
        party("registry", 0xcc)
    }

    /// The app provider of the preapprovals and the activity marker.
    fn provider() -> String {
        party("provider", 0xa1)
    }

    fn ident((module, entity): (&str, &str)) -> Identifier {
        Identifier {
            package_id: PKG.into(),
            module_name: module.into(),
            entity_name: entity.into(),
        }
    }

    fn val(sum: Sum) -> Value {
        Value { sum: Some(sum) }
    }

    fn record(fields: Vec<(&str, Value)>) -> Value {
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

    fn party_v(p: &str) -> Value {
        val(Sum::Party(p.into()))
    }

    fn numeric(n: &str) -> Value {
        val(Sum::Numeric(n.into()))
    }

    fn text(t: &str) -> Value {
        val(Sum::Text(t.into()))
    }

    fn cid_v(cid: &str) -> Value {
        val(Sum::ContractId(cid.into()))
    }

    fn list(elements: Vec<Value>) -> Value {
        val(Sum::List(v2::List { elements }))
    }

    fn optional(value: Option<Value>) -> Value {
        val(Sum::Optional(Box::new(v2::Optional {
            value: value.map(Box::new),
        })))
    }

    fn variant(constructor: &str, value: Value) -> Value {
        val(Sum::Variant(Box::new(v2::Variant {
            variant_id: None,
            constructor: constructor.into(),
            value: Some(Box::new(value)),
        })))
    }

    fn text_map(entries: Vec<(&str, Value)>) -> Value {
        val(Sum::TextMap(v2::TextMap {
            entries: entries
                .into_iter()
                .map(|(key, value)| v2::text_map::Entry {
                    key: key.into(),
                    value: Some(value),
                })
                .collect(),
        }))
    }

    /// An empty `Metadata`.
    fn meta() -> Value {
        record(vec![("values", text_map(Vec::new()))])
    }

    fn create(cid: &str, template: (&str, &str), argument: Value, signatories: &[&str]) -> Create {
        Create {
            lf_version: "2.1".into(),
            contract_id: cid.into(),
            package_name: "pkg".into(),
            template_id: Some(ident(template)),
            argument: Some(argument),
            signatories: signatories.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }

    fn exercise(
        cid: &str,
        template: (&str, &str),
        choice: &str,
        acting: &str,
        consuming: bool,
        chosen: Value,
        children: &[&str],
    ) -> Exercise {
        Exercise {
            lf_version: "2.1".into(),
            contract_id: cid.into(),
            package_name: "pkg".into(),
            template_id: Some(ident(template)),
            acting_parties: vec![acting.to_string()],
            choice_id: choice.into(),
            chosen_value: Some(chosen),
            consuming,
            children: children.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    fn fetch(cid: &str, template: (&str, &str), acting: &str) -> Fetch {
        Fetch {
            lf_version: "2.1".into(),
            contract_id: cid.into(),
            package_name: "pkg".into(),
            template_id: Some(ident(template)),
            acting_parties: vec![acting.to_string()],
            ..Default::default()
        }
    }

    pub(crate) fn node(id: &str, node_type: NodeType) -> daml_transaction::Node {
        daml_transaction::Node {
            node_id: id.into(),
            versioned_node: Some(daml_transaction::node::VersionedNode::V1(V1Node {
                node_type: Some(node_type),
            })),
        }
    }

    pub(crate) fn amulet(cid: &str, owner: &str, amount: &str) -> Create {
        let dso = dso();
        let arg = record(vec![
            ("dso", party_v(&dso)),
            ("owner", party_v(owner)),
            ("amount", record(vec![("initialAmount", numeric(amount))])),
        ]);
        create(cid, AMULET, arg, &[&dso, owner])
    }

    pub(crate) fn holding(cid: &str, owner: &str, id: &str, amount: &str) -> Create {
        let registry = registry();
        let arg = record(vec![
            ("registrar", party_v(&registry)),
            ("owner", party_v(owner)),
            (
                "instrument",
                record(vec![
                    ("source", party_v(&registry)),
                    ("id", text(id)),
                    ("scheme", text("s")),
                ]),
            ),
            ("amount", numeric(amount)),
        ]);
        create(cid, HOLDING, arg, &[&registry, owner])
    }

    fn config_state(cid: &str) -> Create {
        let dso = dso();
        create(cid, CONFIG, record(vec![("dso", party_v(&dso))]), &[&dso])
    }

    /// Whether the shape returns change to the agent.
    fn has_change(s: &Shape) -> bool {
        rust_decimal::Decimal::from_str_exact(&s.change).map_or(true, |d| !d.is_zero())
    }

    /// The `transfer` record of a `TransferFactory_Transfer`, spending [`INPUT`].
    fn transfer_v(s: &Shape, admin: &str, id: &str) -> Value {
        record(vec![
            ("sender", party_v(&s.agent)),
            ("receiver", party_v(&s.declared)),
            ("amount", numeric(&s.amount)),
            (
                "instrumentId",
                record(vec![("admin", party_v(admin)), ("id", text(id))]),
            ),
            ("requestedAt", val(Sum::Timestamp(REQUESTED_AT))),
            (
                "executeBefore",
                val(Sum::Timestamp(REQUESTED_AT + EXECUTE_WITHIN)),
            ),
            ("inputHoldingCids", list(vec![cid_v(INPUT)])),
            ("meta", meta()),
        ])
    }

    fn extra_args(context: Vec<(&str, Value)>) -> Value {
        record(vec![
            ("context", record(vec![("values", text_map(context))])),
            ("meta", meta()),
        ])
    }

    /// A `TransferFactory_Transfer` on `cid` of `template`, through the factory interface,
    /// that completed with [`PAID`] and, when there is any, [`CHANGE`].
    fn factory_transfer(
        s: &Shape,
        (cid, template): (&str, (&str, &str)),
        (admin, id): (&str, &str),
        context: Vec<(&str, Value)>,
        children: &[&str],
    ) -> Exercise {
        let chosen = record(vec![
            ("expectedAdmin", party_v(admin)),
            ("transfer", transfer_v(s, admin, id)),
            ("extraArgs", extra_args(context)),
        ]);
        let mut root = exercise(cid, template, "TransferFactory_Transfer", &s.agent, false, chosen, children);
        root.interface_id = Some(ident(FACTORY));
        let change: Vec<Value> = if has_change(s) { vec![cid_v(CHANGE)] } else { Vec::new() };
        root.exercise_result = Some(record(vec![
            (
                "output",
                variant(
                    "TransferInstructionResult_Completed",
                    record(vec![("receiverHoldingCids", list(vec![cid_v(PAID)]))]),
                ),
            ),
            ("senderChangeCids", list(change)),
            ("meta", meta()),
        ]));
        root
    }

    /// The DSO's non-consuming holdings log on `cid`, which moves nothing.
    fn holdings_log(cid: &str, template: (&str, &str)) -> Exercise {
        let mut log = exercise(cid, template, "EventLog_HoldingsChange", &dso(), false, record(vec![]), &[]);
        log.interface_id = Some(ident(EVENT_LOG));
        log
    }

    fn encode(
        roots: &[&str],
        nodes: Vec<daml_transaction::Node>,
        inputs: Vec<Create>,
        act_as: &str,
        command_id: &str,
    ) -> Vec<u8> {
        let mut prepared = PreparedTransaction {
            transaction: Some(DamlTransaction {
                version: "2.1".into(),
                roots: roots.iter().map(|r| r.to_string()).collect(),
                nodes,
                node_seeds: Vec::new(),
            }),
            metadata: Some(Metadata {
                submitter_info: Some(SubmitterInfo {
                    act_as: vec![act_as.to_string()],
                    command_id: command_id.to_string(),
                }),
                input_contracts: inputs
                    .into_iter()
                    .map(|c| InputContract {
                        created_at: 1,
                        event_blob: vec![1, 2, 3],
                        contract: Some(InputContractOneof::V1(c)),
                    })
                    .collect(),
                ..Default::default()
            }),
        };
        seed(&mut prepared);
        prepared.encode_to_vec()
    }

    /// Seeds every node, so the hash computes.
    fn seed(prepared: &mut PreparedTransaction) {
        if let Some(tx) = prepared.transaction.as_mut() {
            tx.node_seeds = tx
                .nodes
                .iter()
                .map(|n| daml_transaction::NodeSeed {
                    node_id: n.node_id.parse().unwrap(),
                    seed: vec![7u8; 32],
                })
                .collect();
        }
    }

    /// `bytes` as `change` rewrites them, every node seeded again.
    pub(crate) fn rewritten(bytes: &[u8], change: impl FnOnce(&mut PreparedTransaction)) -> Vec<u8> {
        let mut prepared = PreparedTransaction::decode(bytes).unwrap();
        change(&mut prepared);
        seed(&mut prepared);
        prepared.encode_to_vec()
    }

    /// The exercise of node `id`.
    pub(crate) fn exercise_mut<'a>(prepared: &'a mut PreparedTransaction, id: &str) -> Option<&'a mut Exercise> {
        let tx = prepared.transaction.as_mut()?;
        let node = tx.nodes.iter_mut().find(|n| n.node_id == id)?;
        match node.versioned_node.as_mut()? {
            daml_transaction::node::VersionedNode::V1(V1Node { node_type: Some(NodeType::Exercise(e)) }) => Some(e),
            _ => None,
        }
    }

    /// One transfer and what a ledger-service may change in it.
    #[derive(Clone, Debug)]
    pub(crate) struct Shape {
        pub agent: String,
        /// Who the created holding goes to.
        pub paid: String,
        /// Who the transfer's argument names.
        pub declared: String,
        pub amount: String,
        pub input: String,
        pub change: String,
        /// A further holding created for someone else.
        pub extra: Option<(String, String)>,
        /// A second root that moves nothing.
        pub second_root: bool,
        pub command_id: String,
    }

    /// A CC transfer through the external party's amulet rules, completed by a
    /// `TransferPreapproval_SendV2` on the declared receiver's preapproval.
    pub(crate) fn cc_transfer(s: &Shape) -> Vec<u8> {
        let dso = dso();
        let any_cid = |cid: &str| variant("AV_ContractId", cid_v(cid));
        let context = vec![
            ("amulet-rules", any_cid("00a201")),
            ("external-party-config-state", any_cid("00c101")),
            ("featured-app-right", any_cid("00b201")),
            ("open-round", any_cid("00a301")),
            ("transfer-preapproval", any_cid("00b101")),
        ];
        let root = factory_transfer(s, ("00a101", RULES), (&dso, "Amulet"), context, &["1", "2"]);
        let send = record(vec![
            (
                "context",
                record(vec![
                    ("externalPartyConfigState", cid_v("00c101")),
                    ("featuredAppRight", optional(Some(cid_v("00b201")))),
                ]),
            ),
            ("inputs", list(vec![variant("InputAmulet", cid_v(INPUT))])),
            ("amount", numeric(&s.amount)),
            ("sender", party_v(&s.agent)),
            ("description", optional(None)),
            ("meta", optional(None)),
        ]);
        let change = has_change(s);
        let mut sent = vec!["3", "4", "5", "6"];
        if change {
            sent.push("7");
        }
        sent.push("8");
        if s.extra.is_some() {
            sent.push("9");
        }
        let marker = record(vec![
            ("dso", party_v(&dso)),
            ("provider", party_v(&provider())),
            ("beneficiary", party_v(&provider())),
            ("weight", numeric("1.0")),
        ]);
        let mut nodes = vec![
            node("0", NodeType::Exercise(root)),
            node("1", NodeType::Fetch(fetch("00b101", PREAPPROVAL, &dso))),
            node(
                "2",
                NodeType::Exercise(exercise(
                    "00b101",
                    PREAPPROVAL,
                    "TransferPreapproval_SendV2",
                    &s.agent,
                    false,
                    send,
                    &sent,
                )),
            ),
            node("3", NodeType::Fetch(fetch("00c101", CONFIG, &dso))),
            node(
                "4",
                NodeType::Exercise(exercise(INPUT, AMULET, "Archive", &s.agent, true, record(vec![]), &[])),
            ),
            node(
                "5",
                NodeType::Create(create(
                    "00e301",
                    ("Splice.Amulet", "FeaturedAppActivityMarker"),
                    marker,
                    &[&dso],
                )),
            ),
            node("6", NodeType::Create(amulet(PAID, &s.paid, &s.amount))),
        ];
        if change {
            nodes.push(node("7", NodeType::Create(amulet(CHANGE, &s.agent, &s.change))));
        }
        nodes.push(node("8", NodeType::Exercise(holdings_log("00c101", CONFIG))));
        if let Some((to, amount)) = &s.extra {
            nodes.push(node("9", NodeType::Create(amulet("00e401", to, amount))));
        }
        let mut roots = vec!["0"];
        if s.second_root {
            roots.push("10");
            nodes.push(node("10", NodeType::Fetch(fetch("00c101", CONFIG, &s.agent))));
        }
        let preapproval = record(vec![
            ("dso", party_v(&dso)),
            ("receiver", party_v(&s.declared)),
            ("provider", party_v(&provider())),
        ]);
        let featured = record(vec![("dso", party_v(&dso)), ("provider", party_v(&provider()))]);
        let inputs = vec![
            create("00a101", RULES, record(vec![("dso", party_v(&dso))]), &[&dso]),
            config_state("00c101"),
            create("00b101", PREAPPROVAL, preapproval, &[&s.declared, &dso]),
            create("00b201", ("Splice.Amulet", "FeaturedAppRight"), featured, &[&dso]),
            amulet(INPUT, &s.agent, &s.input),
        ];
        encode(&roots, nodes, inputs, &s.agent, &s.command_id)
    }

    /// A CIP-56 transfer of a registry holding on the declared receiver's preapproval,
    /// completed by a direct transfer; a transfer to the agent itself is a [`token_merge`].
    pub(crate) fn token_transfer(s: &Shape, id: &str) -> Vec<u8> {
        if s.declared == s.agent {
            return token_merge(s, id);
        }
        let registry = registry();
        let any_cid = |cid: &str| variant("AV_ContractId", cid_v(cid));
        let context = || vec![("utility.digitalasset.com/instrument-configuration", any_cid("00f301"))];
        let root = factory_transfer(s, ("00f101", TOKEN_PREAPPROVAL), (&registry, id), context(), &["1"]);
        let chosen = record(vec![
            ("transfer", transfer_v(s, &registry, id)),
            ("extraArgs", extra_args(context())),
        ]);
        let change = has_change(s);
        let moved: &[&str] = if change { &["2", "3", "4"] } else { &["2", "3"] };
        let mut direct = exercise("00f201", TRANSFER_RULE, "TransferRule_DirectTransfer", &s.agent, false, chosen, moved);
        direct.acting_parties.push(s.declared.clone());
        let mut nodes = vec![
            node("0", NodeType::Exercise(root)),
            node("1", NodeType::Exercise(direct)),
            node(
                "2",
                NodeType::Exercise(exercise(INPUT, HOLDING, "Archive", &s.agent, true, record(vec![]), &[])),
            ),
            node("3", NodeType::Create(holding(PAID, &s.paid, id, &s.amount))),
        ];
        if change {
            nodes.push(node("4", NodeType::Create(holding(CHANGE, &s.agent, id, &s.change))));
        }
        let preapproval = record(vec![
            ("operator", party_v(&provider())),
            ("receiver", party_v(&s.declared)),
            ("instrumentAdmin", party_v(&registry)),
            ("instrumentAllowances", list(Vec::new())),
        ]);
        let rule = record(vec![
            ("operator", party_v(&provider())),
            ("provider", party_v(&registry)),
            ("registrar", party_v(&registry)),
        ]);
        let inputs = vec![
            holding(INPUT, &s.agent, id, &s.input),
            create("00f101", TOKEN_PREAPPROVAL, preapproval, &[&s.declared]),
            create("00f201", TRANSFER_RULE, rule, &[&registry]),
        ];
        encode(&["0"], nodes, inputs, &s.agent, &s.command_id)
    }

    /// The agent's transfer to itself on the registry's allocation factory: one internal
    /// transfer of the same payload merges [`INPUT`] into one holding, with change if it splits.
    fn token_merge(s: &Shape, id: &str) -> Vec<u8> {
        let registry = registry();
        let any_cid = |cid: &str| variant("AV_ContractId", cid_v(cid));
        let context = vec![("utility.digitalasset.com/instrument-configuration", any_cid("00f301"))];
        let root = factory_transfer(s, ("00f401", ALLOCATION_FACTORY), (&registry, id), context, &["1"]);
        let payload = root.chosen_value.clone().expect("the transfer has its argument");
        let change = has_change(s);
        let moved: &[&str] = if change { &["2", "3", "4", "5"] } else { &["2", "3", "4"] };
        let chosen = record(vec![("payload", payload)]);
        let mut internal = exercise("00f401", ALLOCATION_FACTORY, "AllocationFactory_TransferInternal", &registry, false, chosen, moved);
        internal.acting_parties.push(s.agent.clone());
        internal.exercise_result = root.exercise_result.clone();
        let mut nodes = vec![
            node("0", NodeType::Exercise(root)),
            node("1", NodeType::Exercise(internal)),
            node("2", NodeType::Fetch(fetch(INPUT, HOLDING, &s.agent))),
            node(
                "3",
                NodeType::Exercise(exercise(INPUT, HOLDING, "Archive", &s.agent, true, record(vec![]), &[])),
            ),
            node("4", NodeType::Create(holding(PAID, &s.paid, id, &s.amount))),
        ];
        if change {
            nodes.push(node("5", NodeType::Create(holding(CHANGE, &s.agent, id, &s.change))));
        }
        let factory = record(vec![
            ("provider", party_v(&registry)),
            ("registrar", party_v(&registry)),
            ("operator", party_v(&provider())),
        ]);
        let inputs = vec![
            holding(INPUT, &s.agent, id, &s.input),
            create("00f401", ALLOCATION_FACTORY, factory, &[&registry]),
        ];
        encode(&["0"], nodes, inputs, &s.agent, &s.command_id)
    }

    /// The agent's coin [`INPUT`] of `input` split into its own `outputs` and `change`.
    pub(crate) fn cc_split(agent: &str, outputs: &[&str], input: &str, change: &str, command_id: &str) -> Vec<u8> {
        let dso = dso();
        let output = |amount: &str| {
            record(vec![
                ("receiver", party_v(agent)),
                ("receiverFeeRatio", numeric("0.0")),
                ("amount", numeric(amount)),
            ])
        };
        let transfer = record(vec![
            ("sender", party_v(agent)),
            ("provider", party_v(agent)),
            ("inputs", list(vec![variant("InputAmulet", cid_v(INPUT))])),
            ("outputs", list(outputs.iter().map(|a| output(a)).collect())),
        ]);
        let chosen = record(vec![("transfer", transfer), ("expectedDso", optional(Some(party_v(&dso))))]);
        let archive = exercise(INPUT, AMULET, "Archive", agent, true, record(vec![]), &[]);
        let mut below = vec![NodeType::Exercise(archive)];
        for (n, amount) in outputs.iter().enumerate() {
            below.push(NodeType::Create(amulet(&format!("00e1{n:02}"), agent, amount)));
        }
        below.push(NodeType::Create(amulet(CHANGE, agent, change)));
        below.push(NodeType::Exercise(holdings_log("00a401", AMULET_RULES)));
        let ids: Vec<String> = (1..=below.len()).map(|n| n.to_string()).collect();
        let children: Vec<&str> = ids.iter().map(String::as_str).collect();
        let root = exercise("00a401", AMULET_RULES, "AmuletRules_Transfer", agent, false, chosen, &children);
        let mut nodes = vec![node("0", NodeType::Exercise(root))];
        nodes.extend(children.iter().zip(below).map(|(id, n)| node(id, n)));
        let rules = create("00a401", AMULET_RULES, record(vec![("dso", party_v(&dso))]), &[&dso]);
        encode(&["0"], nodes, vec![rules, amulet(INPUT, agent, input)], agent, command_id)
    }
}

use fixture::Shape;

const CONTROLLER: u8 = 0x21;

fn now() -> UtcTime {
    UtcTime::from_unix_secs(agent_logic::clock::now_secs_i64()).unwrap()
}

fn amount(s: &str) -> Amount {
    Amount::parse(s).unwrap()
}

fn transfer_cc(to: &str, how_much: &str) -> CommandOp {
    CommandOp::TransferCc {
        receiver: PartyId::parse(to).unwrap(),
        amount: amount(how_much),
        description: None,
        settlement_proposal_id: None,
    }
}

fn transfer_token(to: &str, id: &str, how_much: &str) -> CommandOp {
    CommandOp::TransferToken {
        receiver: PartyId::parse(to).unwrap(),
        instrument: InstrumentRef {
            admin: PartyId::parse(&fixture::registry()).unwrap(),
            id: InstrumentId::parse(id).unwrap(),
        },
        amount: amount(how_much),
        reference: None,
    }
}

/// A command the linked controller signed at `head`, as ListPendingControl returns it.
fn command_item(
    agent: &TestAgent,
    op: CommandOp,
    issued_at: UtcTime,
    valid_secs: i64,
    head: HeadRef,
) -> PendingCommand {
    signed_command(&agent.party, op, issued_at, valid_secs, head)
}

/// A fresh command id: a counter in uuid7 form.
fn next_command_id() -> Uuid7 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    Uuid7::parse(&format!("019abc00-0000-7000-8000-{n:012x}")).unwrap()
}

/// A command for `party` that the test chains' linked controller signed at `head`.
pub(crate) fn signed_command(
    party: &PartyId,
    op: CommandOp,
    issued_at: UtcTime,
    valid_secs: i64,
    head: HeadRef,
) -> PendingCommand {
    let command_id = next_command_id();
    let command = Command {
        network: Network::Devnet,
        agent_party: party.clone(),
        command_id: command_id.clone(),
        log_seq: head.seq,
        log_head: head.hash,
        issued_at,
        expires_at: issued_at.checked_add_secs(valid_secs).unwrap(),
        op,
        summary: Text::parse("drill").unwrap(),
    };
    let bytes = command.encode().unwrap();
    let key = SigningKey::from_bytes(&[CONTROLLER; 32]);
    let controller = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
    let proof = ProofEnvelope::new(
        controller,
        &bytes,
        ControllerProof::Ed25519 {
            signature: key.sign(&bytes).to_bytes(),
        },
    )
    .encode()
    .unwrap();
    PendingCommand {
        command_id: command_id.to_string(),
        command: bytes,
        controller_id: controller.to_string(),
        proof,
        submitted_at: None,
    }
}

/// The rpc as a command sees it: a listing, the receipts posted to it, and its clock.
#[derive(Default)]
struct Receipts {
    listed: Mutex<Vec<PendingCommand>>,
    posted: Mutex<Vec<(Vec<u8>, [u8; 64])>>,
    down: AtomicBool,
    /// How far, in seconds, the rpc clock runs ahead of this one.
    ahead: Arc<AtomicI64>,
    syncs: AtomicU32,
}

impl Receipts {
    fn listing(items: Vec<PendingCommand>) -> Arc<Self> {
        let receipts = Self::default();
        *agent_logic::sync::lock(&receipts.listed) = items;
        Arc::new(receipts)
    }

    fn posted(&self) -> Vec<(Vec<u8>, [u8; 64])> {
        agent_logic::sync::lock(&self.posted).clone()
    }

    /// The last receipt, verified for `party`.
    fn last(&self, party: &PartyId) -> Receipt {
        let (bytes, signature) = self.posted().last().cloned().expect("a receipt was posted");
        Receipt::verify(&bytes, &signature, party).unwrap()
    }
}

#[async_trait::async_trait]
impl CommandRpc for Receipts {
    fn checked_now(&self) -> Result<UtcTime> {
        now()
            .checked_add_secs(self.ahead.load(Ordering::SeqCst))
            .context("the clock")
    }

    async fn pending_commands(&self) -> Result<Vec<PendingCommand>, RpcError> {
        Ok(agent_logic::sync::lock(&self.listed).clone())
    }

    async fn post_receipt(&self, receipt: Vec<u8>, signature: [u8; 64]) -> Result<(), RpcError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(RpcError::Unreachable("down".into()));
        }
        agent_logic::sync::lock(&self.posted).push((receipt, signature));
        Ok(())
    }

    async fn sync_log(&self, _ctx: &ControlContext) -> Result<()> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// What the fake ledger saw.
#[derive(Default)]
struct Counts {
    prepares: AtomicU32,
    signatures: AtomicU32,
    scans: AtomicU32,
    remints: AtomicU32,
    /// Why each refused transaction was refused before its signature.
    refusals: Mutex<Vec<String>>,
}

impl Counts {
    fn get(&self) -> (u32, u32) {
        (
            self.prepares.load(Ordering::SeqCst),
            self.signatures.load(Ordering::SeqCst),
        )
    }

    fn refused(&self, reason: String) -> anyhow::Error {
        agent_logic::sync::lock(&self.refusals).push(reason.clone());
        anyhow!(reason)
    }

    fn refusals(&self) -> Vec<String> {
        agent_logic::sync::lock(&self.refusals).clone()
    }
}

#[derive(Clone, Debug)]
enum End {
    Executed(&'static str),
    Ambiguous,
    /// The execute fails with this text after the signature.
    Failing(&'static str),
}

type Build = Arc<dyn Fn(&PrepareTransactionRequest, &str) -> Vec<u8> + Send + Sync>;

/// A ledger-service that prepares what `build` returns, runs the agent's own checks and
/// counts a signature only once the binding passed.
struct FakeLedger {
    party: String,
    build: Build,
    end: End,
    found: Option<String>,
    prepare_refusal: Option<String>,
    counts: Arc<Counts>,
    /// Moves the rpc clock this far ahead right before the binding runs.
    jump: Option<(Arc<AtomicI64>, i64)>,
    /// The next prepare only fails with this.
    refuse_once: Option<String>,
}

fn change_of(input: &str, paid: &str) -> String {
    let rest = Decimal::from_str_exact(input).unwrap() - Decimal::from_str_exact(paid).unwrap();
    rest.normalize().to_string()
}

/// A transfer built from the request's own params, then altered by `alter`.
fn honest(
    alter: impl Fn(&mut Shape) + Send + Sync + 'static,
    token: Option<&'static str>,
) -> Build {
    Arc::new(move |req, ctl| {
        let (receiver, amount) = match &req.params {
            Some(Params::TransferCc(p)) => (p.receiver_party.clone(), p.amount.clone()),
            Some(Params::TransferCip56(p)) => (p.receiver_party.clone(), p.amount.clone()),
            other => panic!("not a transfer: {other:?}"),
        };
        let agent = match req
            .controller_command
            .as_ref()
            .map(|c| Command::decode(&c.canonical).unwrap())
        {
            Some(command) => command.agent_party.to_string(),
            None => panic!("a directed request carries its command"),
        };
        let mut shape = Shape {
            agent,
            paid: receiver.clone(),
            declared: receiver,
            change: change_of("100", &amount),
            amount,
            input: "100".into(),
            extra: None,
            second_root: false,
            command_id: ctl.to_string(),
        };
        alter(&mut shape);
        match token {
            Some(id) => fixture::token_transfer(&shape, id),
            None => fixture::cc_transfer(&shape),
        }
    })
}

impl FakeLedger {
    fn new(agent: &TestAgent, build: Build, end: End, counts: &Arc<Counts>) -> Self {
        Self {
            party: agent.party.to_string(),
            build,
            end,
            found: None,
            prepare_refusal: None,
            counts: Arc::clone(counts),
            jump: None,
            refuse_once: None,
        }
    }
}

#[async_trait::async_trait]
impl DirectedLedger for FakeLedger {
    async fn ledger_end(&mut self) -> Result<i64> {
        Ok(42)
    }

    async fn submit_directed(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        command: &Command,
        bind: PreSign<'_>,
    ) -> Result<ExecuteTransactionResponse> {
        self.counts.prepares.fetch_add(1, Ordering::SeqCst);
        if let Some(text) = self.prepare_refusal.as_ref().or(self.refuse_once.as_ref()) {
            let text = text.clone();
            self.refuse_once = None;
            bail!("PrepareTransaction failed: {text}");
        }
        let bytes = (self.build)(&req, &command.canton_command_id());
        let hash = tx_verifier::hasher::try_compute_hash(&bytes, "V2")?;
        let verification = tx_verifier::verify_and_hash_with(
            &bytes,
            &BASE64.encode(hash),
            "V2",
            expectation,
            VerifyMode { strict_all: true, ..VerifyMode::default() },
            false,
        )?;
        if !verification.accepted {
            let reason = verification.rejection_reason.clone().unwrap_or_default();
            return Err(self.counts.refused(format!("{VERIFICATION_REJECTED}: {reason}")));
        }
        let facts =
            tx_verifier::facts::extract_facts(&bytes, &self.party).map_err(|e| e.to_string());
        let platform = crate::control::platform_parties()?;
        let request = OwnRequest::Ledger(req.params.as_ref());
        intent::check_before_signing_directed(
            &self.party,
            &platform,
            expectation,
            request,
            &verification,
            &facts,
            command,
        )
        .map_err(|refusal| {
            let _ = self.counts.refused(refusal.to_string());
            anyhow!(refusal)
        })?;
        if let Some((ahead, secs)) = &self.jump {
            ahead.store(*secs, Ordering::SeqCst);
        }
        let canton_id = command.canton_command_id();
        bind(&PreSignContext {
            command_id: &canton_id,
            transaction_id: "tx-1",
            verification: &verification,
            facts: &facts,
            prepared_transaction: &bytes,
        })
        .context("refused before signing")?;
        self.counts.signatures.fetch_add(1, Ordering::SeqCst);
        match &self.end {
            End::Executed(update) => Ok(ExecuteTransactionResponse {
                success: true,
                update_id: update.to_string(),
                ..Default::default()
            }),
            End::Ambiguous => Err(anyhow!(
                "{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete (timeout); not retried"
            )),
            End::Failing(text) => Err(anyhow!("ExecuteTransaction failed: {text}")),
        }
    }

    async fn find_command(&mut self, _command_id: &str, _from_offset: i64) -> CommandSearch {
        self.counts.scans.fetch_add(1, Ordering::SeqCst);
        match &self.found {
            Some(update) => CommandSearch::Found(update.clone()),
            None => CommandSearch::NotFound,
        }
    }

    fn remint(&mut self) {
        self.counts.remints.fetch_add(1, Ordering::SeqCst);
    }
}

/// A linked test agent whose DSO is the fixture's.
async fn linked(seed: u8, tag: &str) -> (FakeRpc, TestAgent) {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([seed; 32], &rpc, tag);
    agent.config.dso_party = fixture::dso();
    agent.linked(&rpc, 1).await;
    (rpc, agent)
}

async fn record(agent: &TestAgent, id: &str) -> Option<CommandRecord> {
    let store = agent.store();
    let lock = store.lock().await.unwrap();
    store.commands(&lock).unwrap().get(id).cloned()
}

fn code(item: &PendingCommand) -> String {
    format!("{CODE_PREFIX}{}", item.command_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directed_cc_transfer_executes_once_and_a_replay_only_reposts_its_receipt() {
    let (_rpc, agent) = linked(0x71, "exec-cc").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "10"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("update-1"),
        &counts,
    );
    let mut out = Vec::new();
    let handled = run_execute(
        &ctx,
        receipts.as_ref(),
        &mut ledger,
        &Scripted::new(["transfer 10 CC"]),
        &mut out,
        &code(&item),
    )
    .await
    .unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "update-1".into(),
            delivered: true
        },
        "{text}"
    );
    assert!(
        text.contains("Type 'transfer 10 CC'") && text.contains(&fixture::bob()),
        "{text}"
    );
    assert_eq!(counts.get(), (1, 1));
    let receipt = receipts.last(&agent.party);
    assert_eq!(receipt.outcome, ReceiptOutcome::Executed);
    assert_eq!(
        receipt.update_id.as_ref().map(Token::as_str),
        Some("update-1")
    );
    assert_eq!(
        receipt.subject,
        ReceiptSubject::Command(Uuid7::parse(&item.command_id).unwrap())
    );
    assert_eq!(receipt.subject_hash, document_hash(&item.command));
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert_eq!(kept.status, CommandStatus::Executed);
    assert!(kept.receipt_posted && kept.may_be_signed());
    assert_eq!(
        kept.attempt.as_ref().map(|a| a.canton_command_id.clone()),
        Some(format!("ctl-{}", item.command_id))
    );
    // Its use counts against the daily transfer caps.
    let spends = ctx
        .store
        .update_counters(now().unix_secs(), |b| Ok(b.spends().to_vec()))
        .await
        .unwrap();
    assert!(
        spends.iter().any(
            |s| s.key.starts_with(&format!("command:{}", item.command_id))
                && s.key.ends_with(":transfers")
                && s.amount == "10"
        ),
        "{spends:?}"
    );
    let totals = usage_totals(&ctx.store, now()).await.unwrap();
    assert_eq!(
        totals.get(
            UsageBucket::Transfers,
            &canton_coin_of(&ctx.config).unwrap()
        ),
        Some(amount("10"))
    );

    // A replay signs nothing and posts the same receipt again, byte for byte.
    let mut out = Vec::new();
    let again = run_execute(
        &ctx,
        receipts.as_ref(),
        &mut ledger,
        &Scripted::new(["transfer 10 CC"]),
        &mut out,
        &code(&item),
    )
    .await
    .unwrap();
    assert_eq!(
        again,
        Handled::Settled {
            status: CommandStatus::Executed,
            delivered: true
        }
    );
    assert_eq!(counts.get(), (1, 1), "never prepared or signed again");
    let posted = receipts.posted();
    assert_eq!(posted.len(), 2);
    assert_eq!(posted.first(), posted.last());
    assert!(exit_status(&again).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directed_token_transfer_executes_without_a_prompt_in_the_daemon() {
    let (_rpc, agent) = linked(0x72, "exec-token").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_token(&fixture::bob(), "USDCx", "2.5"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, Some("USDCx")),
        End::Executed("update-t"),
        &counts,
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "update-t".into(),
            delivered: true
        }
    );
    assert_eq!(counts.get(), (1, 1));
    assert_eq!(
        receipts.last(&agent.party).outcome,
        ReceiptOutcome::Executed
    );
    // A confirmed run goes through `control execute` only.
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    assert!(run_pending(&mut exec, &item, Mode::Confirm).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_head_and_an_expired_command_are_refused_with_signed_receipts_even_while_waiting() {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([0x73; 32], &rpc, "exec-stale");
    agent.config.dso_party = fixture::dso();
    agent.linked(&rpc, 2).await;
    let ctx = agent.context().await;
    let first = agent.chain.entries.first().unwrap();
    let old_head = HeadRef {
        seq: first.seq(),
        hash: *first.hash(),
    };
    let stale = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        old_head,
    );
    let issued = now().checked_add_secs(-700).unwrap();
    let expired = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        issued,
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    for (item, expected) in [
        (&stale, CommandError::StaleLogHead),
        (&expired, CommandError::Expired),
    ] {
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        let handled = run_pending(&mut exec, item, Mode::Waiting).await.unwrap();
        assert_eq!(
            handled,
            Handled::Refused {
                code: expected.code(),
                name: expected.code_name(),
                delivered: true
            }
        );
        let receipt = receipts.last(&agent.party);
        assert_eq!(receipt.outcome, ReceiptOutcome::Refused);
        assert_eq!(receipt.reason_code, Some(u64::from(expected.code())));
        assert_eq!(receipt.code_source, Some(CodeSource::Agent));
        assert_eq!(
            record(&agent, &item.command_id).await.unwrap().status,
            CommandStatus::Refused
        );
    }
    assert_eq!(counts.get(), (0, 0), "nothing was prepared");
}

/// The chain of a link, then the acceptance of a policy with `rules` signed by the link's controller.
pub(crate) fn accepting(seed: [u8; 32], party: &PartyId, rules: Rules) -> (TestChain, Vec<u8>) {
    let mut chain = TestChain::new(seed, party, Network::Devnet).unwrap();
    let link = chain.link_pending(CONTROLLER).unwrap();
    chain.push(&link).unwrap();
    let at = chain.created_at.checked_add_secs(3660).unwrap();
    chain.created_at = at;
    let document = PolicyDocument {
        network: Network::Devnet,
        agent_party: party.clone(),
        version: 1,
        prev_policy_hash: Hash32([0; 32]),
        issued_at: at,
        expires_at: at.checked_add_secs(3600).unwrap(),
        not_before: None,
        rules,
    }
    .encode()
    .unwrap();
    let key = SigningKey::from_bytes(&[CONTROLLER; 32]);
    let controller = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
    let envelope = ProofEnvelope::new(
        controller,
        &document,
        ControllerProof::Ed25519 {
            signature: key.sign(&document).to_bytes(),
        },
    );
    let accept = EntryBuilder::new(&chain.state)
        .accept_policy(&document, &envelope, at)
        .unwrap();
    chain.push(&accept).unwrap();
    (chain, document)
}

/// A link made four hours ago, the acceptance of `first`, then of `second` held back until
/// `not_before`; both signed by the link's controller.
pub(crate) fn accepting_then(
    seed: [u8; 32],
    party: &PartyId,
    first: Rules,
    second: Rules,
    not_before: UtcTime,
) -> (TestChain, Vec<Vec<u8>>) {
    let mut chain = TestChain::new(seed, party, Network::Devnet).unwrap();
    chain.created_at = now().checked_add_secs(-4 * 3600).unwrap();
    let link = chain.link_pending(CONTROLLER).unwrap();
    chain.push(&link).unwrap();
    let key = SigningKey::from_bytes(&[CONTROLLER; 32]);
    let controller = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
    let mut documents = Vec::new();
    for (version, rules, hold) in [(1, first, None), (2, second, Some(not_before))] {
        let at = chain.created_at.checked_add_secs(3660).unwrap();
        chain.created_at = at;
        let document = PolicyDocument {
            network: Network::Devnet,
            agent_party: party.clone(),
            version,
            prev_policy_hash: chain
                .state
                .latest_policy()
                .map_or(Hash32([0; 32]), |p| *p.hash()),
            issued_at: at,
            expires_at: at.checked_add_secs(3600).unwrap(),
            not_before: hold,
            rules,
        }
        .encode()
        .unwrap();
        let envelope = ProofEnvelope::new(
            controller,
            &document,
            ControllerProof::Ed25519 {
                signature: key.sign(&document).to_bytes(),
            },
        );
        let accept = EntryBuilder::new(&chain.state)
            .accept_policy(&document, &envelope, at)
            .unwrap();
        chain.push(&accept).unwrap();
        documents.push(document);
    }
    (chain, documents)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_over_cap_transfer_is_refused_before_any_signature() {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([0x74; 32], &rpc, "exec-cap");
    agent.config.dso_party = fixture::dso();
    let mut rules = DEFAULT_LINKED_RULES;
    rules.transfers.per_transfer.items = vec![LimitItem {
        instrument: PolicyInstrument::Cc,
        amount: amount("5"),
    }];
    let (chain, document) = accepting(agent.seed, &agent.party, rules);
    agent.linked_chain(&rpc, chain, &[document]).await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "10"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    let cap = agent_control::ReasonCode::PerOperationCap;
    assert_eq!(
        handled,
        Handled::Refused {
            code: cap.as_u16(),
            name: cap.code_name(),
            delivered: true
        }
    );
    assert_eq!(counts.get(), (1, 0), "prepared, never signed");
    let receipt = receipts.last(&agent.party);
    assert_eq!(
        (receipt.outcome, receipt.reason_code),
        (ReceiptOutcome::Refused, Some(u64::from(cap.as_u16())))
    );
    // Within the cap the same policy lets it through.
    let small = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "5"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &small, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(counts.get(), (2, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substituted_content_a_second_root_an_extra_outflow_and_another_command_id_are_refused_unsigned()
 {
    let (_rpc, agent) = linked(0x75, "exec-drills").await;
    let ctx = agent.context().await;
    let failure = agent_control::CommandFailure::ContentRefused;
    let (bob, eve) = (fixture::bob(), fixture::eve());
    // Each drill and the rule of the inspection that refuses it.
    let drills: Vec<(&str, Build, String)> = vec![
        (
            "substituted",
            honest(|s| s.paid = fixture::eve(), None),
            format!("unexpected Amulet of 10 for {eve}"),
        ),
        (
            "declared",
            honest(|s| s.declared = fixture::eve(), None),
            format!("transfer.receiver={eve}, expected={bob}"),
        ),
        (
            "second root",
            honest(|s| s.second_root = true, None),
            "expected 1 root node, got 2".into(),
        ),
        (
            "extra outflow",
            honest(|s| s.extra = Some((fixture::eve(), "1".into())), None),
            format!("unexpected Amulet of 1 for {eve}"),
        ),
        (
            "command id",
            honest(|s| s.command_id = "send-transfer-1".into(), None),
            "command id send-transfer-1 is not the requested ctl-".into(),
        ),
        (
            "token to another",
            honest(|s| s.paid = fixture::eve(), Some("USDCx")),
            format!("unexpected holding of 10 for {eve}"),
        ),
    ];
    for (name, build, reason) in drills {
        let op = if name.starts_with("token") {
            transfer_token(&fixture::bob(), "USDCx", "10")
        } else {
            transfer_cc(&fixture::bob(), "10")
        };
        let item = command_item(&agent, op, now(), 600, agent.head().unwrap());
        let receipts = Receipts::listing(vec![item.clone()]);
        let counts = Arc::new(Counts::default());
        let mut ledger = FakeLedger::new(&agent, build, End::Executed("never"), &counts);
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        let handled = run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap();
        assert_eq!(
            handled,
            Handled::Failed {
                code: failure.code(),
                name: failure.code_name(),
                delivered: true
            },
            "{name}"
        );
        assert_eq!(counts.get(), (1, 0), "{name}: prepared, never signed");
        let refusals = counts.refusals();
        assert!(
            refusals.len() == 1 && refusals.iter().all(|r| r.contains(&reason)),
            "{name}: {refusals:?}"
        );
        let receipt = receipts.last(&agent.party);
        assert_eq!(
            (receipt.outcome, receipt.reason_code),
            (ReceiptOutcome::Failed, Some(u64::from(failure.code()))),
            "{name}"
        );
        assert_eq!(
            record(&agent, &item.command_id).await.unwrap().status,
            CommandStatus::Failed,
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_signing_reconciles_without_a_second_signature() {
    let (_rpc, agent) = linked(0x76, "exec-crash").await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "3"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    {
        let ctx = agent.context().await;
        let mut ledger = FakeLedger::new(&agent, honest(|_| {}, None), End::Ambiguous, &counts);
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        let handled = run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap();
        assert!(matches!(handled, Handled::Unsettled(_)), "{handled:?}");
        assert!(
            receipts.posted().is_empty(),
            "no receipt while the outcome is open"
        );
    }
    assert_eq!(counts.get(), (1, 1));
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert_eq!(kept.status, CommandStatus::Executing);
    assert!(kept.may_be_signed());
    // A restart: a new context and ledger. The record is only reconciled.
    let ctx = agent.context().await;
    let later = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(&agent, honest(|_| {}, None), End::Executed("never"), &later);
    ledger.found = Some("update-found".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "update-found".into(),
            delivered: true
        }
    );
    assert_eq!(later.get(), (0, 0), "never prepared or signed again");
    assert_eq!(later.scans.load(Ordering::SeqCst), 1);
    assert_eq!(
        receipts
            .last(&agent.party)
            .update_id
            .as_ref()
            .map(Token::as_str),
        Some("update-found")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attempt_stopped_before_its_signature_fails_at_once_and_a_signed_one_after_its_grace() {
    let (_rpc, agent) = linked(0x77, "exec-open").await;
    let ctx = agent.context().await;
    let at = now().unix_secs();
    let attempt = |id: &str| CommandAttempt {
        canton_command_id: format!("ctl-{id}"),
        ledger_offset: 40,
        started_at: at,
        signing: false,
        usage: Vec::new(),
        verdict: None,
        swap: None,
    };
    // Stopped before the signature: listed again, it fails without a ledger call.
    let unsigned = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    // Signed, expired past the grace and not on the ledger: it fails on the next round.
    let signed = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    {
        let store = agent.store();
        let lock = store.lock().await.unwrap();
        store
            .update_commands(&lock, at, |b| {
                b.receive(
                    &unsigned.command_id,
                    &unsigned.command,
                    at.saturating_add(600),
                    at,
                )?;
                b.begin(&unsigned.command_id, attempt(&unsigned.command_id), at)?;
                b.receive(
                    &signed.command_id,
                    &signed.command,
                    at.saturating_sub(RECOVERY_GRACE_SECS + 60),
                    at,
                )?;
                b.begin(&signed.command_id, attempt(&signed.command_id), at)?;
                b.mark_signing(&signed.command_id, Vec::new(), at)
            })
            .unwrap();
    }
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    let submit = agent_control::CommandFailure::SubmitFailed;
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &unsigned, Mode::Waiting)
        .await
        .unwrap();
    assert_eq!(
        handled,
        Handled::Failed {
            code: submit.code(),
            name: submit.code_name(),
            delivered: true
        }
    );
    assert_eq!(counts.scans.load(Ordering::SeqCst), 0);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(
        done.first().map(|(id, _)| id.as_str()),
        Some(signed.command_id.as_str())
    );
    assert!(
        matches!(
            done.first().map(|(_, h)| h),
            Some(Handled::Failed { code: 1409, .. })
        ),
        "{done:?}"
    );
    assert_eq!(counts.get(), (0, 0));
    assert_eq!(counts.scans.load(Ordering::SeqCst), 1);
    assert_eq!(
        record(&agent, &signed.command_id).await.unwrap().status,
        CommandStatus::Failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_poller_executes_only_with_auto_execute_and_always_refuses_what_cannot_run() {
    let (rpc, mut agent) = linked(0x78, "exec-poller").await;
    agent.config.control.auto_execute_commands = false;
    let off = Arc::new(agent.context().await);
    let client = ControlRpc::connect(&off.config).await.unwrap();
    let valid = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "4"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let expired = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "4"),
        now().checked_add_secs(-700).unwrap(),
        600,
        agent.head().unwrap(),
    );
    let listing = ListPendingControlResponse {
        commands: vec![valid.clone(), expired.clone()],
        ..Default::default()
    };
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let factory: LedgerFactory = {
        let (party, counts) = (agent.party.to_string(), Arc::clone(&counts));
        Arc::new(move |_config: &BaseConfig| {
            Box::new(FakeLedger {
                party: party.clone(),
                build: honest(|_| {}, None),
                end: End::Executed("update-auto"),
                found: None,
                prepare_refusal: None,
                counts: Arc::clone(&counts),
                jump: None,
                refuse_once: None,
            }) as Box<dyn DirectedLedger>
        })
    };
    let executor =
        DaemonExecutor::with(factory, Some(Arc::clone(&receipts) as Arc<dyn CommandRpc>));
    let report = poller::handle(&off, &client, listing.clone(), &executor, &Memo::default()).await;
    assert_eq!((report.handed, report.failed), (2, 0), "{report}");
    assert_eq!(
        counts.get(),
        (0, 0),
        "nothing is signed while auto_execute_commands is off"
    );
    assert_eq!(
        record(&agent, &valid.command_id).await.unwrap().status,
        CommandStatus::Received
    );
    assert_eq!(
        record(&agent, &expired.command_id).await.unwrap().status,
        CommandStatus::Refused
    );
    assert_eq!(receipts.posted().len(), 1, "the expired one is answered");

    let mut on = agent.context().await;
    on.config.control.auto_execute_commands = true;
    let report = poller::handle(&Arc::new(on), &client, listing, &executor, &Memo::default()).await;
    assert_eq!((report.handed, report.failed), (2, 0), "{report}");
    assert_eq!(counts.get(), (1, 1));
    assert_eq!(
        record(&agent, &valid.command_id).await.unwrap().status,
        CommandStatus::Executed
    );
    drop(rpc);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burns_are_answered_as_not_supported_yet_and_swaps_wait_for_their_desk() {
    let (_rpc, agent) = linked(0x79, "exec-kinds").await;
    let ctx = agent.context().await;
    let cc = canton_coin_of(&ctx.config).unwrap();
    let usdc = InstrumentRef {
        admin: PartyId::parse(&fixture::registry()).unwrap(),
        id: InstrumentId::parse("USDCx").unwrap(),
    };
    let failure = BindingError::NotExecuted.failure();
    let burn = CommandOp::UsdcxBurn {
        amount: amount("1"),
        destination: EthAddress([0x11; 20]),
    };
    let item = command_item(&agent, burn, now(), 600, agent.head().unwrap());
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Waiting).await.unwrap();
    assert_eq!(
        handled,
        Handled::Refused {
            code: failure.code(),
            name: failure.code_name(),
            delivered: true
        }
    );
    assert_eq!(receipts.last(&agent.party).outcome, ReceiptOutcome::Refused);
    assert_eq!(counts.get(), (0, 0));
    // A swap is valid: it waits, and without an RFQ desk it is left received, unsigned.
    let swap = CommandOp::Swap {
        sell: cc,
        sell_amount: amount("10"),
        buy: usdc,
        min_receive: amount("1"),
        max_fee_cc: amount("0"),
    };
    let item = command_item(&agent, swap, now(), 600, agent.head().unwrap());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    assert_eq!(
        run_pending(&mut exec, &item, Mode::Waiting).await.unwrap(),
        Handled::Waiting
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    assert!(matches!(
        run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap(),
        Handled::NotYet(_)
    ));
    assert_eq!(
        record(&agent, &item.command_id).await.map(|r| r.status),
        Some(CommandStatus::Received)
    );
    assert_eq!((counts.get(), receipts.posted().len()), ((0, 0), 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_codes_are_answered_or_retried_as_c13_says() {
    let (_rpc, agent) = linked(0x7a, "exec-codes").await;
    let ctx = agent.context().await;
    // 4123: a signed refusal, relayed from the ledger-service.
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    ledger.prepare_refusal = Some("request refused (code 4123)".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(
        handled,
        Handled::Refused {
            code: 4123,
            name: LEDGER_REFUSAL,
            delivered: true
        }
    );
    let receipt = receipts.last(&agent.party);
    assert_eq!(
        (receipt.reason_code, receipt.code_source),
        (Some(4123), Some(CodeSource::Ledger))
    );
    // 4101: retried later, with no receipt; the command is received again.
    let retry = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    ledger.prepare_refusal = Some("request refused (code 4101)".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &retry, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::NotYet(_)), "{handled:?}");
    assert_eq!(receipts.posted().len(), 1);
    let kept = record(&agent, &retry.command_id).await.unwrap();
    assert_eq!(
        (kept.status, kept.attempt.is_none()),
        (CommandStatus::Received, true)
    );
    // Once the ledger-service answers, the same command runs.
    ledger.prepare_refusal = None;
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &retry, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::Executed { .. }), "{handled:?}");
    assert_eq!(counts.get(), (3, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receipt_the_rpc_did_not_take_is_kept_and_posted_again() {
    let (_rpc, agent) = linked(0x7b, "exec-repost").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![]);
    receipts.down.store(true, Ordering::SeqCst);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("update-r"),
        &counts,
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(
        handled,
        Handled::Executed {
            update_id: "update-r".into(),
            delivered: false
        }
    );
    assert!(
        !record(&agent, &item.command_id)
            .await
            .unwrap()
            .receipt_posted
    );
    receipts.down.store(false, Ordering::SeqCst);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
    assert!(
        matches!(
            done.as_slice(),
            [(
                _,
                Handled::Settled {
                    status: CommandStatus::Executed,
                    delivered: true
                }
            )]
        ),
        "{done:?}"
    );
    assert!(
        record(&agent, &item.command_id)
            .await
            .unwrap()
            .receipt_posted
    );
    assert_eq!(
        receipts
            .last(&agent.party)
            .update_id
            .as_ref()
            .map(Token::as_str),
        Some("update-r")
    );
    assert_eq!(counts.get(), (1, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_commands_lists_what_is_pending_and_what_is_open() {
    let (_rpc, agent) = linked(0x7c, "exec-list").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "7.5"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let mut out = Vec::new();
    run_list(&ctx, receipts.as_ref(), &mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(
        text.contains(&code(&item))
            && text.contains("transfer_cc 7.5 CC to bob::")
            && text.contains("[new]"),
        "{text}"
    );
    let empty = Receipts::listing(vec![]);
    let mut out = Vec::new();
    run_list(&ctx, empty.as_ref(), &mut out).await.unwrap();
    assert!(String::from_utf8_lossy(&out).contains("No direct command is pending"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_confirmation_signs_nothing_and_leaves_the_command_received() {
    let (_rpc, agent) = linked(0x7d, "exec-confirm").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "10"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    let mut out = Vec::new();
    let err = run_execute(
        &ctx,
        receipts.as_ref(),
        &mut ledger,
        &Scripted::new(["transfer 1 CC"]),
        &mut out,
        &code(&item),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("not confirmed"), "{err}");
    assert_eq!(counts.get(), (0, 0));
    assert_eq!(
        record(&agent, &item.command_id).await.unwrap().status,
        CommandStatus::Received
    );
    assert!(receipts.posted().is_empty());
}

#[test]
fn a_code_is_exactly_cmd_and_a_lowercase_uuid7() {
    let id = "01890a5d-ac96-774b-bcce-b302099a8057";
    assert_eq!(parse_code(&format!("CMD-{id}")).unwrap().as_str(), id);
    assert_eq!(parse_code(&format!("  CMD-{id}\n")).unwrap().as_str(), id);
    for bad in [
        id.to_string(),
        format!("cmd-{id}"),
        format!("CMD-{}", id.to_uppercase()),
        format!("CMD-{id}x"),
        "CMD-".into(),
    ] {
        assert!(parse_code(&bad).is_err(), "{bad}");
    }
}

#[test]
fn spend_records_round_trip_and_others_are_skipped() {
    let usage = vec![CommandUsage {
        bucket: "transfers".into(),
        instrument_admin: fixture::dso(),
        instrument_id: "Amulet".into(),
        amount: "2.5".into(),
    }];
    let spends = spends_of(
        "01890a5d-ac96-774b-bcce-b302099a8057",
        &usage,
        1_795_000_000,
    );
    let record = usage_record(spends.first().unwrap()).unwrap().unwrap();
    assert_eq!(
        (record.bucket, record.amount, record.instrument.id.as_str()),
        (UsageBucket::Transfers, amount("2.5"), "Amulet")
    );
    let other = SpendRecord {
        key: "transfer:x".into(),
        instrument: "CC".into(),
        amount: "1".into(),
        at: 1,
    };
    assert!(usage_record(&other).unwrap().is_none());
    let broken = SpendRecord {
        key: "command:x:0:nope".into(),
        ..other
    };
    assert!(usage_record(&broken).is_err());
}

/// The test chains' controller, scoped to `commands` only.
fn scoped_to(commands: Vec<agent_control::CommandKind>) -> Rules {
    let key = SigningKey::from_bytes(&[CONTROLLER; 32]);
    Rules {
        scopes: vec![agent_control::CommandScope {
            controller: controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes()),
            commands,
            pairs: Vec::new(),
            lps: Vec::new(),
        }],
        ..DEFAULT_LINKED_RULES
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_due_before_the_signature_and_an_out_of_scope_listing_are_refused_unsigned() {
    let rpc = FakeRpc::start().await;
    let mut agent = TestAgent::new([0x7e; 32], &rpc, "exec-narrowed");
    agent.config.dso_party = fixture::dso();
    let swaps_only = scoped_to(vec![agent_control::CommandKind::Swap]);
    let due = now().checked_add_secs(300).unwrap();
    let (chain, documents) = accepting_then(
        agent.seed,
        &agent.party,
        DEFAULT_LINKED_RULES,
        swaps_only,
        due,
    );
    agent.linked_chain(&rpc, chain, &documents).await;
    let ctx = agent.context().await;
    let out_of_scope = CommandError::OutOfScope;
    let refused = Handled::Refused {
        code: out_of_scope.code(),
        name: out_of_scope.code_name(),
        delivered: true,
    };
    // Verified before the narrowing; it falls due before the signature.
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    ledger.jump = Some((Arc::clone(&receipts.ahead), 400));
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(handled, refused);
    assert_eq!(counts.get(), (1, 0), "prepared, never signed");
    let receipt = receipts.last(&agent.party);
    assert_eq!(
        (receipt.outcome, receipt.reason_code, receipt.code_source),
        (
            ReceiptOutcome::Refused,
            Some(u64::from(out_of_scope.code())),
            Some(CodeSource::Agent)
        )
    );
    // Once the narrowing applies, a transfer the rpc lists anyway is refused before its prepare.
    let listed = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &listed, Mode::Automatic)
        .await
        .unwrap();
    assert_eq!(handled, refused);
    assert_eq!(counts.get(), (1, 0), "nothing more was prepared");
    assert_eq!(
        record(&agent, &listed.command_id).await.map(|r| r.status),
        Some(CommandStatus::Refused)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ledger_refusal_after_the_signature_becomes_its_refused_receipt_after_the_grace() {
    let (_rpc, agent) = linked(0x7f, "exec-late-refusal").await;
    let ctx = agent.context().await;
    for code in [4119u16, 4105] {
        let item = command_item(
            &agent,
            transfer_cc(&fixture::bob(), "1"),
            now(),
            600,
            agent.head().unwrap(),
        );
        let receipts = Receipts::listing(vec![]);
        let counts = Arc::new(Counts::default());
        let text: &'static str = if code == 4119 {
            "request refused (code 4119)"
        } else {
            "request refused (code 4105)"
        };
        let mut ledger = FakeLedger::new(&agent, honest(|_| {}, None), End::Failing(text), &counts);
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        // Signed, then refused: the outcome stays open and nothing is posted.
        let handled = run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap();
        assert!(
            matches!(handled, Handled::Unsettled(_)),
            "{code}: {handled:?}"
        );
        assert!(receipts.posted().is_empty(), "{code}");
        let kept = record(&agent, &item.command_id).await.unwrap();
        assert_eq!(
            kept.attempt.as_ref().and_then(|a| a.verdict.clone()),
            Some(CommandVerdict {
                status: CommandStatus::Refused,
                code,
                from_ledger: true
            }),
            "{code}"
        );
        // Past its expiry and the grace, still not on the ledger: the kept verdict is posted.
        receipts
            .ahead
            .store(600 + RECOVERY_GRACE_SECS + 60, Ordering::SeqCst);
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        let handled = run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap();
        assert_eq!(
            handled,
            Handled::Refused {
                code,
                name: LEDGER_REFUSAL,
                delivered: true
            }
        );
        assert_eq!(counts.get(), (1, 1), "{code}: signed once, never again");
        let receipt = receipts.last(&agent.party);
        assert_eq!(
            (receipt.outcome, receipt.reason_code, receipt.code_source),
            (
                ReceiptOutcome::Refused,
                Some(u64::from(code)),
                Some(CodeSource::Ledger)
            ),
            "{code}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_4103_syncs_the_log_and_a_4106_mints_a_new_token_then_the_command_runs_once_more() {
    let (_rpc, agent) = linked(0x6a, "exec-repair").await;
    let ctx = agent.context().await;
    for (code, syncs, remints) in [(4103u16, 1u32, 0u32), (4106, 0, 1)] {
        let item = command_item(
            &agent,
            transfer_cc(&fixture::bob(), "1"),
            now(),
            600,
            agent.head().unwrap(),
        );
        let receipts = Receipts::listing(vec![]);
        let counts = Arc::new(Counts::default());
        let mut ledger = FakeLedger::new(
            &agent,
            honest(|_| {}, None),
            End::Executed("update-repaired"),
            &counts,
        );
        ledger.refuse_once = Some(format!("request refused (code {code})"));
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut ledger,
        };
        let handled = run_pending(&mut exec, &item, Mode::Automatic)
            .await
            .unwrap();
        assert_eq!(
            handled,
            Handled::Executed {
                update_id: "update-repaired".into(),
                delivered: true
            },
            "{code}"
        );
        assert_eq!(counts.get(), (2, 1), "{code}: prepared twice, signed once");
        assert_eq!(
            (
                receipts.syncs.load(Ordering::SeqCst),
                counts.remints.load(Ordering::SeqCst)
            ),
            (syncs, remints),
            "{code}"
        );
    }
    // A code that stays is not repaired again in the same run.
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![]);
    let counts = Arc::new(Counts::default());
    let mut ledger = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    ledger.prepare_refusal = Some("request refused (code 4103)".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut ledger,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::NotYet(_)), "{handled:?}");
    assert_eq!(counts.get(), (2, 0));
    assert_eq!(receipts.syncs.load(Ordering::SeqCst), 1);
    assert_eq!(
        record(&agent, &item.command_id).await.map(|r| r.status),
        Some(CommandStatus::Received)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_posts_its_receipt_through_the_rpcs_checks() {
    let (rpc, mut agent) = linked(0x6b, "exec-post").await;
    agent.config.control.auto_execute_commands = true;
    let ctx = Arc::new(agent.context().await);
    let client = ControlRpc::connect(&ctx.config).await.unwrap();
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "2"),
        now(),
        600,
        agent.head().unwrap(),
    );
    rpc.state().commands.insert(
        item.command_id.clone(),
        (agent.party.clone(), item.command.clone()),
    );
    let counts = Arc::new(Counts::default());
    let factory: LedgerFactory = {
        let (party, counts) = (agent.party.to_string(), Arc::clone(&counts));
        Arc::new(move |_config: &BaseConfig| {
            Box::new(FakeLedger {
                party: party.clone(),
                build: honest(|_| {}, None),
                end: End::Executed("update-posted"),
                found: None,
                prepare_refusal: None,
                counts: Arc::clone(&counts),
                jump: None,
                refuse_once: None,
            }) as Box<dyn DirectedLedger>
        })
    };
    let executor = DaemonExecutor::with(factory, None);
    // A listing first, as the poller makes, gives the client the rpc's clock.
    client.pending().await.unwrap();
    let listing = ListPendingControlResponse {
        commands: vec![item.clone()],
        ..Default::default()
    };
    let report = poller::handle(&ctx, &client, listing, &executor, &Memo::default()).await;
    assert_eq!((report.handed, report.failed), (1, 0), "{report}");
    assert_eq!(counts.get(), (1, 1));
    let posted = rpc.state().receipts.clone();
    assert_eq!(posted.len(), 1, "the rpc took the agent's receipt");
    let (id, bytes) = posted.first().cloned().unwrap();
    assert_eq!(id, item.command_id);
    let receipt = Receipt::decode(&bytes).unwrap();
    assert_eq!(
        (receipt.outcome, receipt.subject_hash),
        (ReceiptOutcome::Executed, document_hash(&item.command))
    );
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert!(kept.status == CommandStatus::Executed && kept.receipt_posted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_in_a_second_run_never_settles_an_attempt_another_run_signed() {
    let (_rpc, agent) = linked(0x13, "exec-two-runs").await;
    let ctx = agent.context().await;
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "2"),
        now(),
        600,
        agent.head().unwrap(),
    );
    let receipts = Receipts::listing(vec![item.clone()]);
    let counts = Arc::new(Counts::default());
    // The CLI run admits the command, then waits at its typed prompt with no lock held.
    let mut cli = FakeLedger::new(
        &agent,
        honest(|_| {}, None),
        End::Executed("never"),
        &counts,
    );
    let ready = {
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut cli,
        };
        match admit(&mut exec, &item, Mode::Confirm).await.unwrap() {
            Admission::Ready(ready) => ready,
            Admission::Done(handled) => panic!("not admitted: {handled:?}"),
        }
    };
    // Meanwhile the daemon signs it, and its outcome stays open.
    let mut daemon = FakeLedger::new(&agent, honest(|_| {}, None), End::Ambiguous, &counts);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut daemon,
    };
    let handled = run_pending(&mut exec, &item, Mode::Automatic)
        .await
        .unwrap();
    assert!(matches!(handled, Handled::Unsettled(_)), "{handled:?}");
    assert_eq!(counts.get(), (1, 1));
    // The phrase is typed once the command expired, and an admission sees the signed record.
    receipts.ahead.store(700, Ordering::SeqCst);
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut cli,
    };
    let handled = dispatch(&mut exec, *ready).await.unwrap();
    assert!(matches!(handled, Handled::Unsettled(_)), "{handled:?}");
    let handled = run_pending(&mut exec, &item, Mode::Waiting).await.unwrap();
    assert!(matches!(handled, Handled::Unsettled(_)), "{handled:?}");
    assert!(
        receipts.posted().is_empty(),
        "no receipt while the outcome is open"
    );
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert!(
        kept.status == CommandStatus::Executing && kept.may_be_signed(),
        "{kept:?}"
    );
    assert_eq!(counts.get(), (1, 1), "never signed twice");
    // The transfer lands: it settles as executed.
    daemon.found = Some("update-late".into());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut daemon,
    };
    let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
    assert!(
        matches!(done.first().map(|(_, h)| h), Some(Handled::Executed { .. })),
        "{done:?}"
    );
    assert_eq!(
        receipts.last(&agent.party).outcome,
        ReceiptOutcome::Executed
    );
    assert_eq!(
        record(&agent, &item.command_id).await.unwrap().status,
        CommandStatus::Executed
    );
}

/// A ledger that only scans: each scan gets the next answer, and its start is kept.
struct Scanner {
    answers: std::collections::VecDeque<CommandSearch>,
    from: Vec<i64>,
}

#[async_trait::async_trait]
impl DirectedLedger for Scanner {
    async fn ledger_end(&mut self) -> Result<i64> {
        bail!("this ledger only scans")
    }

    async fn submit_directed(
        &mut self,
        _req: PrepareTransactionRequest,
        _expectation: &OperationExpectation,
        _command: &Command,
        _bind: PreSign<'_>,
    ) -> Result<ExecuteTransactionResponse> {
        bail!("this ledger only scans")
    }

    async fn find_command(&mut self, _command_id: &str, from_offset: i64) -> CommandSearch {
        self.from.push(from_offset);
        self.answers
            .pop_front()
            .unwrap_or_else(|| CommandSearch::Unknown("no answer".into()))
    }

    fn remint(&mut self) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_scan_resumes_where_it_stopped_until_the_signed_attempt_settles() {
    let (_rpc, agent) = linked(0x14, "exec-resume").await;
    let ctx = agent.context().await;
    let at = now().unix_secs();
    let item = command_item(
        &agent,
        transfer_cc(&fixture::bob(), "1"),
        now(),
        600,
        agent.head().unwrap(),
    );
    {
        let store = agent.store();
        let lock = store.lock().await.unwrap();
        let started = CommandAttempt {
            canton_command_id: format!("ctl-{}", item.command_id),
            ledger_offset: 40,
            started_at: at,
            signing: false,
            usage: Vec::new(),
            verdict: None,
            swap: None,
        };
        store
            .update_commands(&lock, at, |b| {
                let expired = at.saturating_sub(RECOVERY_GRACE_SECS + 60);
                b.receive(&item.command_id, &item.command, expired, at)?;
                b.begin(&item.command_id, started, at)?;
                b.mark_signing(&item.command_id, Vec::new(), at)
            })
            .unwrap();
    }
    let receipts = Receipts::listing(vec![]);
    let mut scanner = Scanner {
        answers: [
            CommandSearch::Partial {
                scanned_to: 500,
                why: "cut".into(),
            },
            CommandSearch::Partial {
                scanned_to: 900,
                why: "cut".into(),
            },
            CommandSearch::NotFound,
        ]
        .into(),
        from: Vec::new(),
    };
    for _ in 0..2 {
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut scanner,
        };
        let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
        assert!(
            matches!(done.first().map(|(_, h)| h), Some(Handled::Unsettled(_))),
            "{done:?}"
        );
    }
    let kept = record(&agent, &item.command_id).await.unwrap();
    assert_eq!(kept.attempt.map(|a| a.ledger_offset), Some(900));
    assert!(receipts.posted().is_empty());
    let mut exec = Executor {
        ctx: &ctx,
        rpc: receipts.as_ref(),
        ledger: &mut scanner,
    };
    let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
    assert!(
        matches!(
            done.first().map(|(_, h)| h),
            Some(Handled::Failed { code: 1409, .. })
        ),
        "{done:?}"
    );
    assert_eq!(
        scanner.from,
        vec![40, 500, 900],
        "each scan resumed where the last stopped"
    );
    assert_eq!(receipts.posted().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_attempt_whose_outcome_stays_unknown_past_the_grace_is_alerted_once() {
    let (_rpc, agent) = linked(0x19, "exec-unknown").await;
    let ctx = agent.context().await;
    let at = now().unix_secs();
    let mut ids = Vec::new();
    for expires in [
        at.saturating_sub(RECOVERY_GRACE_SECS + 60),
        at.saturating_add(600),
    ] {
        let item = command_item(
            &agent,
            transfer_cc(&fixture::bob(), "1"),
            now(),
            600,
            agent.head().unwrap(),
        );
        let store = agent.store();
        let lock = store.lock().await.unwrap();
        let started = CommandAttempt {
            canton_command_id: format!("ctl-{}", item.command_id),
            ledger_offset: 40,
            started_at: at,
            signing: false,
            usage: Vec::new(),
            verdict: None,
            swap: None,
        };
        store
            .update_commands(&lock, at, |b| {
                b.receive(&item.command_id, &item.command, expires, at)?;
                b.begin(&item.command_id, started, at)?;
                b.mark_signing(&item.command_id, Vec::new(), at)
            })
            .unwrap();
        ids.push(item.command_id.clone());
    }
    let receipts = Receipts::listing(vec![]);
    // Every scan fails, as from an offset the participant has pruned.
    let mut scanner = Scanner {
        answers: Default::default(),
        from: Vec::new(),
    };
    for _ in 0..2 {
        let mut exec = Executor {
            ctx: &ctx,
            rpc: receipts.as_ref(),
            ledger: &mut scanner,
        };
        let done = settle_open(&mut exec, &BTreeSet::new()).await.unwrap();
        assert_eq!(done.len(), 2, "{done:?}");
        assert!(
            done.iter().all(|(_, h)| matches!(h, Handled::Unsettled(_))),
            "{done:?}"
        );
    }
    // Raised for the record past the grace, and not again; the other is not alerted yet.
    let (past, within) = (&ids[0], &ids[1]);
    assert!(!first_unknown_alert(past), "raised once already");
    assert!(first_unknown_alert(within), "not past the grace");
    assert!(receipts.posted().is_empty());
    for id in &ids {
        let kept = record(&agent, id).await.unwrap();
        assert!(
            kept.status == CommandStatus::Executing && kept.may_be_signed(),
            "{kept:?}"
        );
    }
}

#[test]
fn a_package_refusal_before_the_binding_is_answered_as_the_binding_answers_it() {
    use super::intent::{PackageRefusal, UnmatchedHash};
    let id = agent_control::PackageId([3; 32]);
    let refused = Submitted::Answered { refused: true, code: 19, name: "package_not_allowed", from_ledger: false };
    for refusal in [PackageRefusal::Local(id), PackageRefusal::Policy(id)] {
        assert_eq!(classify(Ok(Err(anyhow::Error::new(refusal))), false, None), refused);
    }
    let content = agent_control::CommandFailure::ContentRefused;
    for refusal in [
        PackageRefusal::Unreadable("x".into()),
        PackageRefusal::HashUnmatched(UnmatchedHash::Mismatch),
        PackageRefusal::HashUnmatched(UnmatchedHash::Uncomputed("no hash".into())),
    ] {
        assert_eq!(
            classify(Ok(Err(anyhow::Error::new(refusal))), false, None),
            Submitted::Answered { refused: false, code: content.code(), name: content.code_name(), from_ledger: false }
        );
    }
    // The same words untyped are tried again, as any other failure before a signature.
    let untyped = classify(Ok(Err(anyhow!("package_not_allowed: package 03 is outside [vetted_packages]"))), false, None);
    assert!(matches!(untyped, Submitted::Again(_)), "{untyped:?}");
}
