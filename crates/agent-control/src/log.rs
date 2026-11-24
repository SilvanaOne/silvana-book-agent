//! Verification and comparison of agent-signed, hash-chained control logs.

use std::cmp::Ordering;

use crate::canon::time::UtcTime;
use crate::canon::values::{Hash32, Network};
use crate::entry::SignedEntry;
use crate::error::{LogError, LogErrorKind};
use crate::party::{PartyId, verify_party_key};
use crate::sig::ed25519::verify_ed25519_strict;

/// Most entries one log may hold; the agent refuses to sign beyond it.
pub const MAX_LOG_ENTRIES: usize = 4096;

/// A position in a log: the sequence number and the hash of that entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeadRef {
    pub seq: u64,
    pub hash: Hash32,
}

impl HeadRef {
    /// `ctl_seq` and `ctl_head` from a token: both absent, or seq >= 1 with lowercase hex32.
    pub fn from_jwt_claims(
        ctl_seq: Option<u64>,
        ctl_head: Option<&str>,
    ) -> Result<Option<Self>, LogError> {
        let invalid = LogError::new(0, LogErrorKind::InvalidHeadClaim);
        match (ctl_seq, ctl_head) {
            (None, None) => Ok(None),
            (Some(seq), Some(head)) if seq >= 1 => Hash32::parse(head)
                .map(|hash| Some(Self { seq, hash }))
                .map_err(|_| invalid),
            _ => Err(invalid),
        }
    }
}

/// A log whose every entry passed `verify_log`; never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedLog {
    network: Network,
    agent_party: PartyId,
    agent_key: [u8; 32],
    entries: Vec<SignedEntry>,
    head: HeadRef,
}

impl VerifiedLog {
    pub fn network(&self) -> Network {
        self.network
    }

    pub fn agent_party(&self) -> &PartyId {
        &self.agent_party
    }

    pub fn agent_key(&self) -> &[u8; 32] {
        &self.agent_key
    }

    /// Ascending by seq, starting at 1.
    pub fn entries(&self) -> &[SignedEntry] {
        &self.entries
    }

    pub fn head(&self) -> HeadRef {
        self.head
    }

    pub fn entry(&self, seq: u64) -> Option<&SignedEntry> {
        let index = usize::try_from(seq.checked_sub(1)?).ok()?;
        self.entries.get(index)
    }
}

/// The running checks of a chain being verified.
struct Chain<'a> {
    network: Network,
    party: &'a PartyId,
    key: Option<[u8; 32]>,
    head: Option<HeadRef>,
    last_created_at: Option<UtcTime>,
}

impl<'a> Chain<'a> {
    fn new(network: Network, party: &'a PartyId) -> Self {
        Self {
            network,
            party,
            key: None,
            head: None,
            last_created_at: None,
        }
    }

    fn push(&mut self, signed: &SignedEntry) -> Result<(), LogError> {
        let expected_seq = match self.head {
            Some(head) => head.seq.saturating_add(1),
            None => 1,
        };
        let fail = |kind| Err(LogError::new(expected_seq, kind));
        let entry = signed.entry();
        if entry.network() != self.network {
            return fail(LogErrorKind::WrongNetwork);
        }
        if entry.agent_party() != self.party {
            return fail(LogErrorKind::WrongParty);
        }
        match self.key {
            None if !verify_party_key(self.party, entry.agent_key()) => {
                return fail(LogErrorKind::KeyPartyMismatch);
            }
            Some(key) if key != *entry.agent_key() => return fail(LogErrorKind::KeyChanged),
            _ => {}
        }
        if entry.seq() != expected_seq {
            return fail(LogErrorKind::UnexpectedSeq);
        }
        let expected_prev = self.head.map_or(Hash32([0; 32]), |head| head.hash);
        if *entry.prev() != expected_prev {
            return fail(LogErrorKind::PrevMismatch);
        }
        if self
            .last_created_at
            .is_some_and(|last| entry.created_at() < last)
        {
            return fail(LogErrorKind::TimeRegression);
        }
        if let Err(e) = verify_ed25519_strict(entry.agent_key(), signed.bytes(), signed.signature())
        {
            return fail(LogErrorKind::Signature(e));
        }
        self.key = Some(*entry.agent_key());
        self.head = Some(HeadRef {
            seq: expected_seq,
            hash: *signed.hash(),
        });
        self.last_created_at = Some(entry.created_at());
        Ok(())
    }
}

/// Decodes stored rows; a refusal names the row's position as its seq.
pub fn decode_entries(rows: &[(Vec<u8>, [u8; 64])]) -> Result<Vec<SignedEntry>, LogError> {
    let mut out = Vec::with_capacity(rows.len().min(MAX_LOG_ENTRIES));
    for (index, (bytes, signature)) in rows.iter().enumerate() {
        let seq = u64::try_from(index)
            .ok()
            .and_then(|i| i.checked_add(1))
            .unwrap_or(u64::MAX);
        let entry = SignedEntry::new(bytes.clone(), *signature)
            .map_err(|e| LogError::new(seq, LogErrorKind::Canon(e)))?;
        out.push(entry);
    }
    Ok(out)
}

/// Checks network, party, key binding, seq, prev, time order and every signature.
pub fn verify_log(
    network: Network,
    party: &PartyId,
    entries: &[SignedEntry],
) -> Result<VerifiedLog, LogError> {
    if entries.is_empty() {
        return Err(LogError::new(0, LogErrorKind::Empty));
    }
    if entries.len() > MAX_LOG_ENTRIES {
        return Err(LogError::new(0, LogErrorKind::TooManyEntries));
    }
    let mut chain = Chain::new(network, party);
    for entry in entries {
        chain.push(entry)?;
    }
    let (Some(agent_key), Some(head)) = (chain.key, chain.head) else {
        return Err(LogError::new(0, LogErrorKind::Empty));
    };
    Ok(VerifiedLog {
        network,
        agent_party: party.clone(),
        agent_key,
        entries: entries.to_vec(),
        head,
    })
}

/// The same checks for entries appended after `base`'s head.
pub fn verify_extension(
    base: &VerifiedLog,
    appended: &[SignedEntry],
) -> Result<VerifiedLog, LogError> {
    if appended.is_empty() {
        return Err(LogError::new(0, LogErrorKind::Empty));
    }
    let total = base.entries.len().checked_add(appended.len());
    if total.is_none_or(|n| n > MAX_LOG_ENTRIES) {
        return Err(LogError::new(0, LogErrorKind::TooManyEntries));
    }
    let mut chain = Chain {
        network: base.network,
        party: &base.agent_party,
        key: Some(base.agent_key),
        head: Some(base.head),
        last_created_at: base.entries.last().map(SignedEntry::created_at),
    };
    for entry in appended {
        chain.push(entry)?;
    }
    let Some(head) = chain.head else {
        return Err(LogError::new(0, LogErrorKind::Empty));
    };
    let mut entries = base.entries.clone();
    entries.extend_from_slice(appended);
    Ok(VerifiedLog {
        network: base.network,
        agent_party: base.agent_party.clone(),
        agent_key: base.agent_key,
        entries,
        head,
    })
}

/// Two signed entries with one seq and different bytes: the agent key signed both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkEvidence {
    seq: u64,
    local: SignedEntry,
    remote: SignedEntry,
}

impl ForkEvidence {
    /// Rebuilds evidence from stored entries; `verify` checks it.
    pub fn new(local: SignedEntry, remote: SignedEntry) -> Self {
        Self {
            seq: local.seq(),
            local,
            remote,
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn local(&self) -> &SignedEntry {
        &self.local
    }

    pub fn remote(&self) -> &SignedEntry {
        &self.remote
    }

    /// Both entries are bound to the party's key, share the seq, differ, and verify.
    pub fn verify(&self, network: Network, party: &PartyId) -> Result<(), LogError> {
        let fail = |kind| Err(LogError::new(self.seq, kind));
        for signed in [&self.local, &self.remote] {
            let entry = signed.entry();
            if entry.network() != network {
                return fail(LogErrorKind::WrongNetwork);
            }
            if entry.agent_party() != party {
                return fail(LogErrorKind::WrongParty);
            }
            if !verify_party_key(party, entry.agent_key()) {
                return fail(LogErrorKind::KeyPartyMismatch);
            }
            if entry.seq() != self.seq {
                return fail(LogErrorKind::UnexpectedSeq);
            }
            if let Err(e) =
                verify_ed25519_strict(entry.agent_key(), signed.bytes(), signed.signature())
            {
                return fail(LogErrorKind::Signature(e));
            }
        }
        if self.local.hash() == self.remote.hash() {
            return fail(LogErrorKind::NotAFork);
        }
        Ok(())
    }
}

/// How a remote copy relates to the local log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogComparison {
    Equal,
    /// The remote copy is a prefix: a rollback or a hidden tail.
    RemoteBehind {
        local: HeadRef,
        remote: HeadRef,
    },
    /// The local copy is a prefix of the remote one.
    RemoteAhead {
        local: HeadRef,
        remote: HeadRef,
    },
    Fork(Box<ForkEvidence>),
}

/// Compares two verified logs of the same agent entry by entry.
pub fn compare_logs(local: &VerifiedLog, remote: &VerifiedLog) -> Result<LogComparison, LogError> {
    if local.network != remote.network {
        return Err(LogError::new(0, LogErrorKind::WrongNetwork));
    }
    if local.agent_party != remote.agent_party {
        return Err(LogError::new(0, LogErrorKind::WrongParty));
    }
    if local.agent_key != remote.agent_key {
        return Err(LogError::new(0, LogErrorKind::KeyChanged));
    }
    for (mine, theirs) in local.entries.iter().zip(remote.entries.iter()) {
        if mine.hash() != theirs.hash() {
            return Ok(LogComparison::Fork(Box::new(ForkEvidence::new(
                mine.clone(),
                theirs.clone(),
            ))));
        }
    }
    let heads = (local.head, remote.head);
    Ok(match local.entries.len().cmp(&remote.entries.len()) {
        Ordering::Equal => LogComparison::Equal,
        Ordering::Greater => LogComparison::RemoteBehind {
            local: heads.0,
            remote: heads.1,
        },
        Ordering::Less => LogComparison::RemoteAhead {
            local: heads.0,
            remote: heads.1,
        },
    })
}

/// A head claimed by a token, checked against a verified log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadCheck {
    /// The claim is the log's head.
    Matches,
    /// The claim is an older entry of this log.
    ClaimBehind { head: HeadRef },
    /// The claim is beyond this log's head.
    LogBehind { head: HeadRef },
    /// The log has a different entry at the claimed seq.
    Mismatch,
}

pub fn check_head(log: &VerifiedLog, claimed: &HeadRef) -> HeadCheck {
    let head = log.head;
    if claimed.seq > head.seq {
        return HeadCheck::LogBehind { head };
    }
    match log.entry(claimed.seq) {
        Some(entry) if *entry.hash() == claimed.hash => {
            if claimed.seq == head.seq {
                HeadCheck::Matches
            } else {
                HeadCheck::ClaimBehind { head }
            }
        }
        _ => HeadCheck::Mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::EntryBuilder;
    use crate::canon::values::Label;
    use crate::controller::{Capability, CapabilitySet};
    use crate::entry::{EntryKind, LogEntry};
    use crate::policy::DEFAULT_LINKED_RULES;
    use crate::state::replay;
    use crate::testkit::{self, Chain, DAY, DEVNET_DELAY, at};
    use ed25519_dalek::{Signer, SigningKey};

    fn party() -> PartyId {
        testkit::agent_party()
    }

    fn read_only() -> CapabilitySet {
        CapabilitySet::of(&[Capability::Read])
    }

    /// Six entries covering every kind.
    fn full_chain() -> Chain {
        let mut chain = Chain::linked();
        let passkey = testkit::passkey_controller_from(0x51, read_only());
        let passkey_id = *passkey.id();
        let add = testkit::add_descriptor(passkey, 42);
        let pending = chain
            .builder()
            .add_controller_from_code(
                &add.encode().unwrap(),
                &add.commitment().unwrap(),
                &testkit::rp(),
                at(DAY),
            )
            .unwrap();
        chain.push(&pending);
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let label = Label::parse("Backup key").unwrap();
        let policy = CapabilitySet::of(&[Capability::Policy]);
        let pending = chain
            .builder()
            .add_ed25519(key, label, policy, at(DAY + 10))
            .unwrap();
        chain.push(&pending);
        let direct_read = CapabilitySet::of(&[Capability::Direct, Capability::Read]);
        let pending = chain
            .builder()
            .set_capabilities(&testkit::ed25519_id(0x21), direct_read, false, at(DAY + 20))
            .unwrap();
        chain.push(&pending);
        // E81 signs the policy as soon as its add takes effect.
        let accepted = at(DAY + 10 + DEVNET_DELAY);
        let doc = testkit::policy_doc(&chain.state, 1, accepted, DEFAULT_LINKED_RULES);
        chain.accept_ed25519(0x81, &doc, accepted);
        let pending = chain
            .builder()
            .remove_controller(&passkey_id, false, at(DAY + 20 + DEVNET_DELAY))
            .unwrap();
        chain.push(&pending);
        chain
    }

    fn resign(entry: &LogEntry, key: &SigningKey) -> SignedEntry {
        let bytes = entry.encode().unwrap();
        let signature = key.sign(&bytes).to_bytes();
        SignedEntry::new(bytes, signature).unwrap()
    }

    fn refusal(entries: &[SignedEntry]) -> (u64, &'static str) {
        let error = verify_log(Network::Devnet, &party(), entries).unwrap_err();
        (error.seq, error.code_name())
    }

    #[test]
    fn a_log_covering_every_kind_verifies_and_replays() {
        let chain = full_chain();
        let log = chain.log();
        let kinds: Vec<EntryKind> = log.entries().iter().map(SignedEntry::kind).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::Link,
                EntryKind::AddController,
                EntryKind::AddController,
                EntryKind::SetCapabilities,
                EntryKind::AcceptPolicy,
                EntryKind::RemoveController,
            ]
        );
        assert_eq!(log.head().seq, 6);
        assert_eq!(&log.head().hash, chain.entries[5].hash());
        assert_eq!(log.entry(1).unwrap().seq(), 1);
        assert!(log.entry(0).is_none() && log.entry(7).is_none());
        assert_eq!(replay(&log, &chain.docs).unwrap(), chain.state);
        let rows: Vec<(Vec<u8>, [u8; 64])> = chain
            .entries
            .iter()
            .map(|e| (e.bytes().to_vec(), *e.signature()))
            .collect();
        assert_eq!(decode_entries(&rows).unwrap(), chain.entries);
    }

    #[test]
    fn log_refusals_have_stable_names_and_positions() {
        let chain = full_chain();
        let entries = chain.entries.clone();
        assert_eq!(refusal(&[]), (0, "empty_log"));
        let too_many = vec![entries[0].clone(); MAX_LOG_ENTRIES + 1];
        assert_eq!(refusal(&too_many), (0, "too_many_entries"));
        let mut gap = entries.clone();
        gap.remove(1);
        assert_eq!(refusal(&gap), (2, "unexpected_seq"));
        let mut duplicate = entries.clone();
        duplicate.insert(2, entries[1].clone());
        assert_eq!(refusal(&duplicate), (3, "unexpected_seq"));
        let agent = testkit::agent_key();
        let mut wrong_prev = entries.clone();
        let mut entry = entries[1].entry().clone();
        entry.prev = Hash32([1; 32]);
        wrong_prev[1] = resign(&entry, &agent);
        assert_eq!(refusal(&wrong_prev), (2, "prev_mismatch"));
        let error = verify_log(Network::Mainnet, &party(), &entries).unwrap_err();
        assert_eq!((error.seq, error.code_name()), (1, "wrong_network"));
        let other_party = PartyId::parse(&format!(
            "agent-1::{}",
            crate::party::namespace_for_key(&[9; 32])
        ))
        .unwrap();
        let error = verify_log(Network::Devnet, &other_party, &entries).unwrap_err();
        assert_eq!((error.seq, error.code_name()), (1, "wrong_party"));
        let intruder = testkit::ed_key(0x77);
        let mut unbound = entries.clone();
        let mut entry = entries[0].entry().clone();
        entry.agent_key = intruder.verifying_key().to_bytes();
        unbound[0] = resign(&entry, &intruder);
        assert_eq!(refusal(&unbound), (1, "key_party_mismatch"));
        let mut changed = entries.clone();
        let mut entry = entries[1].entry().clone();
        entry.agent_key = intruder.verifying_key().to_bytes();
        changed[1] = resign(&entry, &intruder);
        assert_eq!(refusal(&changed), (2, "key_changed"));
        let mut bad_signature = entries.clone();
        let mut signature = *entries[2].signature();
        signature[10] ^= 1;
        bad_signature[2] = SignedEntry::new(entries[2].bytes().to_vec(), signature).unwrap();
        assert_eq!(refusal(&bad_signature), (3, "bad_signature"));
        // A changed byte that still decodes fails the signature.
        let mut tampered = entries.clone();
        let text = String::from_utf8(entries[3].bytes().to_vec()).unwrap();
        let edited = text.replacen(
            "created_at=2026-11-23T12:00:20Z",
            "created_at=2026-11-23T12:00:21Z",
            1,
        );
        assert_ne!(edited, text);
        tampered[3] = SignedEntry::new(edited.into_bytes(), *entries[3].signature()).unwrap();
        assert_eq!(refusal(&tampered), (4, "bad_signature"));
        // A changed byte that does not decode is caught when the rows are decoded.
        let mut rows: Vec<(Vec<u8>, [u8; 64])> = entries
            .iter()
            .map(|e| (e.bytes().to_vec(), *e.signature()))
            .collect();
        rows[4].0.push(b'x');
        let error = decode_entries(&rows).unwrap_err();
        assert_eq!((error.seq, error.code_name()), (5, "missing_final_newline"));
    }

    #[test]
    fn extensions_must_continue_the_head() {
        let chain = full_chain();
        let base = verify_log(Network::Devnet, &party(), &chain.entries[..3]).unwrap();
        let extended = verify_extension(&base, &chain.entries[3..]).unwrap();
        assert_eq!(extended, chain.log());
        assert_eq!(
            verify_extension(&base, &[]).unwrap_err().code_name(),
            "empty_log"
        );
        let error = verify_extension(&base, &chain.entries[4..]).unwrap_err();
        assert_eq!((error.seq, error.code_name()), (4, "unexpected_seq"));
        let error = verify_extension(&base, &chain.entries[2..]).unwrap_err();
        assert_eq!((error.seq, error.code_name()), (4, "unexpected_seq"));
    }

    #[test]
    fn compare_logs_finds_hidden_tails_and_forks() {
        let chain = full_chain();
        let local = chain.log();
        let prefix = verify_log(Network::Devnet, &party(), &chain.entries[..4]).unwrap();
        assert_eq!(
            compare_logs(&local, &local.clone()).unwrap(),
            LogComparison::Equal
        );
        assert_eq!(
            compare_logs(&local, &prefix).unwrap(),
            LogComparison::RemoteBehind {
                local: local.head(),
                remote: prefix.head()
            }
        );
        assert_eq!(
            compare_logs(&prefix, &local).unwrap(),
            LogComparison::RemoteAhead {
                local: prefix.head(),
                remote: local.head()
            }
        );
        // Another entry at seq 5, signed by the same key from the same prefix.
        let state = replay(&prefix, &chain.docs).unwrap();
        let key = testkit::ed_key(0x82).verifying_key().to_bytes();
        let pending = EntryBuilder::new(&state)
            .add_ed25519(
                key,
                Label::parse("Other key").unwrap(),
                read_only(),
                at(DAY + 15 + DEVNET_DELAY),
            )
            .unwrap();
        let mut forked = chain.entries[..4].to_vec();
        forked.push(testkit::sign_pending(&pending));
        let remote = verify_log(Network::Devnet, &party(), &forked).unwrap();
        let LogComparison::Fork(evidence) = compare_logs(&local, &remote).unwrap() else {
            panic!("expected a fork");
        };
        assert_eq!(evidence.seq(), 5);
        assert_eq!(evidence.local(), &chain.entries[4]);
        assert_eq!(evidence.verify(Network::Devnet, &party()), Ok(()));
        let stored = ForkEvidence::new(evidence.local().clone(), evidence.remote().clone());
        assert_eq!(stored, *evidence);
        let same = ForkEvidence::new(chain.entries[4].clone(), chain.entries[4].clone());
        assert_eq!(
            same.verify(Network::Devnet, &party())
                .unwrap_err()
                .code_name(),
            "not_a_fork"
        );
        let skewed = ForkEvidence::new(chain.entries[4].clone(), chain.entries[3].clone());
        assert_eq!(
            skewed
                .verify(Network::Devnet, &party())
                .unwrap_err()
                .code_name(),
            "unexpected_seq"
        );
        assert_eq!(
            evidence
                .verify(Network::Mainnet, &party())
                .unwrap_err()
                .code_name(),
            "wrong_network"
        );
    }

    #[test]
    fn head_checks() {
        let chain = full_chain();
        let log = chain.log();
        let head = log.head();
        assert_eq!(check_head(&log, &head), HeadCheck::Matches);
        let older = HeadRef {
            seq: 2,
            hash: *chain.entries[1].hash(),
        };
        assert_eq!(check_head(&log, &older), HeadCheck::ClaimBehind { head });
        let ahead = HeadRef {
            seq: 7,
            hash: Hash32([1; 32]),
        };
        assert_eq!(check_head(&log, &ahead), HeadCheck::LogBehind { head });
        let wrong = HeadRef {
            seq: 3,
            hash: *chain.entries[1].hash(),
        };
        assert_eq!(check_head(&log, &wrong), HeadCheck::Mismatch);
        assert_eq!(
            check_head(
                &log,
                &HeadRef {
                    seq: 0,
                    hash: Hash32([0; 32])
                }
            ),
            HeadCheck::Mismatch
        );
    }

    #[test]
    fn head_claims_come_in_pairs() {
        let hex = "ab".repeat(32);
        assert_eq!(HeadRef::from_jwt_claims(None, None), Ok(None));
        assert_eq!(
            HeadRef::from_jwt_claims(Some(3), Some(&hex)),
            Ok(Some(HeadRef {
                seq: 3,
                hash: Hash32([0xab; 32])
            }))
        );
        let upper = hex.to_uppercase();
        let short = "ab".repeat(31);
        let zeros = "00".repeat(32);
        for (seq, head) in [
            (Some(3), None),
            (None, Some(hex.as_str())),
            (Some(0), Some(zeros.as_str())),
            (Some(3), Some(upper.as_str())),
            (Some(3), Some(short.as_str())),
            (Some(3), Some("")),
        ] {
            let error = HeadRef::from_jwt_claims(seq, head).unwrap_err();
            assert_eq!(error.code_name(), "invalid_head_claim", "{seq:?} {head:?}");
        }
    }
}
