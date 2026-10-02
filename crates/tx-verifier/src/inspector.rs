//! Transaction field inspection
//!
//! Verifies `PreparedTransaction` protobuf fields against `OperationExpectation`.
//!
//! Fully verified: TransferCc, AcceptCip56, RequestUserService, PayFee,
//!                 ProposeDvp, AcceptDvp, Allocate, RequestPreapproval, RequestRecurringPayasyougo.
//! Stub: TransferCip56 (send-side).

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use prost::Message;
use tracing::debug;

use crate::decode::canton_proto::com::daml::ledger::api::v2 as proto_v2;
use proto_v2::interactive::{daml_transaction, PreparedTransaction};
use proto_v2::interactive::transaction::v1::node::NodeType;
use proto_v2::interactive::transaction::v1::Exercise;
use proto_v2::{value::Sum, Identifier, Record, Value};

use crate::text::short;
use crate::types::{InspectionResult, OperationExpectation};

/// Verbose mode skips the JSON dump of transactions larger than this.
const VERBOSE_DUMP_MAX_BYTES: usize = 256 * 1024;

// ============================================================================
// Value extraction helpers
// ============================================================================

/// Get a named field from a Record
fn get_record_field<'a>(record: &'a Record, label: &str) -> Option<&'a Value> {
    record
        .fields
        .iter()
        .find(|f| f.label == label)
        .and_then(|f| f.value.as_ref())
}

/// Unwrap a Value as Record
fn as_record(value: &Value) -> Option<&Record> {
    match value.sum.as_ref()? {
        Sum::Record(r) => Some(r),
        _ => None,
    }
}

/// Get Party string from a Value
fn get_party(value: &Value) -> Option<&str> {
    match value.sum.as_ref()? {
        Sum::Party(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Get Numeric string from a Value
fn get_numeric(value: &Value) -> Option<&str> {
    match value.sum.as_ref()? {
        Sum::Numeric(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Get Text string from a Value
fn get_text(value: &Value) -> Option<&str> {
    match value.sum.as_ref()? {
        Sum::Text(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Unwrap a Value as List (slice of elements)
fn as_list(value: &Value) -> Option<&[Value]> {
    match value.sum.as_ref()? {
        Sum::List(l) => Some(&l.elements),
        _ => None,
    }
}

/// Extract (instrument_id, instrument_admin, amount) from an InstrumentQuantity record
fn extract_instrument_quantity<'a>(iq: &'a Record) -> (Option<&'a str>, Option<&'a str>, Option<&'a str>) {
    let instrument = get_record_field(iq, "instrument").and_then(as_record);
    let id = instrument.and_then(|r| get_record_field(r, "id")).and_then(get_text);
    let admin = instrument.and_then(|r| get_record_field(r, "admin")).and_then(get_party);
    let amount = get_record_field(iq, "amount").and_then(get_numeric);
    (id, admin, amount)
}

/// Check if Identifier matches module_name and entity_name (ignoring package_id)
fn template_matches(id: &Identifier, module_name: &str, entity_name: &str) -> bool {
    id.module_name == module_name && id.entity_name == entity_name
}

/// Compare two numeric strings for equality (handles "1" vs "1.0000000000")
fn numeric_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Parse as f64 and compare
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(fa), Ok(fb)) => (fa - fb).abs() < 1e-9,
        _ => false,
    }
}

// ============================================================================
// Node traversal helpers
// ============================================================================

type NodesDict<'a> = HashMap<String, &'a daml_transaction::Node>;

/// Build dict of node_id → DamlNode
fn build_nodes_dict(tx: &proto_v2::interactive::DamlTransaction) -> NodesDict<'_> {
    tx.nodes.iter().map(|n| (n.node_id.clone(), n)).collect()
}

/// Extract v1 NodeType from a DamlNode
fn get_node_type(node: &daml_transaction::Node) -> Option<&NodeType> {
    let versioned = node.versioned_node.as_ref()?;
    match versioned {
        daml_transaction::node::VersionedNode::V1(n) => n.node_type.as_ref(),
    }
}

/// Find Exercise node by choice_id among given node IDs
fn find_exercise_by_choice<'a>(
    choice_id: &str,
    node_ids: &[String],
    nodes_dict: &'a NodesDict<'a>,
) -> Option<&'a Exercise> {
    for nid in node_ids {
        if let Some(node) = nodes_dict.get(nid) {
            if let Some(NodeType::Exercise(ex)) = get_node_type(node) {
                if ex.choice_id == choice_id {
                    return Some(ex);
                }
            }
        }
    }
    None
}

// ============================================================================
// JSON helpers for verbose mode
// ============================================================================

/// Convert a JSON byte array to hex string.
fn bytes_to_hex(bytes_arr: &[serde_json::Value]) -> String {
    let bytes: Vec<u8> = bytes_arr
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u8))
        .collect();
    hex::encode(&bytes)
}

/// Replace input_contracts[*].event_blob byte arrays with base58 strings.
fn compact_event_blobs(json: &mut serde_json::Value) {
    if let Some(meta) = json.pointer_mut("/metadata/input_contracts") {
        if let Some(arr) = meta.as_array_mut() {
            for contract in arr.iter_mut().filter_map(|c| c.as_object_mut()) {
                let Some(bytes_arr) = contract.get("event_blob").and_then(|b| b.as_array()) else {
                    continue;
                };
                let bytes: Vec<u8> = bytes_arr
                    .iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect();
                contract.insert(
                    "event_blob".to_string(),
                    serde_json::Value::String(format!(
                        "base58:{}",
                        bs58::encode(&bytes).into_string()
                    )),
                );
            }
        }
    }
}

/// Replace node_seeds[*].seed byte arrays with hex strings.
fn compact_seeds(json: &mut serde_json::Value) {
    if let Some(seeds) = json.pointer_mut("/transaction/node_seeds") {
        if let Some(arr) = seeds.as_array_mut() {
            for entry in arr.iter_mut().filter_map(|e| e.as_object_mut()) {
                let Some(bytes_arr) = entry.get("seed").and_then(|s| s.as_array()) else {
                    continue;
                };
                let hex_seed = bytes_to_hex(bytes_arr);
                entry.insert("seed".to_string(), serde_json::Value::String(hex_seed));
            }
        }
    }
}

/// PreparedTransaction as compacted JSON, or a size note when it is too large to dump.
fn prepared_json_for_log(prepared_transaction_bytes: &[u8]) -> String {
    if prepared_transaction_bytes.len() > VERBOSE_DUMP_MAX_BYTES {
        return format!(
            "({} bytes, above the {} byte verbose limit; not dumped)",
            prepared_transaction_bytes.len(),
            VERBOSE_DUMP_MAX_BYTES
        );
    }
    match PreparedTransaction::decode(prepared_transaction_bytes)
        .context("Failed to decode PreparedTransaction protobuf")
    {
        Ok(prepared) => match serde_json::to_value(&prepared) {
            Ok(mut val) => {
                compact_event_blobs(&mut val);
                compact_seeds(&mut val);
                serde_json::to_string_pretty(&val)
                    .unwrap_or_else(|e| format!("(json error: {})", e))
            }
            Err(e) => format!("(serialize error: {})", e),
        },
        Err(e) => format!("(decode error: {})", e),
    }
}

/// Dump full PreparedTransaction and OperationExpectation as JSON (verbose mode only).
fn log_verbose(prepared_transaction_bytes: &[u8], expectation: &OperationExpectation) {
    let tx_json = prepared_json_for_log(prepared_transaction_bytes);
    let expect_json = serde_json::to_string_pretty(expectation)
        .unwrap_or_else(|e| format!("(serialize error: {})", e));
    debug!("PreparedTransaction:\n{}", tx_json);
    debug!("OperationExpectation:\n{}", expect_json);
}

// ============================================================================
// TransferCc inspection (Phase B)
// ============================================================================

fn inspect_transfer_cc(
    prepared: &PreparedTransaction,
    sender_party: &str,
    receiver_party: &str,
    expected_amount: &str,
    expected_command_id: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    // --- Metadata checks ---

    // act_as should contain sender_party
    if !submitter.act_as.contains(&sender_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "TransferCc metadata check failed".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain sender_party {}",
                submitter.act_as, sender_party
            )),
        });
    }

    // command_id should match
    if submitter.command_id != expected_command_id {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_command_id, submitter.command_id
        ));
    }

    // --- Build nodes dict ---
    let nodes_dict = build_nodes_dict(tx);

    // --- Find root exercise node ---
    let Some(root_id) = tx.roots.first() else {
        return Ok(InspectionResult {
            accepted: false,
            summary: "TransferCc: no root nodes".to_string(),
            warnings,
            rejection_reason: Some("Transaction has no root nodes".to_string()),
        });
    };

    let root_node = nodes_dict
        .get(root_id)
        .context("Root node not found in nodes dict")?;
    let root_exercise = match get_node_type(root_node) {
        Some(NodeType::Exercise(ex)) => ex,
        _ => {
            return Ok(InspectionResult {
                accepted: false,
                summary: "TransferCc: root is not Exercise".to_string(),
                warnings,
                rejection_reason: Some("Root node is not an Exercise node".to_string()),
            });
        }
    };

    // --- Root exercise checks ---

    // choice_id
    if root_exercise.choice_id != "TransferFactory_Transfer" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!(
                "TransferCc: wrong choice_id: {}",
                root_exercise.choice_id
            ),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=TransferFactory_Transfer, got={}",
                root_exercise.choice_id
            )),
        });
    }

    // template: Splice.ExternalPartyAmuletRules
    if let Some(tid) = &root_exercise.template_id {
        if !template_matches(tid, "Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules")
        {
            warnings.push(format!(
                "Unexpected template: {}.{} (expected Splice.ExternalPartyAmuletRules)",
                tid.module_name, tid.entity_name
            ));
        }
    } else {
        return Ok(InspectionResult {
            accepted: false,
            summary: "TransferCc: root exercise missing template_id".to_string(),
            warnings,
            rejection_reason: Some("Root exercise missing template_id".to_string()),
        });
    }

    // acting_parties should contain sender
    if !root_exercise
        .acting_parties
        .contains(&sender_party.to_string())
    {
        return Ok(InspectionResult {
            accepted: false,
            summary: "TransferCc: sender not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain sender {}",
                root_exercise.acting_parties, sender_party
            )),
        });
    }

    // --- chosen_value checks ---

    let chosen = root_exercise
        .chosen_value
        .as_ref()
        .context("Root exercise missing chosen_value")?;
    let chosen_record = as_record(chosen).context("chosen_value is not a Record")?;

    // Get the "transfer" field
    let transfer_val =
        get_record_field(chosen_record, "transfer").context("chosen_value missing 'transfer'")?;
    let transfer = as_record(transfer_val).context("'transfer' field is not a Record")?;

    // Check sender
    if let Some(sender_val) = get_record_field(transfer, "sender") {
        if let Some(sender) = get_party(sender_val) {
            if sender != sender_party {
                return Ok(InspectionResult {
                    accepted: false,
                    summary: "TransferCc: wrong sender in transfer".to_string(),
                    warnings,
                    rejection_reason: Some(format!(
                        "transfer.sender={}, expected={}",
                        sender, sender_party
                    )),
                });
            }
        }
    }

    // Check receiver (direct Party field on the transfer record)
    if let Some(receiver_val) = get_record_field(transfer, "receiver") {
        if let Some(receiver) = get_party(receiver_val) {
            if receiver != receiver_party {
                return Ok(InspectionResult {
                    accepted: false,
                    summary: "TransferCc: wrong receiver in transfer".to_string(),
                    warnings,
                    rejection_reason: Some(format!(
                        "transfer.receiver={}, expected={}",
                        receiver, receiver_party
                    )),
                });
            }
        }
    } else {
        return Ok(InspectionResult {
            accepted: false,
            summary: "TransferCc: missing receiver field".to_string(),
            warnings,
            rejection_reason: Some("transfer record missing 'receiver' field".to_string()),
        });
    }

    // Check amount (direct Numeric field on the transfer record)
    // Daml Numeric has 10 decimal places (e.g., "1.0000000000"), expected may be "1"
    if let Some(amount_val) = get_record_field(transfer, "amount") {
        if let Some(amt) = get_numeric(amount_val) {
            if !numeric_eq(amt, expected_amount) {
                return Ok(InspectionResult {
                    accepted: false,
                    summary: "TransferCc: wrong amount".to_string(),
                    warnings,
                    rejection_reason: Some(format!(
                        "transfer.amount={}, expected={}",
                        amt, expected_amount
                    )),
                });
            }
        }
    } else {
        warnings.push("Could not extract transfer.amount".to_string());
    }

    // Check instrumentId.id == "Amulet"
    let instrument_id = get_record_field(transfer, "instrumentId")
        .and_then(as_record)
        .and_then(|r| get_record_field(r, "id"))
        .and_then(get_text);
    if let Some(iid) = instrument_id {
        if iid != "Amulet" {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("TransferCc: wrong instrument: {}", iid),
                warnings,
                rejection_reason: Some(format!(
                    "instrumentId.id={}, expected=Amulet",
                    iid
                )),
            });
        }
    } else {
        warnings.push("Could not extract instrumentId.id".to_string());
    }

    // --- Inner exercise checks ---

    // TransferPreapproval_Send should exist
    let preapproval_send = find_exercise_by_choice(
        "TransferPreapproval_Send",
        &root_exercise.children,
        &nodes_dict,
    );
    if preapproval_send.is_none() {
        warnings.push("TransferPreapproval_Send not found in root children".to_string());
    }

    // AmuletRules_Transfer should exist under TransferPreapproval_Send
    if let Some(ps) = preapproval_send {
        let amulet_transfer =
            find_exercise_by_choice("AmuletRules_Transfer", &ps.children, &nodes_dict);
        if amulet_transfer.is_none() {
            warnings.push(
                "AmuletRules_Transfer not found in TransferPreapproval_Send children".to_string(),
            );
        }
    }

    // --- Summary ---
    let node_count = tx.nodes.len();
    let exercise_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Exercise(_))))
        .count();
    let fetch_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Fetch(_))))
        .count();
    let create_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Create(_))))
        .count();

    let sender_short = short(sender_party, 8);
    let receiver_short = short(receiver_party, 8);

    let summary = format!(
        "TransferCc VERIFIED: {} CC {} → {} | {} nodes ({} exercise, {} fetch, {} create)",
        expected_amount,
        sender_short,
        receiver_short,
        node_count,
        exercise_count,
        fetch_count,
        create_count,
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// AcceptCip56 inspection (Phase B)
// ============================================================================

fn inspect_accept_cip56(
    prepared: &PreparedTransaction,
    receiver_party: &str,
    expected_contract_id: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    // --- Metadata checks ---

    // act_as should contain receiver_party
    if !submitter.act_as.contains(&receiver_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptCip56 metadata check failed".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain receiver_party {}",
                submitter.act_as, receiver_party
            )),
        });
    }

    // --- Build nodes dict ---
    let nodes_dict = build_nodes_dict(tx);

    // --- Find root exercise node ---
    let Some(root_id) = tx.roots.first() else {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptCip56: no root nodes".to_string(),
            warnings,
            rejection_reason: Some("Transaction has no root nodes".to_string()),
        });
    };

    let root_node = nodes_dict
        .get(root_id)
        .context("Root node not found in nodes dict")?;
    let root_exercise = match get_node_type(root_node) {
        Some(NodeType::Exercise(ex)) => ex,
        _ => {
            return Ok(InspectionResult {
                accepted: false,
                summary: "AcceptCip56: root is not Exercise".to_string(),
                warnings,
                rejection_reason: Some("Root node is not an Exercise node".to_string()),
            });
        }
    };

    // --- Root exercise checks ---

    // choice_id must be TransferInstruction_Accept
    if root_exercise.choice_id != "TransferInstruction_Accept" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!(
                "AcceptCip56: wrong choice_id: {}",
                root_exercise.choice_id
            ),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=TransferInstruction_Accept, got={}",
                root_exercise.choice_id
            )),
        });
    }

    // contract_id must match the expected TransferOffer contract
    if root_exercise.contract_id != expected_contract_id {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptCip56: wrong contract_id".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "contract_id={}, expected={}",
                root_exercise.contract_id, expected_contract_id
            )),
        });
    }

    // acting_parties should contain receiver_party
    if !root_exercise
        .acting_parties
        .contains(&receiver_party.to_string())
    {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptCip56: receiver not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain receiver {}",
                root_exercise.acting_parties, receiver_party
            )),
        });
    }

    // interface_id should be TransferInstruction (Splice.Api.Token.TransferInstructionV1)
    if let Some(iface) = &root_exercise.interface_id {
        if !template_matches(iface, "Splice.Api.Token.TransferInstructionV1", "TransferInstruction") {
            warnings.push(format!(
                "Unexpected interface_id: {}.{} (expected Splice.Api.Token.TransferInstructionV1.TransferInstruction)",
                iface.module_name, iface.entity_name
            ));
        }
    } else {
        warnings.push("Root exercise missing interface_id".to_string());
    }

    // consuming must be true (transfer offer is consumed)
    if !root_exercise.consuming {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptCip56: exercise is not consuming".to_string(),
            warnings,
            rejection_reason: Some("Root exercise must be consuming (TransferOffer is consumed on accept)".to_string()),
        });
    }

    // --- Inner exercise checks ---

    // TransferRule_Transfer should exist in the children tree
    let transfer_rule = find_exercise_by_choice(
        "TransferRule_Transfer",
        &root_exercise.children,
        &nodes_dict,
    );
    if transfer_rule.is_none() {
        // Check one level deeper (via children of children), each child once
        let mut found = false;
        let mut visited: HashSet<&str> = HashSet::new();
        for child_id in &root_exercise.children {
            if !visited.insert(child_id.as_str()) {
                continue;
            }
            if let Some(child_node) = nodes_dict.get(child_id) {
                if let Some(NodeType::Exercise(ex)) = get_node_type(child_node) {
                    if find_exercise_by_choice("TransferRule_Transfer", &ex.children, &nodes_dict)
                        .is_some()
                    {
                        found = true;
                        break;
                    }
                }
            }
        }
        if !found {
            warnings.push("TransferRule_Transfer not found in exercise tree".to_string());
        }
    }

    // --- Check that a Holding Create exists with owner=receiver_party ---
    let mut receiver_holding_found = false;
    for node in &tx.nodes {
        if let Some(NodeType::Create(create)) = get_node_type(node) {
            if let Some(tid) = &create.template_id {
                if tid.entity_name == "Holding" {
                    // Check owner field
                    if let Some(arg) = &create.argument {
                        if let Some(rec) = as_record(arg) {
                            if let Some(owner_val) = get_record_field(rec, "owner") {
                                if let Some(owner) = get_party(owner_val) {
                                    if owner == receiver_party {
                                        receiver_holding_found = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !receiver_holding_found {
        warnings.push("No Holding Create found with owner=receiver_party".to_string());
    }

    // --- Summary ---
    let node_count = tx.nodes.len();
    let exercise_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Exercise(_))))
        .count();
    let fetch_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Fetch(_))))
        .count();
    let create_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Create(_))))
        .count();

    let summary = format!(
        "AcceptCip56 VERIFIED: receiver {} accepts TransferOffer {} | {} nodes ({} exercise, {} fetch, {} create)",
        receiver_party,
        expected_contract_id,
        node_count,
        exercise_count,
        fetch_count,
        create_count,
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// RequestUserService inspection
// ============================================================================

/// Verify a RequestUserService transaction.
///
/// Expected structure: single Create node for UserServiceRequest with
/// operator = settlement_operator and user = party.
fn inspect_request_user_service(
    prepared: &PreparedTransaction,
    party: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    // --- Metadata: act_as must contain party ---
    if !submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestUserService: party not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                submitter.act_as, party
            )),
        });
    }

    // --- Must have exactly 1 root and 1 node (a Create) ---
    let [root_id] = tx.roots.as_slice() else {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("RequestUserService: expected 1 root, got {}", tx.roots.len()),
            warnings,
            rejection_reason: Some(format!("Expected 1 root node, got {}", tx.roots.len())),
        });
    };

    if tx.nodes.len() != 1 {
        warnings.push(format!("Expected 1 node, got {}", tx.nodes.len()));
    }

    let root_node = tx
        .nodes
        .iter()
        .find(|n| n.node_id == *root_id)
        .context("Root node not found")?;

    let create = match get_node_type(root_node) {
        Some(NodeType::Create(c)) => c,
        _ => {
            return Ok(InspectionResult {
                accepted: false,
                summary: "RequestUserService: root is not a Create node".to_string(),
                warnings,
                rejection_reason: Some("Root node must be a Create node".to_string()),
            });
        }
    };

    // --- Template must be UserServiceRequest ---
    let tid = create
        .template_id
        .as_ref()
        .context("Create node missing template_id")?;
    if !template_matches(tid, "Utility.Settlement.App.V1.Service.User", "UserServiceRequest") {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!(
                "RequestUserService: wrong template {}.{}",
                tid.module_name, tid.entity_name
            ),
            warnings,
            rejection_reason: Some(format!(
                "Expected UserServiceRequest template, got {}.{}",
                tid.module_name, tid.entity_name
            )),
        });
    }

    // --- Check create arguments: user must match party ---
    let arg = create
        .argument
        .as_ref()
        .context("Create node missing argument")?;
    let record = as_record(arg).context("Create argument is not a Record")?;

    let user_val = get_record_field(record, "user");
    let user_party = user_val.and_then(get_party);

    if user_party != Some(party) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestUserService: user field does not match party".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "user={}, expected={}",
                user_party.unwrap_or("<missing>"),
                party
            )),
        });
    }

    // --- Check signatories contain party ---
    if !create.signatories.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestUserService: party not in signatories".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "signatories {:?} does not contain party {}",
                create.signatories, party
            )),
        });
    }

    // --- Extract operator for summary ---
    let operator = get_record_field(record, "operator")
        .and_then(get_party)
        .unwrap_or("<unknown>");

    let summary = format!(
        "RequestUserService VERIFIED: user {} requests service from operator {} | 1 Create node",
        party, operator,
    );

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// RequestPreapproval inspection (Phase B)
// ============================================================================

/// Verify a RequestPreapproval transaction.
///
/// Expected structure: single Create node for TransferPreapprovalProposal
/// with receiver = party (the requesting external party).
fn inspect_request_preapproval(
    prepared: &PreparedTransaction,
    party: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    // --- Metadata: act_as must contain party ---
    if !submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestPreapproval: party not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                submitter.act_as, party
            )),
        });
    }

    // --- Must have exactly 1 root and 1 node (a Create) ---
    let [root_id] = tx.roots.as_slice() else {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("RequestPreapproval: expected 1 root, got {}", tx.roots.len()),
            warnings,
            rejection_reason: Some(format!("Expected 1 root node, got {}", tx.roots.len())),
        });
    };

    if tx.nodes.len() != 1 {
        warnings.push(format!("Expected 1 node, got {}", tx.nodes.len()));
    }

    let root_node = tx
        .nodes
        .iter()
        .find(|n| n.node_id == *root_id)
        .context("Root node not found")?;

    let create = match get_node_type(root_node) {
        Some(NodeType::Create(c)) => c,
        _ => {
            return Ok(InspectionResult {
                accepted: false,
                summary: "RequestPreapproval: root is not a Create node".to_string(),
                warnings,
                rejection_reason: Some("Root node must be a Create node".to_string()),
            });
        }
    };

    // --- Template must be TransferPreapproval (CIP-56 or Splice) ---
    let tid = create
        .template_id
        .as_ref()
        .context("Create node missing template_id")?;
    let is_cip56 = template_matches(tid, "Utility.Registry.App.V0.Model.TransferPreapproval", "TransferPreapproval");
    let is_splice = template_matches(tid, "Splice.Wallet.TransferPreapproval", "TransferPreapprovalProposal");
    if !is_cip56 && !is_splice {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!(
                "RequestPreapproval: wrong template {}.{}",
                tid.module_name, tid.entity_name
            ),
            warnings,
            rejection_reason: Some(format!(
                "Expected TransferPreapproval template, got {}.{}",
                tid.module_name, tid.entity_name
            )),
        });
    }

    // --- Check create arguments: receiver must match party ---
    let arg = create
        .argument
        .as_ref()
        .context("Create node missing argument")?;
    let record = as_record(arg).context("Create argument is not a Record")?;

    let receiver_val = get_record_field(record, "receiver");
    let receiver_party = receiver_val.and_then(get_party);

    if receiver_party != Some(party) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestPreapproval: receiver field does not match party".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "receiver={}, expected={}",
                receiver_party.unwrap_or("<missing>"),
                party
            )),
        });
    }

    // --- Check signatories contain party ---
    if !create.signatories.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "RequestPreapproval: party not in signatories".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "signatories {:?} does not contain party {}",
                create.signatories, party
            )),
        });
    }

    // --- Extract provider/operator for summary ---
    let provider = get_record_field(record, "provider")
        .or_else(|| get_record_field(record, "operator"))
        .and_then(get_party)
        .unwrap_or("<unknown>");

    let template_type = if is_cip56 { "CIP-56" } else { "Splice" };
    let summary = format!(
        "RequestPreapproval VERIFIED ({}): receiver {} preapproval with {} | 1 Create node",
        template_type, party, provider,
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// Recurring payment inspection (Phase B)
// ============================================================================

/// Verify a pay-as-you-go RecurringPaymentRequest Create transaction.
fn inspect_recurring_payasyougo(
    prepared: &PreparedTransaction,
    party: &str,
    app_party: &str,
    amount: &str,
) -> Result<InspectionResult> {
    let op_name = "RequestRecurringPayasyougo";
    let mut warnings = Vec::new();

    let tx = prepared
        .transaction
        .as_ref()
        .context("PreparedTransaction missing transaction")?;
    let metadata = prepared
        .metadata
        .as_ref()
        .context("PreparedTransaction missing metadata")?;
    let submitter = metadata
        .submitter_info
        .as_ref()
        .context("Metadata missing submitter_info")?;

    // --- Metadata: act_as must contain party ---
    if !submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in act_as", op_name),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                submitter.act_as, party
            )),
        });
    }

    // --- Must have exactly 1 root and 1 node (a Create) ---
    let [root_id] = tx.roots.as_slice() else {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: expected 1 root, got {}", op_name, tx.roots.len()),
            warnings,
            rejection_reason: Some(format!("Expected 1 root node, got {}", tx.roots.len())),
        });
    };

    if tx.nodes.len() != 1 {
        warnings.push(format!("Expected 1 node, got {}", tx.nodes.len()));
    }

    let root_node = tx
        .nodes
        .iter()
        .find(|n| n.node_id == *root_id)
        .context("Root node not found")?;

    let create = match get_node_type(root_node) {
        Some(NodeType::Create(c)) => c,
        _ => {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("{}: root is not a Create node", op_name),
                warnings,
                rejection_reason: Some("Root node must be a Create node".to_string()),
            });
        }
    };

    // --- Template must be RecurringPaymentRequest ---
    let tid = create
        .template_id
        .as_ref()
        .context("Create node missing template_id")?;
    if !template_matches(tid, "RecurringPaymentRequest", "RecurringPaymentRequest") {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!(
                "{}: wrong template {}.{}",
                op_name, tid.module_name, tid.entity_name
            ),
            warnings,
            rejection_reason: Some(format!(
                "Expected RecurringPaymentRequest template, got {}.{}",
                tid.module_name, tid.entity_name
            )),
        });
    }

    // --- Check create arguments ---
    let arg = create
        .argument
        .as_ref()
        .context("Create node missing argument")?;
    let record = as_record(arg).context("Create argument is not a Record")?;

    // user must match party
    let user_val = get_record_field(record, "user");
    let user_party = user_val.and_then(get_party);
    if user_party != Some(party) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: user field does not match party", op_name),
            warnings,
            rejection_reason: Some(format!(
                "user={}, expected={}",
                user_party.unwrap_or("<missing>"),
                party
            )),
        });
    }

    // app must match app_party
    let app_val = get_record_field(record, "app");
    let actual_app = app_val.and_then(get_party);
    if actual_app != Some(app_party) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: app field does not match app_party", op_name),
            warnings,
            rejection_reason: Some(format!(
                "app={}, expected={}",
                actual_app.unwrap_or("<missing>"),
                app_party
            )),
        });
    }

    // amountPerDayUsd — warn on mismatch (server may format differently)
    let amount_val = get_record_field(record, "amountPerDayUsd");
    let actual_amount = amount_val.and_then(get_numeric);
    if let Some(actual) = actual_amount {
        if !numeric_eq(actual, amount) {
            warnings.push(format!(
                "amountPerDayUsd mismatch: tx={}, expected={}",
                actual, amount
            ));
        }
    } else {
        warnings.push("amountPerDayUsd field missing or not numeric".to_string());
    }

    // --- Check signatories contain party ---
    if !create.signatories.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in signatories", op_name),
            warnings,
            rejection_reason: Some(format!(
                "signatories {:?} does not contain party {}",
                create.signatories, party
            )),
        });
    }

    let summary = format!(
        "{} VERIFIED: user {} requests from app {}, amount={}/day | 1 Create node",
        op_name,
        short(party, 16),
        short(app_party, 16),
        actual_amount.unwrap_or(amount),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

/// Verify a prepaid recurring payment Exercise transaction.
///
/// Prepaid uses ExerciseCommand on RecurringPaymentAppService with
/// choice RecurringPaymentAppService_LockFundsForRequest.
fn inspect_recurring_prepaid(
    prepared: &PreparedTransaction,
    party: &str,
    app_party: &str,
    amount: &str,
) -> Result<InspectionResult> {
    let op_name = "RequestRecurringPrepaid";
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, op_name) {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as must contain party ---
    if !parts.submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in act_as", op_name),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                parts.submitter.act_as, party
            )),
        });
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != "RecurringPaymentAppService_LockFundsForRequest" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: wrong choice_id: {}", op_name, parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=RecurringPaymentAppService_LockFundsForRequest, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: RecurringPaymentAppService ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "RecurringPaymentAppService", "RecurringPaymentAppService") {
            warnings.push(format!(
                "Unexpected template: {}.{}",
                tid.module_name, tid.entity_name
            ));
        }
    }

    // --- acting_parties must contain party (user) ---
    if !parts.root_exercise.acting_parties.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in acting_parties", op_name),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain party {}",
                parts.root_exercise.acting_parties, party
            )),
        });
    }

    // --- Check chosen_value fields ---
    let chosen = parts.root_exercise.chosen_value.as_ref();
    let chosen_record = chosen.and_then(as_record);

    if let Some(cr) = chosen_record {
        // user must match party
        let user_val = get_record_field(cr, "user");
        let user_party = user_val.and_then(get_party);
        if user_party != Some(party) {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("{}: user field does not match party", op_name),
                warnings,
                rejection_reason: Some(format!(
                    "user={}, expected={}",
                    user_party.unwrap_or("<missing>"),
                    party
                )),
            });
        }

        // amountPerDayUsd — warn on mismatch
        let amount_val = get_record_field(cr, "amountPerDayUsd");
        let actual_amount = amount_val.and_then(get_numeric);
        if let Some(actual) = actual_amount {
            if !numeric_eq(actual, amount) {
                warnings.push(format!(
                    "amountPerDayUsd mismatch: tx={}, expected={}",
                    actual, amount
                ));
            }
        } else {
            warnings.push("amountPerDayUsd field missing or not numeric".to_string());
        }
    } else {
        warnings.push("Could not parse chosen_value as Record".to_string());
    }

    // --- signatories must contain app_party (service owner) ---
    if !parts.root_exercise.signatories.contains(&app_party.to_string()) {
        warnings.push(format!(
            "signatories {:?} does not contain app_party {}",
            parts.root_exercise.signatories, app_party
        ));
    }

    let summary = format!(
        "{} VERIFIED: user {} requests from app {}, amount={}/day | {}",
        op_name,
        short(party, 16),
        short(app_party, 16),
        amount,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// Value extraction: ContractId
// ============================================================================

/// Get ContractId string from a Value
fn get_contract_id(value: &Value) -> Option<&str> {
    match value.sum.as_ref()? {
        Sum::ContractId(s) => Some(s.as_str()),
        _ => None,
    }
}

// ============================================================================
// Node summary helper
// ============================================================================

fn node_summary(tx: &proto_v2::interactive::DamlTransaction) -> String {
    let node_count = tx.nodes.len();
    let exercise_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Exercise(_))))
        .count();
    let fetch_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Fetch(_))))
        .count();
    let create_count = tx
        .nodes
        .iter()
        .filter(|n| matches!(get_node_type(n), Some(NodeType::Create(_))))
        .count();
    format!(
        "{} nodes ({} exercise, {} fetch, {} create)",
        node_count, exercise_count, fetch_count, create_count
    )
}

// ============================================================================
// Common: extract root exercise from prepared transaction
// ============================================================================

struct TxParts<'a> {
    tx: &'a proto_v2::interactive::DamlTransaction,
    submitter: &'a proto_v2::interactive::metadata::SubmitterInfo,
    metadata: &'a proto_v2::interactive::Metadata,
    nodes_dict: NodesDict<'a>,
    root_exercise: &'a Exercise,
}

fn extract_root_exercise<'a>(
    prepared: &'a PreparedTransaction,
    op_name: &str,
) -> Result<TxParts<'a>, InspectionResult> {
    let tx = prepared.transaction.as_ref().ok_or_else(|| InspectionResult {
        accepted: false,
        summary: format!("{}: missing transaction", op_name),
        warnings: vec![],
        rejection_reason: Some("PreparedTransaction missing transaction".to_string()),
    })?;
    let metadata = prepared.metadata.as_ref().ok_or_else(|| InspectionResult {
        accepted: false,
        summary: format!("{}: missing metadata", op_name),
        warnings: vec![],
        rejection_reason: Some("PreparedTransaction missing metadata".to_string()),
    })?;
    let submitter = metadata.submitter_info.as_ref().ok_or_else(|| InspectionResult {
        accepted: false,
        summary: format!("{}: missing submitter_info", op_name),
        warnings: vec![],
        rejection_reason: Some("Metadata missing submitter_info".to_string()),
    })?;

    let nodes_dict = build_nodes_dict(tx);

    let Some(root_id) = tx.roots.first() else {
        return Err(InspectionResult {
            accepted: false,
            summary: format!("{}: no root nodes", op_name),
            warnings: vec![],
            rejection_reason: Some("Transaction has no root nodes".to_string()),
        });
    };

    let root_node = nodes_dict.get(root_id).ok_or_else(|| InspectionResult {
        accepted: false,
        summary: format!("{}: root node not found", op_name),
        warnings: vec![],
        rejection_reason: Some("Root node not found in nodes dict".to_string()),
    })?;

    let root_exercise = match get_node_type(root_node) {
        Some(NodeType::Exercise(ex)) => ex,
        _ => {
            return Err(InspectionResult {
                accepted: false,
                summary: format!("{}: root is not Exercise", op_name),
                warnings: vec![],
                rejection_reason: Some("Root node is not an Exercise node".to_string()),
            });
        }
    };

    Ok(TxParts {
        tx,
        submitter,
        metadata,
        nodes_dict,
        root_exercise,
    })
}

// ============================================================================
// PayFee inspection (Phase B)
// ============================================================================

fn inspect_pay_fee(
    prepared: &PreparedTransaction,
    sender_party: &str,
    fee_party: &str,
    proposal_id: &str,
    fee_type: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "PayFee") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&sender_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "PayFee: sender not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain sender_party {}",
                parts.submitter.act_as, sender_party
            )),
        });
    }

    // --- command_id check ---
    let expected_prefix = format!("fee-{}-", fee_type);
    if !parts.submitter.command_id.starts_with(&expected_prefix) {
        warnings.push(format!(
            "command_id {} does not start with expected prefix {}",
            parts.submitter.command_id, expected_prefix
        ));
    }
    if !parts.submitter.command_id.ends_with(proposal_id) {
        warnings.push(format!(
            "command_id {} does not end with proposal_id {}",
            parts.submitter.command_id, proposal_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != "TransferFactory_Transfer" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("PayFee: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=TransferFactory_Transfer, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: ExternalPartyAmuletRules ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules") {
            warnings.push(format!(
                "Unexpected template: {}.{}",
                tid.module_name, tid.entity_name
            ));
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&sender_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "PayFee: sender not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain sender {}",
                parts.root_exercise.acting_parties, sender_party
            )),
        });
    }

    // --- chosen_value: transfer.receiver = fee_party ---
    let chosen = parts.root_exercise.chosen_value.as_ref();
    let chosen_record = chosen.and_then(as_record);

    if let Some(cr) = chosen_record {
        let transfer_val = get_record_field(cr, "transfer");
        if let Some(transfer) = transfer_val.and_then(as_record) {
            // Check receiver
            if let Some(receiver_val) = get_record_field(transfer, "receiver") {
                if let Some(receiver) = get_party(receiver_val) {
                    if receiver != fee_party {
                        return Ok(InspectionResult {
                            accepted: false,
                            summary: "PayFee: wrong receiver".to_string(),
                            warnings,
                            rejection_reason: Some(format!(
                                "transfer.receiver={}, expected fee_party={}",
                                receiver, fee_party
                            )),
                        });
                    }
                }
            } else {
                warnings.push("Could not extract transfer.receiver".to_string());
            }

            // Check instrumentId.id == "Amulet"
            let instrument_id = get_record_field(transfer, "instrumentId")
                .and_then(as_record)
                .and_then(|r| get_record_field(r, "id"))
                .and_then(get_text);
            if let Some(iid) = instrument_id {
                if iid != "Amulet" {
                    return Ok(InspectionResult {
                        accepted: false,
                        summary: format!("PayFee: wrong instrument: {}", iid),
                        warnings,
                        rejection_reason: Some(format!(
                            "instrumentId.id={}, expected=Amulet",
                            iid
                        )),
                    });
                }
            } else {
                warnings.push("Could not extract instrumentId.id".to_string());
            }
        } else {
            warnings.push("Could not extract 'transfer' from chosen_value".to_string());
        }
    } else {
        warnings.push("Could not parse chosen_value as Record".to_string());
    }

    let summary = format!(
        "PayFee({}) VERIFIED: {} pays fee to {} for {} | {}",
        fee_type,
        short(sender_party, 8),
        short(fee_party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// ProposeDvp inspection (Phase B)
// ============================================================================

fn inspect_propose_dvp(
    prepared: &PreparedTransaction,
    buyer_party: &str,
    proposal_id: &str,
    synchronizer_id: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "ProposeDvp") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&buyer_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "ProposeDvp: buyer not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain buyer_party {}",
                parts.submitter.act_as, buyer_party
            )),
        });
    }

    // --- Metadata: synchronizer_id ---
    if !synchronizer_id.is_empty() && parts.metadata.synchronizer_id != synchronizer_id {
        warnings.push(format!(
            "synchronizer_id mismatch: expected={}, got={}",
            synchronizer_id, parts.metadata.synchronizer_id
        ));
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-propose-{}", proposal_id);
    if parts.submitter.command_id != expected_cmd {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_cmd, parts.submitter.command_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != "UserService_ProposeDvp" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("ProposeDvp: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=UserService_ProposeDvp, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: UserService ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Utility.Settlement.App.V1.Service.User", "UserService") {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("ProposeDvp: wrong template: {}.{}", tid.module_name, tid.entity_name),
                warnings,
                rejection_reason: Some(format!(
                    "Expected UserService template, got {}.{}",
                    tid.module_name, tid.entity_name
                )),
            });
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&buyer_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "ProposeDvp: buyer not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain buyer {}",
                parts.root_exercise.acting_parties, buyer_party
            )),
        });
    }

    // --- chosen_value: terms.id must match proposal_id ---
    if let Some(chosen) = parts.root_exercise.chosen_value.as_ref() {
        if let Some(cr) = as_record(chosen) {
            let terms_id = get_record_field(cr, "terms")
                .and_then(as_record)
                .and_then(|t| get_record_field(t, "id"))
                .and_then(get_text);
            if let Some(tid) = terms_id {
                if tid != proposal_id {
                    return Ok(InspectionResult {
                        accepted: false,
                        summary: "ProposeDvp: wrong terms.id".to_string(),
                        warnings,
                        rejection_reason: Some(format!(
                            "chosen_value.terms.id={}, expected={}",
                            tid, proposal_id
                        )),
                    });
                }
            } else {
                warnings.push("Could not extract chosen_value.terms.id".to_string());
            }
        }
    }

    // --- Child Create node: DvpProposal template ---
    let mut dvp_proposal_created = false;
    for child_id in &parts.root_exercise.children {
        if let Some(child_node) = parts.nodes_dict.get(child_id) {
            if let Some(NodeType::Create(create)) = get_node_type(child_node) {
                if let Some(tid) = &create.template_id {
                    if template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "DvpProposal") {
                        dvp_proposal_created = true;
                        // Verify created DvpProposal terms.id
                        if let Some(arg) = &create.argument {
                            if let Some(rec) = as_record(arg) {
                                let created_id = get_record_field(rec, "terms")
                                    .and_then(as_record)
                                    .and_then(|t| get_record_field(t, "id"))
                                    .and_then(get_text);
                                if let Some(cid) = created_id {
                                    if cid != proposal_id {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "ProposeDvp: created DvpProposal has wrong terms.id".to_string(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DvpProposal.terms.id={}, expected={}",
                                                cid, proposal_id
                                            )),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !dvp_proposal_created {
        warnings.push("No DvpProposal Create node found in children".to_string());
    }

    let summary = format!(
        "ProposeDvp VERIFIED: buyer {} proposes {} | {}",
        short(buyer_party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// DvpProposal GC inspection (Cancel / Reject)
// ============================================================================

/// Verify a direct DvpProposal_Cancel / DvpProposal_Reject exercise: exactly
/// one root, the expected choice on the expected DvpProposal cid, our party
/// acting. Cancel must create nothing; Reject may create only the
/// RejectedDvpProposal.
fn inspect_dvp_proposal_gc(
    prepared: &PreparedTransaction,
    party: &str,
    dvp_proposal_cid: &str,
    choice: &str, // "DvpProposal_Cancel" | "DvpProposal_Reject"
) -> Result<InspectionResult> {
    let op_name = choice;
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, op_name) {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Single root: nothing may ride along with the archive ---
    if parts.tx.roots.len() != 1 {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: expected 1 root, got {}", op_name, parts.tx.roots.len()),
            warnings,
            rejection_reason: Some(format!(
                "Transaction has {} root nodes, expected exactly 1",
                parts.tx.roots.len()
            )),
        });
    }

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in act_as", op_name),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                parts.submitter.act_as, party
            )),
        });
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-proposal-gc-{}", dvp_proposal_cid);
    if parts.submitter.command_id != expected_cmd {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_cmd, parts.submitter.command_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != choice {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: wrong choice_id: {}", op_name, parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id={}, got={}",
                choice, parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: DvpProposal ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "DvpProposal") {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("{}: wrong template: {}.{}", op_name, tid.module_name, tid.entity_name),
                warnings,
                rejection_reason: Some(format!(
                    "Expected DvpProposal template, got {}.{}",
                    tid.module_name, tid.entity_name
                )),
            });
        }
    }

    // --- contract_id must match the expected DvpProposal ---
    if parts.root_exercise.contract_id != dvp_proposal_cid {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: wrong contract_id", op_name),
            warnings,
            rejection_reason: Some(format!(
                "contract_id={}, expected={}",
                parts.root_exercise.contract_id, dvp_proposal_cid
            )),
        });
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("{}: party not in acting_parties", op_name),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain party {}",
                parts.root_exercise.acting_parties, party
            )),
        });
    }

    // --- Children: Cancel creates nothing; Reject only a RejectedDvpProposal ---
    for child_id in &parts.root_exercise.children {
        if let Some(child_node) = parts.nodes_dict.get(child_id) {
            if let Some(NodeType::Create(create)) = get_node_type(child_node) {
                let allowed = choice == "DvpProposal_Reject"
                    && create.template_id.as_ref().is_some_and(|tid| {
                        template_matches(tid, "Utility.Settlement.App.V1.Model.Dvp", "RejectedDvpProposal")
                    });
                if !allowed {
                    let tname = create.template_id.as_ref()
                        .map(|t| format!("{}.{}", t.module_name, t.entity_name))
                        .unwrap_or_else(|| "unknown".to_string());
                    return Ok(InspectionResult {
                        accepted: false,
                        summary: format!("{}: unexpected Create of {}", op_name, tname),
                        warnings,
                        rejection_reason: Some(format!(
                            "{} must not create {} — only DvpProposal_Reject may create RejectedDvpProposal",
                            op_name, tname
                        )),
                    });
                }
            }
        }
    }

    let summary = format!(
        "{} VERIFIED: party {} archives {} | {}",
        op_name,
        short(party, 8),
        short(dvp_proposal_cid, 16),
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// AcceptDvp inspection (Phase B)
// ============================================================================

fn inspect_accept_dvp(
    prepared: &PreparedTransaction,
    seller_party: &str,
    proposal_id: &str,
    dvp_proposal_cid: &str,
    expected_delivery_amount: &str,
    expected_payment_amount: &str,
    expected_delivery_instrument_id: &str,
    expected_delivery_instrument_admin: &str,
    expected_payment_instrument_id: &str,
    expected_payment_instrument_admin: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "AcceptDvp") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&seller_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptDvp: seller not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain seller_party {}",
                parts.submitter.act_as, seller_party
            )),
        });
    }

    // --- Metadata: command_id ---
    let expected_cmd = format!("dvp-accept-{}", proposal_id);
    if parts.submitter.command_id != expected_cmd {
        warnings.push(format!(
            "command_id mismatch: expected={}, got={}",
            expected_cmd, parts.submitter.command_id
        ));
    }

    // --- Root exercise: choice_id ---
    if parts.root_exercise.choice_id != "UserService_AcceptDvpProposal" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("AcceptDvp: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=UserService_AcceptDvpProposal, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: UserService ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Utility.Settlement.App.V1.Service.User", "UserService") {
            return Ok(InspectionResult {
                accepted: false,
                summary: format!("AcceptDvp: wrong template: {}.{}", tid.module_name, tid.entity_name),
                warnings,
                rejection_reason: Some(format!(
                    "Expected UserService template, got {}.{}",
                    tid.module_name, tid.entity_name
                )),
            });
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&seller_party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "AcceptDvp: seller not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain seller {}",
                parts.root_exercise.acting_parties, seller_party
            )),
        });
    }

    // --- chosen_value: cid should reference dvp_proposal_cid ---
    if let Some(chosen) = parts.root_exercise.chosen_value.as_ref() {
        if let Some(cr) = as_record(chosen) {
            if let Some(cid_val) = get_record_field(cr, "cid") {
                if let Some(cid) = get_contract_id(cid_val) {
                    if !dvp_proposal_cid.is_empty() && cid != dvp_proposal_cid {
                        warnings.push(format!(
                            "chosen_value.cid={} does not match expected dvp_proposal_cid={}",
                            short(cid, 16),
                            short(dvp_proposal_cid, 16)
                        ));
                    }
                }
            }
        }
    }

    // --- Verify DVP terms from input contract (DvpProposal disclosed in metadata) ---
    use crate::decode::canton_proto::com::daml::ledger::api::v2::interactive::metadata::input_contract::Contract as InputContractOneof;

    let has_amount_check = !expected_delivery_amount.is_empty();
    let has_instrument_check = !expected_delivery_instrument_id.is_empty()
        || !expected_payment_instrument_id.is_empty();

    if has_amount_check || has_instrument_check {
        let mut found_dvp_proposal = false;
        for ic in &parts.metadata.input_contracts {
            let create = match &ic.contract {
                Some(InputContractOneof::V1(c)) => c,
                _ => continue,
            };
            let is_dvp_proposal = create
                .template_id
                .as_ref()
                .map(|t| {
                    template_matches(
                        t,
                        "Utility.Settlement.App.V1.Model.Dvp",
                        "DvpProposal",
                    )
                })
                .unwrap_or(false);
            if !is_dvp_proposal {
                continue;
            }
            found_dvp_proposal = true;

            if let Some(arg) = &create.argument {
                if let Some(rec) = as_record(arg) {
                    let terms = get_record_field(rec, "terms").and_then(as_record);
                    if let Some(terms) = terms {
                        // Extract deliveries[0] InstrumentQuantity
                        let del_iq = get_record_field(terms, "deliveries")
                            .and_then(as_list)
                            .and_then(|l| l.first())
                            .and_then(as_record);
                        // Extract payments[0] InstrumentQuantity
                        let pay_iq = get_record_field(terms, "payments")
                            .and_then(as_list)
                            .and_then(|l| l.first())
                            .and_then(as_record);

                        // --- Delivery checks ---
                        if let Some(iq) = del_iq {
                            let (del_id, del_admin, del_amount) =
                                extract_instrument_quantity(iq);

                            if !expected_delivery_amount.is_empty() {
                                if let Some(amt) = del_amount {
                                    if !numeric_eq(amt, expected_delivery_amount) {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "AcceptDvp: delivery amount mismatch"
                                                .into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP delivery={}, expected={}",
                                                amt, expected_delivery_amount
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract deliveries[0].amount from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }

                            if !expected_delivery_instrument_id.is_empty() {
                                if let Some(id) = del_id {
                                    if id != expected_delivery_instrument_id {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "AcceptDvp: delivery instrument_id mismatch"
                                                .into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP delivery instrument.id={}, expected={}",
                                                id, expected_delivery_instrument_id
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract deliveries[0].instrument.id from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }

                            if !expected_delivery_instrument_admin.is_empty() {
                                if let Some(admin) = del_admin {
                                    if admin != expected_delivery_instrument_admin {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary:
                                                "AcceptDvp: delivery instrument_admin mismatch"
                                                    .into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP delivery instrument.admin={}, expected={}",
                                                short(admin, 20),
                                                short(expected_delivery_instrument_admin, 20)
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract deliveries[0].instrument.admin from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }
                        } else {
                            warnings.push(
                                "Could not extract deliveries[0] from DvpProposal".to_string(),
                            );
                        }

                        // --- Payment checks ---
                        if let Some(iq) = pay_iq {
                            let (pay_id, pay_admin, pay_amount) =
                                extract_instrument_quantity(iq);

                            if !expected_payment_amount.is_empty() {
                                if let Some(amt) = pay_amount {
                                    if !numeric_eq(amt, expected_payment_amount) {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "AcceptDvp: payment amount mismatch".into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP payment={}, expected={}",
                                                amt, expected_payment_amount
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract payments[0].amount from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }

                            if !expected_payment_instrument_id.is_empty() {
                                if let Some(id) = pay_id {
                                    if id != expected_payment_instrument_id {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary: "AcceptDvp: payment instrument_id mismatch"
                                                .into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP payment instrument.id={}, expected={}",
                                                id, expected_payment_instrument_id
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract payments[0].instrument.id from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }

                            if !expected_payment_instrument_admin.is_empty() {
                                if let Some(admin) = pay_admin {
                                    if admin != expected_payment_instrument_admin {
                                        return Ok(InspectionResult {
                                            accepted: false,
                                            summary:
                                                "AcceptDvp: payment instrument_admin mismatch"
                                                    .into(),
                                            warnings,
                                            rejection_reason: Some(format!(
                                                "DVP payment instrument.admin={}, expected={}",
                                                short(admin, 20),
                                                short(expected_payment_instrument_admin, 20)
                                            )),
                                        });
                                    }
                                } else {
                                    warnings.push(
                                        "Could not extract payments[0].instrument.admin from DvpProposal"
                                            .to_string(),
                                    );
                                }
                            }
                        } else {
                            warnings.push(
                                "Could not extract payments[0] from DvpProposal".to_string(),
                            );
                        }
                    }
                }
            }
            break; // only one DvpProposal expected
        }
        if !found_dvp_proposal {
            warnings.push(
                "No DvpProposal found in input_contracts to verify terms".to_string(),
            );
        }
    }

    let summary = format!(
        "AcceptDvp VERIFIED: seller {} accepts proposal {} | {}",
        short(seller_party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// Allocate inspection (Phase B)
// ============================================================================

fn inspect_allocate(
    prepared: &PreparedTransaction,
    party: &str,
    proposal_id: &str,
    _dvp_cid: &str,
) -> Result<InspectionResult> {
    let mut warnings = Vec::new();

    let parts = match extract_root_exercise(prepared, "Allocate") {
        Ok(p) => p,
        Err(rejection) => return Ok(rejection),
    };

    // --- Metadata: act_as ---
    if !parts.submitter.act_as.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "Allocate: party not in act_as".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "act_as {:?} does not contain party {}",
                parts.submitter.act_as, party
            )),
        });
    }

    // --- Metadata: command_id ---
    if !parts.submitter.command_id.contains(proposal_id) {
        warnings.push(format!(
            "command_id {} does not contain proposal_id {}",
            parts.submitter.command_id, proposal_id
        ));
    }

    // --- Root exercise: choice_id should be AllocationFactory_Allocate ---
    if parts.root_exercise.choice_id != "AllocationFactory_Allocate" {
        return Ok(InspectionResult {
            accepted: false,
            summary: format!("Allocate: wrong choice_id: {}", parts.root_exercise.choice_id),
            warnings,
            rejection_reason: Some(format!(
                "Expected choice_id=AllocationFactory_Allocate, got={}",
                parts.root_exercise.choice_id
            )),
        });
    }

    // --- Template: ExternalPartyAmuletRules (implements AllocationFactory) ---
    if let Some(tid) = &parts.root_exercise.template_id {
        if !template_matches(tid, "Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules")
            && !template_matches(tid, "Utility.Registry.App.V0.Service.AllocationFactory", "AllocationFactory")
        {
            warnings.push(format!(
                "Unexpected template: {}.{} (expected ExternalPartyAmuletRules or AllocationFactory)",
                tid.module_name, tid.entity_name
            ));
        }
    }

    // --- acting_parties ---
    if !parts.root_exercise.acting_parties.contains(&party.to_string()) {
        return Ok(InspectionResult {
            accepted: false,
            summary: "Allocate: party not in acting_parties".to_string(),
            warnings,
            rejection_reason: Some(format!(
                "acting_parties {:?} does not contain party {}",
                parts.root_exercise.acting_parties, party
            )),
        });
    }

    let summary = format!(
        "Allocate VERIFIED: {} allocates for {} | {}",
        short(party, 8),
        proposal_id,
        node_summary(parts.tx),
    );

    debug!("TX INSPECT: {}", summary);

    Ok(InspectionResult {
        accepted: true,
        summary,
        warnings,
        rejection_reason: None,
    })
}

// ============================================================================
// Main inspection entry point
// ============================================================================

/// Inspect the `PreparedTransaction` fields against expected operation.
///
/// Fully verified: TransferCc, AcceptCip56, RequestUserService, PayFee,
///                 ProposeDvp, AcceptDvp, Allocate.
/// Stub: TransferCip56 (send-side).
pub fn inspect(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    verbose: bool,
) -> Result<InspectionResult> {
    let op_name = match expectation {
        OperationExpectation::PayFee { fee_type, .. } => format!("PayFee({})", fee_type),
        OperationExpectation::ProposeDvp { .. } => "ProposeDvp".to_string(),
        OperationExpectation::AcceptDvp { .. } => "AcceptDvp".to_string(),
        OperationExpectation::Allocate { .. } => "Allocate".to_string(),
        OperationExpectation::CancelDvpProposal { .. } => "CancelDvpProposal".to_string(),
        OperationExpectation::RejectDvpProposal { .. } => "RejectDvpProposal".to_string(),
        OperationExpectation::TransferCc { amount, .. } => format!("TransferCc({})", amount),
        OperationExpectation::PrepayTraffic { amount, .. } => format!("PrepayTraffic({})", amount),
        OperationExpectation::RequestPreapproval { .. } => "RequestPreapproval".to_string(),
        OperationExpectation::RequestRecurringPrepaid { .. } => {
            "RequestRecurringPrepaid".to_string()
        }
        OperationExpectation::RequestRecurringPayasyougo { .. } => {
            "RequestRecurringPayasyougo".to_string()
        }
        OperationExpectation::RequestUserService { .. } => "RequestUserService".to_string(),
        OperationExpectation::TransferCip56 {
            amount,
            instrument_id,
            ..
        } => format!("TransferCip56({} {})", amount, instrument_id),
        OperationExpectation::AcceptCip56 { contract_id, .. } => {
            format!("AcceptCip56({})", short(contract_id, 16))
        }
        OperationExpectation::SplitCc { output_amounts, .. } => {
            format!("SplitCc({} outputs)", output_amounts.len())
        }
        OperationExpectation::ExecuteMulticall { op_count, .. } => {
            format!("ExecuteMulticall({} ops)", op_count)
        }
        OperationExpectation::LockHoldings { amount, instrument_id, .. } => {
            format!("LockHoldings({} {})", amount, instrument_id)
        }
        OperationExpectation::ProcessLockUnlockRequests { request_count, .. } => {
            format!("ProcessLockUnlockRequests({} reqs)", request_count)
        }
        OperationExpectation::ResizeLock { new_amount, .. } => {
            format!("ResizeLock({})", new_amount)
        }
        OperationExpectation::TerminateLock { .. } => "TerminateLock".to_string(),
        OperationExpectation::AtomicDvpSettle { quote_id, ticket_id, .. } => {
            let mode = if ticket_id.is_empty() { "ticketless" } else { "ticketed" };
            format!("AtomicDvpSettle({}, {})", quote_id, mode)
        }
        OperationExpectation::IssueTickets { ticket_count, .. } => {
            format!("IssueTickets({} tickets)", ticket_count)
        }
        OperationExpectation::SplitHoldings { instrument_id, split_count, .. } => {
            format!("SplitHoldings({} {} splits)", split_count, instrument_id)
        }
        OperationExpectation::CreateAtomicDvpVenue { pair_name, .. } => {
            format!("CreateAtomicDvpVenue({})", pair_name)
        }
        OperationExpectation::UpdateVenueKey { .. } => "UpdateVenueKey".to_string(),
        OperationExpectation::RetireVenue { .. } => "RetireVenue".to_string(),
        OperationExpectation::CancelTickets { ticket_count, .. } => {
            format!("CancelTickets({} tickets)", ticket_count)
        }
    };

    // Verbose: dump full JSON
    if verbose {
        log_verbose(prepared_transaction_bytes, expectation);
    }

    // Dispatch based on expectation type
    match expectation {
        OperationExpectation::TransferCc {
            sender_party,
            receiver_party,
            amount,
            command_id,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_transfer_cc(&prepared, sender_party, receiver_party, amount, command_id)
        }
        OperationExpectation::AcceptCip56 {
            receiver_party,
            contract_id,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_accept_cip56(&prepared, receiver_party, contract_id)
        }
        OperationExpectation::RequestUserService { party } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_request_user_service(&prepared, party)
        }
        OperationExpectation::PayFee {
            sender_party,
            fee_party,
            proposal_id,
            fee_type,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_pay_fee(&prepared, sender_party, fee_party, proposal_id, fee_type)
        }
        OperationExpectation::ProposeDvp {
            buyer_party,
            proposal_id,
            synchronizer_id,
            ..
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_propose_dvp(&prepared, buyer_party, proposal_id, synchronizer_id)
        }
        OperationExpectation::AcceptDvp {
            seller_party,
            proposal_id,
            dvp_proposal_cid,
            expected_delivery_amount,
            expected_payment_amount,
            expected_delivery_instrument_id,
            expected_delivery_instrument_admin,
            expected_payment_instrument_id,
            expected_payment_instrument_admin,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_accept_dvp(
                &prepared,
                seller_party,
                proposal_id,
                dvp_proposal_cid,
                expected_delivery_amount,
                expected_payment_amount,
                expected_delivery_instrument_id,
                expected_delivery_instrument_admin,
                expected_payment_instrument_id,
                expected_payment_instrument_admin,
            )
        }
        OperationExpectation::Allocate {
            party,
            proposal_id,
            dvp_cid,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_allocate(&prepared, party, proposal_id, dvp_cid)
        }
        OperationExpectation::CancelDvpProposal { party, dvp_proposal_cid } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_dvp_proposal_gc(&prepared, party, dvp_proposal_cid, "DvpProposal_Cancel")
        }
        OperationExpectation::RejectDvpProposal { party, dvp_proposal_cid } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_dvp_proposal_gc(&prepared, party, dvp_proposal_cid, "DvpProposal_Reject")
        }
        OperationExpectation::RequestPreapproval { party } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_request_preapproval(&prepared, party)
        }
        OperationExpectation::RequestRecurringPayasyougo {
            party,
            app_party,
            amount,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_recurring_payasyougo(&prepared, party, app_party, amount)
        }
        OperationExpectation::RequestRecurringPrepaid {
            party,
            app_party,
            amount,
        } => {
            let prepared = PreparedTransaction::decode(prepared_transaction_bytes)
                .context("Failed to decode PreparedTransaction protobuf")?;
            inspect_recurring_prepaid(&prepared, party, app_party, amount)
        }
        // TransferCip56 (send-side) not yet implemented
        _ => {
            debug!(
                "TX INSPECT [{}]: {} bytes (not yet verified)",
                op_name,
                prepared_transaction_bytes.len(),
            );
            Ok(InspectionResult {
                accepted: true,
                summary: format!(
                    "[STUB] {} — {} bytes, not yet verified",
                    op_name,
                    prepared_transaction_bytes.len()
                ),
                warnings: vec![
                    "Transaction fields not yet verified for this operation".to_string()
                ],
                rejection_reason: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use std::time::{Duration, Instant};

    // 3-byte chars: byte cuts at 8, 16 and 20 all land inside a character
    const P: &str = "€€€€€€€€::1220aa";
    const Q: &str = "₿₿₿₿₿₿₿₿::1220bb";
    const CID: &str = "€€€€€€€€€€€€€€€€€€";
    // longer than the 16/20-character summary cuts
    const P_LONG: &str = "€€€€€€€€::1220aa-extra-suffix";
    const Q_LONG: &str = "₿₿₿₿₿₿₿₿::1220bb-extra-suffix";

    fn run(p: &PreparedTransaction, exp: OperationExpectation) -> InspectionResult {
        inspect(&encode(p), &exp, false).expect("inspection must not error")
    }

    fn root_exercise(choice: &str, acting: &[&str]) -> Exercise {
        exercise(choice, acting, vec![])
    }

    fn single_exercise(ex: Exercise, act_as: &[&str]) -> PreparedTransaction {
        prepared(&["0"], vec![exercise_node("0", ex)], vec![], act_as)
    }

    #[test]
    fn transfer_cc_summary_with_multibyte_parties() {
        let mut ex = root_exercise("TransferFactory_Transfer", &[P]);
        ex.template_id = Some(ident("Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules"));
        ex.chosen_value = Some(record(vec![(
            "transfer",
            record(vec![("sender", party(P)), ("receiver", party(Q)), ("amount", numeric("1.0"))]),
        )]));
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::TransferCc {
                sender_party: P.into(),
                receiver_party: Q.into(),
                amount: "1".into(),
                command_id: "cmd".into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("€€€€€€€€ → ₿₿₿₿₿₿₿₿"), "{}", r.summary);
    }

    #[test]
    fn recurring_payasyougo_summary_with_multibyte_parties() {
        let c = create(
            ident("RecurringPaymentRequest", "RecurringPaymentRequest"),
            record(vec![("user", party(P_LONG)), ("app", party(Q_LONG)), ("amountPerDayUsd", numeric("5.0"))]),
            &[P_LONG],
        );
        let p = prepared(&["0"], vec![create_node("0", c)], vec![], &[P_LONG]);
        let r = run(
            &p,
            OperationExpectation::RequestRecurringPayasyougo {
                party: P_LONG.into(),
                app_party: Q_LONG.into(),
                amount: "5".into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("user €€€€€€€€::1220aa requests from app ₿₿₿₿₿₿₿₿::1220bb,"), "{}", r.summary);
    }

    #[test]
    fn recurring_prepaid_summary_with_multibyte_parties() {
        let ex = root_exercise("RecurringPaymentAppService_LockFundsForRequest", &[P_LONG]);
        let r = run(
            &single_exercise(ex, &[P_LONG]),
            OperationExpectation::RequestRecurringPrepaid {
                party: P_LONG.into(),
                app_party: Q_LONG.into(),
                amount: "5".into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("user €€€€€€€€::1220aa requests from app ₿₿₿₿₿₿₿₿::1220bb,"), "{}", r.summary);
    }

    #[test]
    fn pay_fee_summary_with_multibyte_parties() {
        let ex = root_exercise("TransferFactory_Transfer", &[P]);
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::PayFee {
                sender_party: P.into(),
                fee_party: Q.into(),
                proposal_id: "prop".into(),
                fee_type: "dvp".into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("€€€€€€€€ pays fee to ₿₿₿₿₿₿₿₿ for prop"), "{}", r.summary);
    }

    #[test]
    fn propose_dvp_summary_with_multibyte_party() {
        let mut ex = root_exercise("UserService_ProposeDvp", &[P]);
        ex.template_id = Some(ident("Utility.Settlement.App.V1.Service.User", "UserService"));
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::ProposeDvp {
                buyer_party: P.into(),
                seller_party: Q.into(),
                proposal_id: "prop".into(),
                synchronizer_id: String::new(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("buyer €€€€€€€€ proposes prop"), "{}", r.summary);
    }

    #[test]
    fn dvp_proposal_gc_summary_with_multibyte_ids() {
        let mut ex = root_exercise("DvpProposal_Cancel", &[P]);
        ex.template_id = Some(ident("Utility.Settlement.App.V1.Model.Dvp", "DvpProposal"));
        ex.contract_id = CID.into();
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::CancelDvpProposal {
                party: P.into(),
                dvp_proposal_cid: CID.into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("party €€€€€€€€ archives €€€€€€€€€€€€€€€€ |"), "{}", r.summary);
    }

    #[test]
    fn allocate_summary_with_multibyte_party() {
        let ex = root_exercise("AllocationFactory_Allocate", &[P]);
        let r = run(
            &single_exercise(ex, &[P]),
            OperationExpectation::Allocate {
                party: P.into(),
                proposal_id: "prop".into(),
                dvp_cid: String::new(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("€€€€€€€€ allocates for prop"), "{}", r.summary);
    }

    fn accept_dvp_expectation(delivery_admin: &str, payment_admin: &str) -> OperationExpectation {
        OperationExpectation::AcceptDvp {
            seller_party: P.into(),
            proposal_id: "prop".into(),
            dvp_proposal_cid: CID.into(),
            expected_delivery_amount: String::new(),
            expected_payment_amount: String::new(),
            expected_delivery_instrument_id: "D".into(),
            expected_delivery_instrument_admin: delivery_admin.into(),
            expected_payment_instrument_id: "Q".into(),
            expected_payment_instrument_admin: payment_admin.into(),
        }
    }

    fn accept_dvp_tx(delivery_admin: &str, payment_admin: &str) -> PreparedTransaction {
        let mut ex = root_exercise("UserService_AcceptDvpProposal", &[P]);
        ex.template_id = Some(ident("Utility.Settlement.App.V1.Service.User", "UserService"));
        ex.chosen_value = Some(record(vec![("cid", contract_id(&format!("{CID}x")))]));
        let iq = |id: &str, admin: &str| {
            record(vec![
                ("instrument", record(vec![("id", text(id)), ("admin", party(admin))])),
                ("amount", numeric("1.0")),
            ])
        };
        let proposal = create(
            ident("Utility.Settlement.App.V1.Model.Dvp", "DvpProposal"),
            record(vec![(
                "terms",
                record(vec![
                    ("deliveries", list(vec![iq("D", delivery_admin)])),
                    ("payments", list(vec![iq("Q", payment_admin)])),
                ]),
            )]),
            &[P],
        );
        with_input_contract(single_exercise(ex, &[P]), proposal)
    }

    #[test]
    fn accept_dvp_summary_and_cid_warning_with_multibyte_ids() {
        let r = run(&accept_dvp_tx(Q, Q), accept_dvp_expectation(Q, Q));
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("seller €€€€€€€€ accepts proposal prop"), "{}", r.summary);
        assert!(
            r.warnings.iter().any(|w| w.contains("€€€€€€€€€€€€€€€€ does not match")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn accept_dvp_admin_mismatch_with_multibyte_admins() {
        let r = run(&accept_dvp_tx(P_LONG, Q_LONG), accept_dvp_expectation(Q_LONG, Q_LONG));
        assert!(!r.accepted);
        assert_eq!(
            r.rejection_reason.as_deref(),
            Some("DVP delivery instrument.admin=€€€€€€€€::1220aa-ext, expected=₿₿₿₿₿₿₿₿::1220bb-ext")
        );
        let r = run(&accept_dvp_tx(Q_LONG, P_LONG), accept_dvp_expectation(Q_LONG, Q_LONG));
        assert!(!r.accepted);
        assert_eq!(
            r.rejection_reason.as_deref(),
            Some("DVP payment instrument.admin=€€€€€€€€::1220aa-ext, expected=₿₿₿₿₿₿₿₿::1220bb-ext")
        );
    }

    #[test]
    fn accept_cip56_with_multibyte_contract_id_does_not_panic() {
        let p = prepared(&[], vec![], vec![], &[P]);
        let r = run(
            &p,
            OperationExpectation::AcceptCip56 {
                receiver_party: P.into(),
                contract_id: CID.into(),
            },
        );
        assert!(!r.accepted);
        assert_eq!(r.rejection_reason.as_deref(), Some("Transaction has no root nodes"));
    }

    #[test]
    fn root_count_rejections() {
        let c = || create(ident("M", "T"), record(vec![]), &[P]);
        for roots in [vec![], vec!["0", "1"]] {
            let p = prepared(&roots, vec![create_node("0", c()), create_node("1", c())], vec![], &[P]);
            for exp in [
                OperationExpectation::RequestUserService { party: P.into() },
                OperationExpectation::RequestPreapproval { party: P.into() },
                OperationExpectation::RequestRecurringPayasyougo {
                    party: P.into(),
                    app_party: Q.into(),
                    amount: "1".into(),
                },
            ] {
                let r = run(&p, exp);
                assert!(!r.accepted);
                assert_eq!(
                    r.rejection_reason,
                    Some(format!("Expected 1 root node, got {}", roots.len()))
                );
            }
        }
        let empty = prepared(&[], vec![], vec![], &[P]);
        for exp in [
            OperationExpectation::TransferCc {
                sender_party: P.into(),
                receiver_party: Q.into(),
                amount: "1".into(),
                command_id: "cmd".into(),
            },
            OperationExpectation::Allocate {
                party: P.into(),
                proposal_id: "prop".into(),
                dvp_cid: String::new(),
            },
        ] {
            let r = run(&empty, exp);
            assert!(!r.accepted);
            assert_eq!(r.rejection_reason.as_deref(), Some("Transaction has no root nodes"));
        }
    }

    #[test]
    fn accept_cip56_duplicate_children_are_scanned_once() {
        // root lists child "1" 10k times and "1" lists a missing "2" 10k times
        const C: usize = 10_000;
        let mut root = root_exercise("TransferInstruction_Accept", &["r"]);
        root.contract_id = "cid".into();
        root.children = vec!["1".to_string(); C];
        let mut mid = root_exercise("Other", &["r"]);
        mid.children = vec!["2".to_string(); C];
        let p = prepared(&["0"], vec![exercise_node("0", root), exercise_node("1", mid)], vec![], &["r"]);
        let start = Instant::now();
        let r = run(
            &p,
            OperationExpectation::AcceptCip56 {
                receiver_party: "r".into(),
                contract_id: "cid".into(),
            },
        );
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
        assert!(r.accepted, "{r:?}");
        assert!(r.warnings.iter().any(|w| w == "TransferRule_Transfer not found in exercise tree"));
    }

    #[test]
    fn accept_cip56_finds_transfer_rule_one_level_down() {
        let mut root = root_exercise("TransferInstruction_Accept", &["r"]);
        root.contract_id = "cid".into();
        root.children = ids(&["1", "1", "2"]);
        let p = prepared(
            &["0"],
            vec![
                exercise_node("0", root),
                exercise_node("1", root_exercise("Other", &["r"])),
                exercise_node("2", exercise("Other", &["r"], ids(&["3"]))),
                exercise_node("3", root_exercise("TransferRule_Transfer", &["r"])),
            ],
            vec![],
            &["r"],
        );
        let r = run(
            &p,
            OperationExpectation::AcceptCip56 {
                receiver_party: "r".into(),
                contract_id: "cid".into(),
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(!r.warnings.iter().any(|w| w.contains("TransferRule_Transfer")), "{:?}", r.warnings);
    }

    #[test]
    fn verbose_dump_skips_large_transactions() {
        let big = vec![0u8; VERBOSE_DUMP_MAX_BYTES + 1];
        assert_eq!(
            prepared_json_for_log(&big),
            format!("({} bytes, above the {} byte verbose limit; not dumped)", big.len(), VERBOSE_DUMP_MAX_BYTES)
        );
        let small = encode(&prepared(&["0"], vec![exercise_node("0", root_exercise("C", &[P]))], vec![seed(0, 7)], &[P]));
        let json = prepared_json_for_log(&small);
        assert!(json.contains("\"seed\": \"0707"), "{json}");
    }

    #[test]
    fn compaction_rewrites_byte_arrays_and_skips_other_shapes() {
        let mut v = serde_json::json!({
            "metadata": {"input_contracts": [{"event_blob": [1, 2, 3]}, [1, 2], 5, {"event_blob": "x"}]},
            "transaction": {"node_seeds": [{"seed": [255, 1]}, "y", {"seed": 3}]}
        });
        compact_event_blobs(&mut v);
        compact_seeds(&mut v);
        assert_eq!(
            v,
            serde_json::json!({
                "metadata": {"input_contracts": [{"event_blob": "base58:Ldp"}, [1, 2], 5, {"event_blob": "x"}]},
                "transaction": {"node_seeds": [{"seed": "ff01"}, "y", {"seed": 3}]}
            })
        );
    }
}
