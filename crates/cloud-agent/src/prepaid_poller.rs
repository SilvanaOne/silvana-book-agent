//! Keeps the prepaid credit figure fresh for the settlement fee checks.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{debug, info, warn};

use agent_logic::liquidity::LiquidityManager;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Bounded, Policy};

use crate::{DAppProviderClient, PrepaidTrafficBalance};

/// Period of the prepaid balance poll.
pub const PREPAID_POLL_EVERY: Duration = Duration::from_secs(30);

/// Bound on one poll, the call and the update together.
const PREPAID_POLL_BUDGET: Duration = Duration::from_secs(20);

/// While polls keep failing, a warning at most this often.
const FAILURE_WARN_EVERY: Duration = Duration::from_secs(600);

/// Fetch the prepaid balance once and record it, stamped with when the poll was sent.
pub async fn poll_once<F, Fut>(
    fetch: F,
    lm: &LiquidityManager,
    shutdown: &Shutdown,
    budget: Duration,
) -> std::result::Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<PrepaidTrafficBalance>>,
{
    let sent_at = Instant::now();
    let work = async {
        let balance = fetch().await.map_err(|e| format!("{e:#}"))?;
        lm.update_prepaid(balance.available_cc, sent_at).await;
        lm.set_orderbook_fee_discount_pct(balance.orderbook_fee_discount_pct).await;
        Ok(())
    };
    match supervise::bounded(shutdown, budget, work).await {
        Bounded::Done(done) => done,
        Bounded::Elapsed => Err(format!("no answer within {}s", budget.as_secs())),
        Bounded::Shutdown => Err("shutting down".to_string()),
    }
}

/// Poll now and then every [`PREPAID_POLL_EVERY`] until shutdown.
async fn poll_loop<F, Fut>(mut fetch: F, lm: Arc<LiquidityManager>, shutdown: Shutdown)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<PrepaidTrafficBalance>>,
{
    let mut failing = false;
    let mut warned_at: Option<Instant> = None;
    loop {
        match poll_once(&mut fetch, &lm, &shutdown, PREPAID_POLL_BUDGET).await {
            Ok(()) => {
                if failing {
                    info!("Prepaid balance poll recovered");
                }
                failing = false;
                // The next outage warns at once
                warned_at = None;
            }
            Err(_) if shutdown.is_shutting_down() => return,
            Err(e) => {
                failing = true;
                if warned_at.is_none_or(|at| at.elapsed() >= FAILURE_WARN_EVERY) {
                    warned_at = Some(Instant::now());
                    warn!("Prepaid balance poll failed (fee checks defer once it is stale): {}", e);
                } else {
                    debug!("Prepaid balance poll failed: {}", e);
                }
            }
        }
        if shutdown.sleep(PREPAID_POLL_EVERY).await {
            return;
        }
    }
}

/// Run the poll under supervision, restarted if it ever ends early.
pub fn spawn(client: DAppProviderClient, lm: Arc<LiquidityManager>, shutdown: Shutdown) -> Result<()> {
    let s = shutdown.clone();
    supervise::spawn_supervised("prepaid poller", shutdown, Policy::Restart, move || {
        let client = client.clone();
        poll_loop(
            move || {
                let mut client = client.clone();
                async move { client.get_prepaid_traffic_balance().await }
            },
            Arc::clone(&lm),
            s.clone(),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn balance(available: &str) -> PrepaidTrafficBalance {
        let d = Decimal::from_str(available).unwrap();
        PrepaidTrafficBalance {
            balance_cc: d,
            credit_limit_cc: Decimal::ZERO,
            available_cc: d,
            total_credited_cc: d,
            total_debited_cc: Decimal::ZERO,
            orderbook_fee_discount_pct: None,
        }
    }

    #[tokio::test]
    async fn a_poll_caches_the_order_book_fee_discount() {
        let lm = lm();
        let shutdown = Shutdown::new();
        let discounted = PrepaidTrafficBalance { orderbook_fee_discount_pct: Some(Decimal::from(90)), ..balance("10") };
        poll_once(|| async { Ok(discounted) }, &lm, &shutdown, Duration::from_secs(1)).await.unwrap();
        assert_eq!(lm.orderbook_fee_discount_pct().await, Some(Decimal::from(90)));
        // A later poll without one is full price again
        poll_once(|| async { Ok(balance("10")) }, &lm, &shutdown, Duration::from_secs(1)).await.unwrap();
        assert_eq!(lm.orderbook_fee_discount_pct().await, None);
    }

    fn lm() -> Arc<LiquidityManager> {
        LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0)
    }

    #[tokio::test]
    async fn a_poll_feeds_the_prepaid_figure() {
        let lm = lm();
        let shutdown = Shutdown::new();
        assert_eq!(lm.fee_capacity().await, None);
        let polled = poll_once(|| async { Ok(balance("94.27")) }, &lm, &shutdown, Duration::from_secs(1)).await;
        assert!(polled.is_ok());
        assert_eq!(lm.fee_capacity().await, Some(Decimal::from_str("94.27").unwrap()));
    }

    #[tokio::test]
    async fn a_failed_or_slow_poll_leaves_the_figure_unknown() {
        let lm = lm();
        let shutdown = Shutdown::new();
        let failed = poll_once(|| async { Err(anyhow::anyhow!("refused")) }, &lm, &shutdown, Duration::from_secs(1)).await;
        assert!(failed.is_err());
        let slow = poll_once(
            || async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(balance("94"))
            },
            &lm,
            &shutdown,
            Duration::from_millis(20),
        )
        .await;
        assert!(slow.is_err());
        assert_eq!(lm.fee_capacity().await, None);
    }

    // A fee paid while the poll is in flight is not in its answer, so it stays debited
    #[tokio::test]
    async fn a_debit_during_the_poll_is_kept() {
        let lm = lm();
        lm.update_cc_usd_rate(Decimal::from_str("0.1").unwrap()).await;
        let shutdown = Shutdown::new();
        let during = Arc::clone(&lm);
        let polled = poll_once(
            move || async move {
                during.record_fee_paid("p1", None, Some(Decimal::ONE)).await;
                Ok(balance("100"))
            },
            &lm,
            &shutdown,
            Duration::from_secs(1),
        )
        .await;
        assert!(polled.is_ok());
        let capacity = lm.fee_capacity().await.unwrap();
        assert!(capacity < Decimal::from(90) && capacity > Decimal::from(88), "{capacity}");
    }

    // A second outage soon after a recovery is warned about too
    #[tokio::test(start_paused = true)]
    async fn each_outage_after_a_recovery_warns() {
        let logs = crate::test_util::LogBuf::default();
        let _guard = logs.capture(tracing::Level::WARN);
        let lm = lm();
        let shutdown = Shutdown::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (counter, stop) = (Arc::clone(&calls), shutdown.clone());
        let fetch = move || {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let stop = stop.clone();
            async move {
                match n {
                    1 => Ok(balance("94")),
                    0 | 2 => Err(anyhow::anyhow!("refused")),
                    _ => {
                        stop.signal();
                        Err(anyhow::anyhow!("stopping"))
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(600), poll_loop(fetch, Arc::clone(&lm), shutdown)).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(logs.count("Prepaid balance poll failed"), 2);
    }

    #[tokio::test]
    async fn the_poller_stops_on_shutdown() {
        let lm = lm();
        let shutdown = Shutdown::new();
        let run = poll_loop(|| async { Ok(balance("94")) }, Arc::clone(&lm), shutdown.clone());
        let stop = async {
            while lm.fee_capacity().await.is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            shutdown.signal();
        };
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, stop) }).await.unwrap();
    }
}
