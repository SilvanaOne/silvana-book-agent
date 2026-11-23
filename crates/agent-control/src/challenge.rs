//! Controller sign-in challenges (`silvana.agent.controller-challenge.v1`): the rpc issues one,
//! an Ed25519 controller checks and signs it, and the rpc verifies the proof.

use crate::SCHEME_CONTROLLER_CHALLENGE;
use crate::canon::time::UtcTime;
use crate::canon::values::{Network, Nonce16, RpId, sha256};
use crate::canon::{Reader, Writer, decode_exact};
use crate::controller::{ControllerId, ControllerType, controller_id_for};
use crate::error::{CanonError, CanonErrorKind, ChallengeError, ProofError};
use crate::party::PartyId;
use crate::proof::{ControllerProof, ProofEnvelope};
use crate::sig::ed25519::verify_ed25519_strict;

/// Largest challenge, in bytes.
pub const MAX_CHALLENGE_BYTES: usize = 1024;
/// Longest challenge validity, in seconds; the shortest is one second.
pub const MAX_CHALLENGE_SECS: u64 = 60;
/// How far a challenge's `issued_at` may lie from the signer's clock, either way.
pub const CHALLENGE_CLOCK_SKEW_SECS: u64 = 120;

/// A sign-in challenge an rpc issues to one controller of one agent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControllerChallenge {
    pub network: Network,
    /// The TLS host of the rpc that issued it.
    pub rpc: RpId,
    pub controller_id: ControllerId,
    pub agent_party: PartyId,
    pub nonce: Nonce16,
    pub issued_at: UtcTime,
    pub expires_at: UtcTime,
}

fn validity_ok(issued_at: UtcTime, expires_at: UtcTime) -> bool {
    u64::try_from(issued_at.secs_until(expires_at))
        .is_ok_and(|span| (1..=MAX_CHALLENGE_SECS).contains(&span))
}

impl ControllerChallenge {
    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_CONTROLLER_CHALLENGE, MAX_CHALLENGE_BYTES);
        w.line("network", self.network.as_str());
        w.line("rpc", self.rpc.as_str());
        w.line("controller_id", &self.controller_id.to_string());
        w.line("agent_party", self.agent_party.as_str());
        w.line("nonce", &self.nonce.to_string());
        w.line("issued_at", &self.issued_at.to_string());
        w.line("expires_at", &self.expires_at.to_string());
        if !validity_ok(self.issued_at, self.expires_at) {
            w.fail(CanonErrorKind::InvalidValidity);
        }
        w.finish()
    }

    /// Strict decoding: every rule, and the bytes must re-encode identically.
    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_CONTROLLER_CHALLENGE,
            MAX_CHALLENGE_BYTES,
            Self::read,
            Self::encode,
        )
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let network = r.value("network", Network::parse)?;
        let rpc = r.value("rpc", RpId::parse)?;
        let controller_id = r.value("controller_id", ControllerId::parse)?;
        let agent_party = r.value("agent_party", PartyId::parse)?;
        let nonce = r.value("nonce", Nonce16::parse)?;
        let issued_at = r.value("issued_at", UtcTime::parse)?;
        let expires_at = r.value("expires_at", UtcTime::parse)?;
        if !validity_ok(issued_at, expires_at) {
            return Err(r.err(CanonErrorKind::InvalidValidity));
        }
        Ok(Self {
            network,
            rpc,
            controller_id,
            agent_party,
            nonce,
            issued_at,
            expires_at,
        })
    }
}

/// What the signer requires a challenge to name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExpectedChallenge {
    pub network: Network,
    /// The TLS host the signer connected to.
    pub rpc: RpId,
    pub controller_id: ControllerId,
    pub agent_party: PartyId,
}

/// A challenge that passed [`check_challenge`]; a signer signs exactly these bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedChallenge {
    challenge: ControllerChallenge,
    bytes: Vec<u8>,
}

impl CheckedChallenge {
    pub fn challenge(&self) -> &ControllerChallenge {
        &self.challenge
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The signer's check: exact decoding, the expected network, rpc host, controller and agent,
/// and `issued_at` within [`CHALLENGE_CLOCK_SKEW_SECS`] of `now`.
pub fn check_challenge(
    bytes: &[u8],
    expected: &ExpectedChallenge,
    now: UtcTime,
) -> Result<CheckedChallenge, ChallengeError> {
    let challenge = ControllerChallenge::decode(bytes).map_err(ChallengeError::Canon)?;
    if challenge.network != expected.network {
        return Err(ChallengeError::WrongNetwork);
    }
    if challenge.rpc != expected.rpc {
        return Err(ChallengeError::WrongRpc);
    }
    if challenge.controller_id != expected.controller_id {
        return Err(ChallengeError::WrongController);
    }
    if challenge.agent_party != expected.agent_party {
        return Err(ChallengeError::WrongParty);
    }
    if now.secs_until(challenge.issued_at).unsigned_abs() > CHALLENGE_CLOCK_SKEW_SECS {
        return Err(ChallengeError::OutsideWindow);
    }
    Ok(CheckedChallenge {
        challenge,
        bytes: bytes.to_vec(),
    })
}

/// The rpc's check: an Ed25519 proof by `key` over exactly these bytes, in an envelope that
/// names the key's controller id, which the challenge names too.
pub fn verify_challenge_proof(
    bytes: &[u8],
    envelope: &ProofEnvelope,
    key: &[u8; 32],
) -> Result<ControllerChallenge, ChallengeError> {
    let challenge = ControllerChallenge::decode(bytes).map_err(ChallengeError::Canon)?;
    let id = controller_id_for(ControllerType::Ed25519, key);
    if challenge.controller_id != id {
        return Err(ChallengeError::WrongController);
    }
    if *envelope.controller_id() != id {
        return Err(ChallengeError::Proof(ProofError::ControllerMismatch));
    }
    if envelope.payload_sha256().0 != sha256(bytes) {
        return Err(ChallengeError::Proof(ProofError::PayloadMismatch));
    }
    let ControllerProof::Ed25519 { signature } = envelope.proof() else {
        return Err(ChallengeError::Proof(ProofError::ProofTypeMismatch));
    };
    verify_ed25519_strict(key, bytes, signature)
        .map_err(|e| ChallengeError::Proof(ProofError::Signature(e)))?;
    Ok(challenge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::party::namespace_for_key;
    use crate::testkit::{self, t};
    use ed25519_dalek::Signer;

    const RPC: &str = "orderbook-devnet.silvana.dev";

    fn sample() -> ControllerChallenge {
        ControllerChallenge {
            network: Network::Devnet,
            rpc: RpId::parse(RPC).unwrap(),
            controller_id: testkit::ed25519_id(0x81),
            agent_party: testkit::agent_party(),
            nonce: Nonce16([0x5c; 16]),
            issued_at: t("2026-11-23T10:00:00Z"),
            expires_at: t("2026-11-23T10:01:00Z"),
        }
    }

    fn expected() -> ExpectedChallenge {
        ExpectedChallenge {
            network: Network::Devnet,
            rpc: RpId::parse(RPC).unwrap(),
            controller_id: testkit::ed25519_id(0x81),
            agent_party: testkit::agent_party(),
        }
    }

    fn ed25519_envelope(start: u8, id: ControllerId, payload: &[u8]) -> ProofEnvelope {
        let signature = testkit::ed_key(start).sign(payload).to_bytes();
        ProofEnvelope::new(id, payload, ControllerProof::Ed25519 { signature })
    }

    fn name<T: std::fmt::Debug>(result: Result<T, ChallengeError>) -> &'static str {
        result.unwrap_err().code_name()
    }

    #[test]
    fn the_layout_is_fixed_and_round_trips() {
        let challenge = sample();
        let bytes = challenge.encode().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let expected = format!(
            "scheme=silvana.agent.controller-challenge.v1\nnetwork=devnet\n\
             rpc=orderbook-devnet.silvana.dev\ncontroller_id={}\nagent_party={}\n\
             nonce=5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c\nissued_at=2026-11-23T10:00:00Z\n\
             expires_at=2026-11-23T10:01:00Z\n",
            testkit::ed25519_id(0x81),
            testkit::agent_party()
        );
        assert_eq!(text, expected);
        assert_eq!(ControllerChallenge::decode(&bytes).unwrap(), challenge);
        assert!(bytes.len() <= MAX_CHALLENGE_BYTES);
        // The longest fields still fit.
        let label = |c: &str| c.repeat(63);
        let host = format!(
            "{}.{}.{}.{}",
            label("a"),
            label("b"),
            label("c"),
            "d".repeat(61)
        );
        assert_eq!(host.len(), 253);
        let party = format!("{}::{}", "h".repeat(185), namespace_for_key(&[7; 32]));
        let long = ControllerChallenge {
            network: Network::Mainnet,
            rpc: RpId::parse(&host).unwrap(),
            agent_party: PartyId::parse(&party).unwrap(),
            ..challenge.clone()
        };
        let long_bytes = long.encode().unwrap();
        assert!(long_bytes.len() <= MAX_CHALLENGE_BYTES);
        assert_eq!(ControllerChallenge::decode(&long_bytes).unwrap(), long);
        let local = ControllerChallenge {
            rpc: RpId::parse("localhost").unwrap(),
            ..challenge
        };
        assert!(ControllerChallenge::decode(&local.encode().unwrap()).is_ok());
    }

    #[test]
    fn validity_is_one_to_sixty_seconds() {
        let challenge = sample();
        for secs in [1, 60] {
            let mut ok = challenge.clone();
            ok.expires_at = ok.issued_at.checked_add_secs(secs).unwrap();
            assert_eq!(
                ControllerChallenge::decode(&ok.encode().unwrap()).unwrap(),
                ok
            );
        }
        for secs in [0, 61, -1] {
            let mut bad = challenge.clone();
            bad.expires_at = bad.issued_at.checked_add_secs(secs).unwrap();
            assert_eq!(
                bad.encode().unwrap_err().code_name(),
                "invalid_validity",
                "{secs}"
            );
        }
        let text = String::from_utf8(challenge.encode().unwrap()).unwrap();
        let long = text.replacen(
            "expires_at=2026-11-23T10:01:00Z",
            "expires_at=2026-11-23T10:01:01Z",
            1,
        );
        let error = ControllerChallenge::decode(long.as_bytes()).unwrap_err();
        assert_eq!((error.code_name(), error.line), ("invalid_validity", 8));
    }

    #[test]
    fn decoding_refusals_have_stable_names() {
        let text = String::from_utf8(sample().encode().unwrap()).unwrap();
        let id = testkit::ed25519_id(0x81).to_string();
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "scheme",
                text.replacen("controller-challenge.v1", "controller-challenge.v2", 1),
                "wrong_scheme",
            ),
            (
                "read session scheme",
                text.replacen("controller-challenge.v1", "read-session.v1", 1),
                "wrong_scheme",
            ),
            (
                "network",
                text.replacen("network=devnet", "network=testnet", 1),
                "invalid_network",
            ),
            (
                "uppercase rpc",
                text.replacen(RPC, "Orderbook-devnet.silvana.dev", 1),
                "invalid_rp_id",
            ),
            (
                "rpc with port",
                text.replacen(RPC, "orderbook-devnet.silvana.dev:443", 1),
                "invalid_rp_id",
            ),
            (
                "rpc url",
                text.replacen(RPC, "https://orderbook-devnet.silvana.dev", 1),
                "invalid_rp_id",
            ),
            (
                "rpc address",
                text.replacen(RPC, "192.0.2.1", 1),
                "invalid_rp_id",
            ),
            (
                "controller id",
                text.replacen(&id, &id.to_uppercase(), 1),
                "invalid_hex32",
            ),
            (
                "party",
                text.replacen("agent_party=agent-1::", "agent_party=agent 1::", 1),
                "invalid_party",
            ),
            (
                "short nonce",
                text.replacen("nonce=5c5c", "nonce=5c", 1),
                "invalid_hex16",
            ),
            (
                "time",
                text.replacen(
                    "issued_at=2026-11-23T10:00:00Z",
                    "issued_at=2026-11-23T10:00Z",
                    1,
                ),
                "invalid_time",
            ),
            (
                "swapped",
                text.replacen(
                    &format!("rpc={RPC}\ncontroller_id={id}\n"),
                    &format!("controller_id={id}\nrpc={RPC}\n"),
                    1,
                ),
                "unexpected_key",
            ),
            (
                "missing nonce",
                text.replacen("nonce=5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c\n", "", 1),
                "unexpected_key",
            ),
            ("trailing", format!("{text}extra=1\n"), "trailing_line"),
            ("crlf", text.replace('\n', "\r\n"), "invalid_byte"),
            (
                "too large",
                format!("{text}{}", "x".repeat(1024)),
                "too_large",
            ),
        ];
        for (case, bytes, expected) in cases {
            let error = ControllerChallenge::decode(bytes.as_bytes()).unwrap_err();
            assert_eq!(error.code_name(), expected, "{case}");
        }
    }

    #[test]
    fn the_signer_checks_what_the_challenge_names_and_when_it_was_issued() {
        let challenge = sample();
        let bytes = challenge.encode().unwrap();
        let expected = expected();
        let now = challenge.issued_at;
        let checked = check_challenge(&bytes, &expected, now).unwrap();
        assert_eq!(checked.challenge(), &challenge);
        assert_eq!(checked.bytes(), bytes.as_slice());
        let at = |secs: i64| now.checked_add_secs(secs).unwrap();
        for secs in [-120, 0, 90, 120] {
            assert!(
                check_challenge(&bytes, &expected, at(secs)).is_ok(),
                "{secs}"
            );
        }
        for secs in [-121, 121, 3600] {
            assert_eq!(
                name(check_challenge(&bytes, &expected, at(secs))),
                "outside_window",
                "{secs}"
            );
        }
        let other_party =
            PartyId::parse(&format!("agent-2::{}", namespace_for_key(&[9; 32]))).unwrap();
        let cases = [
            (
                ExpectedChallenge {
                    network: Network::Mainnet,
                    ..expected.clone()
                },
                "wrong_network",
            ),
            (
                ExpectedChallenge {
                    rpc: RpId::parse("orderbook-mainnet.silvana.dev").unwrap(),
                    ..expected.clone()
                },
                "wrong_rpc",
            ),
            (
                ExpectedChallenge {
                    controller_id: testkit::ed25519_id(0x82),
                    ..expected.clone()
                },
                "wrong_controller",
            ),
            (
                ExpectedChallenge {
                    agent_party: other_party,
                    ..expected.clone()
                },
                "wrong_party",
            ),
        ];
        for (other, refusal) in cases {
            assert_eq!(name(check_challenge(&bytes, &other, now)), refusal);
        }
        assert_eq!(name(check_challenge(b"junk", &expected, now)), "too_short");
        let mut padded = bytes.clone();
        padded.extend_from_slice(b"extra=1\n");
        assert_eq!(
            name(check_challenge(&padded, &expected, now)),
            "trailing_line"
        );
    }

    #[test]
    fn the_rpc_accepts_only_an_ed25519_proof_by_the_named_key() {
        let challenge = sample();
        let bytes = challenge.encode().unwrap();
        let id = testkit::ed25519_id(0x81);
        let key = testkit::ed_key(0x81).verifying_key().to_bytes();
        let envelope = ed25519_envelope(0x81, id, &bytes);
        assert_eq!(
            verify_challenge_proof(&bytes, &envelope, &key).unwrap(),
            challenge
        );
        let carried = ProofEnvelope::decode(&envelope.encode().unwrap()).unwrap();
        assert_eq!(
            verify_challenge_proof(&bytes, &carried, &key).unwrap(),
            challenge
        );

        let refusal = |bytes: &[u8], envelope: &ProofEnvelope, key: &[u8; 32]| {
            name(verify_challenge_proof(bytes, envelope, key))
        };

        // Another key, whose own proof is valid, is not the controller the challenge names.
        let other = testkit::ed_key(0x82).verifying_key().to_bytes();
        let theirs = ed25519_envelope(0x82, testkit::ed25519_id(0x82), &bytes);
        assert_eq!(refusal(&bytes, &theirs, &other), "wrong_controller");
        let misnamed = ed25519_envelope(0x81, testkit::ed25519_id(0x82), &bytes);
        assert_eq!(refusal(&bytes, &misnamed, &key), "controller_mismatch");
        let mut renewed = challenge.clone();
        renewed.nonce = Nonce16([0x11; 16]);
        let renewed = renewed.encode().unwrap();
        assert_eq!(refusal(&renewed, &envelope, &key), "payload_mismatch");
        let passkey = ProofEnvelope::new(
            id,
            &bytes,
            ControllerProof::WebAuthn(testkit::assertion_over(&bytes)),
        );
        assert_eq!(refusal(&bytes, &passkey, &key), "proof_type_mismatch");
        let mut signature = testkit::ed_key(0x81).sign(&bytes).to_bytes();
        signature[7] ^= 1;
        let flipped = ProofEnvelope::new(id, &bytes, ControllerProof::Ed25519 { signature });
        assert_eq!(refusal(&bytes, &flipped, &key), "bad_signature");
        let signature = testkit::ed_key(0x81).sign(b"another document").to_bytes();
        let elsewhere = ProofEnvelope::new(id, &bytes, ControllerProof::Ed25519 { signature });
        assert_eq!(refusal(&bytes, &elsewhere, &key), "bad_signature");
        assert_eq!(refusal(b"junk", &envelope, &key), "too_short");

        // A small-order key is refused even when everything names it.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        let weak_id = controller_id_for(ControllerType::Ed25519, &weak);
        let weak_bytes = ControllerChallenge {
            controller_id: weak_id,
            ..challenge
        }
        .encode()
        .unwrap();
        let forged = ProofEnvelope::new(
            weak_id,
            &weak_bytes,
            ControllerProof::Ed25519 { signature: [0; 64] },
        );
        assert_eq!(refusal(&weak_bytes, &forged, &weak), "weak_public_key");
    }
}
