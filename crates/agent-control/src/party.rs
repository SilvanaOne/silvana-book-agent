//! Canton party ids `<hint>::1220<64 hex>` and the namespace of an Ed25519 key.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use crate::canon::values::is_lower_hex;
use crate::error::{ValueError, ValueKind};

/// Longest party hint, in bytes.
pub const MAX_PARTY_HINT_LEN: usize = 185;
const NAMESPACE_PREFIX: &str = "1220";
const FINGERPRINT_PURPOSE: [u8; 4] = [0x00, 0x00, 0x00, 0x0c];

/// `[A-Za-z0-9_-]{1,185}::1220[0-9a-f]{64}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartyId(String);

impl PartyId {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let err = ValueError(ValueKind::Party);
        let (hint, namespace) = s.split_once("::").ok_or(err)?;
        let hint_ok = (1..=MAX_PARTY_HINT_LEN).contains(&hint.len())
            && hint
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        let namespace_ok = namespace
            .strip_prefix(NAMESPACE_PREFIX)
            .is_some_and(|fp| fp.len() == 64 && is_lower_hex(fp));
        if hint_ok && namespace_ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(err)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn hint(&self) -> &str {
        self.0.split_once("::").map_or("", |(hint, _)| hint)
    }

    /// `1220` followed by the 64-hex key fingerprint.
    pub fn namespace(&self) -> &str {
        self.0
            .split_once("::")
            .map_or("", |(_, namespace)| namespace)
    }
}

impl fmt::Display for PartyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PartyId {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

/// `1220` + lowercase hex of SHA-256(00 00 00 0C || key).
pub fn namespace_for_key(key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_PURPOSE);
    hasher.update(key);
    format!("{NAMESPACE_PREFIX}{}", hex::encode(hasher.finalize()))
}

/// True when the party's namespace is exactly the namespace of `key`.
pub fn verify_party_key(party: &PartyId, key: &[u8; 32]) -> bool {
    party.namespace() == namespace_for_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    const NS: &str = "12200123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn party_grammar() {
        let hint185 = "h".repeat(185);
        let hint186 = "h".repeat(186);
        let accept = [
            format!("alice::{NS}"),
            format!("agent_1-x::{NS}"),
            format!("{hint185}::{NS}"),
        ];
        for s in &accept {
            let party = PartyId::parse(s).unwrap();
            assert_eq!(party.to_string(), *s);
            assert_eq!(party.namespace(), NS);
        }
        let refuse = [
            String::new(),
            NS.to_owned(),
            format!("::{NS}"),
            format!("alice:{NS}"),
            format!("alice::{}", NS.to_uppercase()),
            format!("alice::1221{}", &NS[4..]),
            format!("alice::{}", &NS[..67]),
            format!("alice::{NS}0"),
            format!("al ice::{NS}"),
            format!("alice::{NS} "),
            format!("al.ice::{NS}"),
            format!("a::b::{NS}"),
            format!("{hint186}::{NS}"),
            format!("caf\u{e9}::{NS}"),
        ];
        for s in &refuse {
            assert_eq!(
                PartyId::parse(s),
                Err(ValueError(ValueKind::Party)),
                "{s:?}"
            );
        }
        assert_eq!(PartyId::parse(&accept[0]).unwrap().hint(), "alice");
    }

    #[test]
    fn namespace_is_the_canton_fingerprint_of_the_key() {
        let key = testkit::agent_key().verifying_key().to_bytes();
        let mut preimage = vec![0u8, 0, 0, 12];
        preimage.extend_from_slice(&key);
        let expected = format!("1220{}", hex::encode(Sha256::digest(&preimage)));
        assert_eq!(namespace_for_key(&key), expected);
        let party = testkit::agent_party();
        assert!(verify_party_key(&party, &key));
        let other = testkit::ed_key(0x21).verifying_key().to_bytes();
        assert!(!verify_party_key(&party, &other));
    }
}
