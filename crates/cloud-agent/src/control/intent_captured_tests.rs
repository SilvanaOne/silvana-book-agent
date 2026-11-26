//! The content check over the captured devnet records, with the devnet pins.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use prost::Message;
use tx_verifier::{VerifyMode, VerifySetting};

use super::*;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tx-verifier/tests/fixtures/devnet-20261124");

/// One captured record, as the content check reads it.
struct Record {
    id: String,
    party: String,
    path: String,
    hashing_scheme_version: String,
    server_hash_base64: String,
    prepared: Vec<u8>,
    request: Vec<u8>,
    expectation: OperationExpectation,
}

fn text(line: &serde_json::Value, key: &str) -> String {
    line.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn load(kind: &str) -> Vec<Record> {
    let path = format!("{FIXTURES}/{kind}.jsonl");
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let records: Vec<Record> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let line: serde_json::Value = serde_json::from_str(l).unwrap();
            let id = format!("{kind}/{}", text(&line, "fixture_id"));
            Record {
                party: text(&line, "party"),
                path: text(&line, "path"),
                hashing_scheme_version: text(&line, "hashing_scheme_version"),
                server_hash_base64: text(&line, "server_hash_base64"),
                prepared: BASE64.decode(text(&line, "prepared_transaction")).unwrap(),
                request: BASE64.decode(text(&line, "request")).unwrap(),
                expectation: with_recorded_terms(
                    serde_json::from_value(line["expectation"].clone())
                        .unwrap_or_else(|e| panic!("{id}: expectation: {e}")),
                    &line,
                ),
                id,
            }
        })
        .collect();
    assert!(!records.is_empty(), "{path} has no records");
    records
}

/// The expectation as the agent now builds it: an allocation with the terms it recorded
/// at propose or accept (the line's `recorded_terms`) and the local settlement operator,
/// a settle with the request's signed fees and the fee pin.
fn with_recorded_terms(mut expectation: OperationExpectation, line: &serde_json::Value) -> OperationExpectation {
    let pins: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/pins.json")).unwrap()).unwrap();
    match &mut expectation {
        OperationExpectation::Allocate { terms, settlement_operator, .. } => {
            *terms = Some(serde_json::from_value(line["recorded_terms"].clone()).unwrap());
            *settlement_operator = Some(text(&pins, "settlement_operator"));
        }
        OperationExpectation::AtomicDvpSettle { expected_fees, fee_party, .. } => {
            let bytes = BASE64.decode(text(line, "request")).unwrap();
            let req = orderbook_proto::rfqv2::PrepareAtomicTransactionRequest::decode(bytes.as_slice()).unwrap();
            let Some(AtomicParams::AtomicDvpSettle(params)) = req.params else { panic!("not a settle request") };
            *expected_fees = crate::atomic_swap::expected_fees(&params.lp_fees);
            *fee_party = Some(text(&pins, "party_fee"));
        }
        _ => {}
    }
    expectation
}

/// The pins of `pins.json`, as a linked agent's env sets them.
fn platform() -> PlatformParties {
    let path = format!("{FIXTURES}/pins.json");
    let pins: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    PlatformParties {
        fee: Some(text(&pins, "party_fee")),
        prepaid_traffic: Some(text(&pins, "party_prepaid_traffic")),
        traffic_fee: None,
    }
}

/// The verification and content check a linked agent runs before signing the record; a
/// directed record is checked against the command rebuilt from its canonical bytes.
fn content_check(r: &Record, platform: &PlatformParties) -> Result<(), ContentRefusal> {
    let linked = VerifyMode { strict_all: true, setting: VerifySetting::Default };
    let verification = tx_verifier::verify_and_hash_with(
        &r.prepared,
        &r.server_hash_base64,
        &r.hashing_scheme_version,
        &r.expectation,
        linked,
        false,
    )
    .unwrap();
    assert!(verification.accepted, "{}: verification refused: {:?}", r.id, verification.rejection_reason);
    let facts = tx_verifier::facts::extract_facts(&r.prepared, &r.party).map_err(|e| e.to_string());
    let check = |request: OwnRequest<'_>, directed: Option<&orderbook_proto::ledger::ControllerCommand>| match directed {
        Some(c) => {
            let command = Command::decode(&c.canonical).unwrap_or_else(|e| panic!("{}: command: {e:?}", r.id));
            check_before_signing_directed(&r.party, platform, &r.expectation, request, &verification, &facts, &command)
        }
        None => check_before_signing(&r.party, platform, &r.expectation, request, &verification, &facts),
    };
    match r.path.as_str() {
        "ledger" => {
            let req = orderbook_proto::ledger::PrepareTransactionRequest::decode(r.request.as_slice()).unwrap();
            check(OwnRequest::Ledger(req.params.as_ref()), req.controller_command.as_ref())
        }
        "atomic" => {
            let req = orderbook_proto::rfqv2::PrepareAtomicTransactionRequest::decode(r.request.as_slice()).unwrap();
            check(OwnRequest::Atomic(req.params.as_ref()), req.controller_command.as_ref())
        }
        other => panic!("{}: unknown path {other:?}", r.id),
    }
}

fn assert_passes(kind: &str) {
    let platform = platform();
    for r in load(kind) {
        assert_eq!(content_check(&r, &platform), Ok(()), "{}", r.id);
    }
}

#[test]
fn propose_dvp_passes() {
    assert_passes("propose_dvp");
}

#[test]
fn accept_dvp_passes() {
    assert_passes("accept_dvp");
}

#[test]
fn allocate_passes() {
    assert_passes("allocate");
}

#[test]
fn allocate_without_recorded_terms_is_refused() {
    let platform = platform();
    for mut r in load("allocate") {
        if let OperationExpectation::Allocate { terms, .. } = &mut r.expectation {
            *terms = None;
        }
        let result = content_check(&r, &platform);
        assert!(matches!(result, Err(ContentRefusal::BadRequest(_))), "{}: {result:?}", r.id);
    }
}

#[test]
fn an_allocation_settling_a_year_from_now_is_refused() {
    let platform = platform();
    let year = agent_logic::clock::now_utc().timestamp_micros() + 365 * 86_400_000_000;
    for r in load("allocate") {
        let linked = VerifyMode { strict_all: true, setting: VerifySetting::Default };
        let verification = tx_verifier::verify_and_hash_with(
            &r.prepared,
            &r.server_hash_base64,
            &r.hashing_scheme_version,
            &r.expectation,
            linked,
            false,
        )
        .unwrap();
        let mut facts = tx_verifier::facts::extract_facts(&r.prepared, &r.party).unwrap();
        facts.allocations.iter_mut().for_each(|a| a.settle_before_micros = year);
        let req = orderbook_proto::ledger::PrepareTransactionRequest::decode(r.request.as_slice()).unwrap();
        let request = OwnRequest::Ledger(req.params.as_ref());
        let result = check_before_signing(&r.party, &platform, &r.expectation, request, &verification, &Ok(facts));
        assert!(matches!(&result, Err(ContentRefusal::BadRequest(why)) if why.contains("72 h")), "{}: {result:?}", r.id);
    }
}

#[test]
fn unpinned_settles_and_top_ups_are_refused_only_for_the_pin() {
    for (kind, platform, pin) in [("atomic_dvp_settle", PlatformParties { fee: None, ..platform() }, "PARTY_FEE")] {
        for r in load(kind) {
            let result = content_check(&r, &platform);
            assert!(matches!(result, Err(ContentRefusal::PinMissing { pin: p, .. }) if p == pin), "{}: {result:?}", r.id);
        }
    }
}

#[test]
fn atomic_dvp_settle_passes() {
    assert_passes("atomic_dvp_settle");
}

#[test]
fn transfer_cc_passes() {
    assert_passes("transfer_cc");
}

#[test]
fn prepay_traffic_passes() {
    assert_passes("prepay_traffic");
}

#[test]
fn split_cc_passes() {
    assert_passes("split_cc");
}

#[test]
fn transfer_cip56_passes() {
    assert_passes("transfer_cip56");
}

#[test]
fn request_preapproval_passes() {
    assert_passes("request_preapproval");
}
