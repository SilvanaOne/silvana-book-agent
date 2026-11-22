//! Ed25519 over the full message (RFC 8032 PureEdDSA) with strict verification.

use ed25519_dalek::{Signature, VerifyingKey};

use crate::error::SignatureError;

/// The group order L, little-endian.
const ORDER_L: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

/// True when the little-endian scalar is below L.
fn scalar_below_order(s: &[u8; 32]) -> bool {
    for (a, b) in s.iter().rev().zip(ORDER_L.iter().rev()) {
        if a != b {
            return a < b;
        }
    }
    false
}

/// A point whose encoding is the canonical one.
fn decode_canonical_point(bytes: &[u8; 32]) -> Option<VerifyingKey> {
    let point = VerifyingKey::from_bytes(bytes).ok()?;
    (point.to_edwards().compress().to_bytes() == *bytes).then_some(point)
}

fn in_prime_subgroup(point: &VerifyingKey) -> bool {
    !point.is_weak() && point.to_edwards().is_torsion_free()
}

/// A canonical point of the prime-order subgroup, other than the identity.
pub fn parse_ed25519_public_key(bytes: &[u8; 32]) -> Result<VerifyingKey, SignatureError> {
    let key = decode_canonical_point(bytes).ok_or(SignatureError::InvalidPublicKey)?;
    if !in_prime_subgroup(&key) {
        return Err(SignatureError::WeakPublicKey);
    }
    Ok(key)
}

/// Strict verification: canonical A and R in the prime-order subgroup, s below L.
pub fn verify_ed25519_strict(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8],
) -> Result<(), SignatureError> {
    let key = parse_ed25519_public_key(public_key)?;
    let signature: &[u8; 64] = signature
        .try_into()
        .map_err(|_| SignatureError::InvalidSignatureLength)?;
    let (Some(r), Some(s)) = (signature.first_chunk::<32>(), signature.last_chunk::<32>()) else {
        return Err(SignatureError::InvalidSignatureLength);
    };
    let r_point = decode_canonical_point(r).ok_or(SignatureError::BadSignature)?;
    if !in_prime_subgroup(&r_point) || !scalar_below_order(s) {
        return Err(SignatureError::BadSignature);
    }
    key.verify_strict(message, &Signature::from_bytes(signature))
        .map_err(|_| SignatureError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;
    use curve25519_dalek::edwards::CompressedEdwardsY;
    use curve25519_dalek::scalar::Scalar;
    use ed25519_dalek::{Signer, Verifier};
    use sha2::{Digest, Sha512};

    const SMALL_ORDER: [&str; 8] = [
        "0100000000000000000000000000000000000000000000000000000000000000",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000080",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    ];

    fn bytes32(hex_str: &str) -> [u8; 32] {
        hex::decode(hex_str).unwrap().try_into().unwrap()
    }

    const MESSAGE: &[u8] = b"scheme=silvana.agent.test.v1\nfield=a full message, never a digest\n";

    #[test]
    fn accepts_a_valid_signature_over_the_full_message() {
        let key = testkit::ed_key(0x21);
        let signature = key.sign(MESSAGE).to_bytes();
        let public = key.verifying_key().to_bytes();
        assert_eq!(verify_ed25519_strict(&public, MESSAGE, &signature), Ok(()));
        assert_eq!(
            verify_ed25519_strict(&public, b"another message", &signature),
            Err(SignatureError::BadSignature)
        );
    }

    #[test]
    fn refuses_small_order_and_torsion_keys() {
        for hex_str in SMALL_ORDER {
            let point = bytes32(hex_str);
            assert_eq!(
                parse_ed25519_public_key(&point).err(),
                Some(SignatureError::WeakPublicKey),
                "{hex_str}"
            );
            assert_eq!(
                verify_ed25519_strict(&point, MESSAGE, &[0u8; 64]),
                Err(SignatureError::WeakPublicKey)
            );
        }
        let key = testkit::ed_key(0x21).verifying_key();
        let torsion = CompressedEdwardsY(bytes32(SMALL_ORDER[4]))
            .decompress()
            .unwrap();
        let mixed = (key.to_edwards() + torsion).compress().to_bytes();
        assert_eq!(
            parse_ed25519_public_key(&mixed).err(),
            Some(SignatureError::WeakPublicKey)
        );
    }

    #[test]
    fn refuses_non_canonical_key_encodings() {
        // y = p is a non-canonical encoding of y = 0; the identity with the sign bit set.
        for hex_str in [
            "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "0100000000000000000000000000000000000000000000000000000000000080",
        ] {
            assert_eq!(
                parse_ed25519_public_key(&bytes32(hex_str)).err(),
                Some(SignatureError::InvalidPublicKey),
                "{hex_str}"
            );
        }
    }

    #[test]
    fn refuses_a_small_order_r_that_plain_verify_accepts() {
        let key = testkit::ed_key(0x21);
        let public = key.verifying_key();
        let r = bytes32(SMALL_ORDER[0]);
        let mut h = Sha512::new();
        h.update(r);
        h.update(public.to_bytes());
        h.update(MESSAGE);
        let k = Scalar::from_bytes_mod_order_wide(&h.finalize().into());
        let s = k * key.to_scalar();
        let mut forged = [0u8; 64];
        forged[..32].clone_from_slice(&r);
        forged[32..].clone_from_slice(&s.to_bytes());
        let forged_sig = Signature::from_bytes(&forged);
        assert!(
            public.verify(MESSAGE, &forged_sig).is_ok(),
            "plain verify accepts the vector"
        );
        assert_eq!(
            verify_ed25519_strict(&public.to_bytes(), MESSAGE, &forged),
            Err(SignatureError::BadSignature)
        );
    }

    #[test]
    fn refuses_s_at_or_above_the_order() {
        let key = testkit::ed_key(0x21);
        let public = key.verifying_key().to_bytes();
        let mut signature = key.sign(MESSAGE).to_bytes();
        let mut carry = 0u16;
        for (i, l) in ORDER_L.iter().enumerate() {
            let sum = u16::from(signature[32 + i]) + u16::from(*l) + carry;
            signature[32 + i] = (sum & 0xff) as u8;
            carry = sum >> 8;
        }
        assert_eq!(carry, 0);
        assert_eq!(
            verify_ed25519_strict(&public, MESSAGE, &signature),
            Err(SignatureError::BadSignature)
        );
        let mut at_order = key.sign(MESSAGE).to_bytes();
        at_order[32..].clone_from_slice(&ORDER_L);
        assert_eq!(
            verify_ed25519_strict(&public, MESSAGE, &at_order),
            Err(SignatureError::BadSignature)
        );
        assert!(scalar_below_order(&{
            let mut below = ORDER_L;
            below[0] -= 1;
            below
        }));
        assert!(!scalar_below_order(&ORDER_L));
    }

    #[test]
    fn refuses_wrong_signature_lengths() {
        let key = testkit::ed_key(0x21);
        let public = key.verifying_key().to_bytes();
        let signature = key.sign(MESSAGE).to_bytes();
        for len in [0usize, 63, 65] {
            let mut bytes = signature.to_vec();
            bytes.resize(len, 0);
            assert_eq!(
                verify_ed25519_strict(&public, MESSAGE, &bytes),
                Err(SignatureError::InvalidSignatureLength),
                "{len}"
            );
        }
    }
}
