//! The online controller commands: resolve the agent, key, network and rpc; sign in; verify
//! the log here; then read, or draft, confirm, sign and submit.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_control::policy::{Allowlist, PolicyInstrument};
use agent_control::{
    Amount, CommandKind, CommandOp, CommandScope, ControllerId, Hash32, InstrumentId,
    InstrumentRef, MAX_PACKAGES, Network, PackageId, PartyId, PolicyDocument, ReceiptOutcome, RpId,
    Rules, SCHEME_POLICY, ScopePair, Text, UtcTime,
};
use agent_controller::{
    CALL_TIMEOUT, ControllerClient, ControllerConfig, ControllerError, ControllerSigner,
    PolicyBase, ReceiptCheck, SeenHeads, VerifiedAgent,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use orderbook_proto::orderbook::{ControlCommandStatus, ControlPolicyStatus};

use super::super::prompt::{Prompter, Yes, confirm_typed};
use super::{
    CommandCommands, CommandOptions, ControllerCommands, ControllerOptions, OnlineArgs,
    PolicyCommands, PolicyEdits, WaitBounds, render, still_pending,
};

/// Largest rules file read.
const MAX_RULES_FILE_BYTES: u64 = 70_000;

/// The agent, key, network and rpc one online command works with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub network: Network,
    pub agent: PartyId,
    pub key_file: PathBuf,
    pub rpc_url: String,
    pub rpc_host: RpId,
}

fn parse_network(text: &str, source: &str) -> Result<Network> {
    match text.trim() {
        "devnet" => Ok(Network::Devnet),
        "mainnet" => Ok(Network::Mainnet),
        other => bail!("{source} = {other:?}: a controller works on devnet or mainnet"),
    }
}

/// An rpc URL and the TLS host its challenges must name: https, or http on loopback only,
/// where challenges name `localhost`.
pub fn rpc_of(url: &str) -> Result<(String, RpId)> {
    let uri: tonic::codegen::http::Uri = url
        .trim()
        .parse()
        .map_err(|_| anyhow!("the rpc {url:?} is not a URL"))?;
    ensure!(!url.contains('@'), "the rpc URL must not carry credentials");
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("the rpc {url:?} names no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    match uri.scheme_str() {
        Some("https") => {}
        Some("http") if loopback => {}
        _ => bail!("the rpc must be an https URL (plain http only on loopback): {url:?}"),
    }
    ensure!(
        matches!(uri.path(), "" | "/") && uri.query().is_none(),
        "the rpc URL must be just the scheme, host and port"
    );
    let rpc_host = if loopback {
        RpId::parse(RpId::LOCALHOST)
    } else {
        RpId::parse(&host)
    }
    .map_err(|_| anyhow!("the rpc host {host:?} is not a DNS name"))?;
    Ok((url.trim().trim_end_matches('/').to_string(), rpc_host))
}

/// The target from the flags, then [controller] of agent.toml, then CANTON_CHAIN.
pub fn resolve(options: &ControllerOptions, online: &OnlineArgs) -> Result<Target> {
    let section = agent_logic::config::load_controller_section(&options.config)?;
    let agent_text = match (&online.agent, &section) {
        (Some(agent), _) => agent.trim().to_string(),
        (None, Some(section)) => section.agent_party.to_string(),
        (None, None) => bail!(
            "no agent: give --agent <party>, or a [controller] section in {}",
            options.config.display()
        ),
    };
    let agent = PartyId::parse(&agent_text)
        .map_err(|_| anyhow!("the agent {agent_text:?} is not a party id"))?;
    if options.local_party.as_deref().map(str::trim) == Some(agent.as_str()) {
        bail!(
            "refusing: {agent} is this machine's own agent (PARTY_AGENT); a controller key controls an agent from another machine"
        );
    }
    let key_file = match (&online.key, &section) {
        (Some(key), _) => key.clone(),
        (None, Some(section)) => section.key_file.clone(),
        (None, None) => bail!("no key: give --key <file>, or key_file in [controller]"),
    };
    let network = match (
        &online.network,
        section.as_ref().and_then(|s| s.network),
        &options.chain,
    ) {
        (Some(text), _, _) => parse_network(text, "--network")?,
        (None, Some(network), _) => network,
        (None, None, Some(chain)) if !chain.trim().is_empty() => {
            parse_network(chain, "CANTON_CHAIN")?
        }
        _ => bail!(
            "no network: give --network devnet|mainnet, network in [controller], or CANTON_CHAIN"
        ),
    };
    let (rpc_url, rpc_host) = match &online.rpc {
        Some(url) => rpc_of(url)?,
        None => rpc_of(crate::network_rpc_url(network.as_str())?)?,
    };
    Ok(Target {
        network,
        agent,
        key_file,
        rpc_url,
        rpc_host,
    })
}

/// A client of the target, over a fresh channel.
pub async fn connect(target: &Target) -> Result<ControllerClient> {
    let signer = ControllerSigner::load(&target.key_file).map_err(|e| anyhow!("{e}"))?;
    let opts = agent_logic::transport::ChannelOpts::default();
    let channel = match tokio::time::timeout(
        CALL_TIMEOUT,
        agent_logic::transport::connect_channel(&target.rpc_url, opts),
    )
    .await
    {
        Ok(Ok(channel)) => channel,
        Ok(Err(e)) => return Err(ControllerError::Unreachable(format!("{e:#}")).into()),
        Err(_) => {
            return Err(ControllerError::Unreachable(format!(
                "no connection within {CALL_TIMEOUT:?}"
            ))
            .into());
        }
    };
    let config = ControllerConfig {
        network: target.network,
        rpc_host: target.rpc_host.clone(),
        agent_party: target.agent.clone(),
    };
    Ok(ControllerClient::new(
        channel,
        config,
        signer,
        SeenHeads::beside(&target.key_file),
    ))
}

fn online_args(command: &ControllerCommands) -> Option<&OnlineArgs> {
    match command {
        ControllerCommands::Keygen { .. } | ControllerCommands::Sign { .. } => None,
        ControllerCommands::Status(online)
        | ControllerCommands::Log(online)
        | ControllerCommands::Holdings(online)
        | ControllerCommands::Traffic(online)
        | ControllerCommands::Activity { online, .. }
        | ControllerCommands::Ledger { online, .. }
        | ControllerCommands::Packages { online, .. }
        | ControllerCommands::Commands { online, .. } => Some(online),
        ControllerCommands::Policy { command } => match command {
            PolicyCommands::Show { online, .. } | PolicyCommands::Set { online, .. } => {
                Some(online)
            }
        },
        ControllerCommands::Command { command } => match command {
            CommandCommands::TransferCc { online, .. }
            | CommandCommands::TransferToken { online, .. }
            | CommandCommands::Swap { online, .. }
            | CommandCommands::Show { online, .. } => Some(online),
        },
    }
}

/// Runs one online command; the session is ended at the end, whatever happened.
pub async fn run(
    options: &ControllerOptions,
    command: ControllerCommands,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    let online =
        online_args(&command).ok_or_else(|| anyhow!("not an online controller command"))?;
    let target = resolve(options, online)?;
    let client = connect(&target)
        .await
        .map_err(|e| connect_error(e, &command))?;
    let result = dispatch(&client, options, command, prompter, out).await;
    let _ = client.end().await;
    result
}

/// A `command show --wait` that reached no rpc: its outcome is not known (exit 3).
fn connect_error(e: anyhow::Error, command: &ControllerCommands) -> anyhow::Error {
    let lost = e.downcast_ref::<ControllerError>().is_some_and(unanswered);
    match command {
        ControllerCommands::Command {
            command: CommandCommands::Show {
                code, wait: true, ..
            },
        } if lost => show_unknown(e, code),
        _ => e,
    }
}

async fn dispatch(
    client: &ControllerClient,
    options: &ControllerOptions,
    command: ControllerCommands,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    match command {
        ControllerCommands::Keygen { .. } | ControllerCommands::Sign { .. } => {
            bail!("not an online controller command")
        }
        ControllerCommands::Status(online) => {
            let agent = client.agent().await?;
            render::status(out, &agent, &client.signer().fingerprint(), online.json);
            agent.ensure_signing_allowed()?;
            Ok(())
        }
        ControllerCommands::Log(online) => {
            let agent = client.agent().await?;
            render::log(out, &agent, online.json);
            Ok(())
        }
        ControllerCommands::Holdings(online) => {
            render::holdings(out, &client.holdings().await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Traffic(online) => {
            render::traffic(out, &client.traffic().await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Activity {
            online,
            limit,
            offset,
        } => {
            render::activity(out, &client.activity(limit, offset).await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Ledger {
            online,
            limit,
            offset,
        } => {
            render::ledger(out, &client.ledger(limit, offset).await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Packages { online, removed } => {
            render::packages(out, &client.packages(removed).await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Commands {
            online,
            limit,
            offset,
        } => {
            render::commands(out, &client.commands(limit, offset).await?.0, online.json);
            Ok(())
        }
        ControllerCommands::Policy {
            command: PolicyCommands::Show { online, rules },
        } => {
            let agent = client.agent().await?;
            if rules {
                let text = rule_lines(agent.state().rules_at(agent.checked_at()).rules(), &agent)?;
                let _ = write!(out, "{text}");
            } else {
                render::policy_show(out, &agent, online.json);
            }
            Ok(())
        }
        ControllerCommands::Policy {
            command:
                PolicyCommands::Set {
                    online,
                    edits,
                    base,
                    validity,
                    yes,
                    wait,
                },
        } => {
            policy_set(
                client,
                options,
                &edits,
                &base,
                validity,
                yes,
                wait,
                online.json,
                prompter,
                out,
            )
            .await
        }
        ControllerCommands::Command { command } => {
            command_run(client, options, command, prompter, out).await
        }
    }
}

// ---- policies ----

/// The rule lines of `rules`, as `policy set --rules` reads them.
pub fn rule_lines(rules: &Rules, agent: &VerifiedAgent) -> Result<String> {
    let at = agent.checked_at();
    let document = PolicyDocument {
        network: agent.state().network(),
        agent_party: agent.state().agent_party().clone(),
        version: 1,
        prev_policy_hash: Hash32([0; 32]),
        issued_at: at,
        expires_at: at
            .checked_add_secs(60)
            .ok_or_else(|| anyhow!("time out of range"))?,
        not_before: None,
        rules: rules.clone(),
    };
    let bytes = document
        .encode()
        .map_err(|e| anyhow!("the rules do not encode ({})", e.code_name()))?;
    let text = String::from_utf8(bytes).map_err(|_| anyhow!("the rules are not text"))?;
    let lines: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("paused="))
        .collect();
    Ok(format!("{}\n", lines.join("\n")))
}

/// The rules of a rules file: a policy document of this network, or rule lines.
pub fn rules_from_file(path: &Path, agent: &VerifiedAgent) -> Result<Rules> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("read {}", path.display()))?
        .len();
    ensure!(
        size <= MAX_RULES_FILE_BYTES,
        "{} is too large for a rules file",
        path.display()
    );
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let state = agent.state();
    if text.starts_with("scheme=") {
        let document = PolicyDocument::decode(text.as_bytes()).map_err(|e| {
            anyhow!(
                "{} is not a policy document ({})",
                path.display(),
                e.code_name()
            )
        })?;
        ensure!(
            document.network == state.network(),
            "{} is a policy for {}, not {}",
            path.display(),
            document.network,
            state.network()
        );
        return Ok(document.rules);
    }
    let at = agent.checked_at();
    let mut canonical = format!(
        "scheme={SCHEME_POLICY}\nnetwork={}\nagent_party={}\nversion=1\nprev_policy_hash={}\nissued_at={at}\nexpires_at={}\nnot_before=\n",
        state.network(),
        state.agent_party(),
        Hash32([0; 32]),
        at.checked_add_secs(60)
            .ok_or_else(|| anyhow!("time out of range"))?
    );
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        canonical.push_str(line);
        canonical.push('\n');
    }
    PolicyDocument::decode(canonical.as_bytes())
        .map(|document| document.rules)
        .map_err(|e| {
            anyhow!(
                "{} does not hold valid rule lines ({})",
                path.display(),
                e.code_name()
            )
        })
}

fn plain_amount(text: &str, what: &str) -> Result<Amount> {
    let value = rust_decimal::Decimal::from_str_exact(text.trim())
        .map_err(|_| anyhow!("{what} {text:?} is not a plain decimal"))?;
    Amount::from_decimal(value).map_err(|_| {
        anyhow!(
            "{what} {text:?} must be at least 0, with at most 17 integer and 10 fraction digits"
        )
    })
}

fn positive_amount(text: &str, what: &str) -> Result<Amount> {
    let amount = plain_amount(text, what)?;
    ensure!(!amount.is_zero(), "{what} must be above 0");
    Ok(amount)
}

fn parse_packages(text: &str) -> Result<Allowlist<PackageId>> {
    if text.trim() == "any" {
        return Ok(Allowlist {
            any: true,
            items: Vec::new(),
        });
    }
    let mut ids = BTreeSet::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let id = PackageId::parse(part)
            .map_err(|_| anyhow!("{part:?} is not a package id (64 lowercase hex characters)"))?;
        ensure!(ids.insert(id), "package {part} is listed twice");
    }
    ensure!(
        !ids.is_empty() && ids.len() <= MAX_PACKAGES,
        "--packages takes `any` or 1 to {MAX_PACKAGES} package ids"
    );
    Ok(Allowlist {
        any: false,
        items: ids.into_iter().collect(),
    })
}

fn parse_kinds(text: &str) -> Result<Vec<CommandKind>> {
    if text.trim() == "none" {
        return Ok(Vec::new());
    }
    let mut kinds = BTreeSet::new();
    for part in text.split(',').map(str::trim) {
        kinds.insert(CommandKind::parse(part).map_err(|_| {
            anyhow!(
                "{part:?} is not a command kind: swap, transfer_cc, transfer_token or usdcx_burn"
            )
        })?);
    }
    Ok(kinds.into_iter().collect())
}

fn policy_instrument(text: &str) -> Result<PolicyInstrument> {
    let text = text.trim();
    if text == PolicyInstrument::CC_ALIAS {
        return Ok(PolicyInstrument::Cc);
    }
    let (id, admin) = text
        .split_once('@')
        .ok_or_else(|| anyhow!("{text:?} is not an instrument: give `cc` or `ID@ADMIN`"))?;
    let admin = PartyId::parse(admin).map_err(|_| anyhow!("{admin:?} is not a party id"))?;
    let id = InstrumentId::parse(id).map_err(|_| anyhow!("{id:?} is not an instrument id"))?;
    Ok(PolicyInstrument::Token(InstrumentRef { admin, id }))
}

fn parse_pairs(texts: &[String]) -> Result<Vec<ScopePair>> {
    let mut pairs: Vec<ScopePair> = Vec::new();
    for text in texts {
        let parts: Vec<&str> = text.split(',').collect();
        let [sell, buy, min_price, max_sell] = parts.as_slice() else {
            bail!("--scope-pair {text:?} must be SELL,BUY,MIN_PRICE,MAX_SELL");
        };
        let pair = ScopePair {
            sell: policy_instrument(sell)?,
            buy: policy_instrument(buy)?,
            min_price: positive_amount(min_price, "the pair's min_price")?,
            max_sell: positive_amount(max_sell, "the pair's max_sell")?,
        };
        ensure!(
            pair.sell != pair.buy,
            "--scope-pair {text:?} sells and buys the same instrument"
        );
        ensure!(
            !pairs
                .iter()
                .any(|p| p.sell == pair.sell && p.buy == pair.buy),
            "--scope-pair {text:?} names its pair twice"
        );
        pairs.push(pair);
    }
    pairs.sort_by(|a, b| (&a.sell, &a.buy).cmp(&(&b.sell, &b.buy)));
    Ok(pairs)
}

fn parse_lps(texts: &[String]) -> Result<Vec<PartyId>> {
    let mut lps = BTreeSet::new();
    for text in texts {
        lps.insert(
            PartyId::parse(text.trim())
                .map_err(|_| anyhow!("--scope-lp {text:?} is not a party id"))?,
        );
    }
    Ok(lps.into_iter().collect())
}

fn normalized_fingerprint(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// A recorded controller of the agent, by id or fingerprint.
fn resolve_controller(text: &str, agent: &VerifiedAgent, rules: &Rules) -> Result<ControllerId> {
    let state = agent.state();
    if let Ok(id) = ControllerId::parse(text.trim()) {
        ensure!(
            state.is_recorded_controller(&id),
            "{text} is not a controller of this agent"
        );
        return Ok(id);
    }
    let wanted = normalized_fingerprint(text);
    let mut known: Vec<ControllerId> = state.controllers().map(|record| *record.id()).collect();
    known.extend(rules.scopes.iter().map(|scope| scope.controller));
    known.sort();
    known.dedup();
    let matches: Vec<ControllerId> = known
        .into_iter()
        .filter(|id| normalized_fingerprint(&id.fingerprint()) == wanted)
        .collect();
    match matches.as_slice() {
        [id] => Ok(*id),
        [] => bail!("no controller of this agent has the fingerprint {text:?}"),
        _ => bail!("the fingerprint {text:?} is ambiguous: give the controller id"),
    }
}

fn edit_scope(
    rules: &mut Rules,
    edits: &PolicyEdits,
    controller: &str,
    agent: &VerifiedAgent,
) -> Result<()> {
    let id = resolve_controller(controller, agent, rules)?;
    let existing = rules
        .scopes
        .iter()
        .find(|scope| scope.controller == id)
        .cloned();
    rules.scopes.retain(|scope| scope.controller != id);
    if edits.scope_remove {
        ensure!(
            existing.is_some(),
            "controller {} has no scope to drop",
            id.fingerprint()
        );
        return Ok(());
    }
    let changes = edits.scope_commands.is_some()
        || !edits.scope_pairs.is_empty()
        || edits.scope_any_pair
        || !edits.scope_lps.is_empty()
        || edits.scope_any_lp;
    ensure!(
        changes,
        "--scope needs --scope-commands, --scope-pair, --scope-lp, an --scope-any-* flag or --scope-remove"
    );
    let mut scope = existing.unwrap_or(CommandScope {
        controller: id,
        commands: CommandKind::ALL.to_vec(),
        pairs: Vec::new(),
        lps: Vec::new(),
    });
    if let Some(kinds) = &edits.scope_commands {
        scope.commands = parse_kinds(kinds)?;
    }
    if edits.scope_any_pair {
        scope.pairs.clear();
    } else if !edits.scope_pairs.is_empty() {
        scope.pairs = parse_pairs(&edits.scope_pairs)?;
    }
    if edits.scope_any_lp {
        scope.lps.clear();
    } else if !edits.scope_lps.is_empty() {
        scope.lps = parse_lps(&edits.scope_lps)?;
    }
    rules.scopes.push(scope);
    rules.scopes.sort_by_key(|scope| scope.controller);
    Ok(())
}

/// Applies the edits to the rules a draft starts from.
pub fn apply_edits(rules: &mut Rules, edits: &PolicyEdits, agent: &VerifiedAgent) -> Result<()> {
    if let Some(path) = &edits.rules {
        *rules = rules_from_file(path, agent)?;
        return Ok(());
    }
    if edits.pause {
        rules.paused = true;
    }
    if edits.resume {
        rules.paused = false;
    }
    if let Some(transfers) = &edits.transfers {
        rules.transfers.enabled = transfers == "on";
    }
    if let Some(packages) = &edits.packages {
        rules.packages = parse_packages(packages)?;
    }
    if let Some(controller) = &edits.scope {
        edit_scope(rules, edits, controller, agent)?;
    }
    Ok(())
}

/// Seconds until `at`, at least one.
fn secs_until(at: UtcTime, now: UtcTime) -> Duration {
    Duration::from_secs(u64::try_from(now.secs_until(at)).unwrap_or(0).max(1))
}

/// How long a wait goes on: until `grace_secs` after `expires_at`, at most `bounds.max`.
fn wait_bound(expires_at: UtcTime, now: UtcTime, grace_secs: i64, bounds: WaitBounds) -> Duration {
    let until = expires_at
        .checked_add_secs(grace_secs)
        .unwrap_or(expires_at);
    secs_until(until, now).min(bounds.max)
}

/// A submission's error: exit 3 when the rpc may hold it, so that a re-run does not send it twice.
fn submit_error(e: ControllerError, what: &str, check: &str) -> anyhow::Error {
    if e.is_unconfirmed() {
        still_pending(format!(
            "{what} may have been submitted ({e}); check it with `cloud-agent controller {check}` before signing it again"
        ))
    } else {
        anyhow::Error::new(e)
    }
}

/// An error once the rpc stored a submission: its outcome is not known (exit 3).
fn outcome_unknown(e: impl std::fmt::Display, what: &str, check: &str) -> anyhow::Error {
    still_pending(format!(
        "{what} was submitted, but its outcome is not known ({e}); check it with `cloud-agent controller {check}`"
    ))
}

/// A wait on a shown command that read no outcome (exit 3).
fn show_unknown(e: impl std::fmt::Display, code: &str) -> anyhow::Error {
    still_pending(format!(
        "the outcome of {code} is not known ({e}); check it again with `cloud-agent controller command show --code {code}` before signing it again"
    ))
}

/// No answer from the rpc: unreachable, or a status without a refusal code.
fn unanswered(e: &ControllerError) -> bool {
    matches!(
        e,
        ControllerError::Unreachable(_) | ControllerError::Refused { code: None, .. }
    )
}

#[allow(clippy::too_many_arguments)]
async fn policy_set(
    client: &ControllerClient,
    options: &ControllerOptions,
    edits: &PolicyEdits,
    base: &str,
    validity: u64,
    yes: bool,
    wait: bool,
    json: bool,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    ensure!(
        !edits.is_empty(),
        "nothing to change: give --pause, --resume, --transfers, --packages, --scope or --rules"
    );
    let agent = client.agent().await?;
    agent.ensure_signing_allowed()?;
    let now = client.checked_now()?;
    let base = if base == "scheduled" {
        PolicyBase::Scheduled
    } else {
        PolicyBase::InForce
    };
    let mut rules = agent.base_rules(base, now)?;
    apply_edits(&mut rules, edits, &agent)?;
    let draft = client.draft_policy(&agent, rules, validity)?;
    render::policy_draft(out, &agent, &draft);
    if options.dry_run {
        let _ = writeln!(
            out,
            "Dry run: nothing was signed or sent. The document (base64url):"
        );
        let _ = writeln!(out, "{}", URL_SAFE_NO_PAD.encode(&draft.bytes));
        return Ok(());
    }
    let confirm = match (yes, draft.widens()) {
        (true, false) => Yes::Given,
        (true, true) => bail!(
            "--yes is refused: this policy {}; type the confirmation instead",
            super::super::policy_accept::widening_text(&draft.widening)
        ),
        (false, false) => Yes::NotGiven,
        (false, true) => Yes::Refused,
    };
    confirm_typed(prompter, out, &draft.confirmation(), confirm).await?;
    let what = format!("policy v{} ({})", draft.document.version, draft.hash);
    let check = "policy show";
    let submitted = client
        .submit_policy(&agent, &draft)
        .await
        .map_err(|e| submit_error(e, &what, check))?;
    let _ = writeln!(
        out,
        "Submitted policy v{} ({}). The agent accepts it at its next poll; once accepted it is in force from {}.",
        submitted.version, submitted.hash, submitted.effective_at
    );
    if !wait {
        return Ok(());
    }
    let max = wait_bound(
        draft.document.expires_at,
        client.checked_now().unwrap_or(now),
        0,
        options.wait,
    );
    let (view, finished) = client
        .wait_policy(&submitted.hash, max)
        .await
        .map_err(|e| outcome_unknown(e, &what, check))?;
    let status = render::policy_status(view.0.status);
    let accepted = finished && view.0.status == ControlPolicyStatus::Accepted as i32;
    // The rpc's word is not enough: the verified log must hold the agent's acceptance.
    let held = if accepted {
        let verified = client
            .agent()
            .await
            .map_err(|e| outcome_unknown(e, &what, check))?;
        verified
            .ensure_signing_allowed()
            .map_err(|e| outcome_unknown(e, &what, check))?;
        verified.holds_accepted_policy(&submitted.hash)
    } else {
        false
    };
    if json {
        let _ = writeln!(
            out,
            "{}",
            serde_json::json!({
                "policyHash": submitted.hash.to_string(),
                "status": status,
                "acceptedInVerifiedLog": held,
            })
        );
    } else {
        let _ = writeln!(
            out,
            "The policy is {status} ({}).",
            agent_controller::Reported::<()>::LABEL
        );
        if held {
            let _ = writeln!(out, "The verified log holds its acceptance.");
        }
    }
    if !finished {
        return Err(still_pending(format!(
            "policy v{} is still pending",
            submitted.version
        )));
    }
    ensure!(accepted, "policy v{} was {status}", submitted.version);
    if !held {
        return Err(still_pending(format!(
            "the rpc reports policy v{} accepted, but the verified log does not hold its acceptance; check it with `cloud-agent controller {check}`",
            submitted.version
        )));
    }
    Ok(())
}

// ---- commands ----

fn party(text: &str, what: &str) -> Result<PartyId> {
    PartyId::parse(text.trim()).map_err(|_| anyhow!("{what} {text:?} is not a party id"))
}

fn instrument(admin: &str, id: &str) -> Result<InstrumentRef> {
    Ok(InstrumentRef {
        admin: party(admin, "the instrument admin")?,
        id: InstrumentId::parse(id.trim())
            .map_err(|_| anyhow!("{id:?} is not an instrument id"))?,
    })
}

fn text_140(text: Option<&String>, what: &str) -> Result<Option<Text<140>>> {
    text.map(|t| {
        Text::<140>::parse(t).map_err(|_| {
            anyhow!("{what} must be 1 to 140 printable ASCII characters, single-spaced")
        })
    })
    .transpose()
}

fn operation(command: &CommandCommands) -> Result<CommandOp> {
    Ok(match command {
        CommandCommands::TransferCc {
            to,
            amount,
            description,
            ..
        } => CommandOp::TransferCc {
            receiver: party(to, "--to")?,
            amount: positive_amount(amount, "--amount")?,
            description: text_140(description.as_ref(), "--description")?,
            settlement_proposal_id: None,
        },
        CommandCommands::TransferToken {
            to,
            instrument_admin,
            instrument_id,
            amount,
            reference,
            ..
        } => CommandOp::TransferToken {
            receiver: party(to, "--to")?,
            instrument: instrument(instrument_admin, instrument_id)?,
            amount: positive_amount(amount, "--amount")?,
            reference: text_140(reference.as_ref(), "--reference")?,
        },
        CommandCommands::Swap {
            sell_admin,
            sell_id,
            sell_amount,
            buy_admin,
            buy_id,
            min_receive,
            max_fee_cc,
            ..
        } => {
            let sell = instrument(sell_admin, sell_id)?;
            let buy = instrument(buy_admin, buy_id)?;
            ensure!(sell != buy, "a swap sells and buys different instruments");
            CommandOp::Swap {
                sell,
                sell_amount: positive_amount(sell_amount, "--sell-amount")?,
                buy,
                min_receive: positive_amount(min_receive, "--min-receive")?,
                max_fee_cc: plain_amount(max_fee_cc, "--max-fee-cc")?,
            }
        }
        CommandCommands::Show { .. } => bail!("not a command to sign"),
    })
}

async fn command_run(
    client: &ControllerClient,
    options: &ControllerOptions,
    command: CommandCommands,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    let (online, settings): (&OnlineArgs, &CommandOptions) = match &command {
        CommandCommands::Show { online, code, wait } => {
            return command_show(client, code, *wait, online.json, options.wait, out).await;
        }
        CommandCommands::TransferCc {
            online, options, ..
        }
        | CommandCommands::TransferToken {
            online, options, ..
        }
        | CommandCommands::Swap {
            online, options, ..
        } => (online, options),
    };
    let op = operation(&command)?;
    let agent = client.agent().await?;
    agent.ensure_signing_allowed()?;
    let draft = client.draft_command(&agent, op, settings.summary.as_deref(), settings.validity)?;
    let code = draft.code();
    render::command_draft(out, &draft.command, &code);
    if options.dry_run {
        let _ = writeln!(
            out,
            "Dry run: nothing was signed or sent. The command (base64url):"
        );
        let _ = writeln!(out, "{}", URL_SAFE_NO_PAD.encode(&draft.bytes));
        return Ok(());
    }
    confirm_typed(prompter, out, &draft.confirmation(), Yes::Refused).await?;
    let check = format!("command show --code {code}");
    let submitted = client
        .submit_command(&agent, &draft)
        .await
        .map_err(|e| submit_error(e, &code, &check))?;
    if online.json {
        let _ = writeln!(
            out,
            "{}",
            serde_json::json!({ "code": code, "status": render::command_status(submitted.view.0.status) })
        );
    } else {
        let _ = writeln!(
            out,
            "Submitted {code}. The agent runs it at its next poll when auto_execute_commands is on;"
        );
        let _ = writeln!(
            out,
            "otherwise its operator runs: cloud-agent control execute --code {code}"
        );
    }
    if !settings.wait {
        return Ok(());
    }
    let now = client
        .checked_now()
        .map_err(|e| outcome_unknown(e, &code, &check))?;
    let max = wait_bound(
        submitted.command.expires_at,
        now,
        options.wait.grace_secs,
        options.wait,
    );
    let waited = client
        .wait(&agent, &submitted.command.command_id, max)
        .await
        .map_err(|e| outcome_unknown(e, &code, &check))?
        .bound_to(&submitted.bytes);
    finish_wait(out, &waited, online.json, &code)
}

/// Only the agent's verified receipt settles a command; the rpc's status alone exits 3.
fn finish_wait(
    out: &mut dyn Write,
    waited: &agent_controller::Waited,
    json: bool,
    code: &str,
) -> Result<()> {
    render::command_state(out, waited, json);
    if let ReceiptCheck::Verified(receipt) = &waited.receipt {
        let outcome = receipt.receipt().outcome;
        ensure!(
            outcome == ReceiptOutcome::Executed,
            "{code} was {} (the agent's signed receipt)",
            outcome.as_str()
        );
        return Ok(());
    }
    let status = waited.view.0.status;
    if !waited.finished && status != ControlCommandStatus::Expired as i32 {
        return Err(still_pending(format!("{code} is still pending")));
    }
    Err(still_pending(format!(
        "the rpc reports {code} {}, but no receipt the agent key signed shows its outcome; check it with `cloud-agent controller command show --code {code}` before signing it again",
        render::command_status(status)
    )))
}

async fn command_show(
    client: &ControllerClient,
    code: &str,
    wait: bool,
    json: bool,
    bounds: WaitBounds,
    out: &mut dyn Write,
) -> Result<()> {
    let id = crate::control::commands::parse_code(code)?;
    // A wait settles only on an answer: without one the outcome is not known.
    let first = |e: ControllerError| {
        if wait && unanswered(&e) {
            show_unknown(e, code)
        } else {
            anyhow::Error::new(e)
        }
    };
    let agent = client.agent().await.map_err(first)?;
    let waited = client.command(&agent, &id).await.map_err(first)?;
    let command = agent_control::Command::decode(&waited.view.0.command)
        .map_err(|e| anyhow!("the rpc's command does not decode ({})", e.code_name()))?;
    if !json {
        render::command_draft(
            out,
            &command,
            &format!("{}{id}", crate::control::commands::CODE_PREFIX),
        );
    }
    if !wait {
        render::command_state(out, &waited, json);
        return Ok(());
    }
    let now = client.checked_now().map_err(|e| show_unknown(e, code))?;
    let max = wait_bound(command.expires_at, now, bounds.grace_secs, bounds);
    let waited = client
        .wait(&agent, &id, max)
        .await
        .map_err(|e| show_unknown(e, code))?;
    finish_wait(out, &waited, json, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::prompt::Scripted;
    use agent_controller::testkit::{FakeRpc, TestAgent, controller_id, key_file, party_for};
    use clap::Parser;

    const AGENT: [u8; 32] = [0x91; 32];
    const KEY: u8 = 0xa1;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(subcommand)]
        command: ControllerCommands,
    }

    struct Env {
        rpc: FakeRpc,
        agent: TestAgent,
        key: PathBuf,
        options: ControllerOptions,
    }

    async fn env(seeds: &[(u8, &str)]) -> Env {
        let rpc = FakeRpc::start(Network::Devnet).await.unwrap();
        let agent = TestAgent::linked(AGENT, Network::Devnet, seeds).unwrap();
        rpc.publish(&agent);
        let key = key_file("cli", KEY).unwrap();
        let options = ControllerOptions {
            config: key.with_file_name("agent.toml"),
            ..ControllerOptions::default()
        };
        Env {
            rpc,
            agent,
            key,
            options,
        }
    }

    impl Env {
        fn args(&self, words: &[&str]) -> Vec<String> {
            let mut args: Vec<String> = words.iter().map(|w| w.to_string()).collect();
            args.extend([
                "--agent".to_string(),
                self.agent.party.to_string(),
                "--key".to_string(),
                self.key.display().to_string(),
                "--network".to_string(),
                "devnet".to_string(),
                "--rpc".to_string(),
                self.rpc.url.clone(),
            ]);
            args
        }

        async fn run_args(&self, args: Vec<String>, answers: &[&str]) -> (Result<()>, String) {
            let cli =
                Cli::try_parse_from(std::iter::once("controller".to_string()).chain(args)).unwrap();
            let prompter = Scripted::new(answers.iter().copied());
            let mut out = Vec::new();
            let result =
                super::super::run_controller_in(&self.options, cli.command, &prompter, &mut out)
                    .await;
            (result, String::from_utf8_lossy(&out).into_owned())
        }

        async fn run(&self, words: &[&str], answers: &[&str]) -> (Result<()>, String) {
            self.run_args(self.args(words), answers).await
        }
    }

    #[tokio::test]
    async fn status_and_log_show_the_verified_agent() {
        let env = env(&[(KEY, "policy,direct,read")]).await;
        let (result, text) = env.run(&["status"], &[]).await;
        result.unwrap();
        assert!(
            text.contains("verified here: 1 entries")
                && text.contains("in effect: policy, direct, read"),
            "{text}"
        );
        assert!(
            text.contains("matches the verified log") && text.contains("Reported by the rpc"),
            "{text}"
        );
        let (result, text) = env.run(&["log", "--json"], &[]).await;
        result.unwrap();
        let entries: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 1);
        for words in [
            vec!["holdings"],
            vec!["traffic"],
            vec!["activity"],
            vec!["ledger"],
            vec!["packages"],
            vec!["commands"],
        ] {
            let (result, text) = env.run(&words, &[]).await;
            result.unwrap();
            assert!(text.contains("Reported by the rpc"), "{words:?}: {text}");
        }
        assert_eq!(
            env.rpc.state().session_count(),
            0,
            "every command ends its session"
        );
    }

    #[tokio::test]
    async fn a_policy_needs_its_typed_confirmation_and_yes_only_when_nothing_widens() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        let (result, _) = env
            .run(&["policy", "set", "--pause"], &["sign policy v2"])
            .await;
        assert!(result.unwrap_err().to_string().contains("not confirmed"));
        assert_eq!(env.rpc.state().count("SubmitControllerPolicy"), 0);
        let (result, text) = env
            .run(&["policy", "set", "--pause"], &["sign policy v1"])
            .await;
        result.unwrap();
        assert!(
            text.contains("Submitted policy v1") && text.contains("widens nothing"),
            "{text}"
        );
        // Once the agent accepts it, a resume widens: --yes is refused, the phrase works.
        let view = env.rpc.state().policies[0].clone();
        let envelope = agent_control::ProofEnvelope::decode(&view.proof).unwrap();
        env.agent.accept(&view.document, &envelope).unwrap();
        env.rpc.publish(&env.agent);
        let (result, _) = env.run(&["policy", "set", "--resume", "--yes"], &[]).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("--yes is refused"), "{err}");
        let (result, text) = env
            .run(&["policy", "set", "--resume"], &["sign policy v2"])
            .await;
        result.unwrap();
        assert!(text.contains("It widens"), "{text}");
        // A narrowing takes --yes.
        let (result, _) = env
            .run(&["policy", "set", "--transfers", "off", "--yes"], &[])
            .await;
        result.unwrap();
        let (result, _) = env
            .run(
                &[
                    "policy",
                    "set",
                    "--scope",
                    "00",
                    "--scope-commands",
                    "swap",
                    "--yes",
                ],
                &[],
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("fingerprint"));
        let (result, _) = env.run(&["policy", "set"], &[]).await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("nothing to change")
        );
    }

    #[tokio::test]
    async fn a_scope_with_pairs_and_lps_is_signed_into_the_policy() {
        let env = env(&[(KEY, "policy,direct,read"), (0xa2, "direct")]).await;
        let direct = controller_id(0xa2);
        let lp = party_for([0x92; 32], "lp-1").unwrap();
        let usdc_admin = party_for([0x93; 32], "registry").unwrap();
        let pair = format!("cc,USDCx@{usdc_admin},0.2,1000");
        let (result, text) = env
            .run(
                &[
                    "policy",
                    "set",
                    "--scope",
                    &direct.fingerprint(),
                    "--scope-commands",
                    "swap",
                    "--scope-pair",
                    &pair,
                    "--scope-lp",
                    lp.as_str(),
                    "--yes",
                ],
                &[],
            )
            .await;
        result.unwrap();
        assert!(text.contains("widens nothing"), "{text}");
        let state = env.rpc.state();
        let document = PolicyDocument::decode(&state.policies[0].document).unwrap();
        let scope = &document.rules.scopes[0];
        assert_eq!(scope.controller, direct);
        assert_eq!(scope.commands, vec![CommandKind::Swap]);
        assert_eq!(scope.pairs.len(), 1);
        assert_eq!(scope.pairs[0].sell, PolicyInstrument::Cc);
        assert_eq!(scope.lps, vec![lp]);
    }

    #[tokio::test]
    async fn the_rules_round_trip_through_a_file() {
        let env = env(&[(KEY, "policy,direct,read")]).await;
        let (result, lines) = env.run(&["policy", "show", "--rules"], &[]).await;
        result.unwrap();
        assert!(lines.starts_with("paused=false\n"), "{lines}");
        let path = env.key.with_file_name("rules.txt");
        std::fs::write(
            &path,
            lines.replace("paused=false", "# paused\npaused=true"),
        )
        .unwrap();
        let (result, _) = env
            .run(
                &["policy", "set", "--rules", path.to_str().unwrap(), "--yes"],
                &[],
            )
            .await;
        result.unwrap();
        let document = PolicyDocument::decode(&env.rpc.state().policies[0].document).unwrap();
        assert!(document.rules.paused);
        std::fs::write(&path, "paused=maybe\n").unwrap();
        let (result, _) = env
            .run(
                &["policy", "set", "--rules", path.to_str().unwrap(), "--yes"],
                &[],
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("valid rule lines"));
    }

    #[tokio::test]
    async fn a_command_needs_its_phrase_and_never_takes_yes() {
        let env = env(&[(KEY, "policy,direct,read")]).await;
        let to = party_for([0x94; 32], "bob").unwrap();
        let words = [
            "command",
            "transfer-cc",
            "--to",
            to.as_str(),
            "--amount",
            "1.50",
        ];
        let (result, _) = env.run(&words, &["transfer 1.50 CC"]).await;
        assert!(result.is_err(), "the phrase names the normalised amount");
        assert_eq!(env.rpc.state().count("SubmitControllerCommand"), 0);
        let (result, text) = env.run(&words, &["transfer 1.5 CC"]).await;
        result.unwrap();
        assert!(
            text.contains("Submitted CMD-") && text.contains("control execute --code CMD-"),
            "{text}"
        );
        let view = env.rpc.state().commands[0].clone();
        let mut args = env.args(&[
            "command",
            "transfer-cc",
            "--to",
            to.as_str(),
            "--amount",
            "1",
        ]);
        args.push("--yes".into());
        assert!(
            Cli::try_parse_from(std::iter::once("controller".to_string()).chain(args)).is_err(),
            "no --yes"
        );

        // show: the receipt is checked against the verified log.
        let code = format!("CMD-{}", view.command_id);
        let id = agent_control::Uuid7::parse(&view.command_id).unwrap();
        let receipt = env
            .agent
            .receipt(
                &id,
                &view.command,
                agent_control::ReceiptOutcome::Executed,
                None,
            )
            .unwrap();
        env.rpc.state().settle(
            &view.command_id,
            ControlCommandStatus::Executed,
            Some(receipt),
        );
        let (result, text) = env
            .run(&["command", "show", "--code", &code, "--wait"], &[])
            .await;
        result.unwrap();
        assert!(
            text.contains("executed") && text.contains("verified receipt"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_swap_and_a_token_transfer_are_drafted_with_explicit_instruments() {
        let env = env(&[(KEY, "policy,direct,read")]).await;
        let admin = party_for([0x95; 32], "registry").unwrap();
        let dso = party_for([0x96; 32], "DSO").unwrap();
        let (result, text) = env
            .run(
                &[
                    "command",
                    "swap",
                    "--sell-admin",
                    dso.as_str(),
                    "--sell-id",
                    "Amulet",
                    "--sell-amount",
                    "100",
                    "--buy-admin",
                    admin.as_str(),
                    "--buy-id",
                    "USDCx",
                    "--min-receive",
                    "15",
                    "--max-fee-cc",
                    "0.5",
                ],
                &["swap 100 Amulet for 15 USDCx"],
            )
            .await;
        result.unwrap();
        assert!(text.contains("operation: swap"), "{text}");
        let to = party_for([0x97; 32], "bob").unwrap();
        let (result, _) = env
            .run(
                &[
                    "command",
                    "transfer-token",
                    "--to",
                    to.as_str(),
                    "--instrument-admin",
                    admin.as_str(),
                    "--instrument-id",
                    "USDCx",
                    "--amount",
                    "2",
                ],
                &["transfer 2 USDCx"],
            )
            .await;
        result.unwrap();
        assert_eq!(env.rpc.state().commands.len(), 2);
    }

    /// Waits that end at once, so no test sleeps out a validity.
    const SHORT_WAIT: WaitBounds = WaitBounds {
        grace_secs: 0,
        max: Duration::from_millis(50),
    };

    #[tokio::test]
    async fn wait_exits_pending_while_no_receipt_arrives() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        let to = party_for([0x98; 32], "bob").unwrap();
        let (result, _) = env
            .run(
                &[
                    "command",
                    "transfer-cc",
                    "--to",
                    to.as_str(),
                    "--amount",
                    "3",
                    "--wait",
                ],
                &["transfer 3 CC"],
            )
            .await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
    }

    #[test]
    fn the_wait_bound_is_the_expiry_plus_the_grace_at_most_the_maximum() {
        let now = UtcTime::from_unix_secs(1_800_000_000).unwrap();
        let at = |secs: i64| now.checked_add_secs(secs).unwrap();
        let bounds = WaitBounds::default();
        assert_eq!(
            bounds.grace_secs,
            crate::control::commands::RECOVERY_GRACE_SECS
        );
        assert_eq!(
            wait_bound(at(600), now, bounds.grace_secs, bounds),
            Duration::from_secs(1200)
        );
        assert_eq!(
            wait_bound(at(3600), now, 0, bounds),
            Duration::from_secs(1800),
            "a policy wait stops at the maximum"
        );
        assert_eq!(
            wait_bound(at(-5), now, 0, bounds),
            Duration::from_secs(1),
            "at least a second"
        );
        assert_eq!(
            wait_bound(at(600), now, 600, SHORT_WAIT),
            Duration::from_millis(50)
        );
    }

    #[tokio::test]
    async fn a_lost_answer_or_a_failed_wait_after_the_submit_exits_pending() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        env.rpc.state().lose_submit_answers = true;
        let to = party_for([0x9a; 32], "bob").unwrap();
        let words = [
            "command",
            "transfer-cc",
            "--to",
            to.as_str(),
            "--amount",
            "1",
        ];
        let (result, text) = env.run(&words, &["transfer 1 CC"]).await;
        let err = result.unwrap_err();
        let code = format!("CMD-{}", env.rpc.state().commands[0].command_id);
        assert!(super::super::is_pending(&err), "{err:#}");
        assert!(
            format!("{err:#}").contains(&format!("command show --code {code}")),
            "{err:#}"
        );
        assert!(text.contains(&code), "{text}");
        let (result, _) = env
            .run(&["policy", "set", "--pause"], &["sign policy v1"])
            .await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        assert_eq!(env.rpc.state().policies.len(), 1);
        // Stored and answered, then the wait cannot read it: pending too.
        {
            let mut state = env.rpc.state();
            state.lose_submit_answers = false;
            state.unavailable = vec!["GetControllerCommand".into(), "GetControllerPolicy".into()];
        }
        let mut waiting = words.to_vec();
        waiting.push("--wait");
        let (result, _) = env.run(&waiting, &["transfer 1 CC"]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        let (result, _) = env
            .run(&["policy", "set", "--pause", "--wait"], &["sign policy v1"])
            .await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        // Nothing sent is no such case.
        let (result, _) = env.run(&words, &["transfer 2 CC"]).await;
        assert!(!super::super::is_pending(&result.unwrap_err()));
    }

    /// Submits a transfer, with `--wait` when `wait`; returns the result and the stored command.
    async fn submit_transfer(
        env: &Env,
        seed: u8,
        wait: bool,
    ) -> (Result<()>, orderbook_proto::orderbook::ControlCommandView) {
        let to = party_for([seed; 32], "bob").unwrap();
        let mut words = vec![
            "command",
            "transfer-cc",
            "--to",
            to.as_str(),
            "--amount",
            "4",
        ];
        if wait {
            words.push("--wait");
        }
        let (result, _) = env.run(&words, &["transfer 4 CC"]).await;
        let view = env.rpc.state().commands.last().cloned().unwrap();
        (result, view)
    }

    #[tokio::test]
    async fn an_expiry_by_the_rpcs_clock_alone_exits_pending_until_the_agents_receipt() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        // Expired with no receipt: the agent may still run it.
        env.rpc.state().claimed_command_status = Some(ControlCommandStatus::Expired);
        let (result, view) = submit_transfer(&env, 0x9b, true).await;
        let err = result.unwrap_err();
        let code = format!("CMD-{}", view.command_id);
        assert!(super::super::is_pending(&err), "{err:#}");
        assert!(
            format!("{err:#}").contains(&format!("command show --code {code}")),
            "{err:#}"
        );
        let show = ["command", "show", "--code", code.as_str(), "--wait"];
        let (result, _) = env.run(&show, &[]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        // The agent's executed receipt, posted after the expiry: done.
        let id = agent_control::Uuid7::parse(&view.command_id).unwrap();
        let receipt = env
            .agent
            .receipt(
                &id,
                &view.command,
                agent_control::ReceiptOutcome::Executed,
                None,
            )
            .unwrap();
        env.rpc.state().settle(
            &view.command_id,
            ControlCommandStatus::Executed,
            Some(receipt),
        );
        let (result, text) = env.run(&show, &[]).await;
        result.unwrap();
        assert!(text.contains("verified receipt: executed"), "{text}");
    }

    #[tokio::test]
    async fn a_wait_succeeds_only_on_the_agents_executed_receipt() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        let (result, view) = submit_transfer(&env, 0x9c, false).await;
        result.unwrap();
        let code = format!("CMD-{}", view.command_id);
        let show = ["command", "show", "--code", code.as_str(), "--wait"];
        let id = agent_control::Uuid7::parse(&view.command_id).unwrap();
        let receipt = |outcome| {
            env.agent
                .receipt(&id, &view.command, outcome, None)
                .unwrap()
        };
        // A final status without the agent's receipt settles nothing.
        for status in [ControlCommandStatus::Executed, ControlCommandStatus::Failed] {
            env.rpc.state().settle(&view.command_id, status, None);
            let (result, _) = env.run(&show, &[]).await;
            let err = result.unwrap_err();
            assert!(super::super::is_pending(&err), "{status:?}: {err:#}");
        }
        // The agent's signed refusal is the outcome, whatever the rpc's status.
        env.rpc.state().settle(
            &view.command_id,
            ControlCommandStatus::Executed,
            Some(receipt(agent_control::ReceiptOutcome::Refused)),
        );
        let (result, _) = env.run(&show, &[]).await;
        let err = result.unwrap_err();
        assert!(!super::super::is_pending(&err), "{err:#}");
        assert!(format!("{err:#}").contains("was refused"), "{err:#}");
        env.rpc.state().settle(
            &view.command_id,
            ControlCommandStatus::Refused,
            Some(receipt(agent_control::ReceiptOutcome::Executed)),
        );
        let (result, _) = env.run(&show, &[]).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn a_show_wait_without_an_answer_from_the_rpc_exits_pending() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        let (result, view) = submit_transfer(&env, 0x9d, false).await;
        result.unwrap();
        let code = format!("CMD-{}", view.command_id);
        let show = ["command", "show", "--code", code.as_str(), "--wait"];
        // The first read finds it, then the wait's read gets no answer.
        let reads = env.rpc.state().count("GetControllerCommand");
        env.rpc.state().unavailable_after = vec![("GetControllerCommand".into(), reads + 1)];
        let (result, _) = env.run(&show, &[]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        assert!(
            format!("{err:#}").contains(&format!("command show --code {code}")),
            "{err:#}"
        );
        // No answer to the first read: pending with --wait, a plain error without it.
        {
            let mut state = env.rpc.state();
            state.unavailable_after.clear();
            state.unavailable = vec!["GetControllerCommand".into()];
        }
        let (result, _) = env.run(&show, &[]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        let (result, _) = env.run(&show[..4], &[]).await;
        assert!(!super::super::is_pending(&result.unwrap_err()));
        // No rpc to connect to: the same.
        let mut args = env.args(&show);
        if let Some(rpc) = args.last_mut() {
            *rpc = "http://127.0.0.1:1".into();
        }
        let (result, _) = env.run_args(args, &[]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        // The rpc's refusal is an answer.
        env.rpc.state().unavailable.clear();
        let unknown = "CMD-017f22e2-79b0-7cc3-98c4-dc0c0c07398f";
        let (result, _) = env
            .run(&["command", "show", "--code", unknown, "--wait"], &[])
            .await;
        assert!(!super::super::is_pending(&result.unwrap_err()));
    }

    #[tokio::test]
    async fn a_wait_counts_a_receipt_only_for_the_bytes_this_key_signed() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        // The rpc serves other bytes under the submitted id, with the agent's genuine receipt.
        let same =
            TestAgent::linked(AGENT, Network::Devnet, &[(KEY, "policy,direct,read")]).unwrap();
        env.rpc.state().substitute = Some(same);
        let to = party_for([0x9e; 32], "bob").unwrap();
        let words = [
            "command",
            "transfer-cc",
            "--to",
            to.as_str(),
            "--amount",
            "4",
            "--wait",
        ];
        let (result, text) = env.run(&words, &["transfer 4 CC"]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        assert!(
            text.contains("other command bytes than the ones this key signed"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_policy_wait_succeeds_only_when_the_verified_log_holds_the_acceptance() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.wait = SHORT_WAIT;
        // Each draft gets its own validity, so that no two documents are the same.
        let set =
            |validity: &'static str| ["policy", "set", "--pause", "--wait", "--validity", validity];
        let (result, _) = env.run(&set("3600"), &["sign policy v1"]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        // The rpc reports an acceptance that the log does not hold.
        env.rpc.state().claimed_policy_status = Some(ControlPolicyStatus::Accepted);
        let (result, text) = env.run(&set("3500"), &["sign policy v1"]).await;
        let err = result.unwrap_err();
        assert!(super::super::is_pending(&err), "{err:#}");
        assert!(
            format!("{err:#}").contains("does not hold its acceptance"),
            "{err:#}"
        );
        assert!(text.contains("accepted (reported by the rpc)"), "{text}");
        // The agent accepts it and its log holds that.
        env.rpc.state().claimed_policy_status = None;
        let same =
            TestAgent::linked(AGENT, Network::Devnet, &[(KEY, "policy,direct,read")]).unwrap();
        let agent = std::mem::replace(&mut env.agent, same);
        env.rpc.state().acceptor = Some(agent);
        let (result, text) = env.run(&set("3400"), &["sign policy v1"]).await;
        result.unwrap();
        assert!(
            text.contains("The verified log holds its acceptance."),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_dry_run_neither_writes_a_key_nor_signs() {
        let options = ControllerOptions {
            dry_run: true,
            ..ControllerOptions::default()
        };
        let dir = agent_controller::testkit::temp_dir("dry-keygen").join("k");
        let mut out = Vec::new();
        let none: [&str; 0] = [];
        let keygen = ControllerCommands::Keygen { dir: dir.clone() };
        let err = super::super::run_controller_in(&options, keygen, &Scripted::new(none), &mut out)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--dry-run"), "{err}");
        assert!(!dir.exists());
        let sign = ControllerCommands::Sign {
            key: key_file("dry-sign", KEY).unwrap(),
            payload: Some(URL_SAFE_NO_PAD.encode(b"scheme=x\n")),
            file: None,
            network: "devnet".into(),
            current_policy: None,
        };
        let err =
            super::super::run_controller_in(&options, sign, &Scripted::new(["sign"]), &mut out)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("--dry-run"), "{err}");
    }

    #[tokio::test]
    async fn a_dry_run_signs_and_sends_nothing() {
        let mut env = env(&[(KEY, "policy,direct,read")]).await;
        env.options.dry_run = true;
        let (result, text) = env.run(&["policy", "set", "--pause"], &[]).await;
        result.unwrap();
        assert!(
            text.contains("Dry run: nothing was signed or sent"),
            "{text}"
        );
        let to = party_for([0x99; 32], "bob").unwrap();
        let (result, _) = env
            .run(
                &[
                    "command",
                    "transfer-cc",
                    "--to",
                    to.as_str(),
                    "--amount",
                    "1",
                ],
                &[],
            )
            .await;
        result.unwrap();
        let state = env.rpc.state();
        assert_eq!(
            state.count("SubmitControllerPolicy") + state.count("SubmitControllerCommand"),
            0
        );
    }

    #[tokio::test]
    async fn the_target_comes_from_the_flags_then_the_section_and_refuses_the_local_agent() {
        let env = env(&[(KEY, "policy,direct,read")]).await;
        let config = env.key.with_file_name("agent.toml");
        std::fs::write(
            &config,
            format!("[controller]\nkey_file = \"controller.key\"\nagent_party = \"{}\"\nnetwork = \"devnet\"\n", env.agent.party),
        )
        .unwrap();
        let options = ControllerOptions {
            config,
            ..ControllerOptions::default()
        };
        let online = OnlineArgs {
            rpc: Some(env.rpc.url.clone()),
            ..OnlineArgs::default()
        };
        let target = resolve(&options, &online).unwrap();
        assert_eq!(target.key_file, env.key);
        assert_eq!(target.agent, env.agent.party);
        assert_eq!(target.rpc_host.as_str(), "localhost");
        let local = ControllerOptions {
            local_party: Some(env.agent.party.to_string()),
            ..options.clone()
        };
        let err = resolve(&local, &online).unwrap_err().to_string();
        assert!(err.contains("this machine's own agent"), "{err}");
        // The default rpc is the network's.
        let target = resolve(&options, &OnlineArgs::default()).unwrap();
        assert_eq!(target.rpc_host.as_str(), "orderbook-devnet.silvana.dev");
        let none = ControllerOptions {
            config: env.key.with_file_name("missing.toml"),
            ..ControllerOptions::default()
        };
        assert!(
            resolve(&none, &online)
                .unwrap_err()
                .to_string()
                .contains("no agent")
        );
        let chain = ControllerOptions {
            chain: Some("testnet".into()),
            ..none.clone()
        };
        let flags = OnlineArgs {
            agent: Some(env.agent.party.to_string()),
            key: Some(env.key.clone()),
            ..OnlineArgs::default()
        };
        assert!(
            resolve(&chain, &flags)
                .unwrap_err()
                .to_string()
                .contains("devnet or mainnet")
        );
        let mainnet = ControllerOptions {
            chain: Some("mainnet".into()),
            ..none
        };
        assert_eq!(resolve(&mainnet, &flags).unwrap().network, Network::Mainnet);
    }

    #[test]
    fn the_rpc_is_https_except_on_loopback() {
        let (url, host) = rpc_of("https://orderbook-devnet.silvana.dev:443").unwrap();
        assert_eq!(
            (url.as_str(), host.as_str()),
            (
                "https://orderbook-devnet.silvana.dev:443",
                "orderbook-devnet.silvana.dev"
            )
        );
        for loopback in [
            "http://localhost:7000",
            "http://127.0.0.1:7000",
            "http://[::1]:7000",
        ] {
            assert_eq!(
                rpc_of(loopback).unwrap().1.as_str(),
                "localhost",
                "{loopback}"
            );
        }
        for bad in [
            "http://orderbook-devnet.silvana.dev",
            "ftp://x.silvana.dev",
            "https://1.2.3.4",
            "https://u:p@x.silvana.dev",
            "https://x.silvana.dev/path",
        ] {
            assert!(rpc_of(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn edits_parse_strictly() {
        assert!(parse_packages("any").unwrap().any);
        let id = "ab".repeat(32);
        assert_eq!(
            parse_packages(&format!("{id},{id}"))
                .unwrap_err()
                .to_string(),
            format!("package {id} is listed twice")
        );
        assert!(parse_packages("").is_err());
        assert_eq!(
            parse_kinds("transfer_cc,swap").unwrap(),
            vec![CommandKind::Swap, CommandKind::TransferCc]
        );
        assert!(parse_kinds("none").unwrap().is_empty());
        assert!(parse_kinds("transfer_all").is_err());
        assert!(parse_pairs(&["cc,cc,1,1".into()]).is_err());
        assert!(
            parse_pairs(&["cc,USDCx,1,1".into()]).is_err(),
            "an admin is required"
        );
        assert!(plain_amount("1e3", "x").is_err());
        assert!(plain_amount("-1", "x").is_err());
        assert_eq!(plain_amount("1.50", "x").unwrap().to_string(), "1.5");
    }

    #[test]
    fn the_cli_parses() {
        let parse = |words: &[&str]| {
            Cli::try_parse_from(std::iter::once("controller").chain(words.iter().copied()))
        };
        assert!(parse(&["status", "--agent", "a", "--json"]).is_ok());
        assert!(parse(&["policy", "set", "--pause", "--resume"]).is_err());
        assert!(
            parse(&["policy", "set", "--scope-commands", "swap"]).is_err(),
            "a scope edit names its controller"
        );
        assert!(parse(&["policy", "set", "--rules", "f", "--pause"]).is_err());
        assert!(parse(&["policy", "set", "--base", "latest", "--pause"]).is_err());
        assert!(
            parse(&[
                "command",
                "transfer-cc",
                "--to",
                "x",
                "--amount",
                "1",
                "--validity",
                "901"
            ])
            .is_err()
        );
        assert!(parse(&["activity", "--limit", "101"]).is_err());
        assert!(parse(&["command", "show", "--code", "CMD-x", "--wait"]).is_ok());
        <Cli as clap::CommandFactory>::command().debug_assert();
    }
}
