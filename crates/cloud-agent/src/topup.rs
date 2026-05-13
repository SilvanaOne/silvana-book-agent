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

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tracing::{debug, info, warn};
use tx_verifier::OperationExpectation;

use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest, PrepayTrafficParams,
    TransactionOperation,
};

use crate::DAppProviderClient;

/// Shared state for the auto-topup hook.
///
/// Owns its own `DAppProviderClient` so the topup's `submit_transaction`
/// does not recurse through the main agent client (whose post-success hook
/// fires this runner). `in_flight` is a try_lock guard — the periodic
/// timer and the per-tx hook can both fire concurrently without stacking.
pub struct TopupRunner {
    client: TokioMutex<DAppProviderClient>,
    party_id: String,
    min_cc: Decimal,
    topup_cc: Decimal,
    in_flight: TokioMutex<()>,
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
                client: TokioMutex::new(client),
                party_id,
                min_cc,
                topup_cc,
                in_flight: TokioMutex::new(()),
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
        let _guard = match self.in_flight.try_lock() {
            Ok(g) => g,
            Err(_) => {
                debug!("Auto-topup already in progress, skipping");
                return;
            }
        };
        if let Err(e) = self.run_inner().await {
            warn!(error = %e, "Auto-topup attempt failed");
        }
    }

    async fn run_inner(&self) -> Result<()> {
        let pt = {
            let mut client = self.client.lock().await;
            client.get_prepaid_traffic_balance().await?
        };

        if pt.balance_cc >= self.min_cc {
            debug!(
                balance_cc = %pt.balance_cc,
                min_cc = %self.min_cc,
                "Prepaid balance above minimum, no topup"
            );
            return Ok(());
        }

        let cc_balance = self.fetch_cc_balance().await?;
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
        self.submit_topup("auto-topup").await?;
        Ok(())
    }

    /// Force a topup unconditionally (skips the balance < min check).
    /// Used by the onboarding flow to seed the prepaid pool with the first
    /// topup right after CC lands. Still respects CC-shortfall warning and
    /// re-entrancy.
    pub async fn force_topup(&self) -> Result<()> {
        let _guard = match self.in_flight.try_lock() {
            Ok(g) => g,
            Err(_) => return Err(anyhow!("Topup already in progress")),
        };

        let cc_balance = self.fetch_cc_balance().await?;
        if cc_balance < self.topup_cc {
            warn!(
                cc_balance = %cc_balance,
                topup_cc = %self.topup_cc,
                "Forced topup skipped: agent CC balance below configured topup amount"
            );
            return Ok(());
        }
        self.submit_topup("first-topup").await?;
        Ok(())
    }

    async fn fetch_cc_balance(&self) -> Result<Decimal> {
        let mut client = self.client.lock().await;
        let balances = client.get_balances().await?;
        Ok(balances
            .iter()
            .find(|b| b.is_canton_coin)
            .map(|b| b.total_amount.parse().unwrap_or(Decimal::ZERO))
            .unwrap_or(Decimal::ZERO))
    }

    async fn submit_topup(&self, command_prefix: &str) -> Result<()> {
        let amount_str = self.topup_cc.to_string();
        let command_id = format!(
            "{}-{}",
            command_prefix,
            chrono::Utc::now().timestamp_millis()
        );
        let expectation = OperationExpectation::PrepayTraffic {
            sender_party: self.party_id.clone(),
            amount: amount_str.clone(),
            command_id: command_id.clone(),
        };
        let result = {
            let mut client = self.client.lock().await;
            client
                .submit_transaction(
                    PrepareTransactionRequest {
                        operation: TransactionOperation::PrepayTraffic as i32,
                        params: Some(Params::PrepayTraffic(PrepayTrafficParams {
                            amount: amount_str,
                            description: Some(command_prefix.to_string()),
                            command_id,
                            amulet_cids: vec![],
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    /*verbose=*/ false,
                    /*dry_run=*/ false,
                    /*force=*/ false,
                )
                .await?
        };
        info!(
            update_id = %result.update_id,
            topup_cc = %self.topup_cc,
            kind = %command_prefix,
            "Topup submitted"
        );
        Ok(())
    }

    /// Read-only view: return the current prepaid traffic balance via the
    /// runner's own client. Used by onboarding to print the post-topup
    /// state without standing up another client.
    pub async fn get_balance(&self) -> Result<crate::PrepaidTrafficBalance> {
        let mut client = self.client.lock().await;
        client.get_prepaid_traffic_balance().await
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
    pub fn spawn(self: Arc<Self>, shutdown: Shutdown) -> TopupTrigger {
        let (tx, mut rx) = mpsc::channel::<()>(16);
        let runner = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(600));
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
        });
        TopupTrigger { tx }
    }
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
