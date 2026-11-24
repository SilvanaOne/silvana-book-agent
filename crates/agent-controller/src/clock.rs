//! The local clock, and the rpc's as its answers report it. Nothing is signed while the
//! two differ by more than [`MAX_CLOCK_SKEW_SECS`].

use std::sync::Mutex;

use agent_control::UtcTime;
use tonic::metadata::MetadataMap;

use crate::error::ControllerError;

/// Largest gap between this clock and the rpc's before anything is signed.
pub const MAX_CLOCK_SKEW_SECS: i64 = 60;
/// A reading older than this no longer counts (it covers a typed confirmation).
const SAMPLE_MAX_AGE_SECS: i64 = 900;

/// The local clock in seconds since the epoch.
pub fn local_secs() -> Result<i64, ControllerError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| ControllerError::Clock("the local clock is unreadable".into()))
}

/// The local clock as a document time.
pub fn local_now() -> Result<UtcTime, ControllerError> {
    UtcTime::from_unix_secs(local_secs()?)
        .ok_or_else(|| ControllerError::Clock("the local clock is out of range".into()))
}

/// The `date` header, in seconds since the epoch.
pub fn date_header_secs(metadata: &MetadataMap) -> Option<i64> {
    let text = metadata.get("date").and_then(|v| v.to_str().ok())?;
    chrono::DateTime::parse_from_rfc2822(text)
        .ok()
        .map(|at| at.timestamp())
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    offset_secs: i64,
    read_at: i64,
}

/// The last reading of the rpc's clock.
#[derive(Debug, Default)]
pub struct Clock {
    sample: Mutex<Option<Sample>>,
}

impl Clock {
    /// Records the rpc's time from an answer.
    pub fn observe(&self, server_secs: i64) {
        if server_secs <= 0 {
            return;
        }
        if let Ok(local) = local_secs() {
            let sample = Sample {
                offset_secs: server_secs.saturating_sub(local),
                read_at: local,
            };
            *self
                .sample
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sample);
        }
    }

    /// The time to sign at: now, when this clock is within [`MAX_CLOCK_SKEW_SECS`] of the
    /// rpc's as last read.
    pub fn checked_now(&self) -> Result<UtcTime, ControllerError> {
        let local = local_secs()?;
        let sample = *self
            .sample
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sample) = sample else {
            return Err(ControllerError::Clock(
                "the rpc's time is unknown (no answer carried it); retry".into(),
            ));
        };
        if local.saturating_sub(sample.read_at).unsigned_abs() > SAMPLE_MAX_AGE_SECS.unsigned_abs()
        {
            return Err(ControllerError::Clock(
                "the rpc's time was read too long ago; retry".into(),
            ));
        }
        let skew = sample.offset_secs.unsigned_abs();
        if skew > MAX_CLOCK_SKEW_SECS.unsigned_abs() {
            return Err(ControllerError::Clock(format!(
                "this machine's clock is {skew}s {} the rpc's (at most {MAX_CLOCK_SKEW_SECS}s allowed): fix the clock and retry",
                if sample.offset_secs > 0 {
                    "behind"
                } else {
                    "ahead of"
                }
            )));
        }
        local_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_needs_a_recent_reading_within_sixty_seconds() {
        let clock = Clock::default();
        assert!(
            clock
                .checked_now()
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        let now = local_secs().unwrap();
        clock.observe(now + 59);
        clock.checked_now().unwrap();
        clock.observe(now - 90);
        let err = clock.checked_now().unwrap_err().to_string();
        assert!(err.contains("ahead of the rpc's"), "{err}");
        clock.observe(now + 61);
        assert!(
            clock
                .checked_now()
                .unwrap_err()
                .to_string()
                .contains("behind")
        );
        clock.observe(0);
        assert!(clock.checked_now().is_err(), "a zero reading is ignored");
    }
}
