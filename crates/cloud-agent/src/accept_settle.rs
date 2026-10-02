//! Single-tx settlement for the `buy` / `sell` (taker) commands.
//!
//! Bundles `Accept_Dvp + Allocate + all fees + own traffic fee` into one
//! `Execute_MultiCall` on-chain transaction, matching the pattern used by
//! `canton-agent/crates/dvp` `swap-test` command M2-compose.
//!
//! The taker side never creates a DvpProposal — the LP creates it when the
//! RFQ quote is accepted. This module just bundles the taker's subsequent
//! Accept + Allocate with fee payments.
//!
//! Fee amounts are sourced from `BaseConfig` / env vars
//! `AGENT_FEE_CC` / `PARTICIPANT_FEE_CC` / `SIGNATURE_FEE_CC` via
//! `agent_logic::fees::taker_settlement_fees`.
//! Traffic fee is estimated in two phases: the first `PrepareTransaction`
//! returns `traffic_estimate.total_bytes`; the second is submitted with the
//! actual CC amount computed from bytes × rate.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use rust_decimal::prelude::FromStr as _;
use rust_decimal::Decimal;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use agent_logic::clock;
use agent_logic::config::BaseConfig;
use agent_logic::confirm::{confirm_transaction, ConfirmLock};
use agent_logic::fees::FeeTarget;
use agent_logic::settlement::StepResult;
use agent_logic::{supervise, sync};
use orderbook_proto::ledger::{
    multi_call_op::Op, prepare_transaction_request::Params, ExecuteMultiCallParams,
    ExecuteTransactionResponse, McAcceptDvpAndAllocate, McBatchTransfer, McTransferTarget,
    MultiCallOp, PrepareTransactionRequest, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use crate::holdings_cache::{CcView, ReservationGuard, TEMPLATE_HOLDING};
use crate::ledger_client::{self, is_ambiguous_execute_error, within, DAppProviderClient};
use crate::payment_queue::{process_tx_result, select_amulets_for_allocation};

/// Longest wait for the multicall execute; past it the outcome is unknown
/// and the execute goes on in the background.
pub const EXECUTE_WAIT: Duration = Duration::from_secs(300);

/// Fee debits still running after this long are logged; they are never cut short.
pub const FEE_DEBIT_SLOW_AFTER: Duration = Duration::from_secs(60);

/// A multicall whose execute may have committed. Its input reservations are
/// kept until they expire; the caller must not re-quote before reconciling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleOutcomeUnknown(pub String);

impl fmt::Display for SettleOutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "settle outcome unknown: {}", self.0)
    }
}

impl std::error::Error for SettleOutcomeUnknown {}

/// Whether a failed multicall execute may still have committed.
fn outcome_unknown(e: &anyhow::Error) -> bool {
    is_ambiguous_execute_error(e) || format!("{e:#}").contains("DUPLICATE_COMMAND")
}

/// Fee debits started after committed multicalls, so the caller can let them
/// finish before it exits.
#[derive(Debug, Default)]
pub struct FeeDebits(Mutex<Vec<JoinHandle<()>>>);

impl FeeDebits {
    fn push(&self, debit: JoinHandle<()>) {
        let mut debits = sync::lock(&self.0);
        debits.retain(|d| !d.is_finished());
        debits.push(debit);
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        sync::lock(&self.0).len()
    }

    /// Debits started so far that have not finished.
    pub fn running(&self) -> usize {
        sync::lock(&self.0).iter().filter(|d| !d.is_finished()).count()
    }

    /// Wait up to `limit` for the debits started so far; returns how many are still running.
    pub async fn wait(&self, limit: Duration) -> usize {
        let debits = std::mem::take(&mut *sync::lock(&self.0));
        let deadline = clock::deadline_after(limit);
        let mut running = 0usize;
        for debit in debits {
            if tokio::time::timeout_at(deadline, debit).await.is_err() {
                running = running.saturating_add(1);
            }
        }
        running
    }
}

/// Options/flags for the multicall settlement (verbose, dry_run, force, confirm).
pub struct MulticallSettler {
    pub config: BaseConfig,
    pub amulet_cache: CcView,
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
    pub confirm: bool,
    pub confirm_lock: ConfirmLock,
    pub fee_debits: FeeDebits,
}

/// (verbose, dry_run, force) for the submit.
type SubmitFlags = (bool, bool, bool);

/// The validated multicall, ready to execute.
struct Prepared {
    client: DAppProviderClient,
    holding_cids: Vec<String>,
    request: PrepareTransactionRequest,
    expectation: OperationExpectation,
}

/// Fetch unlocked CIP-56 Holdings owned by `party_id`. Mirrors
/// `dvp/commands/multicall.rs::fetch_holdings` — filters by `owner == party_id`
/// and `lock is null`. Used to supply USDC (or any non-CC) allocation inputs
/// to the multicall's `holding_cids` pool.
/// Fetch the party's unlocked CIP-56 Holding cids for ONE instrument under ONE
/// registrar. The multicall's holding pool must not mix registrars: a re-issued
/// token (e.g. devnet cETH) can leave the party holding the same `id` under two
/// registrars, and the allocation's registry rejects foreign cids ("Contract
/// group identifier mismatch"). `expected_id` is the on-chain instrument id and
/// `expected_source` the expected registrar (instrument.source); when
/// `expected_source` is empty (registry not yet resolved) the source filter is
/// skipped (lenient) but the instrument-id filter always applies.
async fn fetch_unlocked_cip56_holdings(
    client: &mut DAppProviderClient,
    party_id: &str,
    expected_id: &str,
    expected_source: &str,
) -> Result<Vec<String>> {
    let contracts = client
        .get_active_contracts(&[TEMPLATE_HOLDING.to_string()])
        .await?;
    Ok(contracts
        .into_iter()
        .filter(|c| {
            let Some(ref args) = c.create_arguments else { return false };
            let json = crate::prost_struct_to_json(args);
            holding_matches_pool(&json, party_id, expected_id, expected_source)
        })
        .map(|c| c.contract_id)
        .collect())
}

/// Whether one decoded Holding may enter the taker's single-registrar multicall
/// pool: owned by `party_id`, unlocked, of `expected_id`, and (unless
/// `expected_source` is empty) under registrar `expected_source`.
fn holding_matches_pool(
    json: &serde_json::Value,
    party_id: &str,
    expected_id: &str,
    expected_source: &str,
) -> bool {
    let owner_ok = json.get("owner").and_then(|o| o.as_str()) == Some(party_id);
    let unlocked = json.pointer("/lock").is_none_or(serde_json::Value::is_null);
    let id_ok = json.pointer("/instrument/id").and_then(|v| v.as_str()) == Some(expected_id);
    let source_ok = expected_source.is_empty()
        || json.pointer("/instrument/source").and_then(|v| v.as_str()) == Some(expected_source);
    owner_ok && unlocked && id_ok && source_ok
}

#[cfg(test)]
mod holding_pool_tests {
    use super::holding_matches_pool;
    use serde_json::json;

    fn holding(id: &str, source: &str, owner: &str, locked: bool) -> serde_json::Value {
        json!({
            "owner": owner,
            "lock": if locked { json!({"lockers": []}) } else { json!(null) },
            "instrument": { "id": id, "source": source },
        })
    }

    #[test]
    fn keeps_only_same_instrument_and_registrar() {
        let party = "c2cde443";
        let new_reg = "rails-cethMain-1-dev::12200b6d";
        let old_reg = "ceth-validator-dev::122078c9";

        // exact match
        assert!(holding_matches_pool(&holding("cETH", new_reg, party, false), party, "cETH", new_reg));
        // foreign registrar (old-registry cETH) → excluded
        assert!(!holding_matches_pool(&holding("cETH", old_reg, party, false), party, "cETH", new_reg));
        // wrong instrument → excluded
        assert!(!holding_matches_pool(&holding("USDC", "test-token-1::x", party, false), party, "cETH", new_reg));
        // locked → excluded
        assert!(!holding_matches_pool(&holding("cETH", new_reg, party, true), party, "cETH", new_reg));
        // not owner → excluded
        assert!(!holding_matches_pool(&holding("cETH", new_reg, "someone-else", false), party, "cETH", new_reg));
        // lenient: empty expected_source keeps any registrar of the right id
        assert!(holding_matches_pool(&holding("cETH", old_reg, party, false), party, "cETH", ""));
    }

    #[test]
    fn a_missing_lock_counts_as_unlocked() {
        let held = json!({"owner": "p", "instrument": {"id": "cETH", "source": "r"}});
        assert!(holding_matches_pool(&held, "p", "cETH", "r"));
    }
}

/// The production execute: prepare, verify, sign and execute the multicall.
async fn submit_multicall(
    mut client: DAppProviderClient,
    request: PrepareTransactionRequest,
    expectation: OperationExpectation,
    (verbose, dry_run, force): SubmitFlags,
) -> Result<ExecuteTransactionResponse> {
    client
        .submit_transaction(request, &expectation, verbose, dry_run, force)
        .await
}

/// Cache update once the execute has ended: inputs consumed on commit, kept
/// reserved while the outcome is unknown, released otherwise.
async fn record_execute(cache: &CcView, cids: &[String], result: &Result<ExecuteTransactionResponse>) {
    match result {
        Ok(resp) if resp.success => {
            process_tx_result(cache, cids, resp).await;
            cache.release_reservations(cids).await;
        }
        Err(e) if outcome_unknown(e) => warn!(
            "Multicall outcome unknown; keeping {} input reservation(s) until they expire",
            cids.len()
        ),
        _ => cache.release_reservations(cids).await,
    }
}

/// Run the execute in its own task, which also updates the cache when it ends.
/// A wait past `wait`, a panic or an ambiguous error is [`SettleOutcomeUnknown`].
async fn execute_watched<F>(
    cache: CcView,
    holding_cids: Vec<String>,
    proposal_id: &str,
    wait: Duration,
    execute: F,
) -> Result<ExecuteTransactionResponse>
where
    F: Future<Output = Result<ExecuteTransactionResponse>> + Send + 'static,
{
    let task_cache = cache.clone();
    let task_cids = holding_cids.clone();
    let task = supervise::try_spawn("multicall execute", async move {
        let result = execute.await;
        record_execute(&task_cache, &task_cids, &result).await;
        result
    });
    let Some(mut task) = task else {
        cache.release_reservations(&holding_cids).await;
        anyhow::bail!("Multicall for {proposal_id} not started: no tokio runtime");
    };
    match tokio::time::timeout(wait, &mut task).await {
        Ok(Ok(Ok(resp))) => Ok(resp),
        Ok(Ok(Err(e))) if outcome_unknown(&e) => Err(SettleOutcomeUnknown(format!("{e:#}")).into()),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(e)) => Err(SettleOutcomeUnknown(format!("multicall execute task failed: {e}")).into()),
        Err(_) => {
            let pid = proposal_id.to_string();
            let _ = supervise::try_spawn("multicall outcome", async move {
                match task.await {
                    Ok(Ok(r)) => warn!(
                        "Multicall for {} finished after the wait: update={} success={}",
                        pid, r.update_id, r.success
                    ),
                    Ok(Err(e)) => warn!("Multicall for {} failed after the wait: {:#}", pid, e),
                    Err(e) => warn!("Multicall for {} ended after the wait: {}", pid, e),
                }
            });
            Err(SettleOutcomeUnknown(format!(
                "multicall for {proposal_id} still running after {wait:?}; it continues in the background"
            ))
            .into())
        }
    }
}

async fn create_client(config: &BaseConfig) -> Result<DAppProviderClient> {
    DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(config.request_timeout_secs),
    )
    .await
}

/// Debits one off-chain processing fee; the ledger client in production.
trait FeeDebitClient: Send {
    fn pay_processing_fee(&mut self, proposal_id: &str, fee_type: &str) -> impl Future<Output = Result<()>> + Send;
}

impl FeeDebitClient for DAppProviderClient {
    fn pay_processing_fee(&mut self, proposal_id: &str, fee_type: &str) -> impl Future<Output = Result<()>> + Send {
        DAppProviderClient::pay_processing_fee(self, proposal_id, fee_type)
    }
}

/// Both processing fee debits of a committed multicall, over the client `connected` gave.
async fn pay_processing_fees_with<C: FeeDebitClient>(connected: Result<C>, proposal_id: &str) {
    let mut client = match connected {
        Ok(c) => c,
        Err(e) => {
            warn!(
                proposal_id = %proposal_id,
                error = %e,
                "Off-chain processing fee debits skipped: no ledger client (multicall already committed; reconcile manually)",
            );
            return;
        }
    };
    // Pay the two processing fees off-chain. Sequential, not atomic with
    // the multicall — if either debit fails (e.g. insufficient prepaid
    // balance) the multicall has already committed; operator reconciles.
    // The two fees are distinct: dvp first, then allocate. Failures are
    // logged-and-continued so a transient blip on the alloc fee doesn't
    // abort settlement reporting.
    for fee_type in ["dvp", "allocate"] {
        if let Err(e) = client.pay_processing_fee(proposal_id, fee_type).await {
            tracing::warn!(
                proposal_id = %proposal_id,
                fee_type = %fee_type,
                error = %e,
                "Off-chain {} processing fee debit failed (multicall already committed; reconcile manually)",
                fee_type,
            );
        }
    }
}

/// Both fee debits of a committed multicall, run to their end; true when they
/// were still running after `slow_after`.
async fn run_fee_debits(debits: impl Future<Output = ()>, proposal_id: &str, slow_after: Duration) -> bool {
    let on_slow = || {
        warn!(
            proposal_id = %proposal_id,
            "Off-chain processing fee debits still running after {}s (multicall already committed)",
            slow_after.as_secs(),
        );
    };
    supervise::run_to_end(debits, slow_after, on_slow).await.1
}

/// Longest wait at exit for fee debits: one connect plus four calls, each within its own bound.
fn fee_debit_wait(config: &BaseConfig) -> Duration {
    let calls = ledger_client::call_wait(config.request_timeout_secs).saturating_mul(4);
    ledger_client::connect_wait(config.connection_timeout_secs, config.request_timeout_secs)
        .saturating_add(calls)
}

#[cfg(test)]
impl MulticallSettler {
    /// A settler with an empty cache and 1s ledger timeouts.
    pub(crate) fn for_tests(url: &str) -> Self {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = url.to_string();
        config.connection_timeout_secs = 1;
        config.request_timeout_secs = 1;
        MulticallSettler {
            config,
            amulet_cache: crate::holdings_cache::HoldingsCache::new(false).cc(),
            verbose: false,
            dry_run: false,
            force: false,
            confirm: false,
            confirm_lock: agent_logic::confirm::new_confirm_lock(),
            fee_debits: FeeDebits::default(),
        }
    }
}

impl MulticallSettler {
    /// Accept the LP's DvpProposal + allocate our side + pay all fees + own traffic
    /// fee — in a single `Execute_MultiCall` transaction.
    ///
    /// `allocation_instrument_id` is the instrument the taker is allocating
    /// (e.g. "USDC" for a buy on CC-USDC, "CC" for a sell). When it's not CC,
    /// the settler fetches the party's CIP-56 Holdings and appends their cids
    /// to `holding_cids` alongside the CC amulets used for fee payments.
    /// `dvp_processing_fee_cc` / `alloc_processing_fee_cc` MUST be CC amounts,
    /// already converted from the proposal's USD fees via `cc_usd_rate`.
    /// The matching `*_usd` strings are passed solely so we can assert — at
    /// this defensive boundary — that the caller did not skip the conversion
    /// (see `fill_loop.rs::fetch_settlement_fees`).
    ///
    /// An error carrying [`SettleOutcomeUnknown`] means the multicall may have
    /// committed; any other error means it did not.
    #[allow(clippy::too_many_arguments)]
    pub async fn accept_and_settle(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        dvp_processing_fee_cc: &str,
        alloc_processing_fee_cc: &str,
        dvp_processing_fee_usd: &str,
        alloc_processing_fee_usd: &str,
        allocation_instrument_id: &str,
        allocation_cc: Option<Decimal>,
    ) -> Result<StepResult> {
        // Defense-in-depth against the USD-as-CC regression: if the "CC" fee
        // equals the proposal's USD fee (and USD is non-zero), upstream skipped
        // the rate conversion — refuse to publish an underpayment on-chain.
        let dvp_usd = Decimal::from_str(dvp_processing_fee_usd).unwrap_or(Decimal::ZERO);
        let alloc_usd = Decimal::from_str(alloc_processing_fee_usd).unwrap_or(Decimal::ZERO);
        let dvp_cc = Decimal::from_str(dvp_processing_fee_cc).unwrap_or(Decimal::ZERO);
        let alloc_cc = Decimal::from_str(alloc_processing_fee_cc).unwrap_or(Decimal::ZERO);
        if (dvp_usd > Decimal::ZERO && dvp_cc == dvp_usd)
            || (alloc_usd > Decimal::ZERO && alloc_cc == alloc_usd)
        {
            anyhow::bail!(
                "fee amounts look un-converted (cc == usd): dvp_cc={} dvp_usd={} alloc_cc={} alloc_usd={} — check fetch_settlement_fees rate conversion",
                dvp_processing_fee_cc, dvp_processing_fee_usd,
                alloc_processing_fee_cc, alloc_processing_fee_usd,
            );
        }

        if self.confirm && !self.dry_run {
            confirm_transaction(
                &self.confirm_lock,
                "Accept+Settle (multicall)",
                &format!("proposal: {}, dvp_proposal: {}", proposal_id, dvp_proposal_cid),
            )
            .await?;
        }

        // 1. Processing fees are NOT embedded in the multicall payload anymore —
        //    they're debited off-chain via `client.pay_processing_fee` AFTER
        //    the multicall commits. Multicall body is `Accept_Dvp + Allocate`
        //    only.
        let fee_targets: Vec<FeeTarget> = Vec::new();

        // 2. Select amulets to cover CC allocation + margin only.
        //    Fees and traffic billing are off-chain (debited from prepaid pool).
        let alloc_cc = allocation_cc.unwrap_or(Decimal::ZERO);
        let margin = Decimal::from(5);
        let Some(estimated_cc) = alloc_cc.checked_add(margin) else {
            anyhow::bail!("Allocation of {alloc_cc} CC for proposal {proposal_id} is out of range");
        };
        let selectable = self.amulet_cache.get_selectable_amulets().await;
        let mut selected = select_amulets_for_allocation(&selectable, estimated_cc);
        if selected.is_empty() {
            // Splitter-reserve fail-open: v1 settle success outranks reserve
            // preservation — retry including the reserve.
            let with_reserve = self.amulet_cache.get_selectable_amulets_incl_reserve().await;
            selected = select_amulets_for_allocation(&with_reserve, estimated_cc);
        }
        if selected.is_empty() {
            anyhow::bail!(
                "Insufficient amulets for multicall settlement: need ~{} CC (alloc {} + margin {})",
                estimated_cc, alloc_cc, margin,
            );
        }
        let holding_cids: Vec<String> =
            selected.iter().map(|a| a.contract_id.clone()).collect();
        let payment_id = format!("settle-{}", proposal_id);
        if !self.amulet_cache.reserve(&holding_cids, &payment_id).await {
            anyhow::bail!("Failed to reserve amulets for multicall settlement");
        }
        // Released when anything before the execute fails, unwinds or is cancelled
        let reserved = ReservationGuard::new(Arc::clone(self.amulet_cache.inner()), holding_cids.clone());

        let prepared = self
            .prepare_settle(proposal_id, dvp_proposal_cid, allocation_instrument_id, &fee_targets, holding_cids)
            .await;
        self.execute_reserved(reserved, prepared, proposal_id, EXECUTE_WAIT, submit_multicall)
            .await
    }

    /// Execute a prepared multicall in its own task, which is never cancelled
    /// and updates the cache itself. A failed prepare releases the inputs.
    async fn execute_reserved<X, F>(
        &self,
        reserved: ReservationGuard,
        prepared: Result<Prepared>,
        proposal_id: &str,
        wait: Duration,
        execute: X,
    ) -> Result<StepResult>
    where
        X: FnOnce(DAppProviderClient, PrepareTransactionRequest, OperationExpectation, SubmitFlags) -> F,
        F: Future<Output = Result<ExecuteTransactionResponse>> + Send + 'static,
    {
        let Prepared { client, holding_cids, request, expectation } = match prepared {
            Ok(p) => p,
            Err(e) => {
                self.amulet_cache.release_reservations(&reserved.disarm()).await;
                return Err(e);
            }
        };
        let flags = (self.verbose, self.dry_run, self.force);
        let _ = reserved.disarm();
        let exec_result = execute_watched(
            self.amulet_cache.clone(),
            holding_cids,
            proposal_id,
            wait,
            execute(client, request, expectation, flags),
        )
        .await?;
        Ok(self.finish(proposal_id, exec_result))
    }

    /// Everything before the execute: the client, the CIP-56 inputs of a
    /// non-CC allocation, the traffic estimate. Bounded, and nothing is submitted.
    async fn prepare_settle(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        allocation_instrument_id: &str,
        fee_targets: &[FeeTarget],
        mut holding_cids: Vec<String>,
    ) -> Result<Prepared> {
        let config = &self.config;
        let connect_wait =
            ledger_client::connect_wait(config.connection_timeout_secs, config.request_timeout_secs);
        let mut client = within("ledger connect", connect_wait, create_client(config)).await?;

        // Non-CC allocation (e.g. USDC for a buy on CC-USDC): fetch the party's
        // CIP-56 Holdings for that instrument and add them to the multicall's
        // unified holding pool. Amulets cover the fee payments; CIP-56 holdings
        // cover the allocation input.
        if !allocation_instrument_id.eq_ignore_ascii_case("cc")
            && !allocation_instrument_id.eq_ignore_ascii_case("amulet")
        {
            // Only holdings of THIS instrument under its configured registrar
            // may enter the pool — mixing registrars fails the on-ledger
            // allocation. `resolve_instrument` gives the on-chain id + the
            // GetInstruments/DB registry (same source the atomic path trusts).
            let (on_chain_id, registry) = config.resolve_instrument(allocation_instrument_id);
            let cip56 = within(
                "CIP-56 holdings lookup",
                ledger_client::stream_wait(config.request_timeout_secs),
                fetch_unlocked_cip56_holdings(&mut client, &config.party_id, &on_chain_id, &registry),
            )
            .await?;
            if cip56.is_empty() {
                anyhow::bail!(
                    "Taker has no unlocked CIP-56 holdings to allocate {} under registrar '{}' for proposal {}",
                    allocation_instrument_id, registry, proposal_id
                );
            }
            tracing::debug!(
                "Multicall: adding {} CIP-56 {} holdings to holding_cids pool",
                cip56.len(), allocation_instrument_id
            );
            holding_cids.extend(cip56);
        }

        // Phase 1 — fees are fully off-chain, multicall body is just
        // Accept_Dvp + Allocate.
        let placeholder_batches = self.build_batch_transfers(fee_targets);
        let estimate = client.prepare_transaction(PrepareTransactionRequest {
            operation: TransactionOperation::ExecuteMulticall as i32,
            params: Some(Params::ExecuteMulticall(self.build_multicall_params(
                proposal_id,
                dvp_proposal_cid,
                placeholder_batches,
                &holding_cids,
            ))),
            request_signature: None,
        });
        let prep = within("traffic estimate", ledger_client::call_wait(config.request_timeout_secs), estimate)
            .await
            .context("PrepareTransaction (traffic estimate) failed")?;
        let traffic_bytes = prep
            .traffic_estimate
            .as_ref()
            .map(|t| t.total_bytes)
            .unwrap_or(0);
        debug!(
            "Multicall traffic estimate: {} bytes (proposal={})",
            traffic_bytes, proposal_id
        );

        // Phase 2 — execute. Traffic billing and processing/agent/participant
        // /signature fees are off-chain (debited from prepaid pool by ledger);
        // multicall body is Accept_Dvp + Allocate only.
        let _ = (fee_targets, traffic_bytes); // signature/legacy unused
        let final_batches: Vec<McBatchTransfer> = Vec::new();
        let op_count = final_batches.len().saturating_add(1); // + AcceptDvpAndAllocate
        let params_final =
            self.build_multicall_params(proposal_id, dvp_proposal_cid, final_batches, &holding_cids);
        let expectation = OperationExpectation::ExecuteMulticall {
            party: self.config.party_id.clone(),
            op_count,
        };
        let request = PrepareTransactionRequest {
            operation: TransactionOperation::ExecuteMulticall as i32,
            params: Some(Params::ExecuteMulticall(params_final)),
            request_signature: None,
        };
        Ok(Prepared { client, holding_cids, request, expectation })
    }

    /// Report a decided multicall and start its fee debits. Never an error:
    /// the multicall cannot be undone at this point.
    fn finish(&self, proposal_id: &str, exec_result: ExecuteTransactionResponse) -> StepResult {
        let traffic = exec_result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
        let cid = exec_result.contract_id.clone().unwrap_or_default();
        info!(
            "Multicall submitted for {}: accept+allocate cid={} traffic={} bytes (fees off-chain)",
            proposal_id, cid, traffic,
        );
        // Fees are debited only for a committed multicall, not for a dry run
        if exec_result.success {
            self.debit_fees(proposal_id);
        }
        StepResult {
            contract_id: cid,
            update_id: exec_result.update_id,
            traffic_total: traffic,
        }
    }

    /// Debit the processing fees of a committed multicall in the background;
    /// a failure is only logged. Call it once per commit.
    pub fn debit_fees(&self, proposal_id: &str) {
        self.debit_fees_with(proposal_id, |config| async move { create_client(&config).await });
    }

    /// [`Self::debit_fees`] over the client that `connect` makes.
    fn debit_fees_with<C, F, Fut>(&self, proposal_id: &str, connect: F)
    where
        C: FeeDebitClient,
        F: FnOnce(BaseConfig) -> Fut,
        Fut: Future<Output = Result<C>> + Send + 'static,
    {
        let connecting = connect(self.config.clone());
        let proposal_id = proposal_id.to_string();
        let debits = supervise::try_spawn("processing fee debits", async move {
            let calls = async { pay_processing_fees_with(connecting.await, &proposal_id).await };
            run_fee_debits(calls, &proposal_id, FEE_DEBIT_SLOW_AFTER).await;
        });
        if let Some(debits) = debits {
            self.fee_debits.push(debits);
        }
    }

    /// Group fee targets by receiver into a list of `McBatchTransfer`
    /// ops (multicall-v1). With all fees off-chain, this is now always
    /// called with an empty input and returns an empty list. Kept as a
    /// helper in case future fee categories need to ride on-chain again.
    /// Server-side fallbacks fill `transfer_factory_cid`, `expected_admin`,
    /// `instrument_admin`, and `extra_args_json` when left empty/None
    /// (see `transactions::multicall::build_multicall_command`).
    #[allow(dead_code)]
    fn build_batch_transfers(
        &self,
        fee_targets: &[FeeTarget],
    ) -> Vec<McBatchTransfer> {
        if fee_targets.is_empty() {
            return Vec::new();
        }
        // Flatten fee targets into (receiver, amount, desc).
        let all: Vec<(String, String, String)> = fee_targets
            .iter()
            .map(|t| (t.receiver.clone(), t.amount_cc.clone(), t.description.clone()))
            .collect();

        // Stable group-by-receiver (first-seen order).
        let mut by_receiver: std::collections::HashMap<String, Vec<McTransferTarget>> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (receiver, amount, desc) in all {
            let entry = by_receiver.entry(receiver.clone());
            if matches!(entry, std::collections::hash_map::Entry::Vacant(_)) {
                order.push(receiver.clone());
            }
            entry.or_default().push(McTransferTarget {
                receiver: receiver.clone(),
                amount,
                description: Some(desc),
            });
        }

        // Timestamps — match cc_transfer_builders.rs formatting.
        let now = clock::now_utc();
        let window = chrono::TimeDelta::try_seconds(10)
            .and_then(|d| now.checked_sub_signed(d))
            .zip(chrono::TimeDelta::try_seconds(300).and_then(|d| now.checked_add_signed(d)));
        let Some((requested, before)) = window else {
            warn!("Fee batch transfer timestamps out of range; no on-chain fee transfers built");
            return Vec::new();
        };
        let requested_at = requested.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string();
        let execute_before = before.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        order
            .into_iter()
            .map(|receiver| {
                let targets = by_receiver.remove(&receiver).unwrap_or_default();
                McBatchTransfer {
                    transfer_factory_cid: String::new(), // server resolves from ext_rules
                    expected_admin: String::new(),        // server resolves from dso_party
                    instrument_admin: String::new(),      // server resolves from dso_party
                    instrument_id: "Amulet".to_string(),  // CC on-chain name
                    requested_at: requested_at.clone(),
                    execute_before: execute_before.clone(),
                    extra_args_json: None, // server fills per-receiver
                    targets,
                }
            })
            .collect()
    }

    fn build_multicall_params(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        batch_transfers: Vec<McBatchTransfer>,
        holding_cids: &[String],
    ) -> ExecuteMultiCallParams {
        let mut operations: Vec<MultiCallOp> = batch_transfers
            .into_iter()
            .map(|bt| MultiCallOp {
                op: Some(Op::BatchTransfer(bt)),
            })
            .collect();
        operations.push(MultiCallOp {
            op: Some(Op::AcceptDvpAndAllocate(McAcceptDvpAndAllocate {
                proposal_id: proposal_id.to_string(),
                dvp_proposal_cid: dvp_proposal_cid.to_string(),
            })),
        });
        ExecuteMultiCallParams {
            operations,
            holding_cids: holding_cids.to_vec(),
        }
    }

    /// Wait for the fee debits started so far, up to the bound of one full run;
    /// returns how many are still running.
    pub async fn wait_for_fee_debits(&self) -> usize {
        self.fee_debits.wait(self.fee_debit_wait()).await
    }

    /// The longest [`Self::wait_for_fee_debits`] waits.
    pub fn fee_debit_wait(&self) -> Duration {
        fee_debit_wait(&self.config)
    }
}

#[cfg(test)]
mod settle_tests {
    use super::*;
    use crate::holdings_cache::CachedAmulet;
    use crate::ledger_client::{ATOMIC_EXECUTE_AMBIGUOUS, EXECUTE_OUTCOME_UNKNOWN};
    use crate::test_util::refused_url;
    use anyhow::anyhow;

    const AMULET: &str = "00amulet";

    fn settler(url: &str) -> MulticallSettler {
        MulticallSettler::for_tests(url)
    }

    async fn with_amulet(url: &str) -> MulticallSettler {
        let s = settler(url);
        s.amulet_cache
            .add_created_amulets(vec![CachedAmulet {
                contract_id: AMULET.to_string(),
                amount: Decimal::from(100),
                discovered_at: std::time::Instant::now(),
            }])
            .await;
        s
    }

    async fn amulet_state(cache: &CcView) -> &'static str {
        match cache.stats().await {
            (_, 0, 0, _) => "free",
            (_, 0, 1, _) => "reserved",
            (_, 1, 0, _) => "consumed",
            other => panic!("unexpected cache state {other:?}"),
        }
    }

    async fn reserve(s: &MulticallSettler) -> ReservationGuard {
        assert!(s.amulet_cache.reserve(&[AMULET.to_string()], "settle-p1").await);
        ReservationGuard::new(Arc::clone(s.amulet_cache.inner()), vec![AMULET.to_string()])
    }

    fn prepared() -> Result<Prepared> {
        let channel = tonic::transport::Endpoint::from_shared(refused_url()).unwrap().connect_lazy();
        let key = agent_logic::secret::Secret::seal(&mut [7u8; 32]).unwrap();
        let client = DAppProviderClient::from_channel(channel, "p", "agent", &key, 60, None, &[0u8; 32]).unwrap();
        Ok(Prepared {
            client,
            holding_cids: vec![AMULET.to_string()],
            request: PrepareTransactionRequest::default(),
            expectation: OperationExpectation::ExecuteMulticall { party: "p".into(), op_count: 1 },
        })
    }

    fn committed() -> ExecuteTransactionResponse {
        ExecuteTransactionResponse { success: true, update_id: "u1".into(), ..Default::default() }
    }

    async fn run<F>(s: &MulticallSettler, wait: Duration, execute: F) -> Result<StepResult>
    where
        F: Future<Output = Result<ExecuteTransactionResponse>> + Send + 'static,
    {
        let reserved = reserve(s).await;
        s.execute_reserved(reserved, prepared(), "p1", wait, |_, _, _, _| execute).await
    }

    async fn panics() -> Result<ExecuteTransactionResponse> {
        panic!("execute panicked")
    }

    fn unknown(result: &Result<StepResult>) -> Option<&SettleOutcomeUnknown> {
        result.as_ref().err().and_then(|e| e.downcast_ref::<SettleOutcomeUnknown>())
    }

    // An ambiguous execute used to release its inputs and read as a plain failure, so the round was re-quoted
    #[tokio::test]
    async fn an_ambiguous_execute_is_outcome_unknown_and_keeps_its_reservations() {
        for text in [
            format!("{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete"),
            format!("{ATOMIC_EXECUTE_AMBIGUOUS}: transport"),
            "Command already submitted (DUPLICATE_COMMAND): dup".to_string(),
        ] {
            let s = with_amulet(&refused_url()).await;
            let result = run(&s, Duration::from_secs(5), async move { Err(anyhow!(text)) }).await;
            assert!(unknown(&result).is_some(), "{result:?}");
            assert_eq!(amulet_state(&s.amulet_cache).await, "reserved");
            assert_eq!(s.fee_debits.started(), 0);
        }
    }

    #[tokio::test]
    async fn an_execute_past_its_wait_is_unknown_and_still_finishes_its_bookkeeping() {
        let s = with_amulet(&refused_url()).await;
        let (go, gate) = tokio::sync::oneshot::channel::<()>();
        let execute = async move {
            let _ = gate.await;
            Ok(committed())
        };
        let result = tokio::time::timeout(Duration::from_secs(5), run(&s, Duration::from_millis(50), execute))
            .await
            .expect("the execute wait bounds the call");
        assert!(unknown(&result).is_some_and(|u| u.0.contains("still running")), "{result:?}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "reserved");
        go.send(()).unwrap();
        for _ in 0..100 {
            if amulet_state(&s.amulet_cache).await == "consumed" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the execute task was not left to finish");
    }

    #[tokio::test]
    async fn a_panicking_execute_is_outcome_unknown() {
        let s = with_amulet(&refused_url()).await;
        let result = run(&s, Duration::from_secs(5), panics()).await;
        assert!(unknown(&result).is_some(), "{result:?}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "reserved");
    }

    #[tokio::test]
    async fn a_definite_failure_releases_its_reservations() {
        let s = with_amulet(&refused_url()).await;
        let result = run(&s, Duration::from_secs(5), async { Err(anyhow!("Transaction failed: rejected")) }).await;
        assert!(result.is_err() && unknown(&result).is_none(), "{result:?}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "free");
    }

    // The fee debits used to run inline after the commit, and a failed connect turned a fill into an error
    #[tokio::test]
    async fn a_commit_is_reported_even_when_the_fee_debits_cannot_connect() {
        let s = with_amulet(&refused_url()).await;
        let step = run(&s, Duration::from_secs(5), async { Ok(committed()) }).await.unwrap();
        assert_eq!(step.update_id, "u1");
        assert_eq!(amulet_state(&s.amulet_cache).await, "consumed");
        assert_eq!(sync::lock(&s.fee_debits.0).len(), 1, "the debits run in the background");
        assert_eq!(s.wait_for_fee_debits().await, 0);
    }

    // A dry run used to mark its inputs consumed and pay the processing fees
    #[tokio::test]
    async fn a_dry_run_releases_its_inputs_and_pays_no_fees() {
        let s = with_amulet(&refused_url()).await;
        let dry = ExecuteTransactionResponse { success: false, ..Default::default() };
        run(&s, Duration::from_secs(5), async move { Ok(dry) }).await.unwrap();
        assert_eq!(amulet_state(&s.amulet_cache).await, "free");
        assert!(sync::lock(&s.fee_debits.0).is_empty());
    }

    #[tokio::test]
    async fn a_failed_prepare_releases_and_never_executes() {
        let s = with_amulet(&refused_url()).await;
        let reserved = reserve(&s).await;
        let result = s
            .execute_reserved(reserved, Err(anyhow!("prepare failed")), "p1", Duration::from_secs(5), |_, _, _, _| {
                async { panic!("must not execute") }
            })
            .await;
        assert_eq!(result.unwrap_err().to_string(), "prepare failed");
        assert_eq!(amulet_state(&s.amulet_cache).await, "free");
    }

    async fn settle(s: &MulticallSettler, instrument: &str, allocation: Decimal) -> Result<StepResult> {
        s.accept_and_settle("p1", "00dvp", "0.1", "0.1", "0.2", "0.2", instrument, Some(allocation)).await
    }

    // A failed connect before the CIP-56 lookup used to return without releasing the amulets
    #[tokio::test]
    async fn a_pre_execute_failure_releases_the_reservations() {
        for instrument in ["USDC", "CC"] {
            let s = with_amulet(&refused_url()).await;
            let result = settle(&s, instrument, Decimal::ONE).await;
            assert!(result.is_err() && unknown(&result).is_none(), "{instrument}: {result:?}");
            assert_eq!(amulet_state(&s.amulet_cache).await, "free", "{instrument}");
        }
    }

    #[tokio::test]
    async fn a_silent_ledger_before_the_execute_is_bounded_and_releases() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = with_amulet(&format!("http://{}", silent.local_addr().unwrap())).await;
        let result = tokio::time::timeout(Duration::from_secs(30), settle(&s, "USDC", Decimal::ONE))
            .await
            .expect("every pre-execute call is bounded");
        assert!(result.is_err() && unknown(&result).is_none(), "{result:?}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "free");
        drop(silent);
    }

    #[tokio::test]
    async fn an_out_of_range_allocation_is_refused_before_reserving() {
        let s = with_amulet(&refused_url()).await;
        let err = settle(&s, "CC", Decimal::MAX).await.unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "free");
    }

    // The debits used to be dropped at 60s, cutting one mid-call and never sending the second
    #[tokio::test]
    async fn slow_fee_debits_run_to_their_end() {
        let ledger = crate::test_util::FakeLedger::start(crate::test_util::Fake::Slow(Duration::from_millis(300))).await;
        let s = settler(&ledger.url);
        let debits = async { pay_processing_fees_with(create_client(&s.config).await, "p1").await };
        let slow = tokio::time::timeout(Duration::from_secs(20), run_fee_debits(debits, "p1", Duration::from_millis(200)))
            .await
            .expect("each debit call is bounded");
        assert!(slow, "the slow path was taken");
        assert_eq!(ledger.calls("PreparePayFee"), 2, "the allocate debit still ran");
        ledger.stop().await;
    }

    /// A client whose every debit outlasts the slow mark; counts the debits that finished.
    struct SlowDebits(Arc<std::sync::atomic::AtomicUsize>);

    impl FeeDebitClient for SlowDebits {
        async fn pay_processing_fee(&mut self, _: &str, _: &str) -> Result<()> {
            tokio::time::sleep(FEE_DEBIT_SLOW_AFTER + Duration::from_secs(30)).await;
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    // The same through debit_fees: debits past the 60s slow mark are not cut
    #[tokio::test(start_paused = true)]
    async fn debit_fees_lets_slow_debits_finish() {
        let mut s = settler(&refused_url());
        s.config.request_timeout_secs = 600;
        let paid = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = SlowDebits(Arc::clone(&paid));
        s.debit_fees_with("p1", move |_| async move { Ok(client) });
        assert_eq!(s.wait_for_fee_debits().await, 0, "both debits ended within the exit wait");
        assert_eq!(paid.load(std::sync::atomic::Ordering::SeqCst), 2, "the allocate debit still ran");
    }

    // A commit proven only by reconciliation used to leave both processing fees unpaid
    #[tokio::test]
    async fn a_reconciled_commit_can_start_its_fee_debits() {
        let s = settler(&refused_url());
        s.debit_fees("p1");
        assert_eq!(s.fee_debits.started(), 1);
        assert_eq!(s.wait_for_fee_debits().await, 0);
    }

    #[test]
    fn the_exit_wait_covers_a_connect_and_four_calls() {
        let s = settler(&refused_url());
        let call = ledger_client::call_wait(s.config.request_timeout_secs);
        let connect = ledger_client::connect_wait(s.config.connection_timeout_secs, s.config.request_timeout_secs);
        assert!(fee_debit_wait(&s.config) >= connect + call * 4);
        let mut huge = s.config.clone();
        huge.request_timeout_secs = u64::MAX;
        assert_eq!(fee_debit_wait(&huge), Duration::MAX, "saturates instead of panicking");
    }

    // A failure after an earlier execute that may still commit used to release the inputs
    #[tokio::test]
    async fn a_failure_after_an_uncertain_execute_keeps_its_reservations() {
        let s = with_amulet(&refused_url()).await;
        let later = anyhow!("Transaction failed: INACTIVE_CONTRACTS");
        let marked = ledger_client::mark_uncertain(later, Some("cmd-1"));
        let result = run(&s, Duration::from_secs(5), async move { Err(marked) }).await;
        assert!(unknown(&result).is_some_and(|u| u.0.contains("cmd-1")), "{result:?}");
        assert_eq!(amulet_state(&s.amulet_cache).await, "reserved");
        assert_eq!(s.fee_debits.started(), 0);
    }

    #[tokio::test]
    async fn running_counts_only_unfinished_debits() {
        let debits = FeeDebits::default();
        let done = tokio::spawn(async {});
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an empty task finishes");
        sync::lock(&debits.0).push(done);
        assert_eq!(debits.running(), 0);
        sync::lock(&debits.0).push(tokio::spawn(std::future::pending::<()>()));
        assert_eq!(debits.running(), 1);
        assert_eq!(debits.started(), 2);
    }

    #[tokio::test]
    async fn waiting_for_fee_debits_is_bounded() {
        let debits = FeeDebits::default();
        debits.push(tokio::spawn(std::future::pending::<()>()));
        debits.push(tokio::spawn(async {}));
        let waited = tokio::time::timeout(Duration::from_secs(2), debits.wait(Duration::from_millis(50))).await;
        assert_eq!(waited.expect("the wait is bounded"), 1);
    }

    #[test]
    fn no_fee_targets_build_no_batches() {
        let s = settler(&refused_url());
        assert!(s.build_batch_transfers(&[]).is_empty());
    }
}
