//! UTC times written `YYYY-MM-DDTHH:MM:SSZ`, years 2020 to 9999.

use std::fmt;
use std::str::FromStr;

use crate::error::{ValueError, ValueKind};

/// 2020-01-01T00:00:00Z.
pub const MIN_UNIX_SECS: i64 = 1_577_836_800;
/// 9999-12-31T23:59:59Z.
pub const MAX_UNIX_SECS: i64 = 253_402_300_799;

const SECS_PER_DAY: i64 = 86_400;

/// Whole seconds since the Unix epoch, inside the canonical range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime(i64);

impl UtcTime {
    pub fn from_unix_secs(secs: i64) -> Option<Self> {
        (MIN_UNIX_SECS..=MAX_UNIX_SECS)
            .contains(&secs)
            .then_some(Self(secs))
    }

    pub fn unix_secs(self) -> i64 {
        self.0
    }

    pub fn parse(s: &str) -> Result<Self, ValueError> {
        parse_time(s).ok_or(ValueError(ValueKind::Time))
    }

    /// `None` when the result leaves the canonical range.
    pub fn checked_add_secs(self, secs: i64) -> Option<Self> {
        self.0.checked_add(secs).and_then(Self::from_unix_secs)
    }

    /// Seconds from `self` to `later`; negative when `later` is earlier.
    pub fn secs_until(self, later: Self) -> i64 {
        later.0.saturating_sub(self.0)
    }
}

fn digit(b: u8) -> Option<i64> {
    b.is_ascii_digit().then(|| i64::from(b.wrapping_sub(b'0')))
}

fn two_digits(a: u8, b: u8) -> Option<i64> {
    digit(a)?.checked_mul(10)?.checked_add(digit(b)?)
}

fn divisible(value: i64, by: i64) -> bool {
    value.checked_rem(by) == Some(0)
}

fn is_leap_year(year: i64) -> bool {
    (divisible(year, 4) && !divisible(year, 100)) || divisible(year, 400)
}

fn days_in_month(year: i64, month: i64) -> Option<i64> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date with a non-negative year.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let y = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = y.checked_div(400)?;
    let yoe = y.checked_sub(era.checked_mul(400)?)?;
    let mp = if month > 2 {
        month.checked_sub(3)?
    } else {
        month.checked_add(9)?
    };
    let doy = mp
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day)?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe.checked_div(4)?)?
        .checked_sub(yoe.checked_div(100)?)?
        .checked_add(doy)?;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

/// The date of a non-negative day count since 1970-01-01.
fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let z = days.checked_add(719_468)?;
    let era = z.checked_div(146_097)?;
    let doe = z.checked_sub(era.checked_mul(146_097)?)?;
    let yoe = doe
        .checked_sub(doe.checked_div(1_460)?)?
        .checked_add(doe.checked_div(36_524)?)?
        .checked_sub(doe.checked_div(146_096)?)?
        .checked_div(365)?;
    let year = yoe.checked_add(era.checked_mul(400)?)?;
    let doy = doe.checked_sub(
        yoe.checked_mul(365)?
            .checked_add(yoe.checked_div(4)?)?
            .checked_sub(yoe.checked_div(100)?)?,
    )?;
    let mp = doy.checked_mul(5)?.checked_add(2)?.checked_div(153)?;
    let day = doy
        .checked_sub(mp.checked_mul(153)?.checked_add(2)?.checked_div(5)?)?
        .checked_add(1)?;
    let month = if mp < 10 {
        mp.checked_add(3)?
    } else {
        mp.checked_sub(9)?
    };
    let year = if month <= 2 {
        year.checked_add(1)?
    } else {
        year
    };
    Some((year, month, day))
}

fn parse_time(s: &str) -> Option<UtcTime> {
    let bytes: &[u8; 20] = s.as_bytes().try_into().ok()?;
    let [
        y0,
        y1,
        y2,
        y3,
        b'-',
        mo0,
        mo1,
        b'-',
        d0,
        d1,
        b'T',
        h0,
        h1,
        b':',
        mi0,
        mi1,
        b':',
        s0,
        s1,
        b'Z',
    ] = *bytes
    else {
        return None;
    };
    let year = two_digits(y0, y1)?
        .checked_mul(100)?
        .checked_add(two_digits(y2, y3)?)?;
    let month = two_digits(mo0, mo1)?;
    let day = two_digits(d0, d1)?;
    let hour = two_digits(h0, h1)?;
    let minute = two_digits(mi0, mi1)?;
    let second = two_digits(s0, s1)?;
    if !(2020..=9999).contains(&year) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    if day < 1 || day > days_in_month(year, month)? {
        return None;
    }
    let secs = days_from_civil(year, month, day)?
        .checked_mul(SECS_PER_DAY)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    UtcTime::from_unix_secs(secs)
}

/// Year, month, day, hour, minute and second of an in-range time.
fn fields(secs: i64) -> Option<[i64; 6]> {
    let days = secs.checked_div(SECS_PER_DAY)?;
    let rest = secs.checked_rem(SECS_PER_DAY)?;
    let (year, month, day) = civil_from_days(days)?;
    Some([
        year,
        month,
        day,
        rest.checked_div(3_600)?,
        rest.checked_rem(3_600)?.checked_div(60)?,
        rest.checked_rem(60)?,
    ])
}

impl fmt::Display for UtcTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Out-of-range values cannot be built, so the fallback never prints.
        let [y, mo, d, h, mi, s] = fields(self.0).unwrap_or([0; 6]);
        write!(f, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }
}

impl FromStr for UtcTime {
    type Err = ValueError;

    fn from_str(s: &str) -> Result<Self, ValueError> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_round_trips() {
        let cases = [
            ("2020-01-01T00:00:00Z", MIN_UNIX_SECS),
            ("2024-02-29T23:59:59Z", 1_709_251_199),
            ("2026-11-22T12:34:56Z", 1_795_350_896),
            ("2100-03-01T00:00:00Z", 4_107_542_400),
            ("2400-02-29T00:00:00Z", 13_574_563_200),
            ("9999-12-31T23:59:59Z", MAX_UNIX_SECS),
        ];
        for (text, secs) in cases {
            let t = UtcTime::parse(text).unwrap();
            assert_eq!(t.unix_secs(), secs, "{text}");
            assert_eq!(t.to_string(), text);
        }
    }

    #[test]
    fn refuses_everything_else() {
        let refused = [
            "",
            "2019-12-31T23:59:59Z",
            "10000-01-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2100-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-10T00:00:00Z",
            "2026-11-00T00:00:00Z",
            "2026-11-22T24:00:00Z",
            "2026-11-22T23:60:00Z",
            "2026-11-22T23:59:60Z",
            "2026-11-22T12:00:00.5Z",
            "2026-11-22T12:00:00+00:00",
            "2026-11-22T12:00:00",
            "2026-11-22t12:00:00Z",
            "2026-11-22T12:00:00z",
            "2026-11-22 12:00:00Z",
            "2026/11/22T12:00:00Z",
            "+026-11-22T12:00:00Z",
            "2026-1-22T12:00:00Z ",
            " 2026-11-22T12:00:00Z",
        ];
        for text in refused {
            assert_eq!(
                UtcTime::parse(text),
                Err(ValueError(ValueKind::Time)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn every_day_in_a_span_round_trips() {
        let mut secs = MIN_UNIX_SECS;
        while secs < MIN_UNIX_SECS + 3 * 366 * SECS_PER_DAY {
            let t = UtcTime::from_unix_secs(secs).unwrap();
            assert_eq!(UtcTime::parse(&t.to_string()).unwrap(), t);
            secs += SECS_PER_DAY + 3_607;
        }
    }

    #[test]
    fn arithmetic_is_bounded() {
        let t = UtcTime::parse("2026-11-22T00:00:00Z").unwrap();
        assert_eq!(
            t.checked_add_secs(86_400).unwrap().to_string(),
            "2026-11-23T00:00:00Z"
        );
        assert_eq!(t.checked_add_secs(i64::MAX), None);
        let max = UtcTime::from_unix_secs(MAX_UNIX_SECS).unwrap();
        assert_eq!(max.checked_add_secs(1), None);
        assert_eq!(UtcTime::from_unix_secs(MIN_UNIX_SECS - 1), None);
        assert_eq!(t.secs_until(t.checked_add_secs(5).unwrap()), 5);
        assert_eq!(t.checked_add_secs(5).unwrap().secs_until(t), -5);
    }
}
