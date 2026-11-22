//! Deterministic keys, controllers and WebAuthn assertions for the unit tests.

use ::p256::ecdsa::signature::Signer as _;
use ::p256::ecdsa::{Signature, SigningKey as P256SigningKey};
use sha2::{Digest, Sha256};

use crate::builder::{EntryBuilder, PendingEntry};
use crate::canon::time::UtcTime;
use crate::canon::values::{
    Hash32, Label, Network, Nonce16, Origin, OwnerAccount, RpId, encode_b64url,
};
use crate::controller::{
    BackupPolicy, Capability, CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey,
    ControllerType, WebAuthnKey, controller_id_for,
};
use crate::entry::SignedEntry;
use crate::link::{LinkDescriptor, LinkPurpose, RpPolicy};
use crate::log::{VerifiedLog, verify_log};
use crate::party::{PartyId, namespace_for_key};
use crate::policy::{DEFAULT_LINKED_RULES, PolicyDocument, Rules};
use crate::proof::{ControllerProof, ProofEnvelope};
use crate::sig::p256::P256_SPKI_PREFIX;
use crate::sig::webauthn::{FLAG_BE, FLAG_BS, FLAG_UP, FLAG_UV, WebAuthnAssertion};
use crate::signable::sign_agent;
use crate::state::{ControlState, PolicyDocuments, replay};

pub(crate) const RP_ID: &str = "silvana.link";
pub(crate) const ORIGIN: &str = "https://app.silvana.link";

/// 32 bytes counting up from `start`.
pub(crate) fn seed(start: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = start.wrapping_add(i as u8);
    }
    out
}

pub(crate) fn ed_key(start: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&seed(start))
}

pub(crate) fn agent_key() -> ed25519_dalek::SigningKey {
    ed_key(0x01)
}

pub(crate) fn agent_party() -> PartyId {
    let key = agent_key().verifying_key().to_bytes();
    PartyId::parse(&format!("agent-1::{}", namespace_for_key(&key))).unwrap()
}

pub(crate) fn p256_key_from(start: u8) -> P256SigningKey {
    P256SigningKey::from_slice(&seed(start)).unwrap()
}

pub(crate) fn p256_key() -> P256SigningKey {
    p256_key_from(0x41)
}

pub(crate) fn p256_spki(key: &P256SigningKey) -> Vec<u8> {
    let point = key.verifying_key().to_encoded_point(false);
    let mut out = P256_SPKI_PREFIX.to_vec();
    out.truncate(26);
    out.extend_from_slice(point.as_bytes());
    out
}

/// The same signature with s replaced by n - s when s is low.
pub(crate) fn high_s(signature: &Signature) -> Signature {
    if signature.normalize_s().is_some() {
        return *signature;
    }
    let (r, s) = signature.split_scalars();
    Signature::from_scalars(r, -s).unwrap()
}

pub(crate) fn auth_data(rp_id: &str, flags: u8, counter: u32) -> Vec<u8> {
    let mut out = Sha256::digest(rp_id.as_bytes()).to_vec();
    out.push(flags);
    out.extend_from_slice(&counter.to_be_bytes());
    out
}

pub(crate) fn client_data(challenge: &[u8; 32], origin: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"{origin}","crossOrigin":false}}"#,
        encode_b64url(challenge)
    )
    .into_bytes()
}

pub(crate) fn sign_webauthn(key: &P256SigningKey, auth_data: &[u8], client_data: &[u8]) -> Vec<u8> {
    let mut message = auth_data.to_vec();
    message.extend_from_slice(&Sha256::digest(client_data));
    let signature: Signature = key.sign(&message);
    signature.to_der().as_bytes().to_vec()
}

/// A synced-passkey assertion over SHA-256(`payload`) by the default P-256 key.
pub(crate) fn assertion_over(payload: &[u8]) -> WebAuthnAssertion {
    let challenge: [u8; 32] = Sha256::digest(payload).into();
    let authenticator_data = auth_data(RP_ID, FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS, 0);
    let client_data_json = client_data(&challenge, ORIGIN);
    let signature = sign_webauthn(&p256_key(), &authenticator_data, &client_data_json);
    WebAuthnAssertion {
        authenticator_data,
        client_data_json,
        signature,
    }
}

pub(crate) fn ed25519_controller_from(
    start: u8,
    capabilities: CapabilitySet,
) -> ControllerDescriptor {
    let key = ControllerKey::Ed25519(ed_key(start).verifying_key().to_bytes());
    ControllerDescriptor::new(key, Label::parse("Recovery key").unwrap(), capabilities).unwrap()
}

pub(crate) fn ed25519_controller(capabilities: CapabilitySet) -> ControllerDescriptor {
    ed25519_controller_from(0x21, capabilities)
}

fn passkey(start: u8, rp_id: &str, origin: &str) -> WebAuthnKey {
    let spki = p256_spki(&p256_key_from(start));
    let credential_id = seed(start.wrapping_add(0x80)).to_vec();
    let rp_id = RpId::parse(rp_id).unwrap();
    let origins = vec![Origin::parse(origin).unwrap()];
    WebAuthnKey::new(&spki, credential_id, rp_id, origins, BackupPolicy::Any).unwrap()
}

pub(crate) fn passkey_controller_from(
    start: u8,
    capabilities: CapabilitySet,
) -> ControllerDescriptor {
    let key = ControllerKey::WebAuthn(passkey(start, RP_ID, ORIGIN));
    ControllerDescriptor::new(key, Label::parse("Laptop passkey").unwrap(), capabilities).unwrap()
}

pub(crate) fn passkey_controller(capabilities: CapabilitySet) -> ControllerDescriptor {
    passkey_controller_from(0x41, capabilities)
}

pub(crate) fn localhost_controller() -> ControllerDescriptor {
    let key = ControllerKey::WebAuthn(passkey(0x41, "localhost", "http://localhost:3000"));
    ControllerDescriptor::new(
        key,
        Label::parse("Local passkey").unwrap(),
        CapabilitySet::ALL,
    )
    .unwrap()
}

/// A devnet invite: a passkey with policy and direct, an Ed25519 key with read.
pub(crate) fn invite_descriptor() -> LinkDescriptor {
    LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::Link,
        OwnerAccount::new(42).unwrap(),
        None,
        Nonce16([0x5a; 16]),
        UtcTime::parse("2026-11-29T12:00:00Z").unwrap(),
        vec![
            passkey_controller(CapabilitySet::of(&[Capability::Policy, Capability::Direct])),
            ed25519_controller(CapabilitySet::of(&[Capability::Read])),
        ],
    )
    .unwrap()
}

pub(crate) const T0: &str = "2026-11-22T12:00:00Z";
pub(crate) const DAY: i64 = 86_400;

pub(crate) fn t(s: &str) -> UtcTime {
    UtcTime::parse(s).unwrap()
}

/// `T0` plus `secs`.
pub(crate) fn at(secs: i64) -> UtcTime {
    t(T0).checked_add_secs(secs).unwrap()
}

pub(crate) fn rp() -> RpPolicy {
    RpPolicy::for_network(Network::Devnet)
}

pub(crate) fn empty_state() -> ControlState {
    ControlState::empty(
        Network::Devnet,
        agent_party(),
        agent_key().verifying_key().to_bytes(),
    )
}

pub(crate) fn ed25519_id(start: u8) -> ControllerId {
    controller_id_for(
        ControllerType::Ed25519,
        &ed_key(start).verifying_key().to_bytes(),
    )
}

pub(crate) fn passkey_id() -> ControllerId {
    *passkey_controller(CapabilitySet::ALL).id()
}

/// A `link` descriptor for this agent, expiring a week after `T0`.
pub(crate) fn link_descriptor(controllers: Vec<ControllerDescriptor>, nonce: u8) -> LinkDescriptor {
    LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::Link,
        OwnerAccount::new(42).unwrap(),
        Some(agent_party()),
        Nonce16([nonce; 16]),
        at(7 * DAY),
        controllers,
    )
    .unwrap()
}

/// An `add_controller` descriptor for this agent.
pub(crate) fn add_descriptor(controller: ControllerDescriptor, owner: u64) -> LinkDescriptor {
    LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::AddController,
        OwnerAccount::new(owner).unwrap(),
        Some(agent_party()),
        Nonce16([0x33; 16]),
        at(7 * DAY),
        vec![controller],
    )
    .unwrap()
}

pub(crate) fn sign_pending(pending: &PendingEntry) -> SignedEntry {
    pending
        .finish(sign_agent(&agent_key(), pending.signable()))
        .unwrap()
}

/// A policy for this agent that chains onto the latest accepted one.
pub(crate) fn policy_doc(
    state: &ControlState,
    version: u64,
    issued_at: UtcTime,
    rules: Rules,
) -> PolicyDocument {
    let latest = state.latest_policy();
    PolicyDocument {
        network: Network::Devnet,
        agent_party: agent_party(),
        version,
        prev_policy_hash: latest.map_or(Hash32([0; 32]), |p| *p.hash()),
        issued_at,
        expires_at: issued_at.checked_add_secs(3600).unwrap(),
        not_before: None,
        rules,
    }
}

pub(crate) fn ed25519_envelope(start: u8, payload: &[u8]) -> ProofEnvelope {
    let signature = ed_key(start).sign(payload).to_bytes();
    ProofEnvelope::new(
        ed25519_id(start),
        payload,
        ControllerProof::Ed25519 { signature },
    )
}

pub(crate) fn passkey_envelope(payload: &[u8]) -> ProofEnvelope {
    ProofEnvelope::new(
        passkey_id(),
        payload,
        ControllerProof::WebAuthn(assertion_over(payload)),
    )
}

/// The default rules with transfers no longer directed-only (a transfers widening).
pub(crate) fn undirected_rules() -> Rules {
    let mut rules = DEFAULT_LINKED_RULES;
    rules.transfers.directed_only = false;
    rules
}

/// A signed log under construction, with its replayed state and policy documents.
pub(crate) struct Chain {
    pub(crate) state: ControlState,
    pub(crate) entries: Vec<SignedEntry>,
    pub(crate) docs: PolicyDocuments,
}

impl Chain {
    pub(crate) fn new() -> Self {
        Self {
            state: empty_state(),
            entries: Vec::new(),
            docs: PolicyDocuments::new(),
        }
    }

    /// Onboarded from the invite at `T0`: passkey (policy, direct) and Ed25519 0x21 (read).
    pub(crate) fn linked() -> Self {
        let mut chain = Self::new();
        let invite = invite_descriptor();
        let bytes = invite.encode().unwrap();
        let commitment = invite.commitment().unwrap();
        let pending = chain
            .builder()
            .link(&bytes, &commitment, &rp(), true, at(0))
            .unwrap();
        chain.push(&pending);
        chain
    }

    pub(crate) fn builder(&self) -> EntryBuilder<'_> {
        EntryBuilder::new(&self.state)
    }

    pub(crate) fn push(&mut self, pending: &PendingEntry) -> SignedEntry {
        let signed = sign_pending(pending);
        if let Some(document) = pending.policy_document() {
            self.docs.insert(document.to_vec());
        }
        self.state = pending.state_after().clone();
        self.entries.push(signed.clone());
        signed
    }

    pub(crate) fn log(&self) -> VerifiedLog {
        verify_log(Network::Devnet, &agent_party(), &self.entries).unwrap()
    }

    pub(crate) fn replayed(&self) -> ControlState {
        replay(&self.log(), &self.docs).unwrap()
    }

    /// Signs a policy with the given Ed25519 controller and accepts it at `when`.
    pub(crate) fn accept_ed25519(
        &mut self,
        start: u8,
        doc: &PolicyDocument,
        when: UtcTime,
    ) -> SignedEntry {
        let bytes = doc.encode().unwrap();
        let envelope = ed25519_envelope(start, &bytes);
        let pending = self
            .builder()
            .accept_policy(&bytes, &envelope, when)
            .unwrap();
        self.push(&pending)
    }

    /// Signs a policy with the passkey and accepts it at `when`.
    pub(crate) fn accept_passkey(&mut self, doc: &PolicyDocument, when: UtcTime) -> SignedEntry {
        let bytes = doc.encode().unwrap();
        let envelope = passkey_envelope(&bytes);
        let pending = self
            .builder()
            .accept_policy(&bytes, &envelope, when)
            .unwrap();
        self.push(&pending)
    }
}
