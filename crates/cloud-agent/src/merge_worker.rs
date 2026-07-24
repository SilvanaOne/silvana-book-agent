//! Merge worker — periodically consolidates small amulets via self-transfer
//!
//! When the number of selectable amulets exceeds MERGE_THRESHOLD, the worker
//! picks the smallest amulets (up to MERGE_MAX_AMULETS), reserves them in the
//! cache, and executes a TransferCc to self (sender == receiver). The ledger
//! consolidates the inputs into fewer output amulets.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest,
    SplitCcParams, TransactionOperation,
};
use tx_verifier::OperationExpectation;

use crate::holdings_cache::{CachedAmulet, CcView};
use crate::ledger_client::DAppProviderClient;
use crate::payment_queue::{process_tx_result, handle_inactive_contracts};

/// Spawn the merge worker background task.
/// Only call this if `config.merge_threshold` is Some.
pub fn spawn_merge_worker(
    config: BaseConfig,
    cache: CcView,
    shutdown: Shutdown,
) {
    let threshold = config.merge_threshold.unwrap_or(200);
    let max_amulets = config.merge_max_amulets;
    let interval = Duration::from_secs(config.merge_poll_interval_sec);

    tokio::spawn(async move {
        info!(
            "Merge worker started (threshold={}, max_amulets={}, poll={}s)",
            threshold, max_amulets, interval.as_secs()
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

            match check_and_merge(&config, &cache, threshold, max_amulets).await {
                Ok(Some(msg)) => info!("Merge: {}", msg),
                Ok(None) => debug!("Merge: amulet count below threshold"),
                Err(e) => warn!("Merge failed: {:#}", e),
            }

            if shutdown.sleep(interval).await {
                info!("Merge worker shutting down");
                return;
            }
        }
    });
}

/// Decide which CC amulets to consolidate this cycle.
///
/// `floor` is the CC dust-merge threshold (smallest ladder rung). When present,
/// only sub-`floor` **dust** is eligible — the `>= floor` holdings are the ladder
/// rungs the split worker maintains, and merging them is the split/merge
/// oscillation this guards against. The merge fires only when the dust count
/// exceeds `threshold` (independent of how many rungs exist), then takes the
/// `max_amulets` smallest dust amulets. With no ladder configured (`floor =
/// None`) it falls back to the legacy total-count behavior.
///
/// Returns the amulets to merge (already ascending by amount); empty = no-op.
fn plan_merge(
    selectable: Vec<CachedAmulet>,
    floor: Option<Decimal>,
    threshold: usize,
    max_amulets: usize,
) -> Vec<CachedAmulet> {
    let dust: Vec<CachedAmulet> = match floor {
        Some(f) => selectable.into_iter().filter(|a| a.amount < f).collect(),
        None => selectable,
    };
    if dust.len() <= threshold {
        return Vec::new();
    }
    dust.into_iter().take(max_amulets).collect()
}

async fn check_and_merge(
    config: &BaseConfig,
    cache: &CcView,
    threshold: usize,
    max_amulets: usize,
) -> anyhow::Result<Option<String>> {
    let selectable = cache.get_selectable_amulets().await; // asc by amount, reserve excluded
    let floor = cache.dust_threshold().await; // smallest CC ladder rung, from shared ladder config
    let dust_count = match floor {
        Some(f) => selectable.iter().filter(|a| a.amount < f).count(),
        None => selectable.len(),
    };

    let to_merge = plan_merge(selectable, floor, threshold, max_amulets);
    let merge_count = to_merge.len();
    if merge_count < 2 {
        // Only ladder rungs above threshold (or a single dust amulet): a 1-input
        // "merge" would be a pointless tx — leave the rungs for the split worker.
        return Ok(None);
    }
    let total_amount: Decimal = to_merge.iter().map(|a| a.amount).sum();
    let cids: Vec<String> = to_merge.iter().map(|a| a.contract_id.clone()).collect();

    match floor {
        Some(f) => info!(
            "Dust sweep: {} dust amulets above threshold ({}), merging {} smallest below {} CC ({:.4} CC total)",
            dust_count, threshold, merge_count, f, total_amount
        ),
        None => info!(
            "Merge: {} amulets above threshold ({}), merging {} smallest ({:.4} CC total)",
            dust_count, threshold, merge_count, total_amount
        ),
    }

    // Reserve in cache
    let payment_id = format!("merge-{}", now_millis());
    if !cache.reserve(&cids, &payment_id).await {
        return Err(anyhow::anyhow!("Failed to reserve {} amulets for merge", merge_count));
    }

    // Create client
    let mut client = match DAppProviderClient::new(
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
        Ok(c) => c,
        Err(e) => {
            cache.release_reservations(&cids).await;
            return Err(e);
        }
    };

    let output_amounts = vec![total_amount.to_string()];

    let expectation = OperationExpectation::SplitCc {
        party: config.party_id.clone(),
        output_amounts: output_amounts.clone(),
    };

    let result = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::SplitCc as i32,
                params: Some(Params::SplitCc(SplitCcParams {
                    output_amounts,
                    amulet_cids: cids.clone(),
                })),
                request_signature: None,
            },
            &expectation,
            false, // verbose
            false, // dry_run
            false, // force
        )
        .await;

    match result {
        Ok(ref resp) => {
            process_tx_result(cache, &cids, resp).await;
            Ok(Some(format!(
                "merged {} amulets ({:.4} CC) → update_id={}",
                merge_count, total_amount, resp.update_id
            )))
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("INACTIVE_CONTRACTS") {
                handle_inactive_contracts(cache, &cids).await;
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
    use std::time::Instant;

    fn amulets(amounts: &[&str]) -> Vec<CachedAmulet> {
        amounts
            .iter()
            .enumerate()
            .map(|(i, a)| CachedAmulet {
                contract_id: format!("cid-{i}"),
                amount: a.parse().unwrap(),
                discovered_at: Instant::now(),
            })
            .collect()
    }

    fn cc(n: usize) -> Vec<String> {
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
        let rungs: Vec<String> = cc(200);
        let refs: Vec<&str> = rungs.iter().map(|s| s.as_str()).collect();
        let picked = plan_merge(amulets(&refs), floor(), 100, 10);
        assert!(picked.is_empty());
    }

    // Dust at/below the threshold ⇒ no-op ("merge only when dust > 100").
    #[test]
    fn dust_below_threshold_is_noop() {
        let mut all: Vec<String> = cc(100);
        all.extend(dust(12));
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let picked = plan_merge(amulets(&refs), floor(), 100, 10);
        assert!(picked.is_empty());
    }

    // Dust above the threshold ⇒ sweep only dust, up to max_amulets, rungs untouched.
    #[test]
    fn dust_above_threshold_sweeps_dust_only() {
        let mut all: Vec<String> = cc(100);
        all.extend(dust(120));
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let picked = plan_merge(amulets(&refs), floor(), 100, 10);
        assert_eq!(picked.len(), 10);
        let f: Decimal = "150".parse().unwrap();
        assert!(picked.iter().all(|a| a.amount < f), "only sub-floor dust merged");
    }

    // No ladder configured ⇒ legacy total-count trigger + smallest-N selection.
    #[test]
    fn no_ladder_falls_back_to_legacy() {
        let all: Vec<String> = (0..150).map(|i| (i + 1).to_string()).collect(); // 1..=150
        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let picked = plan_merge(amulets(&refs), None, 100, 10);
        assert_eq!(picked.len(), 10);
        // 150 amulets > threshold 100; nothing filtered, first 10 taken.
        assert_eq!(picked.len(), 10);

        // Below threshold with no ladder ⇒ no-op.
        let few: Vec<String> = (0..80).map(|i| (i + 1).to_string()).collect();
        let refs: Vec<&str> = few.iter().map(|s| s.as_str()).collect();
        assert!(plan_merge(amulets(&refs), None, 100, 10).is_empty());
    }
}
