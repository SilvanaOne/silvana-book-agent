//! Controller proofs and the standalone `silvana.agent.proof.v1` envelope.

use crate::canon::values::{Hash32, Sig64, decode_b64url_bounded, encode_b64url, sha256};
use crate::canon::{Reader, Writer, decode_exact};
use crate::controller::{ControllerDescriptor, ControllerId, ControllerKey};
use crate::error::{CanonError, CanonErrorKind, ProofError, ValueError, ValueKind};
use crate::sig::ed25519::verify_ed25519_strict;
use crate::sig::p256::{MAX_DER_SIGNATURE, MIN_DER_SIGNATURE};
use crate::sig::webauthn::{
    MAX_AUTH_DATA, MAX_CLIENT_DATA, MIN_AUTH_DATA, WebAuthnAssertion, verify_assertion,
};
use crate::{MAX_PROOF_BYTES, SCHEME_PROOF};

/// The proof kind, matching the controller type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProofType {
    Ed25519,
    WebAuthn,
}

impl ProofType {
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
            _ => Err(ValueError(ValueKind::ProofType)),
        }
    }
}

/// An Ed25519 signature over the payload, or a WebAuthn assertion over its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ControllerProof {
    Ed25519 { signature: [u8; 64] },
    WebAuthn(WebAuthnAssertion),
}

/// What a verified proof reports; counters come only from WebAuthn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProofOutcome {
    pub sign_count: Option<u32>,
    pub backup_eligible: bool,
    pub backup_state: bool,
}

fn bounded(
    kind: ValueKind,
    min: usize,
    max: usize,
) -> impl Fn(&str) -> Result<Vec<u8>, ValueError> {
    move |s| decode_b64url_bounded(s, min, max).ok_or(ValueError(kind))
}

impl ControllerProof {
    pub fn proof_type(&self) -> ProofType {
        match self {
            Self::Ed25519 { .. } => ProofType::Ed25519,
            Self::WebAuthn(_) => ProofType::WebAuthn,
        }
    }

    /// Verifies the proof over `payload` with the descriptor's key.
    pub fn verify(
        &self,
        controller: &ControllerDescriptor,
        payload: &[u8],
    ) -> Result<ProofOutcome, ProofError> {
        match (controller.key(), self) {
            (ControllerKey::Ed25519(public_key), Self::Ed25519 { signature }) => {
                verify_ed25519_strict(public_key, payload, signature)?;
                Ok(ProofOutcome {
                    sign_count: None,
                    backup_eligible: false,
                    backup_state: false,
                })
            }
            (ControllerKey::WebAuthn(passkey), Self::WebAuthn(assertion)) => {
                let info = verify_assertion(&passkey.params(), &sha256(payload), assertion)?;
                Ok(ProofOutcome {
                    sign_count: Some(info.sign_count),
                    backup_eligible: info.backup_eligible,
                    backup_state: info.backup_state,
                })
            }
            _ => Err(ProofError::ProofTypeMismatch),
        }
    }

    /// Writes `<prefix>.type` and the signature fields.
    pub(crate) fn write(&self, w: &mut Writer, prefix: &str) {
        let key = |field: &str| format!("{prefix}.{field}");
        w.line(&key("type"), self.proof_type().as_str());
        match self {
            Self::Ed25519 { signature } => {
                w.line(&key("signature"), &Sig64(*signature).to_string());
            }
            Self::WebAuthn(assertion) => {
                let fields = [
                    (
                        "authenticator_data",
                        &assertion.authenticator_data,
                        ValueKind::AuthenticatorData,
                        MIN_AUTH_DATA,
                        MAX_AUTH_DATA,
                    ),
                    (
                        "client_data_json",
                        &assertion.client_data_json,
                        ValueKind::ClientDataJson,
                        1,
                        MAX_CLIENT_DATA,
                    ),
                    (
                        "signature",
                        &assertion.signature,
                        ValueKind::DerSignature,
                        MIN_DER_SIGNATURE,
                        MAX_DER_SIGNATURE,
                    ),
                ];
                for (field, bytes, kind, min, max) in fields {
                    w.line(&key(field), &encode_b64url(bytes));
                    if !(min..=max).contains(&bytes.len()) {
                        w.fail(CanonErrorKind::InvalidValue(kind));
                    }
                }
            }
        }
    }

    /// Reads `<prefix>.type` and the signature fields.
    pub(crate) fn read(r: &mut Reader<'_>, prefix: &str) -> Result<Self, CanonError> {
        let key = |field: &str| format!("{prefix}.{field}");
        match r.value(&key("type"), ProofType::parse)? {
            ProofType::Ed25519 => {
                let signature = r.value(&key("signature"), Sig64::parse)?;
                Ok(Self::Ed25519 {
                    signature: signature.0,
                })
            }
            ProofType::WebAuthn => {
                let authenticator_data = r.value(
                    &key("authenticator_data"),
                    bounded(ValueKind::AuthenticatorData, MIN_AUTH_DATA, MAX_AUTH_DATA),
                )?;
                let client_data_json = r.value(
                    &key("client_data_json"),
                    bounded(ValueKind::ClientDataJson, 1, MAX_CLIENT_DATA),
                )?;
                let signature = r.value(
                    &key("signature"),
                    bounded(
                        ValueKind::DerSignature,
                        MIN_DER_SIGNATURE,
                        MAX_DER_SIGNATURE,
                    ),
                )?;
                Ok(Self::WebAuthn(WebAuthnAssertion {
                    authenticator_data,
                    client_data_json,
                    signature,
                }))
            }
        }
    }
}

/// A proof bound to one controller and one payload hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProofEnvelope {
    controller_id: ControllerId,
    payload_sha256: Hash32,
    proof: ControllerProof,
}

impl ProofEnvelope {
    /// An envelope for `proof` over `payload`.
    pub fn new(controller_id: ControllerId, payload: &[u8], proof: ControllerProof) -> Self {
        Self {
            controller_id,
            payload_sha256: Hash32(sha256(payload)),
            proof,
        }
    }

    pub fn controller_id(&self) -> &ControllerId {
        &self.controller_id
    }

    pub fn payload_sha256(&self) -> &Hash32 {
        &self.payload_sha256
    }

    pub fn proof(&self) -> &ControllerProof {
        &self.proof
    }

    pub fn encode(&self) -> Result<Vec<u8>, CanonError> {
        let mut w = Writer::new(SCHEME_PROOF, MAX_PROOF_BYTES);
        w.line("controller_id", &self.controller_id.to_string());
        w.line("payload_sha256", &self.payload_sha256.to_string());
        self.proof.write(&mut w, "proof");
        w.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CanonError> {
        decode_exact(
            bytes,
            SCHEME_PROOF,
            MAX_PROOF_BYTES,
            Self::read,
            Self::encode,
        )
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, CanonError> {
        let controller_id = r.value("controller_id", ControllerId::parse)?;
        let payload_sha256 = r.value("payload_sha256", Hash32::parse)?;
        let proof = ControllerProof::read(r, "proof")?;
        Ok(Self {
            controller_id,
            payload_sha256,
            proof,
        })
    }
}

/// Requires the envelope to name this controller and payload, then verifies the proof.
pub fn verify_controller_proof(
    controller: &ControllerDescriptor,
    payload: &[u8],
    envelope: &ProofEnvelope,
) -> Result<ProofOutcome, ProofError> {
    if envelope.controller_id != *controller.id() {
        return Err(ProofError::ControllerMismatch);
    }
    if envelope.payload_sha256.0 != sha256(payload) {
        return Err(ProofError::PayloadMismatch);
    }
    envelope.proof.verify(controller, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::CapabilitySet;
    use crate::error::{SignatureError, WebAuthnError};
    use crate::testkit;
    use ed25519_dalek::Signer;

    const PAYLOAD: &[u8] = b"scheme=silvana.agent.policy.v1\nnetwork=devnet\nversion=1\n";

    fn ed25519_envelope() -> (ControllerDescriptor, ProofEnvelope) {
        let controller = testkit::ed25519_controller(CapabilitySet::ALL);
        let signature = testkit::ed_key(0x21).sign(PAYLOAD).to_bytes();
        let envelope = ProofEnvelope::new(
            *controller.id(),
            PAYLOAD,
            ControllerProof::Ed25519 { signature },
        );
        (controller, envelope)
    }

    fn webauthn_envelope() -> (ControllerDescriptor, ProofEnvelope) {
        let controller = testkit::passkey_controller(CapabilitySet::ALL);
        let assertion = testkit::assertion_over(PAYLOAD);
        let envelope = ProofEnvelope::new(
            *controller.id(),
            PAYLOAD,
            ControllerProof::WebAuthn(assertion),
        );
        (controller, envelope)
    }

    #[test]
    fn envelopes_round_trip_in_field_order() {
        for (_, envelope) in [ed25519_envelope(), webauthn_envelope()] {
            let bytes = envelope.encode().unwrap();
            let text = String::from_utf8(bytes.clone()).unwrap();
            let keys: Vec<&str> = text.lines().map(|l| l.split_once('=').unwrap().0).collect();
            match envelope.proof() {
                ControllerProof::Ed25519 { .. } => {
                    assert_eq!(
                        keys,
                        [
                            "scheme",
                            "controller_id",
                            "payload_sha256",
                            "proof.type",
                            "proof.signature"
                        ]
                    );
                }
                ControllerProof::WebAuthn(_) => assert_eq!(
                    keys,
                    [
                        "scheme",
                        "controller_id",
                        "payload_sha256",
                        "proof.type",
                        "proof.authenticator_data",
                        "proof.client_data_json",
                        "proof.signature"
                    ]
                ),
            }
            assert!(text.starts_with("scheme=silvana.agent.proof.v1\n"));
            assert_eq!(ProofEnvelope::decode(&bytes).unwrap(), envelope);
        }
    }

    #[test]
    fn verifies_both_proof_types() {
        let (controller, envelope) = ed25519_envelope();
        let outcome = verify_controller_proof(&controller, PAYLOAD, &envelope).unwrap();
        assert_eq!(outcome.sign_count, None);
        let (controller, envelope) = webauthn_envelope();
        let outcome = verify_controller_proof(&controller, PAYLOAD, &envelope).unwrap();
        assert_eq!(outcome.sign_count, Some(0));
        assert!(outcome.backup_eligible && outcome.backup_state);
    }

    #[test]
    fn verification_refusals() {
        let (ed_controller, ed_envelope) = ed25519_envelope();
        let (passkey, passkey_envelope) = webauthn_envelope();
        assert_eq!(
            verify_controller_proof(&passkey, PAYLOAD, &ed_envelope),
            Err(ProofError::ControllerMismatch)
        );
        assert_eq!(
            verify_controller_proof(&ed_controller, b"other payload bytes", &ed_envelope),
            Err(ProofError::PayloadMismatch)
        );
        let crossed = ProofEnvelope::new(
            *ed_controller.id(),
            PAYLOAD,
            passkey_envelope.proof().clone(),
        );
        assert_eq!(
            verify_controller_proof(&ed_controller, PAYLOAD, &crossed),
            Err(ProofError::ProofTypeMismatch)
        );
        let other_signature = testkit::ed_key(0x21).sign(b"different bytes").to_bytes();
        let wrong = ProofEnvelope::new(
            *ed_controller.id(),
            PAYLOAD,
            ControllerProof::Ed25519 {
                signature: other_signature,
            },
        );
        let err = verify_controller_proof(&ed_controller, PAYLOAD, &wrong).unwrap_err();
        assert_eq!(err, ProofError::Signature(SignatureError::BadSignature));
        assert_eq!(err.code_name(), "bad_signature");
        let mut assertion = testkit::assertion_over(PAYLOAD);
        assertion.authenticator_data[32] &= !0x04;
        let unverified =
            ProofEnvelope::new(*passkey.id(), PAYLOAD, ControllerProof::WebAuthn(assertion));
        assert_eq!(
            verify_controller_proof(&passkey, PAYLOAD, &unverified),
            Err(ProofError::WebAuthn(WebAuthnError::UserNotVerified))
        );
        let other_payload = testkit::assertion_over(b"scheme=silvana.agent.command.v1\n");
        let mismatched = ProofEnvelope::new(
            *passkey.id(),
            PAYLOAD,
            ControllerProof::WebAuthn(other_payload),
        );
        assert_eq!(
            verify_controller_proof(&passkey, PAYLOAD, &mismatched),
            Err(ProofError::WebAuthn(WebAuthnError::ChallengeMismatch))
        );
    }

    fn replace(text: &str, key: &str, value: &str) -> String {
        let lines: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with(&format!("{key}=")) {
                    format!("{key}={value}")
                } else {
                    l.to_owned()
                }
            })
            .collect();
        lines.join("\n") + "\n"
    }

    #[test]
    fn decoding_refusals() {
        let ed = String::from_utf8(ed25519_envelope().1.encode().unwrap()).unwrap();
        let wa = String::from_utf8(webauthn_envelope().1.encode().unwrap()).unwrap();
        let b64 = encode_b64url;
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "scheme",
                ed.replacen("proof.v1", "proof.v2", 1),
                "wrong_scheme",
            ),
            (
                "controller id",
                replace(&ed, "controller_id", "abc"),
                "invalid_hex32",
            ),
            (
                "payload hash",
                replace(&ed, "payload_sha256", &"A".repeat(64)),
                "invalid_hex32",
            ),
            (
                "proof type",
                replace(&ed, "proof.type", "rsa"),
                "invalid_proof_type",
            ),
            (
                "ed signature",
                replace(&ed, "proof.signature", &"0".repeat(127)),
                "invalid_hex64",
            ),
            (
                "type swap",
                replace(&ed, "proof.type", "webauthn"),
                "unexpected_key",
            ),
            (
                "auth data short",
                replace(&wa, "proof.authenticator_data", &b64(&[5u8; 36])),
                "invalid_authenticator_data",
            ),
            (
                "auth data long",
                replace(&wa, "proof.authenticator_data", &b64(&[5u8; 1025])),
                "invalid_authenticator_data",
            ),
            (
                "client data long",
                replace(&wa, "proof.client_data_json", &b64(&[b'x'; 2049])),
                "invalid_client_data_json",
            ),
            (
                "client data empty",
                replace(&wa, "proof.client_data_json", ""),
                "empty_value",
            ),
            (
                "der short",
                replace(&wa, "proof.signature", &b64(&[0x30; 7])),
                "invalid_der_signature",
            ),
            (
                "der long",
                replace(&wa, "proof.signature", &b64(&[0x30; 73])),
                "invalid_der_signature",
            ),
            (
                "der padded",
                replace(&wa, "proof.signature", &(b64(&[0x30; 10]) + "==")),
                "invalid_der_signature",
            ),
            ("trailing", format!("{ed}extra=1\n"), "trailing_line"),
        ];
        for (name, text, expected) in &cases {
            assert_eq!(
                ProofEnvelope::decode(text.as_bytes())
                    .unwrap_err()
                    .code_name(),
                *expected,
                "{name}"
            );
        }
    }

    #[test]
    fn encoding_refuses_out_of_bounds_assertions() {
        let (controller, envelope) = webauthn_envelope();
        let ControllerProof::WebAuthn(mut assertion) = envelope.proof().clone() else {
            unreachable!()
        };
        assertion.signature = vec![0x30; 73];
        let bad = ProofEnvelope::new(
            *controller.id(),
            PAYLOAD,
            ControllerProof::WebAuthn(assertion),
        );
        assert_eq!(
            bad.encode().unwrap_err().kind,
            CanonErrorKind::InvalidValue(ValueKind::DerSignature)
        );
    }
}
