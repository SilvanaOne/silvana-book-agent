//! Shutdown signal shared across all background tasks.
//!
//! Bundles an `Arc<AtomicBool>` (cheap polling) with an `Arc<Notify>` (instant
//! wake-up). Background loops should:
//!   1. Re-check `is_shutting_down()` at the top of each iteration.
//!   2. Replace bare `tokio::sleep(d)` with `shutdown.sleep(d)` so the wait
//!      terminates the moment Ctrl-C arrives.
//!   3. Add a `_ = shutdown.wait() => break` arm to nested `tokio::select!`s.
//!
//! Note: we DO NOT cancel in-flight work (Canton tx, gRPC requests). The select
//! arm only fires when the loop is *idle*; once a non-shutdown branch has been
//! chosen, its body runs to completion.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct Shutdown {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Construct from an existing flag (back-compat — pairs a fresh Notify
    /// with a flag that callers already share).
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flag, notify: Arc::new(Notify::new()) }
    }

    /// Trigger shutdown: set the flag and wake every waiter.
    pub fn signal(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Acquire-load so polling readers (those that did not synchronize
    /// through `notify_waiters()`'s internal lock) see the flag promptly on
    /// weakly-ordered platforms (ARM/AArch64). Free on x86.
    pub fn is_shutting_down(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Raw flag handle for code paths that already store an `Arc<AtomicBool>`.
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }

    /// Future that resolves the moment shutdown is signalled (or immediately
    /// if it already has been). Use as a `select!` arm.
    ///
    /// Uses Tokio's documented [`Notified::enable`] pattern: register as a
    /// waiter *before* checking the flag. This is essential because
    /// `notify_waiters()` does not store a permit — a notification fired
    /// between a naive flag-check and `.notified().await` would be lost.
    pub async fn wait(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    /// Sleep up to `duration`, returning early if shutdown fires.
    /// Returns `true` if shutdown was observed, `false` if the full duration elapsed.
    pub async fn sleep(&self, duration: Duration) -> bool {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.flag.load(Ordering::Acquire) {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(duration) => self.flag.load(Ordering::Acquire),
            _ = notified.as_mut() => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signal_after_creation_is_not_lost_in_wait() {
        let s = Shutdown::new();
        let s2 = s.clone();
        let waiter = tokio::spawn(async move {
            s2.wait().await;
        });
        // Yield so the waiter has a chance to start, then signal.
        tokio::task::yield_now().await;
        s.signal();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("wait() must wake within 1s of signal()")
            .unwrap();
    }

    #[tokio::test]
    async fn signal_after_creation_is_not_lost_in_sleep() {
        let s = Shutdown::new();
        let s2 = s.clone();
        let waiter = tokio::spawn(async move {
            s2.sleep(Duration::from_secs(60)).await
        });
        tokio::task::yield_now().await;
        s.signal();
        let saw_shutdown = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("sleep() must wake within 1s of signal()")
            .unwrap();
        assert!(saw_shutdown, "sleep() must report shutdown");
    }

    #[tokio::test]
    async fn already_signalled_returns_immediately() {
        let s = Shutdown::new();
        s.signal();
        // Both methods must return without ever blocking.
        tokio::time::timeout(Duration::from_millis(50), s.wait())
            .await
            .expect("pre-signalled wait() must return immediately");
        let saw = tokio::time::timeout(Duration::from_millis(50), s.sleep(Duration::from_secs(60)))
            .await
            .expect("pre-signalled sleep() must return immediately");
        assert!(saw);
    }

    #[tokio::test]
    async fn sleep_returns_false_when_duration_elapses() {
        let s = Shutdown::new();
        let saw = s.sleep(Duration::from_millis(20)).await;
        assert!(!saw, "sleep() that times out must report no-shutdown");
    }

    #[tokio::test]
    async fn signal_wakes_multiple_waiters() {
        let s = Shutdown::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let s2 = s.clone();
            handles.push(tokio::spawn(async move {
                s2.wait().await;
            }));
        }
        tokio::task::yield_now().await;
        s.signal();
        for h in handles {
            tokio::time::timeout(Duration::from_secs(1), h)
                .await
                .expect("every waiter must wake within 1s")
                .unwrap();
        }
    }

    /// True race regression: multi-threaded runtime + `signal()` fired
    /// without yielding to the waiter, repeated many times. The pre-fix
    /// `wait()` (no `Notified::enable()`) hangs deterministically here
    /// because `notify_waiters()` finds no registered waiters.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn race_signal_during_wait_registration() {
        for _ in 0..1000 {
            let s = Shutdown::new();
            let s2 = s.clone();
            let h = tokio::spawn(async move { s2.wait().await });
            // No yield_now — racing the signal against the spawn so the
            // waiter may be mid-registration when notify_waiters() fires.
            s.signal();
            tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .expect("wait() must not hang under race")
                .unwrap();
        }
    }

    /// Same race shape, but for `sleep()`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn race_signal_during_sleep_registration() {
        for _ in 0..1000 {
            let s = Shutdown::new();
            let s2 = s.clone();
            let h = tokio::spawn(async move {
                s2.sleep(Duration::from_secs(60)).await
            });
            s.signal();
            let observed = tokio::time::timeout(Duration::from_millis(500), h)
                .await
                .expect("sleep() must not hang under race")
                .unwrap();
            assert!(observed, "sleep() must report shutdown when raced");
        }
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}
