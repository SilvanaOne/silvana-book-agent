//! Auto top-up of the off-chain prepaid traffic balance.
//!
//! When `MIN_PREPAID_TRAFFIC_BALANCE_CC` and `PREPAID_TRAFFIC_TOPUP_CC` are
//! set, the agent monitors its prepaid traffic balance and tops it up by
//! submitting a `PrepayTraffic` ledger op (CC transfer to PARTY_PREPAID_TRAFFIC)
//! whenever the balance drops below the configured minimum.
//!
//! Two attachment points (wired by the caller — see `lib.rs`):
//! 1. After every successful `submit_transaction` (fire-and-forget).
//! 2. On a 10-minute background timer (catches idle drift).
//!
//! Both share a re-entrancy guard so the topup itself (which submits a tx)
//! never recurses or stacks up under load. The runner owns its OWN
//! DAppProviderClient (separate JWT + gRPC connection from the main agent
//! client) so the topup's submit_transaction does NOT trigger the per-tx
//! hook a second time and never blocks the main client mutex.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, Mutex as TokioMutex, OwnedMutexGuard};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use tx_verifier::OperationExpectation;

use agent_logic::clock;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest, PrepayTrafficParams,
    TransactionOperation,
};

use crate::DAppProviderClient;

/// Bound on one topup attempt. A submit still running then is left to finish
/// in the background and holds off further attempts until it does.
const TOPUP_BUDGET: Duration = Duration::from_secs(300);

/// Period of the background balance check.
const CHECK_INTERVAL: Duration = Duration::from_secs(600);

/// Shared state for the auto-topup hook.
///
/// Owns its own `DAppProviderClient` so the topup's `submit_transaction`
/// does not recurse through the main agent client (whose post-success hook
/// fires this runner). `in_flight` is a try_lock guard — the periodic
/// timer and the per-tx hook can both fire concurrently without stacking.
pub struct TopupRunner {
    client: DAppProviderClient,
    party_id: String,
    min_cc: Decimal,
    topup_cc: Decimal,
    in_flight: Arc<TokioMutex<()>>,
}

impl TopupRunner {
    /// Build a runner if both env vars are set; returns `None` otherwise so
    /// callers can branch cleanly on "auto-topup disabled".
    pub fn new(
        client: DAppProviderClient,
        party_id: String,
        min_cc: Option<Decimal>,
        topup_cc: Option<Decimal>,
    ) -> Option<Self> {
        match (min_cc, topup_cc) {
            (Some(min_cc), Some(topup_cc)) => Some(Self {
                client,
                party_id,
                min_cc,
                topup_cc,
                in_flight: Arc::new(TokioMutex::new(())),
            }),
            _ => None,
        }
    }

    /// Check the prepaid traffic balance and top up if it's below the
    /// configured minimum. Best-effort: any error is logged at warn and
    /// swallowed — never fails the caller's tx flow.
    ///
    /// Re-entrancy: if another `maybe_topup` is already running, this call
    /// returns immediately.
    pub async fn maybe_topup(&self) {
        let Ok(guard) = Arc::clone(&self.in_flight).try_lock_owned() else {
            debug!("Auto-topup already in progress, skipping");
            return;
        };
        if let Err(e) = self.run_inner(guard, clock::deadline_after(TOPUP_BUDGET)).await {
            warn!(error = %e, "Auto-topup attempt failed");
        }
    }

    async fn run_inner(&self, guard: OwnedMutexGuard<()>, deadline: Instant) -> Result<()> {
        let mut client = self.client.clone();
        let pt = within(deadline, client.get_prepaid_traffic_balance()).await?;

        if pt.balance_cc >= self.min_cc {
            debug!(
                balance_cc = %pt.balance_cc,
                min_cc = %self.min_cc,
                "Prepaid balance above minimum, no topup"
            );
            return Ok(());
        }

        let cc_balance = within(deadline, self.fetch_cc_balance()).await?;
        if cc_balance < self.topup_cc {
            warn!(
                cc_balance = %cc_balance,
                topup_cc = %self.topup_cc,
                prepaid_balance_cc = %pt.balance_cc,
                "Auto-topup skipped: agent CC balance below configured topup amount"
            );
            return Ok(());
        }

        info!(
            balance_cc = %pt.balance_cc,
            min_cc = %self.min_cc,
            topup_cc = %self.topup_cc,
            "Auto-topping up prepaid traffic balance"
        );
        self.submit_topup("auto-topup", guard, deadline).await
    }

    /// Force a topup unconditionally (skips the balance < min check).
    /// Used by the onboarding flow to seed the prepaid pool with the first
    /// topup right after CC lands. Still respects CC-shortfall warning and
    /// re-entrancy.
    pub async fn force_topup(&self) -> Result<()> {
        let Ok(guard) = Arc::clone(&self.in_flight).try_lock_owned() else {
            return Err(anyhow!("Topup already in progress"));
        };
        let deadline = clock::deadline_after(TOPUP_BUDGET);

        let cc_balance = within(deadline, self.fetch_cc_balance()).await?;
        if cc_balance < self.topup_cc {
            warn!(
                cc_balance = %cc_balance,
                topup_cc = %self.topup_cc,
                "Forced topup skipped: agent CC balance below configured topup amount"
            );
            return Ok(());
        }
        self.submit_topup("first-topup", guard, deadline).await
    }

    async fn fetch_cc_balance(&self) -> Result<Decimal> {
        let balances = self.client.clone().get_balances().await?;
        Ok(balances
            .iter()
            .find(|b| b.is_canton_coin)
            .map(|b| b.total_amount.parse().unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO))
    }

    /// The submit runs in its own task, holding `guard` until it ends; it is
    /// waited for until `deadline` and never cancelled.
    async fn submit_topup(
        &self,
        command_prefix: &str,
        guard: OwnedMutexGuard<()>,
        deadline: Instant,
    ) -> Result<()> {
        let amount_str = self.topup_cc.to_string();
        let command_id = format!("{}-{}", command_prefix, clock::now_millis());
        let expectation = OperationExpectation::PrepayTraffic {
            sender_party: self.party_id.clone(),
            amount: amount_str.clone(),
            command_id: command_id.clone(),
        };
        let request = PrepareTransactionRequest {
            operation: TransactionOperation::PrepayTraffic as i32,
            params: Some(Params::PrepayTraffic(PrepayTrafficParams {
                amount: amount_str,
                description: Some(command_prefix.to_string()),
                command_id,
                amulet_cids: vec![],
            })),
            request_signature: None,
        };
        let mut client = self.client.clone();
        let submit = async move {
            client
                .submit_transaction(
                    request,
                    &expectation,
                    /*verbose=*/ false,
                    /*dry_run=*/ false,
                    /*force=*/ false,
                )
                .await
                .map(|r| r.update_id)
        };
        let task = spawn_submit(guard, submit)?;
        wait_submit(command_prefix.to_string(), self.topup_cc, task, deadline).await
    }

    /// Read-only view: return the current prepaid traffic balance via the
    /// runner's own client. Used by onboarding to print the post-topup
    /// state without standing up another client.
    pub async fn get_balance(&self) -> Result<crate::PrepaidTrafficBalance> {
        let mut client = self.client.clone();
        within(clock::deadline_after(TOPUP_BUDGET), client.get_prepaid_traffic_balance()).await
    }

    /// Spawn the runner's background task. Returns a `TopupTrigger` handle
    /// that the per-tx hook uses to nudge the runner without spawning a
    /// new task itself (avoids the Send bound on submit_transaction's
    /// future).
    ///
    /// The background task:
    /// - Listens on an mpsc channel for nudges from the per-tx hook.
    /// - Fires `maybe_topup` every 10 minutes regardless.
    /// Both go through `maybe_topup`'s try_lock guard, so they coalesce
    /// cleanly — a flood of per-tx nudges results in one topup attempt.
    pub fn spawn(self: Arc<Self>, shutdown: Shutdown) -> Result<TopupTrigger> {
        let (tx, rx) = mpsc::channel::<()>(16);
        let rx = Arc::new(TokioMutex::new(rx));
        let s = shutdown.clone();
        supervise::spawn_supervised("topup runner", shutdown, Policy::Restart, move || {
            run(Arc::clone(&self), Arc::clone(&rx), s.clone())
        })?;
        Ok(TopupTrigger { tx })
    }
}

async fn run(runner: Arc<TopupRunner>, rx: Arc<TokioMutex<mpsc::Receiver<()>>>, shutdown: Shutdown) {
    let mut rx = rx.lock().await;
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await; // consume immediate-fire initial tick
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => {
                info!("Topup runner shutting down");
                break;
            }
            _ = ticker.tick() => {
                runner.maybe_topup().await;
            }
            Some(_) = rx.recv() => {
                runner.maybe_topup().await;
            }
            else => break,
        }
    }
}

/// `call` within the attempt's deadline.
async fn within<T>(deadline: Instant, call: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout_at(deadline, call)
        .await
        .map_err(|_| anyhow!("no answer within the {}s topup budget", TOPUP_BUDGET.as_secs()))?
}

/// Run `submit` in its own task, holding `guard` until it ends.
fn spawn_submit<F>(guard: OwnedMutexGuard<()>, submit: F) -> Result<JoinHandle<Result<String>>>
where
    F: Future<Output = Result<String>> + Send + 'static,
{
    supervise::try_spawn("prepaid traffic topup", async move {
        let _guard = guard;
        submit.await
    })
    .ok_or_else(|| anyhow!("no tokio runtime for the topup submit"))
}

/// Wait for the submit until `deadline`; after that it is left to finish and
/// its outcome is logged when it does.
async fn wait_submit(
    kind: String,
    topup_cc: Decimal,
    mut task: JoinHandle<Result<String>>,
    deadline: Instant,
) -> Result<()> {
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(joined) => submitted(&kind, topup_cc, joined),
        Err(_) => {
            warn!(
                kind = %kind,
                "Topup submit still running after {}s; it continues in the background",
                TOPUP_BUDGET.as_secs()
            );
            let _ = supervise::try_spawn("topup outcome", async move {
                if let Err(e) = submitted(&kind, topup_cc, task.await) {
                    warn!(error = %e, kind = %kind, "Topup submit failed");
                }
            });
            Ok(())
        }
    }
}

fn submitted(kind: &str, topup_cc: Decimal, joined: Result<Result<String>, JoinError>) -> Result<()> {
    let update_id = joined.map_err(|e| anyhow!("topup submit task failed: {e}"))??;
    info!(
        update_id = %update_id,
        topup_cc = %topup_cc,
        kind = %kind,
        "Topup submitted"
    );
    Ok(())
}

/// Lightweight handle attached to the main `DAppProviderClient` so the
/// post-success hook can fire-and-forget a topup nudge without spawning a
/// new task itself. Cloning is cheap (just an `mpsc::Sender`).
#[derive(Clone)]
pub struct TopupTrigger {
    tx: mpsc::Sender<()>,
}

impl TopupTrigger {
    /// Try to nudge the topup runner. Non-blocking and silently drops the
    /// nudge if the channel is full — `maybe_topup`'s try_lock guard
    /// already coalesces, so an extra dropped nudge is harmless.
    pub fn nudge(&self) {
        let _ = self.tx.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_logic::config::BaseConfig;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn runner() -> TopupRunner {
        runner_at("http://127.0.0.1:1")
    }

    fn runner_at(url: &str) -> TopupRunner {
        let config = BaseConfig::test_minimal().unwrap();
        let channel = tonic::transport::Endpoint::from_shared(url.to_string()).unwrap().connect_lazy();
        let client = DAppProviderClient::from_channel(
            channel,
            &config.party_id,
            &config.role,
            &config.private_key,
            config.token_ttl_secs,
            None,
            &config.ledger_service_public_key,
        )
        .unwrap();
        TopupRunner::new(client, config.party_id, Some(Decimal::ONE), Some(Decimal::TEN)).unwrap()
    }

    // A slow submit is waited for up to the budget, then left running and
    // holding off the next attempt until it ends
    #[tokio::test(start_paused = true)]
    async fn a_slow_submit_is_left_running_and_holds_off_the_next_attempt() {
        let lock = Arc::new(TokioMutex::new(()));
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let task = spawn_submit(lock.clone().try_lock_owned().unwrap(), async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            d.store(true, Ordering::SeqCst);
            Ok("u1".to_string())
        })
        .unwrap();
        let deadline = Instant::now() + TOPUP_BUDGET;
        wait_submit("auto-topup".into(), Decimal::TEN, task, deadline).await.unwrap();
        assert!(Instant::now() >= deadline);
        assert!(lock.try_lock().is_err(), "the running submit holds the in-flight lock");
        tokio::time::sleep(Duration::from_secs(400)).await;
        assert!(done.load(Ordering::SeqCst), "the submit was not cancelled");
        assert!(lock.try_lock().is_ok());
    }

    // The runner's own submit: past its deadline it keeps running and keeps the lock
    #[tokio::test]
    async fn a_topup_submit_past_its_deadline_keeps_the_lock_until_it_ends() {
        let slow = crate::test_util::Fake::Slow(Duration::from_secs(1));
        let ledger = crate::test_util::FakeLedger::start(slow).await;
        let runner = runner_at(&ledger.url);
        let guard = Arc::clone(&runner.in_flight).try_lock_owned().unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        runner.submit_topup("auto-topup", guard, deadline).await.unwrap();
        assert!(runner.in_flight.try_lock().is_err(), "the running submit holds the in-flight lock");
        let freed = tokio::time::timeout(Duration::from_secs(30), async {
            while runner.in_flight.try_lock().is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        freed.await.expect("the lock is free once the submit ends");
        assert_eq!(ledger.calls("PrepareTransaction"), 1, "the submit reached the ledger");
        ledger.stop().await;
    }

    #[tokio::test]
    async fn submit_failures_within_the_budget_are_returned() {
        let lock = Arc::new(TokioMutex::new(()));
        let deadline = clock::deadline_after(TOPUP_BUDGET);
        let failed = spawn_submit(lock.clone().try_lock_owned().unwrap(), async { Err(anyhow!("rejected")) }).unwrap();
        let err = wait_submit("auto-topup".into(), Decimal::TEN, failed, deadline).await.unwrap_err();
        assert_eq!(err.to_string(), "rejected");
        let panicked = spawn_submit(lock.clone().try_lock_owned().unwrap(), async { panic!("submit bug") }).unwrap();
        let err = wait_submit("auto-topup".into(), Decimal::TEN, panicked, deadline).await.unwrap_err();
        assert!(err.to_string().starts_with("topup submit task failed"), "{err}");
        assert!(lock.try_lock().is_ok(), "the lock is released after a panic");
    }

    #[tokio::test]
    async fn an_attempt_in_progress_makes_the_next_one_return_at_once() {
        let runner = runner();
        let _held = runner.in_flight.clone().try_lock_owned().unwrap();
        tokio::time::timeout(Duration::from_secs(1), runner.maybe_topup()).await.unwrap();
        let err = runner.force_topup().await.unwrap_err();
        assert_eq!(err.to_string(), "Topup already in progress");
    }

    #[tokio::test(start_paused = true)]
    async fn a_balance_read_that_never_answers_is_bounded() {
        let err = within(Instant::now() + TOPUP_BUDGET, std::future::pending::<Result<()>>()).await.unwrap_err();
        assert!(err.to_string().contains("300s topup budget"), "{err}");
    }

    #[test]
    fn spawning_outside_a_runtime_is_an_error() {
        let runner = {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async { runner() })
        };
        assert!(Arc::new(runner).spawn(Shutdown::new()).is_err());
    }
}
