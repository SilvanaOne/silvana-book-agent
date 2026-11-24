//! Invite and link codes: RFC 4648 base32 (uppercase, no padding) of 16 bytes.

use crate::canon::values::Commitment;
use crate::error::CodeError;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Prefix of an agent invite code.
pub const INVITE_PREFIX: &str = "AGT-";
/// Prefix of a link code for an existing agent.
pub const LINK_PREFIX: &str = "LNK-";
/// Characters in the base32 form of 16 bytes.
pub const BODY_CHARS: usize = 26;

fn alphabet_char(index: u32) -> char {
    usize::try_from(index & 0x1f)
        .ok()
        .and_then(|i| ALPHABET.get(i))
        .map_or('A', |b| char::from(*b))
}

/// RFC 4648 base32 without padding.
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in bytes {
        buffer = buffer.wrapping_shl(8) | u32::from(byte);
        bits = bits.wrapping_add(8);
        while bits >= 5 {
            bits = bits.wrapping_sub(5);
            out.push(alphabet_char(buffer.wrapping_shr(bits)));
        }
        buffer &= 1u32.wrapping_shl(bits).wrapping_sub(1);
    }
    if bits > 0 {
        out.push(alphabet_char(buffer.wrapping_shl(5u32.wrapping_sub(bits))));
    }
    out
}

/// Exactly 26 uppercase base32 characters whose 2 trailing bits are zero.
fn decode_body(s: &str) -> Result<[u8; 16], CodeError> {
    if s.len() != BODY_CHARS {
        return Err(CodeError::InvalidLength);
    }
    let mut out = [0u8; 16];
    let mut slots = out.iter_mut();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        let index = ALPHABET
            .iter()
            .position(|a| *a == c)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or(CodeError::InvalidCharacter)?;
        buffer = buffer.wrapping_shl(5) | index;
        bits = bits.wrapping_add(5);
        if bits >= 8 {
            bits = bits.wrapping_sub(8);
            let byte = u8::try_from(buffer.wrapping_shr(bits) & 0xff)
                .map_err(|_| CodeError::InvalidCharacter)?;
            let slot = slots.next().ok_or(CodeError::InvalidLength)?;
            *slot = byte;
            buffer &= 1u32.wrapping_shl(bits).wrapping_sub(1);
        }
    }
    if slots.next().is_some() || bits != 2 || buffer != 0 {
        return Err(CodeError::NonCanonicalBits);
    }
    Ok(out)
}

/// `AGT-<invite body>-<commitment>`, from a bare `AGT-<invite body>` code.
pub fn format_invite_code(invite: &str, commitment: &Commitment) -> Result<String, CodeError> {
    let (bare, existing) = split_invite_code(invite)?;
    if existing.is_some() {
        return Err(CodeError::InvalidFormat);
    }
    Ok(format!("{bare}-{}", base32_encode(&commitment.0)))
}

/// `LNK-<commitment>`.
pub fn format_link_code(commitment: &Commitment) -> String {
    format!("{LINK_PREFIX}{}", base32_encode(&commitment.0))
}

/// Splits an invite code into the bare `AGT-<body>` code and its commitment, if any.
/// Input is case-insensitive; the bare code comes back uppercase.
pub fn split_invite_code(code: &str) -> Result<(String, Option<Commitment>), CodeError> {
    let upper = code.to_ascii_uppercase();
    let rest = upper
        .strip_prefix(INVITE_PREFIX)
        .ok_or(CodeError::InvalidPrefix)?;
    let mut parts = rest.split('-');
    let body = parts.next().ok_or(CodeError::InvalidFormat)?;
    let commitment = parts.next();
    if parts.next().is_some() {
        return Err(CodeError::InvalidFormat);
    }
    decode_body(body)?;
    let commitment = match commitment {
        Some(c) => Some(Commitment(decode_body(c)?)),
        None => None,
    };
    Ok((format!("{INVITE_PREFIX}{body}"), commitment))
}

/// The commitment of a `LNK-<commitment>` code, case-insensitive.
pub fn parse_link_code(code: &str) -> Result<Commitment, CodeError> {
    let upper = code.to_ascii_uppercase();
    let body = upper
        .strip_prefix(LINK_PREFIX)
        .ok_or(CodeError::InvalidPrefix)?;
    if body.contains('-') {
        return Err(CodeError::InvalidFormat);
    }
    decode_body(body).map(Commitment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc_4648() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "MY"),
            (b"fo", "MZXQ"),
            (b"foo", "MZXW6"),
            (b"foob", "MZXW6YQ"),
            (b"fooba", "MZXW6YTB"),
            (b"foobar", "MZXW6YTBOI"),
        ];
        for (input, expected) in cases {
            assert_eq!(base32_encode(input), expected);
        }
        assert_eq!(base32_encode(&[0u8; 16]), "A".repeat(26));
        assert_eq!(base32_encode(&[0xffu8; 16]), format!("{}4", "7".repeat(25)));
    }

    fn commitment() -> Commitment {
        let mut bytes = [0u8; 16];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        Commitment(bytes)
    }

    #[test]
    fn link_codes_round_trip_case_insensitively() {
        let c = commitment();
        let code = format_link_code(&c);
        assert_eq!(code.len(), 30);
        assert!(code.starts_with("LNK-"));
        assert_eq!(parse_link_code(&code).unwrap(), c);
        assert_eq!(parse_link_code(&code.to_lowercase()).unwrap(), c);
    }

    #[test]
    fn invite_codes_split_into_the_bare_code_and_commitment() {
        let invite = format!("AGT-{}", base32_encode(&[7u8; 16]));
        let c = commitment();
        let full = format_invite_code(&invite, &c).unwrap();
        assert_eq!(full.len(), 57);
        assert_eq!(split_invite_code(&full).unwrap(), (invite.clone(), Some(c)));
        assert_eq!(
            split_invite_code(&full.to_lowercase()).unwrap(),
            (invite.clone(), Some(c))
        );
        assert_eq!(split_invite_code(&invite).unwrap(), (invite.clone(), None));
        assert_eq!(format_invite_code(&full, &c), Err(CodeError::InvalidFormat));
    }

    #[test]
    fn malformed_codes_are_refused() {
        let body = base32_encode(&[7u8; 16]);
        let c = base32_encode(&commitment().0);
        let mut trailing = body.clone();
        trailing.pop();
        trailing.push('B');
        let cases: Vec<(String, CodeError)> = vec![
            (format!("ABC-{body}"), CodeError::InvalidPrefix),
            (format!("LNK-{body}"), CodeError::InvalidPrefix),
            (format!("AGT{body}"), CodeError::InvalidPrefix),
            (format!("AGT-{body}-{c}-{c}"), CodeError::InvalidFormat),
            (format!("AGT-{body}-"), CodeError::InvalidLength),
            (format!("AGT-{}", &body[..25]), CodeError::InvalidLength),
            (format!("AGT-{body}A"), CodeError::InvalidLength),
            (format!("AGT-{}1", &body[..25]), CodeError::InvalidCharacter),
            (format!("AGT-{}=", &body[..25]), CodeError::InvalidCharacter),
            (format!("AGT-{trailing}"), CodeError::NonCanonicalBits),
            (
                format!("AGT-{body}-{trailing}"),
                CodeError::NonCanonicalBits,
            ),
            (format!(" AGT-{body}"), CodeError::InvalidPrefix),
            (format!("AGT-{body} "), CodeError::InvalidLength),
        ];
        for (code, expected) in cases {
            assert_eq!(split_invite_code(&code).err(), Some(expected), "{code}");
        }
        let link_cases: Vec<(String, CodeError)> = vec![
            (format!("AGT-{c}"), CodeError::InvalidPrefix),
            (format!("LNK-{c}-{c}"), CodeError::InvalidFormat),
            (format!("LNK-{}", &c[..25]), CodeError::InvalidLength),
            (format!("LNK-{trailing}"), CodeError::NonCanonicalBits),
            (format!("LNK-{}8", &c[..25]), CodeError::InvalidCharacter),
        ];
        for (code, expected) in link_cases {
            assert_eq!(parse_link_code(&code).err(), Some(expected), "{code}");
        }
        assert_eq!(
            CodeError::NonCanonicalBits.code_name(),
            "invalid_code_trailing_bits"
        );
    }
}
