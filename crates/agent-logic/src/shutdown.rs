//! Shutdown signal shared across all background tasks.
//!
//! Bundles an `Arc<AtomicBool>` (cheap polling) with an `Arc<Notify>` (instant
//! wake-up). Background loops should:
//!   1. Re-check `is_shutting_down()` at the top of each iteration.
//!   2. Replace bare `tokio::sleep(d)` with `shutdown.sleep(d)` so the wait
//!      terminates the moment Ctrl-C arrives.
//!   3. Add a `_ = shutdown.wait() => break` arm to nested `tokio::select!`s.
//!
//! Note: we DO NOT cancel in-flight work (Canton tx, gRPC requests) until a
//! [`StopWindow`] ends. The select arm only fires when the loop is *idle*; once a
//! non-shutdown branch has been chosen, its body runs to completion.

use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

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

/// How long a stop may take once it begins; cutting it short ends it at once.
#[derive(Clone)]
pub struct StopWindow {
    limit: Option<Duration>,
    began: Arc<OnceLock<Instant>>,
    cut: Shutdown,
}

impl StopWindow {
    /// A window of `limit` from its first [`StopWindow::begin`]; `None` ends only when cut short.
    pub fn new(limit: Option<Duration>) -> Self {
        Self { limit, began: Arc::new(OnceLock::new()), cut: Shutdown::new() }
    }

    /// Start the window now; later calls keep the first start.
    pub fn begin(&self) {
        self.began.get_or_init(Instant::now);
    }

    /// End the window at once.
    pub fn cut_short(&self) {
        self.cut.signal();
    }

    /// Whether the window was cut short.
    pub fn is_cut(&self) -> bool {
        self.cut.is_shutting_down()
    }

    /// Time left: `None` without a limit, zero once cut short or past.
    pub fn remaining(&self) -> Option<Duration> {
        if self.is_cut() {
            return Some(Duration::ZERO);
        }
        let limit = self.limit?;
        Some(match self.began.get() {
            Some(began) => limit.saturating_sub(Instant::now().saturating_duration_since(*began)),
            None => limit,
        })
    }

    /// Resolves once `stop` fired and the limit has passed since the window began
    /// (the stop begins it if nothing did); never without a limit.
    pub async fn passed(&self, stop: &Shutdown) {
        let Some(limit) = self.limit else {
            return std::future::pending().await;
        };
        stop.wait().await;
        let began = *self.began.get_or_init(Instant::now);
        match began.checked_add(limit) {
            Some(end) => tokio::time::sleep_until(end).await,
            None => std::future::pending().await,
        }
    }

    /// Resolves once the window is cut short.
    pub async fn cut(&self) {
        self.cut.wait().await;
    }

    /// Resolves when the window passed after `stop` or was cut short.
    pub async fn ended(&self, stop: &Shutdown) {
        tokio::select! {
            () = self.passed(stop) => {}
            () = self.cut() => {}
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

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[tokio::test]
    async fn a_stop_window_ends_its_limit_after_the_first_begin() {
        let (stop, window) = (Shutdown::new(), StopWindow::new(Some(ms(100))));
        assert_eq!(window.remaining(), Some(ms(100)), "not begun yet");
        assert!(tokio::time::timeout(ms(200), window.ended(&stop)).await.is_err(), "nothing ends before the stop");
        stop.signal();
        let started = std::time::Instant::now();
        window.begin();
        tokio::time::sleep(ms(30)).await;
        window.begin();
        assert!(window.remaining().is_some_and(|left| left <= ms(75)), "{:?}", window.remaining());
        tokio::time::timeout(Duration::from_secs(2), window.ended(&stop))
            .await
            .expect("the window must end");
        let took = started.elapsed();
        assert!(took >= ms(95) && took < Duration::from_secs(1), "{took:?}");
        assert_eq!(window.remaining(), Some(Duration::ZERO));
        assert!(!window.is_cut());
    }

    #[tokio::test]
    async fn a_window_without_a_limit_ends_only_when_cut_short() {
        let (stop, window) = (Shutdown::new(), StopWindow::new(None));
        stop.signal();
        window.begin();
        assert_eq!(window.remaining(), None);
        assert!(tokio::time::timeout(ms(150), window.ended(&stop)).await.is_err());
        let w = window.clone();
        let _cutter = crate::supervise::try_spawn("cutter", async move {
            tokio::time::sleep(ms(50)).await;
            w.cut_short();
        });
        tokio::time::timeout(Duration::from_secs(2), window.ended(&stop))
            .await
            .expect("a cut ends the window");
        tokio::time::timeout(ms(50), window.cut()).await.expect("cut() resolves once cut");
        assert!(window.is_cut());
        assert_eq!(window.remaining(), Some(Duration::ZERO));
        assert!(tokio::time::timeout(ms(50), window.passed(&stop)).await.is_err(), "no limit never passes");
    }

    #[tokio::test]
    async fn a_cut_ends_a_long_window_at_once() {
        let (stop, window) = (Shutdown::new(), StopWindow::new(Some(Duration::from_secs(3600))));
        stop.signal();
        window.begin();
        assert!(window.remaining().is_some_and(|left| left > Duration::from_secs(3500)));
        assert!(tokio::time::timeout(ms(100), window.cut()).await.is_err());
        window.cut_short();
        window.cut_short();
        tokio::time::timeout(ms(100), window.ended(&stop)).await.expect("a cut window ends at once");
        assert_eq!(window.remaining(), Some(Duration::ZERO));
    }

    // A stop that did not begin the window starts it when the stop fires
    #[tokio::test]
    async fn passed_counts_from_the_stop_when_nothing_began_the_window() {
        let (stop, window) = (Shutdown::new(), StopWindow::new(Some(ms(100))));
        let (w, s) = (window.clone(), stop.clone());
        let passed = crate::supervise::try_spawn("passed", async move { w.passed(&s).await }).unwrap();
        tokio::time::sleep(ms(150)).await;
        assert!(!passed.is_finished(), "nothing passes before the stop");
        let started = std::time::Instant::now();
        stop.signal();
        tokio::time::timeout(Duration::from_secs(2), passed)
            .await
            .expect("the window must pass")
            .unwrap();
        let took = started.elapsed();
        assert!(took >= ms(95) && took < Duration::from_secs(1), "{took:?}");
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}
