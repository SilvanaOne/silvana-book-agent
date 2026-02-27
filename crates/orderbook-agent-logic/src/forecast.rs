//! Global issuance forecast state.
//!
//! Polled from the orderbook RPC (`GetRoundsData`) at each heartbeat.
//! Traffic fee processing is paused when the forecast is LOW — a low
//! issuance coefficient means the featured app is generating heavy
//! sequencer load, so traffic fee transactions would likely hit
//! SEQUENCER_BACKPRESSURE errors.  Pausing proactively avoids wasting
//! retries and backoff cycles.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use tracing::{info, warn};

/// Current IssuanceForecast enum value (0=Unspecified, 1=LOW, 2=MEDIUM, 3=HIGH).
static FORECAST: AtomicI32 = AtomicI32::new(0);

/// The predicted coefficient string used for the forecast (for logging).
static FORECAST_COEFFICIENT: Mutex<Option<String>> = Mutex::new(None);

/// Whether the previous forecast was LOW (for detecting transitions).
static WAS_LOW: AtomicBool = AtomicBool::new(false);

/// Update the global forecast state.  Logs warn on transition to LOW
/// (traffic paused) and info on transition away from LOW (traffic resumed).
pub fn update_forecast(forecast_value: i32, coefficient: Option<String>) {
    let prev_low = WAS_LOW.load(Ordering::Relaxed);
    let now_low = forecast_value == 1; // ISSUANCE_FORECAST_LOW

    FORECAST.store(forecast_value, Ordering::Relaxed);
    *FORECAST_COEFFICIENT.lock().unwrap() = coefficient;

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
