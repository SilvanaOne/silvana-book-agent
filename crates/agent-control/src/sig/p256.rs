//! P-256 keys as 91-byte SPKI and ECDSA/SHA-256 verification; high-S is accepted.

use ::p256::ecdsa::signature::Verifier;
use ::p256::ecdsa::{Signature, VerifyingKey};
use ::p256::pkcs8::DecodePublicKey;

use crate::error::SignatureError;

/// SPKI DER header for an uncompressed P-256 point, ending with the 0x04 point tag.
pub const P256_SPKI_PREFIX: [u8; 27] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
];
/// Length of the SPKI: the header plus X and Y.
pub const P256_SPKI_LEN: usize = 91;
/// Shortest and longest DER ECDSA signature accepted.
pub const MIN_DER_SIGNATURE: usize = 8;
pub const MAX_DER_SIGNATURE: usize = 72;

/// A validated P-256 public key with its SPKI bytes.
#[derive(Clone, Debug)]
pub struct P256PublicKey {
    spki: [u8; P256_SPKI_LEN],
    key: VerifyingKey,
}

impl P256PublicKey {
    pub fn spki(&self) -> &[u8; P256_SPKI_LEN] {
        &self.spki
    }
}

impl PartialEq for P256PublicKey {
    fn eq(&self, other: &Self) -> bool {
        self.spki == other.spki
    }
}

impl Eq for P256PublicKey {}

/// Exactly the 91-byte uncompressed SPKI with the fixed header and an on-curve point.
pub fn parse_p256_spki(bytes: &[u8]) -> Result<P256PublicKey, SignatureError> {
    let spki: [u8; P256_SPKI_LEN] = bytes
        .try_into()
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    if !spki.starts_with(&P256_SPKI_PREFIX) {
        return Err(SignatureError::InvalidPublicKey);
    }
    let public = ::p256::PublicKey::from_public_key_der(&spki)
        .map_err(|_| SignatureError::InvalidPublicKey)?;
    Ok(P256PublicKey {
        spki,
        key: VerifyingKey::from(public),
    })
}

/// ECDSA P-256/SHA-256 over `message`, signature as 64-byte r||s.
pub fn verify_p256_p1363(
    key: &P256PublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), SignatureError> {
    if signature.len() != 64 {
        return Err(SignatureError::InvalidSignatureLength);
    }
    let signature =
        Signature::from_slice(signature).map_err(|_| SignatureError::InvalidSignatureEncoding)?;
    key.key
        .verify(message, &signature)
        .map_err(|_| SignatureError::BadSignature)
}

/// ECDSA P-256/SHA-256 over `message`, signature as strict DER.
pub(crate) fn verify_p256_der(
    key: &P256PublicKey,
    message: &[u8],
    der: &[u8],
) -> Result<(), SignatureError> {
    if !(MIN_DER_SIGNATURE..=MAX_DER_SIGNATURE).contains(&der.len()) {
        return Err(SignatureError::InvalidSignatureEncoding);
    }
    let signature =
        Signature::from_der(der).map_err(|_| SignatureError::InvalidSignatureEncoding)?;
    key.key
        .verify(message, &signature)
        .map_err(|_| SignatureError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;
    use ::p256::ecdsa::signature::Signer;

    #[test]
    fn parses_the_canonical_spki_only() {
        let key = testkit::p256_key();
        let spki = testkit::p256_spki(&key);
        assert_eq!(spki.len(), 91);
        assert_eq!(
            parse_p256_spki(&spki).unwrap().spki().as_slice(),
            spki.as_slice()
        );

        let compressed_point = key.verifying_key().to_encoded_point(true);
        let mut compressed =
            hex::decode("3039301306072a8648ce3d020106082a8648ce3d030107032200").unwrap();
        compressed.extend_from_slice(compressed_point.as_bytes());
        let secp256k1 = {
            let mut v = hex::decode("3056301006072a8648ce3d020106052b8104000a034200").unwrap();
            v.extend_from_slice(&spki[26..]);
            v
        };
        let mut off_curve = spki.clone();
        off_curve[90] ^= 0x01;
        let mut zero_point = spki[..27].to_vec();
        zero_point.extend_from_slice(&[0u8; 64]);
        let mut wrong_tag = spki.clone();
        wrong_tag[26] = 0x05;
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("short", spki[..90].to_vec()),
            ("long", [spki.as_slice(), &[0]].concat()),
            ("compressed", compressed),
            ("secp256k1", secp256k1),
            ("off curve", off_curve),
            ("zero point", zero_point),
            ("wrong tag", wrong_tag),
        ] {
            assert_eq!(
                parse_p256_spki(&bytes).err(),
                Some(SignatureError::InvalidPublicKey),
                "{name}"
            );
        }
    }

    #[test]
    fn p1363_signatures_verify_including_high_s() {
        let key = testkit::p256_key();
        let public = parse_p256_spki(&testkit::p256_spki(&key)).unwrap();
        let message = b"scheme=silvana.agent.read-request.v1\n";
        let signature: Signature = key.sign(message);
        assert_eq!(
            verify_p256_p1363(&public, message, &signature.to_bytes()),
            Ok(())
        );
        let high = testkit::high_s(&signature);
        assert!(high.normalize_s().is_some(), "the vector is high-S");
        assert_eq!(
            verify_p256_p1363(&public, message, &high.to_bytes()),
            Ok(())
        );
        assert_eq!(
            verify_p256_p1363(&public, b"other", &signature.to_bytes()),
            Err(SignatureError::BadSignature)
        );
        assert_eq!(
            verify_p256_p1363(&public, message, &signature.to_bytes()[..63]),
            Err(SignatureError::InvalidSignatureLength)
        );
        assert_eq!(
            verify_p256_p1363(&public, message, &[0u8; 64]),
            Err(SignatureError::InvalidSignatureEncoding)
        );
        assert_eq!(
            verify_p256_der(&public, message, signature.to_der().as_bytes()),
            Ok(())
        );
        assert_eq!(
            verify_p256_der(&public, message, &[0x30, 0x00]),
            Err(SignatureError::InvalidSignatureEncoding)
        );
    }
}
