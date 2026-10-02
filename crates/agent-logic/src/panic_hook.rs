//! Process panic hook: logs each panic through tracing and reports it, then chains
//! to the previous hook. It never shuts the agent down.

use std::backtrace::Backtrace;
use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use orderbook_proto::settlement::ErrorEvent;

use crate::{clock, num, supervise};

/// Panics after this many are logged without a backtrace.
const MAX_BACKTRACES: u64 = 64;
/// Longest panic text sent to the error reporter.
const MAX_REPORT_CHARS: usize = 2000;

static INSTALLED: OnceLock<()> = OnceLock::new();
static PANICS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Install the hook once per process; later calls do nothing.
pub fn install() {
    if std::thread::panicking() || INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let nested = IN_HOOK.try_with(|f| f.replace(true)).unwrap_or(true);
        if !nested {
            log_and_report(info);
            let _ = IN_HOOK.try_with(|f| f.set(false));
        }
        previous(info);
    }));
}

fn log_and_report(info: &PanicHookInfo<'_>) {
    let payload = supervise::panic_message(info.payload());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown location".to_string());
    let count = PANICS.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    if count <= MAX_BACKTRACES {
        let backtrace = Backtrace::force_capture();
        tracing::error!(panic.location = %location, "panic at {location}: {payload}\n{backtrace}");
    } else {
        tracing::error!(panic.location = %location, "panic at {location}: {payload} (backtrace omitted after {MAX_BACKTRACES} panics)");
    }
    let message = format!("panic at {location}: {payload}");
    let now = clock::unix_now().unwrap_or_default();
    crate::error_reporter::report(ErrorEvent {
        source: "agent".to_string(),
        severity: "critical".to_string(),
        error_type: "INTERNAL".to_string(),
        error_code: Some("agent_panic".to_string()),
        error_message: num::short(&message, MAX_REPORT_CHARS).to_string(),
        module: Some("panic_hook".to_string()),
        occurred_at: Some(prost_types::Timestamp {
            seconds: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
            nanos: i32::try_from(now.subsec_nanos()).unwrap_or(0),
        }),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_logs::LogBuf;
    use std::sync::atomic::AtomicBool;

    static PREVIOUS_RAN: AtomicBool = AtomicBool::new(false);

    // The only test that installs the hook: the hook is process-global
    #[test]
    fn hook_logs_the_panic_and_chains_the_previous_hook() {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PREVIOUS_RAN.store(true, Ordering::SeqCst);
            default(info);
        }));
        install();
        install();
        let logs = LogBuf::default();
        let _guard = logs.capture(tracing::Level::ERROR);
        let caught = std::panic::catch_unwind(|| panic!("np-hook-probe {}", 7));
        assert!(caught.is_err());
        assert_eq!(logs.count("np-hook-probe 7"), 1);
        assert_eq!(logs.count("panic at "), 1);
        assert!(logs.count("panic_hook.rs:") >= 1, "the location is logged");
        assert!(PREVIOUS_RAN.load(Ordering::SeqCst), "the previous hook still runs");
    }
}
