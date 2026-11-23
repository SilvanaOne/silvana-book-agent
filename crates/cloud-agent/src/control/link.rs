//! `control link` and `control add`: an owner's descriptor or a local Ed25519 key,
//! checked, shown and confirmed before the agent signs the entry.

use std::io::Write;

use agent_control::{
    BuildError, Capability, CapabilitySet, Commitment, ControlState, ControllerDescriptor, ControllerId,
    ControllerKey, ControllerType, EntryBuilder, HeadRef, Label, LinkDescriptor, LinkPurpose, LogEntry, Network,
    PartyId, PendingEntry, RpPolicy, UtcTime, controller_id_for, parse_link_code,
};
use anyhow::{Result, anyhow, bail, ensure};
use orderbook_proto::orderbook::ControlLinkStatus;

use super::ControlContext;
use super::prompt::{Prompter, Yes, confirm_typed};
use super::rpc::{ControlRpc, Descriptor};
use super::sync;

/// `--yes` as given; it is accepted for link and add.
fn yes_of(yes: bool) -> Yes {
    if yes { Yes::Given } else { Yes::NotGiven }
}

/// The party fingerprint the owner checks on the Agent page: the 20 hex characters after
/// `1220` in the namespace, uppercase, in 5 groups of 4 (empty for a malformed namespace).
pub fn party_fingerprint(party: &PartyId) -> String {
    let Some(head) = party.namespace().strip_prefix("1220").and_then(|rest| rest.get(..20)) else {
        return String::new();
    };
    let groups: Vec<String> = head
        .to_ascii_uppercase()
        .as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    groups.join(" ")
}

/// Plain words for a refused build; names come from the crate.
pub(crate) fn build_refusal(e: &BuildError) -> anyhow::Error {
    let name = e.code_name();
    let hint = match name {
        "commitment_mismatch" => "the descriptor the rpc returned does not match the code",
        "expired" | "descriptor_expired" => "the code has expired: ask the owner for a new one",
        "party_mismatch" => "the code was made for another agent",
        "invite_not_allowed" => "an invite code links only during onboarding",
        "wrong_network" => "the code is for another network",
        "rp_id_not_allowed" | "origin_not_allowed" => "a passkey in the code is outside this network's relying party",
        "wrong_purpose" => "the code is for another kind of change",
        "link_while_controlled" => "the agent already has controllers in effect or pending",
        "minimum_rule_violation" => "it would leave a capability without a controller",
        "log_full" => "the control log is full",
        "duplicate_controller" => "that controller is already listed",
        "too_many_controllers" => "the agent has the most controllers it may have",
        "agent_key_as_controller" => "the agent's own key cannot be a controller",
        "owner_mismatch" => "the code was made by another owner account",
        _ => "the change is not valid",
    };
    anyhow!("refusing to sign: {hint} ({name})")
}

/// Controllers in effect or with a change pending at `now`.
pub(crate) fn controlled(state: &ControlState, now: UtcTime) -> usize {
    state
        .controllers()
        .filter(|r| !r.capabilities_at(now).is_empty() || r.pending_at(now).is_some())
        .count()
}

/// The controller-change delay of `network` as the CLI prints it, such as "1 h".
pub(crate) fn delay_text(network: Network) -> String {
    let secs = agent_control::controller_change_delay_secs(network);
    match (secs.checked_div(3600), secs.checked_rem(3600)) {
        (Some(hours), Some(0)) => format!("{hours} h"),
        _ => format!("{secs} s"),
    }
}

/// The times of an entry the rpc accepted, read from the signed entry itself.
pub(crate) fn write_signed_times(out: &mut dyn Write, entry: &LogEntry) {
    let (signed, effective) = (entry.created_at(), entry.effective_at());
    if effective == signed {
        let _ = writeln!(out, "Signed at {signed}; it applies at once.");
    } else {
        let _ = writeln!(out, "Signed at {signed}; it takes effect at {effective}.");
    }
}

fn write_controller(out: &mut dyn Write, controller: &ControllerDescriptor, capabilities: CapabilitySet) {
    let _ = writeln!(out, "  - {} \"{}\"", controller.controller_type().as_str(), controller.label().as_str());
    let _ = writeln!(out, "      fingerprint:  {}", controller.id().fingerprint());
    let _ = writeln!(out, "      capabilities: {capabilities}");
    if let ControllerKey::WebAuthn(passkey) = controller.key() {
        let origins: Vec<&str> = passkey.origins().iter().map(|o| o.as_str()).collect();
        let _ = writeln!(out, "      rp_id:        {}", passkey.rp_id().as_str());
        let _ = writeln!(out, "      origins:      {}", origins.join(", "));
        let _ = writeln!(out, "      backup:       {}", passkey.backup_policy().as_str());
    }
}

/// The warning for a controller that may direct commands while no policy scopes it.
fn write_unscoped(out: &mut dyn Write, state: &ControlState, now: UtcTime, id: &ControllerId, capabilities: CapabilitySet) {
    if capabilities.contains(Capability::Direct) && agent_control::scope_of(state.rules_at(now).rules(), id).is_none() {
        let _ = writeln!(
            out,
            "Warning: no policy scopes {}: with direct it may send every kind of command (swap, transfer_cc, \
             transfer_token), for any pair, price and LP, until a policy scopes it.",
            id.fingerprint()
        );
    }
}

fn write_descriptor(out: &mut dyn Write, ctx: &ControlContext, descriptor: &LinkDescriptor, effective_at: UtcTime) {
    let _ = writeln!(out, "Network:       {}", descriptor.network());
    let _ = writeln!(out, "Owner account: {}", descriptor.owner_account());
    let _ = writeln!(
        out,
        "Agent:         {} (party fingerprint {})",
        descriptor.agent_party().map_or_else(|| format!("{} (invite)", ctx.party), ToString::to_string),
        party_fingerprint(&ctx.party)
    );
    let _ = writeln!(out, "Code expires:  {}", descriptor.expires_at());
    let _ = writeln!(out, "Controllers:");
    for controller in descriptor.controllers() {
        write_controller(out, controller, controller.capabilities());
    }
    let _ = writeln!(
        out,
        "They take effect at about {effective_at} ({} after the signature) and can sign nothing before.",
        delay_text(ctx.network)
    );
}

/// The descriptor of `commitment`, refused early when the rpc reports it closed.
async fn fetch_descriptor(rpc: &ControlRpc, commitment: &Commitment) -> Result<(Descriptor, LinkDescriptor)> {
    let descriptor = rpc.descriptor(commitment).await.map_err(|e| anyhow!("fetching the code's descriptor failed: {e}"))?;
    match descriptor.status {
        ControlLinkStatus::Used => bail!("the rpc reports this code as already used"),
        ControlLinkStatus::Cancelled => bail!("the rpc reports this code as cancelled by the owner"),
        ControlLinkStatus::Expired => bail!("the rpc reports this code as expired"),
        _ => {}
    }
    ensure!(
        agent_control::commitment_of(&descriptor.bytes) == *commitment,
        "refusing to sign: the descriptor the rpc returned does not match the code (commitment_mismatch)"
    );
    let decoded = LinkDescriptor::decode(&descriptor.bytes)
        .map_err(|e| anyhow!("the descriptor the rpc returned does not decode ({})", e.code_name()))?;
    Ok((descriptor, decoded))
}

/// One entry from a descriptor: built for a preview, confirmed, rebuilt at signing time.
struct Planned<'a> {
    commitment: Commitment,
    bytes: Vec<u8>,
    purpose: LinkPurpose,
    allow_invite: bool,
    rp: &'a RpPolicy,
    /// The platform parties a link pins where the env sets none.
    pins: Vec<(&'static str, Option<String>)>,
}

impl Planned<'_> {
    fn build(&self, state: &ControlState, at: UtcTime) -> Result<PendingEntry> {
        let builder = EntryBuilder::new(state);
        let built = match self.purpose {
            LinkPurpose::Link => builder.link(&self.bytes, &self.commitment, self.rp, self.allow_invite, at),
            LinkPurpose::AddController => builder.add_controller_from_code(&self.bytes, &self.commitment, self.rp, at),
        };
        built.map_err(|e| build_refusal(&e))
    }
}

/// Signs, after a typed confirmation, the entry a descriptor asks for.
async fn sign_descriptor(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    planned: Planned<'_>,
    phrase: &str,
    yes: Yes,
) -> Result<HeadRef> {
    let prepared = sync::prepare_change(ctx, rpc, out).await?;
    let now = rpc.checked_now()?;
    if planned.purpose == LinkPurpose::Link {
        let n = controlled(&prepared.state, now);
        ensure!(
            n == 0,
            "refusing to link: this agent has {n} controller(s) in effect or pending; use `cloud-agent control add` \
             (a new link needs every controller removed first)"
        );
    } else {
        ensure!(prepared.state.is_linked(), "refusing to add: this agent is not linked; use `cloud-agent control link`");
    }
    let preview = planned.build(&prepared.state, now)?;
    let decoded = LinkDescriptor::decode(&planned.bytes).map_err(|e| anyhow!("the descriptor does not decode ({})", e.code_name()))?;
    write_descriptor(out, ctx, &decoded, preview.entry().effective_at());
    for controller in decoded.controllers() {
        write_unscoped(out, &prepared.state, now, controller.id(), controller.capabilities());
    }
    super::pins::write_offer(out, &planned.pins);
    let (state, _) = prepared.release();
    confirm_typed(prompter, out, phrase, yes).await?;
    let (lock, state) = sync::relock(ctx, state.head()).await?;
    let created_at = rpc.checked_now()?;
    let entry = planned.build(&state, created_at)?;
    ensure!(entry.entry().body() == preview.entry().body(), "the change differs from the one confirmed: run the command again");
    let head = sync::sign_and_upload(ctx, rpc, lock, &entry, out).await?;
    write_signed_times(out, entry.entry());
    if let Some(env_file) = ctx.env_file.as_deref() {
        super::pins::write_after_link(out, env_file, &planned.pins);
    }
    Ok(head)
}

/// `control link --code LNK-…`: the first controllers, from a code naming this agent.
pub async fn run_link(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    code: &str,
    yes: bool,
) -> Result<HeadRef> {
    if code.trim().to_ascii_uppercase().starts_with("AGT-") {
        bail!("an AGT- invite code links only during `cloud-agent onboard`; ask the owner for a link code (LNK-…)");
    }
    let commitment = parse_link_code(code.trim()).map_err(|e| anyhow!("not a link code (LNK-…): {}", e.code_name()))?;
    ctx.require_env_file()?;
    let (descriptor, decoded) = fetch_descriptor(rpc, &commitment).await?;
    ensure!(decoded.purpose() == LinkPurpose::Link, "this code adds a controller: use `cloud-agent control add --code`");
    ensure!(
        decoded.agent_party() == Some(&ctx.party),
        "refusing to link: the code names {} instead of this agent",
        decoded.agent_party().map_or_else(|| "no agent (an invite)".to_string(), ToString::to_string)
    );
    let rp = RpPolicy::for_network(ctx.network);
    let pins = super::pins::for_link(ctx.require_env_file()?, &ctx.config.orderbook_grpc_url).await;
    let planned = Planned { commitment, bytes: descriptor.bytes, purpose: LinkPurpose::Link, allow_invite: false, rp: &rp, pins };
    let head = sign_descriptor(ctx, rpc, prompter, out, planned, "link", yes_of(yes)).await?;
    let _ = writeln!(
        out,
        "Linked at seq {}. Party fingerprint (the Agent page shows the same): {}",
        head.seq,
        party_fingerprint(&ctx.party)
    );
    Ok(head)
}

/// Where an added controller comes from.
pub enum AddSource<'a> {
    Code(&'a str),
    Ed25519 { key: &'a str, label: &'a str, capabilities: &'a str },
}

/// A capability list such as `policy,read`, in any order.
pub fn parse_capabilities(text: &str) -> Result<CapabilitySet> {
    let mut set = CapabilitySet::EMPTY;
    for part in text.split(',').map(str::trim) {
        let capability = agent_control::Capability::ALL
            .into_iter()
            .find(|c| c.as_str() == part)
            .ok_or_else(|| anyhow!("unknown capability {part:?}: use policy, direct or read"))?;
        ensure!(!set.contains(capability), "capability {part} is listed twice");
        set = set.with(capability);
    }
    ensure!(!set.is_empty(), "no capability given");
    Ok(set)
}

/// A 32-byte Ed25519 public key as 64 hex characters or base58.
pub fn parse_ed25519_key(text: &str) -> Result<[u8; 32]> {
    let text = text.trim();
    let bytes = if text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(text).map_err(|_| anyhow!("the key is not hex"))?
    } else {
        bs58::decode(text).into_vec().map_err(|_| anyhow!("the key is neither 64 hex characters nor base58"))?
    };
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow!("an Ed25519 public key is 32 bytes"))?;
    agent_control::parse_ed25519_public_key(&key).map_err(|e| anyhow!("not a usable Ed25519 key ({})", e.code_name()))?;
    Ok(key)
}

/// `control add`: one controller from an add code, or a local Ed25519 key.
pub async fn run_add(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    source: AddSource<'_>,
    yes: bool,
) -> Result<HeadRef> {
    ctx.require_env_file()?;
    match source {
        AddSource::Code(code) => {
            let commitment = parse_link_code(code.trim()).map_err(|e| anyhow!("not a link code (LNK-…): {}", e.code_name()))?;
            let (descriptor, decoded) = fetch_descriptor(rpc, &commitment).await?;
            ensure!(decoded.purpose() == LinkPurpose::AddController, "this code links an agent: use `cloud-agent control link --code`");
            let rp = RpPolicy::for_network(ctx.network);
            let planned = Planned { commitment, bytes: descriptor.bytes, purpose: LinkPurpose::AddController, allow_invite: false, rp: &rp, pins: Vec::new() };
            let head = sign_descriptor(ctx, rpc, prompter, out, planned, "add", yes_of(yes)).await?;
            let _ = writeln!(out, "Controller added at seq {}.", head.seq);
            Ok(head)
        }
        AddSource::Ed25519 { key, label, capabilities } => {
            let key = parse_ed25519_key(key)?;
            let label = Label::parse(label).map_err(|_| anyhow!("invalid label: 1-64 of A-Z a-z 0-9 space . _ -, starting with a letter or digit"))?;
            let capabilities = parse_capabilities(capabilities)?;
            let build = |state: &ControlState, at: UtcTime| {
                EntryBuilder::new(state).add_ed25519(key, label.clone(), capabilities, at).map_err(|e| build_refusal(&e))
            };
            let prepared = sync::prepare_change(ctx, rpc, out).await?;
            ensure!(prepared.state.is_linked(), "refusing to add: this agent is not linked; use `cloud-agent control link`");
            let now = rpc.checked_now()?;
            let preview = build(&prepared.state, now)?;
            let id = controller_id_for(ControllerType::Ed25519, &key);
            let _ = writeln!(out, "Add an Ed25519 controller to {} on {}:", ctx.party, ctx.network);
            let _ = writeln!(out, "  label:        {}", label.as_str());
            let _ = writeln!(out, "  fingerprint:  {}", id.fingerprint());
            let _ = writeln!(out, "  capabilities: {capabilities}");
            let _ = writeln!(
                out,
                "It takes effect at about {} ({} after the signature).",
                preview.entry().effective_at(),
                delay_text(ctx.network)
            );
            write_unscoped(out, &prepared.state, now, &id, capabilities);
            let (state, _) = prepared.release();
            confirm_typed(prompter, out, "add", yes_of(yes)).await?;
            let (lock, state) = sync::relock(ctx, state.head()).await?;
            let entry = build(&state, rpc.checked_now()?)?;
            let head = sync::sign_and_upload(ctx, rpc, lock, &entry, out).await?;
            write_signed_times(out, entry.entry());
            let _ = writeln!(out, "Controller {} added at seq {}.", id.fingerprint(), head.seq);
            Ok(head)
        }
    }
}

/// Onboarding with `AGT-<invite>-<commitment>`: links into an empty log, only for the
/// invite this agent registered with; without a terminal it prints what to run.
pub async fn run_invite_link(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    invite: &str,
    commitment: Commitment,
) -> Result<Option<HeadRef>> {
    if !prompter.interactive() {
        let _ = writeln!(
            out,
            "This invite also links the agent to its owner's controllers, which needs a typed confirmation. \
             No terminal is attached, so nothing was linked. Ask the owner for a link code for party {} \
             (fingerprint {}) and run:\n  cloud-agent control link --code LNK-<code>",
            ctx.party,
            party_fingerprint(&ctx.party)
        );
        return Ok(None);
    }
    ctx.require_env_file()?;
    let (descriptor, decoded) = fetch_descriptor(rpc, &commitment).await?;
    ensure!(decoded.purpose() == LinkPurpose::Link, "the invite's descriptor is not a link");
    match decoded.agent_party() {
        Some(party) => ensure!(party == &ctx.party, "refusing to link: the invite's descriptor names another agent"),
        None => ensure!(
            descriptor.invite_code.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(invite)),
            "refusing to link: the descriptor is not for the invite this agent registered with"
        ),
    }
    let rp = RpPolicy::for_network(ctx.network);
    let pins = super::pins::for_link(ctx.require_env_file()?, &ctx.config.orderbook_grpc_url).await;
    let planned = Planned { commitment, bytes: descriptor.bytes, purpose: LinkPurpose::Link, allow_invite: true, rp: &rp, pins };
    let _ = writeln!(out, "\nLink this agent to the owner who invited it:");
    let head = sign_descriptor(ctx, rpc, prompter, out, planned, "link", Yes::NotGiven).await?;
    let _ = writeln!(
        out,
        "Linked at seq {}. On the Agent page, confirm this agent by checking its party fingerprint: {}",
        head.seq,
        party_fingerprint(&ctx.party)
    );
    Ok(Some(head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_party_fingerprint_is_the_20_hex_after_1220_in_groups_of_4() {
        let namespace = format!("1220abcdef0123456789fedcba98{}", "0".repeat(40));
        let party = PartyId::parse(&format!("agent-1::{namespace}")).unwrap();
        assert_eq!(party_fingerprint(&party), "ABCD EF01 2345 6789 FEDC");
        // The rule, for a party of a real key.
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]).verifying_key().to_bytes();
        let namespace = agent_control::namespace_for_key(&key);
        let party = PartyId::parse(&format!("agent-1::{namespace}")).unwrap();
        let expected = namespace[4..24].to_ascii_uppercase();
        let shown = party_fingerprint(&party);
        assert_eq!(shown.len(), 24);
        assert_eq!(shown.split(' ').collect::<Vec<_>>(), expected.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>());
    }

    #[test]
    fn the_delay_text_follows_the_network() {
        assert_eq!(delay_text(Network::Devnet), "1 h");
        assert_eq!(delay_text(Network::Mainnet), "24 h");
    }
}
