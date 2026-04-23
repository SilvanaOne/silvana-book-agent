//! Shared logging initialization for orderbook agents
//!
//! Handles LOG_DESTINATION=console|file, LOG_DIR, LOG_FILE_PREFIX env vars.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize the tracing subscriber.
///
/// - `verbose`: if true, sets debug level for the given `crate_names`
/// - `crate_names`: crate names to enable at debug level when verbose
/// - `default_log_prefix`: LOG_FILE_PREFIX fallback when LOG_DESTINATION=file
pub fn init_logging(verbose: bool, crate_names: &[&str], default_log_prefix: &str) {
    let filter = if verbose {
        let debug_directives: Vec<String> = crate_names
            .iter()
            .map(|name| format!("{}=debug", name))
            .collect();
        EnvFilter::new(format!("{},info", debug_directives.join(",")))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            let info_directives: Vec<String> = crate_names
                .iter()
                .map(|name| format!("{}=info", name))
                .collect();
            EnvFilter::new(format!("{},warn", info_directives.join(",")))
        })
    };

    let log_dest = std::env::var("LOG_DESTINATION").unwrap_or_else(|_| "console".to_string());
    if log_dest.eq_ignore_ascii_case("file") {
        let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "./logs".to_string());
        let log_prefix = std::env::var("LOG_FILE_PREFIX")
            .unwrap_or_else(|_| default_log_prefix.to_string());
        let file_appender = tracing_appender::rolling::daily(&log_dir, &log_prefix);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        std::mem::forget(guard);
        tracing_subscriber::registry()
            .with(fmt::layer().with_writer(non_blocking).with_ansi(true))
            .with(filter)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(filter)
            .init();
    }
}
