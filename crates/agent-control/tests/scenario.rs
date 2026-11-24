//! One control lifecycle through the public API: link, policies, a directed command,
//! a forced removal and a re-link, then a hidden tail and a fork.

use agent_control::canon::values::encode_b64url;
use agent_control::policy::{LimitItem, Limits, PolicyInstrument};
use agent_control::sig::p256::P256_SPKI_PREFIX;
use agent_control::sig::webauthn::{FLAG_BE, FLAG_BS, FLAG_UP, FLAG_UV};
use agent_control::{
    Amount, BackupPolicy, Capability, CapabilitySet, Command, CommandOp, ControlState,
    ControllerDescriptor, ControllerId, ControllerKey, ControllerProof, DEFAULT_LINKED_RULES,
    EntryBuilder, EvalContext, Flow, ForkEvidence, Hash32, HeadCheck, HeadClaim, HeadRef,
    InstrumentId, InstrumentRef, Intent, Label, LinkDescriptor, LinkPurpose, LogComparison,
    Network, Nonce16, OperationClass, OperationKind, Origin, OwnerAccount, PartyId, PendingEntry,
    PlatformParties, PolicyDocument, PolicyDocuments, ProofEnvelope, ReasonCode, Receipt,
    ReceiptOutcome, ReceiptSubject, Recipient, RpId, RpPolicy, Rules, SignedEntry, Text, Token,
    UsageBucket, UsageTotals, UtcTime, Uuid7, VerifiedLog, WebAuthnKey, check_head, command_matches,
    commitment_of, compare_logs, controller_change_delay_secs, decode_entries, evaluate,
    force_confirmation_phrase, format_link_code, namespace_for_key, parse_link_code, replay,
    sign_agent, verify_command, verify_link_descriptor, verify_log, verify_policy_submission,
    verify_read_session,
};
use ed25519_dalek::SigningKey;
use p256::ecdsa::signature::Signer as _;
use sha2::{Digest, Sha256};

const DAY: i64 = 86_400;
const RP_ID: &str = "silvana.link";
const ORIGIN: &str = "https://app.silvana.link";

fn seed(start: u8) -> [u8; 32] {
    std::array::from_fn(|i| start.wrapping_add(u8::try_from(i).unwrap()))
}

fn t(secs: i64) -> UtcTime {
    UtcTime::parse("2026-11-22T12:00:00Z")
        .unwrap()
        .checked_add_secs(secs)
        .unwrap()
}

fn party(hint: &str, key: &[u8; 32]) -> PartyId {
    PartyId::parse(&format!("{hint}::{}", namespace_for_key(key))).unwrap()
}

fn caps(list: &[Capability]) -> CapabilitySet {
    CapabilitySet::of(list)
}

fn amount(s: &str) -> Amount {
    Amount::parse(s).unwrap()
}

/// A passkey made on the page, signing assertions as a browser would.
struct Passkey {
    key: p256::ecdsa::SigningKey,
}

impl Passkey {
    fn new(start: u8) -> Self {
        Self {
            key: p256::ecdsa::SigningKey::from_slice(&seed(start)).unwrap(),
        }
    }

    fn spki(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut spki = P256_SPKI_PREFIX[..26].to_vec();
        spki.extend_from_slice(point.as_bytes());
        spki
    }

    fn controller(&self, capabilities: CapabilitySet) -> ControllerDescriptor {
        let key = WebAuthnKey::new(
            &self.spki(),
            seed(0xc1).to_vec(),
            RpId::parse(RP_ID).unwrap(),
            vec![Origin::parse(ORIGIN).unwrap()],
            BackupPolicy::Any,
        )
        .unwrap();
        ControllerDescriptor::new(
            ControllerKey::WebAuthn(key),
            Label::parse("Laptop passkey").unwrap(),
            capabilities,
        )
        .unwrap()
    }

    fn envelope(&self, id: ControllerId, payload: &[u8]) -> ProofEnvelope {
        let challenge = Sha256::digest(payload);
        let client_data_json = format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{ORIGIN}","crossOrigin":false}}"#,
            encode_b64url(&challenge)
        )
        .into_bytes();
        let mut authenticator_data = Sha256::digest(RP_ID.as_bytes()).to_vec();
        authenticator_data.push(FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS);
        authenticator_data.extend_from_slice(&0u32.to_be_bytes());
        let mut message = authenticator_data.clone();
        message.extend_from_slice(&Sha256::digest(&client_data_json));
        let signature: p256::ecdsa::Signature = self.key.sign(&message);
        let assertion = agent_control::WebAuthnAssertion {
            authenticator_data,
            client_data_json,
            signature: signature.to_der().as_bytes().to_vec(),
        };
        ProofEnvelope::new(id, payload, ControllerProof::WebAuthn(assertion))
    }
}

/// The agent's own copy: its key, the signed entries and every accepted policy document.
struct Agent {
    key: SigningKey,
    party: PartyId,
    state: ControlState,
    entries: Vec<SignedEntry>,
    documents: Vec<(Hash32, Vec<u8>)>,
}

impl Agent {
    fn new() -> Self {
        let key = SigningKey::from_bytes(&seed(0x01));
        let public = key.verifying_key().to_bytes();
        let party = party("agent-1", &public);
        let state = ControlState::empty(Network::Devnet, party.clone(), public);
        Self {
            key,
            party,
            state,
            entries: Vec::new(),
            documents: Vec::new(),
        }
    }

    fn builder(&self) -> EntryBuilder<'_> {
        EntryBuilder::new(&self.state)
    }

    /// Signs with the agent key alone and appends, as after the rpc accepted the upload.
    fn append(&mut self, pending: &PendingEntry) -> SignedEntry {
        let signed = pending
            .finish(sign_agent(&self.key, pending.signable()))
            .unwrap();
        if let Some(document) = pending.policy_document() {
            let hash = Hash32(Sha256::digest(document).into());
            self.documents.push((hash, document.to_vec()));
        }
        self.state = pending.state_after().clone();
        self.entries.push(signed.clone());
        signed
    }

    fn log(&self) -> VerifiedLog {
        verify_log(Network::Devnet, &self.party, &self.entries).unwrap()
    }

    fn docs(&self) -> PolicyDocuments {
        let mut docs = PolicyDocuments::new();
        for (hash, bytes) in &self.documents {
            docs.insert_keyed(*hash, bytes.clone());
        }
        docs
    }

    fn policy(&self, version: u64, issued_at: UtcTime, rules: Rules) -> PolicyDocument {
        PolicyDocument {
            network: Network::Devnet,
            agent_party: self.party.clone(),
            version,
            prev_policy_hash: self
                .state
                .latest_policy()
                .map_or(Hash32([0; 32]), |p| *p.hash()),
            issued_at,
            expires_at: issued_at.checked_add_secs(3600).unwrap(),
            not_before: None,
            rules,
        }
    }
}

fn version_at(state: &ControlState, now: UtcTime) -> Option<u64> {
    state.rules_at(now).policy().map(|p| p.version())
}

fn cc_cap(value: &str) -> Limits {
    Limits {
        unlisted: agent_control::policy::Unlisted::Unlimited,
        items: vec![LimitItem {
            instrument: PolicyInstrument::Cc,
            amount: amount(value),
        }],
    }
}

#[test]
fn a_control_lifecycle() {
    let delay = i64::try_from(controller_change_delay_secs(Network::Devnet)).unwrap();
    assert_eq!(delay, 3600);
    assert_eq!(controller_change_delay_secs(Network::Mainnet), 86_400);
    let mut agent = Agent::new();
    let passkey = Passkey::new(0x41);
    let recovery_key = SigningKey::from_bytes(&seed(0x21));
    let recovery = ControllerDescriptor::new(
        ControllerKey::Ed25519(recovery_key.verifying_key().to_bytes()),
        Label::parse("Recovery key").unwrap(),
        caps(&[Capability::Read]),
    )
    .unwrap();
    let laptop = passkey.controller(caps(&[Capability::Policy, Capability::Direct]));
    let (passkey_id, recovery_id) = (*laptop.id(), *recovery.id());
    let rp = RpPolicy::for_network(Network::Devnet);

    // 1. The page builds a descriptor; its code carries only the commitment.
    let descriptor = LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::Link,
        OwnerAccount::new(42).unwrap(),
        Some(agent.party.clone()),
        Nonce16([0x5a; 16]),
        t(7 * DAY),
        vec![laptop.clone(), recovery.clone()],
    )
    .unwrap();
    let bytes = descriptor.encode().unwrap();
    let code = format_link_code(&commitment_of(&bytes));
    assert!(code.starts_with("LNK-") && code.len() == 30);
    let commitment = parse_link_code(&code.to_lowercase()).unwrap();
    assert_eq!(commitment, descriptor.commitment().unwrap());
    assert_eq!(passkey_id.fingerprint().len(), 24);
    let mut swapped = bytes.clone();
    let last = swapped.len() - 2;
    swapped[last] ^= 1;
    let refused = verify_link_descriptor(
        &swapped,
        &commitment,
        Network::Devnet,
        Some(&agent.party),
        false,
        &rp,
        t(0),
    )
    .unwrap_err();
    assert_eq!(refused.code_name(), "commitment_mismatch");

    // 2. The agent alone signs the link; the controllers take effect one delay later (C18).
    let pending = agent
        .builder()
        .link(&bytes, &commitment, &rp, false, t(0))
        .unwrap();
    assert_eq!(pending.entry().effective_at(), t(delay));
    agent.append(&pending);
    assert_eq!(replay(&agent.log(), &agent.docs()).unwrap(), agent.state);
    assert_eq!(agent.state.owner_account(), OwnerAccount::new(42).ok());
    for capability in Capability::ALL {
        assert!(agent.state.holders(capability, t(delay - 1)).is_empty());
        assert_eq!(agent.state.holders(capability, t(delay)).len(), 1);
    }
    let early = agent
        .policy(1, t(delay - 60), DEFAULT_LINKED_RULES)
        .encode()
        .unwrap();
    let refused = verify_policy_submission(
        &agent.state,
        &early,
        &passkey.envelope(passkey_id, &early),
        t(delay - 60),
    )
    .unwrap_err();
    assert_eq!(refused.code_name(), "signer_lacks_policy");

    // 3. A widening policy is scheduled for the transfers delay, then in force.
    let t1 = t(delay + 3600);
    let mut widened = DEFAULT_LINKED_RULES;
    widened.transfers.directed_only = false;
    widened.transfers.per_transfer = cc_cap("100");
    let v1 = agent.policy(1, t1, widened.clone()).encode().unwrap();
    let envelope = passkey.envelope(passkey_id, &v1);
    let submitted = verify_policy_submission(&agent.state, &v1, &envelope, t1).unwrap();
    assert!(submitted.widening.transfers && !submitted.widening.trading);
    assert_eq!(submitted.required_delay_secs, 86_400);
    assert_eq!(submitted.effective_at, t1.checked_add_secs(DAY).unwrap());
    let pending = agent.builder().accept_policy(&v1, &envelope, t1).unwrap();
    agent.append(&pending);
    assert_eq!(version_at(&agent.state, t1), None);
    assert_eq!(
        agent.state.scheduled_policy(t1).map(|p| p.version()),
        Some(1)
    );
    let in_force = t1.checked_add_secs(DAY).unwrap();
    assert_eq!(version_at(&agent.state, in_force), Some(1));
    assert!(
        !agent
            .state
            .rules_at(in_force)
            .rules()
            .transfers
            .directed_only
    );

    // 4. A second widening is scheduled; a narrowing accepted during its delay cancels it.
    let t2 = in_force.checked_add_secs(60).unwrap();
    let mut wider = widened.clone();
    wider.transfers.per_transfer = cc_cap("1000");
    let v2 = agent.policy(2, t2, wider).encode().unwrap();
    let pending = agent
        .builder()
        .accept_policy(&v2, &passkey.envelope(passkey_id, &v2), t2)
        .unwrap();
    agent.append(&pending);
    assert_eq!(
        agent.state.scheduled_policy(t2).map(|p| p.version()),
        Some(2)
    );
    let t3 = t2.checked_add_secs(600).unwrap();
    let mut narrowed = widened;
    narrowed.transfers.directed_only = true;
    let v3 = agent.policy(3, t3, narrowed).encode().unwrap();
    let pending = agent
        .builder()
        .accept_policy(&v3, &passkey.envelope(passkey_id, &v3), t3)
        .unwrap();
    assert_eq!(pending.entry().effective_at(), t3);
    agent.append(&pending);
    assert!(agent.state.scheduled_policy(t3).is_none());
    assert_eq!(version_at(&agent.state, t3), Some(3));
    assert_eq!(
        version_at(&agent.state, t3.checked_add_secs(3 * DAY).unwrap()),
        Some(3)
    );
    assert!(agent.state.rules_at(t3).rules().transfers.directed_only);

    // 5. A directed transfer_cc: verified, matched against the content, evaluated, receipted.
    let t4 = t3.checked_add_secs(600).unwrap();
    let head = agent.state.head().unwrap();
    let alice = party("alice", &[3; 32]);
    let bob = party("bob", &[4; 32]);
    let command = Command {
        network: Network::Devnet,
        agent_party: agent.party.clone(),
        command_id: Uuid7::parse("01890a5d-ac96-774b-bcce-b302099a8057").unwrap(),
        log_seq: head.seq,
        log_head: head.hash,
        issued_at: t4,
        expires_at: t4.checked_add_secs(600).unwrap(),
        op: CommandOp::TransferCc {
            receiver: alice.clone(),
            amount: amount("10"),
            description: Some(Text::parse("rent").unwrap()),
            settlement_proposal_id: None,
        },
        summary: Text::parse("Pay alice 10 CC").unwrap(),
    };
    let command_bytes = command.encode().unwrap();
    let verified = verify_command(
        &agent.state,
        &command_bytes,
        &passkey.envelope(passkey_id, &command_bytes),
        t4,
        None,
    )
    .unwrap();
    assert_eq!(
        verified.command().canton_command_id(),
        "ctl-01890a5d-ac96-774b-bcce-b302099a8057"
    );
    let cc = InstrumentRef {
        admin: party("DSO", &[1; 32]),
        id: InstrumentId::parse("Amulet").unwrap(),
    };
    let flow = |to: &PartyId, value: &str| Flow {
        instrument: cc.clone(),
        amount: amount(value),
        to: Some(Recipient::Party(to.clone())),
    };
    let intent = Intent {
        outgoing: vec![flow(&alice, "10"), flow(&agent.party, "2.5")],
        ..Intent::new(OperationKind::TransferCc)
    };
    let usage = UsageTotals::new();
    let platform = PlatformParties::NONE;
    let ctx = EvalContext {
        agent_party: &agent.party,
        cc: &cc,
        platform: &platform,
        now: t4,
        usage: &usage,
    };
    assert!(command_matches(verified.command(), &intent, &ctx));
    let rules = agent.state.rules_at(t4).rules().clone();
    let decision = evaluate(&rules, &ctx, &intent, Some(&verified));
    assert!(decision.allowed, "{decision:?}");
    assert_eq!(decision.class, OperationClass::Transfers);
    assert_eq!(decision.usage.len(), 1);
    assert_eq!(decision.usage[0].bucket, UsageBucket::Transfers);
    assert_eq!(decision.usage[0].amount, amount("10"));
    assert_eq!(
        evaluate(&rules, &ctx, &intent, None).reason,
        ReasonCode::DirectedOnly
    );
    let substituted = Intent {
        outgoing: vec![flow(&bob, "10")],
        ..Intent::new(OperationKind::TransferCc)
    };
    assert!(!command_matches(verified.command(), &substituted, &ctx));
    assert_eq!(
        evaluate(&rules, &ctx, &substituted, Some(&verified)).reason,
        ReasonCode::CommandMismatch
    );
    let receipt = Receipt {
        network: Network::Devnet,
        agent_party: agent.party.clone(),
        agent_key: agent.key.verifying_key().to_bytes(),
        subject: ReceiptSubject::Command(command.command_id.clone()),
        subject_hash: *verified.hash(),
        outcome: ReceiptOutcome::Executed,
        update_id: Some(Token::parse("update-0001").unwrap()),
        reason_code: None,
        code_source: None,
        log_seq: head.seq,
        issued_at: t4.checked_add_secs(30).unwrap(),
    };
    let signable = receipt.signable().unwrap();
    let signature = sign_agent(&agent.key, &signable);
    assert_eq!(
        Receipt::verify(signable.bytes(), &signature, &agent.party).unwrap(),
        receipt
    );
    assert_eq!(
        Receipt::verify(signable.bytes(), &signature, &bob)
            .unwrap_err()
            .code_name(),
        "wrong_party"
    );

    // The ledger-service path: a fresh replay of the stored rows gives the same state and verdict.
    let rows: Vec<(Vec<u8>, [u8; 64])> = agent
        .entries
        .iter()
        .map(|e| (e.bytes().to_vec(), *e.signature()))
        .collect();
    let stored = verify_log(
        Network::Devnet,
        &agent.party,
        &decode_entries(&rows).unwrap(),
    )
    .unwrap();
    let fresh = replay(&stored, &agent.docs()).unwrap();
    assert_eq!(fresh, agent.state);
    assert_eq!(
        evaluate(fresh.rules_at(t4).rules(), &ctx, &intent, Some(&verified)),
        decision
    );
    let mut partial = PolicyDocuments::new();
    partial.insert(v3.clone());
    assert_eq!(
        replay(&stored, &partial).unwrap_err().code_name(),
        "missing_policy_document"
    );
    let fork_base = agent.entries.len();

    // 6. A forced full removal: refused without force, then delayed, the policy frozen.
    let t5 = t4.checked_add_secs(3600).unwrap();
    let refused = agent
        .builder()
        .remove_controller(&passkey_id, false, t5)
        .unwrap_err();
    assert_eq!(refused.code_name(), "minimum_rule_violation");
    assert_eq!(
        force_confirmation_phrase(&agent.state),
        "freeze agent-1 policy v3"
    );
    let pending = agent
        .builder()
        .remove_controller(&passkey_id, true, t5)
        .unwrap();
    assert_eq!(
        pending.entry().effective_at(),
        t5.checked_add_secs(delay).unwrap()
    );
    agent.append(&pending);
    let t5b = t5.checked_add_secs(60).unwrap();
    let pending = agent
        .builder()
        .remove_controller(&recovery_id, true, t5b)
        .unwrap();
    agent.append(&pending);
    let stale = verify_command(
        &agent.state,
        &command_bytes,
        &passkey.envelope(passkey_id, &command_bytes),
        t5b,
        None,
    )
    .unwrap_err();
    assert_eq!(stale.code_name(), "stale_log_head");
    let record = agent.state.controller(&passkey_id).unwrap();
    let removed_at = t5.checked_add_secs(delay).unwrap();
    assert!(
        record
            .capabilities_at(removed_at.checked_add_secs(-1).unwrap())
            .contains(Capability::Direct)
    );
    assert!(record.capabilities_at(removed_at).is_empty());
    let all_removed = t5b.checked_add_secs(delay).unwrap();
    assert_eq!(
        version_at(&agent.state, all_removed.checked_add_secs(DAY).unwrap()),
        Some(3)
    );
    let v4 = agent
        .policy(4, all_removed, DEFAULT_LINKED_RULES)
        .encode()
        .unwrap();
    let frozen = agent
        .builder()
        .accept_policy(&v4, &passkey.envelope(passkey_id, &v4), all_removed)
        .unwrap_err();
    assert_eq!(frozen.code_name(), "unknown_controller");

    // 7. A re-link is refused until every removal has taken effect.
    let backup_key = SigningKey::from_bytes(&seed(0x81));
    let backup = ControllerDescriptor::new(
        ControllerKey::Ed25519(backup_key.verifying_key().to_bytes()),
        Label::parse("Backup key").unwrap(),
        CapabilitySet::ALL,
    )
    .unwrap();
    let relink = LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::Link,
        OwnerAccount::new(42).unwrap(),
        Some(agent.party.clone()),
        Nonce16([0x44; 16]),
        t(10 * DAY),
        vec![backup.clone()],
    )
    .unwrap();
    let relink_bytes = relink.encode().unwrap();
    let relink_commitment = commitment_of(&relink_bytes);
    let too_early = agent
        .builder()
        .link(
            &relink_bytes,
            &relink_commitment,
            &rp,
            false,
            all_removed.checked_add_secs(-1).unwrap(),
        )
        .unwrap_err();
    assert_eq!(too_early.code_name(), "link_while_controlled");
    let pending = agent
        .builder()
        .link(&relink_bytes, &relink_commitment, &rp, false, all_removed)
        .unwrap();
    agent.append(&pending);
    let relinked = agent.state.controller(backup.id()).unwrap();
    let effective = all_removed.checked_add_secs(delay).unwrap();
    assert!(
        relinked
            .capabilities_at(effective.checked_add_secs(-1).unwrap())
            .is_empty()
    );
    assert_eq!(relinked.capabilities_at(effective), CapabilitySet::ALL);
    assert!(t5.secs_until(effective) >= 2 * delay);
    assert!(agent.state.controller(&passkey_id).is_none());
    assert_eq!(replay(&agent.log(), &agent.docs()).unwrap(), agent.state);

    // 8. A server copy that hides the newest entries is detected, by log and by head claim.
    let local = agent.log();
    let hidden = verify_log(
        Network::Devnet,
        &agent.party,
        &agent.entries[..agent.entries.len() - 2],
    )
    .unwrap();
    assert_eq!(
        compare_logs(&local, &hidden).unwrap(),
        LogComparison::RemoteBehind {
            local: local.head(),
            remote: hidden.head()
        }
    );
    let claim = HeadClaim::for_state(&agent.state, effective);
    let claim_signable = claim.signable().unwrap();
    let claim_signature = sign_agent(&agent.key, &claim_signable);
    let claimed = HeadClaim::verify(
        claim_signable.bytes(),
        &claim_signature,
        Network::Devnet,
        &agent.party,
    )
    .unwrap()
    .head
    .unwrap();
    assert_eq!(
        check_head(&hidden, &claimed),
        HeadCheck::LogBehind {
            head: hidden.head()
        }
    );
    assert_eq!(check_head(&local, &claimed), HeadCheck::Matches);
    let from_token =
        HeadRef::from_jwt_claims(Some(claimed.seq), Some(&claimed.hash.to_string())).unwrap();
    assert_eq!(from_token, Some(claimed));

    // 9. A second install signing another entry at the same seq yields verifiable fork evidence.
    let prefix = verify_log(Network::Devnet, &agent.party, &agent.entries[..fork_base]).unwrap();
    let prefix_state = replay(&prefix, &agent.docs()).unwrap();
    let other = SigningKey::from_bytes(&seed(0x84))
        .verifying_key()
        .to_bytes();
    let pending = EntryBuilder::new(&prefix_state)
        .add_ed25519(
            other,
            Label::parse("Other key").unwrap(),
            caps(&[Capability::Read]),
            t5,
        )
        .unwrap();
    let alternative = pending
        .finish(sign_agent(&agent.key, pending.signable()))
        .unwrap();
    let mut forked = agent.entries[..fork_base].to_vec();
    forked.push(alternative);
    let remote = verify_log(Network::Devnet, &agent.party, &forked).unwrap();
    let LogComparison::Fork(evidence) = compare_logs(&local, &remote).unwrap() else {
        panic!("expected a fork");
    };
    assert_eq!(evidence.seq(), u64::try_from(fork_base).unwrap() + 1);
    evidence.verify(Network::Devnet, &agent.party).unwrap();
    let rebuilt = ForkEvidence::new(
        SignedEntry::new(
            evidence.local().bytes().to_vec(),
            *evidence.local().signature(),
        )
        .unwrap(),
        SignedEntry::new(
            evidence.remote().bytes().to_vec(),
            *evidence.remote().signature(),
        )
        .unwrap(),
    );
    assert_eq!(rebuilt, *evidence);
    assert_eq!(
        evidence
            .verify(Network::Mainnet, &agent.party)
            .unwrap_err()
            .code_name(),
        "wrong_network"
    );
}

#[test]
fn a_read_session_spans_agents_that_share_a_reader() {
    let delay = i64::try_from(controller_change_delay_secs(Network::Devnet)).unwrap();
    let passkey = Passkey::new(0x41);
    let reader = passkey.controller(CapabilitySet::ALL);
    let mut states = Vec::new();
    for n in 1..=3u8 {
        let key = SigningKey::from_bytes(&seed(n));
        let public = key.verifying_key().to_bytes();
        let agent_party = party(&format!("agent-{n}"), &public);
        let state = ControlState::empty(Network::Devnet, agent_party.clone(), public);
        let descriptor = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(42).unwrap(),
            Some(agent_party),
            Nonce16([n; 16]),
            t(7 * DAY),
            vec![reader.clone()],
        )
        .unwrap();
        let bytes = descriptor.encode().unwrap();
        let pending = EntryBuilder::new(&state)
            .link(
                &bytes,
                &commitment_of(&bytes),
                &RpPolicy::for_network(Network::Devnet),
                false,
                t(0),
            )
            .unwrap();
        pending
            .finish(sign_agent(&key, pending.signable()))
            .unwrap();
        states.push(pending.state_after().clone());
    }
    let session_key = Passkey::new(0x61);
    let request = agent_control::ReadSessionRequest::new(
        Network::Devnet,
        *reader.id(),
        agent_control::parse_p256_spki(&session_key.spki()).unwrap(),
        Nonce16([0x77; 16]),
        t(delay),
        t(delay + 3600),
        states
            .iter()
            .rev()
            .map(|s| s.agent_party().clone())
            .collect(),
    );
    let bytes = request.encode().unwrap();
    let refs: Vec<&ControlState> = states.iter().collect();
    let envelope = passkey.envelope(*reader.id(), &bytes);
    let session = verify_read_session(&bytes, &envelope, &refs, t(delay)).unwrap();
    assert_eq!(session.request.agents.len(), 3);
    let pending = verify_read_session(&bytes, &envelope, &refs, t(delay - 1)).unwrap_err();
    assert_eq!(pending.code_name(), "signer_lacks_read");
    let issued_at_ms = u64::try_from(t(delay + 60).unix_secs()).unwrap() * 1000;
    let read = agent_control::ReadRequest {
        network: Network::Devnet,
        session_id: session.session_id,
        method: Token::parse("ListAgentActivity").unwrap(),
        agent_party: states[1].agent_party().clone(),
        range_offset: 0,
        range_limit: 50,
        issued_at_ms,
        nonce: Nonce16([0x88; 16]),
    };
    let read_bytes = read.encode().unwrap();
    let signature: p256::ecdsa::Signature = session_key.key.sign(&read_bytes);
    let checked = agent_control::verify_read_request(
        &session,
        &read_bytes,
        &signature.to_bytes(),
        issued_at_ms + 500,
    );
    assert_eq!(checked.unwrap(), read);
}
