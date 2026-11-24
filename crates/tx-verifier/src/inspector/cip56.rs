//! CIP-56 inspection: token transfers and transfer preapprovals.

use super::cc::{
    check_cc_transfer_node, child_exercises, chosen_record, cids_at, completed, create_record, descendants,
    exercise_name, field, input_contract, is_template, numeric_at, only_acting, only_submitter, optional_of,
    party_at, record_at, resolve_dso, same_set, same_value, single_root_exercise, template_name, text_at,
    tx_parts, CcLeg,
};
use super::*;

const PREAPPROVAL_MODULE: &str = "Utility.Registry.App.V0.Model.TransferPreapproval";
const FACTORY_MODULE: &str = "Utility.Registry.App.V0.Service.AllocationFactory";
const TRANSFER_RULE_MODULE: &str = "Utility.Registry.V0.Rule.Transfer";
const HOLDING_MODULE: &str = "Utility.Registry.Holding.V0.Holding";
const CREDENTIAL_MODULE: &str = "Utility.Credential.V0.Credential";
const RECEIPT_MODULE: &str = "Utility.Registry.V0.Holding.Transfer";
const FEATURED_APP_RIGHT_MODULE: &str = "Splice.Api.FeaturedAppRightV1";

/// The longest `executeBefore` window of an Amulet transfer on the CIP-56 path: the
/// builder's 7 days, and a minute.
const AMULET_WINDOW_MICROS: i64 = (7 * 86_400 + 60) * 1_000_000;

/// RequestPreapproval: the agent asks for a transfer preapproval.
pub(super) fn inspect_request_preapproval(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::RequestPreapproval { party, operator, instrument_admin } = expectation else {
        return Err(wrong_expectation("RequestPreapproval"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    check_request_preapproval(&prepared, party, operator.as_deref(), instrument_admin.as_deref(), observed)
}

/// TransferCip56: a registry token transfer, or a self-transfer that merges holdings.
pub(super) fn inspect_transfer_cip56(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::TransferCip56 {
        sender_party,
        receiver_party,
        instrument_id,
        instrument_admin,
        amount,
        command_id,
        input_holding_cids,
    } = expectation
    else {
        return Err(wrong_expectation("TransferCip56"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let expected = Cip56Transfer {
        sender: sender_party,
        receiver: receiver_party,
        instrument_id,
        instrument_admin,
        amount,
        command_id: command_id.as_deref(),
        inputs: input_holding_cids.iter().map(String::as_str).collect(),
    };
    let checked = if expected.instrument_id == "Amulet" {
        check_amulet_cip56(&prepared, &expected)
    } else {
        check_transfer_cip56(&prepared, &expected)
    };
    let summary = match checked {
        Ok(()) => format!(
            "TransferCip56 VERIFIED: {} {} {} → {}",
            amount,
            instrument_id,
            short(sender_party, 8),
            short(receiver_party, 8)
        ),
        Err(reason) => {
            let summary = format!("TransferCip56 check failed: {reason}");
            observed.push(reason);
            summary
        }
    };
    debug!("TX INSPECT: {}", summary);
    Ok(InspectionResult { accepted: true, summary, warnings: vec![], rejection_reason: None })
}

// ============================================================================
// TransferCip56 inspection
// ============================================================================

struct Cip56Transfer<'a> {
    sender: &'a str,
    receiver: &'a str,
    instrument_id: &'a str,
    instrument_admin: &'a str,
    amount: &'a str,
    command_id: Option<&'a str>,
    /// Empty when the server selects the inputs.
    inputs: Vec<&'a str>,
}

/// The submission's command id is the requested one, when one is requested.
fn command_id_is(actual: &str, requested: Option<&str>) -> Result<(), String> {
    match requested {
        Some(id) if actual != id => Err(format!("command id {actual} is not the requested {id}")),
        _ => Ok(()),
    }
}

/// An Amulet transfer on the CIP-56 path: the CC transfer check, with the instrument
/// admin as the DSO.
fn check_amulet_cip56(prepared: &PreparedTransaction, e: &Cip56Transfer<'_>) -> Result<(), String> {
    let (tx, metadata, submitter) = tx_parts(prepared)?;
    only_submitter(submitter, e.sender)?;
    command_id_is(&submitter.command_id, e.command_id)?;
    let nodes = build_nodes_dict(tx);
    let root = single_root_exercise(tx, &nodes)?;
    let chosen = chosen_record(root)?;
    let transfer = record_at(chosen, "transfer")?;
    let coins = cids_at(transfer, "inputHoldingCids")?;
    if !e.inputs.is_empty() && !same_set(&coins, &e.inputs) {
        return Err("the transfer spends other coins than requested".to_string());
    }
    let admins = [
        ("expectedAdmin", party_at(chosen, "expectedAdmin")?),
        ("the instrument admin", party_at(record_at(transfer, "instrumentId")?, "admin")?),
    ];
    let dso = resolve_dso(Some(e.instrument_admin), &admins, metadata, &coins, e.sender)?;
    let leg = CcLeg {
        sender: e.sender,
        receiver: Some(e.receiver),
        amount: e.amount,
        window_micros: AMULET_WINDOW_MICROS,
    };
    check_cc_transfer_node(root, &nodes, metadata, &leg, &dso).map(|_| ())
}

/// One `TransferFactory_Transfer` on the receiver's registry preapproval completed by a
/// direct transfer, or, for a self-transfer, on the registry's allocation factory.
fn check_transfer_cip56(prepared: &PreparedTransaction, e: &Cip56Transfer<'_>) -> Result<(), String> {
    let (tx, metadata, submitter) = tx_parts(prepared)?;
    only_submitter(submitter, e.sender)?;
    command_id_is(&submitter.command_id, e.command_id)?;
    let nodes = build_nodes_dict(tx);
    let root = single_root_exercise(tx, &nodes)?;
    if root.choice_id != "TransferFactory_Transfer" {
        return Err(format!("the transfer is {}", exercise_name(root)));
    }
    if !is_template(root.interface_id.as_ref(), "Splice.Api.Token.TransferInstructionV1", "TransferFactory") {
        return Err("the transfer is not exercised on the TransferFactory interface".to_string());
    }
    let to_self = e.receiver == e.sender;
    let factory = root.template_id.as_ref();
    // The allocation factory's provider, for a self-transfer on that factory.
    let factory_provider = if is_template(factory, PREAPPROVAL_MODULE, "TransferPreapproval") {
        let preapproval = input_contract(metadata, &root.contract_id)
            .ok_or("the receiver's preapproval is not among the input contracts")?;
        let terms = create_record(preapproval)?;
        let owner = party_at(terms, "receiver")?;
        if owner != e.receiver {
            return Err(format!("the preapproval is {owner}'s, not {}'s", e.receiver));
        }
        let admin = party_at(terms, "instrumentAdmin")?;
        if admin != e.instrument_admin {
            return Err(format!("the preapproval's admin {admin} is not {}", e.instrument_admin));
        }
        None
    } else if to_self && is_template(factory, FACTORY_MODULE, "AllocationFactory") {
        let contract = input_contract(metadata, &root.contract_id)
            .filter(|c| is_template(c.template_id.as_ref(), FACTORY_MODULE, "AllocationFactory"))
            .ok_or("the allocation factory is not among the input contracts")?;
        let terms = create_record(contract)?;
        let registrar = party_at(terms, "registrar")?;
        if registrar != e.instrument_admin {
            return Err(format!("the factory's registrar {registrar} is not {}", e.instrument_admin));
        }
        Some(party_at(terms, "provider")?)
    } else {
        return Err(format!("the transfer factory is {}", template_name(factory)));
    };
    only_acting(root, e.sender)?;
    let chosen = chosen_record(root)?;
    let expected_admin = party_at(chosen, "expectedAdmin")?;
    if expected_admin != e.instrument_admin {
        return Err(format!("expectedAdmin {expected_admin} is not {}", e.instrument_admin));
    }
    let transfer_value = field(chosen, "transfer")?;
    let transfer = record_at(chosen, "transfer")?;
    for (role, want) in [("sender", e.sender), ("receiver", e.receiver)] {
        let p = party_at(transfer, role)?;
        if p != want {
            return Err(format!("the transfer {role} {p} is not {want}"));
        }
    }
    let amount = numeric_at(transfer, "amount")?;
    if !numeric_eq(amount, e.amount) {
        return Err(format!("the transfer amount {amount} is not {}", e.amount));
    }
    let instrument = record_at(transfer, "instrumentId")?;
    let admin = party_at(instrument, "admin")?;
    let id = text_at(instrument, "id")?;
    if admin != e.instrument_admin || id != e.instrument_id {
        return Err(format!("the instrument {admin}:{id} is not {}:{}", e.instrument_admin, e.instrument_id));
    }
    let inputs = cids_at(transfer, "inputHoldingCids")?;
    if inputs.is_empty() {
        return Err("the transfer names no input holdings".to_string());
    }
    if !e.inputs.is_empty() && !same_set(&inputs, &e.inputs) {
        return Err("the transfer spends other holdings than requested".to_string());
    }
    completed(root)?;

    let (direct, internal) = match factory_provider {
        None => (Some(direct_transfer(root, &nodes, e, transfer_value)?), None),
        Some(provider) => (None, internal_transfer(root, &nodes, e, provider)?),
    };

    let (mut paid, mut change, mut receipt) = (None, None, false);
    // The children of the activity marker exercise, once seen.
    let mut marked: Option<&[String]> = None;
    for (id, node) in descendants(&root.children, &nodes)? {
        match node {
            NodeType::Exercise(x) => {
                let allowed = match x.choice_id.as_str() {
                    "TransferRule_DirectTransfer" => direct.is_some_and(|d| std::ptr::eq(x, d)),
                    "AllocationFactory_TransferInternal" => internal.is_some_and(|i| std::ptr::eq(x, i)),
                    "Archive" => {
                        x.consuming
                            && is_template(x.template_id.as_ref(), HOLDING_MODULE, "Holding")
                            && inputs.contains(&x.contract_id.as_str())
                    }
                    "EventLog_HoldingsChange" => !x.consuming,
                    "Credential_Get" => {
                        !x.consuming
                            && x.children.is_empty()
                            && is_template(x.template_id.as_ref(), CREDENTIAL_MODULE, "Credential")
                    }
                    "FeaturedAppRight_CreateActivityMarker" => {
                        let creates_only = x.children.iter().all(|c| {
                            matches!(nodes.get(c).copied().and_then(get_node_type), Some(NodeType::Create(_)))
                        });
                        let ok = marked.is_none()
                            && !x.consuming
                            && creates_only
                            && is_template(x.interface_id.as_ref(), FEATURED_APP_RIGHT_MODULE, "FeaturedAppRight");
                        if ok {
                            marked = Some(x.children.as_slice());
                        }
                        ok
                    }
                    _ => false,
                };
                if !allowed {
                    return Err(format!("unexpected {} in the transfer", exercise_name(x)));
                }
            }
            NodeType::Create(c) if marked.is_some_and(|m| m.iter().any(|m| m == id)) => {
                if !is_template(c.template_id.as_ref(), "Splice.Amulet", "FeaturedAppActivityMarker") {
                    return Err(format!(
                        "unexpected Create of {} under the activity marker",
                        template_name(c.template_id.as_ref())
                    ));
                }
            }
            NodeType::Create(c) if is_template(c.template_id.as_ref(), HOLDING_MODULE, "Holding") => {
                let (owner, held) = holding_terms(c, e)?;
                if owner == e.receiver && paid.is_none() && numeric_eq(held, e.amount) {
                    paid = Some(held);
                } else if owner == e.sender && change.is_none() {
                    change = Some(held);
                } else {
                    return Err(format!("unexpected holding of {held} for {owner}"));
                }
            }
            NodeType::Create(c) if !receipt && is_template(c.template_id.as_ref(), RECEIPT_MODULE, "ExecutedTransfer") => {
                check_receipt(c, e)?;
                receipt = true;
            }
            NodeType::Create(c) => {
                return Err(format!("unexpected Create of {} in the transfer", template_name(c.template_id.as_ref())));
            }
            NodeType::Fetch(_) | NodeType::Rollback(_) => {}
        }
    }
    let Some(paid) = paid else {
        return Err(format!("the transfer creates no holding of {} for {}", e.amount, e.receiver));
    };
    let total = inputs_total(metadata, &inputs);
    if change.is_some() && total.is_some_and(|total| numeric_eq(&total.to_string(), e.amount)) {
        return Err("the transfer spends its whole input yet creates change".to_string());
    }
    if let Some(total) = total.filter(|_| inputs_of_instrument(metadata, &inputs, e)) {
        let created = [Some(paid), change]
            .into_iter()
            .flatten()
            .try_fold(rust_decimal::Decimal::ZERO, |sum, a| sum.checked_add(crate::decimal::parse_exact(a)?));
        if created != Some(total) {
            return Err(format!(
                "the inputs total {total}, but the transfer creates {paid} and change {}",
                change.unwrap_or("0")
            ));
        }
    }
    Ok(())
}

/// The one direct transfer under a preapproval transfer, of the same transfer record.
fn direct_transfer<'a>(
    root: &'a Exercise,
    nodes: &'a NodesDict<'a>,
    e: &Cip56Transfer<'_>,
    transfer: &Value,
) -> Result<&'a Exercise, String> {
    let directs: Vec<&Exercise> = child_exercises(root, nodes)
        .filter(|x| x.choice_id == "TransferRule_DirectTransfer")
        .collect();
    let [direct] = directs.as_slice() else {
        return Err(format!(
            "expected 1 TransferRule_DirectTransfer under the transfer, got {}",
            directs.len()
        ));
    };
    if !is_template(direct.template_id.as_ref(), TRANSFER_RULE_MODULE, "TransferRule") {
        return Err(format!("the direct transfer is {}", exercise_name(direct)));
    }
    let acting = &direct.acting_parties;
    if !acting.iter().any(|p| p == e.sender) || acting.iter().any(|p| p != e.sender && p != e.receiver) {
        return Err(format!("the direct transfer is exercised by {acting:?}"));
    }
    if !same_value(transfer, field(chosen_record(direct)?, "transfer")?) {
        return Err("the direct transfer's record differs from the transfer".to_string());
    }
    Ok(direct)
}

/// The allocation factory's internal transfer under the root, if any: the same contract
/// and payload, exercised by the sender with the factory's provider and registrar.
fn internal_transfer<'a>(
    root: &'a Exercise,
    nodes: &'a NodesDict<'a>,
    e: &Cip56Transfer<'_>,
    provider: &str,
) -> Result<Option<&'a Exercise>, String> {
    let internals: Vec<&Exercise> = child_exercises(root, nodes)
        .filter(|x| x.choice_id == "AllocationFactory_TransferInternal")
        .collect();
    let internal = match internals.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        more => return Err(format!("expected at most 1 internal transfer under the transfer, got {}", more.len())),
    };
    if internal.consuming
        || internal.contract_id != root.contract_id
        || !is_template(internal.template_id.as_ref(), FACTORY_MODULE, "AllocationFactory")
    {
        return Err(format!("the internal transfer is {}", exercise_name(internal)));
    }
    let acting = &internal.acting_parties;
    if !acting.iter().any(|p| p == e.sender)
        || acting.iter().any(|p| p != e.sender && p != e.instrument_admin && p != provider)
    {
        return Err(format!("the internal transfer is exercised by {acting:?}"));
    }
    let payload = root.chosen_value.as_ref().ok_or("the transfer has no argument")?;
    if !same_value(payload, field(chosen_record(internal)?, "payload")?) {
        return Err("the internal transfer's payload differs from the transfer".to_string());
    }
    Ok(Some(internal))
}

/// A holding's registrar and instrument are the transfer's.
fn instrument_of(terms: &Record, e: &Cip56Transfer<'_>) -> Result<(), String> {
    let registrar = party_at(terms, "registrar")?;
    let instrument = record_at(terms, "instrument")?;
    let source = party_at(instrument, "source")?;
    let id = text_at(instrument, "id")?;
    if registrar != e.instrument_admin || source != e.instrument_admin || id != e.instrument_id {
        return Err(format!("a created holding is {source}:{id} of registrar {registrar}"));
    }
    Ok(())
}

/// A registry holding create's owner and amount, refused when it is another
/// instrument's or locked.
fn holding_terms<'a>(c: &'a Create, e: &Cip56Transfer<'_>) -> Result<(&'a str, &'a str), String> {
    let terms = create_record(c)?;
    instrument_of(terms, e)?;
    if get_record_field(terms, "lock").and_then(optional_of).flatten().is_some() {
        return Err("a created holding is locked".to_string());
    }
    Ok((party_at(terms, "owner")?, numeric_at(terms, "amount")?))
}

/// A transfer receipt (older registry versions) records the checked transfer.
fn check_receipt(c: &Create, e: &Cip56Transfer<'_>) -> Result<(), String> {
    let transfer = record_at(create_record(c)?, "transfer")?;
    let instrument = record_at(transfer, "instrumentIdentifier")?;
    let same = party_at(transfer, "registrar")? == e.instrument_admin
        && party_at(transfer, "sender")? == e.sender
        && party_at(transfer, "receiver")? == e.receiver
        && party_at(instrument, "source")? == e.instrument_admin
        && text_at(instrument, "id")? == e.instrument_id
        && numeric_eq(numeric_at(transfer, "amount")?, e.amount);
    if !same {
        return Err("the transfer receipt records another transfer".to_string());
    }
    Ok(())
}

/// The summed amount of the input holdings, when every one is listed.
fn inputs_total(metadata: &Metadata, inputs: &[&str]) -> Option<rust_decimal::Decimal> {
    inputs.iter().try_fold(rust_decimal::Decimal::ZERO, |sum, cid| {
        let terms = input_contract(metadata, cid).and_then(|c| create_record(c).ok())?;
        let amount = get_record_field(terms, "amount").and_then(get_numeric)?;
        sum.checked_add(crate::decimal::parse_exact(amount)?)
    })
}

/// Every input is a listed holding of the instrument.
fn inputs_of_instrument(metadata: &Metadata, inputs: &[&str], e: &Cip56Transfer<'_>) -> bool {
    inputs.iter().all(|cid| {
        input_contract(metadata, cid)
            .filter(|c| is_template(c.template_id.as_ref(), HOLDING_MODULE, "Holding"))
            .and_then(|c| create_record(c).ok())
            .is_some_and(|terms| instrument_of(terms, e).is_ok())
    })
}

// ============================================================================
// RequestPreapproval inspection (Phase B)
// ============================================================================

/// Verify a RequestPreapproval transaction.
///
/// Expected structure: single Create node for TransferPreapprovalProposal
/// with receiver = party (the requesting external party).
fn check_request_preapproval(
    prepared: &PreparedTransaction,
    party: &str,
    operator: Option<&str>,
    instrument_admin: Option<&str>,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let warnings = Vec::new();

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
        observed.push(format!("Expected 1 node, got {}", tx.nodes.len()));
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

    // --- The requested terms, exactly ---
    if let Err(reason) = only_submitter(submitter, party) {
        observed.push(reason);
    }
    if !matches!(create.signatories.as_slice(), [only] if only == party) {
        observed.push(format!("the preapproval is signed by {:?}, not only {party}", create.signatories));
    }
    // The CIP-56 preapproval names its operator and admin; the Splice proposal its
    // provider and expected DSO.
    let (operator_field, admin_field) = if is_cip56 { ("operator", "instrumentAdmin") } else { ("provider", "expectedDso") };
    if let Some(want) = operator {
        let named = get_record_field(record, operator_field).and_then(get_party);
        if named != Some(want) {
            observed.push(format!("the preapproval {operator_field} {} is not {want}", named.unwrap_or("<missing>")));
        }
    }
    if let Some(want) = instrument_admin {
        let named = get_record_field(record, admin_field).and_then(|v| match optional_of(v) {
            Some(inner) => inner.and_then(get_party),
            None => get_party(v),
        });
        if named != Some(want) {
            observed.push(format!("the preapproval {admin_field} {} is not {want}", named.unwrap_or("<missing>")));
        }
    }
    if is_cip56 {
        match get_record_field(record, "instrumentAllowances").and_then(as_list) {
            Some([]) => {}
            Some(allowances) => observed.push(format!(
                "the preapproval has {} instrument allowance(s); none were requested",
                allowances.len()
            )),
            None => observed.push("the preapproval has no instrument allowance list".to_string()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::tests::{run, P, Q};
    use crate::test_support::*;

    #[test]
    fn request_preapproval_root_count_rejections() {
        let c = || create(ident("M", "T"), record(vec![]), &[P]);
        for roots in [vec![], vec!["0", "1"]] {
            let p = prepared(&roots, vec![create_node("0", c()), create_node("1", c())], vec![], &[P]);
            let r = run(&p, OperationExpectation::RequestPreapproval { party: P.into(), operator: None, instrument_admin: None });
            assert!(!r.accepted);
            assert_eq!(
                r.rejection_reason,
                Some(format!("Expected 1 root node, got {}", roots.len()))
            );
        }
    }

    fn preapproval(signatories: &[&str]) -> PreparedTransaction {
        let c = create(
            ident(PREAPPROVAL_MODULE, "TransferPreapproval"),
            record(vec![
                ("operator", party(Q)),
                ("receiver", party(P)),
                ("instrumentAdmin", party(Q)),
                ("instrumentAllowances", list(vec![])),
            ]),
            signatories,
        );
        prepared(&["0"], vec![create_node("0", c)], vec![], &[P])
    }

    fn observed_of(p: &PreparedTransaction, operator: Option<&str>) -> (InspectionResult, Vec<String>) {
        let exp = OperationExpectation::RequestPreapproval {
            party: P.into(),
            operator: operator.map(str::to_string),
            instrument_admin: Some(Q.into()),
        };
        crate::inspector::inspect_observing(&encode(p), &exp, false).expect("inspection")
    }

    #[test]
    fn request_preapproval_checks_the_requested_terms() {
        let (r, observed) = observed_of(&preapproval(&[P]), Some(Q));
        assert!(r.accepted && observed.is_empty(), "{r:?} {observed:?}");
        let (r, observed) = observed_of(&preapproval(&[P]), Some(P));
        assert!(r.accepted, "observe first: {r:?}");
        assert_eq!(observed.len(), 1, "{observed:?}");
        let (_, observed) = observed_of(&preapproval(&[P, Q]), Some(Q));
        assert_eq!(observed.len(), 1, "an extra signatory: {observed:?}");
    }

    #[test]
    fn transfer_cip56_with_no_root_is_observed_not_refused() {
        let p = prepared(&[], vec![], vec![], &[P]);
        let exp = OperationExpectation::TransferCip56 {
            sender_party: P.into(),
            receiver_party: Q.into(),
            instrument_id: "X".into(),
            instrument_admin: Q.into(),
            amount: "1".into(),
            command_id: None,
            input_holding_cids: vec![],
        };
        let (r, observed) = crate::inspector::inspect_observing(&encode(&p), &exp, false).expect("inspection");
        assert!(r.accepted && !r.summary.starts_with("[STUB]"), "{r:?}");
        assert_eq!(observed, vec!["expected 1 root node, got 0".to_string()]);
    }
}
