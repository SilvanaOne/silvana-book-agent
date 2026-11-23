//! `control remove` and `control capabilities`, under the minimum rule: a change that
//! would leave a capability without a controller needs `--force` and a typed freeze.

use std::io::Write;

use agent_control::{
    BuildError, Capability, CapabilitySet, ControlState, ControllerId, EntryBuilder, HeadRef,
    MinimumRuleOutcome, PendingEntry, UtcTime, describe_rules, force_confirmation_phrase,
};
use anyhow::{Result, anyhow, bail};

use super::ControlContext;
use super::link::{build_refusal, delay_text, write_signed_times};
use super::prompt::{Prompter, Yes, confirm_typed};
use super::rpc::ControlRpc;
use super::sync;

/// The change asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Remove,
    Capabilities(CapabilitySet),
}

/// Fewest hex characters of an id that may name a controller.
const NAME_MIN_HEX: usize = 8;

/// A controller by a unique id prefix (at least 8 hex characters, a fingerprint's
/// spaces allowed) or by its exact label.
pub fn resolve_controller(state: &ControlState, name: &str) -> Result<ControllerId> {
    let name = name.trim();
    let hex: String = name.chars().filter(|c| *c != ' ').collect::<String>().to_ascii_lowercase();
    let by_label: Vec<&ControllerId> = state
        .controllers()
        .filter(|r| r.descriptor().label().as_str() == name)
        .map(|r| r.id())
        .collect();
    let by_id: Vec<&ControllerId> = if hex.len() >= NAME_MIN_HEX && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        state.controllers().filter(|r| r.id().to_string().starts_with(&hex)).map(|r| r.id()).collect()
    } else {
        Vec::new()
    };
    let mut found: Vec<&ControllerId> = by_label.into_iter().chain(by_id).collect();
    found.sort();
    found.dedup();
    match found.as_slice() {
        [id] => Ok(**id),
        [] => bail!("no controller matches {name:?}: give an exact label or at least {NAME_MIN_HEX} hex characters of its id"),
        _ => bail!("{name:?} matches {} controllers: give more of the id", found.len()),
    }
}

fn caps_text(set: CapabilitySet) -> String {
    if set.is_empty() { "none".to_string() } else { set.to_string() }
}

/// What nobody can do once `capability` has no controller.
fn loss(capability: Capability) -> &'static str {
    match capability {
        Capability::Policy => "nobody can change or pause the policy, which stays frozen as it is",
        Capability::Direct => "nobody can direct a transfer",
        Capability::Read => "nobody can open a read session",
    }
}

/// The refusal of a change that breaks the minimum rule, naming what would be left.
fn minimum_refusal(state: &ControlState, target: &ControllerId, outcome: &MinimumRuleOutcome) -> anyhow::Error {
    let others: Vec<String> = state
        .controllers()
        .filter(|r| r.id() != target)
        .map(|r| {
            let finals = r.final_capabilities().map_or_else(|| "removal pending".to_string(), caps_text);
            format!("{} \"{}\" ({finals})", r.id().fingerprint(), r.descriptor().label().as_str())
        })
        .collect();
    anyhow!(
        "refusing: after every pending change, no controller would hold {} (remaining controllers: {}). \
         Add another controller first, or use --force to freeze",
        caps_text(outcome.orphaned()),
        if others.is_empty() { "none".to_string() } else { others.join("; ") }
    )
}

fn write_freeze_warning(out: &mut dyn Write, state: &ControlState, outcome: &MinimumRuleOutcome, now: UtcTime) {
    let _ = writeln!(out, "WARNING: this leaves {} without any controller:", caps_text(outcome.orphaned()));
    for capability in outcome.orphaned().iter() {
        let _ = writeln!(out, "  - {}: {}", capability.as_str(), loss(capability));
    }
    let rules = state.rules_at(now);
    match rules.policy() {
        Some(policy) => {
            let _ = writeln!(out, "Policy in force: v{} ({}), from {}:", policy.version(), policy.hash(), policy.effective_at());
        }
        None => {
            let _ = writeln!(out, "Policy in force: the default rules:");
        }
    }
    for line in describe_rules(rules.rules()) {
        let _ = writeln!(out, "  {line}");
    }
    if let Some(scheduled) = state.scheduled_policy(now) {
        let _ = writeln!(out, "Scheduled: v{} ({}), from {}:", scheduled.version(), scheduled.hash(), scheduled.effective_at());
        for line in describe_rules(scheduled.rules()) {
            let _ = writeln!(out, "  {line}");
        }
    }
    let _ = writeln!(
        out,
        "A new controller takes {} to take effect, and a new link needs every controller removed first.",
        delay_text(state.network())
    );
}

fn build(state: &ControlState, id: &ControllerId, change: Change, force: bool, at: UtcTime) -> Result<PendingEntry, BuildError> {
    let builder = EntryBuilder::new(state);
    match change {
        Change::Remove => builder.remove_controller(id, force, at),
        Change::Capabilities(set) => builder.set_capabilities(id, set, force, at),
    }
}

/// `control remove <controller>` and `control capabilities <controller> <list>`.
#[allow(clippy::too_many_arguments)]
pub async fn run_change(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    controller: &str,
    change: Change,
    force: bool,
    yes: bool,
) -> Result<HeadRef> {
    if force && yes {
        bail!("--yes is not accepted with --force: the freeze must be typed");
    }
    ctx.require_env_file()?;
    let prepared = sync::prepare_change(ctx, rpc, out).await?;
    let state = &prepared.state;
    let id = resolve_controller(state, controller)?;
    let record = state.controller(&id).ok_or_else(|| anyhow!("no such controller"))?;
    let now = rpc.checked_now()?;
    let (preview, forced) = match build(state, &id, change, false, now) {
        Ok(entry) => (entry, false),
        Err(BuildError::MinimumRule(outcome)) => {
            if !force {
                return Err(minimum_refusal(state, &id, &outcome));
            }
            write_freeze_warning(out, state, &outcome, now);
            (build(state, &id, change, true, now).map_err(|e| build_refusal(&e))?, true)
        }
        Err(e) => return Err(build_refusal(&e)),
    };
    let label = record.descriptor().label().as_str().to_string();
    let current = caps_text(record.capabilities_at(now));
    let target = match change {
        Change::Remove => "removed".to_string(),
        Change::Capabilities(set) => format!("capabilities {set}"),
    };
    let _ = writeln!(out, "Controller {} \"{label}\" (now: {current}) becomes {target}.", id.fingerprint());
    let effective_at = preview.entry().effective_at();
    if effective_at == preview.entry().created_at() {
        let _ = writeln!(out, "It withdraws a grant not yet in effect, so it applies at once.");
    } else {
        let _ = writeln!(
            out,
            "It takes effect at about {effective_at} ({} after the signature); until then the controller keeps what it holds.",
            delay_text(ctx.network)
        );
    }
    let phrase = if forced {
        force_confirmation_phrase(state)
    } else {
        match change {
            Change::Remove => "remove".to_string(),
            Change::Capabilities(_) => "change".to_string(),
        }
    };
    let yes = if forced || force { Yes::Refused } else if yes { Yes::Given } else { Yes::NotGiven };
    let (state, _) = prepared.release();
    confirm_typed(prompter, out, &phrase, yes).await?;
    let (lock, state) = sync::relock(ctx, state.head()).await?;
    let entry = build(&state, &id, change, forced, rpc.checked_now()?).map_err(|e| build_refusal(&e))?;
    if entry.entry().body() != preview.entry().body() {
        bail!("the change differs from the one confirmed: run the command again");
    }
    let head = sync::sign_and_upload(ctx, rpc, lock, &entry, out).await?;
    write_signed_times(out, entry.entry());
    Ok(head)
}
