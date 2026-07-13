//! AtomicDVP canonical quote message + secp256k1 quote-key signing.
//!
//! Byte-exact Rust replica of what the on-ledger choice reconstructs
//! (`canonicalQuoteMessage` in atomic-dvp-v2) and what `DA.Crypto.Text.secp256k1`
//! verifies: ECDSA/secp256k1 over SHA256(utf8(canonical_message)), ASN.1 DER
//! signature (lowercase hex), X.509 SPKI public key with an uncompressed point
//! (lowercase hex). Spec: atomic-dvp/plans/atomic-dvp-design.md §5 +
//! atomic-dvp-fa-design.md §4 (v3/v4: line 3 is `provider=`); reference
//! implementations: the DAML module and tests/.../helpers/quoteSigner.ts.
//! Proven against the committed cross-language golden vectors (see tests below).
//!
//! Scheme id: `secp256k1-sha256-v1` (parallel to message-signing's
//! `ed25519-sha256-v1`). This crate is shared by the LP agent (signs), the
//! user agent (H14 pre-check), canton-agent's dvp test harness, and the
//! enterprise-canton-sdk ledger-service (fail-fast re-verification).

use anyhow::{anyhow, bail, Result};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub mod envelope;

pub const MSG_TYPE: &str = "silvana.atomic-dvp.quote.v3";

/// v4 = v3 + the LP-required fee block (atomic-dvp fees-design §3.6). Selected
/// automatically by `lp_fees` presence; the two formats differ at line 1, so no
/// byte string verifies under both (domain separation).
pub const MSG_TYPE_V4: &str = "silvana.atomic-dvp.quote.v4";

/// Signing-scheme identifier for the quote signature.
pub const SIGNING_SCHEME: &str = "secp256k1-sha256-v1";

/// X.509 SubjectPublicKeyInfo prefix for an uncompressed secp256k1 point:
/// SEQUENCE { SEQUENCE { OID id-ecPublicKey, OID secp256k1 }, BIT STRING 00 || 04||X||Y }.
/// This is the format `AtomicDVP.quotePublicKey` stores.
pub const SPKI_PREFIX_SECP256K1_UNCOMPRESSED: &str =
    "3056301006072a8648ce3d020106052b8104000a034200";

/// PKCS8 EC private-key prefix (as emitted by daml-script's `secp256k1generatekeypair`;
/// the raw 32-byte scalar follows). Only needed to extract test-vector scalars.
#[cfg(test)]
pub const PKCS8_PREFIX_SECP256K1: &str =
    "30818d020100301006072a8648ce3d020106052b8104000a047630740201010420";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuoteSide {
    Buy,
    Sell,
}

impl QuoteSide {
    /// Rendering inside the signed canonical message (line 10).
    pub fn canonical(&self) -> &'static str {
        match self {
            QuoteSide::Buy => "BUY",
            QuoteSide::Sell => "SELL",
        }
    }

    /// DAML constructor name — the Ledger API JSON enum value.
    pub fn daml(&self) -> &'static str {
        match self {
            QuoteSide::Buy => "Buy",
            QuoteSide::Sell => "Sell",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "Buy" | "buy" | "BUY" => Ok(QuoteSide::Buy),
            "Sell" | "sell" | "SELL" => Ok(QuoteSide::Sell),
            other => bail!("invalid side: {other} (expected buy|sell)"),
        }
    }
}

/// One LP-required fee — the SIGNED economic triple (receiver, instrument,
/// amount) of atomic-dvp `FeeSpec` (fees-design §3.1). Factory cids/contexts
/// are unsignable and travel outside the payload.
pub struct LpFeeSpec<'a> {
    pub receiver: &'a str,
    /// fee InstrumentId.admin
    pub admin: &'a str,
    /// fee InstrumentId.id (verbatim; no newline)
    pub id: &'a str,
    pub amount: Decimal,
}

/// The 16 semantic values of the canonical message. NOTE the two overloaded
/// names: line 8 `quote_id=` is the quote-INSTRUMENT id (`quote_instr_id`);
/// line 13 `quote_nonce=` is the Quote.quoteId nonce (`quote_nonce`).
pub struct CanonicalQuoteFields<'a> {
    pub lp: &'a str,
    pub provider: &'a str,
    pub pair: &'a str,
    pub base_admin: &'a str,
    pub base_id: &'a str,
    pub quote_admin: &'a str,
    pub quote_instr_id: &'a str,
    pub user: &'a str,
    pub side: QuoteSide,
    pub base_amount: Decimal,
    pub quote_amount: Decimal,
    pub quote_nonce: &'a str,
    pub created_at_micros: i64,
    pub valid_until_micros: i64,
    /// "" = ticketless; the line is emitted regardless (never omitted).
    pub ticket_id: &'a str,
    /// LP-required fees (Quote.lpFees, fees-design rev 2). None -> v3 message;
    /// Some (must be non-empty) -> v4 message with the fee block. NEVER pass
    /// Some(vec![]) — Some [] has no encoding.
    pub lp_fees: Option<Vec<LpFeeSpec<'a>>>,
}

/// Render a decimal exactly like DAML's `show @Decimal` (Numeric.toUnscaledString):
/// plain notation, all trailing fractional zeros stripped, integral values as
/// `X.0`, zero as `0.0`. Errors (never rounds) on more than 10 decimal places.
pub fn render_decimal(v: Decimal) -> Result<String> {
    let n = v.normalize();
    if n.scale() > 10 {
        bail!("amount has more than 10 decimal places: {v}");
    }
    if n.is_zero() {
        return Ok("0.0".to_string());
    }
    let s = n.to_string();
    Ok(if s.contains('.') { s } else { format!("{s}.0") })
}

/// The signed payload (design §5.1; fees-design §3.6; fa-design §4 — line 3
/// carries the venue's `provider`):
/// * `lp_fees = None` — v3: exactly 16 `key=value\n` lines in fixed order;
/// * `lp_fees = Some(fees)` (non-empty) — v4: the same 16 lines with line 1
///   replaced by `msg_type=silvana.atomic-dvp.quote.v4`, then `fee_count=N`
///   and 4 lines per fee (0-based, list order): fee_<i>_receiver / _admin /
///   _id / _amount.
/// Trailing `\n` on every line. Errors if any value contains a newline.
pub fn build_canonical_message(f: &CanonicalQuoteFields) -> Result<String> {
    let base_amount = render_decimal(f.base_amount)?;
    let quote_amount = render_decimal(f.quote_amount)?;
    let msg_type = match &f.lp_fees {
        None => MSG_TYPE,
        Some(fees) if fees.is_empty() => {
            bail!("lp_fees must be non-empty when present (Some [] has no canonical encoding)")
        }
        Some(_) => MSG_TYPE_V4,
    };
    let mut lines: Vec<(String, String)> = vec![
        ("msg_type".into(), msg_type.into()),
        ("lp".into(), f.lp.into()),
        ("provider".into(), f.provider.into()),
        ("pair".into(), f.pair.into()),
        ("base_admin".into(), f.base_admin.into()),
        ("base_id".into(), f.base_id.into()),
        ("quote_admin".into(), f.quote_admin.into()),
        ("quote_id".into(), f.quote_instr_id.into()),
        ("user".into(), f.user.into()),
        ("side".into(), f.side.canonical().into()),
        ("base_amount".into(), base_amount),
        ("quote_amount".into(), quote_amount),
        ("quote_nonce".into(), f.quote_nonce.into()),
        ("created_at_micros".into(), f.created_at_micros.to_string()),
        ("valid_until_micros".into(), f.valid_until_micros.to_string()),
        ("ticket_id".into(), f.ticket_id.into()),
    ];
    if let Some(fees) = &f.lp_fees {
        lines.push(("fee_count".into(), fees.len().to_string()));
        for (i, fee) in fees.iter().enumerate() {
            lines.push((format!("fee_{i}_receiver"), fee.receiver.into()));
            lines.push((format!("fee_{i}_admin"), fee.admin.into()));
            lines.push((format!("fee_{i}_id"), fee.id.into()));
            lines.push((format!("fee_{i}_amount"), render_decimal(fee.amount)?));
        }
    }
    let mut out = String::new();
    for (k, v) in &lines {
        if v.contains('\n') {
            bail!("newline in canonical field {k}");
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push('\n');
    }
    Ok(out)
}

/// Sign a canonical message the way `DA.Crypto.Text.secp256k1` verifies it:
/// hash ONCE with SHA-256, then ECDSA-sign the 32-byte digest (RFC 6979
/// deterministic nonces, low-s — k256's default). Returns ASN.1 DER, lowercase hex.
pub fn sign_quote(priv_scalar_hex: &str, msg: &str) -> Result<String> {
    let scalar = hex::decode(priv_scalar_hex)
        .map_err(|e| anyhow!("invalid private scalar hex: {e}"))?;
    let key = SigningKey::from_slice(&scalar)
        .map_err(|e| anyhow!("invalid secp256k1 private key: {e}"))?;
    let digest = Sha256::digest(msg.as_bytes());
    let sig: Signature = key
        .sign_prehash(&digest)
        .map_err(|e| anyhow!("signing failed: {e}"))?;
    Ok(hex::encode(sig.to_der().as_bytes()))
}

/// Local verification (the H14 client pre-check). Normalizes high-s signatures
/// before verifying — matching the on-ledger BouncyCastle semantics that accept
/// both. Returns false (never errors) on any malformed input.
pub fn verify_quote(sig_der_hex: &str, msg: &str, pub_spki_hex: &str) -> bool {
    let inner = || -> Result<bool> {
        let der = hex::decode(sig_der_hex)?;
        let sig = Signature::from_der(&der)?;
        let sig = sig.normalize_s().unwrap_or(sig);
        let point = hex::decode(point_from_spki(pub_spki_hex)?)?;
        let vk = VerifyingKey::from_sec1_bytes(&point)?;
        let digest = Sha256::digest(msg.as_bytes());
        Ok(vk.verify_prehash(&digest, &sig).is_ok())
    };
    inner().unwrap_or(false)
}

/// Wrap an uncompressed 65-byte point (`04||X||Y`, lowercase hex) into SPKI DER hex.
pub fn spki_from_point(point_hex: &str) -> Result<String> {
    if point_hex.len() != 130 || !point_hex.starts_with("04") {
        bail!("expected lowercase uncompressed 65-byte point hex");
    }
    Ok(format!("{SPKI_PREFIX_SECP256K1_UNCOMPRESSED}{point_hex}"))
}

/// Extract the uncompressed point from an SPKI DER hex public key.
pub fn point_from_spki(spki_hex: &str) -> Result<String> {
    let spki = spki_hex.to_lowercase();
    let point = spki
        .strip_prefix(SPKI_PREFIX_SECP256K1_UNCOMPRESSED)
        .ok_or_else(|| anyhow!("not an uncompressed secp256k1 SPKI public key"))?;
    if point.len() != 130 || !point.starts_with("04") {
        bail!("malformed point in SPKI");
    }
    Ok(point.to_string())
}

/// The LP's quote-signing keypair, persisted as a JSON keyfile. TEST/OPS tool
/// state — the pub key is what `AtomicDVP.quotePublicKey` stores on-ledger.
#[derive(Serialize, Deserialize, Clone)]
pub struct QuoteKeyFile {
    /// raw 32-byte scalar, lowercase hex
    pub priv_scalar_hex: String,
    /// X.509 SPKI DER with uncompressed point, lowercase hex
    pub pub_spki_hex: String,
}

pub fn gen_keypair() -> Result<QuoteKeyFile> {
    let key = SigningKey::random(&mut rand::rngs::OsRng);
    let point = key.verifying_key().to_encoded_point(false);
    Ok(QuoteKeyFile {
        priv_scalar_hex: hex::encode(key.to_bytes()),
        pub_spki_hex: spki_from_point(&hex::encode(point.as_bytes()))?,
    })
}

/// Build a keyfile from a raw 32-byte scalar (lowercase hex) — the
/// ATOMIC_QUOTE_PRIVATE_KEY env-override path for containerized deploys.
pub fn keyfile_from_scalar(priv_scalar_hex: &str) -> Result<QuoteKeyFile> {
    let key = SigningKey::from_slice(&hex::decode(priv_scalar_hex)?)
        .map_err(|e| anyhow!("invalid secp256k1 private key: {e}"))?;
    let point = key.verifying_key().to_encoded_point(false);
    Ok(QuoteKeyFile {
        priv_scalar_hex: priv_scalar_hex.to_lowercase(),
        pub_spki_hex: spki_from_point(&hex::encode(point.as_bytes()))?,
    })
}

/// Keyfile path precedence: --quote-key-file flag > ATOMIC_QUOTE_KEY_FILE env
/// > ./atomic-quote-key.json.
pub fn resolve_keyfile_path(flag: Option<&str>) -> PathBuf {
    if let Some(p) = flag {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("ATOMIC_QUOTE_KEY_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    PathBuf::from("atomic-quote-key.json")
}

/// Load the keyfile; when absent and `create` is set, generate + write it
/// (owner-only 0600 on Unix — it contains the private scalar).
pub fn load_or_create_keyfile(path: &Path, create: bool) -> Result<QuoteKeyFile> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "⚠ quote keyfile {} has permissive mode {:o} — fixing to 0600",
                    path.display(),
                    mode
                );
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        let raw = std::fs::read_to_string(path)?;
        let kf: QuoteKeyFile = serde_json::from_str(&raw)
            .map_err(|e| anyhow!("invalid quote keyfile {}: {e}", path.display()))?;
        // integrity: the stored pub key must match the scalar
        let key = SigningKey::from_slice(&hex::decode(&kf.priv_scalar_hex)?)
            .map_err(|e| anyhow!("invalid scalar in {}: {e}", path.display()))?;
        let expected = spki_from_point(&hex::encode(key.verifying_key().to_encoded_point(false).as_bytes()))?;
        if !expected.eq_ignore_ascii_case(&kf.pub_spki_hex) {
            bail!("quote keyfile {} is corrupt: public key does not match the scalar", path.display());
        }
        return Ok(kf);
    }
    if !create {
        bail!(
            "quote keyfile {} not found — generate it first (atomic keygen / venue setup)",
            path.display()
        );
    }
    let kf = gen_keypair()?;
    {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts
            .open(path)
            .map_err(|e| anyhow!("cannot create quote keyfile {}: {e}", path.display()))?;
        f.write_all(serde_json::to_string_pretty(&kf)?.as_bytes())?;
    }
    println!("  Generated new secp256k1 quote keypair -> {}", path.display());
    println!("  ⚠ Keep this file safe: it signs all quotes for venues created with it.");
    Ok(kf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::str::FromStr;

    const VECTORS: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/quote-vectors.json"));

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).expect("parse quote-vectors.json")
    }

    /// Raw scalar from a daml-script-style PKCS8 DER hex private key.
    fn scalar_from_pkcs8(pkcs8_hex: &str) -> String {
        let rest = pkcs8_hex
            .to_lowercase()
            .strip_prefix(PKCS8_PREFIX_SECP256K1)
            .expect("unexpected PKCS8 layout")
            .to_string();
        rest[..64].to_string()
    }

    fn fields_from_vector(fields: &Value) -> CanonicalQuoteFields<'_> {
        let s = |k: &str| fields.get(k).and_then(|v| v.as_str()).unwrap();
        let micros = |k: &str| {
            let v = fields.get(k).unwrap();
            match v {
                Value::String(s) => s.parse::<i64>().unwrap(),
                Value::Number(n) => n.as_i64().unwrap(),
                _ => panic!("bad micros"),
            }
        };
        let lp_fees = fields.get("lpFees").and_then(|v| v.as_array()).map(|fees| {
            fees.iter()
                .map(|f| LpFeeSpec {
                    receiver: f["receiver"].as_str().unwrap(),
                    admin: f["admin"].as_str().unwrap(),
                    id: f["id"].as_str().unwrap(),
                    amount: Decimal::from_str(f["amount"].as_str().unwrap()).unwrap(),
                })
                .collect()
        });
        CanonicalQuoteFields {
            lp: s("lp"),
            provider: s("provider"),
            pair: s("pair"),
            base_admin: s("baseAdmin"),
            base_id: s("baseId"),
            quote_admin: s("quoteAdmin"),
            quote_instr_id: s("quoteInstrId"),
            user: s("user"),
            side: QuoteSide::parse(s("side")).unwrap(),
            base_amount: Decimal::from_str(s("baseAmount")).unwrap(),
            quote_amount: Decimal::from_str(s("quoteAmount")).unwrap(),
            quote_nonce: s("quoteNonce"),
            created_at_micros: micros("createdAtMicros"),
            valid_until_micros: micros("validUntilMicros"),
            ticket_id: s("ticketId"),
            lp_fees,
        }
    }

    #[test]
    fn golden_vectors_message_bytes_and_signatures() {
        let v = vectors();
        let quote_pub = v["quotePublicKeySpkiHex"].as_str().unwrap();
        let quote_scalar = scalar_from_pkcs8(v["quotePrivateKeyPkcs8Hex"].as_str().unwrap());
        let wrong_pub = v["wrongPublicKeySpkiHex"].as_str().unwrap();
        let wrong_scalar = scalar_from_pkcs8(v["wrongPrivateKeyPkcs8Hex"].as_str().unwrap());

        // key material self-check: scalars reproduce the committed SPKI keys
        let qk = SigningKey::from_slice(&hex::decode(&quote_scalar).unwrap()).unwrap();
        assert_eq!(
            spki_from_point(&hex::encode(qk.verifying_key().to_encoded_point(false).as_bytes())).unwrap(),
            quote_pub
        );

        let mut checked = 0;
        for case in v["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let fields = fields_from_vector(&case["fields"]);
            let canonical = case["canonicalMessage"].as_str().unwrap();
            let signed_msg = case["signedMessage"].as_str().unwrap();
            let sig_hex = case["signatureDerHex"].as_str().unwrap();
            let expect_verify = case["expectVerify"].as_bool().unwrap();
            let aborts = case["aborts"].as_bool().unwrap_or(false);

            // 1. byte-exact canonical reconstruction
            assert_eq!(
                build_canonical_message(&fields).unwrap(),
                canonical,
                "canonical message mismatch: {name}"
            );

            // 2. deterministic re-signing reproduces the committed DER (RFC 6979),
            //    except for the deliberately mangled signatures
            if name != "malformed-der" && name != "high-s" {
                let scalar = if name == "wrong-key" { &wrong_scalar } else { &quote_scalar };
                assert_eq!(
                    sign_quote(scalar, signed_msg).unwrap(),
                    sig_hex,
                    "signature bytes mismatch: {name}"
                );
            }

            // 3. verification truth table against the reconstructed message
            if !aborts {
                assert_eq!(
                    verify_quote(sig_hex, canonical, quote_pub),
                    expect_verify,
                    "verify truth mismatch: {name}"
                );
            } else {
                // malformed DER: local verify returns false (on-ledger it aborts)
                assert!(!verify_quote(sig_hex, canonical, quote_pub), "{name}");
            }
            assert!(!verify_quote(sig_hex, canonical, wrong_pub) || name == "wrong-key");
            checked += 1;
        }
        assert!(checked >= 20, "expected the full vector set incl. v4 lpfee cases, got {checked}");
    }

    #[test]
    fn v4_fee_block_structure() {
        let v = vectors();
        let one = v["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "lpfee-one")
            .expect("lpfee-one vector present");
        let msg = build_canonical_message(&fields_from_vector(&one["fields"])).unwrap();
        let lines: Vec<&str> = msg.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 21);
        assert_eq!(lines[0], "msg_type=silvana.atomic-dvp.quote.v4");
        assert_eq!(lines[16], "fee_count=1");
        assert!(lines[17].starts_with("fee_0_receiver="));
        assert!(lines[20].starts_with("fee_0_amount="));
        // Some [] has no encoding
        let mut empty = fields_from_vector(&one["fields"]);
        empty.lp_fees = Some(vec![]);
        assert!(build_canonical_message(&empty).is_err());
    }

    #[test]
    fn render_decimal_table() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        assert_eq!(render_decimal(d("10")).unwrap(), "10.0");
        assert_eq!(render_decimal(d("25.50")).unwrap(), "25.5");
        assert_eq!(render_decimal(d("0")).unwrap(), "0.0");
        assert_eq!(render_decimal(d("0.0")).unwrap(), "0.0");
        assert_eq!(render_decimal(d("1.25")).unwrap(), "1.25");
        assert_eq!(render_decimal(d("0.0000000001")).unwrap(), "0.0000000001");
        assert_eq!(render_decimal(d("123456.0000000001")).unwrap(), "123456.0000000001");
        assert_eq!(render_decimal(d("-3.10")).unwrap(), "-3.1");
        assert!(render_decimal(d("1.00000000001")).is_err()); // 11 dp
    }

    #[test]
    fn newline_injection_rejected() {
        let v = vectors();
        let case = &v["cases"][0];
        let mut f = fields_from_vector(&case["fields"]);
        f.quote_nonce = "evil\nvalid_until_micros=9";
        assert!(build_canonical_message(&f).is_err());
    }

    #[test]
    fn keypair_roundtrip() {
        let kf = gen_keypair().unwrap();
        let msg = "msg_type=silvana.atomic-dvp.quote.v3\nlp=lp\n";
        let sig = sign_quote(&kf.priv_scalar_hex, msg).unwrap();
        assert!(verify_quote(&sig, msg, &kf.pub_spki_hex));
        assert!(!verify_quote(&sig, "tampered", &kf.pub_spki_hex));
        assert_eq!(point_from_spki(&kf.pub_spki_hex).unwrap().len(), 130);
        let kf2 = keyfile_from_scalar(&kf.priv_scalar_hex).unwrap();
        assert_eq!(kf2.pub_spki_hex, kf.pub_spki_hex);
    }
}
