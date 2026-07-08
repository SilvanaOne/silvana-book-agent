//! Ledger-submission health circuit breaker.
//!
//! Tracks whether transaction submission to the Canton sequencer is currently
//! failing (e.g. `SEQUENCER_REQUEST_FAILED` / "Failed to send command" during a
//! sequencer outage). When it trips, the RFQ handler stops quoting so the agent
//! does not keep reserving inventory it cannot settle — the reservation-pool
//! saturation that leaves the book pinned at ~0 available while balances sit
//! idle on-chain.
//!
//! Two independent process-global atomics, mirroring the `fetch_max`-on-deadline
//! pattern in `cloud-agent`'s `signal_sequencer_backpressure`:
//!
//! - `CONSECUTIVE_FAILURES` — count of back-to-back submission failures with no
//!   intervening success. Provides hysteresis: a single blip does not trip the
//!   breaker; only `threshold` consecutive failures do.
//! - `UNHEALTHY_UNTIL_MS` — epoch-ms deadline after which the breaker
//!   auto-re-opens. This TIME-BASED reset is essential: once quoting stops there
//!   are no submissions left to observe a recovery, so a success-only reset would
//!   latch the book dark forever. After the cooldown elapses the agent resumes
//!   quoting to *probe* the ledger; a probe success clears the breaker fully, a
//!   probe failure re-trips it.
//!
//! `record_submit_success` clears everything immediately (the ledger is back).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Consecutive submission failures with no intervening success.
static CONSECUTIVE_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Epoch millis after which the breaker auto-re-opens (0 = healthy).
static UNHEALTHY_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Epoch millis when the breaker most recently transitioned into the unhealthy
/// state (0 = healthy). Used by callers that only want to act on a *sustained*
/// outage rather than a momentary trip.
static UNHEALTHY_SINCE_MS: AtomicU64 = AtomicU64::new(0);

/// Consecutive sequencer-unreachable failures required to trip the breaker.
static FAILURE_THRESHOLD: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("LEDGER_UNHEALTHY_FAILURE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
});

/// How long the breaker stays tripped after a failure before it auto-re-opens to
/// probe the ledger. Each new failure extends the deadline.
static COOLDOWN_SECS: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("LEDGER_UNHEALTHY_COOLDOWN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
});

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Record one sequencer-unreachable submission failure. Call **once per logical
/// submission attempt** (not per internal retry) so the threshold reflects real
/// consecutive failures. Trips the breaker once `threshold` consecutive failures
/// accumulate, and (re)extends the cooldown deadline while failures continue.
pub fn record_submit_failure() {
    let failures = CONSECUTIVE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    if failures < *FAILURE_THRESHOLD {
        return;
    }
    let was_healthy = UNHEALTHY_UNTIL_MS.load(Ordering::Relaxed) <= now_ms();
    let resume_at = now_ms() + *COOLDOWN_SECS * 1000;
    UNHEALTHY_UNTIL_MS.fetch_max(resume_at, Ordering::Relaxed);
    if was_healthy {
        UNHEALTHY_SINCE_MS.store(now_ms(), Ordering::Relaxed);
        warn!(
            "Ledger submission unhealthy ({} consecutive failures) — pausing RFQ quoting for up to {}s",
            failures, *COOLDOWN_SECS
        );
    }
}

/// Record a successful submission — the ledger is reachable. Clears the breaker
/// immediately (both the failure counter and the cooldown deadline).
pub fn record_submit_success() {
    let prev = CONSECUTIVE_FAILURES.swap(0, Ordering::Relaxed);
    let was_unhealthy = UNHEALTHY_UNTIL_MS.swap(0, Ordering::Relaxed) > now_ms();
    UNHEALTHY_SINCE_MS.store(0, Ordering::Relaxed);
    if was_unhealthy || prev >= *FAILURE_THRESHOLD {
        info!("Ledger submission healthy again — resuming RFQ quoting");
    }
}

/// Whether ledger submission is currently considered unhealthy (breaker tripped
/// and cooldown not yet elapsed). Auto-clears once the cooldown passes even with
/// no intervening success, so the agent resumes quoting to probe the ledger.
pub fn is_unhealthy() -> bool {
    now_ms() < UNHEALTHY_UNTIL_MS.load(Ordering::Relaxed)
}

/// Epoch millis since the breaker last tripped, or `None` if currently healthy.
/// Lets callers gate on a *sustained* outage (e.g. only shorten abandon windows
/// after the outage has persisted for a while).
pub fn unhealthy_since_ms() -> Option<u64> {
    if !is_unhealthy() {
        return None;
    }
    match UNHEALTHY_SINCE_MS.load(Ordering::Relaxed) {
        0 => None,
        since => Some(since),
    }
}

/// Millis the breaker has been continuously tripped, or 0 if healthy.
pub fn unhealthy_duration_ms() -> u64 {
    unhealthy_since_ms()
        .map(|since| now_ms().saturating_sub(since))
        .unwrap_or(0)
}

/// Whether a submission-failure message indicates the sequencer/ledger is
/// unreachable (a true connectivity outage) as opposed to a business rejection
/// of a specific command. Only the former should trip the breaker — an
/// `INACTIVE_CONTRACTS` / `PRECONDITION_FAILED` / `DUPLICATE_COMMAND` rejection
/// means the ledger is up and processing, so quoting must continue.
///
/// `SEQUENCER_BACKPRESSURE` is deliberately EXCLUDED: it is transient congestion
/// (the ledger is up), already handled by the graduated forecast RFQ gate and
/// the fee-pause `signal_sequencer_backpressure` mechanism. Tripping the hard
/// ledger-wide breaker on it would needlessly dark all quoting for the cooldown.
///
/// Matching is case-insensitive so both the gRPC status token (`UNAVAILABLE`)
/// and the transport phrasing ("the service is currently unavailable") trip it.
pub fn is_sequencer_unreachable(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    const BUSINESS_REJECTIONS: [&str; 3] =
        ["inactive_contracts", "precondition_failed", "duplicate_command"];
    if BUSINESS_REJECTIONS.iter().any(|r| lower.contains(r)) {
        return false;
    }
    const UNREACHABLE: [&str; 4] = [
        "sequencer_request_failed",
        "failed to send command",
        "unavailable",
        "transport error",
    ];
    UNREACHABLE.iter().any(|s| lower.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate process-global state, so they must not run
    // concurrently with each other. `cargo test` runs tests in a module on
    // separate threads, so serialize via a mutex and reset state at the start.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset() {
        CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
        UNHEALTHY_UNTIL_MS.store(0, Ordering::Relaxed);
        UNHEALTHY_SINCE_MS.store(0, Ordering::Relaxed);
    }

    #[test]
    fn below_threshold_stays_healthy() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        // threshold defaults to 3
        record_submit_failure();
        record_submit_failure();
        assert!(!is_unhealthy(), "2 failures < threshold 3 must not trip");
        assert!(unhealthy_since_ms().is_none());
    }

    #[test]
    fn threshold_trips_and_success_clears() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        for _ in 0..3 {
            record_submit_failure();
        }
        assert!(is_unhealthy(), "3 consecutive failures must trip");
        assert!(unhealthy_since_ms().is_some());
        record_submit_success();
        assert!(!is_unhealthy(), "a success must clear the breaker immediately");
        assert!(unhealthy_since_ms().is_none());
    }

    #[test]
    fn success_resets_consecutive_counter() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        record_submit_failure();
        record_submit_failure();
        record_submit_success();
        // Counter reset — need a fresh full run of `threshold` to trip.
        record_submit_failure();
        record_submit_failure();
        assert!(!is_unhealthy(), "counter must reset on success");
        record_submit_failure();
        assert!(is_unhealthy());
        reset();
    }

    #[test]
    fn time_based_probe_auto_reopens_without_success() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        // Trip, then simulate cooldown elapsing by rewinding the deadline into
        // the past — models "backlog drained, no submissions left to observe".
        for _ in 0..3 {
            record_submit_failure();
        }
        assert!(is_unhealthy());
        UNHEALTHY_UNTIL_MS.store(now_ms().saturating_sub(1), Ordering::Relaxed);
        assert!(!is_unhealthy(), "breaker must auto-re-open after cooldown to probe");
        reset();
    }

    #[test]
    fn sequencer_unreachable_matches_outage_not_business_rejections() {
        assert!(is_sequencer_unreachable(
            "Status { code: Aborted, message: SEQUENCER_REQUEST_FAILED(2, ...): Failed to send command"
        ));
        assert!(is_sequencer_unreachable("transport error: UNAVAILABLE"));
        // Case-insensitive: ledger-service transport phrasing must trip it too.
        assert!(is_sequencer_unreachable(
            "ExecuteTransaction RPC failed (The service is currently unavailable): ..."
        ));
        assert!(is_sequencer_unreachable("PrepareSubmission failed: transport error"));
        // Business rejections must NOT trip it.
        assert!(!is_sequencer_unreachable("INACTIVE_CONTRACTS(...)"));
        assert!(!is_sequencer_unreachable("DA.Exception.PreconditionFailed / PRECONDITION_FAILED"));
        assert!(!is_sequencer_unreachable("DUPLICATE_COMMAND"));
        // Backpressure is transient congestion (ledger up) — handled by the
        // fee-pause + forecast gate, must NOT trip the hard breaker.
        assert!(!is_sequencer_unreachable(
            "Status { message: SEQUENCER_BACKPRESSURE: too many requests }"
        ));
    }
}
