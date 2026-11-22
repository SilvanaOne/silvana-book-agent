//! Link descriptors (`silvana.agent.link.v1`), their commitments and verification.

use crate::canon::time::UtcTime;
use crate::canon::values::{Commitment, Network, Nonce16, Origin, OwnerAccount, RpId, sha256};
use crate::canon::{Reader, Writer, check_ascending, decode_exact};
use crate::controller::{
    CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey, read_controller,
    write_controller,
};
use crate::error::{CanonError, CanonErrorKind, LinkError, ValueError, ValueKind};
use crate::party::PartyId;
use crate::{MAX_LINK_BYTES, SCHEME_LINK};

/// Most controllers in one `link` descriptor.
pub const MAX_LINK_CONTROLLERS: usize = 6;

/// What the descriptor asks the agent to sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkPurpose {
    Link,
    AddController,
}

impl LinkPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::AddController => "add_controller",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "link" => Ok(Self::Link),
            "add_controller" => Ok(Self::AddController),
            _ => Err(ValueError(ValueKind::Purpose)),
        }
    }

    fn controller_range(self) -> (usize, usize) {
        match self {
            Self::Link => (1, MAX_LINK_CONTROLLERS),
            Self::AddController => (1, 1),
        }
    }
}

/// The controller set a page commits to; `agent_party` is `None` only for an invite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkDescriptor {
    network: Network,
    purpose: LinkPurpose,
    owner_account: OwnerAccount,
    agent_party: Option<PartyId>,
    nonce: Nonce16,
    expires_at: UtcTime,
    controllers: Vec<ControllerDescriptor>,
}

impl LinkDescriptor {
    /// Sorts the controllers by id, then applies every encoding rule.
    pub fn new(
        network: Network,
        purpose: LinkPurpose,
        owner_account: OwnerAccount,
        agent_party: Option<PartyId>,
        nonce: Nonce16,
        expires_at: UtcTime,
        mut controllers: Vec<ControllerDescriptor>,
    ) -> Result<Self, CanonError> {
        controllers.sort_by(|a, b| a.id().cmp(b.id()));
        let descriptor = Self {
            network,
            purpose,
            owner_account,
            agent_party,
            nonce,
            expires_at,
            controllers,
        };
        descriptor.encode()?;
        Ok(descriptor)
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_LINK, MAX_LINK_BYTES);
        w.line("network", self.network.as_str());
        w.line("purpose", self.purpose.as_str());
        w.line("owner_account", &self.owner_account.to_string());
        w.opt(
            "agent_party",
            self.agent_party.as_ref().map(PartyId::as_str),
        );
        if self.purpose == LinkPurpose::AddController && self.agent_party.is_none() {
            w.fail(CanonErrorKind::PartyRequired);
        }
        w.line("nonce", &self.nonce.to_string());
        w.line("expires_at", &self.expires_at.to_string());
        w.count("controllers", self.controllers.len());
        let (min, max) = self.purpose.controller_range();
        if !(min..=max).contains(&self.controllers.len()) {
            w.fail(CanonErrorKind::CountOutOfRange);
        }
        let mut previous: Option<&ControllerId> = None;
        let mut held = CapabilitySet::EMPTY;
        for (i, controller) in self.controllers.iter().enumerate() {
            write_controller(
                &mut w,
                &format!("controllers.{i}"),
                self.network,
                controller,
            );
            if let Some(kind) = check_ascending(previous, controller.id()) {
                w.fail(kind);
            }
            previous = Some(controller.id());
            held = held.union(controller.capabilities());
        }
        if self.purpose == LinkPurpose::Link && held != CapabilitySet::ALL {
            w.fail(CanonErrorKind::CapabilityNotCovered);
        }
        w.finish()
    }

    /// Strict decoding: every rule above, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(bytes, SCHEME_LINK, MAX_LINK_BYTES, Self::read, Self::encode)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let purpose = r.value("purpose", LinkPurpose::parse)?;
        let owner_account = r.value("owner_account", OwnerAccount::parse)?;
        let agent_party = r.opt_value("agent_party", PartyId::parse)?;
        if purpose == LinkPurpose::AddController && agent_party.is_none() {
            return Err(r.err(CanonErrorKind::PartyRequired));
        }
        let nonce = r.value("nonce", Nonce16::parse)?;
        let expires_at = r.value("expires_at", UtcTime::parse)?;
        let (min, max) = purpose.controller_range();
        let count = r.count("controllers", min, max)?;
        let mut controllers: Vec<ControllerDescriptor> = Vec::with_capacity(count);
        let mut held = CapabilitySet::EMPTY;
        for i in 0..count {
            let previous = controllers.last().map(ControllerDescriptor::id);
            let controller = read_controller(r, &format!("controllers.{i}"), network, previous)?;
            held = held.union(controller.capabilities());
            controllers.push(controller);
        }
        if purpose == LinkPurpose::Link && held != CapabilitySet::ALL {
            return Err(r.err(CanonErrorKind::CapabilityNotCovered));
        }
        Ok(Self {
            network,
            purpose,
            owner_account,
            agent_party,
            nonce,
            expires_at,
            controllers,
        })
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn purpose(&self) -> LinkPurpose {
        self.purpose
    }

    pub fn owner_account(&self) -> OwnerAccount {
        self.owner_account
    }

    pub fn agent_party(&self) -> Option<&PartyId> {
        self.agent_party.as_ref()
    }

    pub fn nonce(&self) -> &Nonce16 {
        &self.nonce
    }

    pub fn expires_at(&self) -> UtcTime {
        self.expires_at
    }

    /// Ascending by controller id.
    pub fn controllers(&self) -> &[ControllerDescriptor] {
        &self.controllers
    }

    /// The commitment of the encoded descriptor.
    pub fn commitment(&self) -> Result<Commitment, CanonError> {
        self.encode().map(|bytes| commitment_of(&bytes))
    }
}

/// The first 16 bytes of SHA-256 over the descriptor bytes.
pub fn commitment_of(bytes: &[u8]) -> Commitment {
    let digest = sha256(bytes);
    let mut out = [0u8; 16];
    for (slot, byte) in out.iter_mut().zip(digest.iter()) {
        *slot = *byte;
    }
    Commitment(out)
}

/// The relying-party ids an agent accepts when it links passkeys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpPolicy {
    network: Network,
    rp_ids: Vec<RpId>,
}

impl RpPolicy {
    pub const DEVNET_RP_ID: &'static str = "silvana.link";
    pub const MAINNET_RP_ID: &'static str = "silvana.one";

    /// Devnet: `silvana.link` and `localhost`. Mainnet: `silvana.one`.
    pub fn for_network(network: Network) -> Self {
        let rp_ids = match network {
            Network::Devnet => vec![
                RpId::from_static(Self::DEVNET_RP_ID),
                RpId::from_static(RpId::LOCALHOST),
            ],
            Network::Mainnet => vec![RpId::from_static(Self::MAINNET_RP_ID)],
        };
        Self { network, rp_ids }
    }

    /// An explicit list in place of the defaults; `localhost` still counts on devnet only.
    pub fn with_rp_ids(network: Network, rp_ids: Vec<RpId>) -> Self {
        Self { network, rp_ids }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn rp_ids(&self) -> &[RpId] {
        &self.rp_ids
    }

    /// The rp_id is listed, and every origin is that rp_id or a subdomain over https.
    pub fn check(&self, rp_id: &RpId, origins: &[Origin]) -> Result<(), LinkError> {
        let localhost_ok = self.network == Network::Devnet;
        if !self.rp_ids.contains(rp_id) || (rp_id.is_localhost() && !localhost_ok) {
            return Err(LinkError::RpIdNotAllowed);
        }
        for origin in origins {
            let scheme_ok = origin.is_secure() || (localhost_ok && rp_id.is_localhost());
            if !scheme_ok || !origin.matches_rp_id(rp_id) {
                return Err(LinkError::OriginNotAllowed);
            }
        }
        Ok(())
    }
}

/// Checks a fetched descriptor against the commitment from the code, then decodes it.
/// `expected_party: None` accepts any party; an invite needs `allow_invite`.
pub fn verify_link_descriptor(
    bytes: &[u8],
    expected_commitment: &Commitment,
    network: Network,
    expected_party: Option<&PartyId>,
    allow_invite: bool,
    rp_policy: &RpPolicy,
    now: UtcTime,
) -> Result<LinkDescriptor, LinkError> {
    if commitment_of(bytes) != *expected_commitment {
        return Err(LinkError::CommitmentMismatch);
    }
    let descriptor = LinkDescriptor::decode(bytes)?;
    if descriptor.network != network || rp_policy.network() != network {
        return Err(LinkError::WrongNetwork);
    }
    match (descriptor.agent_party.as_ref(), expected_party) {
        (Some(found), Some(expected)) if found != expected => {
            return Err(LinkError::PartyMismatch);
        }
        (None, Some(_)) if !allow_invite => return Err(LinkError::InviteNotAllowed),
        _ => {}
    }
    if now > descriptor.expires_at {
        return Err(LinkError::Expired);
    }
    for controller in &descriptor.controllers {
        if let ControllerKey::WebAuthn(passkey) = controller.key() {
            rp_policy.check(passkey.rp_id(), passkey.origins())?;
        }
    }
    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{BackupPolicy, Capability, WebAuthnKey};
    use crate::testkit;

    fn now() -> UtcTime {
        UtcTime::parse("2026-11-22T12:00:00Z").unwrap()
    }

    fn invite() -> LinkDescriptor {
        testkit::invite_descriptor()
    }

    fn add_one(party: PartyId) -> LinkDescriptor {
        let controller = testkit::ed25519_controller(CapabilitySet::of(&[Capability::Read]));
        LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::AddController,
            OwnerAccount::new(42).unwrap(),
            Some(party),
            Nonce16([9; 16]),
            UtcTime::parse("2026-11-23T12:00:00Z").unwrap(),
            vec![controller],
        )
        .unwrap()
    }

    fn text(d: &LinkDescriptor) -> String {
        String::from_utf8(d.encode().unwrap()).unwrap()
    }

    fn refusal(text: &str) -> &'static str {
        LinkDescriptor::decode(text.as_bytes())
            .unwrap_err()
            .code_name()
    }

    #[test]
    fn encodes_in_field_order_and_round_trips() {
        let d = invite();
        let t = text(&d);
        let keys: Vec<&str> = t.lines().map(|l| l.split_once('=').unwrap().0).collect();
        assert_eq!(
            &keys[..8],
            [
                "scheme",
                "network",
                "purpose",
                "owner_account",
                "agent_party",
                "nonce",
                "expires_at",
                "controllers.count"
            ]
        );
        assert!(t.starts_with("scheme=silvana.agent.link.v1\nnetwork=devnet\npurpose=link\nowner_account=42\nagent_party=\n"));
        assert_eq!(LinkDescriptor::decode(t.as_bytes()).unwrap(), d);
        let ids: Vec<&ControllerId> = d
            .controllers()
            .iter()
            .map(ControllerDescriptor::id)
            .collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));

        let party = testkit::agent_party();
        let added = add_one(party.clone());
        let t = text(&added);
        assert!(t.contains(&format!("agent_party={party}\n")));
        assert!(t.contains("purpose=add_controller\n"));
        assert_eq!(LinkDescriptor::decode(t.as_bytes()).unwrap(), added);
    }

    #[test]
    fn webauthn_controller_block_has_the_fixed_field_order() {
        let t = text(&invite());
        let passkey_prefix = t
            .lines()
            .find(|l| l.ends_with(".type=webauthn"))
            .map(|l| l.trim_end_matches(".type=webauthn").to_owned())
            .unwrap();
        let fields: Vec<String> = t
            .lines()
            .filter_map(|l| l.strip_prefix(&format!("{passkey_prefix}.")))
            .map(|l| l.split_once('=').unwrap().0.to_owned())
            .collect();
        assert_eq!(
            fields,
            [
                "controller_id",
                "type",
                "label",
                "capabilities",
                "public_key",
                "credential_id",
                "rp_id",
                "origins.count",
                "origins.0",
                "require_uv",
                "backup_policy"
            ]
        );
    }

    #[test]
    fn commitment_is_the_truncated_hash() {
        let bytes = invite().encode().unwrap();
        let digest = sha256(&bytes);
        assert_eq!(commitment_of(&bytes).0, digest[..16]);
        assert_eq!(invite().commitment().unwrap(), commitment_of(&bytes));
    }

    #[test]
    fn construction_applies_the_purpose_rules() {
        let read_only = testkit::ed25519_controller(CapabilitySet::of(&[Capability::Read]));
        let make = |purpose, party: Option<PartyId>, controllers: Vec<ControllerDescriptor>| {
            LinkDescriptor::new(
                Network::Devnet,
                purpose,
                OwnerAccount::new(1).unwrap(),
                party,
                Nonce16([1; 16]),
                now(),
                controllers,
            )
            .unwrap_err()
            .kind
        };
        assert_eq!(
            make(LinkPurpose::Link, None, vec![read_only.clone()]),
            CanonErrorKind::CapabilityNotCovered
        );
        assert_eq!(
            make(LinkPurpose::Link, None, vec![]),
            CanonErrorKind::CountOutOfRange
        );
        assert_eq!(
            make(LinkPurpose::AddController, None, vec![read_only.clone()]),
            CanonErrorKind::PartyRequired
        );
        let two = invite().controllers().to_vec();
        assert_eq!(
            make(
                LinkPurpose::AddController,
                Some(testkit::agent_party()),
                two
            ),
            CanonErrorKind::CountOutOfRange
        );
        let dup = vec![
            read_only.clone(),
            read_only.clone(),
            testkit::passkey_controller(CapabilitySet::ALL),
        ];
        assert_eq!(
            make(LinkPurpose::Link, None, dup),
            CanonErrorKind::DuplicateItem
        );
        let seven: Vec<ControllerDescriptor> = (0x21u8..0x28)
            .map(|s| testkit::ed25519_controller_from(s, CapabilitySet::ALL))
            .collect();
        assert_eq!(
            make(LinkPurpose::Link, None, seven),
            CanonErrorKind::CountOutOfRange
        );
        let six: Vec<ControllerDescriptor> = (0x21u8..0x27)
            .map(|s| testkit::ed25519_controller_from(s, CapabilitySet::ALL))
            .collect();
        assert!(
            LinkDescriptor::new(
                Network::Devnet,
                LinkPurpose::Link,
                OwnerAccount::new(1).unwrap(),
                None,
                Nonce16([1; 16]),
                now(),
                six
            )
            .is_ok()
        );
    }

    fn with_line(t: &str, prefix: &str, line: &str) -> String {
        let lines: Vec<String> = t
            .lines()
            .map(|l| {
                if l.starts_with(prefix) {
                    line.to_owned()
                } else {
                    l.to_owned()
                }
            })
            .collect();
        lines.join("\n") + "\n"
    }

    /// The text with blocks `first` and `second` renumbered as controllers 0 and 1.
    fn with_blocks(t: &str, first: usize, second: usize) -> String {
        let header: Vec<&str> = t
            .lines()
            .take_while(|l| !l.starts_with("controllers.0."))
            .collect();
        let block = |from: usize, to: usize| -> Vec<String> {
            t.lines()
                .filter(|l| l.starts_with(&format!("controllers.{from}.")))
                .map(|l| {
                    l.replacen(
                        &format!("controllers.{from}."),
                        &format!("controllers.{to}."),
                        1,
                    )
                })
                .collect()
        };
        let mut lines: Vec<String> = header.iter().map(|l| (*l).to_owned()).collect();
        lines.extend(block(first, 0));
        lines.extend(block(second, 1));
        lines.join("\n") + "\n"
    }

    #[test]
    fn decoding_refuses_structural_and_semantic_faults() {
        let t = text(&invite());
        let first_id_line = t
            .lines()
            .find(|l| l.starts_with("controllers.0.controller_id="))
            .unwrap();
        let first_id = first_id_line.split_once('=').unwrap().1;
        let mut tampered_id = first_id.to_owned();
        tampered_id.replace_range(0..1, if first_id.starts_with('0') { "1" } else { "0" });
        let spki_line = t.lines().find(|l| l.contains(".public_key=3059")).unwrap();
        let spki_hex = spki_line.split_once('=').unwrap().1;
        let other_spki = hex::encode(testkit::p256_spki(
            &::p256::ecdsa::SigningKey::from_slice(&testkit::seed(0x61)).unwrap(),
        ));
        let rp_line = t.lines().find(|l| l.contains(".rp_id=")).unwrap();
        let prefix = rp_line.split_once(".rp_id=").unwrap().0.to_owned();
        let cases: Vec<(&str, String, &str)> = vec![
            ("crlf", t.replace('\n', "\r\n"), "invalid_byte"),
            (
                "swapped header",
                t.replacen(
                    "network=devnet\npurpose=link\n",
                    "purpose=link\nnetwork=devnet\n",
                    1,
                ),
                "unexpected_key",
            ),
            (
                "unknown network",
                t.replacen("network=devnet", "network=testnet", 1),
                "invalid_network",
            ),
            (
                "unknown purpose",
                t.replacen("purpose=link", "purpose=set_owner", 1),
                "invalid_purpose",
            ),
            (
                "owner zero",
                t.replacen("owner_account=42", "owner_account=0", 1),
                "invalid_owner_account",
            ),
            (
                "owner leading zero",
                t.replacen("owner_account=42", "owner_account=042", 1),
                "invalid_owner_account",
            ),
            (
                "bad party",
                t.replacen("agent_party=\n", "agent_party=alice\n", 1),
                "invalid_party",
            ),
            (
                "short nonce",
                with_line(&t, "nonce=", &format!("nonce={}", "a".repeat(30))),
                "invalid_hex16",
            ),
            (
                "empty nonce",
                with_line(&t, "nonce=", "nonce="),
                "empty_value",
            ),
            (
                "bad expiry",
                with_line(&t, "expires_at=", "expires_at=2026-11-29T12:00:00.0Z"),
                "invalid_time",
            ),
            (
                "expiry offset",
                with_line(&t, "expires_at=", "expires_at=2026-11-29T12:00:00+00:00"),
                "invalid_time",
            ),
            (
                "count leading zero",
                t.replacen("controllers.count=2", "controllers.count=02", 1),
                "invalid_u64",
            ),
            (
                "count over",
                t.replacen("controllers.count=2", "controllers.count=7", 1),
                "count_out_of_range",
            ),
            (
                "count under",
                t.replacen("controllers.count=2", "controllers.count=0", 1),
                "count_out_of_range",
            ),
            (
                "count mismatch",
                t.replacen("controllers.count=2", "controllers.count=1", 1),
                "capability_not_covered",
            ),
            ("unsorted", with_blocks(&t, 1, 0), "unsorted_list"),
            ("duplicate", with_blocks(&t, 0, 0), "duplicate_item"),
            (
                "tampered id",
                t.replacen(first_id, &tampered_id, 1),
                "controller_id_mismatch",
            ),
            (
                "swapped spki",
                t.replacen(spki_hex, &other_spki, 1),
                "controller_id_mismatch",
            ),
            (
                "uppercase spki",
                t.replacen(spki_hex, &spki_hex.to_uppercase(), 1),
                "invalid_spki",
            ),
            (
                "bad label",
                t.replacen(".label=", ".label= ", 1),
                "invalid_label",
            ),
            (
                "bad capabilities",
                t.replacen(
                    ".capabilities=policy,direct\n",
                    ".capabilities=direct,policy\n",
                    1,
                ),
                "invalid_capabilities",
            ),
            (
                "empty capabilities",
                t.replacen(".capabilities=read\n", ".capabilities=\n", 1),
                "empty_value",
            ),
            (
                "require_uv false",
                t.replacen("require_uv=true", "require_uv=false", 1),
                "invalid_require_uv",
            ),
            (
                "backup policy",
                t.replacen("backup_policy=any", "backup_policy=synced", 1),
                "invalid_backup_policy",
            ),
            (
                "origin outside rp",
                t.replacen("=https://app.silvana.link", "=https://app.silvana.one", 1),
                "origin_outside_rp_id",
            ),
            (
                "origin slash",
                t.replacen("=https://app.silvana.link", "=https://app.silvana.link/", 1),
                "invalid_origin",
            ),
            (
                "origin count",
                t.replacen(
                    &format!("{prefix}.origins.count=1"),
                    &format!("{prefix}.origins.count=5"),
                    1,
                ),
                "count_out_of_range",
            ),
            (
                "credential padding",
                t.replacen(".credential_id=", ".credential_id==", 1),
                "invalid_credential_id",
            ),
            (
                "type unknown",
                t.replacen(".type=webauthn", ".type=passkey", 1),
                "invalid_controller_type",
            ),
            ("trailing line", format!("{t}extra=1\n"), "trailing_line"),
            (
                "missing final newline",
                t.trim_end().to_owned(),
                "missing_final_newline",
            ),
            (
                "non-ascii label",
                t.replacen(".label=", ".label=\u{e9}", 1),
                "invalid_byte",
            ),
            (
                "wrong scheme",
                t.replacen("link.v1", "link.v2", 1),
                "wrong_scheme",
            ),
        ];
        for (name, text, expected) in &cases {
            assert_eq!(refusal(text), *expected, "{name}");
        }
    }

    #[test]
    fn decoding_refuses_network_and_party_coupling_faults() {
        let local = testkit::localhost_controller();
        let d = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(42).unwrap(),
            None,
            Nonce16([3; 16]),
            now(),
            vec![local.clone()],
        )
        .unwrap();
        let t = text(&d);
        assert_eq!(
            refusal(&t.replacen("network=devnet", "network=mainnet", 1)),
            "not_allowed_on_network"
        );
        assert_eq!(
            LinkDescriptor::new(
                Network::Mainnet,
                LinkPurpose::Link,
                OwnerAccount::new(42).unwrap(),
                None,
                Nonce16([3; 16]),
                now(),
                vec![local]
            )
            .unwrap_err()
            .kind,
            CanonErrorKind::NotAllowedOnNetwork
        );
        let added = text(&add_one(testkit::agent_party()));
        let party_line = added
            .lines()
            .find(|l| l.starts_with("agent_party="))
            .unwrap();
        assert_eq!(
            refusal(&added.replacen(party_line, "agent_party=", 1)),
            "party_required"
        );
        let weak = added.replacen(
            &hex::encode(testkit::ed_key(0x21).verifying_key().to_bytes()),
            "0100000000000000000000000000000000000000000000000000000000000000",
            1,
        );
        assert_eq!(refusal(&weak), "weak_public_key");
    }

    #[test]
    fn oversized_descriptors_are_refused() {
        let spki = testkit::p256_spki(&testkit::p256_key());
        let rp = RpId::parse(testkit::RP_ID).unwrap();
        let big = |cred: u8, caps| {
            let key = WebAuthnKey::new(
                &spki,
                vec![cred; 1023],
                rp.clone(),
                vec![Origin::parse(testkit::ORIGIN).unwrap()],
                BackupPolicy::Any,
            )
            .unwrap();
            ControllerDescriptor::new(
                ControllerKey::WebAuthn(key),
                crate::Label::parse("Big").unwrap(),
                caps,
            )
            .unwrap()
        };
        let one = big(1, CapabilitySet::ALL);
        assert!(
            LinkDescriptor::new(
                Network::Devnet,
                LinkPurpose::Link,
                OwnerAccount::new(1).unwrap(),
                None,
                Nonce16([1; 16]),
                now(),
                vec![one.clone()]
            )
            .is_ok()
        );
        let other = testkit::ed25519_controller(CapabilitySet::ALL);
        let mut controllers = vec![one, other];
        for seed in 0x22u8..0x24 {
            controllers.push(testkit::ed25519_controller_from(seed, CapabilitySet::ALL));
        }
        let second = ControllerDescriptor::new(
            ControllerKey::WebAuthn(
                WebAuthnKey::new(
                    &testkit::p256_spki(
                        &::p256::ecdsa::SigningKey::from_slice(&testkit::seed(0x61)).unwrap(),
                    ),
                    vec![2; 1023],
                    rp.clone(),
                    vec![Origin::parse(testkit::ORIGIN).unwrap()],
                    BackupPolicy::Any,
                )
                .unwrap(),
            ),
            crate::Label::parse("Big two").unwrap(),
            CapabilitySet::ALL,
        )
        .unwrap();
        controllers.push(second);
        let err = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(1).unwrap(),
            None,
            Nonce16([1; 16]),
            now(),
            controllers,
        )
        .unwrap_err();
        assert_eq!(err.kind, CanonErrorKind::TooLarge);
    }

    fn verify(
        bytes: &[u8],
        party: Option<&PartyId>,
        allow_invite: bool,
        policy: &RpPolicy,
        at: UtcTime,
    ) -> Result<LinkDescriptor, LinkError> {
        verify_link_descriptor(
            bytes,
            &commitment_of(bytes),
            Network::Devnet,
            party,
            allow_invite,
            policy,
            at,
        )
    }

    #[test]
    fn verification_accepts_a_matching_descriptor() {
        let bytes = invite().encode().unwrap();
        let policy = RpPolicy::for_network(Network::Devnet);
        let party = testkit::agent_party();
        assert_eq!(
            verify(&bytes, None, false, &policy, now()).unwrap(),
            invite()
        );
        assert!(verify(&bytes, Some(&party), true, &policy, now()).is_ok());
        let expiry = invite().expires_at();
        assert!(verify(&bytes, None, false, &policy, expiry).is_ok());
        let added = add_one(party.clone()).encode().unwrap();
        assert!(verify(&added, Some(&party), false, &policy, now()).is_ok());
        let local = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(42).unwrap(),
            None,
            Nonce16([3; 16]),
            now(),
            vec![testkit::localhost_controller()],
        )
        .unwrap();
        assert!(verify(&local.encode().unwrap(), None, false, &policy, now()).is_ok());
    }

    #[test]
    fn verification_refusals_each_have_their_own_error() {
        let bytes = invite().encode().unwrap();
        let policy = RpPolicy::for_network(Network::Devnet);
        let party = testkit::agent_party();

        let swapped = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(42).unwrap(),
            None,
            *invite().nonce(),
            invite().expires_at(),
            vec![
                testkit::passkey_controller_from(0x61, CapabilitySet::ALL),
                testkit::ed25519_controller(CapabilitySet::of(&[Capability::Read])),
            ],
        )
        .unwrap()
        .encode()
        .unwrap();
        let expected = commitment_of(&bytes);
        let err = verify_link_descriptor(
            &swapped,
            &expected,
            Network::Devnet,
            None,
            false,
            &policy,
            now(),
        );
        assert_eq!(err.unwrap_err(), LinkError::CommitmentMismatch);

        let err = verify_link_descriptor(
            &bytes,
            &expected,
            Network::Mainnet,
            None,
            false,
            &RpPolicy::for_network(Network::Mainnet),
            now(),
        );
        assert_eq!(err.unwrap_err(), LinkError::WrongNetwork);
        let err = verify_link_descriptor(
            &bytes,
            &expected,
            Network::Devnet,
            None,
            false,
            &RpPolicy::for_network(Network::Mainnet),
            now(),
        );
        assert_eq!(err.unwrap_err(), LinkError::WrongNetwork);

        let other_party = PartyId::parse(&format!("other::{}", party.namespace())).unwrap();
        let added = add_one(party.clone()).encode().unwrap();
        assert_eq!(
            verify(&added, Some(&other_party), true, &policy, now()).unwrap_err(),
            LinkError::PartyMismatch
        );

        assert_eq!(
            verify(&bytes, Some(&party), false, &policy, now()).unwrap_err(),
            LinkError::InviteNotAllowed
        );

        let late = invite().expires_at().checked_add_secs(1).unwrap();
        assert_eq!(
            verify(&bytes, None, false, &policy, late).unwrap_err(),
            LinkError::Expired
        );

        let mainnet_only =
            RpPolicy::with_rp_ids(Network::Devnet, vec![RpId::parse("silvana.one").unwrap()]);
        assert_eq!(
            verify(&bytes, None, false, &mainnet_only, now()).unwrap_err(),
            LinkError::RpIdNotAllowed
        );

        let mut garbage = bytes.clone();
        garbage.push(b'x');
        assert_eq!(
            verify(&garbage, None, false, &policy, now())
                .unwrap_err()
                .code_name(),
            "missing_final_newline"
        );
    }

    #[test]
    fn rp_policy_rules() {
        let devnet = RpPolicy::for_network(Network::Devnet);
        let mainnet = RpPolicy::for_network(Network::Mainnet);
        for id in [
            RpPolicy::DEVNET_RP_ID,
            RpPolicy::MAINNET_RP_ID,
            RpId::LOCALHOST,
        ] {
            assert!(RpId::parse(id).is_ok());
        }
        let rp = |s: &str| RpId::parse(s).unwrap();
        let o = |s: &str| Origin::parse(s).unwrap();
        assert_eq!(
            devnet.check(&rp("silvana.link"), &[o("https://app.silvana.link")]),
            Ok(())
        );
        assert_eq!(
            devnet.check(&rp("localhost"), &[o("http://localhost:3000")]),
            Ok(())
        );
        assert_eq!(
            mainnet.check(&rp("silvana.one"), &[o("https://app.silvana.one")]),
            Ok(())
        );
        assert_eq!(
            mainnet.check(&rp("silvana.link"), &[o("https://app.silvana.link")]),
            Err(LinkError::RpIdNotAllowed)
        );
        assert_eq!(
            devnet.check(&rp("example.com"), &[o("https://example.com")]),
            Err(LinkError::RpIdNotAllowed)
        );
        let local_on_mainnet = RpPolicy::with_rp_ids(Network::Mainnet, vec![rp("localhost")]);
        assert_eq!(
            local_on_mainnet.check(&rp("localhost"), &[o("http://localhost:3000")]),
            Err(LinkError::RpIdNotAllowed)
        );
        assert_eq!(
            devnet.check(&rp("silvana.link"), &[o("https://app.silvana.one")]),
            Err(LinkError::OriginNotAllowed)
        );
        assert_eq!(
            devnet.check(&rp("silvana.link"), &[o("http://localhost:3000")]),
            Err(LinkError::OriginNotAllowed)
        );
        assert_eq!(
            devnet.check(&rp("localhost"), &[o("https://app.silvana.link")]),
            Err(LinkError::OriginNotAllowed)
        );
        let custom = RpPolicy::with_rp_ids(Network::Devnet, vec![rp("example.com")]);
        assert_eq!(
            custom.check(&rp("example.com"), &[o("https://login.example.com:8443")]),
            Ok(())
        );
        assert_eq!(custom.rp_ids().len(), 1);
    }

    #[test]
    fn golden_examples_are_pinned() {
        let d = invite();
        let ids: Vec<String> = d.controllers().iter().map(|c| c.id().to_string()).collect();
        assert_eq!(
            ids,
            [
                "cc576a6f27ad78737510a98cbe096db60f3e2e58ae7c6ecc18fae3335b98ce15",
                "d9e5d4b1f24362d5c8461d7de2a6702b90983add8e13c2a6feb20ada5d7b69bd",
            ]
        );
        let fingerprints: Vec<String> = d
            .controllers()
            .iter()
            .map(|c| c.id().fingerprint())
            .collect();
        assert_eq!(
            fingerprints,
            ["CC57 6A6F 27AD 7873 7510", "D9E5 D4B1 F243 62D5 C846"]
        );
        let commitment = d.commitment().unwrap();
        assert_eq!(commitment.to_string(), "fbc6bc1c6bdd1ec04b08d0fe4e163048");
        assert_eq!(
            crate::format_link_code(&commitment),
            "LNK-7PDLYHDL3UPMASYI2D7E4FRQJA"
        );
        let invite_code = format!("AGT-{}", crate::canon::codes::base32_encode(&[7u8; 16]));
        assert_eq!(
            crate::format_invite_code(&invite_code, &commitment).unwrap(),
            "AGT-A4DQOBYHA4DQOBYHA4DQOBYHA4-7PDLYHDL3UPMASYI2D7E4FRQJA"
        );
        assert_eq!(
            testkit::agent_party().to_string(),
            "agent-1::1220133628fcf469de1c1726313351f2729e6dd6f47b6c1989b76099d363754a39b6"
        );
    }
}
