//! Overflow-safe `Decimal` sums and display, and char-safe string truncation.

use std::borrow::{Borrow, Cow};
use std::fmt;

use rust_decimal::{Decimal, RoundingStrategy};

/// Sum of the items, or `None` on overflow (`Iterator::sum` panics instead).
pub fn dec_sum<I>(items: I) -> Option<Decimal>
where
    I: IntoIterator,
    I::Item: Borrow<Decimal>,
{
    items
        .into_iter()
        .try_fold(Decimal::ZERO, |acc, d| acc.checked_add(*d.borrow()))
}

/// Sum of the items, clamped at `Decimal::MAX`/`MIN` on overflow.
pub fn dec_sum_saturating<I>(items: I) -> Decimal
where
    I: IntoIterator,
    I::Item: Borrow<Decimal>,
{
    items
        .into_iter()
        .fold(Decimal::ZERO, |acc, d| acc.saturating_add(*d.borrow()))
}

/// Displays a `Decimal` cut to `n` places and padded with zeros, like `{:.n}`
/// (which truncates toward zero) but without its panic on large values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dp(pub Decimal, pub u32);

impl fmt::Display for Dp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let places = self.1.min(Decimal::MAX_SCALE);
        let mut v = self.0.round_dp_with_strategy(places, RoundingStrategy::ToZero);
        v.rescale(places);
        // The sign follows the input, as `{:.n}` prints "-0" for a negative value cut to zero
        let negative = self.0.is_sign_negative() && !self.0.is_zero();
        f.pad_integral(!negative, "", &v.abs().to_string())
    }
}

/// Displays an `f64` like `{:.n}` (n capped at 64), but a nonzero value below
/// 1e-3 gets enough places for six significant digits, capped at 24 places.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Px(pub f64, pub usize);

const PX_MAX_PLACES: usize = 24;
const PX_MAX_N: usize = 64;
const PX_POW10: [f64; PX_MAX_PLACES] = [
    1e-1, 1e-2, 1e-3, 1e-4, 1e-5, 1e-6, 1e-7, 1e-8, 1e-9, 1e-10, 1e-11, 1e-12, 1e-13, 1e-14,
    1e-15, 1e-16, 1e-17, 1e-18, 1e-19, 1e-20, 1e-21, 1e-22, 1e-23, 1e-24,
];

impl fmt::Display for Px {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Px(v, n) = *self;
        // A runtime precision above u16::MAX panics inside fmt.
        let n = n.min(PX_MAX_N);
        let a = v.abs();
        let places = if v.is_finite() && a > 0.0 && a < 1e-3 {
            let zeros = PX_POW10.iter().take_while(|&&p| a < p).count();
            n.max(zeros.saturating_add(6).min(PX_MAX_PLACES))
        } else {
            n
        };
        write!(f, "{v:.places$}")
    }
}

/// The first `n` characters of `s` (all of `s` if shorter).
pub fn short(s: &str, n: usize) -> &str {
    s.char_indices()
        .nth(n)
        .and_then(|(i, _)| s.get(..i))
        .unwrap_or(s)
}

/// The last `n` characters of `s` (all of `s` if shorter).
pub fn tail(s: &str, n: usize) -> &str {
    let Some(skip) = n.checked_sub(1) else { return "" };
    s.char_indices()
        .rev()
        .nth(skip)
        .and_then(|(i, _)| s.get(i..))
        .unwrap_or(s)
}

/// `head...tail` form of a long identifier (12 + 8 characters); shorter ids are unchanged.
pub fn short_id(s: &str) -> Cow<'_, str> {
    if s.chars().nth(24).is_some() {
        Cow::Owned(format!("{}...{}", short(s, 12), tail(s, 8)))
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    // Tiny values keep six significant digits; ordinary ones print exactly as {:.n}
    #[test]
    fn px_keeps_tiny_values_significant() {
        assert_eq!(Px(1.8463e-8, 6).to_string(), "0.0000000184630");
        assert_eq!(Px(1.8463189766e-8, 6).to_string(), "0.0000000184632");
        assert_eq!(Px(-1.6012e-9, 6).to_string(), "-0.00000000160120");
        assert_eq!(Px(2e-4, 6).to_string(), "0.000200000");
        assert_eq!(Px(9.99e-4, 6).to_string(), "0.000999000");
        assert_eq!(Px(1.6e-9, 20).to_string(), format!("{:.20}", 1.6e-9));
        assert_eq!(Px(1e-30, 6).to_string(), format!("{:.24}", 1e-30));
        for (v, n) in [(0.0794, 6), (0.0794123456789, 10), (1234.5678, 6), (1e-3, 6), (0.0, 6), (-0.5, 4)] {
            assert_eq!(Px(v, n).to_string(), format!("{v:.n$}"), "{v} {n}");
        }
        assert_eq!(Px(f64::NAN, 6).to_string(), "NaN");
        assert_eq!(Px(f64::INFINITY, 6).to_string(), "inf");
        // An absurd precision is capped instead of panicking inside fmt.
        assert_eq!(Px(0.08, usize::MAX).to_string(), format!("{:.64}", 0.08));
    }

    // Iterator::sum panics here; dec_sum reports the overflow
    #[test]
    fn dec_sum_at_decimal_max() {
        assert_eq!(dec_sum([Decimal::MAX, Decimal::ONE]), None);
        assert_eq!(dec_sum([Decimal::MIN, -Decimal::ONE].iter()), None);
        assert_eq!(dec_sum([Decimal::MAX, Decimal::MIN]), Some(Decimal::ZERO));
        assert_eq!(dec_sum(&vec![d("1.5"), d("2.25")][..]), Some(d("3.75")));
        assert_eq!(dec_sum(Vec::<Decimal>::new()), Some(Decimal::ZERO));
    }

    #[test]
    fn dec_sum_saturating_at_decimal_max() {
        assert_eq!(dec_sum_saturating([Decimal::MAX, Decimal::MAX]), Decimal::MAX);
        assert_eq!(dec_sum_saturating([Decimal::MIN, Decimal::MIN]), Decimal::MIN);
        assert_eq!(dec_sum_saturating([d("0.1"), d("0.2")].iter()), d("0.3"));
    }

    // `{:.4}` on these values panics inside rust_decimal
    #[test]
    fn dp_at_decimal_max() {
        assert_eq!(Dp(Decimal::MAX, 4).to_string(), "79228162514264337593543950335");
        assert_eq!(Dp(Decimal::MIN, 10).to_string(), "-79228162514264337593543950335");
        assert_eq!(Dp(d("1000000000000000000000"), 10).to_string(), "1000000000000000000000.0000000");
        assert_eq!(Dp(d("0.0000000000000000000000000001"), 40).to_string(), "0.0000000000000000000000000001");
        let wide = Dp(d("123.45"), 99).to_string();
        assert!(wide.starts_with("123.45000") && wide.len() <= 32, "{wide}");
    }

    #[test]
    fn dp_matches_fixed_precision_for_normal_values() {
        assert_eq!(Dp(d("1.5"), 4).to_string(), "1.5000");
        assert_eq!(Dp(d("2"), 2).to_string(), "2.00");
        assert_eq!(Dp(d("1.23456"), 4).to_string(), "1.2345");
        assert_eq!(Dp(d("-0.5"), 2).to_string(), "-0.50");
        assert_eq!(Dp(d("12.3"), 0).to_string(), "12");
        assert_eq!(format!("[{:>8}]", Dp(d("1.5"), 2)), "[    1.50]");
        assert_eq!(format!("{:.1}", Dp(d("1.5"), 3)), "1.500", "outer precision is ignored");
    }

    #[test]
    fn short_and_tail_respect_char_boundaries() {
        assert_eq!(short("abcdef", 3), "abc");
        assert_eq!(short("ab", 3), "ab");
        assert_eq!(short("", 3), "");
        assert_eq!(short("абвгд", 2), "аб");
        assert_eq!(short("ab", 0), "");
        assert_eq!(tail("abcdef", 2), "ef");
        assert_eq!(tail("ab", 5), "ab");
        assert_eq!(tail("абвгд", 2), "гд");
        assert_eq!(tail("abc", 0), "");
    }

    #[test]
    fn short_id_matches_the_head_tail_form() {
        let id = "00a1b2c3d4e5f60718293a4b5c6d7e8f90";
        assert_eq!(short_id(id), "00a1b2c3d4e5...6d7e8f90");
        assert_eq!(short_id("short::1220"), "short::1220");
        let exactly_24 = "abcdefghijklmnopqrstuvwx";
        assert_eq!(short_id(exactly_24), exactly_24);
        let multibyte = "пользователь::1220abcdef0123456789";
        assert_eq!(short_id(multibyte), "пользователь...23456789");
    }

    #[test]
    fn dp_matches_precision_format() {
        let values = [
            "1.239", "1.235", "-1.239", "0.0001", "-0.5", "2.5", "123.456789", "1.00000000006",
            "999999.99999", "0", "12", "-0.001", "-0.4",
        ];
        for v in values {
            for n in [0u32, 1, 2, 4, 10] {
                let x = d(v);
                let want = format!("{:.*}", n as usize, x);
                assert_eq!(Dp(x, n).to_string(), want, "value {v} at {n} places");
            }
        }
    }

}
