//! What the controller commands print: text for people, `--json` for scripts. Values the rpc
//! reports are labelled so; everything else comes from the log verified here.

use std::io::Write;

use agent_control::json::{CommandOpJson, PolicyJson, ReceiptJson, SignedEntryJson};
use agent_control::{AcceptedPolicy, Capability, CapabilitySet, CommandOp, PendingTarget, UtcTime};
use agent_controller::{
    PinCheck, PolicyDraft, ReceiptCheck, Reported, Standing, VerifiedAgent, Waited,
};
use orderbook_proto::orderbook::{
    ControlCommandStatus, ControlCommandView, ControlPolicyStatus, ControlPolicyView,
    GetAgentActivityResponse, GetAgentHoldingsResponse, GetAgentLedgerResponse,
    GetAgentTrafficResponse, ListCommandsResponse, ListNodePackagesResponse,
};
use serde_json::{Value, json};

/// The label of every value the rpc reports.
pub(crate) const REPORTED: &str = "Reported by the rpc (not verified here):";

pub(crate) fn caps(set: CapabilitySet) -> String {
    if set.is_empty() {
        return "none".into();
    }
    set.iter()
        .map(Capability::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn ts(at: Option<&prost_types::Timestamp>) -> String {
    at.and_then(|t| UtcTime::from_unix_secs(t.seconds))
        .map_or_else(|| "-".to_string(), |t| t.to_string())
}

fn ts_json(at: Option<&prost_types::Timestamp>) -> Value {
    at.and_then(|t| UtcTime::from_unix_secs(t.seconds))
        .map_or(Value::Null, |t| Value::String(t.to_string()))
}

pub(crate) fn command_status(value: i32) -> &'static str {
    match ControlCommandStatus::try_from(value) {
        Ok(ControlCommandStatus::Pending) => "pending",
        Ok(ControlCommandStatus::Delivered) => "delivered",
        Ok(ControlCommandStatus::Executed) => "executed",
        Ok(ControlCommandStatus::Failed) => "failed",
        Ok(ControlCommandStatus::Refused) => "refused",
        Ok(ControlCommandStatus::Expired) => "expired",
        Ok(ControlCommandStatus::Unspecified) | Err(_) => "unknown",
    }
}

pub(crate) fn policy_status(value: i32) -> &'static str {
    match ControlPolicyStatus::try_from(value) {
        Ok(ControlPolicyStatus::Pending) => "pending",
        Ok(ControlPolicyStatus::Accepted) => "accepted",
        Ok(ControlPolicyStatus::Superseded) => "superseded",
        Ok(ControlPolicyStatus::Refused) => "refused",
        Ok(ControlPolicyStatus::Expired) => "expired",
        Ok(ControlPolicyStatus::Unspecified) | Err(_) => "unknown",
    }
}

/// `operation: <name>` and the operation's fields, one per line.
pub(crate) fn write_command_op(out: &mut dyn Write, op: &CommandOp, indent: &str) {
    let _ = writeln!(out, "{indent}operation: {}", op.name());
    let fields = serde_json::to_value(CommandOpJson::from(op)).unwrap_or_default();
    if let Some(fields) = fields.as_object() {
        for (name, value) in fields.iter().filter(|(name, _)| name.as_str() != "name") {
            let _ = writeln!(
                out,
                "{indent}  {name}: {}",
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string)
            );
        }
    }
}

fn print_json(out: &mut dyn Write, value: &Value) {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into());
    let _ = writeln!(out, "{text}");
}

pub(crate) fn standing_text(standing: &Standing) -> String {
    match standing {
        Standing::NotRecorded => "not a controller of this agent".into(),
        Standing::Removed => "removed".into(),
        Standing::Pending {
            capabilities,
            effective_at,
        } => {
            format!("pending: {} from {effective_at}", caps(*capabilities))
        }
        Standing::InEffect {
            capabilities,
            pending,
        } => {
            let mut text = format!("in effect: {}", caps(*capabilities));
            match pending.map(|p| (p.target, p.effective_at)) {
                Some((PendingTarget::Removal, at)) => text.push_str(&format!("; removed at {at}")),
                Some((PendingTarget::Capabilities(next), at)) => {
                    text.push_str(&format!("; becomes {} at {at}", caps(next)));
                }
                None => {}
            }
            text
        }
    }
}

fn pin_text(pin: &PinCheck) -> String {
    match pin {
        PinCheck::New => "first time this key sees the agent".into(),
        PinCheck::Same => "matches the head seen before".into(),
        PinCheck::Ahead { pinned } => format!("extends seq {} seen before", pinned.seq),
        PinCheck::Shorter { pinned } => format!(
            "REFUSED: shorter than seq {} seen before; nothing is signed until the rpc serves it again",
            pinned.seq
        ),
        PinCheck::Divergent { pinned } => format!(
            "REFUSED: another entry at seq {} than the one seen before; nothing is signed",
            pinned.seq
        ),
    }
}

fn policy_line(policy: &AcceptedPolicy) -> String {
    format!(
        "v{} {} signed by {}, accepted at seq {}, in effect from {}",
        policy.version(),
        policy.hash(),
        policy.signed_by().fingerprint(),
        policy.seq(),
        policy.effective_at()
    )
}

fn label_of(record: &agent_control::ControllerRecord) -> String {
    record.descriptor().label().as_str().to_string()
}

/// `controller status`.
pub(crate) fn status(out: &mut dyn Write, agent: &VerifiedAgent, fingerprint: &str, json: bool) {
    let state = agent.state();
    let now = agent.checked_at();
    let head = agent.head();
    if json {
        let controllers: Vec<Value> = state
            .controllers()
            .map(|record| {
                json!({
                    "controllerId": record.id().to_string(),
                    "fingerprint": record.id().fingerprint(),
                    "label": label_of(record),
                    "capabilities": record.capabilities_at(now).iter().map(Capability::as_str).collect::<Vec<_>>(),
                    "pending": record.pending_at(now).map(|p| json!({
                        "effectiveAt": p.effective_at.to_string(),
                        "removal": matches!(p.target, PendingTarget::Removal),
                    })),
                })
            })
            .collect();
        let reported = &agent.reported().0;
        print_json(
            out,
            &json!({
                "agentParty": state.agent_party().to_string(),
                "network": state.network().as_str(),
                "head": { "seq": head.seq, "hash": head.hash.to_string() },
                "entries": agent.log().entries().len(),
                "pin": pin_text(&agent.pin()),
                "signingAllowed": agent.pin().allows_signing(),
                "key": { "fingerprint": fingerprint, "standing": standing_text(&agent.standing()) },
                "controllers": controllers,
                "policyInForce": state.in_force_policy(now).map(|p| PolicyJson::from(p.document())),
                "policyScheduled": state.scheduled_policy(now).map(|p| PolicyJson::from(p.document())),
                "disagreements": agent.disagreements(),
                "reportedByRpc": {
                    "pendingPolicies": reported.pending_policies.len(),
                    "commands": reported.commands.len(),
                    "agentConnected": reported.agent_connected,
                    "agentLastSeen": ts_json(reported.agent_last_seen.as_ref()),
                },
            }),
        );
        return;
    }
    let _ = writeln!(
        out,
        "Agent:       {} ({})",
        state.agent_party(),
        state.network()
    );
    let _ = writeln!(
        out,
        "Log:         verified here: {} entries, head seq {} ({})",
        agent.log().entries().len(),
        head.seq,
        head.hash
    );
    let _ = writeln!(out, "Pin:         {}", pin_text(&agent.pin()));
    let _ = writeln!(
        out,
        "This key:    {fingerprint}: {}",
        standing_text(&agent.standing())
    );
    match state.in_force_policy(now) {
        Some(policy) => {
            let _ = writeln!(out, "Policy:      {}", policy_line(policy));
        }
        None => {
            let _ = writeln!(out, "Policy:      the default rules (no policy accepted)");
        }
    }
    if let Some(policy) = state.scheduled_policy(now) {
        let _ = writeln!(out, "Scheduled:   {}", policy_line(policy));
    }
    let _ = writeln!(out, "Controllers:");
    for record in state.controllers() {
        let standing = Standing::of(state, record.id(), now);
        let _ = writeln!(
            out,
            "  {} \"{}\": {}",
            record.id().fingerprint(),
            label_of(record),
            standing_text(&standing)
        );
    }
    if agent.disagreements().is_empty() {
        let _ = writeln!(out, "The rpc's account matches the verified log.");
    } else {
        let _ = writeln!(out, "The rpc's account differs from the verified log:");
        for line in agent.disagreements() {
            let _ = writeln!(out, "  - {line}");
        }
    }
    let reported = &agent.reported().0;
    let _ = writeln!(out, "{REPORTED}");
    let _ = writeln!(
        out,
        "  {} pending policies, {} commands; agent connected: {} (last seen {})",
        reported.pending_policies.len(),
        reported.commands.len(),
        if reported.agent_connected {
            "yes"
        } else {
            "no"
        },
        ts(reported.agent_last_seen.as_ref())
    );
}

/// `controller log`.
pub(crate) fn log(out: &mut dyn Write, agent: &VerifiedAgent, json: bool) {
    if json {
        let entries: Vec<SignedEntryJson> = agent
            .log()
            .entries()
            .iter()
            .map(SignedEntryJson::from)
            .collect();
        print_json(out, &serde_json::to_value(entries).unwrap_or_default());
        return;
    }
    let _ = writeln!(
        out,
        "Control log of {} (verified here):",
        agent.state().agent_party()
    );
    for entry in agent.log().entries() {
        let body = entry.entry();
        let _ = writeln!(
            out,
            "  {:>4}  {:<18} signed {}  effective {}  {}",
            entry.seq(),
            body.kind().as_str(),
            body.created_at(),
            body.effective_at(),
            entry.hash()
        );
    }
}

/// `controller policy show`.
pub(crate) fn policy_show(out: &mut dyn Write, agent: &VerifiedAgent, json: bool) {
    let state = agent.state();
    let now = agent.checked_at();
    let reported = &agent.reported().0;
    if json {
        let pending: Vec<Value> = reported
            .pending_policies
            .iter()
            .map(policy_view_json)
            .collect();
        print_json(
            out,
            &json!({
                "inForce": state.in_force_policy(now).map(|p| PolicyJson::from(p.document())),
                "scheduled": state.scheduled_policy(now).map(|p| PolicyJson::from(p.document())),
                "defaultRules": state.in_force_policy(now).is_none(),
                "pendingReportedByRpc": pending,
            }),
        );
        return;
    }
    match state.in_force_policy(now) {
        Some(policy) => {
            let _ = writeln!(out, "In force: {}", policy_line(policy));
        }
        None => {
            let _ = writeln!(out, "In force: the default rules (no policy accepted)");
        }
    }
    for line in agent_control::describe_rules(state.rules_at(now).rules()) {
        let _ = writeln!(out, "  {line}");
    }
    if let Some(policy) = state.scheduled_policy(now) {
        let _ = writeln!(out, "Scheduled: {}", policy_line(policy));
        for line in agent_control::describe_rules(policy.rules()) {
            let _ = writeln!(out, "  {line}");
        }
    }
    if !reported.pending_policies.is_empty() {
        let _ = writeln!(out, "{REPORTED}");
        for view in &reported.pending_policies {
            let _ = writeln!(out, "  {}", policy_view_line(view));
        }
    }
}

fn policy_view_line(view: &ControlPolicyView) -> String {
    format!(
        "v{} {} {} (signed by {}, effective at {})",
        view.version,
        view.policy_hash,
        policy_status(view.status),
        view.signed_by,
        ts(view.effective_at.as_ref())
    )
}

fn policy_view_json(view: &ControlPolicyView) -> Value {
    json!({
        "policyHash": view.policy_hash,
        "version": view.version,
        "status": policy_status(view.status),
        "signedBy": view.signed_by,
        "effectiveAt": ts_json(view.effective_at.as_ref()),
        "widening": view.widening,
    })
}

/// A policy draft before its confirmation.
pub(crate) fn policy_draft(out: &mut dyn Write, agent: &VerifiedAgent, draft: &PolicyDraft) {
    let doc = &draft.document;
    let _ = writeln!(
        out,
        "Policy v{} for {} on {}",
        doc.version, doc.agent_party, doc.network
    );
    let _ = writeln!(out, "  chained to {}", doc.prev_policy_hash);
    let _ = writeln!(
        out,
        "  issued {}, valid for acceptance until {}",
        doc.issued_at, doc.expires_at
    );
    for line in agent_control::describe_rules(&doc.rules) {
        let _ = writeln!(out, "  {line}");
    }
    let base = draft
        .baseline_version
        .map_or_else(|| "the default rules".to_string(), |v| format!("v{v}"));
    let _ = writeln!(
        out,
        "  against {base}: {}",
        super::super::policy_accept::widening_text(&draft.widening)
    );
    let baseline = agent.state().rules_at(agent.checked_at()).rules();
    for line in super::super::policy_accept::package_diff(baseline, &doc.rules) {
        let _ = writeln!(out, "  {line}");
    }
    if draft.widens() {
        let _ = writeln!(
            out,
            "  It widens: once accepted it takes effect at {} ({} s delay).",
            draft.effective_at, draft.required_delay_secs
        );
    } else {
        let _ = writeln!(
            out,
            "  It widens nothing: it takes effect when the agent accepts it."
        );
    }
}

/// A command before its confirmation, or as shown.
pub(crate) fn command_draft(out: &mut dyn Write, command: &agent_control::Command, code: &str) {
    let _ = writeln!(
        out,
        "Command {code} for {} on {}",
        command.agent_party, command.network
    );
    write_command_op(out, &command.op, "  ");
    let _ = writeln!(out, "  summary: {}", command.summary.as_str());
    let _ = writeln!(
        out,
        "  at log seq {} ({})",
        command.log_seq, command.log_head
    );
    let _ = writeln!(
        out,
        "  issued {}, expires {}",
        command.issued_at, command.expires_at
    );
}

fn receipt_text(check: &ReceiptCheck) -> String {
    match check {
        ReceiptCheck::None => "no receipt yet".into(),
        ReceiptCheck::Verified(receipt) => {
            let body = receipt.receipt();
            let mut text = format!("verified receipt: {}", body.outcome.as_str());
            if let Some(update) = &body.update_id {
                text.push_str(&format!(", update {}", update.as_str()));
            }
            if let (Some(code), Some(source)) = (body.reason_code, body.code_source) {
                text.push_str(&format!(", code {code} ({})", source.as_str()));
            }
            if let Some(result) = receipt.swap_result() {
                text.push_str(&format!(
                    ", sold {} received {} fee {} CC via {}",
                    result.sold, result.received, result.fee_cc, result.lp
                ));
            }
            text
        }
        ReceiptCheck::Unverified(why) => format!("UNVERIFIED receipt: {why}"),
    }
}

/// A command as the rpc reports it, with its receipt checked here.
pub(crate) fn command_state(out: &mut dyn Write, waited: &Waited, json: bool) {
    let view = &waited.view.0;
    if json {
        let receipt = match &waited.receipt {
            ReceiptCheck::Verified(receipt) => {
                json!({ "verified": true, "receipt": ReceiptJson::from(receipt.as_ref()) })
            }
            ReceiptCheck::Unverified(why) => json!({ "verified": false, "why": why }),
            ReceiptCheck::None => Value::Null,
        };
        let mut value = command_view_json(view);
        if let Some(fields) = value.as_object_mut() {
            fields.insert("receipt".into(), receipt);
        }
        print_json(out, &value);
        return;
    }
    let _ = writeln!(
        out,
        "Command CMD-{}: {} ({})",
        view.command_id,
        command_status(view.status),
        Reported::<()>::LABEL
    );
    let _ = writeln!(out, "  {}", receipt_text(&waited.receipt));
}

fn command_view_json(view: &ControlCommandView) -> Value {
    json!({
        "code": format!("CMD-{}", view.command_id),
        "commandId": view.command_id,
        "operationKind": view.operation_kind,
        "summary": view.summary,
        "status": command_status(view.status),
        "signedBy": view.controller_id,
        "issuedAt": ts_json(view.issued_at.as_ref()),
        "expiresAt": ts_json(view.expires_at.as_ref()),
        "submittedAt": ts_json(view.submitted_at.as_ref()),
        "deliveredAt": ts_json(view.delivered_at.as_ref()),
    })
}

/// `controller commands`.
pub(crate) fn commands(out: &mut dyn Write, list: &ListCommandsResponse, json: bool) {
    if json {
        let items: Vec<Value> = list.commands.iter().map(command_view_json).collect();
        print_json(
            out,
            &json!({ "reportedByRpc": true, "total": list.total, "commands": items }),
        );
        return;
    }
    let _ = writeln!(out, "{REPORTED} {} command(s) in total", list.total);
    for view in &list.commands {
        let _ = writeln!(
            out,
            "  CMD-{}  {:<14} {:<9} expires {}  {}",
            view.command_id,
            view.operation_kind,
            command_status(view.status),
            ts(view.expires_at.as_ref()),
            view.summary
        );
    }
}

/// `controller holdings`.
pub(crate) fn holdings(out: &mut dyn Write, reply: &GetAgentHoldingsResponse, json: bool) {
    if json {
        let totals: Vec<Value> = reply
            .totals
            .iter()
            .map(|t| json!({
                "instrumentId": t.instrument_id, "instrumentAdmin": t.instrument_admin,
                "total": t.total_amount, "locked": t.locked_amount, "unlocked": t.unlocked_amount,
            }))
            .collect();
        let items: Vec<Value> = reply
            .holdings
            .iter()
            .map(|h| json!({
                "contractId": h.contract_id, "instrumentId": h.instrument_id, "instrumentAdmin": h.instrument_admin,
                "amount": h.amount, "locked": h.is_locked, "label": h.label,
            }))
            .collect();
        print_json(
            out,
            &json!({
                "reportedByRpc": true, "asOf": ts_json(reply.as_of.as_ref()), "stale": reply.stale,
                "truncated": reply.truncated, "totals": totals, "holdings": items,
            }),
        );
        return;
    }
    let _ = writeln!(
        out,
        "{REPORTED} holdings as of {}{}",
        ts(reply.as_of.as_ref()),
        if reply.stale { " (stale)" } else { "" }
    );
    for total in &reply.totals {
        let _ = writeln!(
            out,
            "  {:<12} {:>24} total, {} unlocked, {} locked  (registry {})",
            total.instrument_id,
            total.total_amount,
            total.unlocked_amount,
            total.locked_amount,
            total.instrument_admin
        );
    }
    let _ = writeln!(
        out,
        "  {} holding contract(s){}",
        reply.holdings.len(),
        if reply.truncated {
            ", list truncated"
        } else {
            ""
        }
    );
}

/// `controller traffic`.
pub(crate) fn traffic(out: &mut dyn Write, reply: &GetAgentTrafficResponse, json: bool) {
    if json {
        let grants: Vec<Value> = reply
            .grants
            .iter()
            .map(|g| json!({
                "id": g.id, "source": g.source, "amountCc": g.amount_cc, "remainingCc": g.remaining_cc,
                "state": g.state, "expiresAt": ts_json(g.expires_at.as_ref()),
            }))
            .collect();
        print_json(
            out,
            &json!({
                "reportedByRpc": true, "availableCc": reply.available_cc, "poolCc": reply.pool_cc,
                "grantedCc": reply.granted_cc, "purchasedCc": reply.purchased_cc, "creditLimitCc": reply.credit_limit_cc,
                "discountPct": reply.discount_pct, "nextExpiry": ts_json(reply.next_expiry.as_ref()), "grants": grants,
            }),
        );
        return;
    }
    let _ = writeln!(out, "{REPORTED} traffic");
    let _ = writeln!(
        out,
        "  available {} CC (pool {}, granted {}, purchased {})",
        reply.available_cc, reply.pool_cc, reply.granted_cc, reply.purchased_cc
    );
    let _ = writeln!(
        out,
        "  credit limit {} CC, discount {} %, next expiry {}",
        reply.credit_limit_cc,
        reply.discount_pct,
        ts(reply.next_expiry.as_ref())
    );
    for grant in &reply.grants {
        let _ = writeln!(
            out,
            "  grant {}: {} of {} CC left, {} (expires {})",
            grant.id,
            grant.remaining_cc,
            grant.amount_cc,
            grant.state,
            ts(grant.expires_at.as_ref())
        );
    }
}

/// `controller activity`.
pub(crate) fn activity(out: &mut dyn Write, reply: &GetAgentActivityResponse, json: bool) {
    if json {
        let items: Vec<Value> = reply
            .items
            .iter()
            .map(|i| json!({
                "rowId": i.row_id, "source": i.source, "side": i.side, "role": i.role, "marketId": i.market_id,
                "baseInstrument": i.base_instrument, "baseQuantity": i.base_quantity,
                "quoteInstrument": i.quote_instrument, "quoteQuantity": i.quote_quantity, "status": i.status,
                "createdAt": ts_json(i.created_at.as_ref()), "settledAt": ts_json(i.settled_at.as_ref()),
                "updateId": i.update_id,
            }))
            .collect();
        print_json(
            out,
            &json!({ "reportedByRpc": true, "total": reply.total, "items": items }),
        );
        return;
    }
    let _ = writeln!(out, "{REPORTED} {} activity row(s) in total", reply.total);
    for item in &reply.items {
        let _ = writeln!(
            out,
            "  {}  {:<10} {:<4} {} {} / {} {}  {}",
            ts(item.created_at.as_ref()),
            item.market_id,
            item.side,
            item.base_quantity,
            item.base_instrument,
            item.quote_quantity,
            item.quote_instrument,
            item.status
        );
    }
}

/// `controller ledger`.
pub(crate) fn ledger(out: &mut dyn Write, reply: &GetAgentLedgerResponse, json: bool) {
    if json {
        let items: Vec<Value> = reply
            .transactions
            .iter()
            .map(|t| {
                json!({
                    "id": t.id, "txType": t.tx_type, "result": t.result, "updateId": t.update_id,
                    "marketId": t.market_id, "amount": t.amount, "token": t.token,
                    "createdAt": ts_json(t.created_at.as_ref()),
                })
            })
            .collect();
        print_json(
            out,
            &json!({ "reportedByRpc": true, "total": reply.total, "transactions": items }),
        );
        return;
    }
    let _ = writeln!(
        out,
        "{REPORTED} {} ledger transaction(s) in total",
        reply.total
    );
    for tx in &reply.transactions {
        let _ = writeln!(
            out,
            "  {}  {:<24} {:<10} {} {}",
            ts(tx.created_at.as_ref()),
            tx.tx_type,
            tx.result,
            tx.amount.as_deref().unwrap_or("-"),
            tx.token.as_deref().unwrap_or("")
        );
    }
}

/// `controller packages`.
pub(crate) fn packages(out: &mut dyn Write, reply: &ListNodePackagesResponse, json: bool) {
    if json {
        let items: Vec<Value> = reply
            .packages
            .iter()
            .map(|p| {
                json!({
                    "packageId": p.package_id, "name": p.package_name, "version": p.package_version,
                    "removedAt": ts_json(p.removed_at.as_ref()),
                })
            })
            .collect();
        print_json(
            out,
            &json!({
                "reportedByRpc": true, "network": reply.network, "participantId": reply.participant_id,
                "refreshedAt": ts_json(reply.refreshed_at.as_ref()), "maxPackages": reply.max_packages, "packages": items,
            }),
        );
        return;
    }
    let _ = writeln!(
        out,
        "{REPORTED} {} package(s) vetted by {} (refreshed {})",
        reply.packages.len(),
        reply.participant_id,
        ts(reply.refreshed_at.as_ref())
    );
    for package in &reply.packages {
        let removed = if package.removed_at.is_some() {
            "  (removed)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {}  {} {}{removed}",
            package.package_id, package.package_name, package.package_version
        );
    }
}
