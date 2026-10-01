//! Order-grid task: refreshes the grid on its own cadence, apart from the
//! settlement loop, and hands the grid back when it stops.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{info, warn};

use orderbook_proto::ledger::TokenBalance;

use crate::config::BaseConfig;
use crate::liquidity::LiquidityManager;
use crate::order_manager::{deadline_after, CycleReport, OrderManager};
use crate::runner::{push_balances_to_lm, BalanceProvider};
use crate::shutdown::Shutdown;

/// Time between grid cycles when nothing wakes the task early.
pub const GRID_PERIOD: Duration = Duration::from_secs(5);

/// Shortest pause between the end of one cycle and a woken start of the next.
pub const GRID_MIN_GAP: Duration = Duration::from_secs(2);

/// Soft grid-cycle deadline, checked between markets.
pub const GRID_CYCLE_SOFT_DEADLINE: Duration = Duration::from_secs(60);

/// Upper bound for the balance fetch that starts each cycle.
pub const GRID_BALANCE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an aborted grid task is awaited before it is left behind.
const ABORT_JOIN_WAIT: Duration = Duration::from_secs(5);

/// Rate limit for the grid task's balance-fetch warnings.
const BALANCE_WARN_EVERY: Duration = Duration::from_secs(60);

/// Grid state driven by [`spawn_grid_task`].
#[async_trait]
pub trait GridCycle: Send + 'static {
    /// Take a fresh, registry-filtered balance snapshot.
    fn apply_balances(&mut self, balances: Vec<TokenBalance>);

    /// One pass over the markets; stops between markets on shutdown or past `deadline`.
    async fn run_cycle(&mut self, stop: &Shutdown, deadline: Instant) -> Result<CycleReport>;
}

#[async_trait]
impl GridCycle for OrderManager {
    fn apply_balances(&mut self, balances: Vec<TokenBalance>) {
        self.set_balances(balances);
    }

    async fn run_cycle(&mut self, stop: &Shutdown, deadline: Instant) -> Result<CycleReport> {
        self.update_cycle(stop, deadline).await
    }
}

/// Grid task cadence; `Default` is the production timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridTiming {
    pub period: Duration,
    pub min_gap: Duration,
    pub soft_deadline: Duration,
    pub balance_timeout: Duration,
}

impl Default for GridTiming {
    fn default() -> Self {
        Self {
            period: GRID_PERIOD,
            min_gap: GRID_MIN_GAP,
            soft_deadline: GRID_CYCLE_SOFT_DEADLINE,
            balance_timeout: GRID_BALANCE_TIMEOUT,
        }
    }
}

/// Grid activity as seen by the heartbeat. Counters cover the interval since
/// the last [`GridStats::take_interval`]; the rest describes the latest cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSnapshot {
    pub cycles: u64,
    pub max_ms: u64,
    pub refreshed: u64,
    pub placed: u64,
    pub cancelled: u64,
    pub failed: u64,
    /// Most markets deferred by any one cycle in the interval.
    pub deferred: u32,
    pub last_ms: u64,
    pub markets: u32,
    pub parked: u32,
    pub held: u32,
    pub last_end: Option<Instant>,
    pub busy_since: Option<Instant>,
    pub started_at: Instant,
}

impl GridSnapshot {
    fn new(now: Instant) -> Self {
        Self {
            cycles: 0,
            max_ms: 0,
            refreshed: 0,
            placed: 0,
            cancelled: 0,
            failed: 0,
            deferred: 0,
            last_ms: 0,
            markets: 0,
            parked: 0,
            held: 0,
            last_end: None,
            busy_since: None,
            started_at: now,
        }
    }

    /// Time since the last cycle ended (or since the stats were created).
    pub fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_end.unwrap_or(self.started_at))
    }
}

/// Shared, poison-tolerant grid statistics.
#[derive(Debug, Clone)]
pub struct GridStats {
    inner: Arc<std::sync::Mutex<GridSnapshot>>,
}

impl Default for GridStats {
    fn default() -> Self {
        Self::new()
    }
}

impl GridStats {
    pub fn new() -> Self {
        Self { inner: Arc::new(std::sync::Mutex::new(GridSnapshot::new(Instant::now()))) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut GridSnapshot) -> R) -> R {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut inner)
    }

    fn begin(&self, now: Instant) {
        self.with(|s| s.busy_since = Some(now));
    }

    fn finish(&self, elapsed: Duration, report: Option<&CycleReport>, now: Instant) {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.with(|s| {
            s.cycles = s.cycles.saturating_add(1);
            s.max_ms = s.max_ms.max(ms);
            s.last_ms = ms;
            s.last_end = Some(now);
            s.busy_since = None;
            match report {
                Some(r) => {
                    s.markets = r.markets;
                    s.deferred = s.deferred.max(r.deferred);
                    s.parked = r.parked;
                    s.held = r.held_unknown;
                    s.refreshed = s.refreshed.saturating_add(u64::from(r.refreshed));
                    s.placed = s.placed.saturating_add(u64::from(r.placed));
                    s.cancelled = s.cancelled.saturating_add(u64::from(r.cancelled));
                }
                None => s.failed = s.failed.saturating_add(1),
            }
        });
    }

    /// Current view without resetting anything.
    pub fn snapshot(&self) -> GridSnapshot {
        self.with(|s| *s)
    }

    /// Current view; the interval counters restart from zero.
    pub fn take_interval(&self) -> GridSnapshot {
        self.with(|s| {
            let out = *s;
            s.cycles = 0;
            s.max_ms = 0;
            s.refreshed = 0;
            s.placed = 0;
            s.cancelled = 0;
            s.failed = 0;
            s.deferred = 0;
            out
        })
    }
}

/// Run the grid every `timing.period`, or on `wake` no sooner than `min_gap` after
/// the last cycle. Stops between markets on shutdown and returns the grid.
#[allow(clippy::too_many_arguments)]
pub fn spawn_grid_task<G: GridCycle>(
    mut grid: G,
    config: BaseConfig,
    provider: Arc<dyn BalanceProvider>,
    lm: Option<Arc<LiquidityManager>>,
    shutdown: Shutdown,
    wake: Arc<Notify>,
    stats: GridStats,
    timing: GridTiming,
) -> JoinHandle<G> {
    tokio::spawn(async move {
        info!("Grid task started: interval={}s", timing.period.as_secs());
        // A zero period would make interval() panic
        let mut ticker = interval(timing.period.max(Duration::from_millis(1)));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_end: Option<Instant> = None;
        let mut balance_warned_at: Option<Instant> = None;
        loop {
            if shutdown.is_shutting_down() {
                break;
            }
            tokio::select! {
                biased;
                _ = shutdown.wait() => break,
                _ = ticker.tick() => {}
                _ = wake.notified() => {
                    let since = last_end.map(|t| t.elapsed()).unwrap_or(timing.min_gap);
                    if since < timing.min_gap && shutdown.sleep(timing.min_gap.saturating_sub(since)).await {
                        break;
                    }
                }
            }

            // This cycle covers any wake raised before it starts
            let _ = futures::FutureExt::now_or_never(wake.notified());
            let started = Instant::now();
            stats.begin(started);

            let failure = match tokio::time::timeout(timing.balance_timeout, provider.fetch_balances()).await {
                Ok(Ok(balances)) => {
                    grid.apply_balances(push_balances_to_lm(&config, lm.as_ref(), balances).await);
                    None
                }
                Ok(Err(e)) => Some(format!("Failed to fetch balances: {e:#}")),
                Err(_) => Some("Balance fetch timed out".to_string()),
            };
            if let Some(failure) = failure {
                if balance_warned_at.is_none_or(|t| t.elapsed() >= BALANCE_WARN_EVERY) {
                    balance_warned_at = Some(Instant::now());
                    warn!("{}", failure);
                }
            }

            let outcome = grid.run_cycle(&shutdown, deadline_after(timing.soft_deadline)).await;
            let now = Instant::now();
            match outcome {
                Ok(report) => stats.finish(now.saturating_duration_since(started), Some(&report), now),
                Err(e) => {
                    warn!("Order update cycle failed: {:#}", e);
                    stats.finish(now.saturating_duration_since(started), None, now);
                }
            }
            last_end = Some(now);
            ticker.reset();
        }
        info!("Grid task stopped");
        grid
    })
}

/// Wait up to `limit` for the grid task to return the grid. On timeout or a
/// failed join the task is aborted and `None` is returned.
pub async fn join_grid<G>(mut handle: JoinHandle<G>, limit: Duration) -> Option<G> {
    match tokio::time::timeout(limit, &mut handle).await {
        Ok(Ok(grid)) => Some(grid),
        Ok(Err(e)) => {
            warn!("Grid task failed: {}", e);
            None
        }
        Err(_) => {
            warn!("Grid task did not stop within {}s; aborting it", limit.as_secs());
            handle.abort();
            if tokio::time::timeout(ABORT_JOIN_WAIT, handle).await.is_err() {
                warn!("Aborted grid task still running after {}s", ABORT_JOIN_WAIT.as_secs());
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CycleReport {
        CycleReport { markets: 5, parked: 2, held_unknown: 1, refreshed: 3, placed: 6, cancelled: 4, deferred: 2, ..Default::default() }
    }

    #[test]
    fn grid_stats_survive_a_poisoned_lock() {
        let stats = GridStats::new();
        let poisoner = stats.clone();
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.inner.lock().unwrap();
            panic!("poison the stats lock");
        })
        .join();
        assert!(joined.is_err());
        assert!(stats.inner.is_poisoned());

        stats.finish(Duration::from_millis(7), Some(&report()), Instant::now());
        let snap = stats.snapshot();
        assert_eq!((snap.cycles, snap.last_ms, snap.parked, snap.held), (1, 7, 2, 1));
        assert_eq!((snap.markets, snap.deferred), (5, 2));
        assert_eq!(stats.take_interval().cycles, 1);
    }

    #[test]
    fn take_interval_restarts_only_the_interval_counters() {
        let stats = GridStats::new();
        let now = Instant::now();
        stats.begin(now);
        assert_eq!(stats.snapshot().busy_since, Some(now));
        stats.finish(Duration::from_millis(9), Some(&report()), now);
        stats.finish(Duration::from_millis(5), None, now);

        let first = stats.take_interval();
        assert_eq!((first.cycles, first.max_ms, first.last_ms, first.failed), (2, 9, 5, 1));
        assert_eq!((first.refreshed, first.placed, first.cancelled), (3, 6, 4));
        assert_eq!((first.markets, first.deferred), (5, 2), "a failed cycle leaves the market count");
        assert_eq!(first.busy_since, None);
        assert_eq!(first.idle_for(now), Duration::ZERO);

        let second = stats.take_interval();
        assert_eq!((second.cycles, second.max_ms, second.refreshed, second.failed), (0, 0, 0, 0));
        assert_eq!((second.last_ms, second.parked, second.held, second.last_end), (5, 2, 1, Some(now)));
        assert_eq!((second.markets, second.deferred), (5, 0));
    }

    #[test]
    fn deferred_is_the_interval_maximum_and_markets_the_last_cycle() {
        let stats = GridStats::new();
        let now = Instant::now();
        for (markets, deferred) in [(3, 4), (7, 0), (6, 1)] {
            let r = CycleReport { markets, deferred, ..Default::default() };
            stats.finish(Duration::from_millis(1), Some(&r), now);
        }
        let snap = stats.take_interval();
        assert_eq!((snap.markets, snap.deferred), (6, 4));
        assert_eq!(stats.snapshot().deferred, 0);
    }

    #[tokio::test]
    async fn join_grid_aborts_a_task_that_does_not_stop() {
        let stuck = tokio::spawn(async {
            std::future::pending::<()>().await;
            1u8
        });
        let started = Instant::now();
        assert_eq!(join_grid(stuck, Duration::from_millis(50)).await, None);
        // An abort that did not take would wait out the full ABORT_JOIN_WAIT
        assert!(started.elapsed() < ABORT_JOIN_WAIT.saturating_sub(Duration::from_secs(1)));

        let done = tokio::spawn(async { 7u8 });
        assert_eq!(join_grid(done, Duration::from_secs(10)).await, Some(7));
    }

    #[test]
    fn default_timing_is_the_production_cadence() {
        let t = GridTiming::default();
        assert_eq!(t.period, Duration::from_secs(5));
        assert_eq!(t.min_gap, Duration::from_secs(2));
        assert_eq!(t.soft_deadline, Duration::from_secs(60));
        assert_eq!(t.balance_timeout, Duration::from_secs(10));
    }
}
