//! Replay of a verified log: owner, controllers with delayed changes, and the policy timeline.

use std::collections::BTreeMap;

use crate::canon::time::UtcTime;
use crate::canon::values::{Commitment, Hash32, Network, OwnerAccount, sha256};
use crate::controller::{
    Capability, CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey,
};
use crate::entry::{
    AcceptPolicyBody, AddControllerBody, EntryBody, EntryKind, LinkBody, LogEntry,
    RemoveControllerBody, SetCapabilitiesBody,
};
use crate::error::{CanonError, ReplayError, ReplayErrorKind};
use crate::link::{LinkDescriptor, LinkPurpose};
use crate::log::{HeadRef, VerifiedLog};
use crate::party::PartyId;
use crate::policy::{
    DEFAULT_LINKED_RULES, POLICY_CLOCK_SKEW_SECS, PolicyDocument, Rules, expected_effective_at,
};

/// Delay before a controller change takes effect, except a withdrawal.
pub const CONTROLLER_CHANGE_DELAY_SECS: u64 = 86_400;
/// Most controllers one agent may have, in effect or pending.
pub const MAX_AGENT_CONTROLLERS: usize = 32;

static DEFAULT_RULES: Rules = DEFAULT_LINKED_RULES;

pub(crate) fn add_secs(time: UtcTime, secs: u64) -> Option<UtcTime> {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| time.checked_add_secs(secs))
}

fn is_subset(a: CapabilitySet, b: CapabilitySet) -> bool {
    a.iter().all(|capability| b.contains(capability))
}

fn intersection(a: CapabilitySet, b: CapabilitySet) -> CapabilitySet {
    a.iter()
        .filter(|capability| b.contains(*capability))
        .fold(CapabilitySet::EMPTY, CapabilitySet::with)
}

/// Accepted policy documents, keyed by their SHA-256.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyDocuments {
    docs: BTreeMap<Hash32, Vec<u8>>,
}

impl PolicyDocuments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the bytes under their own hash, which it returns.
    pub fn insert(&mut self, bytes: Vec<u8>) -> Hash32 {
        let hash = Hash32(sha256(&bytes));
        self.docs.insert(hash, bytes);
        hash
    }

    /// Stores the bytes under a claimed hash; replay checks the claim.
    pub fn insert_keyed(&mut self, hash: Hash32, bytes: Vec<u8>) {
        self.docs.insert(hash, bytes);
    }

    pub fn get(&self, hash: &Hash32) -> Option<&[u8]> {
        self.docs.get(hash).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

/// What a pending change does when it takes effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PendingTarget {
    Capabilities(CapabilitySet),
    Removal,
}

/// A change that takes effect at `effective_at`; `seq` is the entry that scheduled it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PendingChange {
    pub seq: u64,
    pub effective_at: UtcTime,
    pub target: PendingTarget,
}

/// One controller as of the last entry: capabilities in effect and at most one pending change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerRecord {
    descriptor: ControllerDescriptor,
    added_seq: u64,
    effective: Option<CapabilitySet>,
    pending: Option<PendingChange>,
}

impl ControllerRecord {
    fn pending_add(descriptor: ControllerDescriptor, seq: u64, effective_at: UtcTime) -> Self {
        let target = PendingTarget::Capabilities(descriptor.capabilities());
        Self {
            descriptor,
            added_seq: seq,
            effective: None,
            pending: Some(PendingChange {
                seq,
                effective_at,
                target,
            }),
        }
    }

    /// The key and label; capabilities live in the record, not here.
    pub fn descriptor(&self) -> &ControllerDescriptor {
        &self.descriptor
    }

    pub fn id(&self) -> &ControllerId {
        self.descriptor.id()
    }

    pub fn added_seq(&self) -> u64 {
        self.added_seq
    }

    /// The pending change as of the last entry, due or not.
    pub fn pending(&self) -> Option<&PendingChange> {
        self.pending.as_ref()
    }

    fn due(&self, now: UtcTime) -> Option<PendingTarget> {
        self.pending
            .filter(|pending| pending.effective_at <= now)
            .map(|pending| pending.target)
    }

    /// Capabilities in effect at `now`; empty while the add is pending and once removed.
    pub fn capabilities_at(&self, now: UtcTime) -> CapabilitySet {
        match self.due(now) {
            Some(PendingTarget::Capabilities(capabilities)) => capabilities,
            Some(PendingTarget::Removal) => CapabilitySet::EMPTY,
            None => self.effective.unwrap_or(CapabilitySet::EMPTY),
        }
    }

    /// True once the add has taken effect and until a removal does.
    pub fn is_effective_at(&self, now: UtcTime) -> bool {
        !self.capabilities_at(now).is_empty()
    }

    /// The pending change not yet in effect at `now`.
    pub fn pending_at(&self, now: UtcTime) -> Option<&PendingChange> {
        self.pending
            .as_ref()
            .filter(|pending| pending.effective_at > now)
    }

    /// Capabilities once every pending change has applied; `None` when a removal is pending.
    pub fn final_capabilities(&self) -> Option<CapabilitySet> {
        match self.pending {
            Some(PendingChange {
                target: PendingTarget::Capabilities(capabilities),
                ..
            }) => Some(capabilities),
            Some(PendingChange {
                target: PendingTarget::Removal,
                ..
            }) => None,
            None => self.effective,
        }
    }
}

/// A policy document accepted into the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedPolicy {
    seq: u64,
    hash: Hash32,
    signed_by: ControllerId,
    accepted_at: UtcTime,
    effective_at: UtcTime,
    document: PolicyDocument,
}

impl AcceptedPolicy {
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn hash(&self) -> &Hash32 {
        &self.hash
    }

    pub fn signed_by(&self) -> &ControllerId {
        &self.signed_by
    }

    pub fn accepted_at(&self) -> UtcTime {
        self.accepted_at
    }

    pub fn effective_at(&self) -> UtcTime {
        self.effective_at
    }

    pub fn version(&self) -> u64 {
        self.document.version
    }

    pub fn document(&self) -> &PolicyDocument {
        &self.document
    }

    pub fn rules(&self) -> &Rules {
        &self.document.rules
    }
}

/// The rules in force: the defaults, or an accepted policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveRules<'a> {
    Default,
    Accepted(&'a AcceptedPolicy),
}

impl<'a> EffectiveRules<'a> {
    pub fn rules(&self) -> &'a Rules {
        match self {
            Self::Default => &DEFAULT_RULES,
            Self::Accepted(policy) => policy.rules(),
        }
    }

    pub fn policy(&self) -> Option<&'a AcceptedPolicy> {
        match self {
            Self::Default => None,
            Self::Accepted(policy) => Some(policy),
        }
    }
}

/// A controller change the minimum rule is asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControllerChange {
    Remove(ControllerId),
    SetCapabilities(ControllerId, CapabilitySet),
}

impl ControllerChange {
    pub fn controller_id(&self) -> &ControllerId {
        match self {
            Self::Remove(id) | Self::SetCapabilities(id, _) => id,
        }
    }
}

/// Capabilities left without a holder, and holder counts, once every pending change applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MinimumRuleOutcome {
    orphaned: CapabilitySet,
    remaining: [usize; 3],
}

impl MinimumRuleOutcome {
    pub fn orphaned(&self) -> CapabilitySet {
        self.orphaned
    }

    pub fn remaining(&self, capability: Capability) -> usize {
        Capability::ALL
            .into_iter()
            .zip(self.remaining)
            .find(|(c, _)| *c == capability)
            .map_or(0, |(_, n)| n)
    }

    /// True when the change needs `forced`.
    pub fn is_violation(&self) -> bool {
        !self.orphaned.is_empty()
    }
}

/// The state a log replays to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlState {
    network: Network,
    agent_party: PartyId,
    agent_key: [u8; 32],
    head: Option<HeadRef>,
    last_created_at: Option<UtcTime>,
    owner_account: Option<OwnerAccount>,
    controllers: BTreeMap<ControllerId, ControllerRecord>,
    in_force: Option<AcceptedPolicy>,
    scheduled: Option<AcceptedPolicy>,
}

impl ControlState {
    /// The state of an agent with no log.
    pub fn empty(network: Network, agent_party: PartyId, agent_key: [u8; 32]) -> Self {
        Self {
            network,
            agent_party,
            agent_key,
            head: None,
            last_created_at: None,
            owner_account: None,
            controllers: BTreeMap::new(),
            in_force: None,
            scheduled: None,
        }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn agent_party(&self) -> &PartyId {
        &self.agent_party
    }

    pub fn agent_key(&self) -> &[u8; 32] {
        &self.agent_key
    }

    pub fn head(&self) -> Option<HeadRef> {
        self.head
    }

    pub fn is_linked(&self) -> bool {
        self.head.is_some()
    }

    pub fn last_created_at(&self) -> Option<UtcTime> {
        self.last_created_at
    }

    /// Set by `link`, from its signature on.
    pub fn owner_account(&self) -> Option<OwnerAccount> {
        self.owner_account
    }

    pub fn controller(&self, id: &ControllerId) -> Option<&ControllerRecord> {
        self.controllers.get(id)
    }

    /// Records as of the last entry, ascending by id; ask each one about `now`.
    pub fn controllers(&self) -> impl Iterator<Item = &ControllerRecord> {
        self.controllers.values()
    }

    /// Controllers holding `capability` in effect at `now`.
    pub fn holders(&self, capability: Capability, now: UtcTime) -> Vec<&ControllerId> {
        self.controllers
            .values()
            .filter(|record| record.capabilities_at(now).contains(capability))
            .map(ControllerRecord::id)
            .collect()
    }

    /// Pending controller changes not yet in effect at `now`.
    pub fn pending_changes(&self, now: UtcTime) -> Vec<(&ControllerId, &PendingChange)> {
        self.controllers
            .values()
            .filter_map(|record| record.pending_at(now).map(|p| (record.id(), p)))
            .collect()
    }

    /// The accepted policy in force at `now`, if any.
    pub fn in_force_policy(&self, now: UtcTime) -> Option<&AcceptedPolicy> {
        match &self.scheduled {
            Some(scheduled) if scheduled.effective_at <= now => Some(scheduled),
            _ => self.in_force.as_ref(),
        }
    }

    /// The accepted widening not yet in force at `now`.
    pub fn scheduled_policy(&self, now: UtcTime) -> Option<&AcceptedPolicy> {
        self.scheduled
            .as_ref()
            .filter(|scheduled| scheduled.effective_at > now)
    }

    /// The most recently accepted policy, in force or scheduled.
    pub fn latest_policy(&self) -> Option<&AcceptedPolicy> {
        self.scheduled.as_ref().or(self.in_force.as_ref())
    }

    pub fn rules_at(&self, now: UtcTime) -> EffectiveRules<'_> {
        match self.in_force_policy(now) {
            Some(policy) => EffectiveRules::Accepted(policy),
            None => EffectiveRules::Default,
        }
    }

    /// Applies the entries of `log` after this state's head.
    pub fn extend(&self, log: &VerifiedLog, docs: &PolicyDocuments) -> Result<Self, ReplayError> {
        use ReplayErrorKind as K;
        if log.network() != self.network {
            return Err(ReplayError::new(0, K::WrongNetwork));
        }
        if *log.agent_party() != self.agent_party {
            return Err(ReplayError::new(0, K::WrongParty));
        }
        if *log.agent_key() != self.agent_key {
            return Err(ReplayError::new(0, K::WrongAgentKey));
        }
        let start = match self.head {
            None => 0,
            Some(head) => match log.entry(head.seq) {
                Some(entry) if *entry.hash() == head.hash => usize::try_from(head.seq)
                    .map_err(|_| ReplayError::new(head.seq, K::UnexpectedSeq))?,
                _ => return Err(ReplayError::new(head.seq, K::PrevMismatch)),
            },
        };
        let mut next = self.clone();
        for signed in log.entries().iter().skip(start) {
            apply_entry(&mut next, signed.entry(), signed.hash(), docs)
                .map_err(|kind| ReplayError::new(signed.seq(), kind))?;
        }
        Ok(next)
    }

    pub(crate) fn next_seq(&self) -> Option<u64> {
        self.head.map_or(Some(1), |head| head.seq.checked_add(1))
    }

    /// Applies every pending change and scheduled policy due at `at`.
    pub(crate) fn advance_to(&mut self, at: UtcTime) {
        self.controllers
            .retain(|_, record| record.due(at) != Some(PendingTarget::Removal));
        for record in self.controllers.values_mut() {
            if let Some(PendingTarget::Capabilities(capabilities)) = record.due(at) {
                record.effective = Some(capabilities);
                record.pending = None;
            }
        }
        if self
            .scheduled
            .as_ref()
            .is_some_and(|scheduled| scheduled.effective_at <= at)
        {
            self.in_force = self.scheduled.take();
        }
    }
}

/// Replays a verified log; every referenced policy document must be in `docs`.
pub fn replay(log: &VerifiedLog, docs: &PolicyDocuments) -> Result<ControlState, ReplayError> {
    ControlState::empty(log.network(), log.agent_party().clone(), *log.agent_key())
        .extend(log, docs)
}

/// True when the change only withdraws grants that have not taken effect yet.
pub(crate) fn is_withdrawal(record: &ControllerRecord, change: &ControllerChange) -> bool {
    match change {
        ControllerChange::Remove(_) => record.effective.is_none(),
        ControllerChange::SetCapabilities(_, target) => match record.pending {
            Some(PendingChange {
                target: PendingTarget::Capabilities(pending),
                ..
            }) => {
                let effective = record.effective.unwrap_or(CapabilitySet::EMPTY);
                *target != pending
                    && is_subset(*target, pending)
                    && is_subset(intersection(pending, effective), *target)
            }
            _ => false,
        },
    }
}

/// When a change made at `at` takes effect: at once for a withdrawal, else after the delay.
pub(crate) fn change_effective_at(
    record: &ControllerRecord,
    change: &ControllerChange,
    at: UtcTime,
) -> Option<UtcTime> {
    if is_withdrawal(record, change) {
        Some(at)
    } else {
        add_secs(at, CONTROLLER_CHANGE_DELAY_SECS)
    }
}

fn tally(counts: &mut [usize; 3], capabilities: CapabilitySet) {
    for (slot, capability) in counts.iter_mut().zip(Capability::ALL) {
        if capabilities.contains(capability) {
            *slot = slot.saturating_add(1);
        }
    }
}

fn minimum_rule_outcome(state: &ControlState, change: &ControllerChange) -> MinimumRuleOutcome {
    let mut before = [0usize; 3];
    let mut after = [0usize; 3];
    for (id, record) in &state.controllers {
        let current = record.final_capabilities().unwrap_or(CapabilitySet::EMPTY);
        let next = if id == change.controller_id() {
            match change {
                ControllerChange::Remove(_) => CapabilitySet::EMPTY,
                ControllerChange::SetCapabilities(_, capabilities) => *capabilities,
            }
        } else {
            current
        };
        tally(&mut before, current);
        tally(&mut after, next);
    }
    let orphaned = Capability::ALL
        .into_iter()
        .zip(before)
        .zip(after)
        .filter(|((_, b), a)| *b > 0 && *a == 0)
        .fold(CapabilitySet::EMPTY, |set, ((capability, _), _)| {
            set.with(capability)
        });
    MinimumRuleOutcome {
        orphaned,
        remaining: after,
    }
}

/// Which held capabilities `change` leaves with no controller, after every pending change.
pub fn minimum_rule(
    state: &ControlState,
    change: &ControllerChange,
) -> Result<MinimumRuleOutcome, ReplayError> {
    if !state.controllers.contains_key(change.controller_id()) {
        let seq = state.next_seq().unwrap_or(u64::MAX);
        return Err(ReplayError::new(seq, ReplayErrorKind::UnknownController));
    }
    Ok(minimum_rule_outcome(state, change))
}

/// The phrase typed to force a change; it names the policy that stays frozen.
pub fn force_confirmation_phrase(state: &ControlState) -> String {
    let hint = state.agent_party.hint();
    match state.latest_policy() {
        Some(policy) => format!("freeze {hint} policy v{}", policy.version()),
        None => format!("freeze {hint} default policy"),
    }
}

/// Applies one entry whose header and position the caller has not yet checked.
pub(crate) fn apply_entry(
    state: &mut ControlState,
    entry: &LogEntry,
    hash: &Hash32,
    docs: &PolicyDocuments,
) -> Result<(), ReplayErrorKind> {
    use ReplayErrorKind as K;
    if entry.network() != state.network {
        return Err(K::WrongNetwork);
    }
    if *entry.agent_party() != state.agent_party {
        return Err(K::WrongParty);
    }
    if *entry.agent_key() != state.agent_key {
        return Err(K::WrongAgentKey);
    }
    let seq = state.next_seq().ok_or(K::UnexpectedSeq)?;
    if entry.seq() != seq {
        return Err(K::UnexpectedSeq);
    }
    if *entry.prev() != state.head.map_or(Hash32([0; 32]), |head| head.hash) {
        return Err(K::PrevMismatch);
    }
    let at = entry.created_at();
    if state.last_created_at.is_some_and(|last| at < last) {
        return Err(K::TimeRegression);
    }
    if seq == 1 && entry.kind() != EntryKind::Link {
        return Err(K::FirstNotLink);
    }
    state.advance_to(at);
    match entry.body() {
        EntryBody::Link(body) => apply_link(state, entry, body)?,
        EntryBody::AddController(body) => apply_add(state, entry, body)?,
        EntryBody::RemoveController(body) => apply_remove(state, entry, body)?,
        EntryBody::SetCapabilities(body) => apply_set_capabilities(state, entry, body)?,
        EntryBody::AcceptPolicy(body) => apply_accept(state, entry, body, docs)?,
    }
    state.head = Some(HeadRef { seq, hash: *hash });
    state.last_created_at = Some(at);
    Ok(())
}

fn refuse_agent_key(
    state: &ControlState,
    controller: &ControllerDescriptor,
) -> Result<(), ReplayErrorKind> {
    match controller.key() {
        ControllerKey::Ed25519(key) if *key == state.agent_key => {
            Err(ReplayErrorKind::AgentKeyAsController)
        }
        _ => Ok(()),
    }
}

fn delayed_effective_at(entry: &LogEntry) -> Result<UtcTime, ReplayErrorKind> {
    let expected = add_secs(entry.created_at(), CONTROLLER_CHANGE_DELAY_SECS)
        .ok_or(ReplayErrorKind::TimeOverflow)?;
    if entry.effective_at() != expected {
        return Err(ReplayErrorKind::EffectiveAtMismatch);
    }
    Ok(expected)
}

fn commitment_matches(
    descriptor: Result<LinkDescriptor, CanonError>,
    expected: &Commitment,
) -> bool {
    descriptor
        .and_then(|d| d.commitment())
        .is_ok_and(|commitment| commitment == *expected)
}

fn apply_link(
    state: &mut ControlState,
    entry: &LogEntry,
    body: &LinkBody,
) -> Result<(), ReplayErrorKind> {
    use ReplayErrorKind as K;
    if !state.controllers.is_empty() {
        return Err(K::LinkWhileControlled);
    }
    if body.invite && entry.seq() != 1 {
        return Err(K::InviteNotFirst);
    }
    if entry.created_at() > body.expires_at {
        return Err(K::DescriptorExpired);
    }
    let party = (!body.invite).then(|| state.agent_party.clone());
    let rebuilt = LinkDescriptor::new(
        state.network,
        LinkPurpose::Link,
        body.owner_account,
        party,
        body.nonce,
        body.expires_at,
        body.controllers.clone(),
    );
    if !commitment_matches(rebuilt, &body.commitment) {
        return Err(K::CommitmentMismatch);
    }
    for controller in &body.controllers {
        refuse_agent_key(state, controller)?;
    }
    let effective_at = delayed_effective_at(entry)?;
    state.owner_account = Some(body.owner_account);
    for controller in &body.controllers {
        state.controllers.insert(
            *controller.id(),
            ControllerRecord::pending_add(controller.clone(), entry.seq(), effective_at),
        );
    }
    Ok(())
}

fn apply_add(
    state: &mut ControlState,
    entry: &LogEntry,
    body: &AddControllerBody,
) -> Result<(), ReplayErrorKind> {
    use ReplayErrorKind as K;
    if state.owner_account != Some(body.owner_account) {
        return Err(K::OwnerMismatch);
    }
    let id = *body.controller.id();
    if state.controllers.contains_key(&id) {
        return Err(K::DuplicateController);
    }
    if state.controllers.len() >= MAX_AGENT_CONTROLLERS {
        return Err(K::TooManyControllers);
    }
    refuse_agent_key(state, &body.controller)?;
    match &body.descriptor {
        Some(descriptor) => {
            if entry.created_at() > descriptor.expires_at {
                return Err(K::DescriptorExpired);
            }
            let rebuilt = LinkDescriptor::new(
                state.network,
                LinkPurpose::AddController,
                body.owner_account,
                Some(state.agent_party.clone()),
                descriptor.nonce,
                descriptor.expires_at,
                vec![body.controller.clone()],
            );
            if !commitment_matches(rebuilt, &descriptor.commitment) {
                return Err(K::CommitmentMismatch);
            }
        }
        None if !matches!(body.controller.key(), ControllerKey::Ed25519(_)) => {
            return Err(K::CommitmentRequired);
        }
        None => {}
    }
    let effective_at = delayed_effective_at(entry)?;
    state.controllers.insert(
        id,
        ControllerRecord::pending_add(body.controller.clone(), entry.seq(), effective_at),
    );
    Ok(())
}

/// The checks shared by remove and set_capabilities; returns (withdrawal, effective_at).
fn check_change(
    state: &ControlState,
    entry: &LogEntry,
    change: &ControllerChange,
    forced: bool,
) -> Result<(bool, UtcTime), ReplayErrorKind> {
    use ReplayErrorKind as K;
    let record = state
        .controllers
        .get(change.controller_id())
        .ok_or(K::UnknownController)?;
    let unchanged = match change {
        ControllerChange::Remove(_) => record.final_capabilities().is_none(),
        ControllerChange::SetCapabilities(_, target) => {
            record.final_capabilities() == Some(*target)
        }
    };
    if unchanged {
        return Err(K::NoChange);
    }
    if forced != minimum_rule_outcome(state, change).is_violation() {
        return Err(K::ForcedMismatch);
    }
    let withdrawal = is_withdrawal(record, change);
    let expected =
        change_effective_at(record, change, entry.created_at()).ok_or(K::TimeOverflow)?;
    if entry.effective_at() != expected {
        return Err(K::EffectiveAtMismatch);
    }
    Ok((withdrawal, expected))
}

fn apply_remove(
    state: &mut ControlState,
    entry: &LogEntry,
    body: &RemoveControllerBody,
) -> Result<(), ReplayErrorKind> {
    let change = ControllerChange::Remove(body.controller_id);
    let (withdrawal, effective_at) = check_change(state, entry, &change, body.forced)?;
    if withdrawal {
        state.controllers.remove(&body.controller_id);
    } else if let Some(record) = state.controllers.get_mut(&body.controller_id) {
        record.pending = Some(PendingChange {
            seq: entry.seq(),
            effective_at,
            target: PendingTarget::Removal,
        });
    }
    Ok(())
}

fn apply_set_capabilities(
    state: &mut ControlState,
    entry: &LogEntry,
    body: &SetCapabilitiesBody,
) -> Result<(), ReplayErrorKind> {
    let target = body.capabilities;
    let change = ControllerChange::SetCapabilities(body.controller_id, target);
    let (withdrawal, effective_at) = check_change(state, entry, &change, body.forced)?;
    if let Some(record) = state.controllers.get_mut(&body.controller_id) {
        if !withdrawal {
            record.pending = Some(PendingChange {
                seq: entry.seq(),
                effective_at,
                target: PendingTarget::Capabilities(target),
            });
        } else if record.effective == Some(target) {
            record.pending = None;
        } else if let Some(pending) = record.pending.as_mut() {
            pending.target = PendingTarget::Capabilities(target);
        }
    }
    Ok(())
}

fn apply_accept(
    state: &mut ControlState,
    entry: &LogEntry,
    body: &AcceptPolicyBody,
    docs: &PolicyDocuments,
) -> Result<(), ReplayErrorKind> {
    use ReplayErrorKind as K;
    let at = entry.created_at();
    let signer = state
        .controllers
        .get(&body.signed_by)
        .ok_or(K::UnknownController)?;
    if !signer.capabilities_at(at).contains(Capability::Policy) {
        return Err(K::SignerLacksPolicy);
    }
    let bytes = docs
        .get(&body.policy_hash)
        .ok_or(K::MissingPolicyDocument)?;
    if sha256(bytes) != body.policy_hash.0 {
        return Err(K::PolicyDocumentMismatch);
    }
    let document = PolicyDocument::decode(bytes).map_err(K::PolicyDecode)?;
    if document.network != state.network {
        return Err(K::PolicyWrongNetwork);
    }
    if document.agent_party != state.agent_party {
        return Err(K::PolicyWrongParty);
    }
    if document.version != body.policy_version {
        return Err(K::PolicyVersionMismatch);
    }
    let latest = state.latest_policy();
    if document.version <= latest.map_or(0, AcceptedPolicy::version) {
        return Err(K::PolicyVersionNotIncreasing);
    }
    if document.prev_policy_hash != latest.map_or(Hash32([0; 32]), |p| p.hash) {
        return Err(K::PolicyPrevMismatch);
    }
    let latest_issue = add_secs(at, POLICY_CLOCK_SKEW_SECS).ok_or(K::TimeOverflow)?;
    if document.issued_at > latest_issue {
        return Err(K::PolicyIssuedInFuture);
    }
    if at > document.expires_at {
        return Err(K::PolicyExpired);
    }
    body.proof
        .verify(signer.descriptor(), bytes)
        .map_err(K::Proof)?;
    let baseline = state.rules_at(at).rules();
    let expected = expected_effective_at(at, baseline, &document).ok_or(K::TimeOverflow)?;
    if entry.effective_at() != expected {
        return Err(K::EffectiveAtMismatch);
    }
    let accepted = AcceptedPolicy {
        seq: entry.seq(),
        hash: body.policy_hash,
        signed_by: body.signed_by,
        accepted_at: at,
        effective_at: expected,
        document,
    };
    if expected == at {
        state.in_force = Some(accepted);
        state.scheduled = None;
    } else {
        state.scheduled = Some(accepted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::BuildError;
    use crate::canon::values::Label;
    use crate::controller::Capability::{Direct, Policy, Read};
    use crate::entry::{DescriptorRef, SignedEntry};
    use crate::error::{PolicyError, ProofError, SignatureError};
    use crate::log::verify_log;
    use crate::proof::ControllerProof;
    use crate::testkit::{self, Chain, DAY, at};
    use ed25519_dalek::Signer;

    fn caps(list: &[Capability]) -> CapabilitySet {
        CapabilitySet::of(list)
    }

    fn label(s: &str) -> Label {
        Label::parse(s).unwrap()
    }

    fn e81() -> ControllerId {
        testkit::ed25519_id(0x81)
    }

    fn e21() -> ControllerId {
        testkit::ed25519_id(0x21)
    }

    fn passkey() -> ControllerId {
        testkit::passkey_id()
    }

    fn add_e81(chain: &mut Chain, capabilities: CapabilitySet, when: UtcTime) -> SignedEntry {
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let pending = chain
            .builder()
            .add_ed25519(key, label("Backup key"), capabilities, when)
            .unwrap();
        chain.push(&pending)
    }

    /// Linked at T0, plus E81 holding policy from 2 days after T0.
    fn with_ed25519_policy() -> Chain {
        let mut chain = Chain::linked();
        add_e81(&mut chain, caps(&[Policy]), at(DAY));
        chain
    }

    /// An entry built and signed by hand, bypassing the builder's checks.
    fn forge(
        chain: &Chain,
        created_at: UtcTime,
        effective_at: UtcTime,
        body: EntryBody,
    ) -> SignedEntry {
        let state = &chain.state;
        let entry = LogEntry {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            agent_key: *state.agent_key(),
            seq: state.next_seq().unwrap(),
            prev: state.head().map_or(Hash32([0; 32]), |head| head.hash),
            created_at,
            effective_at,
            body,
        };
        let bytes = entry.encode().unwrap();
        let signature = testkit::agent_key().sign(&bytes).to_bytes();
        SignedEntry::from_parts(entry, bytes, signature)
    }

    fn replay_with(
        chain: &Chain,
        extra: SignedEntry,
        docs: &PolicyDocuments,
    ) -> Result<ControlState, ReplayError> {
        let mut entries = chain.entries.clone();
        entries.push(extra);
        let log = verify_log(Network::Devnet, &testkit::agent_party(), &entries).unwrap();
        replay(&log, docs)
    }

    fn refusal(chain: &Chain, extra: SignedEntry) -> &'static str {
        replay_with(chain, extra, &chain.docs)
            .unwrap_err()
            .code_name()
    }

    fn replay_error(error: BuildError) -> ReplayErrorKind {
        match error {
            BuildError::Replay(e) => e.kind,
            other => panic!("not a replay error: {other:?}"),
        }
    }

    #[test]
    fn link_sets_the_owner_at_once_and_the_controllers_after_the_delay() {
        let chain = Chain::linked();
        let state = &chain.state;
        assert_eq!(state.owner_account(), Some(OwnerAccount::new(42).unwrap()));
        assert_eq!(chain.entries[0].effective_at(), at(DAY));
        for capability in Capability::ALL {
            assert!(state.holders(capability, at(DAY - 1)).is_empty());
        }
        assert_eq!(state.holders(Policy, at(DAY)), vec![&passkey()]);
        assert_eq!(state.holders(Direct, at(DAY)), vec![&passkey()]);
        assert_eq!(state.holders(Read, at(DAY)), vec![&e21()]);
        assert_eq!(state.pending_changes(at(DAY - 1)).len(), 2);
        assert!(state.pending_changes(at(DAY)).is_empty());
        assert!(
            !state
                .controller(&passkey())
                .unwrap()
                .is_effective_at(at(DAY - 1))
        );
        assert_eq!(chain.replayed(), chain.state);
        assert_eq!(state.rules_at(at(0)), EffectiveRules::Default);
    }

    #[test]
    fn an_add_takes_effect_exactly_one_day_after_its_signature() {
        let mut chain = Chain::linked();
        let added = add_e81(&mut chain, caps(&[Policy]), at(2 * DAY));
        assert_eq!(added.effective_at(), at(3 * DAY));
        let state = &chain.state;
        assert_eq!(state.holders(Policy, at(3 * DAY - 1)), vec![&passkey()]);
        assert_eq!(state.holders(Policy, at(3 * DAY)).len(), 2);
        let pending = state.pending_changes(at(2 * DAY));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, &e81());
        assert_eq!(pending[0].1.effective_at, at(3 * DAY));
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_removal_keeps_the_controller_effective_until_it_takes_effect() {
        let mut chain = Chain::linked();
        add_e81(&mut chain, caps(&[Policy, Direct]), at(DAY));
        let pending = chain
            .builder()
            .remove_controller(&passkey(), false, at(4 * DAY))
            .unwrap();
        assert_eq!(pending.entry().effective_at(), at(5 * DAY));
        chain.push(&pending);
        let state = &chain.state;
        let record = state.controller(&passkey()).unwrap();
        assert_eq!(record.final_capabilities(), None);
        assert_eq!(
            record.capabilities_at(at(5 * DAY - 1)),
            caps(&[Policy, Direct])
        );
        assert!(record.capabilities_at(at(5 * DAY)).is_empty());
        assert!(state.holders(Policy, at(5 * DAY - 1)).contains(&&passkey()));
        assert!(!state.holders(Policy, at(5 * DAY)).contains(&&passkey()));
        // Until then the passkey still signs policies.
        let doc = testkit::policy_doc(state, 1, at(5 * DAY - 60), DEFAULT_LINKED_RULES);
        chain.accept_passkey(&doc, at(5 * DAY - 30));
        let late = testkit::policy_doc(&chain.state, 2, at(5 * DAY), DEFAULT_LINKED_RULES);
        let bytes = late.encode().unwrap();
        let envelope = testkit::passkey_envelope(&bytes);
        let refused = chain
            .builder()
            .accept_policy(&bytes, &envelope, at(5 * DAY))
            .unwrap_err();
        assert_eq!(refused, BuildError::Policy(PolicyError::UnknownController));
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn withdrawing_a_pending_add_applies_at_once() {
        let mut chain = Chain::linked();
        add_e81(&mut chain, caps(&[Read]), at(2 * DAY));
        let pending = chain
            .builder()
            .remove_controller(&e81(), false, at(2 * DAY + 100))
            .unwrap();
        assert_eq!(pending.entry().effective_at(), at(2 * DAY + 100));
        chain.push(&pending);
        assert!(chain.state.controller(&e81()).is_none());
        assert_eq!(chain.replayed(), chain.state);
        // The same id can be added again; the delay starts over.
        let again = add_e81(&mut chain, caps(&[Read]), at(2 * DAY + 200));
        assert_eq!(again.effective_at(), at(3 * DAY + 200));
    }

    #[test]
    fn dropping_a_pending_capability_applies_at_once() {
        let mut chain = Chain::linked();
        let grant = chain
            .builder()
            .set_capabilities(&e21(), caps(&[Policy, Read]), false, at(2 * DAY))
            .unwrap();
        assert_eq!(grant.entry().effective_at(), at(3 * DAY));
        chain.push(&grant);
        let record = chain.state.controller(&e21()).unwrap();
        assert_eq!(record.capabilities_at(at(3 * DAY - 1)), caps(&[Read]));
        assert_eq!(record.capabilities_at(at(3 * DAY)), caps(&[Policy, Read]));
        let drop = chain
            .builder()
            .set_capabilities(&e21(), caps(&[Read]), false, at(2 * DAY + 10))
            .unwrap();
        assert_eq!(drop.entry().effective_at(), at(2 * DAY + 10));
        chain.push(&drop);
        let record = chain.state.controller(&e21()).unwrap();
        assert_eq!(record.pending(), None);
        assert_eq!(record.capabilities_at(at(4 * DAY)), caps(&[Read]));

        // On a pending add, dropping a capability keeps the add's own time.
        add_e81(&mut chain, caps(&[Policy, Read]), at(2 * DAY + 20));
        let narrowed = chain
            .builder()
            .set_capabilities(&e81(), caps(&[Read]), false, at(2 * DAY + 30))
            .unwrap();
        assert_eq!(narrowed.entry().effective_at(), at(2 * DAY + 30));
        chain.push(&narrowed);
        let record = chain.state.controller(&e81()).unwrap();
        assert_eq!(record.pending().unwrap().effective_at, at(3 * DAY + 20));
        assert_eq!(record.capabilities_at(at(3 * DAY + 20)), caps(&[Read]));
        // Granting more is delayed again, and pushes the add back.
        let widened = chain
            .builder()
            .set_capabilities(&e81(), caps(&[Direct, Read]), false, at(2 * DAY + 40))
            .unwrap();
        assert_eq!(widened.entry().effective_at(), at(3 * DAY + 40));
        chain.push(&widened);
        let record = chain.state.controller(&e81()).unwrap();
        assert!(record.capabilities_at(at(3 * DAY + 39)).is_empty());
        assert_eq!(
            record.capabilities_at(at(3 * DAY + 40)),
            caps(&[Direct, Read])
        );
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_capability_removal_is_delayed() {
        let mut chain = Chain::linked();
        let pending = chain
            .builder()
            .set_capabilities(&passkey(), caps(&[Policy]), true, at(2 * DAY))
            .unwrap();
        assert_eq!(pending.entry().effective_at(), at(3 * DAY));
        chain.push(&pending);
        let record = chain.state.controller(&passkey()).unwrap();
        assert_eq!(
            record.capabilities_at(at(3 * DAY - 1)),
            caps(&[Policy, Direct])
        );
        assert_eq!(record.capabilities_at(at(3 * DAY)), caps(&[Policy]));
        // Restoring what is still in effect is not a withdrawal of a pending grant.
        let restore = chain
            .builder()
            .set_capabilities(&passkey(), caps(&[Policy, Direct]), false, at(2 * DAY + 5))
            .unwrap();
        assert_eq!(restore.entry().effective_at(), at(3 * DAY + 5));
    }

    #[test]
    fn link_is_refused_while_any_controller_is_in_effect_or_pending() {
        let mut chain = Chain::linked();
        let fresh = testkit::link_descriptor(
            vec![testkit::ed25519_controller_from(0x81, CapabilitySet::ALL)],
            0x44,
        );
        let bytes = fresh.encode().unwrap();
        let commitment = fresh.commitment().unwrap();
        for when in [at(DAY / 2), at(2 * DAY)] {
            let refused = chain
                .builder()
                .link(&bytes, &commitment, &testkit::rp(), false, when);
            assert_eq!(
                replay_error(refused.unwrap_err()),
                ReplayErrorKind::LinkWhileControlled
            );
        }
        // A forced full removal, then a re-link once it has taken effect: 48 h in all.
        let first = chain
            .builder()
            .remove_controller(&passkey(), true, at(2 * DAY))
            .unwrap();
        assert!(matches!(
            first.entry().body(),
            EntryBody::RemoveController(RemoveControllerBody { forced: true, .. })
        ));
        chain.push(&first);
        let second = chain
            .builder()
            .remove_controller(&e21(), true, at(2 * DAY))
            .unwrap();
        chain.push(&second);
        let early =
            chain
                .builder()
                .link(&bytes, &commitment, &testkit::rp(), false, at(3 * DAY - 1));
        assert_eq!(
            replay_error(early.unwrap_err()),
            ReplayErrorKind::LinkWhileControlled
        );
        let relink = chain
            .builder()
            .link(&bytes, &commitment, &testkit::rp(), false, at(3 * DAY))
            .unwrap();
        assert_eq!(relink.entry().effective_at(), at(4 * DAY));
        chain.push(&relink);
        assert_eq!(chain.state.holders(Policy, at(4 * DAY)), vec![&e81()]);
        assert_eq!(chain.replayed(), chain.state);
        // An invite descriptor is accepted only into an empty log.
        let invite = testkit::invite_descriptor();
        let refused = chain.builder().link(
            &invite.encode().unwrap(),
            &invite.commitment().unwrap(),
            &testkit::rp(),
            true,
            at(6 * DAY),
        );
        assert_eq!(refused.unwrap_err().code_name(), "invite_not_allowed");
    }

    #[test]
    fn the_minimum_rule_counts_the_state_after_pending_changes() {
        let mut chain = Chain::linked();
        add_e81(&mut chain, CapabilitySet::ALL, at(2 * DAY));
        // The pending add will hold policy and direct, so removing the passkey orphans nothing.
        let outcome = minimum_rule(&chain.state, &ControllerChange::Remove(passkey())).unwrap();
        assert!(!outcome.is_violation());
        assert_eq!(outcome.remaining(Policy), 1);
        assert_eq!(outcome.remaining(Read), 2);
        let remove = chain
            .builder()
            .remove_controller(&passkey(), false, at(2 * DAY + 1))
            .unwrap();
        chain.push(&remove);
        // With the passkey's removal pending, withdrawing E81 would orphan policy and direct.
        let refused = chain
            .builder()
            .remove_controller(&e81(), false, at(2 * DAY + 2))
            .unwrap_err();
        let BuildError::MinimumRule(outcome) = refused else {
            panic!("expected a minimum-rule refusal, got {refused:?}");
        };
        assert_eq!(outcome.orphaned(), caps(&[Policy, Direct]));
        assert_eq!(outcome.remaining(Read), 1);
        let forced = chain
            .builder()
            .remove_controller(&e81(), true, at(2 * DAY + 2))
            .unwrap();
        assert_eq!(forced.entry().effective_at(), at(2 * DAY + 2));
        chain.push(&forced);
        // Adding never trips the rule; removing the last reader does.
        let outcome = minimum_rule(&chain.state, &ControllerChange::Remove(e21())).unwrap();
        assert_eq!(outcome.orphaned(), caps(&[Read]));
        let outcome = minimum_rule(
            &chain.state,
            &ControllerChange::SetCapabilities(e21(), caps(&[Policy])),
        )
        .unwrap();
        assert_eq!(outcome.orphaned(), caps(&[Read]));
        assert_eq!(
            minimum_rule(&chain.state, &ControllerChange::Remove(e81()))
                .unwrap_err()
                .kind,
            ReplayErrorKind::UnknownController
        );
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_pending_controller_cannot_sign_a_policy() {
        let chain = Chain::linked();
        let doc = testkit::policy_doc(&chain.state, 1, at(DAY / 2), DEFAULT_LINKED_RULES);
        let bytes = doc.encode().unwrap();
        let envelope = testkit::passkey_envelope(&bytes);
        let refused = chain
            .builder()
            .accept_policy(&bytes, &envelope, at(DAY / 2))
            .unwrap_err();
        assert_eq!(refused, BuildError::Policy(PolicyError::SignerLacksPolicy));
        // The same entry signed by hand is refused by replay.
        let mut docs = chain.docs.clone();
        let hash = docs.insert(bytes);
        let body = EntryBody::AcceptPolicy(AcceptPolicyBody {
            policy_hash: hash,
            policy_version: 1,
            signed_by: passkey(),
            proof: envelope.proof().clone(),
        });
        let forged = forge(&chain, at(DAY / 2), at(DAY / 2), body);
        let error = replay_with(&chain, forged, &docs).unwrap_err();
        assert_eq!(
            (error.seq, error.kind),
            (2, ReplayErrorKind::SignerLacksPolicy)
        );
    }

    #[test]
    fn accept_policy_takes_ed25519_and_webauthn_proofs() {
        let mut chain = with_ed25519_policy();
        let v1 = testkit::policy_doc(&chain.state, 1, at(2 * DAY), DEFAULT_LINKED_RULES);
        chain.accept_ed25519(0x81, &v1, at(2 * DAY));
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.enabled = false;
        let v2 = testkit::policy_doc(&chain.state, 2, at(2 * DAY + 60), rules);
        let entry = chain.accept_passkey(&v2, at(2 * DAY + 60));
        assert_eq!(entry.effective_at(), at(2 * DAY + 60));
        let state = &chain.state;
        assert_eq!(state.latest_policy().unwrap().version(), 2);
        assert_eq!(
            state.in_force_policy(at(2 * DAY + 60)).unwrap().signed_by(),
            &passkey()
        );
        assert!(!state.rules_at(at(2 * DAY + 60)).rules().transfers.enabled);
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_removed_signers_policy_stays_in_force() {
        let mut chain = with_ed25519_policy();
        let v1 = testkit::policy_doc(&chain.state, 1, at(2 * DAY), DEFAULT_LINKED_RULES);
        chain.accept_ed25519(0x81, &v1, at(2 * DAY));
        let remove = chain
            .builder()
            .remove_controller(&e81(), false, at(2 * DAY + 1))
            .unwrap();
        chain.push(&remove);
        let later = at(10 * DAY);
        assert!(
            chain
                .state
                .holders(Policy, later)
                .iter()
                .all(|id| **id != e81())
        );
        assert_eq!(
            chain.state.in_force_policy(later).unwrap().signed_by(),
            &e81()
        );
        assert_eq!(
            force_confirmation_phrase(&chain.state),
            "freeze agent-1 policy v1"
        );
        assert_eq!(
            force_confirmation_phrase(&Chain::linked().state),
            "freeze agent-1 default policy"
        );
    }

    fn accept_body(
        hash: Hash32,
        version: u64,
        signer: ControllerId,
        proof: ControllerProof,
    ) -> EntryBody {
        EntryBody::AcceptPolicy(AcceptPolicyBody {
            policy_hash: hash,
            policy_version: version,
            signed_by: signer,
            proof,
        })
    }

    fn ed25519_proof(start: u8, payload: &[u8]) -> ControllerProof {
        testkit::ed25519_envelope(start, payload).proof().clone()
    }

    #[test]
    fn accept_policy_refusals() {
        let mut chain = with_ed25519_policy();
        let now = at(2 * DAY);
        let v1 = testkit::policy_doc(&chain.state, 1, now, DEFAULT_LINKED_RULES);
        let v1_bytes = v1.encode().unwrap();
        let v1_hash = Hash32(sha256(&v1_bytes));
        let mut docs = chain.docs.clone();
        docs.insert(v1_bytes.clone());
        let good = |chain: &Chain, effective_at: UtcTime| {
            forge(
                chain,
                now,
                effective_at,
                accept_body(v1_hash, 1, e81(), ed25519_proof(0x81, &v1_bytes)),
            )
        };
        assert!(replay_with(&chain, good(&chain, now), &docs).is_ok());
        let off_by_one = good(&chain, at(2 * DAY + 1));
        assert_eq!(
            replay_with(&chain, off_by_one, &docs)
                .unwrap_err()
                .code_name(),
            "effective_at_mismatch"
        );
        // Every referenced document is required, and must hash to its id.
        assert_eq!(
            refusal(&chain, good(&chain, now)),
            "missing_policy_document"
        );
        let mut swapped = chain.docs.clone();
        swapped.insert_keyed(v1_hash, DEFAULT_LINKED_RULES_TEXT.as_bytes().to_vec());
        assert_eq!(
            replay_with(&chain, good(&chain, now), &swapped)
                .unwrap_err()
                .code_name(),
            "policy_document_mismatch"
        );
        let cases: Vec<(&str, EntryBody)> = vec![
            (
                "signer_lacks_policy",
                accept_body(v1_hash, 1, e21(), ed25519_proof(0x21, &v1_bytes)),
            ),
            (
                "unknown_controller",
                accept_body(
                    v1_hash,
                    1,
                    testkit::ed25519_id(0x99),
                    ed25519_proof(0x99, &v1_bytes),
                ),
            ),
            (
                "policy_version_mismatch",
                accept_body(v1_hash, 2, e81(), ed25519_proof(0x81, &v1_bytes)),
            ),
            (
                "bad_signature",
                accept_body(v1_hash, 1, e81(), ed25519_proof(0x81, b"other bytes")),
            ),
            (
                "proof_type_mismatch",
                accept_body(
                    v1_hash,
                    1,
                    e81(),
                    ControllerProof::WebAuthn(testkit::assertion_over(&v1_bytes)),
                ),
            ),
        ];
        for (expected, body) in cases {
            let error = replay_with(&chain, forge(&chain, now, now, body), &docs).unwrap_err();
            assert_eq!(error.code_name(), expected);
        }
        assert_eq!(
            ReplayErrorKind::Proof(ProofError::Signature(SignatureError::BadSignature)).code(),
            204
        );
        // Expiry and issue time are checked against the entry's created_at.
        let mut expired = v1.clone();
        expired.expires_at = at(2 * DAY - 1);
        expired.issued_at = at(2 * DAY - 60);
        let mut future = v1.clone();
        future.issued_at = at(2 * DAY + 301);
        future.expires_at = at(2 * DAY + 3600);
        for (doc, expected) in [
            (expired, "policy_expired"),
            (future, "policy_issued_in_future"),
        ] {
            let bytes = doc.encode().unwrap();
            let mut docs = chain.docs.clone();
            let hash = docs.insert(bytes.clone());
            let body = accept_body(hash, 1, e81(), ed25519_proof(0x81, &bytes));
            let error = replay_with(&chain, forge(&chain, now, now, body), &docs).unwrap_err();
            assert_eq!(error.code_name(), expected);
        }
        // Versions increase and chain onto the latest accepted document.
        chain.accept_ed25519(0x81, &v1, now);
        let mut again = testkit::policy_doc(&chain.state, 1, now, DEFAULT_LINKED_RULES);
        again.expires_at = at(2 * DAY + 7200);
        let mut unchained = testkit::policy_doc(&chain.state, 2, now, DEFAULT_LINKED_RULES);
        unchained.prev_policy_hash = Hash32([0; 32]);
        let mut wrong_party = testkit::policy_doc(&chain.state, 2, now, DEFAULT_LINKED_RULES);
        wrong_party.agent_party = crate::party::PartyId::parse(&format!(
            "other::{}",
            crate::party::namespace_for_key(&[7; 32])
        ))
        .unwrap();
        for (doc, expected) in [
            (again, "policy_version_not_increasing"),
            (unchained, "policy_prev_mismatch"),
            (wrong_party, "policy_wrong_party"),
        ] {
            let bytes = doc.encode().unwrap();
            let mut docs = chain.docs.clone();
            let hash = docs.insert(bytes.clone());
            let body = accept_body(hash, doc.version, e81(), ed25519_proof(0x81, &bytes));
            let error = replay_with(
                &chain,
                forge(&chain, at(2 * DAY + 10), at(2 * DAY + 10), body),
                &docs,
            )
            .unwrap_err();
            assert_eq!(error.code_name(), expected);
        }
    }

    const DEFAULT_LINKED_RULES_TEXT: &str = "not the policy";

    #[test]
    fn a_missing_historical_document_breaks_replay() {
        let mut chain = with_ed25519_policy();
        let v1 = testkit::policy_doc(&chain.state, 1, at(2 * DAY), DEFAULT_LINKED_RULES);
        chain.accept_ed25519(0x81, &v1, at(2 * DAY));
        let v2 = testkit::policy_doc(&chain.state, 2, at(2 * DAY + 60), DEFAULT_LINKED_RULES);
        chain.accept_ed25519(0x81, &v2, at(2 * DAY + 60));
        let log = chain.log();
        let mut partial = PolicyDocuments::new();
        partial.insert(v2.encode().unwrap());
        let error = replay(&log, &partial).unwrap_err();
        assert_eq!(
            (error.seq, error.code_name()),
            (3, "missing_policy_document")
        );
        assert!(replay(&log, &chain.docs).is_ok());
    }

    #[test]
    fn replay_refusals() {
        let chain = Chain::linked();
        let now = at(2 * DAY);
        let delayed = at(3 * DAY);
        let reader = testkit::ed25519_controller_from(0x81, caps(&[Read]));
        let add =
            |owner: u64, controller: ControllerDescriptor, descriptor: Option<DescriptorRef>| {
                EntryBody::AddController(AddControllerBody {
                    owner_account: OwnerAccount::new(owner).unwrap(),
                    descriptor,
                    controller,
                })
            };
        let agent_as_controller = ControllerDescriptor::new(
            ControllerKey::Ed25519(testkit::agent_key().verifying_key().to_bytes()),
            label("Agent"),
            caps(&[Read]),
        )
        .unwrap();
        let passkey_add =
            testkit::add_descriptor(testkit::passkey_controller_from(0x51, caps(&[Read])), 42);
        let passkey_ref = DescriptorRef {
            nonce: *passkey_add.nonce(),
            expires_at: passkey_add.expires_at(),
            commitment: passkey_add.commitment().unwrap(),
        };
        let cases: Vec<(&str, UtcTime, UtcTime, EntryBody)> = vec![
            ("owner_mismatch", now, delayed, add(7, reader.clone(), None)),
            (
                "duplicate_controller",
                now,
                delayed,
                add(42, testkit::ed25519_controller(caps(&[Read])), None),
            ),
            (
                "agent_key_as_controller",
                now,
                delayed,
                add(42, agent_as_controller, None),
            ),
            (
                "commitment_required",
                now,
                delayed,
                add(42, passkey_add.controllers()[0].clone(), None),
            ),
            (
                "effective_at_mismatch",
                now,
                at(3 * DAY - 1),
                add(42, reader.clone(), None),
            ),
            (
                "commitment_mismatch",
                now,
                delayed,
                add(
                    42,
                    passkey_add.controllers()[0].clone(),
                    Some(DescriptorRef {
                        commitment: crate::canon::values::Commitment([0; 16]),
                        ..passkey_ref
                    }),
                ),
            ),
            (
                "descriptor_expired",
                at(8 * DAY),
                at(9 * DAY),
                add(42, passkey_add.controllers()[0].clone(), Some(passkey_ref)),
            ),
            (
                "forced_mismatch",
                now,
                delayed,
                EntryBody::RemoveController(RemoveControllerBody {
                    controller_id: e21(),
                    forced: false,
                }),
            ),
            (
                "forced_mismatch",
                now,
                delayed,
                EntryBody::SetCapabilities(SetCapabilitiesBody {
                    controller_id: passkey(),
                    capabilities: caps(&[Direct]),
                    forced: false,
                }),
            ),
            (
                "no_change",
                now,
                delayed,
                EntryBody::SetCapabilities(SetCapabilitiesBody {
                    controller_id: e21(),
                    capabilities: caps(&[Read]),
                    forced: false,
                }),
            ),
            (
                "unknown_controller",
                now,
                delayed,
                EntryBody::RemoveController(RemoveControllerBody {
                    controller_id: e81(),
                    forced: false,
                }),
            ),
        ];
        for (expected, created_at, effective_at, body) in cases {
            assert_eq!(
                refusal(&chain, forge(&chain, created_at, effective_at, body)),
                expected
            );
        }
        // The descriptor-backed add itself replays.
        let ok = forge(
            &chain,
            now,
            delayed,
            add(42, passkey_add.controllers()[0].clone(), Some(passkey_ref)),
        );
        assert!(replay_with(&chain, ok, &chain.docs).is_ok());
        // A first entry that is not a link, and an entry signed out of time order.
        let empty = Chain::new();
        let first = forge(&empty, now, delayed, add(42, reader.clone(), None));
        let log = verify_log(Network::Devnet, &testkit::agent_party(), &[first]).unwrap();
        assert_eq!(
            replay(&log, &empty.docs).unwrap_err().code_name(),
            "first_entry_not_link"
        );
        let backdated = forge(&chain, at(-1), at(DAY - 1), add(42, reader, None));
        let mut entries = chain.entries.clone();
        entries.push(backdated.clone());
        assert_eq!(
            verify_log(Network::Devnet, &testkit::agent_party(), &entries)
                .unwrap_err()
                .code_name(),
            "time_regression"
        );
        let mut state = chain.state.clone();
        let kind =
            apply_entry(&mut state, backdated.entry(), backdated.hash(), &chain.docs).unwrap_err();
        assert_eq!(kind, ReplayErrorKind::TimeRegression);
    }

    #[test]
    fn an_invite_link_is_accepted_only_as_the_first_entry() {
        let mut chain = Chain::linked();
        for id in [passkey(), e21()] {
            let pending = chain
                .builder()
                .remove_controller(&id, true, at(2 * DAY))
                .unwrap();
            chain.push(&pending);
        }
        let invite = testkit::invite_descriptor();
        let body = EntryBody::Link(LinkBody {
            owner_account: invite.owner_account(),
            invite: true,
            nonce: *invite.nonce(),
            expires_at: invite.expires_at(),
            commitment: invite.commitment().unwrap(),
            controllers: invite.controllers().to_vec(),
        });
        let relink = forge(&chain, at(3 * DAY), at(4 * DAY), body);
        assert_eq!(refusal(&chain, relink), "invite_not_first");
    }

    #[test]
    fn the_controller_cap_is_enforced() {
        let mut chain = Chain::linked();
        for i in 0..30u8 {
            let key = testkit::ed_key(0x90u8.wrapping_add(i))
                .verifying_key()
                .to_bytes();
            let pending = chain
                .builder()
                .add_ed25519(
                    key,
                    label("Extra key"),
                    caps(&[Read]),
                    at(DAY + i64::from(i)),
                )
                .unwrap();
            chain.push(&pending);
        }
        assert_eq!(chain.state.controllers().count(), MAX_AGENT_CONTROLLERS);
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let refused =
            chain
                .builder()
                .add_ed25519(key, label("One too many"), caps(&[Read]), at(2 * DAY));
        assert_eq!(
            replay_error(refused.unwrap_err()),
            ReplayErrorKind::TooManyControllers
        );
    }

    #[test]
    fn extend_applies_only_the_new_entries() {
        let mut chain = Chain::linked();
        let before = chain.state.clone();
        add_e81(&mut chain, caps(&[Read]), at(DAY));
        let extended = before.extend(&chain.log(), &chain.docs).unwrap();
        assert_eq!(extended, chain.state);
        let other = Chain::linked();
        let mut diverged = other.state.clone();
        diverged.head = Some(HeadRef {
            seq: 1,
            hash: Hash32([1; 32]),
        });
        assert_eq!(
            diverged
                .extend(&chain.log(), &chain.docs)
                .unwrap_err()
                .code_name(),
            "prev_mismatch"
        );
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use crate::builder::{BuildError, EntryBuilder};
    use crate::canon::values::Label;
    use crate::testkit::{self, Chain, DAY, at};

    fn narrow_rules() -> Rules {
        let mut rules = DEFAULT_LINKED_RULES;
        rules.transfers.enabled = false;
        rules
    }

    fn accept(chain: &mut Chain, version: u64, rules: Rules, when: UtcTime) -> UtcTime {
        let doc = testkit::policy_doc(&chain.state, version, when, rules);
        chain.accept_passkey(&doc, when).effective_at()
    }

    #[test]
    fn a_narrowing_applies_at_once() {
        let mut chain = Chain::linked();
        let effective = accept(&mut chain, 1, narrow_rules(), at(2 * DAY));
        assert_eq!(effective, at(2 * DAY));
        assert!(!chain.state.rules_at(at(2 * DAY)).rules().transfers.enabled);
        assert!(chain.state.scheduled_policy(at(2 * DAY)).is_none());
    }

    #[test]
    fn a_widening_is_scheduled_and_applies_only_from_its_time() {
        let mut chain = Chain::linked();
        let effective = accept(&mut chain, 1, testkit::undirected_rules(), at(2 * DAY));
        assert_eq!(effective, at(3 * DAY));
        let state = &chain.state;
        assert_eq!(state.rules_at(at(3 * DAY - 1)), EffectiveRules::Default);
        assert!(
            state
                .rules_at(at(3 * DAY - 1))
                .rules()
                .transfers
                .directed_only
        );
        assert_eq!(
            state.scheduled_policy(at(3 * DAY - 1)).unwrap().version(),
            1
        );
        assert!(!state.rules_at(at(3 * DAY)).rules().transfers.directed_only);
        assert!(state.scheduled_policy(at(3 * DAY)).is_none());
        assert_eq!(state.latest_policy().unwrap().version(), 1);
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_narrowing_during_the_delay_cancels_the_scheduled_widening() {
        let mut chain = Chain::linked();
        accept(&mut chain, 1, testkit::undirected_rules(), at(2 * DAY));
        let effective = accept(&mut chain, 2, narrow_rules(), at(2 * DAY + 600));
        assert_eq!(effective, at(2 * DAY + 600));
        let state = &chain.state;
        assert!(state.scheduled_policy(at(2 * DAY + 600)).is_none());
        for when in [at(3 * DAY), at(9 * DAY)] {
            let in_force = state.in_force_policy(when).unwrap();
            assert_eq!(in_force.version(), 2);
            assert!(in_force.rules().transfers.directed_only);
        }
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn a_second_widening_replaces_the_first_and_due_ones_are_promoted() {
        let mut chain = Chain::linked();
        accept(&mut chain, 1, testkit::undirected_rules(), at(2 * DAY));
        let mut wider = testkit::undirected_rules();
        wider.trading.lp_enabled = false;
        wider.transfers.recipients.any = true;
        let mut second = wider.clone();
        second.delays.transfers_secs = 3600;
        let effective = accept(&mut chain, 2, second, at(2 * DAY + 60));
        assert_eq!(effective, at(3 * DAY + 60));
        let state = &chain.state;
        assert_eq!(state.scheduled_policy(at(3 * DAY)).unwrap().version(), 2);
        assert_eq!(state.rules_at(at(3 * DAY)), EffectiveRules::Default);
        // After the scheduled time, the next accept compares against the promoted policy.
        let mut third = wider;
        third.delays.transfers_secs = 1800;
        let effective = accept(&mut chain, 3, third, at(4 * DAY));
        assert_eq!(effective, at(4 * DAY + 3600));
        let state = &chain.state;
        assert_eq!(state.in_force_policy(at(4 * DAY)).unwrap().version(), 2);
        assert_eq!(
            state.in_force_policy(at(4 * DAY + 3600)).unwrap().version(),
            3
        );
        assert_eq!(chain.replayed(), chain.state);
    }

    #[test]
    fn not_before_holds_back_even_a_narrowing() {
        let mut chain = Chain::linked();
        let mut doc = testkit::policy_doc(&chain.state, 1, at(2 * DAY), narrow_rules());
        doc.not_before = Some(at(5 * DAY));
        let effective = chain.accept_passkey(&doc, at(2 * DAY)).effective_at();
        assert_eq!(effective, at(5 * DAY));
        assert_eq!(
            chain.state.rules_at(at(5 * DAY - 1)),
            EffectiveRules::Default
        );
        assert!(!chain.state.rules_at(at(5 * DAY)).rules().transfers.enabled);
    }

    #[test]
    fn the_log_cap_is_enforced_by_the_builder() {
        let mut state = Chain::linked().state;
        state.head = Some(HeadRef {
            seq: u64::try_from(crate::log::MAX_LOG_ENTRIES).unwrap(),
            hash: Hash32([1; 32]),
        });
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let refused = EntryBuilder::new(&state)
            .add_ed25519(
                key,
                Label::parse("Late").unwrap(),
                CapabilitySet::ALL,
                at(DAY),
            )
            .unwrap_err();
        assert_eq!(refused, BuildError::LogFull);
    }
}
