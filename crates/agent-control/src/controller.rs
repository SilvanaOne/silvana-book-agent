//! Controllers: Ed25519 keys or WebAuthn passkeys holding control capabilities.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::CONTROLLER_ID_DOMAIN;
use crate::canon::values::{
    Hash32, Label, Network, Origin, RpId, decode_b64url_bounded, encode_b64url, parse_hex_array,
};
use crate::canon::{Reader, Writer, check_ascending};
use crate::error::{CanonError, CanonErrorKind, SignatureError, ValueError, ValueKind};
use crate::sig::ed25519::parse_ed25519_public_key;
use crate::sig::p256::{P256_SPKI_LEN, parse_p256_spki};
use crate::sig::webauthn::WebAuthnParams;

/// Most origins one passkey controller may list.
pub const MAX_ORIGINS: usize = 4;
/// Credential id length bounds, in bytes.
pub const MIN_CREDENTIAL_ID: usize = 16;
pub const MAX_CREDENTIAL_ID: usize = 1023;

/// A control capability, in canonical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Policy,
    Direct,
    Read,
}

impl Capability {
    pub const ALL: [Self; 3] = [Self::Policy, Self::Direct, Self::Read];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Direct => "direct",
            Self::Read => "read",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Policy => 1,
            Self::Direct => 2,
            Self::Read => 4,
        }
    }
}

/// A set of capabilities, written as its members in canonical order joined by `,`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CapabilitySet(u8);

impl CapabilitySet {
    pub const EMPTY: Self = Self(0);
    pub const ALL: Self = Self(7);

    pub fn of(capabilities: &[Capability]) -> Self {
        capabilities
            .iter()
            .fold(Self::EMPTY, |set, capability| set.with(*capability))
    }

    pub fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    pub fn with(self, capability: Capability) -> Self {
        Self(self.0 | capability.bit())
    }

    pub fn without(self, capability: Capability) -> Self {
        Self(self.0 & !capability.bit())
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .into_iter()
            .filter(move |capability| self.contains(*capability))
    }

    /// A non-empty set, members strictly in canonical order.
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let err = ValueError(ValueKind::Capabilities);
        let mut set = Self::EMPTY;
        let mut last: Option<Capability> = None;
        for part in s.split(',') {
            let capability = match part {
                "policy" => Capability::Policy,
                "direct" => Capability::Direct,
                "read" => Capability::Read,
                _ => return Err(err),
            };
            if last.is_some_and(|l| l >= capability) {
                return Err(err);
            }
            last = Some(capability);
            set = set.with(capability);
        }
        if set.is_empty() { Err(err) } else { Ok(set) }
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.iter().map(Capability::as_str).collect();
        f.write_str(&names.join(","))
    }
}

/// Whether a passkey may be synced (`any`) or must be device-bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackupPolicy {
    Any,
    DeviceBound,
}

impl BackupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::DeviceBound => "device_bound",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "any" => Ok(Self::Any),
            "device_bound" => Ok(Self::DeviceBound),
            _ => Err(ValueError(ValueKind::BackupPolicy)),
        }
    }
}

/// The kind of key a controller holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControllerType {
    Ed25519,
    WebAuthn,
}

impl ControllerType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ed25519 => "ed25519",
            Self::WebAuthn => "webauthn",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "ed25519" => Ok(Self::Ed25519),
            "webauthn" => Ok(Self::WebAuthn),
            _ => Err(ValueError(ValueKind::ControllerType)),
        }
    }
}

/// SHA-256 over the domain, the type, a newline and the key bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControllerId(pub [u8; 32]);

impl ControllerId {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        Hash32::parse(s).map(|h| Self(h.0))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(self)
    }
}

impl fmt::Display for ControllerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl FromStr for ControllerId {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

/// The id of a controller of `kind` whose key bytes are `key`.
pub fn controller_id_for(kind: ControllerType, key: &[u8]) -> ControllerId {
    let mut hasher = Sha256::new();
    hasher.update(CONTROLLER_ID_DOMAIN.as_bytes());
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b"\n");
    hasher.update(key);
    ControllerId(hasher.finalize().into())
}

/// The first 10 bytes of the id as uppercase hex, in 5 groups of 4.
pub fn fingerprint(id: &ControllerId) -> String {
    let head: Vec<u8> = id.0.iter().take(10).copied().collect();
    let head = hex::encode_upper(head);
    let mut out = String::with_capacity(24);
    for (i, group) in head.as_bytes().chunks(4).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.extend(group.iter().map(|b| char::from(*b)));
    }
    out
}

/// A passkey's SPKI, credential id, relying party, origins and backup policy.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WebAuthnKey {
    spki: [u8; P256_SPKI_LEN],
    credential_id: Vec<u8>,
    rp_id: RpId,
    origins: Vec<Origin>,
    backup_policy: BackupPolicy,
}

impl WebAuthnKey {
    /// Validates every field; origins are sorted, and duplicates are refused.
    pub fn new(
        spki: &[u8],
        credential_id: Vec<u8>,
        rp_id: RpId,
        mut origins: Vec<Origin>,
        backup_policy: BackupPolicy,
    ) -> Result<Self, CanonError> {
        let refuse = |kind| CanonError::new(0, kind);
        let key = parse_p256_spki(spki).map_err(|_| refuse(CanonErrorKind::InvalidPublicKey))?;
        if !(MIN_CREDENTIAL_ID..=MAX_CREDENTIAL_ID).contains(&credential_id.len()) {
            return Err(refuse(CanonErrorKind::InvalidValue(
                ValueKind::CredentialId,
            )));
        }
        if !(1..=MAX_ORIGINS).contains(&origins.len()) {
            return Err(refuse(CanonErrorKind::CountOutOfRange));
        }
        origins.sort();
        let mut previous: Option<&Origin> = None;
        for origin in &origins {
            if let Some(kind) = check_ascending(previous, origin) {
                return Err(refuse(kind));
            }
            if !origin.matches_rp_id(&rp_id) {
                return Err(refuse(CanonErrorKind::OriginOutsideRpId));
            }
            previous = Some(origin);
        }
        Ok(Self {
            spki: *key.spki(),
            credential_id,
            rp_id,
            origins,
            backup_policy,
        })
    }

    pub fn spki(&self) -> &[u8; P256_SPKI_LEN] {
        &self.spki
    }

    pub fn credential_id(&self) -> &[u8] {
        &self.credential_id
    }

    pub fn rp_id(&self) -> &RpId {
        &self.rp_id
    }

    pub fn origins(&self) -> &[Origin] {
        &self.origins
    }

    pub fn backup_policy(&self) -> BackupPolicy {
        self.backup_policy
    }

    pub fn params(&self) -> WebAuthnParams<'_> {
        WebAuthnParams {
            spki: &self.spki,
            rp_id: &self.rp_id,
            origins: &self.origins,
            backup_policy: self.backup_policy,
        }
    }
}

/// A controller's key material.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ControllerKey {
    Ed25519([u8; 32]),
    WebAuthn(WebAuthnKey),
}

impl ControllerKey {
    pub fn controller_type(&self) -> ControllerType {
        match self {
            Self::Ed25519(_) => ControllerType::Ed25519,
            Self::WebAuthn(_) => ControllerType::WebAuthn,
        }
    }

    /// The 32-byte Ed25519 key or the 91-byte SPKI.
    pub fn key_bytes(&self) -> &[u8] {
        match self {
            Self::Ed25519(key) => key,
            Self::WebAuthn(key) => &key.spki,
        }
    }
}

fn key_refusal(error: SignatureError) -> CanonErrorKind {
    match error {
        SignatureError::WeakPublicKey => CanonErrorKind::WeakPublicKey,
        _ => CanonErrorKind::InvalidPublicKey,
    }
}

/// One controller: id, label, capabilities and key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControllerDescriptor {
    id: ControllerId,
    label: Label,
    capabilities: CapabilitySet,
    key: ControllerKey,
}

impl ControllerDescriptor {
    /// Validates the key and derives the id.
    pub fn new(
        key: ControllerKey,
        label: Label,
        capabilities: CapabilitySet,
    ) -> Result<Self, CanonError> {
        if capabilities.is_empty() {
            return Err(CanonError::new(
                0,
                CanonErrorKind::InvalidValue(ValueKind::Capabilities),
            ));
        }
        if let ControllerKey::Ed25519(public_key) = &key {
            parse_ed25519_public_key(public_key).map_err(|e| CanonError::new(0, key_refusal(e)))?;
        }
        let id = controller_id_for(key.controller_type(), key.key_bytes());
        Ok(Self {
            id,
            label,
            capabilities,
            key,
        })
    }

    pub fn id(&self) -> &ControllerId {
        &self.id
    }

    pub fn label(&self) -> &Label {
        &self.label
    }

    pub fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    pub fn key(&self) -> &ControllerKey {
        &self.key
    }

    pub fn controller_type(&self) -> ControllerType {
        self.key.controller_type()
    }

    /// The same controller with other capabilities.
    pub fn with_capabilities(&self, capabilities: CapabilitySet) -> Result<Self, CanonError> {
        Self::new(self.key.clone(), self.label.clone(), capabilities)
    }
}

/// Writes the controller block under `prefix`.
pub(crate) fn write_controller(
    w: &mut Writer,
    prefix: &str,
    network: Network,
    controller: &ControllerDescriptor,
) {
    let key = |field: &str| format!("{prefix}.{field}");
    w.line(&key("controller_id"), &controller.id.to_string());
    w.line(&key("type"), controller.controller_type().as_str());
    w.line(&key("label"), controller.label.as_str());
    w.line(&key("capabilities"), &controller.capabilities.to_string());
    match &controller.key {
        ControllerKey::Ed25519(public_key) => {
            w.line(&key("public_key"), &hex::encode(public_key));
        }
        ControllerKey::WebAuthn(passkey) => {
            w.line(&key("public_key"), &hex::encode(passkey.spki));
            w.line(
                &key("credential_id"),
                &encode_b64url(&passkey.credential_id),
            );
            w.line(&key("rp_id"), passkey.rp_id.as_str());
            if passkey.rp_id.is_localhost() && network != Network::Devnet {
                w.fail(CanonErrorKind::NotAllowedOnNetwork);
            }
            w.count(&key("origins"), passkey.origins.len());
            for (j, origin) in passkey.origins.iter().enumerate() {
                w.line(&key(&format!("origins.{j}")), origin.as_str());
            }
            w.line(&key("require_uv"), "true");
            w.line(&key("backup_policy"), passkey.backup_policy.as_str());
        }
    }
}

/// Reads the controller block under `prefix`; its id must sort after `previous`.
pub(crate) fn read_controller(
    r: &mut Reader<'_>,
    prefix: &str,
    network: Network,
    previous: Option<&ControllerId>,
) -> Result<ControllerDescriptor, CanonError> {
    let key = |field: &str| format!("{prefix}.{field}");
    let id = r.value(&key("controller_id"), ControllerId::parse)?;
    if let Some(kind) = check_ascending(previous, &id) {
        return Err(r.err(kind));
    }
    let kind = r.value(&key("type"), ControllerType::parse)?;
    let label = r.value(&key("label"), Label::parse)?;
    let capabilities = r.value(&key("capabilities"), CapabilitySet::parse)?;
    let controller_key = match kind {
        ControllerType::Ed25519 => {
            let public_key = r.value(&key("public_key"), |s| {
                parse_hex_array::<32>(s).ok_or(ValueError(ValueKind::Hex32))
            })?;
            parse_ed25519_public_key(&public_key).map_err(|e| r.err(key_refusal(e)))?;
            ControllerKey::Ed25519(public_key)
        }
        ControllerType::WebAuthn => {
            let spki = r.value(&key("public_key"), |s| {
                parse_hex_array::<P256_SPKI_LEN>(s).ok_or(ValueError(ValueKind::Spki))
            })?;
            parse_p256_spki(&spki).map_err(|_| r.err(CanonErrorKind::InvalidPublicKey))?;
            let credential_id = r.value(&key("credential_id"), |s| {
                decode_b64url_bounded(s, MIN_CREDENTIAL_ID, MAX_CREDENTIAL_ID)
                    .ok_or(ValueError(ValueKind::CredentialId))
            })?;
            let rp_id = r.value(&key("rp_id"), RpId::parse)?;
            if rp_id.is_localhost() && network != Network::Devnet {
                return Err(r.err(CanonErrorKind::NotAllowedOnNetwork));
            }
            let count = r.count(&key("origins"), 1, MAX_ORIGINS)?;
            let mut origins: Vec<Origin> = Vec::with_capacity(count);
            for j in 0..count {
                let origin = r.value(&key(&format!("origins.{j}")), Origin::parse)?;
                if !origin.matches_rp_id(&rp_id) {
                    return Err(r.err(CanonErrorKind::OriginOutsideRpId));
                }
                if let Some(kind) = check_ascending(origins.last(), &origin) {
                    return Err(r.err(kind));
                }
                origins.push(origin);
            }
            r.value(&key("require_uv"), |s| {
                (s == "true")
                    .then_some(())
                    .ok_or(ValueError(ValueKind::RequireUv))
            })?;
            let backup_policy = r.value(&key("backup_policy"), BackupPolicy::parse)?;
            ControllerKey::WebAuthn(WebAuthnKey {
                spki,
                credential_id,
                rp_id,
                origins,
                backup_policy,
            })
        }
    };
    if controller_id_for(kind, controller_key.key_bytes()) != id {
        return Err(r.err(CanonErrorKind::ControllerIdMismatch));
    }
    Ok(ControllerDescriptor {
        id,
        label,
        capabilities,
        key: controller_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    #[test]
    fn capability_set_grammar() {
        for (text, set) in [
            ("policy,direct,read", CapabilitySet::ALL),
            ("policy", CapabilitySet::of(&[Capability::Policy])),
            (
                "direct,read",
                CapabilitySet::of(&[Capability::Read, Capability::Direct]),
            ),
            ("read", CapabilitySet::of(&[Capability::Read])),
        ] {
            assert_eq!(CapabilitySet::parse(text), Ok(set));
            assert_eq!(set.to_string(), text);
        }
        for bad in [
            "",
            "read,policy",
            "policy,policy",
            "policy,",
            ",policy",
            "POLICY",
            "policy direct",
            "all",
            "policy, read",
        ] {
            assert_eq!(
                CapabilitySet::parse(bad),
                Err(ValueError(ValueKind::Capabilities)),
                "{bad:?}"
            );
        }
        let set = CapabilitySet::ALL.without(Capability::Direct);
        assert!(set.contains(Capability::Policy) && !set.contains(Capability::Direct));
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            vec![Capability::Policy, Capability::Read]
        );
    }

    #[test]
    fn enum_grammars() {
        assert_eq!(BackupPolicy::parse("any"), Ok(BackupPolicy::Any));
        assert_eq!(
            BackupPolicy::parse("device_bound"),
            Ok(BackupPolicy::DeviceBound)
        );
        for bad in ["", "Any", "device-bound", "synced"] {
            assert_eq!(
                BackupPolicy::parse(bad),
                Err(ValueError(ValueKind::BackupPolicy))
            );
        }
        assert_eq!(
            ControllerType::parse("webauthn"),
            Ok(ControllerType::WebAuthn)
        );
        for bad in ["", "Ed25519", "passkey", "p256"] {
            assert_eq!(
                ControllerType::parse(bad),
                Err(ValueError(ValueKind::ControllerType))
            );
        }
    }

    #[test]
    fn ids_are_domain_separated_hashes_of_type_and_key() {
        let ed = testkit::ed_key(0x21).verifying_key().to_bytes();
        let mut preimage = b"silvana.agent.controller-id.v1\ned25519\n".to_vec();
        preimage.extend_from_slice(&ed);
        let expected = ControllerId(Sha256::digest(&preimage).into());
        assert_eq!(controller_id_for(ControllerType::Ed25519, &ed), expected);

        let spki = testkit::p256_spki(&testkit::p256_key());
        let mut preimage = b"silvana.agent.controller-id.v1\nwebauthn\n".to_vec();
        preimage.extend_from_slice(&spki);
        let expected = ControllerId(Sha256::digest(&preimage).into());
        assert_eq!(controller_id_for(ControllerType::WebAuthn, &spki), expected);
        assert_ne!(
            controller_id_for(ControllerType::Ed25519, &spki),
            controller_id_for(ControllerType::WebAuthn, &spki)
        );
    }

    #[test]
    fn fingerprints_are_five_groups_of_four_uppercase_hex() {
        let id = ControllerId([0xab; 32]);
        assert_eq!(fingerprint(&id), "ABAB ABAB ABAB ABAB ABAB");
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(
            ControllerId(bytes).fingerprint(),
            "0001 0203 0405 0607 0809"
        );
        let ed = testkit::ed25519_controller(CapabilitySet::ALL);
        let fp = ed.id().fingerprint();
        assert_eq!(fp.len(), 24);
        assert_eq!(
            fp,
            hex::encode_upper(&ed.id().0[..10])
                .as_bytes()
                .chunks(4)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    #[test]
    fn descriptors_refuse_bad_keys() {
        let label = Label::parse("Key").unwrap();
        let weak = ControllerKey::Ed25519([0u8; 32]);
        assert_eq!(
            ControllerDescriptor::new(weak, label.clone(), CapabilitySet::ALL)
                .unwrap_err()
                .kind,
            CanonErrorKind::WeakPublicKey
        );
        let good = testkit::ed25519_controller(CapabilitySet::ALL);
        assert_eq!(
            ControllerDescriptor::new(good.key().clone(), label, CapabilitySet::EMPTY)
                .unwrap_err()
                .kind,
            CanonErrorKind::InvalidValue(ValueKind::Capabilities)
        );
        assert_eq!(
            good.with_capabilities(CapabilitySet::of(&[Capability::Read]))
                .unwrap()
                .id(),
            good.id()
        );
    }

    #[test]
    fn passkeys_validate_every_field() {
        let spki = testkit::p256_spki(&testkit::p256_key());
        let rp = RpId::parse(testkit::RP_ID).unwrap();
        let origin = |s: &str| Origin::parse(s).unwrap();
        let ok = |origins: Vec<Origin>| {
            WebAuthnKey::new(&spki, vec![7u8; 16], rp.clone(), origins, BackupPolicy::Any)
        };
        let sorted = ok(vec![
            origin("https://b.silvana.link"),
            origin("https://a.silvana.link"),
        ])
        .unwrap();
        assert_eq!(sorted.origins()[0].as_str(), "https://a.silvana.link");
        let kind = |r: Result<WebAuthnKey, CanonError>| r.unwrap_err().kind;
        assert_eq!(kind(ok(vec![])), CanonErrorKind::CountOutOfRange);
        let five: Vec<Origin> = (1..=5)
            .map(|i| origin(&format!("https://a{i}.silvana.link")))
            .collect();
        assert_eq!(kind(ok(five)), CanonErrorKind::CountOutOfRange);
        assert_eq!(
            kind(ok(vec![origin(testkit::ORIGIN), origin(testkit::ORIGIN)])),
            CanonErrorKind::DuplicateItem
        );
        assert_eq!(
            kind(ok(vec![origin("https://silvana.one")])),
            CanonErrorKind::OriginOutsideRpId
        );
        assert_eq!(
            kind(ok(vec![origin("http://localhost:3000")])),
            CanonErrorKind::OriginOutsideRpId
        );
        let short_id = WebAuthnKey::new(
            &spki,
            vec![7u8; 15],
            rp.clone(),
            vec![origin(testkit::ORIGIN)],
            BackupPolicy::Any,
        );
        assert_eq!(
            kind(short_id),
            CanonErrorKind::InvalidValue(ValueKind::CredentialId)
        );
        let long_id = WebAuthnKey::new(
            &spki,
            vec![7u8; 1024],
            rp.clone(),
            vec![origin(testkit::ORIGIN)],
            BackupPolicy::Any,
        );
        assert_eq!(
            kind(long_id),
            CanonErrorKind::InvalidValue(ValueKind::CredentialId)
        );
        let bad_spki = WebAuthnKey::new(
            &spki[..90],
            vec![7u8; 16],
            rp,
            vec![origin(testkit::ORIGIN)],
            BackupPolicy::Any,
        );
        assert_eq!(kind(bad_spki), CanonErrorKind::InvalidPublicKey);
    }
}
