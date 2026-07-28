//! Global issuance forecast state.
//!
//! Polled from the orderbook RPC (`GetRoundsData`) by the background task
//! [`spawn_forecast_poller`]. It deliberately does NOT ride the runner's main
//! `tokio::select!` loop: that loop is `biased`, and a low-frequency arm placed
//! after the 2s/5s/7s timers is unreachable once an iteration body takes longer
//! than the shortest timer period. Mainnet ran for days with a coefficient
//! frozen at its startup value because of exactly that.
//!
//! Two independent pause thresholds protect against sequencer overload:
//!
//! 1. **Traffic fees** are paused when forecast is LOW (coefficient < 0.6).
//!    LOW means the featured app is generating heavy sequencer load, so
//!    traffic fee transactions would likely hit SEQUENCER_BACKPRESSURE errors.
//!
//! 2. **Normal fees** (DVP/allocation) are paused when the predicted coefficient
//!    drops below `SEQUENCER_OVERLOAD_THRESHOLD` (default 0.5).  This is a more
//!    severe threshold — at this level the sequencer is extremely overloaded and
//!    even normal fee transactions would hit SEQUENCER_BACKPRESSURE.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Current IssuanceForecast enum value (0=Unspecified, 1=LOW, 2=MEDIUM, 3=HIGH).
static FORECAST: AtomicI32 = AtomicI32::new(0);

/// The predicted coefficient string used for the forecast (for logging).
static FORECAST_COEFFICIENT: Mutex<Option<String>> = Mutex::new(None);

/// The predicted coefficient as f64 (for overload threshold comparison).
static FORECAST_COEFF_VALUE: Mutex<f64> = Mutex::new(0.0);

/// Whether the previous forecast was LOW (for detecting traffic pause transitions).
static WAS_LOW: AtomicBool = AtomicBool::new(false);

/// Whether fees were previously paused by overload (for detecting transitions).
static WAS_OVERLOADED: AtomicBool = AtomicBool::new(false);

/// Epoch-seconds of the last `update_forecast` that carried a prediction.
/// 0 means "never" — every gate that reads the coefficient is acting on a
/// startup default until this is non-zero.
static LAST_UPDATE_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);

/// Interval for the background poller (`FORECAST_POLL_SECS`, default 30).
///
/// This is deliberately much faster than the data: the server republishes the
/// predicted coefficient only about every 10 minutes, so ~20 of every 21 polls
/// re-read a value that has not moved. That redundancy is the point. Every
/// successful poll restamps `LAST_UPDATE_EPOCH_SECS` whether or not the value
/// changed, which turns [`forecast_age_secs`] into a liveness signal for the
/// poller itself rather than a measure of how fresh the prediction is — a dead
/// poller shows up in minutes instead of hiding behind the 10-minute cadence.
/// It also means a newly published coefficient reaches the gates within 30s
/// rather than up to a full poll period late.
static POLL_SECS: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("FORECAST_POLL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(30)
});

/// Set once the poller task exists, so every entry point can call
/// `spawn_forecast_poller` unconditionally.
static POLLER_SPAWNED: AtomicBool = AtomicBool::new(false);

/// Warn after this many consecutive poll failures (not on every one — a poll
/// runs every 30s and transient RPC errors are normal).
const POLL_FAILURES_BEFORE_WARN: u32 = 3;

/// Rebuild the client every N consecutive failures. Auth self-refreshes via the
/// interceptor, so a persistent failure is far more likely to be a dead
/// transport than an expired token.
const POLL_FAILURES_BEFORE_RECONNECT: u32 = 5;

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Predicted coefficient threshold below which normal fees are paused.
/// When the coefficient drops below this, the sequencer is extremely overloaded
/// and fee transactions would hit SEQUENCER_BACKPRESSURE errors.
static OVERLOAD_THRESHOLD: LazyLock<f64> = LazyLock::new(|| {
    std::env::var("SEQUENCER_OVERLOAD_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5)
});

/// Update the global forecast state.  Logs warn on transition to LOW
/// (traffic paused) and info on transition away from LOW (traffic resumed).
/// Also tracks overload threshold for normal fee pausing.
pub fn update_forecast(forecast_value: i32, coefficient: Option<String>) {
    let prev_low = WAS_LOW.load(Ordering::Relaxed);
    let now_low = forecast_value == 1; // ISSUANCE_FORECAST_LOW

    // Parse and store the coefficient value for overload threshold comparison
    let coeff_f64 = coefficient
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    *FORECAST_COEFF_VALUE.lock().unwrap() = coeff_f64;

    FORECAST.store(forecast_value, Ordering::Relaxed);
    *FORECAST_COEFFICIENT.lock().unwrap() = coefficient;
    LAST_UPDATE_EPOCH_SECS.store(now_epoch_secs(), Ordering::Relaxed);

    // Traffic fee pause transitions (LOW threshold)
    if now_low && !prev_low {
        warn!("Issuance forecast LOW — pausing traffic fee processing (heavy sequencer load expected)");
        WAS_LOW.store(true, Ordering::Relaxed);
    } else if !now_low && prev_low {
        info!(
            "Issuance forecast {} — resuming traffic fee processing",
            forecast_label()
        );
        WAS_LOW.store(false, Ordering::Relaxed);
    }

    // Normal fee overload transitions (SEQUENCER_OVERLOAD_THRESHOLD)
    let prev_overloaded = WAS_OVERLOADED.load(Ordering::Relaxed);
    let now_overloaded = coeff_f64 > 0.0 && coeff_f64 < *OVERLOAD_THRESHOLD;
    if now_overloaded && !prev_overloaded {
        warn!(
            "Predicted coefficient {:.4} below overload threshold {:.2} — pausing normal fee processing",
            coeff_f64, *OVERLOAD_THRESHOLD
        );
        WAS_OVERLOADED.store(true, Ordering::Relaxed);
    } else if !now_overloaded && prev_overloaded {
        info!(
            "Predicted coefficient {:.4} above overload threshold {:.2} — resuming normal fee processing",
            coeff_f64, *OVERLOAD_THRESHOLD
        );
        WAS_OVERLOADED.store(false, Ordering::Relaxed);
    }
}

/// Returns `true` when traffic fees should be paused.
///
/// Traffic fees are paused when the issuance forecast is LOW because a low
/// coefficient means the featured app is generating heavy transaction volume
/// on the sequencer, so traffic fee transactions would likely hit
/// SEQUENCER_BACKPRESSURE errors.
pub fn is_traffic_paused_by_forecast() -> bool {
    FORECAST.load(Ordering::Relaxed) == 1
}

/// Returns `true` when normal fees should be paused due to sequencer overload.
///
/// Normal fees (DVP/allocation PayFee) are paused when the predicted coefficient
/// drops below `SEQUENCER_OVERLOAD_THRESHOLD` (default 0.5).  At this level
/// the sequencer is extremely overloaded and fee transactions would hit
/// SEQUENCER_BACKPRESSURE errors.
pub fn is_fees_paused_by_overload() -> bool {
    let coeff = *FORECAST_COEFF_VALUE.lock().unwrap();
    coeff > 0.0 && coeff < *OVERLOAD_THRESHOLD
}

/// Returns `true` when RFQs should be rejected due to extreme sequencer overload.
///
/// RFQs are rejected when the predicted coefficient drops below
/// `SEQUENCER_OVERLOAD_THRESHOLD - 0.1` (default 0.4).  At this level the
/// sequencer is critically overloaded — proposing new trades would fail
/// with SEQUENCER_BACKPRESSURE errors.
pub fn is_rfq_rejected_by_overload() -> bool {
    let coeff = *FORECAST_COEFF_VALUE.lock().unwrap();
    coeff > 0.0 && coeff < *OVERLOAD_THRESHOLD - 0.1
}

/// Human-readable label for the current forecast.
pub fn forecast_label() -> &'static str {
    match FORECAST.load(Ordering::Relaxed) {
        1 => "low",
        2 => "medium",
        3 => "high",
        _ => "unknown",
    }
}

/// The predicted coefficient value (for heartbeat logging).
pub fn forecast_coefficient() -> Option<String> {
    FORECAST_COEFFICIENT.lock().unwrap().clone()
}

/// The predicted coefficient as f64. Returns 0.0 when no forecast has been
/// received yet — callers gating on "coefficient must exceed a threshold"
/// therefore stay paused until the first real forecast arrives.
pub fn coefficient_value() -> f64 {
    *FORECAST_COEFF_VALUE.lock().unwrap()
}

/// Seconds since the coefficient was last refreshed, or `None` if no forecast
/// has ever landed.
///
/// The `None` case is deliberately distinct from a large age: it separates "the
/// poller has not produced its first result yet" (normal for the first seconds
/// of a process) from "the poller is dead and every gate is reading a frozen
/// value" (an incident). Gates that pause on a low coefficient should surface
/// the second case loudly — a stale reading is indistinguishable from a real
/// one at the call site.
pub fn forecast_age_secs() -> Option<u64> {
    match LAST_UPDATE_EPOCH_SECS.load(Ordering::Relaxed) {
        0 => None,
        last => Some(now_epoch_secs().saturating_sub(last)),
    }
}

/// Spawn the background issuance-forecast poller.
///
/// Idempotent: the second and subsequent calls in a process are no-ops, so
/// every entry point may call it unconditionally without coordinating.
///
/// Owns its own [`crate::client::OrderbookClient`] and its own task so that no
/// amount of load on the runner's main loop can starve it. Auth needs no
/// special handling — `AuthInterceptor` regenerates the JWT on every request
/// once it is within 300s of expiry — but the transport can die, so the client
/// is rebuilt after repeated failures.
pub fn spawn_forecast_poller(config: crate::config::BaseConfig, shutdown: crate::shutdown::Shutdown) {
    if POLLER_SPAWNED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        let period = Duration::from_secs(*POLL_SECS);
        info!("Issuance forecast poller started: interval={}s", period.as_secs());

        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut client: Option<crate::client::OrderbookClient> = None;
        let mut failures: u32 = 0;
        let mut warned = false;

        loop {
            tokio::select! {
                biased;
                _ = shutdown.wait() => {
                    info!("Issuance forecast poller shutting down");
                    break;
                }
                // Fires immediately on the first pass, so the first coefficient
                // lands before any consumer gates on it.
                _ = ticker.tick() => {}
            }

            if client.is_none() {
                // Bounded: `OrderbookClient::create_channel` builds its tonic
                // Endpoint without a connect_timeout, so a TCP-accepted but
                // stalled handshake would otherwise park this task forever —
                // the exact frozen-coefficient failure this poller exists to
                // eliminate. The next tick retries.
                let connect = tokio::time::timeout(
                    Duration::from_secs(config.connection_timeout_secs.max(5)),
                    crate::client::OrderbookClient::new(&config),
                )
                .await;
                match connect {
                    Ok(Ok(c)) => client = Some(c),
                    Ok(Err(e)) => {
                        failures += 1;
                        note_poll_failure(failures, &mut warned, &format!("client create failed: {:#}", e));
                        continue;
                    }
                    Err(_) => {
                        failures += 1;
                        note_poll_failure(failures, &mut warned, "client create timed out");
                        continue;
                    }
                }
            }

            let outcome = {
                let c = client.as_mut().expect("client is Some — built directly above");
                tokio::time::timeout(Duration::from_secs(5), c.get_rounds_data(Some(1))).await
            };

            // Three outcomes, not two: a healthy response WITHOUT a prediction
            // must be neither a success (it would clear `warned` mid-outage
            // and hide the freeze) nor a failure (the transport is fine, so
            // reconnecting is pointless and the breaker would lie).
            // Ok(true) = prediction stored, Ok(false) = healthy/no prediction,
            // Err = transport failure.
            let poll: Result<bool, String> = match outcome {
                Ok(Ok(resp)) => match resp.prediction {
                    Some(prediction) => {
                        update_forecast(prediction.forecast, prediction.forecast_coefficient);
                        Ok(true)
                    }
                    None => Ok(false),
                },
                Ok(Err(e)) => Err(format!("{:#}", e)),
                Err(_) => Err("timed out after 5s".to_string()),
            };

            match poll {
                Ok(true) => {
                    if warned {
                        info!("Issuance forecast poller recovered — prediction received");
                        warned = false;
                    }
                    failures = 0;
                }
                Ok(false) => {
                    // Transport healthy: reset the reconnect counter, but the
                    // coefficient is silently going stale — and a frozen HIGH
                    // value passes every gate without ever reaching the GC's
                    // stale canary, so once the data is genuinely old this has
                    // to surface at warn level (once per outage, like
                    // note_poll_failure; a real prediction re-arms it).
                    failures = 0;
                    let stale = forecast_age_secs().is_none_or(|age| age > *POLL_SECS * 10);
                    if stale && !warned {
                        let age = forecast_age_secs()
                            .map(|a| format!("{}s old", a))
                            .unwrap_or_else(|| "never received".to_string());
                        warn!(
                            "Issuance forecast poll returned no prediction — coefficient is {}",
                            age
                        );
                        warned = true;
                    } else {
                        debug!("Issuance forecast poll returned no prediction");
                    }
                }
                Err(msg) => {
                    failures += 1;
                    note_poll_failure(failures, &mut warned, &msg);
                    if failures % POLL_FAILURES_BEFORE_RECONNECT == 0 {
                        debug!("Issuance forecast poller: rebuilding client after {} failures", failures);
                        client = None;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not two: `LAST_UPDATE_EPOCH_SECS` is process-global, so a
    /// separate "is None before the first update" test would race the one that
    /// performs an update. Nothing else in this crate calls `update_forecast`.
    #[test]
    fn age_is_none_until_the_first_update_then_tracks_it() {
        assert_eq!(
            forecast_age_secs(),
            None,
            "no forecast received yet must be None, not a huge age — the two mean \
             different things to the gates that read this",
        );

        update_forecast(2, Some("0.75".to_string()));

        let age = forecast_age_secs().expect("age is Some once a forecast has landed");
        assert!(age <= 2, "age should be ~0 right after an update, got {}s", age);
        assert!((coefficient_value() - 0.75).abs() < 1e-9);
    }
}

/// Log a poll failure, warning only on the first crossing of the threshold so a
/// sustained outage does not spam one line per poll.
fn note_poll_failure(failures: u32, warned: &mut bool, msg: &str) {
    if failures >= POLL_FAILURES_BEFORE_WARN && !*warned {
        let age = forecast_age_secs()
            .map(|a| format!("{}s old", a))
            .unwrap_or_else(|| "never received".to_string());
        warn!(
            "Issuance forecast poll failing ({} consecutive) — coefficient is {}: {}",
            failures, age, msg
        );
        *warned = true;
    } else {
        debug!("Issuance forecast poll failed ({} consecutive): {}", failures, msg);
    }
}
