//! WebAuthn ES256 assertion verification. Sign counters are checked separately.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::canon::values::{Origin, RpId, encode_b64url, sha256};
use crate::controller::BackupPolicy;
use crate::error::{SignatureError, WebAuthnError};
use crate::sig::p256::{MAX_DER_SIGNATURE, MIN_DER_SIGNATURE, parse_p256_spki, verify_p256_der};

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
pub const FLAG_BE: u8 = 0x08;
pub const FLAG_BS: u8 = 0x10;
pub const FLAG_AT: u8 = 0x40;
pub const FLAG_ED: u8 = 0x80;
/// rpIdHash, flags and the 4-byte counter.
pub const MIN_AUTH_DATA: usize = 37;
pub const MAX_AUTH_DATA: usize = 1024;
pub const MAX_CLIENT_DATA: usize = 2048;
/// Deepest object or array nesting accepted in clientDataJSON.
pub const MAX_CLIENT_DATA_DEPTH: usize = 16;

const GET_TYPE: &str = "webauthn.get";

/// Bracket depth outside strings stays within `max`.
fn nesting_within(text: &str, max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for b in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > max {
                    return false;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// The three byte strings a browser returns from `navigator.credentials.get`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WebAuthnAssertion {
    pub authenticator_data: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub signature: Vec<u8>,
}

/// What a controller descriptor fixes for its assertions.
#[derive(Clone, Copy, Debug)]
pub struct WebAuthnParams<'a> {
    pub spki: &'a [u8],
    pub rp_id: &'a RpId,
    pub origins: &'a [Origin],
    pub backup_policy: BackupPolicy,
}

/// Facts from a verified assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssertionInfo {
    pub sign_count: u32,
    pub backup_eligible: bool,
    pub backup_state: bool,
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default, deserialize_with = "present")]
    cross_origin: Option<Value>,
    #[serde(rename = "topOrigin", default, deserialize_with = "present")]
    top_origin: Option<Value>,
}

/// Any present value, including null, is `Some`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// Verifies an assertion whose challenge is `challenge`, in a fixed check order.
pub fn verify_assertion(
    params: &WebAuthnParams<'_>,
    challenge: &[u8; 32],
    assertion: &WebAuthnAssertion,
) -> Result<AssertionInfo, WebAuthnError> {
    let key = parse_p256_spki(params.spki).map_err(|_| WebAuthnError::InvalidPublicKey)?;

    let client_data_json = assertion.client_data_json.as_slice();
    if client_data_json.len() > MAX_CLIENT_DATA {
        return Err(WebAuthnError::ClientDataTooLarge);
    }
    let text =
        std::str::from_utf8(client_data_json).map_err(|_| WebAuthnError::ClientDataInvalid)?;
    let object = text
        .trim_start_matches([' ', '\t', '\n', '\r'])
        .starts_with('{');
    if !object || !nesting_within(text, MAX_CLIENT_DATA_DEPTH) {
        return Err(WebAuthnError::ClientDataInvalid);
    }
    let client: ClientData =
        serde_json::from_str(text).map_err(|_| WebAuthnError::ClientDataInvalid)?;
    if client.kind != GET_TYPE {
        return Err(WebAuthnError::WrongType);
    }
    if client.challenge != encode_b64url(challenge) {
        return Err(WebAuthnError::ChallengeMismatch);
    }
    if !params.origins.iter().any(|o| o.as_str() == client.origin) {
        return Err(WebAuthnError::OriginNotAllowed);
    }
    match client.cross_origin {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => return Err(WebAuthnError::CrossOrigin),
        Some(_) => return Err(WebAuthnError::ClientDataInvalid),
    }
    if client.top_origin.is_some() {
        return Err(WebAuthnError::CrossOrigin);
    }

    let auth_data = assertion.authenticator_data.as_slice();
    if auth_data.len() < MIN_AUTH_DATA {
        return Err(WebAuthnError::AuthDataTooShort);
    }
    if auth_data.len() > MAX_AUTH_DATA {
        return Err(WebAuthnError::AuthDataTooLarge);
    }
    let (Some(rp_id_hash), Some(&flags), Some(counter)) = (
        auth_data.first_chunk::<32>(),
        auth_data.get(32),
        auth_data
            .get(33..37)
            .and_then(|c| <[u8; 4]>::try_from(c).ok()),
    ) else {
        return Err(WebAuthnError::AuthDataTooShort);
    };
    if *rp_id_hash != sha256(params.rp_id.as_str().as_bytes()) {
        return Err(WebAuthnError::RpIdHashMismatch);
    }
    if flags & FLAG_UP == 0 {
        return Err(WebAuthnError::UserNotPresent);
    }
    if flags & FLAG_UV == 0 {
        return Err(WebAuthnError::UserNotVerified);
    }
    if flags & FLAG_AT != 0 {
        return Err(WebAuthnError::AttestedDataPresent);
    }
    let has_extensions = flags & FLAG_ED != 0;
    if !has_extensions && auth_data.len() != MIN_AUTH_DATA {
        return Err(WebAuthnError::TrailingAuthData);
    }
    if has_extensions && auth_data.len() == MIN_AUTH_DATA {
        return Err(WebAuthnError::AuthDataTooShort);
    }
    let backup_eligible = flags & FLAG_BE != 0;
    let backup_state = flags & FLAG_BS != 0;
    if backup_state && !backup_eligible {
        return Err(WebAuthnError::InvalidBackupFlags);
    }
    if params.backup_policy == BackupPolicy::DeviceBound && backup_eligible {
        return Err(WebAuthnError::BackupNotAllowed);
    }

    let der = assertion.signature.as_slice();
    if !(MIN_DER_SIGNATURE..=MAX_DER_SIGNATURE).contains(&der.len()) {
        return Err(WebAuthnError::InvalidSignatureEncoding);
    }
    let mut signed = Vec::with_capacity(auth_data.len().saturating_add(32));
    signed.extend_from_slice(auth_data);
    signed.extend_from_slice(&sha256(client_data_json));
    verify_p256_der(&key, &signed, der).map_err(|e| match e {
        SignatureError::BadSignature => WebAuthnError::BadSignature,
        _ => WebAuthnError::InvalidSignatureEncoding,
    })?;
    Ok(AssertionInfo {
        sign_count: u32::from_be_bytes(counter),
        backup_eligible,
        backup_state,
    })
}

/// Accepts a counter that stays at zero or strictly increases.
pub fn check_sign_count(previous: u32, current: u32) -> Result<(), WebAuthnError> {
    if (previous == 0 && current == 0) || current > previous {
        Ok(())
    } else {
        Err(WebAuthnError::CounterRegression)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{self, ORIGIN, RP_ID};

    const SYNCED: u8 = FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS;
    const DEVICE: u8 = FLAG_UP | FLAG_UV;

    struct Case {
        spki: Vec<u8>,
        rp_id: RpId,
        origins: Vec<Origin>,
        backup_policy: BackupPolicy,
        challenge: [u8; 32],
        assertion: WebAuthnAssertion,
    }

    impl Case {
        /// A valid synced-passkey assertion over a fixed challenge.
        fn new() -> Self {
            Self::with(SYNCED, 0, b"", None)
        }

        fn with(flags: u8, counter: u32, extensions: &[u8], client_data: Option<Vec<u8>>) -> Self {
            let challenge = sha256(b"scheme=silvana.agent.policy.v1\n");
            let client_data =
                client_data.unwrap_or_else(|| testkit::client_data(&challenge, ORIGIN));
            let mut auth_data = testkit::auth_data(RP_ID, flags, counter);
            auth_data.extend_from_slice(extensions);
            let key = testkit::p256_key();
            let signature = testkit::sign_webauthn(&key, &auth_data, &client_data);
            Self {
                spki: testkit::p256_spki(&key),
                rp_id: RpId::parse(RP_ID).unwrap(),
                origins: vec![Origin::parse(ORIGIN).unwrap()],
                backup_policy: BackupPolicy::Any,
                challenge,
                assertion: WebAuthnAssertion {
                    authenticator_data: auth_data,
                    client_data_json: client_data,
                    signature,
                },
            }
        }

        fn with_client_data(json: &str) -> Self {
            Self::with(SYNCED, 0, b"", Some(json.as_bytes().to_vec()))
        }

        fn resign(mut self) -> Self {
            let key = testkit::p256_key();
            self.assertion.signature = testkit::sign_webauthn(
                &key,
                &self.assertion.authenticator_data,
                &self.assertion.client_data_json,
            );
            self
        }

        fn verify(&self) -> Result<AssertionInfo, WebAuthnError> {
            let params = WebAuthnParams {
                spki: &self.spki,
                rp_id: &self.rp_id,
                origins: &self.origins,
                backup_policy: self.backup_policy,
            };
            verify_assertion(&params, &self.challenge, &self.assertion)
        }
    }

    fn challenge_b64() -> String {
        encode_b64url(&Case::new().challenge)
    }

    fn json(extra: &str) -> String {
        format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{ORIGIN}"{extra}}}"#,
            challenge_b64()
        )
    }

    #[test]
    fn accepts_a_synced_passkey_with_counter_zero() {
        let info = Case::new().verify().unwrap();
        assert_eq!(
            info,
            AssertionInfo {
                sign_count: 0,
                backup_eligible: true,
                backup_state: true
            }
        );
    }

    #[test]
    fn accepts_a_device_bound_key_under_device_bound_policy() {
        let mut case = Case::with(DEVICE, 7, b"", None);
        case.backup_policy = BackupPolicy::DeviceBound;
        let info = case.verify().unwrap();
        assert_eq!(info.sign_count, 7);
        assert!(!info.backup_eligible && !info.backup_state);
    }

    #[test]
    fn accepts_a_high_s_signature() {
        let mut case = Case::new();
        let signature = ::p256::ecdsa::Signature::from_der(&case.assertion.signature).unwrap();
        let high = testkit::high_s(&signature);
        assert!(high.normalize_s().is_some());
        case.assertion.signature = high.to_der().as_bytes().to_vec();
        assert!(case.verify().is_ok());
    }

    #[test]
    fn accepts_extra_keys_absent_cross_origin_and_extensions() {
        let other =
            json(r#","other_keys_can_be_added_here":"x","tokenBinding":{"status":"present"}"#);
        assert!(Case::with_client_data(&other).verify().is_ok());
        assert!(Case::with_client_data(&json("")).verify().is_ok());
        assert!(
            Case::with_client_data(&json(r#","crossOrigin":false"#))
                .verify()
                .is_ok()
        );
        assert!(
            Case::with(SYNCED | FLAG_ED, 0, &[0xa0], None)
                .verify()
                .is_ok()
        );
    }

    #[test]
    fn invalid_public_key() {
        let mut case = Case::new();
        case.spki[90] ^= 0x01;
        assert_eq!(case.verify(), Err(WebAuthnError::InvalidPublicKey));
        case.spki.truncate(65);
        assert_eq!(case.verify(), Err(WebAuthnError::InvalidPublicKey));
    }

    #[test]
    fn client_data_too_large() {
        let padded = json(&format!(r#","pad":"{}""#, "x".repeat(2048)));
        assert_eq!(
            Case::with_client_data(&padded).verify(),
            Err(WebAuthnError::ClientDataTooLarge)
        );
    }

    #[test]
    fn client_data_invalid() {
        let duplicate = json(&format!(r#","challenge":"{}""#, challenge_b64()));
        let cases = [
            duplicate,
            json(r#","type":"webauthn.get""#),
            json(r#","crossOrigin":null"#),
            json(r#","crossOrigin":"false""#),
            "not json".to_owned(),
            format!("{} trailing", json("")),
            r#"{"type":"webauthn.get"}"#.to_owned(),
            r#"["webauthn.get"]"#.to_owned(),
            format!(r#"["webauthn.get","{}","{ORIGIN}"]"#, challenge_b64()),
        ];
        for text in &cases {
            assert_eq!(
                Case::with_client_data(text).verify(),
                Err(WebAuthnError::ClientDataInvalid),
                "{text}"
            );
        }
        let mut non_utf8 = json("").into_bytes();
        non_utf8.insert(1, 0xff);
        let case = Case::with(SYNCED, 0, b"", Some(non_utf8));
        assert_eq!(case.verify(), Err(WebAuthnError::ClientDataInvalid));
        let bom = format!("\u{feff}{}", json(""));
        assert_eq!(
            Case::with_client_data(&bom).verify(),
            Err(WebAuthnError::ClientDataInvalid)
        );
        let nested = |depth: usize| {
            json(&format!(
                r#","x":{}0{}"#,
                "[".repeat(depth),
                "]".repeat(depth)
            ))
        };
        assert!(Case::with_client_data(&nested(15)).verify().is_ok());
        assert_eq!(
            Case::with_client_data(&nested(16)).verify(),
            Err(WebAuthnError::ClientDataInvalid)
        );
        let in_string = json(&format!(r#","x":"{}\"{}""#, "[".repeat(40), "{".repeat(40)));
        assert!(Case::with_client_data(&in_string).verify().is_ok());
    }

    #[test]
    fn wrong_type() {
        let text = json("").replace("webauthn.get", "webauthn.create");
        assert_eq!(
            Case::with_client_data(&text).verify(),
            Err(WebAuthnError::WrongType)
        );
    }

    #[test]
    fn challenge_mismatch() {
        let b64 = challenge_b64();
        let padded = json("").replace(&b64, &format!("{b64}="));
        let standard = json("").replace(
            &b64,
            &base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD_NO_PAD,
                Case::new().challenge,
            ),
        );
        let other = json("").replace(&b64, &encode_b64url(&sha256(b"another payload")));
        let hex_form = json("").replace(&b64, &hex::encode(Case::new().challenge));
        for text in [padded, other, hex_form] {
            assert_eq!(
                Case::with_client_data(&text).verify(),
                Err(WebAuthnError::ChallengeMismatch),
                "{text}"
            );
        }
        if standard != json("") {
            assert_eq!(
                Case::with_client_data(&standard).verify(),
                Err(WebAuthnError::ChallengeMismatch)
            );
        }
    }

    #[test]
    fn origin_not_allowed() {
        for origin in [
            "https://APP.silvana.link",
            "https://app.silvana.link/",
            "https://silvana.link",
            "https://app.silvana.link:443",
        ] {
            let text = json("").replace(ORIGIN, origin);
            assert_eq!(
                Case::with_client_data(&text).verify(),
                Err(WebAuthnError::OriginNotAllowed),
                "{origin}"
            );
        }
    }

    #[test]
    fn cross_origin() {
        let cross = json(r#","crossOrigin":true"#);
        let top = json(r#","topOrigin":"https://app.silvana.link""#);
        let top_null = json(r#","crossOrigin":false,"topOrigin":null"#);
        for text in [cross, top, top_null] {
            assert_eq!(
                Case::with_client_data(&text).verify(),
                Err(WebAuthnError::CrossOrigin),
                "{text}"
            );
        }
    }

    #[test]
    fn auth_data_too_short() {
        let mut case = Case::new();
        case.assertion.authenticator_data.truncate(36);
        assert_eq!(case.resign().verify(), Err(WebAuthnError::AuthDataTooShort));
        let ed_without_data = Case::with(SYNCED | FLAG_ED, 0, b"", None);
        assert_eq!(
            ed_without_data.verify(),
            Err(WebAuthnError::AuthDataTooShort)
        );
    }

    #[test]
    fn auth_data_too_large() {
        let case = Case::with(SYNCED | FLAG_ED, 0, &[0u8; 988], None);
        assert_eq!(case.assertion.authenticator_data.len(), 1025);
        assert_eq!(case.verify(), Err(WebAuthnError::AuthDataTooLarge));
        assert!(
            Case::with(SYNCED | FLAG_ED, 0, &[0u8; 987], None)
                .verify()
                .is_ok()
        );
    }

    #[test]
    fn rp_id_hash_mismatch() {
        let mut case = Case::new();
        case.assertion.authenticator_data = testkit::auth_data("evil.example", SYNCED, 0);
        assert_eq!(case.resign().verify(), Err(WebAuthnError::RpIdHashMismatch));
    }

    #[test]
    fn user_not_present() {
        assert_eq!(
            Case::with(FLAG_UV, 0, b"", None).verify(),
            Err(WebAuthnError::UserNotPresent)
        );
    }

    #[test]
    fn user_not_verified() {
        assert_eq!(
            Case::with(FLAG_UP, 0, b"", None).verify(),
            Err(WebAuthnError::UserNotVerified)
        );
    }

    #[test]
    fn attested_data_present() {
        let case = Case::with(DEVICE | FLAG_AT, 0, &[0u8; 20], None);
        assert_eq!(case.verify(), Err(WebAuthnError::AttestedDataPresent));
    }

    #[test]
    fn trailing_auth_data() {
        assert_eq!(
            Case::with(SYNCED, 0, &[0xa0], None).verify(),
            Err(WebAuthnError::TrailingAuthData)
        );
    }

    #[test]
    fn invalid_backup_flags() {
        let case = Case::with(FLAG_UP | FLAG_UV | FLAG_BS, 0, b"", None);
        assert_eq!(case.verify(), Err(WebAuthnError::InvalidBackupFlags));
    }

    #[test]
    fn backup_not_allowed() {
        let mut case = Case::with(FLAG_UP | FLAG_UV | FLAG_BE, 3, b"", None);
        case.backup_policy = BackupPolicy::DeviceBound;
        assert_eq!(case.verify(), Err(WebAuthnError::BackupNotAllowed));
    }

    #[test]
    fn invalid_signature_encoding() {
        let mut case = Case::new();
        let good = case.assertion.signature.clone();
        for bad in [
            vec![0x30, 0x00],
            vec![0u8; 7],
            vec![0u8; 73],
            good[..good.len() - 1].to_vec(),
            [good.as_slice(), &[0]].concat(),
        ] {
            case.assertion.signature = bad;
            assert_eq!(case.verify(), Err(WebAuthnError::InvalidSignatureEncoding));
        }
    }

    #[test]
    fn bad_signature() {
        let mut case = Case::new();
        let other = ::p256::ecdsa::SigningKey::from_slice(&testkit::seed(0x61)).unwrap();
        case.assertion.signature = testkit::sign_webauthn(
            &other,
            &case.assertion.authenticator_data,
            &case.assertion.client_data_json,
        );
        assert_eq!(case.verify(), Err(WebAuthnError::BadSignature));
        let mut case = Case::new();
        let last = case.assertion.authenticator_data.len() - 1;
        case.assertion.authenticator_data[last] ^= 0x01;
        assert_eq!(case.verify(), Err(WebAuthnError::BadSignature));
    }

    #[test]
    fn counter_regression() {
        assert_eq!(check_sign_count(0, 0), Ok(()));
        assert_eq!(check_sign_count(5, 6), Ok(()));
        assert_eq!(check_sign_count(0, 1), Ok(()));
        assert_eq!(
            check_sign_count(6, 6),
            Err(WebAuthnError::CounterRegression)
        );
        assert_eq!(
            check_sign_count(6, 5),
            Err(WebAuthnError::CounterRegression)
        );
        assert_eq!(
            check_sign_count(6, 0),
            Err(WebAuthnError::CounterRegression)
        );
    }
}
