//! Wall-clock, deadline and randomness helpers that never panic.
//! A clock before 1970 reads as epoch 0; `uuid_v7` returns an error instead.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use rand::RngCore;
use uuid::Uuid;

/// Fallback horizon for a deadline whose duration does not fit in an `Instant`.
pub const FAR_HORIZON: Duration = Duration::from_secs(365 * 24 * 3600);

/// Time since the epoch, or `None` when the clock reads before 1970.
pub fn unix_now() -> Option<Duration> {
    since_epoch(SystemTime::now())
}

fn since_epoch(t: SystemTime) -> Option<Duration> {
    t.duration_since(UNIX_EPOCH).ok()
}

fn elapsed_or_zero(t: SystemTime) -> Duration {
    since_epoch(t).unwrap_or_default()
}

fn secs_at(t: SystemTime) -> u64 {
    elapsed_or_zero(t).as_secs()
}

fn millis_at(t: SystemTime) -> u64 {
    u64::try_from(elapsed_or_zero(t).as_millis()).unwrap_or(u64::MAX)
}

fn micros_i64_at(t: SystemTime) -> i64 {
    i64::try_from(elapsed_or_zero(t).as_micros()).unwrap_or(i64::MAX)
}

fn secs_i64_at(t: SystemTime) -> i64 {
    i64::try_from(secs_at(t)).unwrap_or(i64::MAX)
}

fn utc_at(t: SystemTime) -> DateTime<Utc> {
    let d = elapsed_or_zero(t);
    i64::try_from(d.as_secs())
        .ok()
        .and_then(|s| DateTime::from_timestamp(s, d.subsec_nanos()))
        .unwrap_or(DateTime::UNIX_EPOCH)
}

/// Seconds since the epoch (0 before 1970).
pub fn now_secs() -> u64 {
    secs_at(SystemTime::now())
}

/// Seconds since the epoch as `i64` (0 before 1970).
pub fn now_secs_i64() -> i64 {
    secs_i64_at(SystemTime::now())
}

/// Milliseconds since the epoch (0 before 1970).
pub fn now_millis() -> u64 {
    millis_at(SystemTime::now())
}

/// Microseconds since the epoch as `i64` (0 before 1970).
pub fn now_micros_i64() -> i64 {
    micros_i64_at(SystemTime::now())
}

/// Current UTC time; replaces `chrono::Utc::now`, which panics before 1970.
pub fn now_utc() -> DateTime<Utc> {
    utc_at(SystemTime::now())
}

/// A UUIDv7 from the wall clock and the OS RNG; replaces `Uuid::now_v7`.
pub fn uuid_v7() -> Result<Uuid> {
    let mut random = [0u8; 10];
    rand::rngs::OsRng
        .try_fill_bytes(&mut random)
        .map_err(|e| anyhow!("OS random source failed: {e}"))?;
    uuid_v7_at(SystemTime::now(), &random)
}

fn uuid_v7_at(t: SystemTime, random: &[u8; 10]) -> Result<Uuid> {
    let since = since_epoch(t).ok_or_else(|| anyhow!("system clock reads before 1970"))?;
    let ms = u64::try_from(since.as_millis()).context("system clock out of range")?;
    Ok(uuid::Builder::from_unix_timestamp_millis(ms, random).into_uuid())
}

/// `now + d` as a tokio deadline; a duration too large for an `Instant`
/// falls back to `FAR_HORIZON`, then to `now`.
pub fn deadline_after(d: Duration) -> tokio::time::Instant {
    deadline_from(tokio::time::Instant::now(), d)
}

fn deadline_from(now: tokio::time::Instant, d: Duration) -> tokio::time::Instant {
    now.checked_add(d)
        .or_else(|| now.checked_add(FAR_HORIZON))
        .unwrap_or(now)
}

/// Random jitter in `0..max` milliseconds from the OS RNG; 0 if `max` is 0
/// or the RNG fails.
pub fn jitter_ms(max: u64) -> u64 {
    let mut bytes = [0u8; 8];
    match rand::rngs::OsRng.try_fill_bytes(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes).checked_rem(max).unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre_epoch() -> SystemTime {
        UNIX_EPOCH.checked_sub(Duration::from_secs(3600)).unwrap()
    }

    // A clock before 1970 reads as epoch 0 instead of panicking
    #[test]
    fn pre_epoch_clock_reads_as_epoch_zero() {
        let t = pre_epoch();
        assert_eq!(since_epoch(t), None);
        assert_eq!(secs_at(t), 0);
        assert_eq!(secs_i64_at(t), 0);
        assert_eq!(millis_at(t), 0);
        assert_eq!(micros_i64_at(t), 0);
        assert_eq!(utc_at(t), DateTime::UNIX_EPOCH);
    }

    #[test]
    fn pre_epoch_clock_fails_uuid_v7() {
        let err = uuid_v7_at(pre_epoch(), &[7u8; 10]).unwrap_err();
        assert!(err.to_string().contains("before 1970"), "{err}");
    }

    #[test]
    fn epoch_values_match_the_clock() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(secs_at(t), 1_700_000_000);
        assert_eq!(secs_i64_at(t), 1_700_000_000);
        assert_eq!(millis_at(t), 1_700_000_000_123);
        assert_eq!(micros_i64_at(t), 1_700_000_000_123_000);
        assert_eq!(utc_at(t).timestamp_millis(), 1_700_000_000_123);
    }

    #[test]
    fn uuid_v7_carries_the_timestamp_and_version() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let id = uuid_v7_at(t, &[0xAB; 10]).unwrap();
        assert_eq!(id.get_version_num(), 7);
        let (secs, nanos) = id.get_timestamp().unwrap().to_unix();
        assert_eq!((secs, nanos), (1_700_000_000, 123_000_000));
        let live = uuid_v7().unwrap();
        assert_eq!(live.get_version_num(), 7);
        assert_ne!(live, uuid_v7().unwrap());
    }

    #[test]
    fn live_clock_is_after_2020() {
        assert!(now_secs() > 1_577_836_800);
        assert!(now_secs_i64() > 1_577_836_800);
        assert!(now_millis() > 1_577_836_800_000);
        assert!(now_micros_i64() > 1_577_836_800_000_000);
        assert!(now_utc().timestamp() > 1_577_836_800);
        assert!(unix_now().is_some());
    }

    // Duration::MAX would overflow `Instant + Duration`
    #[test]
    fn deadline_after_a_huge_duration_does_not_panic() {
        let now = tokio::time::Instant::now();
        let far = deadline_from(now, Duration::MAX);
        assert!(far > now);
        assert!(far.duration_since(now) <= FAR_HORIZON);
        assert_eq!(deadline_from(now, Duration::from_secs(5)).duration_since(now), Duration::from_secs(5));
        assert!(deadline_after(Duration::MAX) > tokio::time::Instant::now());
    }

    #[test]
    fn jitter_stays_in_range() {
        assert_eq!(jitter_ms(0), 0);
        assert_eq!(jitter_ms(1), 0);
        for _ in 0..1000 {
            assert!(jitter_ms(800) < 800);
        }
        let distinct: std::collections::HashSet<u64> = (0..64).map(|_| jitter_ms(u64::MAX)).collect();
        assert!(distinct.len() > 1, "jitter must vary");
    }
}
