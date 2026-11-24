//! Captured devnet records: loading, local hashing, inspection and facts.
#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use prost::Message;
use tx_verifier::decode::canton_proto::com::daml::ledger::api::v2::interactive::PreparedTransaction;
use tx_verifier::facts::{self, TxFacts};
use tx_verifier::{
    ExpectedDvpTerms, InspectionResult, OperationExpectation, VerificationResult, VerifyMode,
};

/// The directory of the captured records.
pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/devnet-20261124");

/// Records name no capture host: each `source` is a neutral label.
#[test]
fn fixture_sources_are_neutral_labels() {
    let neutral = |label: &str| {
        let rest = label.strip_prefix("linked-").or_else(|| {
            label.strip_prefix("fleet-").and_then(|r| r.strip_prefix(|c: char| c.is_ascii_lowercase())).and_then(|r| r.strip_prefix('-'))
        });
        rest.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let mut labels = std::collections::HashSet::new();
    for entry in std::fs::read_dir(FIXTURES).expect("fixtures directory") {
        let path = entry.expect("fixture entry").path();
        let text = std::fs::read_to_string(&path).expect("fixture text");
        // A network host name appears only as a party id hint.
        let outside_party_ids = text.match_indices("-devnet-").filter(|(at, _)| {
            let token = &text[*at..];
            !token[..token.find(|c: char| c == '"' || c.is_whitespace()).unwrap_or(token.len())].contains("::")
        });
        assert_eq!(outside_party_ids.count(), 0, "{}: names a capture host", path.display());
        if path.extension().is_some_and(|e| e == "jsonl") {
            for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                let raw: serde_json::Value = serde_json::from_str(line).expect("fixture line");
                let source = raw["source"].as_str().unwrap_or_default().to_string();
                assert!(neutral(&source), "{}:{}: source {source:?}", path.display(), i + 1);
                assert!(labels.insert(source.clone()), "{}:{}: source {source:?} repeats", path.display(), i + 1);
            }
        }
    }
    assert!(!labels.is_empty());
}

/// One captured record.
#[derive(Debug, Clone)]
pub struct Captured {
    pub fixture_id: String,
    pub source: String,
    pub kind: String,
    pub party: String,
    /// `ledger` or `atomic`.
    pub path: String,
    pub command_id: String,
    pub hashing_scheme_version: String,
    pub server_hash_hex: String,
    pub server_hash_base64: String,
    /// The `PreparedTransaction` bytes.
    pub prepared: Vec<u8>,
    /// The prepare request bytes, without signatures.
    pub request: Vec<u8>,
    pub expectation: OperationExpectation,
    /// Allocate records: the terms the same agent recorded at propose or accept.
    pub recorded_terms: Option<ExpectedDvpTerms>,
    /// The whole line.
    pub raw: serde_json::Value,
}

/// The platform parties pinned for the captured agents.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Pins {
    pub party_fee: String,
    pub party_prepaid_traffic: String,
    pub dso: String,
    pub settlement_operator: String,
}

fn text(line: &serde_json::Value, key: &str) -> String {
    line.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn bytes(line: &serde_json::Value, key: &str, at: &str) -> Vec<u8> {
    BASE64
        .decode(text(line, key))
        .unwrap_or_else(|e| panic!("{at}: {key} is not base64: {e}"))
}

/// Every record of `kind`, one per line of `<kind>.jsonl`.
pub fn load(kind: &str) -> Vec<Captured> {
    let path = format!("{FIXTURES}/{kind}.jsonl");
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let records: Vec<Captured> = body
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let at = format!("{kind}.jsonl:{}", i + 1);
            let raw: serde_json::Value =
                serde_json::from_str(l).unwrap_or_else(|e| panic!("{at}: {e}"));
            let expectation = serde_json::from_value(raw["expectation"].clone())
                .unwrap_or_else(|e| panic!("{at}: expectation: {e}"));
            let recorded_terms = match raw.get("recorded_terms") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value(v.clone())
                        .unwrap_or_else(|e| panic!("{at}: recorded_terms: {e}")),
                ),
            };
            Captured {
                fixture_id: text(&raw, "fixture_id"),
                source: text(&raw, "source"),
                kind: text(&raw, "kind"),
                party: text(&raw, "party"),
                path: text(&raw, "path"),
                command_id: text(&raw, "command_id"),
                hashing_scheme_version: text(&raw, "hashing_scheme_version"),
                server_hash_hex: text(&raw, "server_hash_hex"),
                server_hash_base64: text(&raw, "server_hash_base64"),
                prepared: bytes(&raw, "prepared_transaction", &at),
                request: bytes(&raw, "request", &at),
                expectation,
                recorded_terms,
                raw,
            }
        })
        .collect();
    assert!(!records.is_empty(), "{path} has no records");
    records
}

/// The pinned platform parties of `pins.json`.
pub fn pins() -> Pins {
    let path = format!("{FIXTURES}/pins.json");
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The hash the agent computes for the record's scheme version, in hex.
pub fn local_hash_hex(c: &Captured) -> String {
    let hash = tx_verifier::hasher::try_compute_hash(&c.prepared, &c.hashing_scheme_version)
        .unwrap_or_else(|e| panic!("{}: no local hash: {e}", c.fixture_id));
    hex::encode(hash)
}

/// The inspector's result for `bytes` against `expectation`.
pub fn inspect_bytes(bytes: &[u8], expectation: &OperationExpectation) -> (InspectionResult, Vec<String>) {
    tx_verifier::inspector::inspect_observing(bytes, expectation, false)
        .unwrap_or_else(|e| panic!("inspection error: {e:#}"))
}

/// The inspector's result for the record, with the checks only enforce would refuse.
pub fn inspect_observing(c: &Captured) -> (InspectionResult, Vec<String>) {
    inspect_bytes(&c.prepared, &c.expectation)
}

/// The inspector's result for the record.
pub fn inspect(c: &Captured) -> InspectionResult {
    inspect_observing(c).0
}

/// The whole verification of the record under `mode`, against its server hash.
pub fn verify(c: &Captured, mode: VerifyMode) -> VerificationResult {
    tx_verifier::verify_and_hash_with(
        &c.prepared,
        &c.server_hash_base64,
        &c.hashing_scheme_version,
        &c.expectation,
        mode,
        false,
    )
    .unwrap_or_else(|e| panic!("{}: verification error: {e:#}", c.fixture_id))
}

/// The verification an agent that is not linked runs (`VerifyMode::default()`).
pub fn verify_default(c: &Captured) -> VerificationResult {
    verify(c, VerifyMode::default())
}

/// The record's facts, as the agent's content check reads them.
pub fn facts(c: &Captured) -> TxFacts {
    facts::extract_facts(&c.prepared, &c.party)
        .unwrap_or_else(|e| panic!("{}: facts: {e}", c.fixture_id))
}

/// The record's transaction, decoded for a mutation.
pub fn prepared_transaction(c: &Captured) -> PreparedTransaction {
    PreparedTransaction::decode(c.prepared.as_slice())
        .unwrap_or_else(|e| panic!("{}: undecodable transaction: {e}", c.fixture_id))
}

/// The bytes of a (mutated) transaction.
pub fn encode(p: &PreparedTransaction) -> Vec<u8> {
    p.encode_to_vec()
}

/// Assertions 1 and 3 for every record of `kind`: the local hash is the server's, and
/// the facts model every template.
pub fn assert_hash_and_facts(kind: &str) {
    for c in load(kind) {
        assert_eq!(c.kind, kind, "{}", c.fixture_id);
        assert_eq!(local_hash_hex(&c), c.server_hash_hex, "{} ({})", c.fixture_id, c.source);
        let f = facts(&c);
        assert!(f.unmodelled.is_empty(), "{} ({}): unmodelled {:?}", c.fixture_id, c.source, f.unmodelled);
    }
}

/// Assertion 2 for every record of `kind`: the inspector accepts with no warning and
/// nothing that enforce would refuse.
pub fn assert_inspects_clean(kind: &str) {
    for c in load(kind) {
        let (r, observed) = inspect_observing(&c);
        assert!(r.accepted, "{} ({}): refused: {:?}", c.fixture_id, c.source, r.rejection_reason);
        assert!(r.warnings.is_empty(), "{} ({}): warnings {:?}", c.fixture_id, c.source, r.warnings);
        assert!(observed.is_empty(), "{} ({}): enforce would refuse {:?}", c.fixture_id, c.source, observed);
    }
}
