//! Exact decimal parsing and comparison of Daml numeric strings.

use rust_decimal::Decimal;

/// True for `-?[0-9]+(\.[0-9]+)?`.
fn is_plain_decimal(s: &str) -> bool {
    let unsigned = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match unsigned.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (unsigned, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// Parses a plain decimal exactly; other forms and values out of range are `None`.
pub(crate) fn parse_exact(s: &str) -> Option<Decimal> {
    if !is_plain_decimal(s) {
        return None;
    }
    Decimal::from_str_exact(s).ok()
}

/// Exact, scale-insensitive equality; unparsable or out-of-range values are unequal.
pub(crate) fn eq_exact(a: &str, b: &str) -> bool {
    match (parse_exact(a), parse_exact(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_exact_ignores_scale() {
        assert!(eq_exact("1", "1.0000000000"));
        assert!(eq_exact("10.5", "10.5000000000"));
        assert!(eq_exact("-0", "0"));
        assert!(eq_exact("-0.0000000000", "0"));
        assert!(eq_exact("007", "7.0"));
        assert!(eq_exact("-2.5", "-2.50"));
    }

    #[test]
    fn eq_exact_refuses_tiny_and_large_differences() {
        // the former f64 tolerance accepted the first pair
        assert!(!eq_exact("1", "1.0000000001"));
        assert!(!eq_exact("1", "1.00000000000000000001"));
        assert!(!eq_exact("12345678901234567890", "12345678901234567891"));
        assert!(!eq_exact("1234567890.1234567890", "1234567890.1234567891"));
        assert!(!eq_exact("1", "-1"));
        assert!(!eq_exact("2", "1"));
    }

    #[test]
    fn eq_exact_refuses_non_daml_forms() {
        for bad in [
            "1e3", "1E3", "1e-3", " 1", "1 ", "1.", ".5", "+1", "", "-", "--1", "1..0", "1.0.0",
            "0x10", "1_000", "١", "NaN", "inf", "1,5",
        ] {
            assert!(!eq_exact(bad, bad), "{bad:?} equals itself");
            assert!(!eq_exact(bad, "1"), "{bad:?}");
            assert!(!eq_exact("1", bad), "{bad:?}");
        }
        assert!(!eq_exact("1e3", "1000"));
    }

    #[test]
    fn values_out_of_range_are_unequal() {
        let huge = "9".repeat(40);
        assert!(parse_exact(&huge).is_none());
        assert!(!eq_exact(&huge, &huge));
        let many_places = format!("1.{}", "1".repeat(40));
        assert!(parse_exact(&many_places).is_none());
        assert!(!eq_exact(&many_places, &many_places));
    }

    #[test]
    fn parse_exact_keeps_every_digit() {
        let d = parse_exact("123456789012345678.0000000001").expect("in range");
        assert_eq!(d.to_string(), "123456789012345678.0000000001");
        assert_eq!(
            parse_exact("-0.5").map(|d| d.to_string()).as_deref(),
            Some("-0.5")
        );
    }
}
