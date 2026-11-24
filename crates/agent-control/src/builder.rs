//! Builds the next log entry from the current state; only the agent key may sign it.

use std::fmt;

use crate::canon::time::UtcTime;
use crate::canon::values::{Commitment, Hash32, Label, sha256};
use crate::controller::{CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey};
use crate::entry::{
    AcceptPolicyBody, AddControllerBody, DescriptorRef, EntryBody, LinkBody, LogEntry,
    RemoveControllerBody, SetCapabilitiesBody, SignedEntry,
};
use crate::error::{
    CanonError, LinkError, PolicyError, ReplayError, ReplayErrorKind, SignatureError,
};
use crate::link::{LinkPurpose, RpPolicy, verify_link_descriptor};
use crate::log::MAX_LOG_ENTRIES;
use crate::policy::verify_policy_submission;
use crate::proof::ProofEnvelope;
use crate::sig::ed25519::verify_ed25519_strict;
use crate::signable::AgentSignable;
use crate::state::{
    ControlState, ControllerChange, MinimumRuleOutcome, PolicyDocuments, add_secs, apply_entry,
    change_effective_at, controller_change_delay_secs, minimum_rule,
};

/// Why an entry could not be built or finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildError {
    Link(LinkError),
    Canon(CanonError),
    Replay(ReplayError),
    Policy(PolicyError),
    Signature(SignatureError),
    WrongPurpose,
    MinimumRule(MinimumRuleOutcome),
    LogFull,
}

impl BuildError {
    /// Stable name of the refusal; wrapped errors keep their own names.
    pub fn code_name(&self) -> &'static str {
        match self {
            Self::Link(e) => e.code_name(),
            Self::Canon(e) => e.code_name(),
            Self::Replay(e) => e.code_name(),
            Self::Policy(e) => e.code_name(),
            Self::Signature(e) => e.code_name(),
            Self::WrongPurpose => "wrong_purpose",
            Self::MinimumRule(_) => "minimum_rule_violation",
            Self::LogFull => "log_full",
        }
    }

    /// Stable numeric code (1000-1002, or the wrapped code).
    pub fn code(&self) -> u16 {
        match self {
            Self::Link(e) => e.code(),
            Self::Canon(e) => e.code(),
            Self::Replay(e) => e.code(),
            Self::Policy(e) => e.code(),
            Self::Signature(e) => e.code(),
            Self::WrongPurpose => 1000,
            Self::MinimumRule(_) => 1001,
            Self::LogFull => 1002,
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code_name())
    }
}

impl std::error::Error for BuildError {}

/// A built entry, already applied to a copy of the state, waiting for the agent signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEntry {
    entry: LogEntry,
    signable: AgentSignable,
    state: ControlState,
    policy_document: Option<Vec<u8>>,
}

impl PendingEntry {
    pub fn entry(&self) -> &LogEntry {
        &self.entry
    }

    pub fn signable(&self) -> &AgentSignable {
        &self.signable
    }

    /// The state once this entry is appended.
    pub fn state_after(&self) -> &ControlState {
        &self.state
    }

    /// The policy document an `accept_policy` entry references; it travels with the entry.
    pub fn policy_document(&self) -> Option<&[u8]> {
        self.policy_document.as_deref()
    }

    /// Accepts only a signature that verifies strictly under the entry's agent key.
    pub fn finish(&self, signature: [u8; 64]) -> Result<SignedEntry, BuildError> {
        verify_ed25519_strict(self.entry.agent_key(), self.signable.bytes(), &signature)
            .map_err(BuildError::Signature)?;
        Ok(SignedEntry::from_parts(
            self.entry.clone(),
            self.signable.bytes().to_vec(),
            signature,
        ))
    }
}

/// Fills seq, prev and effective_at, and checks the entry by replaying it on a copy.
pub struct EntryBuilder<'a> {
    state: &'a ControlState,
}

impl<'a> EntryBuilder<'a> {
    pub fn new(state: &'a ControlState) -> Self {
        Self { state }
    }

    fn replay_error(&self, kind: ReplayErrorKind) -> BuildError {
        let seq = self.state.next_seq().unwrap_or(u64::MAX);
        BuildError::Replay(ReplayError::new(seq, kind))
    }

    fn delayed(&self, created_at: UtcTime) -> Result<UtcTime, BuildError> {
        add_secs(created_at, controller_change_delay_secs(self.state.network()))
            .ok_or_else(|| self.replay_error(ReplayErrorKind::TimeOverflow))
    }

    /// The state with every change due at `created_at` applied.
    fn advanced(&self, created_at: UtcTime) -> ControlState {
        let mut state = self.state.clone();
        state.advance_to(created_at);
        state
    }

    /// `link` from a page descriptor; an invite (no party) only with `allow_invite`, into an empty log.
    pub fn link(
        &self,
        descriptor: &[u8],
        commitment: &Commitment,
        rp_policy: &RpPolicy,
        allow_invite: bool,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let state = self.state;
        let decoded = verify_link_descriptor(
            descriptor,
            commitment,
            state.network(),
            Some(state.agent_party()),
            allow_invite && !state.is_linked(),
            rp_policy,
            created_at,
        )
        .map_err(BuildError::Link)?;
        if decoded.purpose() != LinkPurpose::Link {
            return Err(BuildError::WrongPurpose);
        }
        let body = EntryBody::Link(LinkBody {
            owner_account: decoded.owner_account(),
            invite: decoded.agent_party().is_none(),
            nonce: *decoded.nonce(),
            expires_at: decoded.expires_at(),
            commitment: *commitment,
            controllers: decoded.controllers().to_vec(),
        });
        let effective_at = self.delayed(created_at)?;
        self.build(
            self.advanced(created_at),
            created_at,
            effective_at,
            body,
            None,
        )
    }

    /// `add_controller` from an `add_controller` descriptor naming this agent.
    pub fn add_controller_from_code(
        &self,
        descriptor: &[u8],
        commitment: &Commitment,
        rp_policy: &RpPolicy,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let state = self.state;
        let decoded = verify_link_descriptor(
            descriptor,
            commitment,
            state.network(),
            Some(state.agent_party()),
            false,
            rp_policy,
            created_at,
        )
        .map_err(BuildError::Link)?;
        if decoded.purpose() != LinkPurpose::AddController {
            return Err(BuildError::WrongPurpose);
        }
        let Some(controller) = decoded.controllers().first() else {
            return Err(BuildError::WrongPurpose);
        };
        let body = EntryBody::AddController(AddControllerBody {
            owner_account: decoded.owner_account(),
            descriptor: Some(DescriptorRef {
                nonce: *decoded.nonce(),
                expires_at: decoded.expires_at(),
                commitment: *commitment,
            }),
            controller: controller.clone(),
        });
        let effective_at = self.delayed(created_at)?;
        self.build(
            self.advanced(created_at),
            created_at,
            effective_at,
            body,
            None,
        )
    }

    /// `add_controller` for a local Ed25519 key, with no descriptor.
    pub fn add_ed25519(
        &self,
        public_key: [u8; 32],
        label: Label,
        capabilities: CapabilitySet,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let controller =
            ControllerDescriptor::new(ControllerKey::Ed25519(public_key), label, capabilities)
                .map_err(BuildError::Canon)?;
        let owner_account = self
            .state
            .owner_account()
            .ok_or_else(|| self.replay_error(ReplayErrorKind::FirstNotLink))?;
        let body = EntryBody::AddController(AddControllerBody {
            owner_account,
            descriptor: None,
            controller,
        });
        let effective_at = self.delayed(created_at)?;
        self.build(
            self.advanced(created_at),
            created_at,
            effective_at,
            body,
            None,
        )
    }

    /// The minimum rule, the forced flag and the C18 timing for a remove or a capability change.
    fn change(
        &self,
        change: ControllerChange,
        force: bool,
        created_at: UtcTime,
    ) -> Result<(ControlState, bool, UtcTime), BuildError> {
        let advanced = self.advanced(created_at);
        let outcome = minimum_rule(&advanced, &change).map_err(BuildError::Replay)?;
        if outcome.is_violation() && !force {
            return Err(BuildError::MinimumRule(outcome));
        }
        let record = advanced
            .controller(change.controller_id())
            .ok_or_else(|| self.replay_error(ReplayErrorKind::UnknownController))?;
        let effective_at = change_effective_at(self.state.network(), record, &change, created_at)
            .ok_or_else(|| self.replay_error(ReplayErrorKind::TimeOverflow))?;
        Ok((advanced, outcome.is_violation(), effective_at))
    }

    pub fn remove_controller(
        &self,
        id: &ControllerId,
        force: bool,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let (advanced, forced, effective_at) =
            self.change(ControllerChange::Remove(*id), force, created_at)?;
        let body = EntryBody::RemoveController(RemoveControllerBody {
            controller_id: *id,
            forced,
        });
        self.build(advanced, created_at, effective_at, body, None)
    }

    pub fn set_capabilities(
        &self,
        id: &ControllerId,
        capabilities: CapabilitySet,
        force: bool,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let change = ControllerChange::SetCapabilities(*id, capabilities);
        let (advanced, forced, effective_at) = self.change(change, force, created_at)?;
        let body = EntryBody::SetCapabilities(SetCapabilitiesBody {
            controller_id: *id,
            capabilities,
            forced,
        });
        self.build(advanced, created_at, effective_at, body, None)
    }

    /// `accept_policy` for a signed document that passes `verify_policy_submission` now.
    pub fn accept_policy(
        &self,
        policy: &[u8],
        envelope: &ProofEnvelope,
        created_at: UtcTime,
    ) -> Result<PendingEntry, BuildError> {
        let advanced = self.advanced(created_at);
        let verified = verify_policy_submission(&advanced, policy, envelope, created_at)
            .map_err(BuildError::Policy)?;
        let body = EntryBody::AcceptPolicy(AcceptPolicyBody {
            policy_hash: verified.hash,
            policy_version: verified.document.version,
            signed_by: verified.signer,
            proof: envelope.proof().clone(),
        });
        self.build(
            advanced,
            created_at,
            verified.effective_at,
            body,
            Some(verified.bytes),
        )
    }

    fn build(
        &self,
        mut advanced: ControlState,
        created_at: UtcTime,
        effective_at: UtcTime,
        body: EntryBody,
        policy_document: Option<Vec<u8>>,
    ) -> Result<PendingEntry, BuildError> {
        let seq = self.state.next_seq().ok_or(BuildError::LogFull)?;
        if usize::try_from(seq).map_or(true, |n| n > MAX_LOG_ENTRIES) {
            return Err(BuildError::LogFull);
        }
        let entry = LogEntry {
            network: self.state.network(),
            agent_party: self.state.agent_party().clone(),
            agent_key: *self.state.agent_key(),
            seq,
            prev: self.state.head().map_or(Hash32([0; 32]), |head| head.hash),
            created_at,
            effective_at,
            body,
        };
        let bytes = entry.encode().map_err(BuildError::Canon)?;
        let mut docs = PolicyDocuments::new();
        if let Some(document) = &policy_document {
            docs.insert(document.clone());
        }
        apply_entry(&mut advanced, &entry, &Hash32(sha256(&bytes)), &docs)
            .map_err(|kind| BuildError::Replay(ReplayError::new(seq, kind)))?;
        Ok(PendingEntry {
            entry,
            signable: AgentSignable::log_entry(bytes),
            state: advanced,
            policy_document,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::values::Network;
    use crate::controller::Capability;
    use crate::entry::EntryKind;
    use crate::error::CanonErrorKind;
    use crate::policy::DEFAULT_LINKED_RULES;
    use crate::signable::{SignablePurpose, sign_agent};
    use crate::testkit::{self, Chain, DAY, DEVNET_DELAY, MAINNET_DELAY, at};

    fn read_only() -> CapabilitySet {
        CapabilitySet::of(&[Capability::Read])
    }

    #[test]
    fn pending_entries_carry_the_signable_and_finish_strictly() {
        let chain = Chain::linked();
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let label = Label::parse("Backup key").unwrap();
        let pending = chain
            .builder()
            .add_ed25519(key, label, read_only(), at(DAY))
            .unwrap();
        assert_eq!(pending.signable().purpose(), SignablePurpose::LogEntry);
        assert_eq!(
            pending.signable().bytes(),
            pending.entry().encode().unwrap()
        );
        assert_eq!(pending.entry().seq(), 2);
        assert_eq!(&pending.entry().prev, chain.entries[0].hash());
        assert_eq!(pending.entry().kind(), EntryKind::AddController);
        assert!(pending.policy_document().is_none());
        let wrong = sign_agent(&testkit::ed_key(0x77), pending.signable());
        assert_eq!(
            pending.finish(wrong).unwrap_err(),
            BuildError::Signature(SignatureError::BadSignature)
        );
        let signed = pending
            .finish(sign_agent(&testkit::agent_key(), pending.signable()))
            .unwrap();
        assert_eq!(signed.entry(), pending.entry());
        assert_eq!(pending.state_after().head().unwrap().hash, *signed.hash());
    }

    #[test]
    fn descriptors_are_verified_before_anything_is_built() {
        let chain = Chain::linked();
        let rp = testkit::rp();
        let add = testkit::add_descriptor(testkit::passkey_controller_from(0x51, read_only()), 42);
        let add_bytes = add.encode().unwrap();
        let add_commitment = add.commitment().unwrap();
        let link = testkit::link_descriptor(
            vec![testkit::ed25519_controller_from(0x81, CapabilitySet::ALL)],
            0x44,
        );
        let link_bytes = link.encode().unwrap();
        let link_commitment = link.commitment().unwrap();
        let builder = chain.builder();
        assert_eq!(
            builder
                .link(&add_bytes, &add_commitment, &rp, false, at(DAY))
                .unwrap_err(),
            BuildError::WrongPurpose
        );
        assert_eq!(
            builder
                .add_controller_from_code(&link_bytes, &link_commitment, &rp, at(DAY))
                .unwrap_err(),
            BuildError::WrongPurpose
        );
        assert_eq!(
            builder
                .add_controller_from_code(&add_bytes, &link_commitment, &rp, at(DAY))
                .unwrap_err(),
            BuildError::Link(LinkError::CommitmentMismatch)
        );
        assert_eq!(
            builder
                .add_controller_from_code(&add_bytes, &add_commitment, &rp, at(8 * DAY))
                .unwrap_err(),
            BuildError::Link(LinkError::Expired)
        );
        let pending = builder
            .add_controller_from_code(&add_bytes, &add_commitment, &rp, at(DAY))
            .unwrap();
        assert_eq!(pending.entry().effective_at(), at(DAY + DEVNET_DELAY));
        let other_owner =
            testkit::add_descriptor(testkit::passkey_controller_from(0x51, read_only()), 7);
        let refused = builder.add_controller_from_code(
            &other_owner.encode().unwrap(),
            &other_owner.commitment().unwrap(),
            &rp,
            at(DAY),
        );
        assert_eq!(refused.unwrap_err().code_name(), "owner_mismatch");
        // An invite needs allow_invite, and only an empty log takes one.
        let invite = testkit::invite_descriptor();
        let empty = testkit::empty_state();
        let refused = EntryBuilder::new(&empty).link(
            &invite.encode().unwrap(),
            &invite.commitment().unwrap(),
            &rp,
            false,
            at(0),
        );
        assert_eq!(
            refused.unwrap_err(),
            BuildError::Link(LinkError::InviteNotAllowed)
        );
        let pending = EntryBuilder::new(&empty)
            .link(&link_bytes, &link_commitment, &rp, false, at(0))
            .unwrap();
        assert!(matches!(
            pending.entry().body(),
            EntryBody::Link(LinkBody { invite: false, .. })
        ));
    }

    #[test]
    fn the_minimum_rule_needs_force_and_sets_forced() {
        let chain = Chain::linked();
        let reader = testkit::ed25519_id(0x21);
        let refused = chain
            .builder()
            .remove_controller(&reader, false, at(2 * DAY))
            .unwrap_err();
        let BuildError::MinimumRule(outcome) = refused else {
            panic!("expected a minimum-rule refusal, got {refused:?}");
        };
        assert_eq!(outcome.orphaned(), read_only());
        assert_eq!(refused.code_name(), "minimum_rule_violation");
        let forced = chain
            .builder()
            .remove_controller(&reader, true, at(2 * DAY))
            .unwrap();
        assert!(matches!(
            forced.entry().body(),
            EntryBody::RemoveController(RemoveControllerBody { forced: true, .. })
        ));
        // Forcing a change that orphans nothing still records forced=false.
        let more = CapabilitySet::of(&[Capability::Policy, Capability::Read]);
        let unforced = chain
            .builder()
            .set_capabilities(&reader, more, true, at(2 * DAY))
            .unwrap();
        assert!(matches!(
            unforced.entry().body(),
            EntryBody::SetCapabilities(SetCapabilitiesBody { forced: false, .. })
        ));
        let unknown = testkit::ed25519_id(0x99);
        let refused = chain
            .builder()
            .remove_controller(&unknown, true, at(2 * DAY))
            .unwrap_err();
        assert_eq!(refused.code_name(), "unknown_controller");
    }

    #[test]
    fn the_delay_comes_from_the_state_network() {
        for (network, delay) in [
            (Network::Devnet, DEVNET_DELAY),
            (Network::Mainnet, MAINNET_DELAY),
        ] {
            let mut chain = Chain::linked_on(network);
            assert_eq!(chain.entries[0].effective_at(), at(delay));
            let key = testkit::ed_key(0x81).verifying_key().to_bytes();
            let label = Label::parse("Backup key").unwrap();
            let add = chain
                .builder()
                .add_ed25519(key, label, read_only(), at(2 * DAY))
                .unwrap();
            assert_eq!(add.entry().effective_at(), at(2 * DAY + delay));
            let reader = testkit::ed25519_id(0x21);
            let narrowed = chain
                .builder()
                .set_capabilities(&reader, read_only(), true, at(2 * DAY))
                .unwrap();
            assert_eq!(narrowed.entry().effective_at(), at(2 * DAY + delay));
            // A withdrawal still applies at once.
            chain.push(&add);
            let withdrawn = chain
                .builder()
                .remove_controller(&testkit::ed25519_id(0x81), false, at(2 * DAY + 10))
                .unwrap();
            assert_eq!(withdrawn.entry().effective_at(), at(2 * DAY + 10));
        }
    }

    #[test]
    fn accept_policy_travels_with_its_document() {
        let chain = Chain::linked();
        let doc = testkit::policy_doc(&chain.state, 1, at(2 * DAY), DEFAULT_LINKED_RULES);
        let bytes = doc.encode().unwrap();
        let envelope = testkit::passkey_envelope(&bytes);
        let pending = chain
            .builder()
            .accept_policy(&bytes, &envelope, at(2 * DAY))
            .unwrap();
        assert_eq!(pending.policy_document(), Some(bytes.as_slice()));
        assert_eq!(pending.entry().effective_at(), at(2 * DAY));
        let EntryBody::AcceptPolicy(body) = pending.entry().body() else {
            panic!("expected accept_policy");
        };
        assert_eq!(body.policy_hash, Hash32(sha256(&bytes)));
        assert_eq!(body.signed_by, testkit::passkey_id());
        assert_eq!(body.proof, *envelope.proof());
        assert_eq!(
            pending
                .state_after()
                .in_force_policy(at(2 * DAY))
                .unwrap()
                .version(),
            1
        );
    }

    #[test]
    fn entry_sizes_follow_the_descriptor_overhead() {
        let empty = testkit::empty_state();
        let rp = testkit::rp();
        let invite = testkit::invite_descriptor();
        let bytes = invite.encode().unwrap();
        let pending = EntryBuilder::new(&empty)
            .link(&bytes, &invite.commitment().unwrap(), &rp, true, at(0))
            .unwrap();
        let party = testkit::agent_party().as_str().len();
        assert_eq!(pending.signable().bytes().len(), bytes.len() + 273 + party);
        let bound = testkit::link_descriptor(
            vec![testkit::ed25519_controller_from(0x81, CapabilitySet::ALL)],
            0x44,
        );
        let bytes = bound.encode().unwrap();
        let pending = EntryBuilder::new(&empty)
            .link(&bytes, &bound.commitment().unwrap(), &rp, false, at(0))
            .unwrap();
        assert_eq!(pending.signable().bytes().len(), bytes.len() + 274);
        let chain = Chain::linked();
        for (controller, lines) in [
            (testkit::ed25519_controller_from(0x81, read_only()), 5),
            (testkit::passkey_controller_from(0x51, read_only()), 11),
        ] {
            let add = testkit::add_descriptor(controller, 42);
            let bytes = add.encode().unwrap();
            let pending = chain
                .builder()
                .add_controller_from_code(&bytes, &add.commitment().unwrap(), &rp, at(DAY))
                .unwrap();
            assert_eq!(
                pending.signable().bytes().len(),
                bytes.len() + 241 - 3 * lines
            );
        }
    }

    #[test]
    fn entries_that_do_not_fit_are_refused() {
        let chain = Chain::linked();
        let bad = ControllerDescriptor::new(
            ControllerKey::Ed25519([0; 32]),
            Label::parse("Weak").unwrap(),
            read_only(),
        );
        assert!(bad.is_err());
        let refused = chain
            .builder()
            .add_ed25519([0; 32], Label::parse("Weak").unwrap(), read_only(), at(DAY))
            .unwrap_err();
        assert_eq!(
            refused,
            BuildError::Canon(CanonError::new(0, CanonErrorKind::WeakPublicKey))
        );
        let refused = chain
            .builder()
            .add_ed25519(
                testkit::ed_key(0x81).verifying_key().to_bytes(),
                Label::parse("Early").unwrap(),
                read_only(),
                at(-1),
            )
            .unwrap_err();
        assert_eq!(refused.code_name(), "time_regression");
    }
}

#[cfg(test)]
mod golden {
    use crate::canon::values::{Hash32, Network};
    use crate::policy::{DEFAULT_LINKED_RULES, PolicyDocument};
    use crate::testkit::{self, Chain, DAY, at};

    #[test]
    fn the_first_link_entry_and_the_default_policy_are_pinned() {
        let chain = Chain::linked();
        let entry = &chain.entries[0];
        let text = String::from_utf8(entry.bytes().to_vec()).unwrap();
        let head: Vec<&str> = text.lines().take(15).collect();
        assert_eq!(
            head,
            [
                "scheme=silvana.agent.control.v1",
                "network=devnet",
                "agent_party=agent-1::1220133628fcf469de1c1726313351f2729e6dd6f47b6c1989b76099d363754a39b6",
                "agent_key=79b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad049664",
                "seq=1",
                "prev=0000000000000000000000000000000000000000000000000000000000000000",
                "created_at=2026-11-22T12:00:00Z",
                "kind=link",
                "effective_at=2026-11-22T13:00:00Z",
                "owner_account=42",
                "invite=true",
                "nonce=5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
                "expires_at=2026-11-29T12:00:00Z",
                "commitment=fbc6bc1c6bdd1ec04b08d0fe4e163048",
                "controllers.count=2",
            ]
        );
        let descriptor = String::from_utf8(testkit::invite_descriptor().encode().unwrap()).unwrap();
        let blocks: Vec<&str> = descriptor.lines().skip(8).collect();
        let entry_blocks: Vec<&str> = text.lines().skip(15).collect();
        assert_eq!(entry_blocks, blocks);
        assert_eq!(
            entry.hash().to_string(),
            "796dd4e8a44a32f21b395c99212cc5f9a5d2e3d37c1c6ff04ce7403ffa3e25f4"
        );
        assert_eq!(
            hex::encode(entry.signature()),
            "1078467e4a17439715efe37979c072f602d67f4975651fbccea33dcabb86d06c\
             e3e9cfbde9e1e12c449c729eba8cd6f83692887fa4c71e8e00fad33dfeab8c07"
        );
        let doc = PolicyDocument {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            version: 1,
            prev_policy_hash: Hash32([0; 32]),
            issued_at: at(0),
            expires_at: at(DAY),
            not_before: None,
            rules: DEFAULT_LINKED_RULES,
        };
        assert_eq!(doc.encode().unwrap().len(), 1214);
        assert_eq!(
            doc.hash().unwrap().to_string(),
            "561997890deb4585de1fc2371aae015531d8c6c6f3e7898f65d0f80132cfa411"
        );
    }
}
