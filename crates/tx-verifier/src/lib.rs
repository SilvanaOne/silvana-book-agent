//! Transaction verification for Canton interactive submissions
//!
//! Verifies `PreparedTransaction` content and recomputes its hash before signing.
//!
//! - The inspector checks the fields of most operations against the expectation;
//!   [`OperationExpectation::coverage`] names the operations it does not check yet
//! - Hasher computes the full 3-layer SHA-256 hash, and [`HashCheck`] records how it
//!   compared with the server hash
//! - By default a mismatched or uncomputed hash falls back to the server hash
//!   (`computed_hash` is all zero); [`VerifyMode::strict_all`] refuses it instead
//!
//! USDCx bridge operations and AcceptDvp are always strict: they are inspected fail-closed and
//! signed only with a matching hash ([`OperationExpectation::requires_hash_match`]).
//! [`facts`] reads the value flows of a whole transaction.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

mod decimal;
pub mod decode;
pub mod facts;
pub mod hasher;
pub mod inspector;
#[cfg(test)]
mod test_support;
mod text;
pub mod types;
pub mod usdcx;
mod usdcx_inspect;

pub use types::{Coverage, HashCheck, InspectionResult, OperationExpectation, VerificationResult};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use tracing::{debug, error};

/// How strictly the hash comparison is enforced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VerifyMode {
    /// Refuse a mismatched or uncomputed hash for every operation, not only strict ones.
    pub strict_all: bool,
}

/// Verify a prepared transaction and compute its hash.
///
/// This is the main entry point called by agents before signing.
///
/// # Arguments
/// - `prepared_transaction_bytes`: Raw `PreparedTransaction` protobuf from server
/// - `server_hash_base64`: Server-provided hash (for comparison only, NEVER trusted)
/// - `hashing_scheme_version`: `"V2"` or `"V3"`
/// - `expectation`: What the agent expects this transaction to do
///
/// # Returns
/// `VerificationResult` with the self-computed hash to sign.
///
/// # Behavior
/// - The inspector checks the transaction against `expectation` and refuses a mismatch
/// - Hasher computes the full 3-layer SHA-256 hash
/// - On hash match: returns computed hash (caller signs it — identical to server hash)
/// - On hash mismatch: returns `[0u8; 32]` sentinel (caller falls back to server hash),
///   except that an operation with `requires_hash_match` is refused
pub fn verify_and_hash(
    prepared_transaction_bytes: &[u8],
    server_hash_base64: &str,
    hashing_scheme_version: &str,
    expectation: &OperationExpectation,
    verbose: bool,
) -> Result<VerificationResult> {
    verify_and_hash_with(
        prepared_transaction_bytes,
        server_hash_base64,
        hashing_scheme_version,
        expectation,
        VerifyMode::default(),
        verbose,
    )
}

/// [`verify_and_hash`] with an explicit [`VerifyMode`]; under `strict_all` every
/// mismatched, undecodable or uncomputed hash refuses the transaction.
pub fn verify_and_hash_with(
    prepared_transaction_bytes: &[u8],
    server_hash_base64: &str,
    hashing_scheme_version: &str,
    expectation: &OperationExpectation,
    mode: VerifyMode,
    verbose: bool,
) -> Result<VerificationResult> {
    let coverage = expectation.coverage();
    let mut warnings = Vec::new();

    // Step 1: Inspect transaction fields against expectation
    let (inspection, burn_holding_cids) =
        match usdcx_inspect::inspect(prepared_transaction_bytes, expectation)? {
            Some(usdcx) => {
                if verbose {
                    inspector::log_verbose(prepared_transaction_bytes, expectation);
                }
                (usdcx.result, usdcx.burn_holding_cids)
            }
            None => (
                inspector::inspect(prepared_transaction_bytes, expectation, verbose)?,
                Vec::new(),
            ),
        };
    if !inspection.accepted {
        return Ok(VerificationResult {
            accepted: false,
            computed_hash: [0u8; 32],
            summary: inspection.summary,
            warnings: inspection.warnings,
            rejection_reason: inspection.rejection_reason,
            burn_holding_cids: Vec::new(),
            hash_check: HashCheck::Uncomputed {
                reason: "the inspection refused the transaction".to_string(),
            },
            coverage,
        });
    }
    warnings.extend(inspection.warnings);
    let strict = mode.strict_all || expectation.requires_hash_match();

    // Step 2: Compute hash from PreparedTransaction (a decode failure is an error)
    let computed = match hasher::decode_and_hash(prepared_transaction_bytes, hashing_scheme_version)? {
        Ok(hash) if hash != [0u8; 32] => Ok(hash),
        Ok(_) => Err("the computed hash is all zero".to_string()),
        Err(e) => {
            error!("TX HASH computation failed: {:#}", e);
            Err(format!("{e:#}"))
        }
    };

    // Step 3: Compare with the server hash, which must be exactly 32 bytes
    let computed_hash = match computed {
        Ok(hash) => hash,
        Err(reason) => {
            let hash_check = HashCheck::Uncomputed { reason };
            if strict {
                return Ok(VerificationResult {
                    accepted: false,
                    computed_hash: [0u8; 32],
                    summary: format!("{}\nHash computation failed.", inspection.summary),
                    warnings,
                    rejection_reason: Some(
                        "The transaction hash could not be computed; this operation is signed only \
                         with a matching hash"
                            .to_string(),
                    ),
                    burn_holding_cids: Vec::new(),
                    hash_check,
                    coverage,
                });
            }
            // Fall back to the server hash
            return Ok(VerificationResult {
                accepted: true,
                computed_hash: [0u8; 32],
                summary: format!(
                    "{}\nHash computation failed — using server hash.",
                    inspection.summary
                ),
                warnings,
                rejection_reason: None,
                burn_holding_cids,
                hash_check,
                coverage,
            });
        }
    };
    let server_hash_bytes = match BASE64.decode(server_hash_base64) {
        Ok(bytes) => Some(bytes),
        Err(e) if !mode.strict_all => {
            return Err(anyhow::anyhow!("Failed to decode server hash: {}", e));
        }
        Err(_) => None,
    };
    let server_hex = server_hash_bytes.as_deref().map(hex::encode).unwrap_or_default();
    let computed_hex = hex::encode(computed_hash);

    if server_hash_bytes.as_deref() == Some(computed_hash.as_slice()) {
        debug!("TX HASH MATCH: {}", computed_hex);
        return Ok(VerificationResult {
            accepted: true,
            computed_hash,
            summary: format!("{}\nHash verified — MATCH.", inspection.summary),
            warnings,
            rejection_reason: None,
            burn_holding_cids,
            hash_check: HashCheck::Match,
            coverage,
        });
    }
    let server_shown = if server_hash_bytes.is_some() { server_hex.as_str() } else { "(not base64)" };
    let rejection = format!("Hash mismatch: server={}, computed={}", server_shown, computed_hex);
    if !strict {
        debug!(
            "TX HASH MISMATCH: server={}, computed={} — using server hash (debug mode)",
            server_shown, computed_hex,
        );
    }
    let hash_check = HashCheck::Mismatch {
        server_hex,
        computed_hex,
    };
    if strict {
        return Ok(VerificationResult {
            accepted: false,
            computed_hash: [0u8; 32],
            summary: format!("{}\nHash MISMATCH.", inspection.summary),
            warnings,
            rejection_reason: Some(format!(
                "{rejection}; this operation is signed only with a matching hash"
            )),
            burn_holding_cids: Vec::new(),
            hash_check,
            coverage,
        });
    }
    warnings.push(rejection);
    // Return sentinel so caller falls back to server hash (safe)
    Ok(VerificationResult {
        accepted: true,
        computed_hash: [0u8; 32],
        summary: format!(
            "{}\nHash MISMATCH (debug mode — using server hash).",
            inspection.summary
        ),
        warnings,
        rejection_reason: None,
        burn_holding_cids,
        hash_check,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::canton_proto::com::daml::ledger::api::v2::interactive::PreparedTransaction;
    use crate::test_support::*;
    use std::collections::BTreeSet;

    const P: &str = "alice::1220aa";
    const Q: &str = "bob::1220bb";
    const STRICT: VerifyMode = VerifyMode { strict_all: true };

    fn transfer_cc() -> (PreparedTransaction, OperationExpectation) {
        let mut ex = exercise("TransferFactory_Transfer", &[P], vec![]);
        ex.template_id = Some(ident("Splice.ExternalPartyAmuletRules", "ExternalPartyAmuletRules"));
        ex.chosen_value = Some(record(vec![(
            "transfer",
            record(vec![("sender", party(P)), ("receiver", party(Q)), ("amount", numeric("1.0"))]),
        )]));
        let p = prepared(&["0"], vec![exercise_node("0", ex)], vec![seed(0, 1)], &[P]);
        let exp = OperationExpectation::TransferCc {
            sender_party: P.into(),
            receiver_party: Q.into(),
            amount: "1".into(),
            command_id: "cmd".into(),
        };
        (p, exp)
    }

    fn split_cc() -> (PreparedTransaction, OperationExpectation) {
        let p = prepared(&["0"], vec![exercise_node("0", exercise("C", &[P], vec![]))], vec![seed(0, 1)], &[P]);
        (p, OperationExpectation::SplitCc { party: P.into(), output_amounts: vec!["1".into()] })
    }

    fn unseeded(mut p: PreparedTransaction) -> PreparedTransaction {
        p.transaction.as_mut().unwrap().node_seeds.clear();
        p
    }

    fn verify(bytes: &[u8], server: &[u8], exp: &OperationExpectation, mode: VerifyMode) -> VerificationResult {
        verify_and_hash_with(bytes, &BASE64.encode(server), "V2", exp, mode, false).unwrap()
    }

    fn mismatch(server: &[u8], computed: [u8; 32]) -> HashCheck {
        HashCheck::Mismatch { server_hex: hex::encode(server), computed_hex: hex::encode(computed) }
    }

    #[test]
    fn both_modes_sign_a_matching_hash() {
        for (p, exp) in [transfer_cc(), split_cc()] {
            let bytes = encode(&p);
            let h = hasher::try_compute_hash(&bytes, "V2").unwrap();
            for mode in [VerifyMode::default(), STRICT] {
                let r = verify(&bytes, &h, &exp, mode);
                assert!(r.accepted, "{r:?}");
                assert_eq!((r.computed_hash, r.hash_check), (h, HashCheck::Match));
            }
            assert_eq!(verify_and_hash(&bytes, &BASE64.encode(h), "V2", &exp, false).unwrap().hash_check, HashCheck::Match);
        }
    }

    #[test]
    fn strict_all_refuses_a_mismatch_for_a_non_strict_operation() {
        for (p, exp) in [transfer_cc(), split_cc()] {
            assert!(!exp.requires_hash_match());
            let bytes = encode(&p);
            let h = hasher::try_compute_hash(&bytes, "V2").unwrap();
            let r = verify(&bytes, &[7u8; 32], &exp, STRICT);
            assert!(!r.accepted, "{exp:?}");
            assert_eq!(r.computed_hash, [0u8; 32]);
            assert_eq!(r.hash_check, mismatch(&[7u8; 32], h));
            assert!(r.rejection_reason.unwrap().contains("Hash mismatch"));
        }
    }

    #[test]
    fn strict_all_refuses_an_uncomputable_hash() {
        for (p, exp) in [transfer_cc(), split_cc()] {
            let bytes = encode(&unseeded(p));
            assert!(hasher::try_compute_hash(&bytes, "V2").is_err());
            let r = verify(&bytes, &[7u8; 32], &exp, STRICT);
            assert!(!r.accepted, "{exp:?}");
            assert!(matches!(r.hash_check, HashCheck::Uncomputed { .. }), "{:?}", r.hash_check);
            assert!(r.rejection_reason.unwrap().contains("could not be computed"));
        }
    }

    #[test]
    fn default_mode_keeps_the_explicit_fallback() {
        for (p, exp) in [transfer_cc(), split_cc()] {
            let bytes = encode(&p);
            let h = hasher::try_compute_hash(&bytes, "V2").unwrap();
            for r in [
                verify(&bytes, &[7u8; 32], &exp, VerifyMode::default()),
                verify_and_hash(&bytes, &BASE64.encode([7u8; 32]), "V2", &exp, false).unwrap(),
            ] {
                assert!(r.accepted, "{r:?}");
                assert_eq!(r.computed_hash, [0u8; 32]);
                assert_eq!(r.hash_check, mismatch(&[7u8; 32], h));
                assert!(r.warnings.iter().any(|w| w.starts_with("Hash mismatch")), "{:?}", r.warnings);
            }

            let r = verify(&encode(&unseeded(p)), &h, &exp, VerifyMode::default());
            assert!(r.accepted, "{r:?}");
            assert_eq!(r.computed_hash, [0u8; 32]);
            assert!(matches!(r.hash_check, HashCheck::Uncomputed { .. }), "{:?}", r.hash_check);
            assert!(r.summary.ends_with("Hash computation failed — using server hash."), "{}", r.summary);
        }
    }

    #[test]
    fn server_hash_of_wrong_length_is_a_mismatch() {
        let (p, exp) = transfer_cc();
        let bytes = encode(&p);
        let h = hasher::try_compute_hash(&bytes, "V2").unwrap();
        let short = h[..31].to_vec();
        let long = [h.as_slice(), &[0u8]].concat();
        for server in [short, long, Vec::new()] {
            let r = verify(&bytes, &server, &exp, VerifyMode::default());
            assert!(r.accepted);
            assert_eq!((r.computed_hash, r.hash_check), ([0u8; 32], mismatch(&server, h)));
            let r = verify(&bytes, &server, &exp, STRICT);
            assert!(!r.accepted, "{} bytes", server.len());
            assert_eq!(r.hash_check, mismatch(&server, h));
        }
    }

    #[test]
    fn a_server_hash_that_is_not_base64_is_an_error_by_default_and_a_mismatch_under_strict_all() {
        let (p, exp) = transfer_cc();
        let bytes = encode(&p);
        let err = verify_and_hash_with(&bytes, "not base64!", "V2", &exp, VerifyMode::default(), false)
            .unwrap_err();
        assert!(err.to_string().starts_with("Failed to decode server hash"), "{err}");
        let r = verify_and_hash_with(&bytes, "not base64!", "V2", &exp, STRICT, false).unwrap();
        assert!(!r.accepted);
        assert!(
            matches!(&r.hash_check, HashCheck::Mismatch { server_hex, .. } if server_hex.is_empty()),
            "{:?}",
            r.hash_check
        );
    }

    #[test]
    fn an_inspection_refusal_records_no_hash() {
        let (p, _) = transfer_cc();
        let wrong = OperationExpectation::TransferCc {
            sender_party: P.into(),
            receiver_party: Q.into(),
            amount: "2".into(),
            command_id: "cmd".into(),
        };
        let r = verify(&encode(&p), &[7u8; 32], &wrong, VerifyMode::default());
        assert!(!r.accepted);
        assert!(matches!(r.hash_check, HashCheck::Uncomputed { .. }));
        assert_eq!(r.coverage, Coverage::Fields);
    }

    /// One index per variant; the match is exhaustive, so a new variant needs a new entry.
    fn variant(e: &OperationExpectation) -> usize {
        use OperationExpectation::*;
        match e {
            PayFee { .. } => 0,
            ProposeDvp { .. } => 1,
            AcceptDvp { .. } => 2,
            Allocate { .. } => 3,
            CancelDvpProposal { .. } => 4,
            RejectDvpProposal { .. } => 5,
            TransferCc { .. } => 6,
            PrepayTraffic { .. } => 7,
            RequestPreapproval { .. } => 8,
            RequestRecurringPrepaid { .. } => 9,
            RequestRecurringPayasyougo { .. } => 10,
            RequestUserService { .. } => 11,
            TransferCip56 { .. } => 12,
            AcceptCip56 { .. } => 13,
            SplitCc { .. } => 14,
            ExecuteMulticall { .. } => 15,
            LockHoldings { .. } => 16,
            ProcessLockUnlockRequests { .. } => 17,
            ResizeLock { .. } => 18,
            TerminateLock { .. } => 19,
            AtomicDvpSettle { .. } => 20,
            IssueTickets { .. } => 21,
            SplitHoldings { .. } => 22,
            CreateAtomicDvpVenue { .. } => 23,
            UpdateVenueKey { .. } => 24,
            RetireVenue { .. } => 25,
            CancelTickets { .. } => 26,
            UsdcxRequestAgreement { .. } => 27,
            UsdcxMint { .. } => 28,
            UsdcxBurn { .. } => 29,
            UsdcxReclaimHolding { .. } => 30,
            UsdcxUpdatePreApproval { .. } => 31,
            UsdcxTerminateAgreement { .. } => 32,
        }
    }
    const VARIANTS: usize = 33;

    fn every_expectation() -> Vec<OperationExpectation> {
        use OperationExpectation::*;
        let s = || String::from("x");
        let net = || String::from("mainnet");
        vec![
            PayFee { sender_party: P.into(), fee_party: Q.into(), proposal_id: s(), fee_type: "dvp".into() },
            ProposeDvp { buyer_party: P.into(), seller_party: Q.into(), proposal_id: s(), synchronizer_id: s() },
            AcceptDvp {
                seller_party: P.into(),
                proposal_id: s(),
                dvp_proposal_cid: s(),
                expected_delivery_amount: "1".into(),
                expected_payment_amount: "1".into(),
                expected_delivery_instrument_id: s(),
                expected_delivery_instrument_admin: s(),
                expected_payment_instrument_id: s(),
                expected_payment_instrument_admin: s(),
            },
            Allocate { party: P.into(), proposal_id: s(), dvp_cid: s() },
            CancelDvpProposal { party: P.into(), dvp_proposal_cid: s() },
            RejectDvpProposal { party: P.into(), dvp_proposal_cid: s() },
            TransferCc { sender_party: P.into(), receiver_party: Q.into(), amount: "1".into(), command_id: s() },
            PrepayTraffic { sender_party: P.into(), amount: "1".into(), command_id: s() },
            RequestPreapproval { party: P.into() },
            RequestRecurringPrepaid { party: P.into(), app_party: Q.into(), amount: "1".into() },
            RequestRecurringPayasyougo { party: P.into(), app_party: Q.into(), amount: "1".into() },
            RequestUserService { party: P.into() },
            TransferCip56 {
                sender_party: P.into(),
                receiver_party: Q.into(),
                instrument_id: s(),
                instrument_admin: s(),
                amount: "1".into(),
            },
            AcceptCip56 { receiver_party: P.into(), contract_id: s() },
            SplitCc { party: P.into(), output_amounts: vec!["1".into()] },
            ExecuteMulticall { party: P.into(), op_count: 1 },
            LockHoldings { party: P.into(), amount: "1".into(), instrument_id: s(), context: s() },
            ProcessLockUnlockRequests { party: P.into(), request_count: 1 },
            ResizeLock { party: P.into(), new_amount: "1".into() },
            TerminateLock { party: P.into() },
            AtomicDvpSettle {
                user_party: P.into(),
                venue_cid: s(),
                template_id: s(),
                quote_id: s(),
                ticket_id: s(),
                ticket_cid: None,
                side: "Buy".into(),
                base_amount: "1".into(),
                quote_amount: "1".into(),
                lp_party: Q.into(),
                base_instrument_id: s(),
                base_instrument_admin: s(),
                quote_instrument_id: s(),
                quote_instrument_admin: s(),
                valid_until_micros: 0,
                lp_input_holding_cids: vec![],
                user_input_holding_cids: vec![],
            },
            IssueTickets { lp_party: P.into(), ticket_count: 1 },
            SplitHoldings { lp_party: P.into(), instrument_id: s(), split_count: 1, input_cids: vec![] },
            CreateAtomicDvpVenue { lp_party: P.into(), pair_name: s(), quote_public_key_spki_hex: s() },
            UpdateVenueKey { lp_party: P.into(), venue_cid: s(), new_key_spki_hex: s() },
            RetireVenue { lp_party: P.into(), venue_cid: s() },
            CancelTickets { lp_party: P.into(), ticket_count: 1 },
            UsdcxRequestAgreement { party: P.into(), network: net(), pre_approval: false, command_id: s() },
            UsdcxMint { party: P.into(), network: net(), deposit_attestation_cid: s(), command_id: s() },
            UsdcxBurn {
                party: P.into(),
                network: net(),
                amount: "1".into(),
                destination_recipient: s(),
                request_id: s(),
                command_id: s(),
            },
            UsdcxReclaimHolding { party: P.into(), network: net(), revoked_withdraw_attestation_cid: s(), command_id: s() },
            UsdcxUpdatePreApproval { party: P.into(), network: net(), pre_approval: true },
            UsdcxTerminateAgreement { party: P.into(), network: net(), reason: s() },
        ]
    }

    #[test]
    fn coverage_agrees_with_the_inspector() {
        let all = every_expectation();
        let indices: BTreeSet<usize> = all.iter().map(variant).collect();
        assert_eq!((all.len(), indices.len()), (VARIANTS, VARIANTS), "one expectation per variant");
        let (p, _) = split_cc();
        let bytes = encode(&p);
        let mut unverified = 0;
        for exp in &all {
            let stub = match inspector::inspect(&bytes, exp, false) {
                Ok(r) => r.summary.starts_with("[STUB]"),
                Err(_) => false,
            };
            assert_eq!(stub, exp.coverage() == Coverage::Unverified, "{exp:?}");
            if stub {
                unverified += 1;
            }
            let r = verify_and_hash_with(&bytes, "", "V2", exp, VerifyMode::default(), false);
            if let Ok(r) = r {
                assert_eq!(r.coverage, exp.coverage(), "{exp:?}");
            }
        }
        assert_eq!(unverified, 15);
    }
}
