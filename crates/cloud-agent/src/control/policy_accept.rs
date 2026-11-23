//! `control policy accept` and the daemon's acceptance: a pending policy is checked against
//! the local verified log, then accepted, or answered with a refusal the agent signs.

use std::collections::BTreeSet;
use std::io::Write;

use agent_control::{
    CodeSource, ControlState, EntryBuilder, Hash32, HeadRef, PackageId, PolicyError, ProofEnvelope,
    Receipt, ReceiptOutcome, ReceiptSubject, Rules, UtcTime, VerifiedPolicy, Widening,
    describe_rules, verify_policy_submission,
};
use anyhow::{Result, anyhow, bail, ensure};
use orderbook_proto::orderbook::PendingPolicy;

use super::ControlContext;
use super::link::{build_refusal, write_signed_times};
use super::prompt::{Prompter, Yes, confirm_typed};
use super::rpc::{ControlRpc, document_hash};
use super::sync;

/// A policy waiting for the agent; only its document and proof count, once checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub hash: Hash32,
    pub document: Vec<u8>,
    pub proof: Vec<u8>,
}

impl Candidate {
    /// A ListPendingControl item; its document must hash to the name it is listed under.
    pub fn from_pending(item: &PendingPolicy) -> Result<Self> {
        let hash = document_hash(&item.document);
        ensure!(
            hash.to_string() == item.policy_hash,
            "the rpc lists pending policy {} with a document that hashes to {hash}",
            item.policy_hash
        );
        Ok(Self {
            hash,
            document: item.document.clone(),
            proof: item.proof.clone(),
        })
    }
}

/// What the agent makes of a pending policy now.
#[derive(Clone, Debug)]
pub enum Verdict {
    /// It passes every check of the submission rules.
    Acceptable(Box<VerifiedPolicy>),
    /// It can never pass: the code and name of the check it fails.
    Refused { code: u16, name: &'static str },
    /// Issued later than the agent's clock allows: it stays pending.
    NotYet,
}

/// Judges `candidate` against the local verified state at `now`.
pub fn judge(state: &ControlState, candidate: &Candidate, now: UtcTime) -> Verdict {
    let envelope = match ProofEnvelope::decode(&candidate.proof) {
        Ok(envelope) => envelope,
        Err(e) => {
            return Verdict::Refused {
                code: e.code(),
                name: e.code_name(),
            };
        }
    };
    match verify_policy_submission(state, &candidate.document, &envelope, now) {
        Ok(verified) => Verdict::Acceptable(Box::new(verified)),
        Err(PolicyError::IssuedInFuture) => Verdict::NotYet,
        Err(e) => Verdict::Refused {
            code: e.code(),
            name: e.code_name(),
        },
    }
}

/// What happened to one pending policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted: the control log head after the acceptance.
    Accepted(HeadRef),
    /// Refused with a signed receipt; the code of the check it failed.
    Refused(u16),
    /// Left pending.
    Left,
    /// The local log holds it already; the rpc's listing lags.
    AlreadyAccepted,
}

/// The categories a policy widens, by name.
pub(crate) fn widened_categories(widened: &Widening) -> Vec<&'static str> {
    [
        (widened.transfers, "transfers"),
        (widened.trading, "trading"),
        (widened.other, "other"),
        (widened.packages, "packages"),
        (widened.scopes, "scopes"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

/// The widening as one phrase.
pub(crate) fn widening_text(widened: &Widening) -> String {
    let parts = widened_categories(widened);
    if parts.is_empty() {
        "narrows or keeps every category".to_string()
    } else {
        format!("widens {}", parts.join(", "))
    }
}

/// The packages rule of `new` against `old`: a summary line, then the ids that change.
pub(crate) fn package_diff(old: &Rules, new: &Rules) -> Vec<String> {
    let (old, new) = (&old.packages, &new.packages);
    let set = |items: &[PackageId]| items.iter().copied().collect::<BTreeSet<PackageId>>();
    match (old.any, new.any) {
        (true, true) => vec!["Packages: any package, as before.".to_string()],
        (false, true) => vec![format!(
            "Packages: any package, instead of {} selected.",
            old.items.len()
        )],
        (true, false) => {
            let mut lines = vec![format!(
                "Packages: only these {}, instead of any package:",
                new.items.len()
            )];
            lines.extend(new.items.iter().map(|id| format!("  = {id}")));
            lines
        }
        (false, false) => {
            let (before, after) = (set(&old.items), set(&new.items));
            let added: Vec<&PackageId> = after.difference(&before).collect();
            let removed: Vec<&PackageId> = before.difference(&after).collect();
            if added.is_empty() && removed.is_empty() {
                return vec![format!("Packages: the same {} selected.", after.len())];
            }
            let mut lines = vec![format!(
                "Packages: {} selected, {} added, {} removed:",
                after.len(),
                added.len(),
                removed.len()
            )];
            lines.extend(added.iter().map(|id| format!("  + {id}")));
            lines.extend(removed.iter().map(|id| format!("  - {id}")));
            lines
        }
    }
}

/// A span of seconds as the CLI prints it, such as "1 h" or "90 s".
fn secs_text(secs: u64) -> String {
    match (
        secs.checked_div(3600),
        secs.checked_rem(3600),
        secs.checked_div(60),
        secs.checked_rem(60),
    ) {
        (Some(hours), Some(0), _, _) if hours > 0 => format!("{hours} h"),
        (_, _, Some(minutes), Some(0)) if minutes > 0 => format!("{minutes} min"),
        _ => format!("{secs} s"),
    }
}

/// The policy, its rules, what it widens against the rules in force, and its timing.
pub(crate) fn write_summary(
    out: &mut dyn Write,
    state: &ControlState,
    verified: &VerifiedPolicy,
    now: UtcTime,
) {
    let doc = &verified.document;
    let label = state
        .controller(&verified.signer)
        .map(|r| r.descriptor().label().as_str().to_string())
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "Pending policy v{} ({}), signed by {} \"{label}\"",
        doc.version,
        verified.hash,
        verified.signer.fingerprint()
    );
    let _ = writeln!(
        out,
        "  issued {}, expires {}",
        doc.issued_at, doc.expires_at
    );
    if let Some(not_before) = doc.not_before {
        let _ = writeln!(out, "  not before {not_before}");
    }
    let _ = writeln!(out, "Rules:");
    for line in describe_rules(&doc.rules) {
        let _ = writeln!(out, "  {line}");
    }
    let in_force = state.rules_at(now);
    let against = match in_force.policy() {
        Some(policy) => format!("policy v{} in force", policy.version()),
        None => "the default rules in force".to_string(),
    };
    let _ = writeln!(
        out,
        "Against {against}: it {}.",
        widening_text(&verified.widening)
    );
    for line in package_diff(in_force.rules(), &doc.rules) {
        let _ = writeln!(out, "{line}");
    }
    if let Some(scheduled) = state.scheduled_policy(now) {
        let _ = writeln!(
            out,
            "Scheduled now: policy v{} from {}.",
            scheduled.version(),
            scheduled.effective_at()
        );
    }
    if verified.effective_at <= now {
        let _ = writeln!(out, "It applies at once when accepted.");
    } else if verified.required_delay_secs > 0 {
        let _ = writeln!(
            out,
            "It takes effect at about {}: a widening waits {} after acceptance; until then the rules in force stay.",
            verified.effective_at,
            secs_text(verified.required_delay_secs)
        );
    } else {
        let _ = writeln!(
            out,
            "It takes effect at about {}, its not-before time.",
            verified.effective_at
        );
    }
}

/// The agent's refusal of `candidate` with `code`, signed, as AckPolicy takes it.
pub(crate) fn refusal_receipt(
    ctx: &ControlContext,
    state: &ControlState,
    candidate: &Candidate,
    code: u16,
    now: UtcTime,
) -> Result<(Vec<u8>, [u8; 64])> {
    let receipt = Receipt {
        network: ctx.network,
        agent_party: ctx.party.clone(),
        agent_key: ctx.signer.public_key(),
        subject: ReceiptSubject::Policy(candidate.hash),
        subject_hash: candidate.hash,
        outcome: ReceiptOutcome::Refused,
        update_id: None,
        reason_code: Some(u64::from(code)),
        code_source: Some(CodeSource::Agent),
        log_seq: state.head().map_or(0, |head| head.seq),
        issued_at: now,
    };
    let bytes = receipt
        .encode()
        .map_err(|e| anyhow!("the refusal receipt does not encode ({})", e.code_name()))?;
    let signature = ctx.signer.sign_receipt(&receipt)?;
    Ok((bytes, signature))
}

/// Signs the refusal of `candidate` and answers the rpc with it.
async fn send_refusal(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    state: &ControlState,
    candidate: &Candidate,
    code: u16,
    now: UtcTime,
) -> Result<()> {
    let (receipt, signature) = refusal_receipt(ctx, state, candidate, code, now)?;
    rpc.ack_policy(&candidate.hash, receipt, signature)
        .await
        .map_err(|e| {
            anyhow!(
                "the refusal of policy {} was signed but not delivered: {e}",
                candidate.hash
            )
        })?;
    Ok(())
}

/// Whether the local log already accepted `candidate`: a listing that lags is never refused.
fn accepted_before(
    ctx: &ControlContext,
    prepared: &sync::Prepared,
    candidate: &Candidate,
    out: &mut dyn Write,
) -> Result<bool> {
    let stored = ctx.store.load(prepared.lock())?;
    let accepted = stored.is_some_and(|s| s.documents.get(&candidate.hash).is_some());
    if accepted {
        let _ = writeln!(
            out,
            "Policy {} is already accepted in the control log.",
            candidate.hash
        );
    }
    Ok(accepted)
}

/// The daemon's step for one pending policy, with no prompt: accepted when valid, else
/// refused with a signed receipt. `out` gets the account of what was done.
pub async fn accept_automatically(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    candidate: &Candidate,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let prepared = sync::prepare_change(ctx, rpc, out).await?;
    ensure!(
        prepared.state.is_linked(),
        "this agent is not linked: no policy applies"
    );
    if accepted_before(ctx, &prepared, candidate, out)? {
        return Ok(Outcome::AlreadyAccepted);
    }
    let now = rpc.checked_now()?;
    match judge(&prepared.state, candidate, now) {
        Verdict::NotYet => Ok(Outcome::Left),
        Verdict::Refused { code, name } => {
            let (state, _) = prepared.release();
            let _ = writeln!(
                out,
                "Pending policy {} refused: {name} (code {code}).",
                candidate.hash
            );
            send_refusal(ctx, rpc, &state, candidate, code, now).await?;
            Ok(Outcome::Refused(code))
        }
        Verdict::Acceptable(verified) => {
            ctx.require_env_file()?;
            let entry = EntryBuilder::new(&prepared.state)
                .accept_policy(&verified.bytes, &verified.envelope, now)
                .map_err(|e| build_refusal(&e))?;
            write_summary(out, &prepared.state, &verified, now);
            let head = sync::sign_and_upload(ctx, rpc, prepared.into_lock(), &entry, out).await?;
            write_signed_times(out, entry.entry());
            Ok(Outcome::Accepted(head))
        }
    }
}

/// The pending policy named by `hash`, else the only one pending, as GetPolicy returns it.
pub async fn fetch_candidate(rpc: &ControlRpc, hash: Option<&str>) -> Result<Candidate> {
    let hash = match hash {
        Some(text) => Hash32::parse(text.trim())
            .map_err(|_| anyhow!("--hash must be 64 lowercase hex characters"))?,
        None => {
            let pending = rpc
                .pending()
                .await
                .map_err(|e| anyhow!("listing pending policies failed: {e}"))?;
            match pending.policies.as_slice() {
                [] => bail!("no policy is pending for this agent"),
                [one] => Hash32::parse(&one.policy_hash)
                    .map_err(|_| anyhow!("the rpc lists a pending policy under an invalid hash"))?,
                many => bail!(
                    "{} policies are pending ({}): give --hash",
                    many.len(),
                    many.iter()
                        .map(|p| p.policy_hash.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    };
    let view = rpc
        .policy(&hash)
        .await
        .map_err(|e| anyhow!("fetching policy {hash} failed: {e}"))?;
    Ok(Candidate {
        hash,
        document: view.document,
        proof: view.proof,
    })
}

/// Shows `candidate`, asks for the typed `accept policy v<N>` (`--yes` only when nothing
/// widens), then signs and uploads the acceptance. An invalid one is refused and signed.
pub async fn accept_with_prompt(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    candidate: &Candidate,
    yes: bool,
) -> Result<Outcome> {
    ctx.require_env_file()?;
    let prepared = sync::prepare_change(ctx, rpc, out).await?;
    ensure!(
        prepared.state.is_linked(),
        "this agent is not linked: no policy applies"
    );
    if accepted_before(ctx, &prepared, candidate, out)? {
        return Ok(Outcome::AlreadyAccepted);
    }
    let now = rpc.checked_now()?;
    let verified = match judge(&prepared.state, candidate, now) {
        Verdict::Acceptable(verified) => verified,
        Verdict::NotYet => bail!(
            "the policy is issued later than this clock allows; nothing was signed: retry later"
        ),
        Verdict::Refused { code, name } => {
            let (state, _) = prepared.release();
            let _ = writeln!(
                out,
                "Pending policy {} cannot be accepted: {name} (code {code}).",
                candidate.hash
            );
            send_refusal(ctx, rpc, &state, candidate, code, now).await?;
            let _ = writeln!(out, "The agent's signed refusal was sent to the rpc.");
            return Ok(Outcome::Refused(code));
        }
    };
    let preview = EntryBuilder::new(&prepared.state)
        .accept_policy(&verified.bytes, &verified.envelope, now)
        .map_err(|e| build_refusal(&e))?;
    write_summary(out, &prepared.state, &verified, now);
    let phrase = format!("accept policy v{}", verified.document.version);
    let yes = if verified.widening.any() {
        Yes::Refused
    } else if yes {
        Yes::Given
    } else {
        Yes::NotGiven
    };
    let (state, _) = prepared.release();
    confirm_typed(prompter, out, &phrase, yes).await?;
    let (lock, state) = sync::relock(ctx, state.head()).await?;
    let entry = EntryBuilder::new(&state)
        .accept_policy(&verified.bytes, &verified.envelope, rpc.checked_now()?)
        .map_err(|e| build_refusal(&e))?;
    if entry.entry().body() != preview.entry().body() {
        bail!("the acceptance differs from the one confirmed: run the command again");
    }
    let head = sync::sign_and_upload(ctx, rpc, lock, &entry, out).await?;
    write_signed_times(out, entry.entry());
    Ok(Outcome::Accepted(head))
}

/// `control policy accept [--hash H] [--yes]`.
pub async fn run_accept(
    ctx: &ControlContext,
    rpc: &ControlRpc,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
    hash: Option<&str>,
    yes: bool,
) -> Result<Outcome> {
    ctx.require_env_file()?;
    let candidate = fetch_candidate(rpc, hash).await?;
    accept_with_prompt(ctx, rpc, prompter, out, &candidate, yes).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::control::prompt::Scripted;
    use crate::control::test_support::{FakeRpc, TestAgent};
    use agent_control::policy::Allowlist;
    use agent_control::{
        CommandScope, ControllerProof, ControllerType, DEFAULT_LINKED_RULES, Network, PartyId,
        PolicyDocument, controller_id_for,
    };
    use agent_logic::control_store::testing::TestChain;
    use ed25519_dalek::Signer;

    /// The controller seed of the test chains' link.
    pub(crate) const LINKED: u8 = 0x21;
    const SEED: [u8; 32] = [0x5a; 32];

    pub(crate) fn now() -> UtcTime {
        UtcTime::from_unix_secs(agent_logic::clock::now_secs_i64()).unwrap()
    }

    /// A policy signed by the Ed25519 controller of `signer`, valid `valid_secs` from `issued_at`.
    pub(crate) fn candidate(
        party: &PartyId,
        version: u64,
        prev: Hash32,
        rules: Rules,
        signer: u8,
        issued_at: UtcTime,
        valid_secs: i64,
    ) -> Candidate {
        let document = PolicyDocument {
            network: Network::Devnet,
            agent_party: party.clone(),
            version,
            prev_policy_hash: prev,
            issued_at,
            expires_at: issued_at.checked_add_secs(valid_secs).unwrap(),
            not_before: None,
            rules,
        }
        .encode()
        .unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[signer; 32]);
        let id = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
        let proof = ControllerProof::Ed25519 {
            signature: key.sign(&document).to_bytes(),
        };
        let proof = ProofEnvelope::new(id, &document, proof).encode().unwrap();
        Candidate {
            hash: document_hash(&document),
            document,
            proof,
        }
    }

    fn first(party: &PartyId, rules: Rules, at: UtcTime) -> Candidate {
        candidate(party, 1, Hash32([0; 32]), rules, LINKED, at, 3600)
    }

    fn refused(verdict: Verdict) -> (u16, &'static str) {
        match verdict {
            Verdict::Refused { code, name } => (code, name),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_verdict_follows_the_submission_rules_909_included() {
        let party = agent_logic::control_store::testing::party_for(SEED, "agent-1").unwrap();
        let chain = TestChain::build(SEED, &party, Network::Devnet, 1).unwrap();
        let state = &chain.state;
        let at = chain.created_at.checked_add_secs(7200).unwrap();
        let ok = first(&party, DEFAULT_LINKED_RULES, at);
        let Verdict::Acceptable(verified) = judge(state, &ok, at) else {
            panic!("acceptable")
        };
        assert!(
            !verified.widening.any() && verified.effective_at == at,
            "{verified:?}"
        );

        let wrong_prev = candidate(
            &party,
            1,
            Hash32([1; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            at,
            3600,
        );
        assert_eq!(
            refused(judge(state, &wrong_prev, at)),
            (905, "prev_mismatch")
        );
        let expired = candidate(
            &party,
            1,
            Hash32([0; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            at,
            60,
        );
        assert_eq!(
            refused(judge(state, &expired, at.checked_add_secs(120).unwrap())),
            (907, "expired")
        );
        let stranger = candidate(
            &party,
            1,
            Hash32([0; 32]),
            DEFAULT_LINKED_RULES,
            0x55,
            at,
            3600,
        );
        assert_eq!(refused(judge(state, &stranger, at)).0, 902);
        let future = first(
            &party,
            DEFAULT_LINKED_RULES,
            at.checked_add_secs(900).unwrap(),
        );
        assert!(matches!(judge(state, &future, at), Verdict::NotYet));
        let garbled = Candidate {
            proof: b"scheme=nonsense\n".to_vec(),
            ..ok.clone()
        };
        assert!(matches!(
            judge(state, &garbled, at),
            Verdict::Refused { .. }
        ));

        // A scope naming a controller the log never recorded is refused (909), which replay does not check.
        let unknown = controller_id_for(ControllerType::Ed25519, &[0x77; 32]);
        let scoped = Rules {
            scopes: vec![CommandScope {
                controller: unknown,
                commands: Vec::new(),
                pairs: Vec::new(),
                lps: Vec::new(),
            }],
            ..DEFAULT_LINKED_RULES
        };
        assert_eq!(
            refused(judge(state, &first(&party, scoped, at), at)),
            (909, "unknown_scope_controller")
        );
    }

    #[test]
    fn widenings_and_package_changes_are_spelled_out() {
        let id = |b: u8| PackageId([b; 32]);
        let selected = |items: Vec<PackageId>| Rules {
            packages: Allowlist { any: false, items },
            ..DEFAULT_LINKED_RULES
        };
        let any = DEFAULT_LINKED_RULES;
        assert_eq!(
            package_diff(&any, &any),
            vec!["Packages: any package, as before.".to_string()]
        );
        let narrowed = package_diff(&any, &selected(vec![id(1)]));
        assert_eq!(
            narrowed,
            vec![
                "Packages: only these 1, instead of any package:".to_string(),
                format!("  = {}", id(1))
            ]
        );
        assert_eq!(
            package_diff(&selected(vec![id(1), id(2)]), &any),
            vec!["Packages: any package, instead of 2 selected.".to_string()]
        );
        let changed = package_diff(&selected(vec![id(1), id(2)]), &selected(vec![id(2), id(3)]));
        assert_eq!(
            changed,
            vec![
                "Packages: 2 selected, 1 added, 1 removed:".to_string(),
                format!("  + {}", id(3)),
                format!("  - {}", id(1)),
            ]
        );
        assert_eq!(
            package_diff(&selected(vec![id(2)]), &selected(vec![id(2)])),
            vec!["Packages: the same 1 selected.".to_string()]
        );

        let all = Widening {
            transfers: true,
            trading: true,
            other: true,
            packages: true,
            scopes: true,
        };
        assert_eq!(
            widened_categories(&all),
            vec!["transfers", "trading", "other", "packages", "scopes"]
        );
        assert_eq!(
            widening_text(&Widening {
                packages: true,
                scopes: true,
                ..Widening::default()
            }),
            "widens packages, scopes"
        );
        assert_eq!(
            widening_text(&Widening::default()),
            "narrows or keeps every category"
        );
        assert_eq!(
            (
                secs_text(3600),
                secs_text(86_400),
                secs_text(90),
                secs_text(120)
            ),
            ("1 h".into(), "24 h".into(), "90 s".into(), "2 min".into())
        );
    }

    /// A linked agent; each test takes its own seed, since a key's guard is process-wide.
    async fn linked_agent(
        rpc: &FakeRpc,
        seed: u8,
        tag: &str,
    ) -> (TestAgent, ControlContext, ControlRpc) {
        let mut agent = TestAgent::new([seed; 32], rpc, tag);
        agent.linked(rpc, 1).await;
        let ctx = agent.context().await;
        let client = ControlRpc::connect(&ctx.config).await.unwrap();
        (agent, ctx, client)
    }

    #[tokio::test]
    async fn a_valid_policy_is_accepted_into_the_log_and_the_marker() {
        let rpc = FakeRpc::start().await;
        let (agent, ctx, client) = linked_agent(&rpc, 0x61, "accept-auto").await;
        let policy = first(
            &agent.party,
            Rules {
                paused: true,
                ..DEFAULT_LINKED_RULES
            },
            now(),
        );
        let mut out = Vec::new();
        let outcome = accept_automatically(&ctx, &client, &policy, &mut out)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let Outcome::Accepted(head) = outcome else {
            panic!("{outcome:?}: {text}")
        };
        assert_eq!(head.seq, 2, "{text}");
        assert_eq!(rpc.seqs(), vec![1, 2]);
        assert!(
            rpc.state()
                .documents
                .iter()
                .any(|d| d.document == policy.document),
            "the document travels with it"
        );
        assert!(
            agent.marker().unwrap().contains(":2:"),
            "{:?}",
            agent.marker()
        );
        assert!(
            text.contains("Pending policy v1")
                && text.contains("it narrows or keeps every category"),
            "{text}"
        );
        assert!(
            text.contains("Signed at") && text.contains("applies at once"),
            "{text}"
        );
        let state = ctx.guard.view().unwrap().state().clone();
        assert_eq!(state.latest_policy().map(|p| p.version()), Some(1));
        // A listing that still shows it is never answered with a refusal, and nothing is appended.
        let mut again = Vec::new();
        let outcome = accept_automatically(&ctx, &client, &policy, &mut again)
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::AlreadyAccepted);
        assert!(String::from_utf8_lossy(&again).contains("already accepted"));
        assert_eq!(rpc.seqs(), vec![1, 2]);
    }

    #[tokio::test]
    async fn an_invalid_policy_gets_the_agents_signed_refusal() {
        let rpc = FakeRpc::start().await;
        let (agent, ctx, client) = linked_agent(&rpc, 0x62, "accept-refuse").await;
        let stale = candidate(
            &agent.party,
            1,
            Hash32([9; 32]),
            DEFAULT_LINKED_RULES,
            LINKED,
            now(),
            3600,
        );
        let state = ctx.guard.view().unwrap().state().clone();
        let (bytes, signature) = refusal_receipt(&ctx, &state, &stale, 905, now()).unwrap();
        let receipt = Receipt::verify(&bytes, &signature, &agent.party).unwrap();
        assert_eq!(receipt.subject, ReceiptSubject::Policy(stale.hash));
        assert_eq!(receipt.subject_hash, stale.hash);
        assert_eq!(
            (receipt.outcome, receipt.reason_code, receipt.code_source),
            (ReceiptOutcome::Refused, Some(905), Some(CodeSource::Agent))
        );
        assert_eq!((receipt.log_seq, receipt.network), (1, Network::Devnet));
        // A policy the rpc does not hold for this agent: the signed refusal is not delivered.
        let mut out = Vec::new();
        let err = accept_automatically(&ctx, &client, &stale, &mut out)
            .await
            .unwrap_err();
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("refused: prev_mismatch (code 905)"), "{text}");
        assert!(
            format!("{err:#}").contains("signed but not delivered"),
            "{err:#}"
        );
        // Held for this agent, the rpc takes the refusal through its checks; nothing is appended.
        rpc.state()
            .policies
            .insert(stale.hash.to_string(), agent.party.clone());
        let mut out = Vec::new();
        accept_automatically(&ctx, &client, &stale, &mut out)
            .await
            .unwrap();
        let acks = rpc.state().acks.clone();
        assert_eq!(acks.len(), 1);
        let (hash, delivered) = acks.first().cloned().unwrap();
        assert_eq!(hash, stale.hash.to_string());
        let delivered = Receipt::decode(&delivered).unwrap();
        assert_eq!(
            (delivered.subject, delivered.reason_code),
            (ReceiptSubject::Policy(stale.hash), Some(905))
        );
        assert_eq!(rpc.seqs(), vec![1]);
    }

    #[tokio::test]
    async fn the_cli_needs_the_typed_phrase_and_takes_yes_only_when_nothing_widens() {
        let rpc = FakeRpc::start().await;
        let (agent, ctx, client) = linked_agent(&rpc, 0x63, "accept-cli").await;
        let narrow = first(
            &agent.party,
            Rules {
                paused: true,
                ..DEFAULT_LINKED_RULES
            },
            now(),
        );
        let mut out = Vec::new();
        let err = accept_with_prompt(
            &ctx,
            &client,
            &Scripted::new(["accept policy v2"]),
            &mut out,
            &narrow,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not confirmed"), "{err}");
        assert_eq!(rpc.seqs(), vec![1], "nothing was signed");
        let outcome = accept_with_prompt(&ctx, &client, &Scripted::none(), &mut out, &narrow, true)
            .await
            .unwrap();
        assert!(
            matches!(outcome, Outcome::Accepted(HeadRef { seq: 2, .. })),
            "{outcome:?}"
        );

        // A widening policy needs the phrase typed, --yes or not.
        let in_force = ctx
            .guard
            .view()
            .unwrap()
            .state()
            .latest_policy()
            .map(|p| *p.hash())
            .unwrap();
        let mut wide = DEFAULT_LINKED_RULES;
        wide.transfers.directed_only = false;
        let wider = candidate(&agent.party, 2, in_force, wide, LINKED, now(), 3600);
        let mut out = Vec::new();
        let err = accept_with_prompt(&ctx, &client, &Scripted::none(), &mut out, &wider, true)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("accept policy v2") && !err.to_string().contains("--yes"),
            "{err}"
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains("it widens transfers")
                && text.contains("a widening waits 24 h after acceptance"),
            "{text}"
        );
        let outcome = accept_with_prompt(
            &ctx,
            &client,
            &Scripted::new(["accept policy v2"]),
            &mut Vec::new(),
            &wider,
            false,
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, Outcome::Accepted(HeadRef { seq: 3, .. })),
            "{outcome:?}"
        );
        assert_eq!(rpc.seqs(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn controller_sign_prints_the_packages_and_scopes_widenings() {
        let party = agent_logic::control_store::testing::party_for(SEED, "agent-1").unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[LINKED; 32]);
        let direct = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
        let scope = |commands| CommandScope {
            controller: direct,
            commands,
            pairs: Vec::new(),
            lps: Vec::new(),
        };
        let mut old_rules = DEFAULT_LINKED_RULES;
        old_rules.packages = Allowlist {
            any: false,
            items: vec![PackageId([1; 32])],
        };
        old_rules.scopes = vec![scope(vec![agent_control::CommandKind::Swap])];
        let mut new_rules = DEFAULT_LINKED_RULES;
        new_rules.scopes = vec![scope(vec![
            agent_control::CommandKind::Swap,
            agent_control::CommandKind::TransferCc,
        ])];
        let old = first(&party, old_rules, now());
        let new = candidate(&party, 2, old.hash, new_rules, LINKED, now(), 3600);
        let mut out = Vec::new();
        let prompter = Scripted::new(["sign"]);
        crate::control::controller_tool::sign(
            &key,
            &new.document,
            Network::Devnet,
            Some(&old.document),
            &prompter,
            &mut out,
        )
        .await
        .unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains("against v1: widens packages, scopes"),
            "{text}"
        );
        assert!(
            text.contains("Packages: any package, instead of 1 selected."),
            "{text}"
        );
    }

    #[test]
    fn a_listed_policy_must_hash_to_its_name() {
        let party = agent_logic::control_store::testing::party_for(SEED, "agent-1").unwrap();
        let policy = first(&party, DEFAULT_LINKED_RULES, now());
        let item = PendingPolicy {
            policy_hash: policy.hash.to_string(),
            document: policy.document.clone(),
            controller_id: String::new(),
            proof: policy.proof.clone(),
            submitted_at: None,
        };
        assert_eq!(Candidate::from_pending(&item).unwrap(), policy);
        let renamed = PendingPolicy {
            policy_hash: Hash32([3; 32]).to_string(),
            ..item
        };
        assert!(
            Candidate::from_pending(&renamed)
                .unwrap_err()
                .to_string()
                .contains("hashes to")
        );
    }
}
