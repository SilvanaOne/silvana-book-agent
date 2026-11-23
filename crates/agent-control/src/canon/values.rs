//! Value grammars. Every type parses only its canonical text and displays it back.

use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::error::{ValueError, ValueKind};
use crate::party::PartyId;

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Exactly `N` bytes as lowercase hex.
pub(crate) fn parse_hex_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    if Some(s.len()) != N.checked_mul(2) || !is_lower_hex(s) {
        return None;
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).ok()?;
    Some(out)
}

macro_rules! hex_value {
    ($(#[$doc:meta])* $name:ident, $len:expr, $kind:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub fn parse(s: &str) -> Result<Self, ValueError> {
                parse_hex_array::<$len>(s).map(Self).ok_or(ValueError($kind))
            }

            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(s: &str) -> Result<Self, ValueError> {
                Self::parse(s)
            }
        }
    };
}

hex_value!(
    /// 32 bytes as 64 lowercase hex characters.
    Hash32, 32, ValueKind::Hex32
);
hex_value!(
    /// 16 bytes as 32 lowercase hex characters.
    Nonce16, 16, ValueKind::Hex16
);
hex_value!(
    /// The first 16 bytes of SHA-256 over a link descriptor.
    Commitment, 16, ValueKind::Hex16
);
hex_value!(
    /// 64 bytes as 128 lowercase hex characters.
    Sig64, 64, ValueKind::Hex64
);
hex_value!(
    /// A Daml package id: 32 bytes as 64 lowercase hex characters.
    PackageId, 32, ValueKind::PackageId
);

macro_rules! display_from_str {
    ($($t:ty),*) => {
        $(
            impl FromStr for $t {
                type Err = ValueError;

                fn from_str(s: &str) -> Result<Self, ValueError> {
                    Self::parse(s)
                }
            }
        )*
    };
}

/// The network a document belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Network {
    Devnet,
    Mainnet,
}

impl Network {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Devnet => "devnet",
            Self::Mainnet => "mainnet",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        match s {
            "devnet" => Ok(Self::Devnet),
            "mainnet" => Ok(Self::Mainnet),
            _ => Err(ValueError(ValueKind::Network)),
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `0|[1-9][0-9]{0,19}`, at most 2^64-1.
pub fn parse_u64(s: &str) -> Result<u64, ValueError> {
    let err = ValueError(ValueKind::U64);
    let shape = !s.is_empty()
        && s.len() <= 20
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if !shape {
        return Err(err);
    }
    s.parse::<u64>().map_err(|_| err)
}

/// `true|false`.
pub fn parse_bool(s: &str) -> Result<bool, ValueError> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ValueError(ValueKind::Bool)),
    }
}

pub fn format_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// An owner account id: a u64 of at least 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerAccount(u64);

impl OwnerAccount {
    pub fn new(id: u64) -> Result<Self, ValueError> {
        if id == 0 {
            return Err(ValueError(ValueKind::OwnerAccount));
        }
        Ok(Self(id))
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let id = parse_u64(s).map_err(|_| ValueError(ValueKind::OwnerAccount))?;
        Self::new(id)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for OwnerAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 1-64 of `[A-Za-z0-9 ._-]`, alphanumeric first, no trailing or double space.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(String);

impl Label {
    pub const MAX_LEN: usize = 64;

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let b = s.as_bytes();
        let ok = (1..=Self::MAX_LEN).contains(&b.len())
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b' ' | b'.' | b'_' | b'-'))
            && b.first().is_some_and(u8::is_ascii_alphanumeric)
            && b.last() != Some(&b' ')
            && !s.contains("  ");
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ValueError(ValueKind::Label))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Printable ASCII of 1 to `MAX` bytes, no leading, trailing or double space.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Text<const MAX: usize>(String);

impl<const MAX: usize> Text<MAX> {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let b = s.as_bytes();
        let ok = (1..=MAX).contains(&b.len())
            && b.iter().all(|c| (0x20..=0x7e).contains(c))
            && b.first() != Some(&b' ')
            && b.last() != Some(&b' ')
            && !s.contains("  ");
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ValueError(ValueKind::Text))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> FromStr for Text<MAX> {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

fn is_dns_label(label: &str) -> bool {
    let b = label.as_bytes();
    (1..=63).contains(&b.len())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b.first() != Some(&b'-')
        && b.last() != Some(&b'-')
}

/// Lowercase DNS name of at least two labels, the last not all digits.
pub(crate) fn is_dns_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    let mut labels = 0usize;
    let mut last = "";
    for label in s.split('.') {
        if !is_dns_label(label) {
            return false;
        }
        labels = labels.saturating_add(1);
        last = label;
    }
    labels >= 2 && !last.bytes().all(|c| c.is_ascii_digit())
}

/// A WebAuthn relying-party id: a lowercase DNS name, or `localhost`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RpId(String);

impl RpId {
    pub const LOCALHOST: &'static str = "localhost";

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        if s == Self::LOCALHOST || is_dns_name(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(ValueError(ValueKind::RpId))
        }
    }

    /// For constants that are known to parse.
    pub(crate) fn from_static(s: &'static str) -> Self {
        Self(s.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_localhost(&self) -> bool {
        self.0 == Self::LOCALHOST
    }
}

/// `https://<dns name>[:port]` or `http://localhost[:port]`, never a default port.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    text: String,
    secure: bool,
    host: String,
    port: Option<u16>,
}

fn parse_port(p: &str, secure: bool) -> Option<u16> {
    let shape = !p.is_empty()
        && p.len() <= 5
        && p.bytes().all(|b| b.is_ascii_digit())
        && !p.starts_with('0');
    if !shape {
        return None;
    }
    let port: u16 = p.parse().ok()?;
    let default = if secure { 443 } else { 80 };
    (port != default).then_some(port)
}

impl Origin {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let err = ValueError(ValueKind::Origin);
        let (secure, rest) = if let Some(rest) = s.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = s.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(err);
        };
        let (host, port) = match rest.split_once(':') {
            Some((host, port)) => (host, Some(parse_port(port, secure).ok_or(err)?)),
            None => (rest, None),
        };
        let host_ok = if secure {
            is_dns_name(host)
        } else {
            host == RpId::LOCALHOST
        };
        if !host_ok {
            return Err(err);
        }
        Ok(Self {
            text: s.to_owned(),
            secure,
            host: host.to_owned(),
            port,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_secure(&self) -> bool {
        self.secure
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// https with the rp_id or a subdomain of it; http only for a localhost rp_id.
    pub fn matches_rp_id(&self, rp_id: &RpId) -> bool {
        if rp_id.is_localhost() {
            return !self.secure && self.host == RpId::LOCALHOST;
        }
        self.secure
            && (self.host == rp_id.as_str()
                || self
                    .host
                    .strip_suffix(rp_id.as_str())
                    .is_some_and(|sub| sub.ends_with('.')))
    }
}

/// `[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid7(String);

impl Uuid7 {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let parts: Vec<&str> = s.split('-').collect();
        let ok = match parts.as_slice() {
            [a, b, c, d, e] => {
                a.len() == 8
                    && b.len() == 4
                    && c.len() == 4
                    && d.len() == 4
                    && e.len() == 12
                    && [a, b, c, d, e].iter().all(|p| is_lower_hex(p))
                    && c.starts_with('7')
                    && d.starts_with(['8', '9', 'a', 'b'])
            }
            _ => false,
        };
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ValueError(ValueKind::Uuid7))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn charset_ok(s: &str, max: usize, extra: &[u8]) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

macro_rules! charset_value {
    ($(#[$doc:meta])* $name:ident, $max:expr, $extra:expr, $kind:expr) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(s: &str) -> Result<Self, ValueError> {
                if charset_ok(s, $max, $extra) {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(ValueError($kind))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

charset_value!(
    /// `[A-Za-z0-9._-]{1,64}`.
    InstrumentId, 64, b"._-", ValueKind::InstrumentId
);
charset_value!(
    /// `[A-Za-z0-9._/-]{1,64}`.
    MarketId, 64, b"._/-", ValueKind::MarketId
);
charset_value!(
    /// `[A-Za-z0-9:._-]{1,256}`: update ids, settlement ids, method names.
    Token, 256, b":._-", ValueKind::Token
);

/// `0x` followed by 40 lowercase hex characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EthAddress(pub [u8; 20]);

impl EthAddress {
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        s.strip_prefix("0x")
            .and_then(parse_hex_array::<20>)
            .map(Self)
            .ok_or(ValueError(ValueKind::EthAddress))
    }
}

/// An instrument named by its admin party and id; ordered by (admin, id).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentRef {
    pub admin: PartyId,
    pub id: InstrumentId,
}

fn amount_shape(s: &str) -> bool {
    let (int, frac) = match s.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (s, None),
    };
    let int_ok = int == "0"
        || ((1..=17).contains(&int.len())
            && int.bytes().all(|b| b.is_ascii_digit())
            && !int.starts_with('0'));
    let frac_ok = frac.is_none_or(|f| {
        (1..=10).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) && !f.ends_with('0')
    });
    int_ok && frac_ok
}

/// A non-negative decimal: at most 17 integer and 10 fraction digits, minimal form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(Decimal);

impl Amount {
    pub const ZERO: Self = Self(Decimal::ZERO);

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let err = ValueError(ValueKind::Amount);
        if !amount_shape(s) {
            return Err(err);
        }
        let value = Decimal::from_str_exact(s).map_err(|_| err)?;
        Self::from_decimal(value)
    }

    /// Normalises `value`; refused when its minimal form breaks the grammar.
    pub fn from_decimal(value: Decimal) -> Result<Self, ValueError> {
        let normal = value.normalize();
        if amount_shape(&normal.to_string()) {
            Ok(Self(normal))
        } else {
            Err(ValueError(ValueKind::Amount))
        }
    }

    pub fn as_decimal(&self) -> Decimal {
        self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// `None` when the sum leaves the grammar.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0
            .checked_add(other.0)
            .and_then(|sum| Self::from_decimal(sum).ok())
    }

    /// `None` when `other` is larger, since an amount is never negative.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0
            .checked_sub(other.0)
            .and_then(|difference| Self::from_decimal(difference).ok())
    }

    /// The amount as a whole number of 10^-10 units; below 10^27, so it fits.
    fn units(self) -> Option<u128> {
        let scale = 10u32.checked_sub(self.0.scale())?;
        let factor = 10u128.checked_pow(scale)?;
        u128::try_from(self.0.mantissa()).ok()?.checked_mul(factor)
    }

    /// True when `self / denominator >= floor`, compared exactly with no rounding;
    /// `None` when `denominator` is zero.
    pub fn ratio_at_least(self, denominator: Self, floor: Self) -> Option<bool> {
        let scaled = self.units()?.checked_mul(10u128.checked_pow(10)?)?;
        let quotient = scaled.checked_div(denominator.units()?)?;
        Some(quotient >= floor.units()?)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! display_as_str {
    ($($t:ty),*) => {
        $(
            impl fmt::Display for $t {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(self.as_str())
                }
            }
        )*
    };
}

display_as_str!(Label, RpId, Origin, Uuid7, InstrumentId, MarketId, Token);
display_from_str!(
    Network,
    OwnerAccount,
    Label,
    RpId,
    Origin,
    Uuid7,
    InstrumentId,
    MarketId,
    Token,
    EthAddress,
    Amount
);

impl<const MAX: usize> fmt::Display for Text<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for EthAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

/// RFC 4648 section 5 base64url without padding.
pub fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Canonical base64url without padding: non-empty, and it must re-encode identically.
pub fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    let alphabet = s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if s.is_empty() || !alphabet {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
    (URL_SAFE_NO_PAD.encode(&bytes) == s).then_some(bytes)
}

pub(crate) fn decode_b64url_bounded(s: &str, min: usize, max: usize) -> Option<Vec<u8>> {
    decode_b64url(s).filter(|b| (min..=max).contains(&b.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table<T: fmt::Display>(
        parse: impl Fn(&str) -> Result<T, ValueError>,
        kind: ValueKind,
        accept: &[&str],
        refuse: &[&str],
    ) {
        for s in accept {
            let value = parse(s).unwrap_or_else(|e| panic!("{s:?} refused: {e}"));
            assert_eq!(value.to_string(), *s, "{s:?} does not display back");
        }
        for s in refuse {
            assert_eq!(parse(s).err(), Some(ValueError(kind)), "{s:?} accepted");
        }
    }

    const H64: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn hex_grammars() {
        let upper = H64.to_uppercase();
        let short = &H64[..62];
        let long = format!("{H64}00");
        table(
            Hash32::parse,
            ValueKind::Hex32,
            &[H64],
            &[&upper, short, &long, "", "0x00", &H64.replacen('0', "g", 1)],
        );
        let h32 = &H64[..32];
        table(
            Nonce16::parse,
            ValueKind::Hex16,
            &[h32],
            &[&h32.to_uppercase(), &H64[..30], H64, ""],
        );
        table(
            Commitment::parse,
            ValueKind::Hex16,
            &[h32],
            &[" 00112233445566778899aabbccddeef"],
        );
        let h128 = format!("{H64}{H64}");
        table(
            Sig64::parse,
            ValueKind::Hex64,
            &[&h128],
            &[H64, &h128.to_uppercase(), &format!("{h128}0")],
        );
        table(
            PackageId::parse,
            ValueKind::PackageId,
            &[H64, &"f".repeat(64)],
            &[
                &upper,
                &H64[..63],
                &format!("{H64}0"),
                "",
                &format!("0x{}", &H64[..62]),
                &format!("{} ", &H64[..63]),
            ],
        );
        assert_eq!(PackageId::parse(&upper).unwrap_err().code(), 188);
    }

    #[test]
    fn u64_and_bool_grammars() {
        table(
            parse_u64,
            ValueKind::U64,
            &["0", "1", "42", "18446744073709551615"],
            &[
                "",
                "00",
                "01",
                "+1",
                "-1",
                "1.0",
                " 1",
                "1 ",
                "18446744073709551616",
                "1e3",
                "0x1",
            ],
        );
        table(
            parse_bool,
            ValueKind::Bool,
            &["true", "false"],
            &["", "TRUE", "True", "1", "0", "yes"],
        );
        assert_eq!(format_bool(true), "true");
    }

    #[test]
    fn owner_account_grammar() {
        table(
            OwnerAccount::parse,
            ValueKind::OwnerAccount,
            &["1", "123456", "18446744073709551615"],
            &["0", "", "01", "-5", "abc"],
        );
        assert!(OwnerAccount::new(0).is_err());
    }

    #[test]
    fn network_grammar() {
        table(
            Network::parse,
            ValueKind::Network,
            &["devnet", "mainnet"],
            &["", "Devnet", "testnet", "mainnet "],
        );
    }

    #[test]
    fn amount_grammar() {
        let max_int = "99999999999999999";
        table(
            Amount::parse,
            ValueKind::Amount,
            &[
                "0",
                "5",
                "0.5",
                "10.25",
                max_int,
                "0.0000000001",
                "12345678901234567.1234567891",
            ],
            &[
                "",
                "00",
                "01",
                "1.0",
                "1.50",
                "1.",
                ".5",
                "-1",
                "+1",
                "1e3",
                "0.00000000001",
                "100000000000000000",
                " 1",
                "1 ",
                "1,5",
                "0.0",
            ],
        );
        let a = Amount::parse("0.5").unwrap();
        let b = Amount::parse("10.25").unwrap();
        assert_eq!(a.checked_add(b).unwrap().to_string(), "10.75");
        let max = Amount::parse(max_int).unwrap();
        assert_eq!(max.checked_add(Amount::parse("1").unwrap()), None);
        assert!(Amount::ZERO.is_zero());
        let scaled = Decimal::from_str_exact("2.500").unwrap();
        assert_eq!(Amount::from_decimal(scaled).unwrap().to_string(), "2.5");
        assert!(Amount::from_decimal(Decimal::from_str_exact("-2").unwrap()).is_err());
        assert!(Amount::from_decimal(Decimal::from_str_exact("0.00000000001").unwrap()).is_err());
    }

    #[test]
    fn subtraction_never_goes_negative() {
        let amount = |s: &str| Amount::parse(s).unwrap();
        assert_eq!(
            amount("10.75").checked_sub(amount("0.5")),
            Some(amount("10.25"))
        );
        assert_eq!(amount("3").checked_sub(amount("3")), Some(Amount::ZERO));
        assert_eq!(amount("3").checked_sub(amount("3.0000000001")), None);
        assert_eq!(Amount::ZERO.checked_sub(amount("0.0000000001")), None);
        let max = amount("99999999999999999.9999999999");
        assert_eq!(max.checked_sub(max), Some(Amount::ZERO));
        assert_eq!(
            max.checked_sub(amount("0.0000000001")).unwrap().to_string(),
            "99999999999999999.9999999998"
        );
    }

    #[test]
    fn ratios_compare_exactly() {
        let amount = |s: &str| Amount::parse(s).unwrap();
        let at_least =
            |n: &str, d: &str, floor: &str| amount(n).ratio_at_least(amount(d), amount(floor));
        assert_eq!(at_least("150", "1000", "0.15"), Some(true));
        assert_eq!(at_least("149.9999999999", "1000", "0.15"), Some(false));
        assert_eq!(at_least("1", "3", "0.3333333333"), Some(true));
        assert_eq!(at_least("1", "3", "0.3333333334"), Some(false));
        assert_eq!(at_least("2", "3", "0.6666666666"), Some(true));
        assert_eq!(at_least("2", "3", "0.6666666667"), Some(false));
        assert_eq!(at_least("5", "1", "0"), Some(true));
        assert_eq!(at_least("0", "1", "0"), Some(true));
        assert_eq!(at_least("0", "1", "0.0000000001"), Some(false));
        assert_eq!(at_least("1", "0", "0"), None);
        // Products far past 28 digits, which a rounding multiply could get wrong.
        let max = "99999999999999999.9999999999";
        assert_eq!(at_least(max, max, "1"), Some(true));
        assert_eq!(at_least(max, max, "1.0000000001"), Some(false));
        assert_eq!(at_least("0.0000000001", max, "0"), Some(true));
        assert_eq!(at_least("0.0000000001", max, "0.0000000001"), Some(false));
        assert_eq!(at_least(max, "0.0000000001", max), Some(true));
        let (big, small) = ("12345678901234567.1234567891", "0.1234567891");
        let floor = "99999999928999993.1114099346";
        assert_eq!(at_least(big, small, floor), Some(true));
        let above = "99999999928999993.1114099347";
        assert_eq!(at_least(big, small, above), Some(false));
        assert_eq!(
            at_least("1234567.1234567891", big, "0.0000000001"),
            Some(false)
        );
    }

    #[test]
    fn label_grammar() {
        let max = "a".repeat(64);
        let over = "a".repeat(65);
        table(
            Label::parse,
            ValueKind::Label,
            &["A", "My passkey", "key-1_b.c", "9 lives", &max],
            &[
                "",
                " lead",
                "trail ",
                "two  spaces",
                "-dash",
                ".dot",
                &over,
                "tab\tkey",
                "caf\u{e9}",
                "a=b",
                "a,b",
            ],
        );
    }

    #[test]
    fn text_grammar() {
        let max = "x".repeat(140);
        let over = "x".repeat(141);
        table(
            Text::<140>::parse,
            ValueKind::Text,
            &["Pay rent", "a=b, c: d!", "~", &max],
            &[
                "",
                " lead",
                "trail ",
                "two  spaces",
                &over,
                "new\nline",
                "caf\u{e9}",
                "\u{7f}",
            ],
        );
        assert!(Text::<280>::parse(&over).is_ok());
    }

    #[test]
    fn rp_id_grammar() {
        let label63 = "a".repeat(63);
        let label64 = "a".repeat(64);
        let ok63 = format!("{label63}.com");
        let bad64 = format!("{label64}.com");
        table(
            RpId::parse,
            ValueKind::RpId,
            &[
                "silvana.link",
                "app.silvana.one",
                "localhost",
                "a-b.c0",
                "xn--d1acufc.xn--p1ai",
                &ok63,
            ],
            &[
                "",
                "Silvana.link",
                "silvana",
                "silvana.link.",
                ".silvana.link",
                "a..b",
                "-a.com",
                "a-.com",
                "1.2.3.4",
                "a.123",
                "silvana_link.com",
                "https://silvana.link",
                "silvana.link:443",
                &bad64,
            ],
        );
        assert!(RpId::parse("localhost").unwrap().is_localhost());
    }

    #[test]
    fn origin_grammar() {
        table(
            Origin::parse,
            ValueKind::Origin,
            &[
                "https://silvana.link",
                "https://app.silvana.link",
                "https://app.silvana.link:8443",
                "http://localhost",
                "http://localhost:3000",
            ],
            &[
                "",
                "https://app.silvana.link/",
                "https://APP.silvana.link",
                "https://app.silvana.link:443",
                "https://app.silvana.link:0",
                "https://app.silvana.link:065",
                "https://app.silvana.link:65536",
                "https://app.silvana.link:",
                "http://app.silvana.link",
                "http://localhost:80",
                "https://localhost",
                "https://localhost:3000",
                "https://app.silvana.link/path",
                "https://user@app.silvana.link",
                "https://app.silvana.link?q",
                "ftp://silvana.link",
                "https://1.2.3.4",
                "HTTPS://silvana.link",
                "https://[::1]",
            ],
        );
        let rp = RpId::parse("silvana.link").unwrap();
        let local = RpId::parse("localhost").unwrap();
        let o = |s: &str| Origin::parse(s).unwrap();
        assert!(o("https://silvana.link").matches_rp_id(&rp));
        assert!(o("https://app.silvana.link:8443").matches_rp_id(&rp));
        assert!(!o("https://evilsilvana.link").matches_rp_id(&rp));
        assert!(!o("https://silvana.link.evil.com").matches_rp_id(&rp));
        assert!(!o("http://localhost:3000").matches_rp_id(&rp));
        assert!(o("http://localhost:3000").matches_rp_id(&local));
        assert!(!o("https://app.localhost").matches_rp_id(&local));
        assert_eq!(o("https://app.silvana.link:8443").port(), Some(8443));
        assert_eq!(o("https://app.silvana.link").host(), "app.silvana.link");
    }

    #[test]
    fn uuid7_grammar() {
        table(
            Uuid7::parse,
            ValueKind::Uuid7,
            &[
                "01890a5d-ac96-774b-bcce-b302099a8057",
                "00000000-0000-7000-8000-000000000000",
                "ffffffff-ffff-7fff-bfff-ffffffffffff",
            ],
            &[
                "",
                "01890a5d-ac96-474b-bcce-b302099a8057",
                "01890A5D-AC96-774B-BCCE-B302099A8057",
                "01890a5d-ac96-774b-ccce-b302099a8057",
                "01890a5dac96774bbcceb302099a8057",
                "01890a5d-ac96-774b-bcce-b302099a805",
                "{01890a5d-ac96-774b-bcce-b302099a8057}",
            ],
        );
    }

    #[test]
    fn identifier_grammars() {
        let id64 = "I".repeat(64);
        let id65 = "I".repeat(65);
        table(
            InstrumentId::parse,
            ValueKind::InstrumentId,
            &["Amulet", "USDCx", "c.ETH-1_x", &id64],
            &["", "a/b", "a b", "a:b", &id65],
        );
        table(
            MarketId::parse,
            ValueKind::MarketId,
            &["CC/USDCx", "cETH-USDC", &id64],
            &["", "a b", "a:b", &id65],
        );
        let token256 = "t".repeat(256);
        let token257 = "t".repeat(257);
        table(
            Token::parse,
            ValueKind::Token,
            &["1220abcd:0", "update-1.x_y", &token256],
            &["", "a b", "a/b", "a=b", &token257],
        );
        let eth = "0x52908400098527886e0f7030069857d2e4169ee7";
        table(
            EthAddress::parse,
            ValueKind::EthAddress,
            &[eth],
            &[
                "",
                "52908400098527886e0f7030069857d2e4169ee7",
                "0X52908400098527886e0f7030069857d2e4169ee7",
                "0x52908400098527886E0F7030069857D2E4169EE7",
                "0x52908400098527886e0f7030069857d2e4169ee",
            ],
        );
    }

    #[test]
    fn base64url_is_canonical() {
        assert_eq!(encode_b64url(b"\xfb\xff"), "-_8");
        assert_eq!(decode_b64url("-_8").unwrap(), b"\xfb\xff");
        for bad in ["", "-_8=", "+/8", "-_9", "A", "AB==", "A B", "AA\n"] {
            assert_eq!(decode_b64url(bad), None, "{bad:?}");
        }
        assert_eq!(decode_b64url_bounded("AAAA", 3, 3).unwrap(), vec![0, 0, 0]);
        assert_eq!(decode_b64url_bounded("AAAA", 4, 8), None);
    }
}
