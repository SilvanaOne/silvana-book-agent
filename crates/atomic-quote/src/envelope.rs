//! The quote envelope (`silvana.atomic-dvp.envelope.v1`) — the payload the LP
//! hands to the user at confirm time — and the H14 client pre-check.
//!
//! Serde shapes match the reference file format produced/consumed by the
//! canton-agent dvp CLI (`orderbook atomic quote --out` / `atomic swap --file`),
//! so envelopes are interchangeable between the CLI harness and the agent.
//! Transport variants (the silvana.rfqv2.v1 proto AtomicQuoteEnvelope) convert
//! to/from these types; transport-only fields (rfq_id, lp_party_id, market_id)
//! are dropped on export.

use anyhow::{anyhow, bail, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

use crate::{build_canonical_message, verify_quote, CanonicalQuoteFields, LpFeeSpec, QuoteSide};

pub const ENVELOPE_VERSION: &str = "silvana.atomic-dvp.envelope.v1";

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AcsContractJson {
    pub contract_id: String,
    pub template_id: String,
    pub created_event_blob: String,
    pub payload: Value,
}

/// A Daml `InstrumentId` as Ledger-API JSON.
#[derive(Serialize, Deserialize, Clone)]
pub struct InstrumentIdJson {
    pub admin: String,
    pub id: String,
}

/// One LP-required fee — the Daml `FeeSpec` record as Ledger-API JSON
/// (fees-design rev 2). Part of the SIGNED quote (canonical message v2).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LpFeeJson {
    pub receiver: String,
    pub instrument_id: InstrumentIdJson,
    /// plain decimal string
    pub amount: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuoteJson {
    pub quote_id: String,
    pub ticket_id: String,
    pub user: String,
    /// DAML constructor name: "Buy" | "Sell"
    pub side: String,
    pub base_amount: String,
    pub quote_amount: String,
    pub created_at_micros: String,
    pub valid_until_micros: String,
    /// Quote.lpFees: None = no LP-required fees (v1 message, byte-identical to
    /// pre-fee envelopes); Some (non-empty) = v2 message. serde(default) keeps
    /// old envelope files parseable.
    #[serde(default)]
    pub lp_fees: Option<Vec<LpFeeJson>>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuoteEnvelope {
    pub version: String,
    pub synchronizer_id: String,
    pub dvp: AcsContractJson,
    pub quote: QuoteJson,
    pub canonical_message: String,
    /// ASN.1 DER, lowercase hex (secp256k1 over SHA256 of the canonical message)
    pub quote_signature: String,
    pub ticket: Option<AcsContractJson>,
    pub lp_input_holding_cids: Vec<String>,
    /// LP-side disclosures (venue + ticket + LP input holdings). NO transfer
    /// contexts here: factories/contexts are unsigned choice arguments, and the
    /// registry ties a context to the exact transfer (amount + holding cids) —
    /// so the USER resolves BOTH legs' contexts at swap time.
    pub disclosed: Vec<Value>,
}

/// Rebuild the canonical message exactly as the on-ledger choice does — from the
/// AtomicDVP contract payload (venue lines 2-8) + the quote (lines 9-16).
pub fn canonical_from_dvp(dvp_payload: &Value, quote: &QuoteJson) -> Result<String> {
    let s = |ptr: &str| {
        dvp_payload
            .pointer(ptr)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("AtomicDVP payload missing {ptr}"))
    };
    let lp_fees = match &quote.lp_fees {
        None => None,
        Some(fees) => Some(
            fees.iter()
                .map(|f| {
                    Ok(LpFeeSpec {
                        receiver: &f.receiver,
                        admin: &f.instrument_id.admin,
                        id: &f.instrument_id.id,
                        amount: Decimal::from_str(&f.amount)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
    };
    let fields = CanonicalQuoteFields {
        lp: s("/lp")?,
        operator: s("/operator")?,
        pair: s("/pairName")?,
        base_admin: s("/baseInstrumentId/admin")?,
        base_id: s("/baseInstrumentId/id")?,
        quote_admin: s("/quoteInstrumentId/admin")?,
        quote_instr_id: s("/quoteInstrumentId/id")?,
        user: &quote.user,
        side: QuoteSide::parse(&quote.side)?,
        base_amount: Decimal::from_str(&quote.base_amount)?,
        quote_amount: Decimal::from_str(&quote.quote_amount)?,
        quote_nonce: &quote.quote_id,
        created_at_micros: quote.created_at_micros.parse()?,
        valid_until_micros: quote.valid_until_micros.parse()?,
        ticket_id: &quote.ticket_id,
        lp_fees,
    };
    build_canonical_message(&fields)
}

/// H14 client pre-check: signature, message reconstruction, addressee, expiry,
/// ticket consistency, and disclosure completeness — before any submission.
///
/// Transport-agnostic: `now_micros` and the expected synchronizer are supplied
/// by the caller (no clock or config dependency here). `min_validity_micros`
/// is the safety margin the remaining window must exceed (0 = only "not yet
/// expired", the reference-CLI behavior; agents pass ~10 s).
pub fn pre_submit_check(
    envelope: &QuoteEnvelope,
    expected_user: &str,
    expected_synchronizer_id: &str,
    now_micros: i64,
    min_validity_micros: i64,
) -> Result<()> {
    if envelope.version != ENVELOPE_VERSION {
        bail!("unsupported envelope version: {}", envelope.version);
    }
    if envelope.synchronizer_id != expected_synchronizer_id {
        bail!(
            "envelope synchronizer {} != expected {}",
            envelope.synchronizer_id,
            expected_synchronizer_id
        );
    }
    let rebuilt = canonical_from_dvp(&envelope.dvp.payload, &envelope.quote)?;
    if rebuilt != envelope.canonical_message {
        bail!("H14: canonical message mismatch (envelope tampered or malformed)");
    }
    let venue_key = envelope
        .dvp
        .payload
        .get("quotePublicKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("AtomicDVP payload missing quotePublicKey"))?;
    if !verify_quote(&envelope.quote_signature, &rebuilt, venue_key) {
        bail!("H14: quote signature does not verify against the venue's quote key");
    }
    if envelope.quote.user != expected_user {
        bail!("H14: quote is for {} — not {}", envelope.quote.user, expected_user);
    }
    let valid_until: i64 = envelope.quote.valid_until_micros.parse()?;
    if now_micros + min_validity_micros >= valid_until {
        bail!(
            "quote expires at {valid_until} µs (now {now_micros} µs, required margin {min_validity_micros} µs)"
        );
    }
    match (&envelope.ticket, envelope.quote.ticket_id.as_str()) {
        (Some(_), "") => bail!("H14: ticketless quote must not carry a ticket"),
        (None, "") => {}
        (None, _) => bail!("H14: ticketed quote without its ticket contract"),
        (Some(t), tid) => {
            if t.payload.get("ticketId").and_then(|v| v.as_str()) != Some(tid) {
                bail!("H14: ticket contract does not match the signed ticketId");
            }
        }
    }
    let disclosed_cids: std::collections::HashSet<&str> = envelope
        .disclosed
        .iter()
        .filter_map(|d| d.get("contractId").and_then(|v| v.as_str()))
        .collect();
    for cid in &envelope.lp_input_holding_cids {
        if !disclosed_cids.contains(cid.as_str()) {
            bail!("H14: LP input holding {cid} is not in the disclosure package");
        }
    }
    if let Some(t) = &envelope.ticket {
        if !disclosed_cids.contains(t.contract_id.as_str()) {
            bail!("H14: the ticket is not in the disclosure package");
        }
    }
    if !disclosed_cids.contains(envelope.dvp.contract_id.as_str()) {
        bail!("H14: the AtomicDVP venue is not in the disclosure package");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_envelope() -> (QuoteEnvelope, crate::QuoteKeyFile) {
        let kf = crate::gen_keypair().unwrap();
        let dvp_payload = json!({
            "lp": "lp::1220aa",
            "operator": "op::1220bb",
            "pairName": "CC-USDC",
            "baseInstrumentId": {"admin": "dso::1220cc", "id": "Amulet"},
            "quoteInstrumentId": {"admin": "reg::1220dd", "id": "USDC"},
            "quotePublicKey": kf.pub_spki_hex,
        });
        let quote = QuoteJson {
            quote_id: "0198-quote".into(),
            ticket_id: "".into(),
            user: "user::1220ee".into(),
            side: "Buy".into(),
            base_amount: "5.0".into(),
            quote_amount: "25.0".into(),
            created_at_micros: "1000000".into(),
            valid_until_micros: "999999999999999".into(),
            lp_fees: None,
        };
        let canonical = canonical_from_dvp(&dvp_payload, &quote).unwrap();
        let sig = crate::sign_quote(&kf.priv_scalar_hex, &canonical).unwrap();
        let env = QuoteEnvelope {
            version: ENVELOPE_VERSION.into(),
            synchronizer_id: "sync::1".into(),
            dvp: AcsContractJson {
                contract_id: "00venue".into(),
                template_id: "#atomic-dvp-v1:AtomicDVP:AtomicDVP".into(),
                created_event_blob: "blob".into(),
                payload: dvp_payload,
            },
            quote,
            canonical_message: canonical,
            quote_signature: sig,
            ticket: None,
            lp_input_holding_cids: vec!["00lp1".into()],
            disclosed: vec![
                json!({"contractId": "00venue"}),
                json!({"contractId": "00lp1"}),
            ],
        };
        (env, kf)
    }

    #[test]
    fn pre_check_happy_and_tampered() {
        let (env, _kf) = sample_envelope();
        pre_submit_check(&env, "user::1220ee", "sync::1", 2_000_000, 0).unwrap();
        // wrong addressee
        assert!(pre_submit_check(&env, "other::1", "sync::1", 2_000_000, 0).is_err());
        // wrong synchronizer
        assert!(pre_submit_check(&env, "user::1220ee", "sync::2", 2_000_000, 0).is_err());
        // tampered amount breaks the canonical match
        let mut bad = env.clone();
        bad.quote.base_amount = "6.0".into();
        assert!(pre_submit_check(&bad, "user::1220ee", "sync::1", 2_000_000, 0).is_err());
        // undisclosed LP holding
        let mut bad2 = env.clone();
        bad2.lp_input_holding_cids.push("00hidden".into());
        assert!(pre_submit_check(&bad2, "user::1220ee", "sync::1", 2_000_000, 0).is_err());
        // ticket claimed but absent
        let mut bad3 = env.clone();
        bad3.quote.ticket_id = "t-1".into();
        assert!(pre_submit_check(&bad3, "user::1220ee", "sync::1", 2_000_000, 0).is_err());
    }
}
