//! Global issuance forecast state.
//!
//! Polled from the orderbook RPC (`GetRoundsData`) at each heartbeat.
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

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{LazyLock, Mutex};
use tracing::{info, warn};

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
