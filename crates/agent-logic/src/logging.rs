//! Shared logging initialization for orderbook agents
//!
//! Handles LOG_DESTINATION=console|file, LOG_DIR, LOG_FILE_PREFIX env vars.

use std::path::Path;

use anyhow::{Context, Result};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize the tracing subscriber.
///
/// - `verbose`: if true, sets debug level for the given `crate_names`
/// - `crate_names`: crate names to enable at debug level when verbose
/// - `default_log_prefix`: LOG_FILE_PREFIX fallback when LOG_DESTINATION=file
///
/// Fails when the log directory or file cannot be opened. A subscriber that is
/// already installed is kept, with a warning.
pub fn init_logging(verbose: bool, crate_names: &[&str], default_log_prefix: &str) -> Result<()> {
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
    let installed = if log_dest.eq_ignore_ascii_case("file") {
        let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "./logs".to_string());
        let log_prefix = std::env::var("LOG_FILE_PREFIX")
            .unwrap_or_else(|_| default_log_prefix.to_string());
        let file_appender = daily_appender(Path::new(&log_dir), &log_prefix)?;
        let (non_blocking, guard) = spawn_log_writer(file_appender);
        std::mem::forget(guard);
        tracing_subscriber::registry()
            .with(fmt::layer().with_writer(non_blocking).with_ansi(true))
            .with(filter)
            .try_init()
    } else {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(filter)
            .try_init()
    };
    if let Err(e) = installed {
        tracing::warn!("logging was already initialised; keeping the existing subscriber: {e}");
    }
    Ok(())
}

/// Daily-rotated appender for `<dir>/<prefix>.<date>`, creating `dir` if needed.
fn daily_appender(dir: &Path, prefix: &str) -> Result<RollingFileAppender> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .build(dir)
        .with_context(|| format!("cannot open log directory {}", dir.display()))
}

// Startup-only exception: the writer thread is spawned once, before any work starts.
#[expect(clippy::disallowed_methods, reason = "startup-only; tracing-appender has no fallible spawn")]
fn spawn_log_writer(
    appender: RollingFileAppender,
) -> (tracing_appender::non_blocking::NonBlocking, tracing_appender::non_blocking::WorkerGuard) {
    tracing_appender::non_blocking(appender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_directory_under_a_file_is_an_error() {
        let base = std::env::temp_dir().join(format!("agent-logging-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let err = daily_appender(&file.join("logs"), "agent").unwrap_err();
        assert!(format!("{err:#}").contains("cannot open log directory"), "{err:#}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_log_directory_is_created() {
        let base = std::env::temp_dir().join(format!("agent-logging-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("nested").join("logs");
        let appender = daily_appender(&dir, "agent").unwrap();
        drop(appender);
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "the first log file is created up front");
        let _ = std::fs::remove_dir_all(&base);
    }
}
