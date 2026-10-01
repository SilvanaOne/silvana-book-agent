//! Log capture for unit tests.

use std::sync::{Arc, Mutex};

/// Collects formatted log output for assertions.
#[derive(Clone, Default)]
pub(crate) struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

impl LogBuf {
    /// Capture this thread's events at `level` and above until the guard drops.
    pub(crate) fn capture(&self, level: tracing::Level) -> tracing::subscriber::DefaultGuard {
        // A second live dispatcher keeps callsite interest from being taken from one thread's default
        static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
        KEEP.get_or_init(|| {
            tracing::Dispatch::new(tracing_subscriber::fmt().with_writer(std::io::sink).finish())
        });
        let subscriber = tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_ansi(false)
            .with_max_level(level)
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    pub(crate) fn count(&self, needle: &str) -> usize {
        String::from_utf8_lossy(&self.0.lock().unwrap()).matches(needle).count()
    }
}

mod tests {
    use super::LogBuf;

    fn probe() {
        tracing::info!("log capture probe");
    }

    // A callsite first reached on a thread without a subscriber still reaches the capture
    #[test]
    fn capture_sees_callsites_first_hit_on_other_threads() {
        let logs = LogBuf::default();
        let _guard = logs.capture(tracing::Level::INFO);
        std::thread::spawn(probe).join().unwrap();
        probe();
        assert_eq!(logs.count("log capture probe"), 1);
    }
}
