//! CC inspection: transfers, traffic top-ups and splits, and the CC transfer check
//! batch payments share.

use super::*;
use proto_v2::interactive::metadata::SubmitterInfo;
use proto_v2::interactive::DamlTransaction;

/// The longest `executeBefore` window of a direct CC transfer, in microseconds.
const CC_TRANSFER_WINDOW_MICROS: i64 = 600 * 1_000_000;

const AMULET_MODULE: &str = "Splice.Amulet";
const AMULET_RULES_MODULE: &str = "Splice.AmuletRules";
const TRANSFER_INSTRUCTION_MODULE: &str = "Splice.Api.Token.TransferInstructionV1";

/// The context entries a CC transfer may name.
const CC_CONTEXT_KEYS: [&str; 5] = [
    "amulet-rules",
    "open-round",
    "external-party-config-state",
    "featured-app-right",
    "transfer-preapproval",
];

/// TransferCc: a CC transfer to another party.
pub(super) fn inspect_transfer_cc(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::TransferCc { sender_party, receiver_party, amount, command_id, dso_party } = expectation
    else {
        return Err(wrong_expectation("TransferCc"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let result = check_transfer_cc(&prepared, sender_party, receiver_party, amount)?;
    if result.accepted {
        let leg = CcLeg {
            sender: sender_party,
            receiver: Some(receiver_party),
            amount,
            window_micros: CC_TRANSFER_WINDOW_MICROS,
        };
        if let Err(reason) = check_single_cc_transfer(&prepared, &leg, dso_party.as_deref(), command_id) {
            observed.push(reason);
        }
    }
    Ok(result)
}

/// PrepayTraffic: a CC transfer to the prepaid-traffic party.
pub(super) fn inspect_prepay_traffic(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::PrepayTraffic { sender_party, amount, command_id, receiver_party, dso_party } = expectation
    else {
        return Err(wrong_expectation("PrepayTraffic"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let mut warnings = Vec::new();
    if receiver_party.is_none() {
        warnings.push("PARTY_PREPAID_TRAFFIC is not pinned: the top-up receiver is not checked".to_string());
    }
    let leg = CcLeg {
        sender: sender_party,
        receiver: receiver_party.as_deref(),
        amount,
        window_micros: CC_TRANSFER_WINDOW_MICROS,
    };
    let summary = match check_single_cc_transfer(&prepared, &leg, dso_party.as_deref(), command_id) {
        Ok(seen) => format!(
            "PrepayTraffic VERIFIED: {} CC {} → {}",
            amount,
            short(sender_party, 8),
            short(&seen.receiver, 8)
        ),
        Err(reason) => {
            let summary = format!("PrepayTraffic check failed: {reason}");
            observed.push(reason);
            summary
        }
    };
    debug!("TX INSPECT: {}", summary);
    Ok(InspectionResult { accepted: true, summary, warnings, rejection_reason: None })
}

/// SplitCc: the party's own coins split (or merged) into new coins of its own.
pub(super) fn inspect_split_cc(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::SplitCc { party, output_amounts, amulet_cids, dso_party } = expectation else {
        return Err(wrong_expectation("SplitCc"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let mut warnings = Vec::new();
    if amulet_cids.is_empty() {
        warnings.push("The split names no input coins: any coins of the party are accepted".to_string());
    }
    let summary = match check_split_cc(
        &prepared,
        prepared_transaction_bytes,
        party,
        output_amounts,
        amulet_cids,
        dso_party.as_deref(),
    ) {
        Ok(inputs) => format!(
            "SplitCc VERIFIED: {} coin(s) of {} into {} output(s)",
            inputs,
            short(party, 8),
            output_amounts.len()
        ),
        Err(reason) => {
            let summary = format!("SplitCc check failed: {reason}");
            observed.push(reason);
            summary
        }
    };
    debug!("TX INSPECT: {}", summary);
    Ok(InspectionResult { accepted: true, summary, warnings, rejection_reason: None })
}

// ============================================================================
// The CC transfer check (TransferCc, PrepayTraffic, each BatchPayCc target)
// ============================================================================

/// One CC transfer the agent expects.
pub(super) struct CcLeg<'a> {
    pub(super) sender: &'a str,
    /// `None` when the receiver is not pinned: the transaction's own receiver is used.
    pub(super) receiver: Option<&'a str>,
    pub(super) amount: &'a str,
    /// The longest `executeBefore` - `requestedAt` window, in microseconds.
    pub(super) window_micros: i64,
}

/// What a checked CC transfer does.
pub(super) struct CcTransferSeen {
    pub(super) receiver: String,
}

/// A transaction that is one direct CC transfer.
fn check_single_cc_transfer(
    prepared: &PreparedTransaction,
    leg: &CcLeg<'_>,
    pinned_dso: Option<&str>,
    command_id: &str,
) -> Result<CcTransferSeen, String> {
    let (tx, metadata, submitter) = tx_parts(prepared)?;
    only_submitter(submitter, leg.sender)?;
    if submitter.command_id != command_id {
        return Err(format!(
            "command id {} is not the requested {}",
            submitter.command_id, command_id
        ));
    }
    let nodes = build_nodes_dict(tx);
    let root = single_root_exercise(tx, &nodes)?;
    let chosen = chosen_record(root)?;
    let transfer = record_at(chosen, "transfer")?;
    let admins = [
        ("expectedAdmin", party_at(chosen, "expectedAdmin")?),
        ("the instrument admin", party_at(record_at(transfer, "instrumentId")?, "admin")?),
    ];
    let coins = cids_at(transfer, "inputHoldingCids")?;
    let dso = resolve_dso(pinned_dso, &admins, metadata, &coins, leg.sender)?;
    check_cc_transfer_node(root, &nodes, metadata, leg, &dso)
}

/// One `TransferFactory_Transfer` on the external-party amulet rules, completed
/// through the receiver's preapproval, and everything below it.
pub(super) fn check_cc_transfer_node(
    ex: &Exercise,
    nodes: &NodesDict<'_>,
    metadata: &Metadata,
    leg: &CcLeg<'_>,
    dso: &str,
) -> Result<CcTransferSeen, String> {
    if ex.choice_id != "TransferFactory_Transfer" {
        return Err(format!("the transfer is {}", exercise_name(ex)));
    }
    if !is_template(ex.template_id.as_ref(), "Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules") {
        return Err(format!("the transfer factory is {}", template_name(ex.template_id.as_ref())));
    }
    if !is_template(ex.interface_id.as_ref(), TRANSFER_INSTRUCTION_MODULE, "TransferFactory") {
        return Err("the transfer is not exercised on the TransferFactory interface".to_string());
    }
    only_acting(ex, leg.sender)?;
    let chosen = chosen_record(ex)?;
    let expected_admin = party_at(chosen, "expectedAdmin")?;
    if expected_admin != dso {
        return Err(format!("expectedAdmin {expected_admin} is not the DSO {dso}"));
    }
    let transfer = record_at(chosen, "transfer")?;
    let sender = party_at(transfer, "sender")?;
    if sender != leg.sender {
        return Err(format!("the transfer sender {sender} is not {}", leg.sender));
    }
    let receiver = party_at(transfer, "receiver")?;
    if let Some(expected) = leg.receiver
        && receiver != expected
    {
        return Err(format!("the transfer receiver {receiver} is not {expected}"));
    }
    let amount = numeric_at(transfer, "amount")?;
    if !numeric_eq(amount, leg.amount) {
        return Err(format!("the transfer amount {amount} is not {}", leg.amount));
    }
    let instrument = record_at(transfer, "instrumentId")?;
    let admin = party_at(instrument, "admin")?;
    if admin != dso {
        return Err(format!("the instrument admin {admin} is not the DSO {dso}"));
    }
    let id = text_at(instrument, "id")?;
    if id != "Amulet" {
        return Err(format!("the instrument {id} is not Amulet"));
    }
    let inputs = cids_at(transfer, "inputHoldingCids")?;
    if inputs.is_empty() {
        return Err("the transfer names no input coins".to_string());
    }
    check_window(transfer, leg.window_micros)?;
    let keys = context_keys(chosen)?;
    if let Some(key) = keys.iter().find(|k| !CC_CONTEXT_KEYS.contains(*k)) {
        return Err(format!("the transfer context names {key}"));
    }
    let featured = keys.contains(&"featured-app-right");
    completed(ex)?;

    let sends: Vec<&Exercise> = child_exercises(ex, nodes)
        .filter(|e| matches!(e.choice_id.as_str(), "TransferPreapproval_SendV2" | "TransferPreapproval_Send"))
        .collect();
    let [send] = sends.as_slice() else {
        return Err(format!(
            "expected 1 TransferPreapproval_SendV2 under the transfer, got {}",
            sends.len()
        ));
    };
    let legacy = send.choice_id == "TransferPreapproval_Send";
    check_preapproval_send(send, nodes, metadata, leg, receiver, &inputs, dso, legacy)?;

    let (mut paid, mut change, mut marker) = (false, false, false);
    for (_, node) in descendants(&ex.children, nodes)? {
        match node {
            NodeType::Exercise(e) => {
                let allowed = match e.choice_id.as_str() {
                    "TransferPreapproval_SendV2" | "TransferPreapproval_Send" => std::ptr::eq(e, *send),
                    "AmuletRules_Transfer" => legacy,
                    "Archive" => {
                        e.consuming
                            && is_template(e.template_id.as_ref(), AMULET_MODULE, "Amulet")
                            && inputs.contains(&e.contract_id.as_str())
                    }
                    "EventLog_HoldingsChange" => !e.consuming,
                    _ => false,
                };
                if !allowed {
                    return Err(format!("unexpected {} in the transfer", exercise_name(e)));
                }
            }
            NodeType::Create(c) if is_template(c.template_id.as_ref(), AMULET_MODULE, "Amulet") => {
                let (owner, initial) = amulet_terms(c, dso)?;
                if owner == receiver && !paid && numeric_eq(initial, leg.amount) {
                    paid = true;
                } else if owner == leg.sender && !change {
                    change = true;
                } else {
                    return Err(format!("unexpected Amulet of {initial} for {owner}"));
                }
            }
            NodeType::Create(c) if is_template(c.template_id.as_ref(), AMULET_MODULE, "FeaturedAppActivityMarker") => {
                if !featured || marker {
                    return Err("a FeaturedAppActivityMarker without a featured context".to_string());
                }
                let marker_dso = party_at(create_record(c)?, "dso")?;
                if marker_dso != dso {
                    return Err(format!("the activity marker's dso {marker_dso} is not the DSO {dso}"));
                }
                marker = true;
            }
            NodeType::Create(c)
                if legacy
                    && (is_template(c.template_id.as_ref(), AMULET_MODULE, "AppRewardCoupon")
                        || is_template(c.template_id.as_ref(), AMULET_MODULE, "ValidatorRewardCoupon")) => {}
            NodeType::Create(c) => {
                return Err(format!("unexpected Create of {} in the transfer", template_name(c.template_id.as_ref())));
            }
            NodeType::Fetch(_) | NodeType::Rollback(_) => {}
        }
    }
    if !paid {
        return Err(format!("the transfer creates no Amulet of {} for {receiver}", leg.amount));
    }
    Ok(CcTransferSeen { receiver: receiver.to_string() })
}

/// The `TransferPreapproval_SendV2` (or the older `TransferPreapproval_Send`) that
/// completes a CC transfer on the receiver's preapproval.
#[allow(clippy::too_many_arguments)]
fn check_preapproval_send(
    send: &Exercise,
    nodes: &NodesDict<'_>,
    metadata: &Metadata,
    leg: &CcLeg<'_>,
    receiver: &str,
    inputs: &[&str],
    dso: &str,
    legacy: bool,
) -> Result<(), String> {
    if !is_template(send.template_id.as_ref(), AMULET_RULES_MODULE, "TransferPreapproval") {
        return Err(format!("the preapproval send is {}", exercise_name(send)));
    }
    only_acting(send, leg.sender)?;
    let chosen = chosen_record(send)?;
    let sender = party_at(chosen, "sender")?;
    if sender != leg.sender {
        return Err(format!("the preapproval send's sender {sender} is not {}", leg.sender));
    }
    let amount = numeric_at(chosen, "amount")?;
    if !numeric_eq(amount, leg.amount) {
        return Err(format!("the preapproval send's amount {amount} is not {}", leg.amount));
    }
    let mut spent = Vec::new();
    for input in list_at(chosen, "inputs")? {
        match variant_of(input) {
            Some(("InputAmulet", Some(v))) => {
                spent.push(get_contract_id(v).ok_or("a preapproval send input has no contract id")?);
            }
            _ => return Err("a preapproval send input is not an Amulet".to_string()),
        }
    }
    if !same_set(&spent, inputs) {
        return Err("the preapproval send spends other coins than the transfer".to_string());
    }
    let preapproval = input_contract(metadata, &send.contract_id)
        .ok_or("the receiver's preapproval is not among the input contracts")?;
    if !is_template(preapproval.template_id.as_ref(), AMULET_RULES_MODULE, "TransferPreapproval") {
        return Err("the preapproval send is not on a TransferPreapproval".to_string());
    }
    let terms = create_record(preapproval)?;
    let owner = party_at(terms, "receiver")?;
    if owner != receiver {
        return Err(format!("the preapproval is {owner}'s, not {receiver}'s"));
    }
    let preapproval_dso = party_at(terms, "dso")?;
    if preapproval_dso != dso {
        return Err(format!("the preapproval's dso {preapproval_dso} is not the DSO {dso}"));
    }
    if legacy {
        let rules = child_exercises(send, nodes).filter(|e| e.choice_id == "AmuletRules_Transfer").count();
        if rules != 1 {
            return Err(format!("expected 1 AmuletRules_Transfer under TransferPreapproval_Send, got {rules}"));
        }
    }
    Ok(())
}

/// An Amulet create's owner and initial amount, refused when another DSO's.
fn amulet_terms<'a>(c: &'a Create, dso: &str) -> Result<(&'a str, &'a str), String> {
    let terms = create_record(c)?;
    let amulet_dso = party_at(terms, "dso")?;
    if amulet_dso != dso {
        return Err(format!("an Amulet's dso {amulet_dso} is not the DSO {dso}"));
    }
    let initial = numeric_at(record_at(terms, "amount")?, "initialAmount")?;
    Ok((party_at(terms, "owner")?, initial))
}

/// The DSO: the pinned party, or else the one party every named admin and the
/// dso of every input coin agree on. Every input coin must be an Amulet of `owner`.
pub(super) fn resolve_dso(
    pinned: Option<&str>,
    admins: &[(&str, &str)],
    metadata: &Metadata,
    coins: &[&str],
    owner: &str,
) -> Result<String, String> {
    let mut named: Vec<(String, String)> = admins.iter().map(|(what, p)| (what.to_string(), p.to_string())).collect();
    for coin in coins {
        let c = input_contract(metadata, coin)
            .ok_or_else(|| format!("input coin {} is not among the input contracts", short(coin, 16)))?;
        if !is_template(c.template_id.as_ref(), AMULET_MODULE, "Amulet") {
            return Err(format!("input coin {} is not an Amulet", short(coin, 16)));
        }
        let terms = create_record(c)?;
        let coin_owner = party_at(terms, "owner")?;
        if coin_owner != owner {
            return Err(format!("input coin {} is {coin_owner}'s", short(coin, 16)));
        }
        named.push((format!("the dso of input coin {}", short(coin, 16)), party_at(terms, "dso")?.to_string()));
    }
    let dso = match pinned {
        Some(p) => p.to_string(),
        None => named.first().map(|(_, p)| p.clone()).ok_or("no DSO is named")?,
    };
    if let Some((what, p)) = named.iter().find(|(_, p)| *p != dso) {
        return Err(format!("{what} {p} is not the DSO {dso}"));
    }
    Ok(dso)
}

/// `executeBefore` after `requestedAt`, by at most `max_micros`.
pub(super) fn check_window(r: &Record, max_micros: i64) -> Result<(), String> {
    let requested = timestamp_at(r, "requestedAt")?;
    let before = timestamp_at(r, "executeBefore")?;
    match before.checked_sub(requested) {
        Some(w) if w > 0 && w <= max_micros => Ok(()),
        Some(w) if w > 0 => Err(format!(
            "the execution window of {} s is longer than {} s",
            w / 1_000_000,
            max_micros / 1_000_000
        )),
        _ => Err("executeBefore is not after requestedAt".to_string()),
    }
}

/// A transfer instruction result that completed.
pub(super) fn completed(ex: &Exercise) -> Result<(), String> {
    let result = ex
        .exercise_result
        .as_ref()
        .and_then(as_record)
        .ok_or("the transfer has no result record")?;
    match variant_of(field(result, "output")?) {
        Some(("TransferInstructionResult_Completed", _)) => Ok(()),
        Some((other, _)) => Err(format!("the transfer result is {other}, not Completed")),
        None => Err("the transfer result output is not a variant".to_string()),
    }
}

/// The keys of `extraArgs.context.values`.
fn context_keys(chosen: &Record) -> Result<Vec<&str>, String> {
    let context = record_at(record_at(chosen, "extraArgs")?, "context")?;
    match field(context, "values")?.sum.as_ref() {
        Some(Sum::TextMap(m)) => Ok(m.entries.iter().map(|e| e.key.as_str()).collect()),
        _ => Err("the transfer context is not a text map".to_string()),
    }
}

// ============================================================================
// SplitCc (and the CC merge)
// ============================================================================

/// One `AmuletRules_Transfer` from the party to itself; the number of input coins.
fn check_split_cc(
    prepared: &PreparedTransaction,
    prepared_transaction_bytes: &[u8],
    party: &str,
    output_amounts: &[String],
    amulet_cids: &[String],
    pinned_dso: Option<&str>,
) -> Result<usize, String> {
    let (tx, metadata, submitter) = tx_parts(prepared)?;
    only_submitter(submitter, party)?;
    let nodes = build_nodes_dict(tx);
    let root = single_root_exercise(tx, &nodes)?;
    if root.choice_id != "AmuletRules_Transfer"
        || !is_template(root.template_id.as_ref(), AMULET_RULES_MODULE, "AmuletRules")
    {
        return Err(format!("the split is {}", exercise_name(root)));
    }
    only_acting(root, party)?;
    let chosen = chosen_record(root)?;
    let transfer = record_at(chosen, "transfer")?;
    for role in ["sender", "provider"] {
        let p = party_at(transfer, role)?;
        if p != party {
            return Err(format!("the split {role} {p} is not {party}"));
        }
    }
    let mut inputs = Vec::new();
    for input in list_at(transfer, "inputs")? {
        match variant_of(input) {
            Some(("InputAmulet", Some(v))) => {
                inputs.push(get_contract_id(v).ok_or("a split input has no contract id")?);
            }
            _ => return Err("a split input is not an Amulet".to_string()),
        }
    }
    if inputs.is_empty() {
        return Err("the split names no input coins".to_string());
    }
    let requested: Vec<&str> = amulet_cids.iter().map(String::as_str).collect();
    if !requested.is_empty() && !same_set(&inputs, &requested) {
        return Err("the split spends other coins than requested".to_string());
    }
    let outputs = list_at(transfer, "outputs")?;
    if outputs.len() != output_amounts.len() {
        return Err(format!(
            "the split has {} outputs, not {}",
            outputs.len(),
            output_amounts.len()
        ));
    }
    for (output, expected) in outputs.iter().zip(output_amounts) {
        let output = as_record(output).ok_or("a split output is not a record")?;
        let receiver = party_at(output, "receiver")?;
        if receiver != party {
            return Err(format!("a split output goes to {receiver}"));
        }
        let amount = numeric_at(output, "amount")?;
        if !numeric_eq(amount, expected) {
            return Err(format!("a split output of {amount} is not {expected}"));
        }
        if get_record_field(output, "lock").and_then(optional_of).flatten().is_some() {
            return Err("a split output is locked".to_string());
        }
    }
    let dso = resolve_dso(pinned_dso, &[], metadata, &inputs, party)?;
    if let Some(expected) = get_record_field(chosen, "expectedDso").and_then(optional_of).flatten() {
        let named = get_party(expected).ok_or("expectedDso is not a party")?;
        if named != dso {
            return Err(format!("expectedDso {named} is not the DSO {dso}"));
        }
    }

    let mut created = Vec::new();
    for (_, node) in descendants(&root.children, &nodes)? {
        match node {
            NodeType::Exercise(e) => {
                let allowed = match e.choice_id.as_str() {
                    "Archive" => {
                        e.consuming
                            && is_template(e.template_id.as_ref(), AMULET_MODULE, "Amulet")
                            && inputs.contains(&e.contract_id.as_str())
                    }
                    "EventLog_HoldingsChange" => !e.consuming,
                    _ => false,
                };
                if !allowed {
                    return Err(format!("unexpected {} in the split", exercise_name(e)));
                }
            }
            NodeType::Create(c) if is_template(c.template_id.as_ref(), AMULET_MODULE, "Amulet") => {
                let (owner, initial) = amulet_terms(c, &dso)?;
                if owner != party {
                    return Err(format!("the split creates an Amulet for {owner}"));
                }
                created.push(initial);
            }
            NodeType::Create(c) => {
                return Err(format!("unexpected Create of {} in the split", template_name(c.template_id.as_ref())));
            }
            NodeType::Fetch(_) | NodeType::Rollback(_) => {}
        }
    }
    for expected in output_amounts {
        let at = created
            .iter()
            .position(|a| numeric_eq(a, expected))
            .ok_or_else(|| format!("the split creates no coin of {expected}"))?;
        created.swap_remove(at);
    }
    if created.len() > 1 {
        return Err(format!("the split creates {} coins beyond its outputs", created.len()));
    }
    let facts = crate::facts::extract_facts(prepared_transaction_bytes, party).map_err(|e| e.to_string())?;
    if let Some(flow) = facts.outflows.first() {
        return Err(format!("{} {} leaves the party for {}", flow.amount, flow.instrument_id, flow.to));
    }
    if let Some(c) = facts.consumed.iter().find(|c| !inputs.contains(&c.contract_id.as_str())) {
        return Err(format!("the split consumes coin {} it does not name", short(&c.contract_id, 16)));
    }
    Ok(inputs.len())
}

// ============================================================================
// Shared helpers (CC, CIP-56 and multicall checks)
// ============================================================================

/// The transaction, metadata and submitter of a prepared transaction.
pub(super) fn tx_parts(prepared: &PreparedTransaction) -> Result<(&DamlTransaction, &Metadata, &SubmitterInfo), String> {
    let tx = prepared.transaction.as_ref().ok_or("the prepared transaction has no transaction")?;
    let metadata = prepared.metadata.as_ref().ok_or("the prepared transaction has no metadata")?;
    let submitter = metadata.submitter_info.as_ref().ok_or("the metadata has no submitter")?;
    Ok((tx, metadata, submitter))
}

/// `act_as` is exactly `party`.
pub(super) fn only_submitter(submitter: &SubmitterInfo, party: &str) -> Result<(), String> {
    match submitter.act_as.as_slice() {
        [only] if only == party => Ok(()),
        other => Err(format!("act_as is {other:?}, not only {party}")),
    }
}

/// The acting parties of `ex` are exactly `party`.
pub(super) fn only_acting(ex: &Exercise, party: &str) -> Result<(), String> {
    match ex.acting_parties.as_slice() {
        [only] if only == party => Ok(()),
        other => Err(format!("{} is exercised by {other:?}, not only {party}", ex.choice_id)),
    }
}

/// The single root node, an exercise.
pub(super) fn single_root_exercise<'a>(tx: &'a DamlTransaction, nodes: &NodesDict<'a>) -> Result<&'a Exercise, String> {
    let [root] = tx.roots.as_slice() else {
        return Err(format!("expected 1 root node, got {}", tx.roots.len()));
    };
    match nodes.get(root).copied().and_then(get_node_type) {
        Some(NodeType::Exercise(ex)) => Ok(ex),
        Some(_) => Err("the root node is not an exercise".to_string()),
        None => Err("the root node is missing".to_string()),
    }
}

/// The exercises among the direct children of `ex`.
pub(super) fn child_exercises<'a>(ex: &'a Exercise, nodes: &'a NodesDict<'a>) -> impl Iterator<Item = &'a Exercise> + 'a {
    ex.children.iter().filter_map(|id| match nodes.get(id).copied().and_then(get_node_type) {
        Some(NodeType::Exercise(e)) => Some(e),
        _ => None,
    })
}

/// Every node below `children`, depth first in tree order. A missing, repeated or
/// rolled-back node is refused.
pub(super) fn descendants<'a>(children: &[String], nodes: &NodesDict<'a>) -> Result<Vec<(&'a str, &'a NodeType)>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<String> = children.iter().rev().cloned().collect();
    while let Some(id) = stack.pop() {
        let node = nodes.get(&id).copied().ok_or_else(|| format!("node {id} is missing"))?;
        if !seen.insert(node.node_id.as_str()) {
            return Err(format!("node {id} appears twice"));
        }
        let kind = get_node_type(node).ok_or_else(|| format!("node {id} has no type"))?;
        match kind {
            NodeType::Exercise(e) => stack.extend(e.children.iter().rev().cloned()),
            NodeType::Rollback(_) => return Err(format!("node {id} is a rollback")),
            _ => {}
        }
        out.push((node.node_id.as_str(), kind));
    }
    Ok(out)
}

/// The input contract `cid` of the metadata.
pub(super) fn input_contract<'a>(metadata: &'a Metadata, cid: &str) -> Option<&'a Create> {
    metadata.input_contracts.iter().find_map(|ic| match ic.contract.as_ref() {
        Some(InputContractOneof::V1(c)) if c.contract_id == cid => Some(c),
        _ => None,
    })
}

/// Whether `id` names `module:entity`, whatever the package.
pub(super) fn is_template(id: Option<&Identifier>, module: &str, entity: &str) -> bool {
    id.is_some_and(|t| template_matches(t, module, entity))
}

pub(super) fn template_name(id: Option<&Identifier>) -> String {
    id.map_or_else(|| "unknown".to_string(), |t| format!("{}.{}", t.module_name, t.entity_name))
}

pub(super) fn exercise_name(e: &Exercise) -> String {
    format!("{} on {}", e.choice_id, template_name(e.template_id.as_ref()))
}

pub(super) fn chosen_record(e: &Exercise) -> Result<&Record, String> {
    e.chosen_value
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| format!("{} has no argument record", e.choice_id))
}

pub(super) fn create_record(c: &Create) -> Result<&Record, String> {
    c.argument
        .as_ref()
        .and_then(as_record)
        .ok_or_else(|| format!("a Create of {} has no argument record", template_name(c.template_id.as_ref())))
}

pub(super) fn field<'a>(r: &'a Record, label: &str) -> Result<&'a Value, String> {
    get_record_field(r, label).ok_or_else(|| format!("{label} is missing"))
}

pub(super) fn record_at<'a>(r: &'a Record, label: &str) -> Result<&'a Record, String> {
    as_record(field(r, label)?).ok_or_else(|| format!("{label} is not a record"))
}

pub(super) fn party_at<'a>(r: &'a Record, label: &str) -> Result<&'a str, String> {
    get_party(field(r, label)?).ok_or_else(|| format!("{label} is not a party"))
}

pub(super) fn numeric_at<'a>(r: &'a Record, label: &str) -> Result<&'a str, String> {
    get_numeric(field(r, label)?).ok_or_else(|| format!("{label} is not a number"))
}

pub(super) fn text_at<'a>(r: &'a Record, label: &str) -> Result<&'a str, String> {
    get_text(field(r, label)?).ok_or_else(|| format!("{label} is not text"))
}

pub(super) fn list_at<'a>(r: &'a Record, label: &str) -> Result<&'a [Value], String> {
    as_list(field(r, label)?).ok_or_else(|| format!("{label} is not a list"))
}

pub(super) fn timestamp_at(r: &Record, label: &str) -> Result<i64, String> {
    match field(r, label)?.sum.as_ref() {
        Some(Sum::Timestamp(t)) => Ok(*t),
        _ => Err(format!("{label} is not a time")),
    }
}

pub(super) fn cids_at<'a>(r: &'a Record, label: &str) -> Result<Vec<&'a str>, String> {
    list_at(r, label)?
        .iter()
        .map(|v| get_contract_id(v).ok_or_else(|| format!("{label} holds a value that is not a contract id")))
        .collect()
}

/// A variant's constructor and value.
pub(super) fn variant_of(v: &Value) -> Option<(&str, Option<&Value>)> {
    match v.sum.as_ref()? {
        Sum::Variant(x) => Some((x.constructor.as_str(), x.value.as_deref())),
        _ => None,
    }
}

/// An optional's value; `None` when `v` is not an optional.
pub(super) fn optional_of(v: &Value) -> Option<Option<&Value>> {
    match v.sum.as_ref()? {
        Sum::Optional(o) => Some(o.value.as_deref()),
        _ => None,
    }
}

/// The same contract ids, whatever the order.
pub(super) fn same_set(a: &[&str], b: &[&str]) -> bool {
    a.len() == b.len() && a.iter().collect::<HashSet<_>>() == b.iter().collect::<HashSet<_>>()
}

/// The same value, ignoring type ids and numeric scale.
pub(super) fn same_value(a: &Value, b: &Value) -> bool {
    fn both(a: Option<&Value>, b: Option<&Value>) -> bool {
        match (a, b) {
            (Some(x), Some(y)) => same_value(x, y),
            (None, None) => true,
            _ => false,
        }
    }
    match (a.sum.as_ref(), b.sum.as_ref()) {
        (Some(Sum::Record(x)), Some(Sum::Record(y))) => {
            x.fields.len() == y.fields.len()
                && x.fields.iter().zip(&y.fields).all(|(f, g)| f.label == g.label && both(f.value.as_ref(), g.value.as_ref()))
        }
        (Some(Sum::Variant(x)), Some(Sum::Variant(y))) => {
            x.constructor == y.constructor && both(x.value.as_deref(), y.value.as_deref())
        }
        (Some(Sum::Optional(x)), Some(Sum::Optional(y))) => both(x.value.as_deref(), y.value.as_deref()),
        (Some(Sum::List(x)), Some(Sum::List(y))) => {
            x.elements.len() == y.elements.len() && x.elements.iter().zip(&y.elements).all(|(u, v)| same_value(u, v))
        }
        (Some(Sum::TextMap(x)), Some(Sum::TextMap(y))) => {
            x.entries.len() == y.entries.len()
                && x.entries.iter().zip(&y.entries).all(|(u, v)| u.key == v.key && both(u.value.as_ref(), v.value.as_ref()))
        }
        (Some(Sum::GenMap(x)), Some(Sum::GenMap(y))) => {
            x.entries.len() == y.entries.len()
                && x.entries
                    .iter()
                    .zip(&y.entries)
                    .all(|(u, v)| both(u.key.as_ref(), v.key.as_ref()) && both(u.value.as_ref(), v.value.as_ref()))
        }
        (Some(Sum::Numeric(x)), Some(Sum::Numeric(y))) => numeric_eq(x, y),
        (Some(Sum::Enum(x)), Some(Sum::Enum(y))) => x.constructor == y.constructor,
        (x, y) => x == y,
    }
}

// ============================================================================
// TransferCc inspection (Phase B)
// ============================================================================

fn check_transfer_cc(
    prepared: &PreparedTransaction,
    sender_party: &str,
    receiver_party: &str,
    expected_amount: &str,
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

    // template_id must be present (the template itself is checked by the shared CC check)
    if root_exercise.template_id.is_none() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::tests::{root_exercise, run, single_exercise, P, Q};
    use crate::test_support::*;

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
                dso_party: None,
            },
        );
        assert!(r.accepted, "{r:?}");
        assert!(r.summary.contains("€€€€€€€€ → ₿₿₿₿₿₿₿₿"), "{}", r.summary);
    }

    #[test]
    fn transfer_cc_without_roots_is_refused() {
        let empty = prepared(&[], vec![], vec![], &[P]);
        let r = run(
            &empty,
            OperationExpectation::TransferCc {
                sender_party: P.into(),
                receiver_party: Q.into(),
                amount: "1".into(),
                command_id: "cmd".into(),
                dso_party: None,
            },
        );
        assert!(!r.accepted);
        assert_eq!(r.rejection_reason.as_deref(), Some("Transaction has no root nodes"));
    }
}
