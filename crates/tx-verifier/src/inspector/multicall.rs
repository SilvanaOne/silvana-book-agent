//! Multicall inspection: CC batch payments.

use super::cc::{
    check_cc_transfer_node, check_window, child_exercises, chosen_record, cids_at, exercise_name, is_template,
    list_at, only_acting, only_submitter, party_at, record_at, resolve_dso, same_set, single_root_exercise,
    text_at, tx_parts, variant_of, CcLeg,
};
use super::*;
use crate::facts::FlowKind;
use crate::types::BatchPayTarget;
use rust_decimal::Decimal;

/// The longest execution window of a batch-pay transfer, in microseconds.
const BATCH_PAY_WINDOW_MICROS: i64 = 310 * 1_000_000;

/// BatchPayCc: CC payments to several receivers in one `Execute_MultiCall`.
pub(super) fn inspect_batch_pay(
    prepared_transaction_bytes: &[u8],
    expectation: &OperationExpectation,
    observed: &mut Vec<String>,
) -> Result<InspectionResult> {
    let OperationExpectation::BatchPayCc { party, targets, holding_cids, dso_party } = expectation else {
        return Err(wrong_expectation("BatchPayCc"));
    };
    let prepared = decode_prepared(prepared_transaction_bytes)?;
    let summary = match check_batch_pay(
        &prepared,
        prepared_transaction_bytes,
        party,
        targets,
        holding_cids,
        dso_party.as_deref(),
    ) {
        Ok(receivers) => format!("BatchPayCc VERIFIED: {} pays {} receiver(s)", short(party, 8), receivers),
        Err(reason) => {
            let summary = format!("BatchPayCc check failed: {reason}");
            observed.push(reason);
            summary
        }
    };
    debug!("TX INSPECT: {}", summary);
    Ok(InspectionResult { accepted: true, summary, warnings: vec![], rejection_reason: None })
}

/// The targets summed per receiver, in first-seen order.
fn per_receiver(targets: &[BatchPayTarget]) -> Result<Vec<(String, Decimal)>, String> {
    let mut out: Vec<(String, Decimal)> = Vec::new();
    for t in targets {
        let amount = crate::decimal::parse_exact(&t.amount)
            .ok_or_else(|| format!("the target amount {} is not a decimal", t.amount))?;
        match out.iter_mut().find(|(r, _)| *r == t.receiver) {
            Some((_, sum)) => *sum = sum.checked_add(amount).ok_or("the target amounts overflow")?,
            None => out.push((t.receiver.clone(), amount)),
        }
    }
    Ok(out)
}

/// One `Execute_MultiCall` of the party's coins, one `Op_BatchTransfer` per receiver,
/// each paid by a direct CC transfer; the number of receivers.
fn check_batch_pay(
    prepared: &PreparedTransaction,
    prepared_transaction_bytes: &[u8],
    party: &str,
    targets: &[BatchPayTarget],
    holding_cids: &[String],
    pinned_dso: Option<&str>,
) -> Result<usize, String> {
    let expected = per_receiver(targets)?;
    if expected.is_empty() {
        return Err("the batch names no targets".to_string());
    }
    let (tx, metadata, submitter) = tx_parts(prepared)?;
    only_submitter(submitter, party)?;
    let nodes = build_nodes_dict(tx);
    let root = single_root_exercise(tx, &nodes)?;
    if root.choice_id != "Execute_MultiCall" || !is_template(root.template_id.as_ref(), "MultiCall", "MultiCall") {
        return Err(format!("the batch is {}", exercise_name(root)));
    }
    only_acting(root, party)?;
    let chosen = chosen_record(root)?;
    let sender = party_at(chosen, "sender")?;
    if sender != party {
        return Err(format!("the batch sender {sender} is not {party}"));
    }
    let coins = cids_at(chosen, "inputHoldings")?;
    let requested: Vec<&str> = holding_cids.iter().map(String::as_str).collect();
    if coins.is_empty() || !same_set(&coins, &requested) {
        return Err("the batch spends other coins than requested".to_string());
    }

    // The operations: one receiver each, in Amulet.
    let mut admins: Vec<(&str, &str)> = Vec::new();
    let mut paid: Vec<(&str, Decimal)> = Vec::new();
    let mut legs: Vec<(&str, &str)> = Vec::new();
    for op in list_at(chosen, "operations")? {
        let (constructor, inner) = variant_of(op).ok_or("a batch operation is not a variant")?;
        if constructor != "Op_BatchTransfer" {
            return Err(format!("the batch runs {constructor}"));
        }
        let op = inner.and_then(as_record).ok_or("an Op_BatchTransfer has no record")?;
        admins.push(("an operation's expectedAdmin", party_at(op, "expectedAdmin")?));
        let instrument = record_at(op, "instrumentId")?;
        admins.push(("an operation's instrument admin", party_at(instrument, "admin")?));
        let id = text_at(instrument, "id")?;
        if id != "Amulet" {
            return Err(format!("an operation pays {id}, not Amulet"));
        }
        check_window(op, BATCH_PAY_WINDOW_MICROS)?;
        let mut receiver: Option<&str> = None;
        let mut sum = Decimal::ZERO;
        for target in list_at(op, "transferTargets")? {
            let target = as_record(target).ok_or("a transfer target is not a record")?;
            let to = party_at(target, "receiver")?;
            if receiver.is_some_and(|r| r != to) {
                return Err("an Op_BatchTransfer pays more than one receiver".to_string());
            }
            receiver = Some(to);
            let amount = get_record_field(target, "amount").and_then(get_numeric).ok_or("a transfer target has no amount")?;
            let value = crate::decimal::parse_exact(amount).ok_or("a transfer target amount is not a decimal")?;
            sum = sum.checked_add(value).ok_or("the transfer target amounts overflow")?;
            legs.push((to, amount));
        }
        let to = receiver.ok_or("an Op_BatchTransfer has no targets")?;
        if paid.iter().any(|(r, _)| *r == to) {
            return Err(format!("{to} is paid by more than one operation"));
        }
        paid.push((to, sum));
    }
    if paid.len() != expected.len() {
        return Err(format!("the batch pays {} receiver(s), not {}", paid.len(), expected.len()));
    }
    for (to, sum) in &paid {
        match expected.iter().find(|(r, _)| r.as_str() == *to) {
            Some((_, want)) if want == sum => {}
            Some((_, want)) => return Err(format!("the batch pays {sum} to {to}, not {want}")),
            None => return Err(format!("the batch pays {to}, who is not a target")),
        }
    }
    let dso = resolve_dso(pinned_dso, &admins, metadata, &coins, party)?;

    // Each target: one direct CC transfer, in operation order.
    let transfers: Vec<&Exercise> = child_exercises(root, &nodes).collect();
    if transfers.len() != legs.len() || transfers.iter().any(|e| e.choice_id != "TransferFactory_Transfer") {
        return Err(format!(
            "the batch runs {} exercise(s) for {} transfer target(s)",
            transfers.len(),
            legs.len()
        ));
    }
    for (ex, (to, amount)) in transfers.iter().zip(&legs) {
        let leg = CcLeg { sender: party, receiver: Some(*to), amount, window_micros: BATCH_PAY_WINDOW_MICROS };
        check_cc_transfer_node(ex, &nodes, metadata, &leg, &dso)?;
    }
    for id in &root.children {
        match nodes.get(id).copied().and_then(get_node_type) {
            Some(NodeType::Exercise(_)) | Some(NodeType::Fetch(_)) => {}
            Some(other) => return Err(format!("unexpected {} in the batch", node_kind(other))),
            None => return Err(format!("node {id} is missing")),
        }
    }

    // What leaves the party: exactly the targets, as holdings.
    let facts = crate::facts::extract_facts(prepared_transaction_bytes, party).map_err(|e| e.to_string())?;
    let mut flows: Vec<(&str, Decimal)> = Vec::new();
    for flow in &facts.outflows {
        if flow.kind != FlowKind::Holding {
            return Err(format!("the batch sends {} {} to {} as {:?}", flow.amount, flow.instrument_id, flow.to, flow.kind));
        }
        if flow.instrument_admin != dso || flow.instrument_id != "Amulet" {
            return Err(format!("the batch sends {} {}", flow.instrument_admin, flow.instrument_id));
        }
        match flows.iter_mut().find(|(to, _)| *to == flow.to) {
            Some((_, sum)) => *sum = sum.checked_add(flow.amount).ok_or("the batch outflows overflow")?,
            None => flows.push((flow.to.as_str(), flow.amount)),
        }
    }
    if flows.len() != expected.len() || flows.iter().any(|(to, sum)| !expected.iter().any(|(r, a)| r.as_str() == *to && a == sum)) {
        return Err("the batch outflows are not the targets".to_string());
    }
    if let Some(c) = facts.consumed.iter().find(|c| !requested.contains(&c.contract_id.as_str())) {
        return Err(format!("the batch consumes coin {} it does not name", short(&c.contract_id, 16)));
    }
    if let Some(cid) = facts.unlisted_inputs.first() {
        return Err(format!("the batch consumes unlisted contract {}", short(cid, 16)));
    }
    Ok(expected.len())
}

fn node_kind(node: &NodeType) -> &'static str {
    match node {
        NodeType::Create(_) => "Create",
        NodeType::Exercise(_) => "Exercise",
        NodeType::Fetch(_) => "Fetch",
        NodeType::Rollback(_) => "Rollback",
    }
}
