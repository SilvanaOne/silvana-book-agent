//! Read sessions (`silvana.agent.read-session.v1`) and signed reads (`silvana.agent.read-request.v1`).

use std::fmt;

use crate::canon::time::UtcTime;
use crate::canon::values::{Network, Nonce16, Token, parse_hex_array, parse_u64, sha256};
use crate::canon::{Reader, Writer, check_ascending, decode_exact};
use crate::controller::{Capability, ControllerId};
use crate::error::{CanonError, CanonErrorKind, ReadSessionError, ValueError, ValueKind};
use crate::party::PartyId;
use crate::proof::{ProofEnvelope, ProofOutcome, verify_controller_proof};
use crate::sig::p256::{P256_SPKI_LEN, P256PublicKey, parse_p256_spki, verify_p256_p1363};
use crate::state::{ControlState, add_secs};
use crate::{SCHEME_READ_REQUEST, SCHEME_READ_SESSION};

/// Largest read-session request, in bytes.
pub const MAX_READ_SESSION_BYTES: usize = 20_480;
/// Longest session, in seconds.
pub const MAX_READ_SESSION_SECS: u64 = 3600;
/// Most agents one session covers.
pub const MAX_SESSION_AGENTS: usize = 64;
/// How far a session's `issued_at` may run ahead of the verifier's clock.
pub const READ_SESSION_CLOCK_SKEW_SECS: u64 = 120;
/// Largest signed read request, in bytes.
pub const MAX_READ_REQUEST_BYTES: usize = 1024;
/// A read request is valid within this distance of the verifier's clock, either way.
pub const READ_REQUEST_WINDOW_MS: u64 = 60_000;
/// 2020-01-01T00:00:00.000Z and 9999-12-31T23:59:59.999Z in milliseconds.
pub const MIN_UNIX_MS: u64 = 1_577_836_800_000;
pub const MAX_UNIX_MS: u64 = 253_402_300_799_999;

/// The first 16 bytes of SHA-256 over the session request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub [u8; 16]);

impl SessionId {
    pub fn of(request_bytes: &[u8]) -> Self {
        let mut id = [0u8; 16];
        for (out, byte) in id.iter_mut().zip(sha256(request_bytes)) {
            *out = byte;
        }
        Self(id)
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        parse_hex_array::<16>(s)
            .map(Self)
            .ok_or(ValueError(ValueKind::Hex16))
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

/// A `read` controller's grant of a browser session key over some agents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSessionRequest {
    pub network: Network,
    pub controller_id: ControllerId,
    pub session_key: P256PublicKey,
    pub nonce: Nonce16,
    pub issued_at: UtcTime,
    pub expires_at: UtcTime,
    /// Strictly ascending.
    pub agents: Vec<PartyId>,
}

fn session_validity_ok(issued_at: UtcTime, expires_at: UtcTime) -> bool {
    u64::try_from(issued_at.secs_until(expires_at))
        .is_ok_and(|span| (1..=MAX_READ_SESSION_SECS).contains(&span))
}

impl ReadSessionRequest {
    /// Sorts and deduplicates the agents.
    pub fn new(
        network: Network,
        controller_id: ControllerId,
        session_key: P256PublicKey,
        nonce: Nonce16,
        issued_at: UtcTime,
        expires_at: UtcTime,
        mut agents: Vec<PartyId>,
    ) -> Self {
        agents.sort_unstable();
        agents.dedup();
        Self {
            network,
            controller_id,
            session_key,
            nonce,
            issued_at,
            expires_at,
            agents,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_READ_SESSION, MAX_READ_SESSION_BYTES);
        w.line("network", self.network.as_str());
        w.line("controller_id", &self.controller_id.to_string());
        w.line("session_key", &hex::encode(self.session_key.spki()));
        w.line("nonce", &self.nonce.to_string());
        w.line("issued_at", &self.issued_at.to_string());
        w.line("expires_at", &self.expires_at.to_string());
        if !session_validity_ok(self.issued_at, self.expires_at) {
            w.fail(CanonErrorKind::InvalidValidity);
        }
        w.count("agents", self.agents.len());
        if !(1..=MAX_SESSION_AGENTS).contains(&self.agents.len()) {
            w.fail(CanonErrorKind::CountOutOfRange);
        }
        let mut previous: Option<&PartyId> = None;
        for (i, agent) in self.agents.iter().enumerate() {
            w.line(&format!("agents.{i}"), agent.as_str());
            if let Some(kind) = check_ascending(previous, agent) {
                w.fail(kind);
            }
            previous = Some(agent);
        }
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_READ_SESSION,
            MAX_READ_SESSION_BYTES,
            Self::read,
            Self::encode,
        )
    }

    pub fn session_id(&self) -> Result<SessionId, CanonError> {
        self.encode().map(|bytes| SessionId::of(&bytes))
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let controller_id = r.value("controller_id", ControllerId::parse)?;
        let spki = r.value("session_key", |s| {
            parse_hex_array::<P256_SPKI_LEN>(s).ok_or(ValueError(ValueKind::Spki))
        })?;
        let session_key =
            parse_p256_spki(&spki).map_err(|_| r.err(CanonErrorKind::InvalidPublicKey))?;
        let nonce = r.value("nonce", Nonce16::parse)?;
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        let expires_at = r.value("expires_at", UtcTime::parse)?;
        if !session_validity_ok(issued_at, expires_at) {
            return Err(r.err(CanonErrorKind::InvalidValidity));
        }
        let count = r.count("agents", 1, MAX_SESSION_AGENTS)?;
        let mut agents: Vec<PartyId> = Vec::with_capacity(count);
        for i in 0..count {
            let agent = r.value(&format!("agents.{i}"), PartyId::parse)?;
            if let Some(kind) = check_ascending(agents.last(), &agent) {
                return Err(r.err(kind));
            }
            agents.push(agent);
        }
        Ok(Self {
            network,
            controller_id,
            session_key,
            nonce,
            issued_at,
            expires_at,
            agents,
        })
    }
}

/// A session that passed `verify_read_session`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedReadSession {
    pub request: ReadSessionRequest,
    pub bytes: Vec<u8>,
    pub session_id: SessionId,
    /// The proof's outcome against the first listed agent's controller record.
    pub outcome: ProofOutcome,
}

/// Requires the controller to hold `read`, in effect at `now`, in every listed agent's state.
pub fn verify_read_session(
    bytes: &[u8],
    envelope: &ProofEnvelope,
    states: &[&ControlState],
    now: UtcTime,
) -> Result<VerifiedReadSession, ReadSessionError> {
    let request = ReadSessionRequest::decode(bytes).map_err(ReadSessionError::Canon)?;
    let mut signers = Vec::with_capacity(request.agents.len());
    for agent in &request.agents {
        let state = states
            .iter()
            .find(|state| state.agent_party() == agent)
            .ok_or(ReadSessionError::UnknownAgent)?;
        if state.network() != request.network {
            return Err(ReadSessionError::WrongNetwork);
        }
        let signer = state
            .controller(&request.controller_id)
            .ok_or(ReadSessionError::UnknownController)?;
        if !signer.capabilities_at(now).contains(Capability::Read) {
            return Err(ReadSessionError::SignerLacksRead);
        }
        signers.push(signer.descriptor());
    }
    let latest_issue =
        add_secs(now, READ_SESSION_CLOCK_SKEW_SECS).ok_or(ReadSessionError::TimeOverflow)?;
    if request.issued_at > latest_issue {
        return Err(ReadSessionError::IssuedInFuture);
    }
    if now > request.expires_at {
        return Err(ReadSessionError::Expired);
    }
    let mut outcome = None;
    for descriptor in signers {
        let verified = verify_controller_proof(descriptor, bytes, envelope)
            .map_err(ReadSessionError::Proof)?;
        outcome.get_or_insert(verified);
    }
    let outcome = outcome.ok_or(ReadSessionError::UnknownAgent)?;
    Ok(VerifiedReadSession {
        session_id: SessionId::of(bytes),
        request,
        bytes: bytes.to_vec(),
        outcome,
    })
}

/// One read, signed by the session key as P-256 r||s.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReadRequest {
    pub network: Network,
    pub session_id: SessionId,
    pub method: Token,
    pub agent_party: PartyId,
    /// The logical page the browser asked for; 0/0 for holdings and traffic.
    pub range_offset: u64,
    pub range_limit: u64,
    pub issued_at_ms: u64,
    pub nonce: Nonce16,
}

fn ms_in_range(ms: u64) -> bool {
    (MIN_UNIX_MS..=MAX_UNIX_MS).contains(&ms)
}

impl ReadRequest {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_READ_REQUEST, MAX_READ_REQUEST_BYTES);
        w.line("network", self.network.as_str());
        w.line("session_id", &self.session_id.to_string());
        w.line("method", self.method.as_str());
        w.line("agent_party", self.agent_party.as_str());
        w.line("range_offset", &self.range_offset.to_string());
        w.line("range_limit", &self.range_limit.to_string());
        if self.range_offset.checked_add(self.range_limit).is_none() {
            w.fail(CanonErrorKind::InvalidRange);
        }
        w.line("issued_at_ms", &self.issued_at_ms.to_string());
        if !ms_in_range(self.issued_at_ms) {
            w.fail(CanonErrorKind::InvalidValue(ValueKind::Time));
        }
        w.line("nonce", &self.nonce.to_string());
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_READ_REQUEST,
            MAX_READ_REQUEST_BYTES,
            Self::read,
            Self::encode,
        )
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let session_id = r.value("session_id", SessionId::parse)?;
        let method = r.value("method", Token::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let range_offset = r.value("range_offset", parse_u64)?;
        let range_limit = r.value("range_limit", parse_u64)?;
        if range_offset.checked_add(range_limit).is_none() {
            return Err(r.err(CanonErrorKind::InvalidRange));
        }
        let issued_at_ms = r.value("issued_at_ms", parse_u64)?;
        if !ms_in_range(issued_at_ms) {
            return Err(r.err(CanonErrorKind::InvalidValue(ValueKind::Time)));
        }
        let nonce = r.value("nonce", Nonce16::parse)?;
        Ok(Self {
            network,
            session_id,
            method,
            agent_party,
            range_offset,
            range_limit,
            issued_at_ms,
            nonce,
        })
    }
}

/// Checks one signed read against its session and the clock; the nonce is the caller's job.
pub fn verify_read_request(
    session: &VerifiedReadSession,
    bytes: &[u8],
    signature: &[u8],
    now_ms: u64,
) -> Result<ReadRequest, ReadSessionError> {
    let request = ReadRequest::decode(bytes).map_err(ReadSessionError::Canon)?;
    if request.network != session.request.network {
        return Err(ReadSessionError::WrongNetwork);
    }
    if request.session_id != session.session_id {
        return Err(ReadSessionError::SessionMismatch);
    }
    if session
        .request
        .agents
        .binary_search(&request.agent_party)
        .is_err()
    {
        return Err(ReadSessionError::AgentNotInSession);
    }
    if now_ms.abs_diff(request.issued_at_ms) > READ_REQUEST_WINDOW_MS {
        return Err(ReadSessionError::OutsideWindow);
    }
    let now_secs = now_ms
        .checked_div(1000)
        .ok_or(ReadSessionError::TimeOverflow)?;
    let expired = u64::try_from(session.request.expires_at.unix_secs())
        .map_or(true, |expires_at| now_secs > expires_at);
    if expired {
        return Err(ReadSessionError::Expired);
    }
    verify_p256_p1363(&session.request.session_key, bytes, signature)
        .map_err(ReadSessionError::Signature)?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::EntryBuilder;
    use crate::canon::values::OwnerAccount;
    use crate::controller::{CapabilitySet, ControllerDescriptor};
    use crate::link::{LinkDescriptor, LinkPurpose};
    use crate::party::namespace_for_key;
    use crate::testkit::{self, DAY, DEVNET_DELAY, at};
    use ::p256::ecdsa::signature::Signer as _;
    use ::p256::ecdsa::{Signature, SigningKey};

    fn linked_agent(seed: u8, hint: &str, controllers: Vec<ControllerDescriptor>) -> ControlState {
        let public = testkit::ed_key(seed).verifying_key().to_bytes();
        let party = PartyId::parse(&format!("{hint}::{}", namespace_for_key(&public))).unwrap();
        let state = ControlState::empty(Network::Devnet, party.clone(), public);
        let descriptor = LinkDescriptor::new(
            Network::Devnet,
            LinkPurpose::Link,
            OwnerAccount::new(42).unwrap(),
            Some(party),
            Nonce16([seed; 16]),
            at(7 * DAY),
            controllers,
        )
        .unwrap();
        let bytes = descriptor.encode().unwrap();
        let commitment = descriptor.commitment().unwrap();
        EntryBuilder::new(&state)
            .link(&bytes, &commitment, &testkit::rp(), false, at(0))
            .unwrap()
            .state_after()
            .clone()
    }

    struct Fixture {
        states: Vec<ControlState>,
        session_key: SigningKey,
    }

    impl Fixture {
        fn new() -> Self {
            let all = testkit::passkey_controller(CapabilitySet::ALL);
            let no_read = vec![
                testkit::passkey_controller(CapabilitySet::of(&[
                    Capability::Policy,
                    Capability::Direct,
                ])),
                testkit::ed25519_controller(CapabilitySet::of(&[Capability::Read])),
            ];
            Self {
                states: vec![
                    linked_agent(0x01, "agent-1", vec![all.clone()]),
                    linked_agent(0x02, "agent-2", vec![all.clone()]),
                    linked_agent(0x03, "agent-3", vec![all]),
                    linked_agent(0x04, "agent-4", no_read),
                ],
                session_key: testkit::p256_key_from(0x61),
            }
        }

        fn party(&self, i: usize) -> PartyId {
            self.states[i].agent_party().clone()
        }

        fn request(&self, agents: &[usize], issued_at: UtcTime, secs: i64) -> ReadSessionRequest {
            let spki = testkit::p256_spki(&self.session_key);
            ReadSessionRequest::new(
                Network::Devnet,
                testkit::passkey_id(),
                parse_p256_spki(&spki).unwrap(),
                Nonce16([0x77; 16]),
                issued_at,
                issued_at.checked_add_secs(secs).unwrap(),
                agents.iter().map(|i| self.party(*i)).collect(),
            )
        }

        fn states(&self) -> Vec<&ControlState> {
            self.states.iter().collect()
        }

        fn verify(
            &self,
            request: &ReadSessionRequest,
            now: UtcTime,
        ) -> Result<VerifiedReadSession, ReadSessionError> {
            let bytes = request.encode().unwrap();
            verify_read_session(
                &bytes,
                &testkit::passkey_envelope(&bytes),
                &self.states(),
                now,
            )
        }

        fn sign(&self, bytes: &[u8]) -> Vec<u8> {
            let signature: Signature = self.session_key.sign(bytes);
            signature.to_bytes().to_vec()
        }
    }

    #[test]
    fn a_session_verifies_across_three_agents() {
        let f = Fixture::new();
        let now = at(DAY);
        let request = f.request(&[2, 0, 1], now, 3600);
        let verified = f.verify(&request, now).unwrap();
        let bytes = request.encode().unwrap();
        assert_eq!(verified.session_id, SessionId::of(&bytes));
        assert_eq!(verified.session_id, request.session_id().unwrap());
        assert_eq!(
            verified.session_id.0.as_slice(),
            sha256(&bytes).get(..16).unwrap()
        );
        assert_eq!(verified.request.agents.len(), 3);
        assert_eq!(ReadSessionRequest::decode(&bytes).unwrap(), request);
        assert_eq!(verified.outcome.sign_count, Some(0));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("scheme=silvana.agent.read-session.v1\nnetwork=devnet\n"));
        assert!(text.contains("\nagents.count=3\n"));
    }

    #[test]
    fn sessions_are_refused_with_stable_names() {
        let f = Fixture::new();
        let now = at(DAY);
        let name = |request: &ReadSessionRequest, when: UtcTime| {
            f.verify(request, when).unwrap_err().code_name()
        };
        assert_eq!(
            name(&f.request(&[0, 3], now, 600), now),
            "signer_lacks_read"
        );
        let pending = at(DEVNET_DELAY - 1);
        assert_eq!(
            name(&f.request(&[0], pending, 600), pending),
            "signer_lacks_read"
        );
        let mut stranger = f.request(&[0], now, 600);
        stranger.agents =
            vec![PartyId::parse(&format!("nobody::{}", namespace_for_key(&[9; 32]))).unwrap()];
        assert_eq!(name(&stranger, now), "unknown_agent");
        let mut mainnet = f.request(&[0], now, 600);
        mainnet.network = Network::Mainnet;
        assert_eq!(name(&mainnet, now), "wrong_network");
        let mut unknown = f.request(&[0], now, 600);
        unknown.controller_id = testkit::ed25519_id(0x99);
        assert_eq!(name(&unknown, now), "unknown_controller");
        let early = f.request(&[0], now.checked_add_secs(121).unwrap(), 600);
        assert_eq!(name(&early, now), "issued_in_future");
        let skewed = f.request(&[0], now.checked_add_secs(120).unwrap(), 600);
        assert!(f.verify(&skewed, now).is_ok());
        let ending = f.request(&[0], now, 600);
        assert!(
            f.verify(&ending, now.checked_add_secs(600).unwrap())
                .is_ok()
        );
        assert_eq!(name(&ending, now.checked_add_secs(601).unwrap()), "expired");
        let bytes = ending.encode().unwrap();
        let wrong = verify_read_session(&bytes, &testkit::passkey_envelope(b"x"), &f.states(), now);
        assert_eq!(wrong.unwrap_err().code_name(), "payload_mismatch");
        let long = f.request(&[0], now, 3601);
        assert_eq!(long.encode().unwrap_err().code_name(), "invalid_validity");
        let text = String::from_utf8(f.request(&[0, 1], now, 600).encode().unwrap()).unwrap();
        let (first, second) = (f.party(0), f.party(1));
        let (low, high) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "validity",
                text.replacen(
                    "expires_at=2026-11-23T12:10:00Z",
                    "expires_at=2026-11-23T13:00:01Z",
                    1,
                ),
                "invalid_validity",
            ),
            (
                "unsorted",
                text.replacen(
                    &format!("agents.0={low}\nagents.1={high}"),
                    &format!("agents.0={high}\nagents.1={low}"),
                    1,
                ),
                "unsorted_list",
            ),
            (
                "duplicate",
                text.replacen(&format!("agents.1={high}"), &format!("agents.1={low}"), 1),
                "duplicate_item",
            ),
            (
                "no agents",
                text.replacen(
                    &format!("agents.count=2\nagents.0={low}\nagents.1={high}\n"),
                    "agents.count=0\n",
                    1,
                ),
                "count_out_of_range",
            ),
            (
                "short key",
                text.replacen("session_key=3059", "session_key=59", 1),
                "invalid_spki",
            ),
            (
                "off curve",
                {
                    let spki = hex::encode(testkit::p256_spki(&f.session_key));
                    let mut bad = spki.clone();
                    let last = if bad.ends_with('0') { "1" } else { "0" };
                    bad.replace_range(181.., last);
                    text.replacen(&spki, &bad, 1)
                },
                "invalid_public_key",
            ),
        ];
        for (case, t, expected) in cases {
            let err = ReadSessionRequest::decode(t.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{case}");
        }
        let too_many = ReadSessionRequest::new(
            Network::Devnet,
            testkit::passkey_id(),
            ending.session_key.clone(),
            Nonce16([0; 16]),
            now,
            now.checked_add_secs(60).unwrap(),
            (0..65u8)
                .map(|i| {
                    PartyId::parse(&format!("a{i:02}::{}", namespace_for_key(&[i; 32]))).unwrap()
                })
                .collect(),
        );
        assert_eq!(
            too_many.encode().unwrap_err().code_name(),
            "count_out_of_range"
        );
    }

    #[test]
    fn read_requests_check_the_session_window_and_signature() {
        let f = Fixture::new();
        let now = at(DAY);
        let session = f.verify(&f.request(&[0, 1], now, 600), now).unwrap();
        let now_ms = u64::try_from(now.unix_secs()).unwrap() * 1000 + 250;
        let request = ReadRequest {
            network: Network::Devnet,
            session_id: session.session_id,
            method: Token::parse("GetAgentActivity").unwrap(),
            agent_party: f.party(1),
            range_offset: 40,
            range_limit: 20,
            issued_at_ms: now_ms,
            nonce: Nonce16([0x42; 16]),
        };
        let check = |request: &ReadRequest, at_ms: u64| {
            let bytes = request.encode().unwrap();
            verify_read_request(&session, &bytes, &f.sign(&bytes), at_ms)
        };
        assert_eq!(check(&request, now_ms).unwrap(), request);
        let bytes = request.encode().unwrap();
        assert_eq!(ReadRequest::decode(&bytes).unwrap(), request);
        assert!(check(&request, now_ms + READ_REQUEST_WINDOW_MS).is_ok());
        assert!(check(&request, now_ms - READ_REQUEST_WINDOW_MS).is_ok());
        let name = |result: Result<ReadRequest, ReadSessionError>| result.unwrap_err().code_name();
        assert_eq!(name(check(&request, now_ms + 60_001)), "outside_window");
        assert_eq!(name(check(&request, now_ms - 60_001)), "outside_window");
        let mut other = request.clone();
        other.session_id = SessionId([1; 16]);
        assert_eq!(name(check(&other, now_ms)), "session_mismatch");
        let mut outside = request.clone();
        outside.agent_party = f.party(2);
        assert_eq!(name(check(&outside, now_ms)), "agent_not_in_session");
        let mut mainnet = request.clone();
        mainnet.network = Network::Mainnet;
        assert_eq!(name(check(&mainnet, now_ms)), "wrong_network");
        let mut late = request.clone();
        late.issued_at_ms = now_ms + 601_000;
        assert_eq!(name(check(&late, now_ms + 601_000)), "expired");
        assert_eq!(
            name(verify_read_request(
                &session,
                &bytes,
                &f.sign(b"other"),
                now_ms
            )),
            "bad_signature"
        );
        let short = f.sign(&bytes);
        assert_eq!(
            name(verify_read_request(&session, &bytes, &short[..63], now_ms)),
            "invalid_signature_length"
        );
        let holdings = ReadRequest {
            range_offset: 0,
            range_limit: 0,
            ..request.clone()
        };
        assert!(check(&holdings, now_ms).is_ok());
        let text = String::from_utf8(bytes).unwrap();
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "range overflow",
                text.replacen("range_offset=40", &format!("range_offset={}", u64::MAX), 1),
                "invalid_range",
            ),
            (
                "seconds not ms",
                text.replacen(
                    &format!("issued_at_ms={now_ms}"),
                    &format!("issued_at_ms={}", now.unix_secs()),
                    1,
                ),
                "invalid_time",
            ),
            (
                "method",
                text.replacen("method=GetAgentActivity", "method=Get Activity", 1),
                "invalid_token",
            ),
            (
                "scheme",
                text.replacen("read-request.v1", "read-call.v1", 1),
                "wrong_scheme",
            ),
        ];
        for (case, t, expected) in cases {
            let err = ReadRequest::decode(t.as_bytes()).unwrap_err();
            assert_eq!(err.code_name(), expected, "{case}");
        }
        assert_eq!(
            u64::try_from(crate::canon::time::MIN_UNIX_SECS).unwrap() * 1000,
            MIN_UNIX_MS
        );
        assert_eq!(
            u64::try_from(crate::canon::time::MAX_UNIX_SECS).unwrap() * 1000 + 999,
            MAX_UNIX_MS
        );
    }
}
