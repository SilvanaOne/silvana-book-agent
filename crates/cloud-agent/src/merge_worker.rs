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

use crate::holdings_cache::CcView;
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

async fn check_and_merge(
    config: &BaseConfig,
    cache: &CcView,
    threshold: usize,
    max_amulets: usize,
) -> anyhow::Result<Option<String>> {
    let selectable = cache.get_selectable_amulets().await;
    let count = selectable.len();

    if count <= threshold {
        return Ok(None);
    }

    // Take smallest amulets (already sorted ascending by amount)
    let to_merge: Vec<_> = selectable.into_iter().take(max_amulets).collect();
    let merge_count = to_merge.len();
    let total_amount: Decimal = to_merge.iter().map(|a| a.amount).sum();
    let cids: Vec<String> = to_merge.iter().map(|a| a.contract_id.clone()).collect();

    info!(
        "Merge: {} amulets above threshold ({}), merging {} smallest ({:.4} CC total)",
        count, threshold, merge_count, total_amount
    );

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
