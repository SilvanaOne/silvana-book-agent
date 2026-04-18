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

use std::sync::Arc;

use anyhow::{Context, Result};
use rust_decimal::prelude::FromStr as _;
use rust_decimal::Decimal;
use tracing::{debug, info};

use agent_logic::config::BaseConfig;
use agent_logic::confirm::{confirm_transaction, ConfirmLock};
use agent_logic::fees::{taker_settlement_fees, FeeTarget};
use agent_logic::settlement::StepResult;
use orderbook_proto::ledger::{
    multi_call_op::Op, prepare_transaction_request::Params, ExecuteMultiCallParams,
    ExecuteTransactionResponse, McAcceptDvpAndAllocate, McBatchTransfer, McTransferTarget,
    MultiCallOp, PrepareTransactionRequest, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use crate::amulet_cache::AmuletCache;
use crate::ledger_client::DAppProviderClient;
use crate::payment_queue::{process_tx_result, select_amulets_for_allocation};

/// Options/flags for the multicall settlement (verbose, dry_run, force, confirm).
pub struct MulticallSettler {
    pub config: BaseConfig,
    pub amulet_cache: Arc<AmuletCache>,
    pub verbose: bool,
    pub dry_run: bool,
    pub force: bool,
    pub confirm: bool,
    pub confirm_lock: ConfirmLock,
}

/// CIP-56 Holding template uploaded on this participant. Matches the constant
/// exported by the SDK's `transactions` crate (`TEMPLATE_HOLDING`). Inlined
/// here because `orderbook-cloud-agent` doesn't depend on `transactions`
/// directly. If the SDK changes this template, update here too.
const TEMPLATE_HOLDING: &str =
    "#utility-registry-holding-v0:Utility.Registry.Holding.V0.Holding:Holding";

/// Fetch unlocked CIP-56 Holdings owned by `party_id`. Mirrors
/// `dvp/commands/multicall.rs::fetch_holdings` — filters by `owner == party_id`
/// and `lock is null`. Used to supply USDC (or any non-CC) allocation inputs
/// to the multicall's `holding_cids` pool.
async fn fetch_unlocked_cip56_holdings(
    client: &mut DAppProviderClient,
    party_id: &str,
) -> Result<Vec<String>> {
    let contracts = client
        .get_active_contracts(&[TEMPLATE_HOLDING.to_string()])
        .await?;
    Ok(contracts
        .into_iter()
        .filter(|c| {
            let Some(ref args) = c.create_arguments else { return false };
            let json = crate::prost_struct_to_json(args);
            let owner_ok =
                json.get("owner").and_then(|o| o.as_str()) == Some(party_id);
            let lock = json.pointer("/lock");
            let unlocked = lock.is_none() || lock.unwrap().is_null();
            owner_ok && unlocked
        })
        .map(|c| c.contract_id)
        .collect())
}

impl MulticallSettler {
    /// Accept the LP's DvpProposal + allocate our side + pay all fees + own traffic
    /// fee — in a single `Execute_MultiCall` transaction.
    ///
    /// `allocation_instrument_id` is the instrument the taker is allocating
    /// (e.g. "USDC" for a buy on CC-USDC, "CC" for a sell). When it's not CC,
    /// the settler fetches the party's CIP-56 Holdings and appends their cids
    /// to `holding_cids` alongside the CC amulets used for fee payments.
    pub async fn accept_and_settle(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        dvp_processing_fee_cc: &str,
        alloc_processing_fee_cc: &str,
        allocation_instrument_id: &str,
        allocation_cc: Option<Decimal>,
    ) -> Result<StepResult> {
        if self.confirm && !self.dry_run {
            confirm_transaction(
                &self.confirm_lock,
                "Accept+Settle (multicall)",
                &format!("proposal: {}, dvp_proposal: {}", proposal_id, dvp_proposal_cid),
            )
            .await?;
        }

        // 1. Build fee list (sans traffic)
        let fee_targets = taker_settlement_fees(
            &self.config,
            dvp_processing_fee_cc,
            alloc_processing_fee_cc,
        );
        let fee_total: Decimal = fee_targets
            .iter()
            .filter_map(|t| Decimal::from_str(&t.amount_cc).ok())
            .sum();

        // 2. Select amulets to cover fees + CC allocation + traffic estimate + margin.
        // For sell (CC allocation), amulets must also cover the trade quantity.
        // Traffic for a 3-op multicall is ~100-150KB; estimate conservatively.
        let alloc_cc = allocation_cc.unwrap_or(Decimal::ZERO);
        let traffic_estimate_bytes = 150_000u64;
        let traffic_usd = Decimal::from(traffic_estimate_bytes)
            * Decimal::from_f64_retain(self.config.traffic_fee_usd_per_byte)
                .unwrap_or(Decimal::ZERO);
        let est_cc_usd = Decimal::new(15, 2); // conservative CC/USD for estimation
        let traffic_cc_estimate = if est_cc_usd > Decimal::ZERO {
            (traffic_usd / est_cc_usd).round_dp(4)
        } else {
            Decimal::from(50)
        };
        let margin = Decimal::from(5);
        let estimated_cc = fee_total + alloc_cc + traffic_cc_estimate + margin;
        let selectable = self.amulet_cache.get_selectable_amulets().await;
        let selected = select_amulets_for_allocation(&selectable, estimated_cc);
        if selected.is_empty() {
            anyhow::bail!(
                "Insufficient amulets for multicall settlement: need ~{} CC (fees {} + alloc {} + traffic_est {} + margin {})",
                estimated_cc, fee_total, alloc_cc, traffic_cc_estimate, margin,
            );
        }
        let mut holding_cids: Vec<String> =
            selected.iter().map(|a| a.contract_id.clone()).collect();
        let payment_id = format!("settle-{}", proposal_id);
        if !self.amulet_cache.reserve(&holding_cids, &payment_id).await {
            anyhow::bail!("Failed to reserve amulets for multicall settlement");
        }

        // Non-CC allocation (e.g. USDC for a buy on CC-USDC): fetch the party's
        // CIP-56 Holdings for that instrument and add them to the multicall's
        // unified holding pool. Amulets cover the fee payments; CIP-56 holdings
        // cover the allocation input.
        if !allocation_instrument_id.eq_ignore_ascii_case("cc")
            && !allocation_instrument_id.eq_ignore_ascii_case("amulet")
        {
            let mut client = self.create_client().await?;
            let cip56 = fetch_unlocked_cip56_holdings(&mut client, &self.config.party_id).await?;
            if cip56.is_empty() {
                self.amulet_cache.release_reservations(&holding_cids).await;
                anyhow::bail!(
                    "Taker has no unlocked CIP-56 holdings to allocate {} for proposal {}",
                    allocation_instrument_id, proposal_id
                );
            }
            tracing::debug!(
                "Multicall: adding {} CIP-56 {} holdings to holding_cids pool",
                cip56.len(), allocation_instrument_id
            );
            holding_cids.extend(cip56);
        }

        // 3. Run the two-phase prepare-then-submit; release reservations on any error.
        let result = self
            .run_multicall(proposal_id, dvp_proposal_cid, &fee_targets, &holding_cids)
            .await;
        if result.is_err() {
            self.amulet_cache.release_reservations(&holding_cids).await;
        }
        let exec_result = result?;

        process_tx_result(&self.amulet_cache, &holding_cids, &exec_result).await;
        self.amulet_cache.release_reservations(&holding_cids).await;

        let traffic = exec_result.traffic.as_ref().map(|t| t.total_bytes).unwrap_or(0);
        let cid = exec_result.contract_id.clone().unwrap_or_default();
        info!(
            "Multicall submitted for {}: accept+allocate cid={} traffic={} bytes fees={:.4} CC",
            proposal_id, cid, traffic, fee_total,
        );

        Ok(StepResult {
            contract_id: cid,
            update_id: exec_result.update_id,
            traffic_total: traffic,
        })
    }

    /// Phase 1 (placeholder traffic → measure bytes) + Phase 2 (actual traffic → execute).
    async fn run_multicall(
        &self,
        proposal_id: &str,
        dvp_proposal_cid: &str,
        fee_targets: &[FeeTarget],
        holding_cids: &[String],
    ) -> Result<ExecuteTransactionResponse> {
        let mut client = self.create_client().await?;

        // Phase 1 — placeholder traffic fee, fixed-width description so the
        // second prepare produces an identically-sized payload.
        let placeholder_batches =
            self.build_batch_transfers(fee_targets, "0.000001", "Traffic fee for 100000 bytes");
        let prep = client
            .prepare_transaction(PrepareTransactionRequest {
                operation: TransactionOperation::ExecuteMulticall as i32,
                params: Some(Params::ExecuteMulticall(self.build_multicall_params(
                    proposal_id,
                    dvp_proposal_cid,
                    placeholder_batches,
                    holding_cids,
                ))),
                request_signature: None,
            })
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

        // Compute actual traffic-fee CC from bytes × rate
        let rates = client.get_dso_rates().await?;
        let cc_usd_rate = Decimal::from_str(&rates.cc_usd_rate).unwrap_or(Decimal::ZERO);
        let traffic_fee_cc = if cc_usd_rate > Decimal::ZERO && traffic_bytes > 0 {
            let fee_usd = Decimal::from(traffic_bytes)
                * Decimal::from_f64_retain(self.config.traffic_fee_usd_per_byte)
                    .unwrap_or(Decimal::ZERO);
            (fee_usd / cc_usd_rate).round_dp(10)
        } else {
            Decimal::ZERO
        };
        let traffic_fee_str = if traffic_fee_cc > Decimal::ZERO {
            traffic_fee_cc.to_string()
        } else {
            "0.000001".to_string()
        };
        let traffic_desc = format!("Traffic fee for {:06} bytes", traffic_bytes);

        // Phase 2 — execute with actual traffic fee
        let final_batches =
            self.build_batch_transfers(fee_targets, &traffic_fee_str, &traffic_desc);
        let op_count = final_batches.len() + 1; // + AcceptDvpAndAllocate
        let params_final =
            self.build_multicall_params(proposal_id, dvp_proposal_cid, final_batches, holding_cids);
        let expectation = OperationExpectation::ExecuteMulticall {
            party: self.config.party_id.clone(),
            op_count,
        };
        client
            .submit_transaction(
                PrepareTransactionRequest {
                    operation: TransactionOperation::ExecuteMulticall as i32,
                    params: Some(Params::ExecuteMulticall(params_final)),
                    request_signature: None,
                },
                &expectation,
                self.verbose,
                self.dry_run,
                self.force,
            )
            .await
    }

    /// Group fee + traffic targets by receiver into a list of `McBatchTransfer`
    /// ops (multicall-v1). Each BatchTransfer must have a single receiver, so
    /// we produce one op per unique receiver — typically two:
    ///   - `fee_party`         (DVP + allocation + agent + signature fees)
    ///   - `traffic_fee_party` (participant fees + the tx's own traffic fee)
    /// Server-side fallbacks fill `transfer_factory_cid`, `expected_admin`,
    /// `instrument_admin`, and `extra_args_json` when left empty/None
    /// (see `transactions::multicall::build_multicall_command`).
    fn build_batch_transfers(
        &self,
        fee_targets: &[FeeTarget],
        traffic_fee_cc: &str,
        traffic_desc: &str,
    ) -> Vec<McBatchTransfer> {
        // Flatten fee targets + the traffic-fee target into (receiver, amount, desc).
        let mut all: Vec<(String, String, String)> = fee_targets
            .iter()
            .map(|t| (t.receiver.clone(), t.amount_cc.clone(), t.description.clone()))
            .collect();
        all.push((
            self.config.traffic_fee_party.clone(),
            traffic_fee_cc.to_string(),
            traffic_desc.to_string(),
        ));

        // Stable group-by-receiver (first-seen order).
        let mut by_receiver: std::collections::HashMap<String, Vec<McTransferTarget>> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (receiver, amount, desc) in all {
            let entry = by_receiver.entry(receiver.clone());
            if matches!(entry, std::collections::hash_map::Entry::Vacant(_)) {
                order.push(receiver.clone());
            }
            entry.or_insert_with(Vec::new).push(McTransferTarget {
                receiver: receiver.clone(),
                amount,
                description: Some(desc),
            });
        }

        // Timestamps — match cc_transfer_builders.rs formatting.
        let now = chrono::Utc::now();
        let requested_at = (now - chrono::Duration::seconds(10))
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string();
        let execute_before = (now + chrono::Duration::seconds(300))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();

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

    async fn create_client(&self) -> Result<DAppProviderClient> {
        DAppProviderClient::new(
            &self.config.orderbook_grpc_url,
            &self.config.party_id,
            &self.config.role,
            &self.config.private_key_bytes,
            self.config.token_ttl_secs,
            Some(self.config.node_name.as_str()),
            &self.config.ledger_service_public_key,
            Some(self.config.connection_timeout_secs),
            Some(self.config.request_timeout_secs),
        )
        .await
    }
}
