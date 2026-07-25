//! Merge worker — periodically consolidates sub-rung "dust" holdings via a
//! self-transfer (sender == receiver == the LP), per instrument.
//!
//! For each configured instrument, when the number of *dust* holdings (below the
//! instrument's smallest ladder rung) exceeds `MERGE_THRESHOLD`, the worker picks
//! the smallest such holdings (up to `MERGE_MAX_AMULETS`), reserves them, and
//! folds them into ONE output:
//! - **CC** → the v1 `SplitCc` self-consolidation.
//! - **utility / CIP-56** → a `TransferCip56` self-transfer (receiver == sender)
//!   with explicit `input_holding_cids`; the DA Utility registry classifies a
//!   same-owner multi-input transfer as `TxKind_MergeSplit`, and with
//!   `amount == Σ inputs` it yields exactly one output holding, no change. This
//!   uses the cheap TransferFactory path (NOT `AtomicDVPService_SplitHoldings`).
//!
//! Ladder rungs (`>= floor`) are never touched, so the merge worker never fights
//! the split worker.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest,
    SplitCcParams, TransferCip56Params, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use crate::holdings_cache::{CachedHolding, HoldingsCache};
use crate::ledger_client::DAppProviderClient;
use crate::split_worker::SplitInstrument;

/// Spawn the merge worker background task for the given instruments.
/// Only call this if `config.merge_threshold` is Some.
pub fn spawn_merge_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    instruments: Vec<SplitInstrument>,
    shutdown: Shutdown,
) {
    let threshold = config.merge_threshold.unwrap_or(200);
    let max_amulets = config.merge_max_amulets;
    let interval = Duration::from_secs(config.merge_poll_interval_sec);

    tokio::spawn(async move {
        let keys: Vec<&str> = instruments.iter().map(|i| i.key.as_str()).collect();
        info!(
            "Merge worker started (threshold={}, max_amulets={}, poll={}s, instruments={:?})",
            threshold, max_amulets, interval.as_secs(), keys
        );

        // Initial delay to let ACS cache populate (cancellable)
        if shutdown.sleep(Duration::from_secs(30)).await {
            info!("Merge worker shutting down");
            return;
        }

        loop {
            if shutdown.is_shutting_down() {
                info!("Merge worker shutting down");
                return;
            }

            // One client per cycle, created lazily on the first instrument that
            // actually merges (most cycles merge nothing).
            let mut client: Option<DAppProviderClient> = None;
            for inst in &instruments {
                if shutdown.is_shutting_down() {
                    break;
                }
                match check_and_merge_instrument(
                    &config, &cache, inst, threshold, max_amulets, &mut client,
                ).await {
                    Ok(Some(msg)) => info!("{}", msg),
                    Ok(None) => debug!("Merge {}: dust count below threshold", inst.key),
                    Err(e) => warn!("Merge {} failed: {:#}", inst.key, e),
                }
            }

            if shutdown.sleep(interval).await {
                info!("Merge worker shutting down");
                return;
            }
        }
    });
}

/// Decide which holdings to consolidate this cycle for one instrument.
///
/// `floor` is the instrument's dust-merge threshold (smallest ladder rung). When
/// present, only sub-`floor` **dust** is eligible — the `>= floor` holdings are
/// the ladder rungs the split worker maintains, and merging them is the
/// split/merge oscillation this guards against. The merge fires only when the
/// dust count exceeds `threshold` (independent of how many rungs exist), then
/// takes the `max_amulets` smallest dust holdings. With no ladder configured
/// (`floor = None`) it falls back to legacy total-count behavior.
///
/// Returns the holdings to merge (already ascending by amount); empty = no-op.
fn plan_merge(
    selectable: Vec<CachedHolding>,
    floor: Option<Decimal>,
    threshold: usize,
    max_amulets: usize,
) -> Vec<CachedHolding> {
    let dust: Vec<CachedHolding> = match floor {
        Some(f) => selectable.into_iter().filter(|a| a.amount < f).collect(),
        None => selectable,
    };
    if dust.len() <= threshold {
        return Vec::new();
    }
    dust.into_iter().take(max_amulets).collect()
}

async fn check_and_merge_instrument(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    inst: &SplitInstrument,
    threshold: usize,
    max_amulets: usize,
    client: &mut Option<DAppProviderClient>,
) -> anyhow::Result<Option<String>> {
    // Selectable holdings for this instrument, ascending by amount, splitter
    // reserve excluded (same set the split worker leaves for consolidation).
    let selectable = cache.get_selectable(&inst.key, true).await;
    let floor = cache.dust_threshold(&inst.key).await;
    let dust_count = match floor {
        Some(f) => selectable.iter().filter(|a| a.amount < f).count(),
        None => selectable.len(),
    };

    let to_merge = plan_merge(selectable, floor, threshold, max_amulets);
    let merge_count = to_merge.len();
    if merge_count < 2 {
        // Only ladder rungs above threshold (or a single dust holding): a 1-input
        // "merge" would be a pointless tx — leave the rungs for the split worker.
        return Ok(None);
    }
    let total_amount: Decimal = to_merge.iter().map(|a| a.amount).sum();
    let cids: Vec<String> = to_merge.iter().map(|a| a.contract_id.clone()).collect();

    match floor {
        Some(f) => info!(
            "Dust sweep {}: {} dust holdings above threshold ({}), merging {} smallest below {} ({} total)",
            inst.key, dust_count, threshold, merge_count, f, total_amount
        ),
        None => info!(
            "Merge {}: {} holdings above threshold ({}), merging {} smallest ({} total)",
            inst.key, dust_count, threshold, merge_count, total_amount
        ),
    }

    // Reserve the inputs so no other selection (split worker, settlement) grabs
    // them mid-flight. All-or-nothing.
    let job_id = format!("merge-{}-{}", inst.key, now_millis());
    if !cache.reserve_split(&cids, &job_id).await {
        return Err(anyhow::anyhow!(
            "Failed to reserve {} holdings for merge of {}", merge_count, inst.key
        ));
    }

    // Lazily create (and reuse) the ledger client for this cycle.
    if client.is_none() {
        match DAppProviderClient::new(
            &config.orderbook_grpc_url,
            &config.party_id,
            &config.role,
            &config.private_key_bytes,
            config.token_ttl_secs,
            Some(config.node_name.as_str()),
            &config.ledger_service_public_key,
            Some(config.connection_timeout_secs),
            Some(config.request_timeout_secs),
        )
        .await
        {
            Ok(c) => *client = Some(c),
            Err(e) => {
                cache.release_reservations(&cids).await;
                return Err(e);
            }
        }
    }
    let client = client.as_mut().expect("client initialized above");

    // Build the self-consolidation op: CC via SplitCc, utility via a
    // self-transfer (receiver == sender) with amount == Σ inputs → one output.
    let amount_str = total_amount.to_string();
    let (request, expectation) = if inst.is_cc {
        (
            PrepareTransactionRequest {
                operation: TransactionOperation::SplitCc as i32,
                params: Some(Params::SplitCc(SplitCcParams {
                    output_amounts: vec![amount_str.clone()],
                    amulet_cids: cids.clone(),
                })),
                request_signature: None,
            },
            OperationExpectation::SplitCc {
                party: config.party_id.clone(),
                output_amounts: vec![amount_str.clone()],
            },
        )
    } else {
        (
            PrepareTransactionRequest {
                operation: TransactionOperation::TransferCip56 as i32,
                params: Some(Params::TransferCip56(TransferCip56Params {
                    instrument_id: inst.on_chain_id.clone(),
                    instrument_admin: inst.admin.clone(),
                    receiver_party: config.party_id.clone(), // self-transfer = merge
                    amount: amount_str.clone(),
                    reference: None,
                    input_holding_cids: cids.clone(),
                })),
                request_signature: None,
            },
            OperationExpectation::TransferCip56 {
                sender_party: config.party_id.clone(),
                receiver_party: config.party_id.clone(),
                instrument_id: inst.on_chain_id.clone(),
                instrument_admin: inst.admin.clone(),
                amount: amount_str.clone(),
            },
        )
    };

    let result = client
        .submit_transaction(request, &expectation, false, false, false)
        .await;

    match result {
        Ok(ref resp) => {
            // Consume the inputs; the single merged output re-enters via the ACS
            // refresh / updates watcher (same as the split worker — the response
            // carries no amounts for the atomic-free path).
            cache.mark_consumed(&cids, &resp.update_id).await;
            cache.record_pending_split(&inst.key, &[(total_amount, 1)]).await;
            Ok(Some(format!(
                "Merged {} {} holdings ({} total) → update_id={}",
                merge_count, inst.key, total_amount, resp.update_id
            )))
        }
        Err(e) => {
            if e.to_string().contains("INACTIVE_CONTRACTS") {
                cache.mark_consumed(&cids, "inactive").await;
            } else {
                cache.release_reservations(&cids).await;
            }
            Err(e)
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holdings_cache::InstrumentKey;
    use std::time::Instant;

    fn holdings(amounts: &[&str]) -> Vec<CachedHolding> {
        amounts
            .iter()
            .enumerate()
            .map(|(i, a)| CachedHolding {
                contract_id: format!("cid-{i}"),
                template_id: "T".to_string(),
                instrument: InstrumentKey::from("USDC"),
                amount: a.parse().unwrap(),
                created_event_blob: None,
                synchronizer_id: "sync".to_string(),
                discovered_at: Instant::now(),
            })
            .collect()
    }

    fn rungs(n: usize) -> Vec<String> {
        (0..n).map(|_| "150".to_string()).collect() // exactly on the rung floor (not dust)
    }
    fn dust(n: usize) -> Vec<String> {
        (0..n).map(|_| "50".to_string()).collect() // below the 150 floor
    }
    fn floor() -> Option<Decimal> {
        Some("150".parse().unwrap())
    }

    // Core regression: ladder rungs (>= floor) are never counted or merged, no
    // matter how many there are — the merge worker must not fight the split worker.
    #[test]
    fn rungs_never_merged() {
        let all: Vec<String> = rungs(200);
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert!(plan_merge(holdings(&refs), floor(), 100, 10).is_empty());
    }

    // Dust at/below the threshold ⇒ no-op ("merge only when dust > 100").
    #[test]
    fn dust_below_threshold_is_noop() {
        let mut all: Vec<String> = rungs(100);
        all.extend(dust(12));
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert!(plan_merge(holdings(&refs), floor(), 100, 10).is_empty());
    }

    // Dust above the threshold ⇒ sweep only dust, up to max_amulets, rungs untouched.
    #[test]
    fn dust_above_threshold_sweeps_dust_only() {
        let mut all: Vec<String> = rungs(100);
        all.extend(dust(120));
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let picked = plan_merge(holdings(&refs), floor(), 100, 10);
        assert_eq!(picked.len(), 10);
        let f: Decimal = "150".parse().unwrap();
        assert!(picked.iter().all(|a| a.amount < f), "only sub-floor dust merged");
    }

    // No ladder configured ⇒ legacy total-count trigger + smallest-N selection.
    #[test]
    fn no_ladder_falls_back_to_legacy() {
        let all: Vec<String> = (0..150).map(|i| (i + 1).to_string()).collect(); // 1..=150
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(plan_merge(holdings(&refs), None, 100, 10).len(), 10);

        // Below threshold with no ladder ⇒ no-op.
        let few: Vec<String> = (0..80).map(|i| (i + 1).to_string()).collect();
        let refs: Vec<&str> = few.iter().map(|s| s.as_str()).collect();
        assert!(plan_merge(holdings(&refs), None, 100, 10).is_empty());
    }
}
