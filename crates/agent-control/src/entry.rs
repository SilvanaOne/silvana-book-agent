//! Control log entries (`silvana.agent.control.v1`) and signed entries.

use crate::canon::time::UtcTime;
use crate::canon::values::{
    Commitment, Hash32, Network, Nonce16, OwnerAccount, format_bool, parse_bool, parse_hex_array,
    parse_u64, sha256,
};
use crate::canon::{Reader, Writer, check_ascending, decode_exact};
use crate::controller::{
    CapabilitySet, ControllerDescriptor, ControllerId, read_controller, write_controller,
};
use crate::error::{CanonError, CanonErrorKind, ValueError, ValueKind};
use crate::link::MAX_LINK_CONTROLLERS;
use crate::party::PartyId;
use crate::proof::ControllerProof;
use crate::{MAX_ENTRY_BYTES, SCHEME_CONTROL};

/// The change an entry records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Link,
    AddController,
    RemoveController,
    SetCapabilities,
    AcceptPolicy,
}

impl EntryKind {
    pub const ALL: [Self; 5] = [
        Self::Link,
        Self::AddController,
        Self::RemoveController,
        Self::SetCapabilities,
        Self::AcceptPolicy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::AddController => "add_controller",
            Self::RemoveController => "remove_controller",
            Self::SetCapabilities => "set_capabilities",
            Self::AcceptPolicy => "accept_policy",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "link" => Ok(Self::Link),
            "add_controller" => Ok(Self::AddController),
            "remove_controller" => Ok(Self::RemoveController),
            "set_capabilities" => Ok(Self::SetCapabilities),
            "accept_policy" => Ok(Self::AcceptPolicy),
            _ => Err(ValueError(ValueKind::EntryKind)),
        }
    }
}

/// The descriptor fields an `add_controller` entry keeps to recompute the commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DescriptorRef {
    pub nonce: Nonce16,
    pub expires_at: UtcTime,
    pub commitment: Commitment,
}

/// `link`: the owner and the first controllers; `invite` means the descriptor had no party.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkBody {
    pub owner_account: OwnerAccount,
    pub invite: bool,
    pub nonce: Nonce16,
    pub expires_at: UtcTime,
    pub commitment: Commitment,
    pub controllers: Vec<ControllerDescriptor>,
}

/// `add_controller`: from a page descriptor, or a local Ed25519 key with no descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddControllerBody {
    pub owner_account: OwnerAccount,
    pub descriptor: Option<DescriptorRef>,
    pub controller: ControllerDescriptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoveControllerBody {
    pub controller_id: ControllerId,
    pub forced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetCapabilitiesBody {
    pub controller_id: ControllerId,
    pub capabilities: CapabilitySet,
    pub forced: bool,
}

/// `accept_policy`: the document hash and version, its signer and the signer's proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptPolicyBody {
    pub policy_hash: Hash32,
    pub policy_version: u64,
    pub signed_by: ControllerId,
    pub proof: ControllerProof,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryBody {
    Link(LinkBody),
    AddController(AddControllerBody),
    RemoveController(RemoveControllerBody),
    SetCapabilities(SetCapabilitiesBody),
    AcceptPolicy(AcceptPolicyBody),
}

impl EntryBody {
    pub fn kind(&self) -> EntryKind {
        match self {
            Self::Link(_) => EntryKind::Link,
            Self::AddController(_) => EntryKind::AddController,
            Self::RemoveController(_) => EntryKind::RemoveController,
            Self::SetCapabilities(_) => EntryKind::SetCapabilities,
            Self::AcceptPolicy(_) => EntryKind::AcceptPolicy,
        }
    }
}

/// One decoded log entry. Only this crate builds new ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub(crate) network: Network,
    pub(crate) agent_party: PartyId,
    pub(crate) agent_key: [u8; 32],
    pub(crate) seq: u64,
    pub(crate) prev: Hash32,
    pub(crate) created_at: UtcTime,
    pub(crate) effective_at: UtcTime,
    pub(crate) body: EntryBody,
}

impl LogEntry {
    pub fn network(&self) -> Network {
        self.network
    }

    pub fn agent_party(&self) -> &PartyId {
        &self.agent_party
    }

    pub fn agent_key(&self) -> &[u8; 32] {
        &self.agent_key
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn prev(&self) -> &Hash32 {
        &self.prev
    }

    pub fn created_at(&self) -> UtcTime {
        self.created_at
    }

    pub fn effective_at(&self) -> UtcTime {
        self.effective_at
    }

    pub fn kind(&self) -> EntryKind {
        self.body.kind()
    }

    pub fn body(&self) -> &EntryBody {
        &self.body
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_CONTROL, MAX_ENTRY_BYTES);
        w.line("network", self.network.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("agent_key", &hex::encode(self.agent_key));
        w.line("seq", &self.seq.to_string());
        w.line("prev", &self.prev.to_string());
        w.line("created_at", &self.created_at.to_string());
        w.line("kind", self.kind().as_str());
        w.line("effective_at", &self.effective_at.to_string());
        match &self.body {
            EntryBody::Link(body) => write_link(&mut w, self.network, body),
            EntryBody::AddController(body) => {
                w.line("owner_account", &body.owner_account.to_string());
                let descriptor = body.descriptor.as_ref();
                w.opt("nonce", descriptor.map(|d| d.nonce.to_string()).as_deref());
                w.opt(
                    "expires_at",
                    descriptor.map(|d| d.expires_at.to_string()).as_deref(),
                );
                w.opt(
                    "commitment",
                    descriptor.map(|d| d.commitment.to_string()).as_deref(),
                );
                write_controller(&mut w, "controller", self.network, &body.controller);
            }
            EntryBody::RemoveController(body) => {
                w.line("controller_id", &body.controller_id.to_string());
                w.line("forced", format_bool(body.forced));
            }
            EntryBody::SetCapabilities(body) => {
                w.line("controller_id", &body.controller_id.to_string());
                w.line("capabilities", &body.capabilities.to_string());
                if body.capabilities.is_empty() {
                    w.fail(CanonErrorKind::InvalidValue(ValueKind::Capabilities));
                }
                w.line("forced", format_bool(body.forced));
            }
            EntryBody::AcceptPolicy(body) => {
                w.line("policy_hash", &body.policy_hash.to_string());
                w.line("policy_version", &body.policy_version.to_string());
                w.line("signed_by", &body.signed_by.to_string());
                body.proof.write(&mut w, "proof");
            }
        }
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_CONTROL,
            MAX_ENTRY_BYTES,
            Self::read,
            Self::encode,
        )
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let agent_key = r.value("agent_key", |s| {
            parse_hex_array::<32>(s).ok_or(ValueError(ValueKind::Hex32))
        })?;
        let seq = r.value("seq", parse_u64)?;
        let prev = r.value("prev", Hash32::parse)?;
        let created_at = r.value("created_at", UtcTime::parse)?;
        let kind = r.value("kind", EntryKind::parse)?;
        let effective_at = r.value("effective_at", UtcTime::parse)?;
        let body = match kind {
            EntryKind::Link => EntryBody::Link(read_link(r, network)?),
            EntryKind::AddController => {
                let owner_account = r.value("owner_account", OwnerAccount::parse)?;
                let nonce = r.opt_value("nonce", Nonce16::parse)?;
                let expires_at = r.opt_value("expires_at", UtcTime::parse)?;
                let commitment = r.opt_value("commitment", Commitment::parse)?;
                let descriptor = match (nonce, expires_at, commitment) {
                    (Some(nonce), Some(expires_at), Some(commitment)) => Some(DescriptorRef {
                        nonce,
                        expires_at,
                        commitment,
                    }),
                    (None, None, None) => None,
                    _ => return Err(r.err(CanonErrorKind::PartialDescriptor)),
                };
                let controller = read_controller(r, "controller", network, None)?;
                EntryBody::AddController(AddControllerBody {
                    owner_account,
                    descriptor,
                    controller,
                })
            }
            EntryKind::RemoveController => {
                let controller_id = r.value("controller_id", ControllerId::parse)?;
                let forced = r.value("forced", parse_bool)?;
                EntryBody::RemoveController(RemoveControllerBody {
                    controller_id,
                    forced,
                })
            }
            EntryKind::SetCapabilities => {
                let controller_id = r.value("controller_id", ControllerId::parse)?;
                let capabilities = r.value("capabilities", CapabilitySet::parse)?;
                let forced = r.value("forced", parse_bool)?;
                EntryBody::SetCapabilities(SetCapabilitiesBody {
                    controller_id,
                    capabilities,
                    forced,
                })
            }
            EntryKind::AcceptPolicy => {
                let policy_hash = r.value("policy_hash", Hash32::parse)?;
                let policy_version = r.value("policy_version", parse_u64)?;
                let signed_by = r.value("signed_by", ControllerId::parse)?;
                let proof = ControllerProof::read(r, "proof")?;
                EntryBody::AcceptPolicy(AcceptPolicyBody {
                    policy_hash,
                    policy_version,
                    signed_by,
                    proof,
                })
            }
        };
        Ok(Self {
            network,
            agent_party,
            agent_key,
            seq,
            prev,
            created_at,
            effective_at,
            body,
        })
    }
}

fn write_link(w: &mut Writer, network: Network, body: &LinkBody) {
    w.line("owner_account", &body.owner_account.to_string());
    w.line("invite", format_bool(body.invite));
    w.line("nonce", &body.nonce.to_string());
    w.line("expires_at", &body.expires_at.to_string());
    w.line("commitment", &body.commitment.to_string());
    w.count("controllers", body.controllers.len());
    if !(1..=MAX_LINK_CONTROLLERS).contains(&body.controllers.len()) {
        w.fail(CanonErrorKind::CountOutOfRange);
    }
    let mut previous: Option<&ControllerId> = None;
    let mut held = CapabilitySet::EMPTY;
    for (i, controller) in body.controllers.iter().enumerate() {
        write_controller(w, &format!("controllers.{i}"), network, controller);
        if let Some(kind) = check_ascending(previous, controller.id()) {
            w.fail(kind);
        }
        previous = Some(controller.id());
        held = held.union(controller.capabilities());
    }
    if held != CapabilitySet::ALL {
        w.fail(CanonErrorKind::CapabilityNotCovered);
    }
}

fn read_link(r: &mut Reader<'_>, network: Network) -> Result<LinkBody, CanonError> {
    let owner_account = r.value("owner_account", OwnerAccount::parse)?;
    let invite = r.value("invite", parse_bool)?;
    let nonce = r.value("nonce", Nonce16::parse)?;
    let expires_at = r.value("expires_at", UtcTime::parse)?;
    let commitment = r.value("commitment", Commitment::parse)?;
    let count = r.count("controllers", 1, MAX_LINK_CONTROLLERS)?;
    let mut controllers: Vec<ControllerDescriptor> = Vec::with_capacity(count);
    let mut held = CapabilitySet::EMPTY;
    for i in 0..count {
        let previous = controllers.last().map(ControllerDescriptor::id);
        let controller = read_controller(r, &format!("controllers.{i}"), network, previous)?;
        held = held.union(controller.capabilities());
        controllers.push(controller);
    }
    if held != CapabilitySet::ALL {
        return Err(r.err(CanonErrorKind::CapabilityNotCovered));
    }
    Ok(LinkBody {
        owner_account,
        invite,
        nonce,
        expires_at,
        commitment,
        controllers,
    })
}

/// Entry bytes with the agent's 64-byte signature beside them, decoded once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedEntry {
    entry: LogEntry,
    bytes: Vec<u8>,
    signature: [u8; 64],
    hash: Hash32,
}

impl SignedEntry {
    /// Decodes the bytes strictly; the signature is checked by log verification.
    pub fn new(bytes: Vec<u8>, signature: [u8; 64]) -> Result<Self, CanonError> {
        let entry = LogEntry::decode(&bytes)?;
        Ok(Self::from_parts(entry, bytes, signature))
    }

    pub(crate) fn from_parts(entry: LogEntry, bytes: Vec<u8>, signature: [u8; 64]) -> Self {
        let hash = Hash32(sha256(&bytes));
        Self {
            entry,
            bytes,
            signature,
            hash,
        }
    }

    pub fn entry(&self) -> &LogEntry {
        &self.entry
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }

    /// SHA-256 of the entry bytes; the next entry's `prev`.
    pub fn hash(&self) -> &Hash32 {
        &self.hash
    }

    pub fn seq(&self) -> u64 {
        self.entry.seq
    }

    pub fn kind(&self) -> EntryKind {
        self.entry.kind()
    }

    pub fn prev(&self) -> &Hash32 {
        &self.entry.prev
    }

    pub fn created_at(&self) -> UtcTime {
        self.entry.created_at
    }

    pub fn effective_at(&self) -> UtcTime {
        self.entry.effective_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Capability;
    use crate::testkit;

    fn header(seq: u64, body: EntryBody) -> LogEntry {
        LogEntry {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            agent_key: testkit::agent_key().verifying_key().to_bytes(),
            seq,
            prev: Hash32([0; 32]),
            created_at: testkit::t("2026-11-22T12:00:00Z"),
            effective_at: testkit::t("2026-11-23T12:00:00Z"),
            body,
        }
    }

    fn link_entry() -> LogEntry {
        let descriptor = testkit::invite_descriptor();
        header(
            1,
            EntryBody::Link(LinkBody {
                owner_account: descriptor.owner_account(),
                invite: true,
                nonce: *descriptor.nonce(),
                expires_at: descriptor.expires_at(),
                commitment: descriptor.commitment().unwrap(),
                controllers: descriptor.controllers().to_vec(),
            }),
        )
    }

    fn samples() -> Vec<LogEntry> {
        let reader = testkit::ed25519_controller_from(0x81, CapabilitySet::of(&[Capability::Read]));
        let id = *reader.id();
        vec![
            link_entry(),
            header(
                2,
                EntryBody::AddController(AddControllerBody {
                    owner_account: OwnerAccount::new(42).unwrap(),
                    descriptor: Some(DescriptorRef {
                        nonce: Nonce16([3; 16]),
                        expires_at: testkit::t("2026-11-23T00:00:00Z"),
                        commitment: Commitment([4; 16]),
                    }),
                    controller: reader.clone(),
                }),
            ),
            header(
                3,
                EntryBody::AddController(AddControllerBody {
                    owner_account: OwnerAccount::new(42).unwrap(),
                    descriptor: None,
                    controller: reader,
                }),
            ),
            header(
                4,
                EntryBody::RemoveController(RemoveControllerBody {
                    controller_id: id,
                    forced: true,
                }),
            ),
            header(
                5,
                EntryBody::SetCapabilities(SetCapabilitiesBody {
                    controller_id: id,
                    capabilities: CapabilitySet::ALL,
                    forced: false,
                }),
            ),
            header(
                6,
                EntryBody::AcceptPolicy(AcceptPolicyBody {
                    policy_hash: Hash32([7; 32]),
                    policy_version: 3,
                    signed_by: id,
                    proof: ControllerProof::WebAuthn(testkit::assertion_over(b"policy bytes")),
                }),
            ),
        ]
    }

    fn text(entry: &LogEntry) -> String {
        String::from_utf8(entry.encode().unwrap()).unwrap()
    }

    fn refusal(text: &str) -> &'static str {
        LogEntry::decode(text.as_bytes()).unwrap_err().code_name()
    }

    #[test]
    fn every_kind_round_trips() {
        for entry in samples() {
            let bytes = entry.encode().unwrap();
            assert_eq!(
                LogEntry::decode(&bytes).unwrap(),
                entry,
                "{:?}",
                entry.kind()
            );
            assert!(bytes.len() <= MAX_ENTRY_BYTES);
        }
        for kind in EntryKind::ALL {
            assert_eq!(EntryKind::parse(kind.as_str()), Ok(kind));
        }
    }

    #[test]
    fn header_and_bodies_have_the_fixed_field_order() {
        let keys = |entry: &LogEntry| -> Vec<String> {
            text(entry)
                .lines()
                .map(|l| l.split_once('=').unwrap().0.to_owned())
                .filter(|k| {
                    !k.starts_with("controller.")
                        && !k.starts_with("controllers.")
                        && !k.starts_with("proof.")
                })
                .collect()
        };
        let header = [
            "scheme",
            "network",
            "agent_party",
            "agent_key",
            "seq",
            "prev",
            "created_at",
            "kind",
            "effective_at",
        ];
        let entries = samples();
        let bodies: [&[&str]; 6] = [
            &[
                "owner_account",
                "invite",
                "nonce",
                "expires_at",
                "commitment",
            ],
            &["owner_account", "nonce", "expires_at", "commitment"],
            &["owner_account", "nonce", "expires_at", "commitment"],
            &["controller_id", "forced"],
            &["controller_id", "capabilities", "forced"],
            &["policy_hash", "policy_version", "signed_by"],
        ];
        for (entry, body) in entries.iter().zip(bodies) {
            let expected: Vec<String> = header
                .iter()
                .chain(body.iter())
                .map(|s| (*s).to_owned())
                .collect();
            assert_eq!(keys(entry), expected, "{:?}", entry.kind());
        }
        let t = text(&entries[2]);
        assert!(t.contains("\nnonce=\nexpires_at=\ncommitment=\ncontroller.controller_id="));
        let t = text(&entries[0]);
        assert!(t.contains(
            "\nkind=link\neffective_at=2026-11-23T12:00:00Z\nowner_account=42\ninvite=true\n"
        ));
        assert!(t.contains("controllers.count=2\n"));
    }

    #[test]
    fn decoding_refusals_have_stable_names() {
        let link = text(&link_entry());
        let entries = samples();
        let add = text(&entries[1]);
        let set = text(&entries[4]);
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "kind",
                link.replacen("kind=link", "kind=set_owner", 1),
                "invalid_entry_kind",
            ),
            ("seq", link.replacen("seq=1", "seq=01", 1), "invalid_u64"),
            (
                "prev",
                link.replacen("prev=0000", "prev=000", 1),
                "invalid_hex32",
            ),
            (
                "effective_at",
                link.replacen(
                    "effective_at=2026-11-23T12:00:00Z",
                    "effective_at=2026-11-23T12:00:00",
                    1,
                ),
                "invalid_time",
            ),
            (
                "invite",
                link.replacen("invite=true", "invite=yes", 1),
                "invalid_bool",
            ),
            (
                "partial descriptor",
                add.replacen(
                    &format!("commitment={}", Commitment([4; 16])),
                    "commitment=",
                    1,
                ),
                "partial_descriptor",
            ),
            (
                "empty capabilities",
                set.replacen("capabilities=policy,direct,read", "capabilities=", 1),
                "empty_value",
            ),
            (
                "scheme",
                link.replacen("control.v1", "control.v2", 1),
                "wrong_scheme",
            ),
            (
                "missing effective_at",
                link.replacen("effective_at=2026-11-23T12:00:00Z\n", "", 1),
                "unexpected_key",
            ),
        ];
        for (name, t, expected) in cases {
            assert_eq!(refusal(&t), expected, "{name}");
        }
        let uncovered = {
            let mut entry = link_entry();
            if let EntryBody::Link(body) = &mut entry.body {
                body.controllers.truncate(1);
            }
            entry
        };
        assert_eq!(
            uncovered.encode().unwrap_err().code_name(),
            "capability_not_covered"
        );
        let t = link.replacen("controllers.count=2", "controllers.count=1", 1);
        assert_eq!(refusal(&t), "capability_not_covered");
        let t = link.replacen("controllers.count=2", "controllers.count=3", 1);
        assert_eq!(refusal(&t), "missing_line");
        let oversized = {
            let mut entry = entries[5].clone();
            if let EntryBody::AcceptPolicy(AcceptPolicyBody {
                proof: ControllerProof::WebAuthn(a),
                ..
            }) = &mut entry.body
            {
                a.client_data_json = vec![b'x'; 2048];
                a.authenticator_data = vec![0; 1024];
            }
            entry
        };
        assert_eq!(oversized.encode().unwrap_err().code_name(), "too_large");
    }

    #[test]
    fn signed_entry_exposes_the_decoded_header() {
        let entry = link_entry();
        let bytes = entry.encode().unwrap();
        let signed = SignedEntry::new(bytes.clone(), [9; 64]).unwrap();
        assert_eq!(signed.seq(), 1);
        assert_eq!(signed.kind(), EntryKind::Link);
        assert_eq!(signed.hash().0, sha256(&bytes));
        assert_eq!(signed.prev(), &Hash32([0; 32]));
        assert_eq!(signed.created_at(), entry.created_at());
        assert_eq!(signed.effective_at(), entry.effective_at());
        assert_eq!(signed.entry(), &entry);
        assert_eq!(signed.signature(), &[9; 64]);
        let mut tampered = bytes;
        tampered.push(b'x');
        assert!(SignedEntry::new(tampered, [9; 64]).is_err());
    }
}
