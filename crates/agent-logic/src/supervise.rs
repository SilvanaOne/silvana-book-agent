//! Supervised background tasks: a failed task is logged, reported, then restarted,
//! escalated to a graceful shutdown, or left stopped.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use tokio::runtime::Handle;
use tokio::task::{AbortHandle, JoinError, JoinHandle};
use tracing::{error, warn};

use crate::shutdown::Shutdown;

/// What happens when a supervised task panics or stops on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Start it again after a backoff.
    Restart,
    /// Signal a graceful shutdown; `escalated_task` then names the task.
    Escalate,
    /// Log and report only.
    LogOnly,
}

#[derive(Clone, Copy, Debug)]
struct Backoff {
    first: Duration,
    max: Duration,
    /// A run at least this long resets the backoff.
    healthy_run: Duration,
}

const DEFAULT_BACKOFF: Backoff = Backoff {
    first: Duration::from_secs(5),
    max: Duration::from_secs(60),
    healthy_run: Duration::from_secs(300),
};

static ESCALATED: OnceLock<&'static str> = OnceLock::new();

/// The first task that escalated, if any; the process should then exit non-zero.
pub fn escalated_task() -> Option<&'static str> {
    ESCALATED.get().copied()
}

/// Run `factory()` as a task on the current runtime under `policy`.
/// Aborting the returned handle also aborts the running task.
pub fn spawn_supervised<F, Fut>(
    name: &'static str,
    shutdown: Shutdown,
    policy: Policy,
    factory: F,
) -> Result<JoinHandle<()>>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    spawn_with(name, shutdown, policy, factory, DEFAULT_BACKOFF)
}

fn spawn_with<F, Fut>(
    name: &'static str,
    shutdown: Shutdown,
    policy: Policy,
    factory: F,
    backoff: Backoff,
) -> Result<JoinHandle<()>>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let rt = Handle::try_current().map_err(|_| anyhow!("no tokio runtime to run {name}"))?;
    Ok(rt.spawn(supervise(name, shutdown, policy, factory, backoff, rt.clone())))
}

/// Spawn `fut` on the current runtime; with no runtime, log and return `None`.
pub fn try_spawn<F>(what: &str, fut: F) -> Option<JoinHandle<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match Handle::try_current() {
        Ok(rt) => Some(rt.spawn(fut)),
        Err(_) => {
            warn!("no tokio runtime; {what} not started");
            None
        }
    }
}

/// How a bounded unit of work ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Bounded<T> {
    Done(T),
    /// The budget elapsed and the work was dropped.
    Elapsed,
    /// Shutdown came first and the work was dropped.
    Shutdown,
}

/// Run cancel-safe `work` for at most `budget`, or until shutdown.
pub async fn bounded<F: Future>(shutdown: &Shutdown, budget: Duration, work: F) -> Bounded<F::Output> {
    tokio::select! {
        biased;
        _ = shutdown.wait() => Bounded::Shutdown,
        r = tokio::time::timeout(budget, work) => match r {
            Ok(v) => Bounded::Done(v),
            Err(_) => Bounded::Elapsed,
        },
    }
}

/// Await `work` to its end; `on_slow` runs once if it is still going after
/// `slow_after`. Returns the output and whether it was slow.
pub async fn run_to_end<F: Future>(work: F, slow_after: Duration, on_slow: impl FnOnce()) -> (F::Output, bool) {
    tokio::pin!(work);
    tokio::select! {
        biased;
        out = &mut work => (out, false),
        () = tokio::time::sleep(slow_after) => {
            on_slow();
            (work.await, true)
        }
    }
}

/// How a watched run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Watch<T> {
    Done(T),
    /// Still running after the budget; it goes on in the background.
    Slow,
    /// An earlier run is still going, so nothing was started.
    Busy,
    /// It panicked, was cancelled, or could not start.
    Failed(String),
}

/// Runs work in its own task and waits for it up to a budget. Work is never
/// cancelled: a slow run continues and later runs report `Busy` until it ends.
#[derive(Debug)]
pub struct Watched<T> {
    running: Option<JoinHandle<T>>,
}

impl<T> Default for Watched<T> {
    fn default() -> Self {
        Self { running: None }
    }
}

impl<T: Send + 'static> Watched<T> {
    /// Whether an earlier run is still going.
    pub fn is_busy(&self) -> bool {
        self.running.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Start `work` unless an earlier run is still going, then wait up to `budget`.
    pub async fn run<F>(&mut self, what: &str, budget: Duration, work: F) -> Watch<T>
    where
        F: Future<Output = T> + Send + 'static,
    {
        if let Some(earlier) = self.running.take() {
            if !earlier.is_finished() {
                self.running = Some(earlier);
                return Watch::Busy;
            }
            if let Err(e) = earlier.await {
                warn!("{what} {} after its budget", describe_join_error(e));
            }
        }
        let Some(mut task) = try_spawn(what, work) else {
            return Watch::Failed("no tokio runtime".to_string());
        };
        match tokio::time::timeout(budget, &mut task).await {
            Ok(Ok(v)) => Watch::Done(v),
            Ok(Err(e)) => Watch::Failed(describe_join_error(e)),
            Err(_) => {
                self.running = Some(task);
                Watch::Slow
            }
        }
    }
}

/// Start `cycle` every `interval` until shutdown, each run watched for `budget`
/// and never cancelled; `report` gets how each start went.
pub async fn run_watched<F, Fut>(
    name: &str,
    budget: Duration,
    interval: Duration,
    shutdown: &Shutdown,
    report: impl Fn(Watch<()>),
    mut cycle: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut runs = Watched::default();
    loop {
        if shutdown.is_shutting_down() {
            return;
        }
        let work = cycle();
        let watch = tokio::select! {
            biased;
            _ = shutdown.wait() => return,
            w = runs.run(name, budget, work) => w,
        };
        report(watch);
        if shutdown.sleep(interval).await {
            return;
        }
    }
}

/// Aborts the supervised task when the supervisor itself is dropped or aborted.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn supervise<F, Fut>(
    name: &'static str,
    shutdown: Shutdown,
    policy: Policy,
    mut factory: F,
    backoff: Backoff,
    rt: Handle,
) where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut delay = backoff.first;
    loop {
        if shutdown.is_shutting_down() {
            return;
        }
        let started = Instant::now();
        let failure = match std::panic::catch_unwind(AssertUnwindSafe(&mut factory)) {
            Ok(fut) => {
                let task = rt.spawn(fut);
                let _guard = AbortOnDrop(task.abort_handle());
                match task.await {
                    Ok(()) if shutdown.is_shutting_down() => return,
                    Ok(()) => "exited unexpectedly".to_string(),
                    Err(e) => describe_join_error(e),
                }
            }
            Err(payload) => format!("panicked while starting: {}", panic_message(payload.as_ref())),
        };
        if shutdown.is_shutting_down() {
            warn!("background task {name} {failure} during shutdown");
            return;
        }
        error!("background task {name} {failure}");
        report(name, &failure);
        match policy {
            Policy::Restart => {
                if started.elapsed() >= backoff.healthy_run {
                    delay = backoff.first;
                }
                warn!("restarting background task {name} in {delay:?}");
                if shutdown.sleep(delay).await {
                    return;
                }
                delay = delay.saturating_mul(2).min(backoff.max);
            }
            Policy::Escalate => {
                let _ = ESCALATED.set(name);
                error!("background task {name} is required; shutting the agent down");
                shutdown.signal();
                return;
            }
            Policy::LogOnly => return,
        }
    }
}

fn describe_join_error(e: JoinError) -> String {
    if e.is_cancelled() {
        return "was cancelled".to_string();
    }
    match e.try_into_panic() {
        Ok(payload) => format!("panicked: {}", panic_message(payload.as_ref())),
        Err(e) => format!("failed: {e}"),
    }
}

/// Text of a panic payload.
pub fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

fn report(name: &str, failure: &str) {
    crate::error_reporter::ErrorEventBuilder::new("INTERNAL", format!("background task {name} {failure}"))
        .severity("critical")
        .error_code("task_failed")
        .module(name)
        .send();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_logs::LogBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const FAST: Backoff = Backoff {
        first: Duration::from_millis(10),
        max: Duration::from_millis(40),
        healthy_run: Duration::from_secs(300),
    };

    async fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn try_spawn_runs_on_the_current_runtime() {
        let handle = try_spawn("np-try-spawn", async { 7u8 }).unwrap();
        assert_eq!(handle.await.unwrap(), 7);
    }

    // Outside a runtime tokio::spawn panics; try_spawn reports it instead
    #[test]
    fn try_spawn_outside_a_runtime_returns_none() {
        let logs = LogBuf::default();
        let _g = logs.capture(tracing::Level::WARN);
        assert!(try_spawn("np-no-runtime", async {}).is_none());
        assert_eq!(logs.count("no tokio runtime; np-no-runtime not started"), 1);
    }

    // Two panics, then a run that lasts until shutdown
    #[tokio::test]
    async fn restart_policy_restarts_after_a_panic() {
        let logs = LogBuf::default();
        let _g = logs.capture(tracing::Level::WARN);
        let shutdown = Shutdown::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let s = shutdown.clone();
        let handle = spawn_with("np-restart", shutdown.clone(), Policy::Restart, move || {
            let n = r.fetch_add(1, Ordering::SeqCst);
            let s = s.clone();
            async move {
                if n < 2 {
                    panic!("np-restart boom {n}");
                }
                s.wait().await;
            }
        }, FAST)
        .unwrap();
        wait_for("the third run", || runs.load(Ordering::SeqCst) == 3).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert_eq!(logs.count("background task np-restart panicked: np-restart boom"), 2);
        assert_eq!(logs.count("restarting background task np-restart"), 2);
        assert!(!shutdown_was_escalated_by("np-restart"));
    }

    #[tokio::test]
    async fn restart_policy_restarts_a_task_that_returns_early() {
        let shutdown = Shutdown::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let handle = spawn_with("np-early", shutdown.clone(), Policy::Restart, move || {
            r.fetch_add(1, Ordering::SeqCst);
            async {}
        }, FAST)
        .unwrap();
        wait_for("three runs", || runs.load(Ordering::SeqCst) >= 3).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn escalate_policy_signals_shutdown() {
        let shutdown = Shutdown::new();
        let handle = spawn_with("np-escalate", shutdown.clone(), Policy::Escalate, || async {
            panic!("np-escalate boom");
        }, FAST)
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
        assert!(shutdown.is_shutting_down());
        assert!(shutdown_was_escalated_by("np-escalate"));
    }

    #[tokio::test]
    async fn log_only_policy_does_not_restart_or_shut_down() {
        let logs = LogBuf::default();
        let _g = logs.capture(tracing::Level::WARN);
        let shutdown = Shutdown::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let handle = spawn_with("np-logonly", shutdown.clone(), Policy::LogOnly, move || {
            r.fetch_add(1, Ordering::SeqCst);
            async { panic!("np-logonly boom") }
        }, FAST)
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(!shutdown.is_shutting_down());
        assert_eq!(logs.count("background task np-logonly panicked: np-logonly boom"), 1);
    }

    #[tokio::test]
    async fn a_panicking_factory_is_supervised_too() {
        let shutdown = Shutdown::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let r = runs.clone();
        let s = shutdown.clone();
        let handle = spawn_with("np-factory", shutdown.clone(), Policy::Restart, move || {
            let n = r.fetch_add(1, Ordering::SeqCst);
            assert!(n > 0, "np-factory first start fails");
            let s = s.clone();
            async move { s.wait().await }
        }, FAST)
        .unwrap();
        wait_for("the second start", || runs.load(Ordering::SeqCst) == 2).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn clean_exit_on_shutdown_is_not_a_failure() {
        let logs = LogBuf::default();
        let _g = logs.capture(tracing::Level::WARN);
        let shutdown = Shutdown::new();
        let s = shutdown.clone();
        let handle = spawn_with("np-clean", shutdown.clone(), Policy::Escalate, move || {
            let s = s.clone();
            async move { s.wait().await }
        }, FAST)
        .unwrap();
        tokio::task::yield_now().await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
        assert_eq!(logs.count("np-clean"), 0);
        assert!(!shutdown_was_escalated_by("np-clean"));
    }

    #[tokio::test]
    async fn aborting_the_supervisor_aborts_the_task() {
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let (d, st) = (dropped.clone(), started.clone());
        let handle = spawn_with("np-abort", Shutdown::new(), Policy::Restart, move || {
            let guard = SetOnDrop(d.clone());
            let st = st.clone();
            async move {
                let _guard = guard;
                st.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
        }, FAST)
        .unwrap();
        wait_for("the task to start", || started.load(Ordering::SeqCst)).await;
        handle.abort();
        wait_for("the task to be dropped", || dropped.load(Ordering::SeqCst)).await;
    }

    #[test]
    fn spawning_without_a_runtime_is_an_error() {
        let err = spawn_supervised("np-nort", Shutdown::new(), Policy::Restart, || async {}).unwrap_err();
        assert!(err.to_string().contains("no tokio runtime to run np-nort"), "{err}");
    }

    #[tokio::test]
    async fn bounded_ends_hung_work_and_yields_to_shutdown() {
        let shutdown = Shutdown::new();
        let budget = Duration::from_millis(50);
        assert_eq!(bounded(&shutdown, budget, async { 3u8 }).await, Bounded::Done(3));
        let hung = std::future::pending::<()>();
        let ended = tokio::time::timeout(Duration::from_secs(5), bounded(&shutdown, budget, hung)).await;
        assert_eq!(ended.expect("the budget ends the work"), Bounded::Elapsed);
        shutdown.signal();
        let hung = std::future::pending::<()>();
        assert_eq!(bounded(&shutdown, Duration::MAX, hung).await, Bounded::Shutdown);
    }

    // Slow work is reported once and still runs to its own end
    #[tokio::test]
    async fn run_to_end_reports_slow_work_without_cutting_it() {
        let warned = AtomicUsize::new(0);
        let slow = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            7u8
        };
        let out = run_to_end(slow, Duration::from_millis(50), || {
            warned.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(out, (7, true));
        assert_eq!(warned.load(Ordering::SeqCst), 1);
        let quick = run_to_end(async { 8u8 }, Duration::from_secs(300), || {
            warned.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(quick, (8, false));
        assert_eq!(warned.load(Ordering::SeqCst), 1);
    }

    // A slow run is never cancelled; the next run waits for it
    #[tokio::test]
    async fn a_slow_watched_run_keeps_going_and_blocks_the_next() {
        let finished = Arc::new(AtomicBool::new(false));
        let mut watched = Watched::default();
        let f = finished.clone();
        let slow = async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            f.store(true, Ordering::SeqCst);
            1u8
        };
        let budget = Duration::from_millis(50);
        assert_eq!(watched.run("np-watch", budget, slow).await, Watch::Slow);
        assert!(watched.is_busy());
        assert_eq!(watched.run("np-watch", budget, async { 2u8 }).await, Watch::Busy);
        wait_for("the slow run", || finished.load(Ordering::SeqCst)).await;
        wait_for("the slow run to end", || !watched.is_busy()).await;
        assert_eq!(watched.run("np-watch", budget, async { 3u8 }).await, Watch::Done(3));
    }

    // The loop the merge and split workers run: Slow, then Busy, then Done; never cancelled
    #[tokio::test]
    async fn a_watched_loop_lets_a_slow_cycle_finish() {
        let shutdown = Shutdown::new();
        let finished = Arc::new(AtomicBool::new(false));
        let budget = Duration::from_millis(100);
        let mut starts = 0u32;
        let cycle = || {
            starts += 1;
            let slow = (starts == 1).then(|| Arc::clone(&finished));
            async move {
                if let Some(done) = slow {
                    tokio::time::sleep(budget * 2).await;
                    done.store(true, Ordering::SeqCst);
                }
            }
        };
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&reports);
        let report = move |w: Watch<()>| seen.lock().unwrap().push(w);
        let stop = shutdown.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            stop.signal();
        });
        let looped = run_watched("np-loop", budget, Duration::from_millis(30), &shutdown, report, cycle);
        tokio::time::timeout(Duration::from_secs(10), looped).await.expect("the loop ends at shutdown");
        stopper.await.unwrap();
        let reports = reports.lock().unwrap();
        assert_eq!(reports.first(), Some(&Watch::Slow), "{reports:?}");
        assert!(reports.contains(&Watch::Busy), "{reports:?}");
        assert_eq!(reports.last(), Some(&Watch::Done(())), "{reports:?}");
        assert!(finished.load(Ordering::SeqCst), "the slow cycle was not cancelled");
    }

    #[tokio::test]
    async fn a_panicking_watched_run_is_reported() {
        let mut watched = Watched::default();
        let out = watched.run("np-watch-panic", Duration::from_secs(5), async { panic!("np-watch boom") }).await;
        assert_eq!(out, Watch::<()>::Failed("panicked: np-watch boom".to_string()));
        assert_eq!(watched.run("np-watch-panic", Duration::from_secs(5), async {}).await, Watch::Done(()));
    }

    #[test]
    fn panic_messages_are_extracted() {
        let s: Box<dyn Any + Send> = Box::new("static text");
        assert_eq!(panic_message(s.as_ref()), "static text");
        let owned: Box<dyn Any + Send> = Box::new(String::from("owned text"));
        assert_eq!(panic_message(owned.as_ref()), "owned text");
        let other: Box<dyn Any + Send> = Box::new(42u8);
        assert_eq!(panic_message(other.as_ref()), "non-string panic payload");
    }

    fn shutdown_was_escalated_by(name: &str) -> bool {
        escalated_task() == Some(name)
    }
}
