//! Real browser assertions from tests/vectors/webauthn-captured.json, when the file exists.
//! Each one must verify exactly as the synthetic vectors do.

use std::path::PathBuf;

use agent_control::canon::values::decode_b64url;
use agent_control::{
    BackupPolicy, CapabilitySet, ControllerDescriptor, ControllerKey, ControllerProof, Label,
    Origin, ProofEnvelope, RpId, WebAuthnAssertion, WebAuthnKey, WebAuthnParams, parse_p256_spki,
    verify_assertion, verify_controller_proof,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA: &str = "silvana.agent-control.webauthn-captured.v1";

fn text<'a>(capture: &'a Value, key: &str, label: &str) -> &'a str {
    capture[key]
        .as_str()
        .unwrap_or_else(|| panic!("{label}: {key} is missing"))
}

fn b64(capture: &Value, key: &str, label: &str) -> Vec<u8> {
    decode_b64url(text(capture, key, label))
        .unwrap_or_else(|| panic!("{label}: {key} is not base64url"))
}

fn check(capture: &Value) {
    let label = capture["browser"].as_str().unwrap_or("unlabelled capture");
    let spki = hex::decode(text(capture, "spkiHex", label)).expect("spkiHex is hex");
    parse_p256_spki(&spki).unwrap_or_else(|e| panic!("{label}: {}", e.code_name()));
    let rp_id = RpId::parse(text(capture, "rpId", label)).expect("rpId");
    let origin = Origin::parse(text(capture, "origin", label)).expect("origin");
    assert!(origin.matches_rp_id(&rp_id), "{label}: origin outside rpId");
    let payload = text(capture, "payload", label).as_bytes();
    assert!(
        payload.starts_with(b"scheme=silvana.agent.")
            || payload.starts_with(b"silvana.agent.control-key-check.v1\n"),
        "{label}: not a control document or key check"
    );
    let challenge: [u8; 32] = Sha256::digest(payload).into();
    let assertion = WebAuthnAssertion {
        authenticator_data: b64(capture, "authenticatorDataB64url", label),
        client_data_json: b64(capture, "clientDataJsonB64url", label),
        signature: b64(capture, "signatureB64url", label),
    };
    let origins = [origin.clone()];
    let params = WebAuthnParams {
        spki: &spki,
        rp_id: &rp_id,
        origins: &origins,
        backup_policy: BackupPolicy::Any,
    };
    let verdict = verify_assertion(&params, &challenge, &assertion);
    let expected = text(capture, "expect", label);
    if expected != "ok" {
        let refused = verdict.expect_err(label);
        assert_eq!(refused.code_name(), expected, "{label}");
        return;
    }
    let info = verdict.unwrap_or_else(|e| panic!("{label}: {}", e.code_name()));
    assert_eq!(
        Some(info.backup_eligible),
        capture["backupEligible"].as_bool(),
        "{label}: BE"
    );
    assert_eq!(
        Some(info.backup_state),
        capture["backupState"].as_bool(),
        "{label}: BS"
    );
    if let Some(count) = capture["signCount"].as_u64() {
        assert_eq!(u64::from(info.sign_count), count, "{label}: signCount");
    }
    let device_bound = WebAuthnParams {
        backup_policy: BackupPolicy::DeviceBound,
        ..params
    };
    match verify_assertion(&device_bound, &challenge, &assertion) {
        Ok(_) => assert!(!info.backup_eligible, "{label}: BE set under device_bound"),
        Err(e) => {
            assert!(info.backup_eligible, "{label}: {}", e.code_name());
            assert_eq!(e.code_name(), "backup_not_allowed", "{label}");
        }
    }
    let other: [u8; 32] = Sha256::digest(b"another payload").into();
    let refused = verify_assertion(&params, &other, &assertion).expect_err(label);
    assert_eq!(refused.code_name(), "challenge_mismatch", "{label}");
    let other_origin = if origin.to_string() == "https://app.silvana.link" {
        "https://www.silvana.link"
    } else {
        "https://app.silvana.link"
    };
    let elsewhere = [Origin::parse(other_origin).expect("origin")];
    let moved = WebAuthnParams {
        origins: &elsewhere,
        ..params
    };
    let refused = verify_assertion(&moved, &challenge, &assertion).expect_err(label);
    assert_eq!(refused.code_name(), "origin_not_allowed", "{label}");
    if capture.get("credentialIdB64url").is_some() {
        let key = WebAuthnKey::new(
            &spki,
            b64(capture, "credentialIdB64url", label),
            rp_id,
            vec![origin],
            BackupPolicy::Any,
        )
        .unwrap_or_else(|e| panic!("{label}: {}", e.code_name()));
        let controller = ControllerDescriptor::new(
            ControllerKey::WebAuthn(key),
            Label::parse("Captured passkey").expect("label"),
            CapabilitySet::ALL,
        )
        .expect("controller");
        let envelope = ProofEnvelope::new(
            *controller.id(),
            payload,
            ControllerProof::WebAuthn(assertion),
        );
        let bytes = envelope
            .encode()
            .unwrap_or_else(|e| panic!("{label}: {}", e.code_name()));
        let decoded = ProofEnvelope::decode(&bytes).expect("envelope");
        let outcome = verify_controller_proof(&controller, payload, &decoded)
            .unwrap_or_else(|e| panic!("{label}: {}", e.code_name()));
        assert_eq!(outcome.sign_count, Some(info.sign_count), "{label}");
    }
}

#[test]
fn captured_browser_assertions_verify() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/webauthn-captured.json");
    let Ok(file) = std::fs::read_to_string(&path) else {
        eprintln!(
            "skipped: {} is absent; capture it with the WebAuthn capture page",
            path.display()
        );
        return;
    };
    let file: Value = serde_json::from_str(&file).expect("webauthn-captured.json is JSON");
    assert_eq!(file["schema"], SCHEMA);
    let captures = file["captures"].as_array().expect("captures");
    assert!(!captures.is_empty(), "no captures");
    for capture in captures {
        check(capture);
    }
}
