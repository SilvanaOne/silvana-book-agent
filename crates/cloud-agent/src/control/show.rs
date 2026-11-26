//! `control show` and `control policy show`, in plain words or JSON.

use std::io::Write;

use agent_control::{AcceptedPolicy, ControlState, PendingTarget, PolicyDocument, RpPolicy, UtcTime, describe_rules};
use agent_logic::control_store::{PendingStatus, StoredLog};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::ControlContext;
use super::link::party_fingerprint;
use super::policy_accept::{Candidate, Verdict, judge, widening_text};
use super::rpc::{ControlRpc, RpcError};
use super::sync::state_of;
use super::{Standing, standing, verify_remote};

/// `[vetted_packages]` in words: a selection is checked before every signature.
pub(crate) fn vetted_packages_text(vetted: &agent_logic::config::VettedPackages) -> String {
    match vetted {
        agent_logic::config::VettedPackages::All => "all (no check)".to_string(),
        agent_logic::config::VettedPackages::Selected(_) => {
            format!("{}, enforced on every signature (package_not_allowed)", vetted.describe())
        }
    }
}

fn now_utc() -> Result<UtcTime> {
    agent_logic::clock::unix_now()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .and_then(UtcTime::from_unix_secs)
        .ok_or_else(|| anyhow!("the local clock is unreadable"))
}

/// How the rpc's copy compares with the local one.
async fn remote_status(ctx: &ControlContext, rpc: &ControlRpc, stored: Option<&StoredLog>) -> String {
    let remote = match rpc.fetch_log().await {
        Ok(remote) => remote,
        Err(RpcError::Unimplemented(_)) => return "the rpc has no control service".into(),
        Err(e) => return format!("not compared: {e}"),
    };
    let view = match verify_remote(ctx.network, &ctx.party, &ctx.signer.public_key(), &remote) {
        Ok(view) => view,
        Err(e) => return format!("INVALID: {e:#}"),
    };
    match (stored, view.head()) {
        (None, Some(r)) => format!("the rpc holds seq {} but there is no local copy: run `cloud-agent control restore`", r.seq),
        (None, None) if remote.is_empty() => "no log on either side".into(),
        (None, None) => "the rpc holds rows that do not verify, and there is no local copy".into(),
        (Some(_), _) if remote.is_empty() => "the rpc holds no log: run `cloud-agent control sync`".into(),
        (Some(local), _) => match standing(&local.log, &remote, &view) {
            Ok(Standing::Equal) => format!("equal (seq {})", local.log.head().seq),
            Ok(Standing::Behind { bad }) if view.broken_at.is_some() => format!(
                "BROKEN at seq {} ({} entries missing or not verifying): run `cloud-agent control sync` to repair it",
                bad.first().copied().unwrap_or(1),
                bad.len()
            ),
            Ok(Standing::Behind { .. }) => format!(
                "the rpc is behind (seq {}): run `cloud-agent control sync`",
                view.head().map_or(0, |h| h.seq)
            ),
            Ok(Standing::Ahead { remote_tip, .. }) => {
                format!("the rpc is AHEAD (seq {remote_tip}): check which copy is genuine")
            }
            Ok(Standing::Fork(evidence)) => format!("FORK at seq {}", evidence.seq()),
            Err(e) => format!("not comparable: {e:#}"),
        },
    }
}

fn policy_json(policy: Option<&AcceptedPolicy>, now: UtcTime) -> Value {
    match policy {
        None => json!({"source": "default", "rules": describe_rules(&agent_control::DEFAULT_LINKED_RULES)}),
        Some(p) => json!({
            "source": "accepted",
            "version": p.version(),
            "hash": p.hash().to_string(),
            "seq": p.seq(),
            "signedBy": p.signed_by().to_string(),
            "acceptedAt": p.accepted_at().to_string(),
            "effectiveAt": p.effective_at().to_string(),
            "inForce": p.effective_at() <= now,
            "rules": describe_rules(p.rules()),
        }),
    }
}

fn write_policy(out: &mut dyn Write, title: &str, policy: Option<&AcceptedPolicy>) {
    match policy {
        None => {
            let _ = writeln!(out, "{title}: the default rules");
            for line in describe_rules(&agent_control::DEFAULT_LINKED_RULES) {
                let _ = writeln!(out, "  {line}");
            }
        }
        Some(p) => {
            let _ = writeln!(
                out,
                "{title}: v{} ({}), accepted at seq {}, effective {}, signed by {}",
                p.version(),
                p.hash(),
                p.seq(),
                p.effective_at(),
                p.signed_by().fingerprint()
            );
            for line in describe_rules(p.rules()) {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
}

fn pending_text(target: PendingTarget) -> String {
    match target {
        PendingTarget::Capabilities(set) => format!("capabilities {set}"),
        PendingTarget::Removal => "removal".to_string(),
    }
}

/// `control show [--json] [--offline]`.
pub async fn run_show(ctx: &ControlContext, rpc: Option<&ControlRpc>, out: &mut dyn Write, as_json: bool) -> Result<()> {
    let stored = ctx.store.snapshot()?;
    let state: ControlState = state_of(ctx, stored.as_ref())?;
    let now = now_utc()?;
    let remote = match rpc {
        Some(rpc) => Some(remote_status(ctx, rpc, stored.as_ref()).await),
        None => None,
    };
    let rp_ids: Vec<String> = RpPolicy::for_network(ctx.network).rp_ids().iter().map(|r| r.as_str().to_string()).collect();
    let pending = stored.as_ref().and_then(|s| s.pending.as_ref());
    let marker = ctx.marker.map(|m| m.to_string());
    let fee = ctx.config.party_fee.clone();
    let traffic = ctx.config.party_prepaid_traffic.clone();
    let traffic_fee = ctx.config.party_traffic_fee.clone();
    let topup_tokens = &ctx.config.prepaid_traffic_topup_tokens;
    let in_force = state.in_force_policy(now);
    let scheduled = state.scheduled_policy(now);

    if as_json {
        let controllers: Vec<Value> = state
            .controllers()
            .map(|r| {
                let view = agent_control::json::ControllerJson::from(r.descriptor());
                json!({
                    "controller": serde_json::to_value(&view).unwrap_or(Value::Null),
                    "capabilitiesNow": r.capabilities_at(now).to_string(),
                    "finalCapabilities": r.final_capabilities().map(|c| c.to_string()),
                    "pending": r.pending_at(now).map(|p| json!({
                        "change": pending_text(p.target),
                        "effectiveAt": p.effective_at.to_string(),
                        "seq": p.seq,
                    })),
                    "addedSeq": r.added_seq(),
                })
            })
            .collect();
        let view = json!({
            "network": ctx.network.as_str(),
            "agentParty": ctx.party.as_str(),
            "partyFingerprint": party_fingerprint(&ctx.party),
            "linked": state.is_linked(),
            "head": state.head().map(|h| json!({"seq": h.seq, "hash": h.hash.to_string()})),
            "ownerAccount": state.owner_account().map(|o| o.to_string()),
            "marker": marker,
            "pendingEntry": pending.map(|p| json!({
                "seq": p.entry.seq(),
                "kind": p.entry.kind().as_str(),
                "status": match p.status { PendingStatus::Uploading => "uploading", PendingStatus::Refused => "refused" },
                "refusedCode": p.refused_code,
            })),
            "controllers": controllers,
            "policyInForce": state.is_linked().then(|| policy_json(in_force, now)),
            "policyScheduled": scheduled.map(|p| policy_json(Some(p), now)),
            "platformParties": {"PARTY_FEE": fee, "PARTY_PREPAID_TRAFFIC": traffic, "PARTY_TRAFFIC_FEE": traffic_fee},
            "settings": {
                "prepaidTrafficTopupTokens": topup_tokens,
                "autoExecuteCommands": ctx.config.control.auto_execute_commands,
                "autoAcceptPolicies": ctx.config.control.auto_accept_policies,
                "swapMaxDeviationBps": ctx.config.control.swap_max_deviation_bps,
                "swapReferenceMaxAgeSecs": ctx.config.control.swap_reference_max_age_secs,
                "vettedPackages": ctx.config.vetted_packages.describe(),
                "vettedPackagesEnforced": ctx.config.vetted_packages != agent_logic::config::VettedPackages::All,
                "verifyMode": ctx.config.verify.mode.as_str(),
            },
            "rpIds": rp_ids,
            "remote": remote,
            "storeDir": ctx.store.dir().display().to_string(),
        });
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let _ = writeln!(out, "Agent:        {} on {}", ctx.party, ctx.network);
    let _ = writeln!(out, "Fingerprint:  {} (party; the Agent page shows the same)", party_fingerprint(&ctx.party));
    match state.head() {
        Some(head) => {
            let _ = writeln!(out, "Control log:  linked, head seq {} ({})", head.seq, head.hash);
        }
        None => {
            let _ = writeln!(out, "Control log:  not linked");
        }
    }
    if let Some(owner) = state.owner_account() {
        let _ = writeln!(out, "Owner:        account {owner}");
    }
    let _ = writeln!(out, "Marker:       {}", marker.as_deref().unwrap_or("unset"));
    let _ = writeln!(out, "Store:        {}", ctx.store.dir().display());
    if let Some(p) = pending {
        let status = match p.status {
            PendingStatus::Uploading => "signed, awaiting the rpc (run `cloud-agent control sync`)".to_string(),
            PendingStatus::Refused => format!(
                "REFUSED by the rpc{}; not signed again automatically (see `control discard-pending`)",
                p.refused_code.map(|c| format!(" (code {c})")).unwrap_or_default()
            ),
        };
        let _ = writeln!(out, "Pending:      {} at seq {}: {status}", p.entry.kind().as_str(), p.entry.seq());
    }
    let _ = writeln!(out, "Controllers:");
    let mut any = false;
    for r in state.controllers() {
        any = true;
        let _ = writeln!(
            out,
            "  {} \"{}\" ({}) id {}",
            r.id().fingerprint(),
            r.descriptor().label().as_str(),
            r.descriptor().controller_type().as_str(),
            r.id()
        );
        let now_caps = r.capabilities_at(now);
        let _ = writeln!(out, "      in effect: {}", if now_caps.is_empty() { "none".to_string() } else { now_caps.to_string() });
        if let Some(p) = r.pending_at(now) {
            let _ = writeln!(out, "      pending:   {} at {} (seq {})", pending_text(p.target), p.effective_at, p.seq);
        }
    }
    if !any {
        let _ = writeln!(out, "  none");
    }
    if state.is_linked() {
        write_policy(out, "Policy in force", in_force);
    } else {
        let _ = writeln!(out, "Policy:       none (not linked)");
    }
    if let Some(p) = scheduled {
        write_policy(out, "Policy scheduled", Some(p));
    }
    let _ = writeln!(
        out,
        "Pinned parties: PARTY_FEE={} PARTY_PREPAID_TRAFFIC={} PARTY_TRAFFIC_FEE={}",
        fee.as_deref().unwrap_or("unset"),
        traffic.as_deref().unwrap_or("unset"),
        traffic_fee.as_deref().unwrap_or("unset")
    );
    if fee.is_none() || traffic.is_none() {
        let _ = writeln!(out, "  (an unset party means no payment is treated as a fee or traffic purchase)");
    }
    if traffic_fee.is_none() && super::pins::traffic_fee_needed(topup_tokens) {
        let _ = writeln!(
            out,
            "  (PARTY_TRAFFIC_FEE is unset: top-ups paid in a token other than CC are refused; top-up tokens {})",
            topup_tokens.join(",")
        );
    }
    let _ = writeln!(
        out,
        "Settings:     {}; vetted packages {}; verify mode {}",
        ctx.config.control.describe(),
        vetted_packages_text(&ctx.config.vetted_packages),
        ctx.config.verify.mode.as_str()
    );
    let _ = writeln!(out, "Passkey rp_ids: {}", rp_ids.join(", "));
    match remote {
        Some(remote) => {
            let _ = writeln!(out, "rpc copy:     {remote}");
        }
        None => {
            let _ = writeln!(out, "rpc copy:     not checked (--offline)");
        }
    }
    Ok(())
}

/// `control policy show [--pending] [--json]`.
pub async fn run_policy_show(ctx: &ControlContext, rpc: Option<&ControlRpc>, out: &mut dyn Write, as_json: bool) -> Result<()> {
    let stored = ctx.store.snapshot()?;
    let state = state_of(ctx, stored.as_ref())?;
    let now = now_utc()?;
    let in_force = state.in_force_policy(now);
    let scheduled = state.scheduled_policy(now);
    let mut pending_views = Vec::new();
    if let Some(rpc) = rpc {
        match rpc.pending().await {
            Ok(list) => {
                for item in list.policies {
                    let verdict = match Candidate::from_pending(&item) {
                        Err(e) => format!("not judged: {e:#}"),
                        Ok(candidate) => match judge(&state, &candidate, now) {
                            Verdict::Acceptable(verified) => format!(
                                "acceptable: it {}; effective at {}",
                                widening_text(&verified.widening),
                                verified.effective_at
                            ),
                            Verdict::Refused { code, name } => format!("refused: {name} (code {code})"),
                            Verdict::NotYet => "not yet: issued later than this clock allows".to_string(),
                        },
                    };
                    let rules = PolicyDocument::decode(&item.document)
                        .map(|d| (d.version, describe_rules(&d.rules)))
                        .map_err(|e| e.code_name());
                    pending_views.push((item.policy_hash, verdict, rules));
                }
            }
            Err(RpcError::Unimplemented(_)) => pending_views.push((String::new(), "the rpc does not list pending policies yet".into(), Err("unimplemented"))),
            Err(e) => return Err(anyhow!("listing pending policies failed: {e}")),
        }
    }
    if as_json {
        let pending: Vec<Value> = pending_views
            .iter()
            .map(|(hash, verdict, rules)| match rules {
                Ok((version, lines)) => json!({"hash": hash, "verdict": verdict, "version": version, "rules": lines}),
                Err(name) => json!({"hash": hash, "verdict": verdict, "error": name}),
            })
            .collect();
        let view = json!({
            "linked": state.is_linked(),
            "inForce": policy_json(in_force, now),
            "scheduled": scheduled.map(|p| policy_json(Some(p), now)),
            "pending": rpc.map(|_| pending),
        });
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    if !state.is_linked() {
        let _ = writeln!(out, "This agent is not linked: no policy applies.");
        return Ok(());
    }
    write_policy(out, "Policy in force", in_force);
    match scheduled {
        Some(p) => write_policy(out, "Policy scheduled", Some(p)),
        None => {
            let _ = writeln!(out, "No policy is scheduled.");
        }
    }
    if rpc.is_some() {
        if pending_views.is_empty() {
            let _ = writeln!(out, "No pending policy.");
        }
        for (hash, verdict, rules) in &pending_views {
            let _ = writeln!(out, "Pending {hash}: {verdict}");
            if let Ok((version, lines)) = rules {
                let _ = writeln!(out, "  version {version}");
                for line in lines {
                    let _ = writeln!(out, "  {line}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_logic::config::VettedPackages;

    #[test]
    fn a_selection_is_stated_as_enforced_on_every_signature() {
        assert_eq!(vetted_packages_text(&VettedPackages::All), "all (no check)");
        let one = VettedPackages::Selected([agent_control::PackageId([1; 32])].into_iter().collect());
        assert_eq!(
            vetted_packages_text(&one),
            "selected (1 packages), enforced on every signature (package_not_allowed)"
        );
    }
}
