//! DvpProposal garbage collector — archives expired legacy-DVP proposals.
//!
//! Legacy RFQ V1 leaves the on-chain DvpProposal active when a settlement is
//! abandoned (the server-side cancel is a DB-only status flip), so expired
//! proposals accumulate in the party's ACS by the tens of thousands and
//! dominate the GetSettlementContracts stream. Only the proposer can archive
//! its own proposals (`DvpProposal_Cancel`, controller proposer) and only the
//! counterparty can reject foreign ones (`DvpProposal_Reject`, controller
//! counterparty), so this worker runs inside the agent that holds the LP
//! party key — the settlement operator has no choice it can exercise.
//!
//! Safety: only proposals with `terms.settleBefore` at least
//! `DVP_GC_SAFETY_MARGIN_SECS` in the past are touched. Past `settleBefore`
//! the agent's own liveness gate has abandoned the settlement, and acceptance
//! was already impossible at `allocateBefore` (`DvpProposal_Accept` asserts
//! `assertWithinDeadline terms.allocateBefore`).
//!
//! Cost control: archival only proceeds while the predicted issuance
//! coefficient is above `DVP_GC_MIN_COEFFICIENT` (high coefficient = light
//! sequencer load = cheap window), one proposal per transaction with
//! `DVP_GC_DELAY_SECS` between submissions.

use std::sync::LazyLock;
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::prepare_transaction_request::Params;
use orderbook_proto::ledger::{
    CancelDvpProposalParams, PrepareTransactionRequest, RejectDvpProposalParams,
    TransactionOperation,
};
use tx_verifier::OperationExpectation;

use crate::ledger_client::DAppProviderClient;
use crate::prost_struct_to_json;

const TEMPLATE_DVP_PROPOSAL: &str =
    "#utility-settlement-app-v1:Utility.Settlement.App.V1.Model.Dvp:DvpProposal";

const REJECT_REASON: &str = "expired";

/// Abort the drain phase after this many consecutive submit failures — the
/// remaining queue is retried on the next refresh cycle.
const MAX_CONSECUTIVE_FAILURES: u32 = 20;

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

static GC_ENABLED: LazyLock<bool> = LazyLock::new(|| env_flag("DVP_GC_ENABLED", true));
static GC_MIN_COEFFICIENT: LazyLock<f64> =
    LazyLock::new(|| env_parse("DVP_GC_MIN_COEFFICIENT", 0.72));
static GC_DELAY_SECS: LazyLock<u64> = LazyLock::new(|| env_parse("DVP_GC_DELAY_SECS", 10));
static GC_REJECT_ENABLED: LazyLock<bool> =
    LazyLock::new(|| env_flag("DVP_GC_REJECT_ENABLED", true));
static GC_REFRESH_SECS: LazyLock<u64> = LazyLock::new(|| env_parse("DVP_GC_REFRESH_SECS", 3600));
static GC_SAFETY_MARGIN_SECS: LazyLock<u64> =
    LazyLock::new(|| env_parse("DVP_GC_SAFETY_MARGIN_SECS", 3600));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GcAction {
    Cancel,
    Reject,
}

struct GcItem {
    cid: String,
    action: GcAction,
    settle_before_micros: i64,
}

/// Classify one DvpProposal payload against the eligibility predicate.
/// Returns the archival action, or None when the proposal must be left alone
/// (still live, not ours to archive, or unparseable — unparseable is a skip,
/// never an archive).
fn classify_proposal(
    args: &Value,
    party_id: &str,
    now_micros: i64,
    margin_micros: i64,
    reject_enabled: bool,
) -> Option<(GcAction, i64)> {
    let settle_before = micros_value(args.pointer("/terms/settleBefore")?)?;
    if settle_before >= now_micros - margin_micros {
        return None; // still inside (or too close to) the settle window
    }

    let proposer = args.get("proposer").and_then(Value::as_str)?;
    if proposer == party_id {
        return Some((GcAction::Cancel, settle_before));
    }
    let counterparty = args.get("counterparty").and_then(Value::as_str)?;
    if counterparty == party_id && reject_enabled {
        return Some((GcAction::Reject, settle_before));
    }
    None
}

/// Daml timestamps arrive as micros-since-epoch, but the JSON encoding varies
/// by path (integer, float, or decimal string). Accept all three.
fn micros_value(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    v.as_str()?.trim().parse::<i64>().ok()
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Spawn the DvpProposal GC background task. Returns without spawning when
/// `DVP_GC_ENABLED=false`.
pub fn spawn_dvp_gc_worker(config: BaseConfig, shutdown: Shutdown) {
    if !*GC_ENABLED {
        info!("DvpProposal GC disabled (DVP_GC_ENABLED=false)");
        return;
    }
    tokio::spawn(async move {
        info!(
            "DvpProposal GC started: min_coefficient={:.2}, delay={}s, refresh={}s, \
             safety_margin={}s, reject_enabled={}",
            *GC_MIN_COEFFICIENT, *GC_DELAY_SECS, *GC_REFRESH_SECS,
            *GC_SAFETY_MARGIN_SECS, *GC_REJECT_ENABLED,
        );
        run(config, shutdown).await;
    });
}

async fn run(config: BaseConfig, shutdown: Shutdown) {
    let delay = Duration::from_secs((*GC_DELAY_SECS).max(1));

    loop {
        if shutdown.is_shutting_down() {
            info!("DvpProposal GC shutting down");
            return;
        }

        let mut client = match create_client(&config).await {
            Ok(c) => c,
            Err(e) => {
                warn!("DvpProposal GC: client create failed: {:#}", e);
                if shutdown.sleep(delay).await {
                    return;
                }
                continue;
            }
        };

        // --- Scan: full DvpProposal ACS for this party, classify each ---
        let (queue, scanned) = match scan(&mut client, &config).await {
            Ok(r) => r,
            Err(e) => {
                warn!("DvpProposal GC: ACS scan failed: {:#}", e);
                if shutdown.sleep(Duration::from_secs(*GC_REFRESH_SECS)).await {
                    return;
                }
                continue;
            }
        };

        let cancels = queue.iter().filter(|i| i.action == GcAction::Cancel).count();
        let rejects = queue.len() - cancels;
        info!(
            "DvpProposal GC cycle: scanned={}, eligible={} (cancel={}, reject={}), coefficient={:.4}",
            scanned, queue.len(), cancels, rejects,
            agent_logic::forecast::coefficient_value(),
        );

        // --- Drain: one archival tx per proposal, coefficient-gated, throttled ---
        let mut done: u64 = 0;
        let mut skipped_gone: u64 = 0;
        let mut consecutive_failures: u32 = 0;
        'drain: for item in &queue {
            // Wait (without popping) until the issuance coefficient is high
            // enough. 0.0 means "no forecast yet" and stays paused.
            loop {
                if shutdown.is_shutting_down() {
                    info!("DvpProposal GC shutting down");
                    return;
                }
                let coeff = agent_logic::forecast::coefficient_value();
                if coeff > *GC_MIN_COEFFICIENT {
                    break;
                }
                debug!(
                    "DvpProposal GC paused: coefficient {:.4} <= {:.2}",
                    coeff, *GC_MIN_COEFFICIENT
                );
                if shutdown.sleep(delay).await {
                    return;
                }
            }

            match archive_one(&mut client, &config, item).await {
                Ok(()) => {
                    done += 1;
                    consecutive_failures = 0;
                    info!(
                        "DvpProposal GC: {} {} ({}/{})",
                        if item.action == GcAction::Cancel { "cancelled" } else { "rejected" },
                        &item.cid[..item.cid.len().min(16)],
                        done, queue.len(),
                    );
                }
                Err(e) => {
                    let msg = format!("{:#}", e);
                    // Only the archived-contract error ids count as "already
                    // gone" — a bare NOT_FOUND substring would also swallow
                    // systemic USER_NOT_FOUND / PACKAGE_NOT_FOUND failures and
                    // defeat the consecutive-failure breaker. A skip leaves the
                    // failure counter untouched: neither success nor failure.
                    if msg.contains("CONTRACT_NOT_FOUND") || msg.contains("CONTRACT_NOT_ACTIVE") {
                        skipped_gone += 1;
                        debug!(
                            "DvpProposal GC: {} already gone: {}",
                            &item.cid[..item.cid.len().min(16)], msg,
                        );
                    } else {
                        consecutive_failures += 1;
                        warn!(
                            "DvpProposal GC: archive failed ({} consecutive): {}",
                            consecutive_failures, msg,
                        );
                        if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                            warn!(
                                "DvpProposal GC: {} consecutive failures — abandoning cycle, \
                                 will rescan in {}s",
                                consecutive_failures, *GC_REFRESH_SECS,
                            );
                            break 'drain;
                        }
                        // Recreate the client on failure — the error may be a
                        // dead connection rather than a bad proposal.
                        if let Ok(c) = create_client(&config).await {
                            client = c;
                        }
                    }
                }
            }

            if shutdown.sleep(delay).await {
                return;
            }
        }

        if !queue.is_empty() {
            info!(
                "DvpProposal GC cycle done: archived={}, already_gone={}, remaining={}",
                done, skipped_gone,
                queue.len() as u64 - done - skipped_gone,
            );
            // A cycle where everything was "already gone" but the scan still
            // returned it is only plausible when the prepare path is looking
            // at the wrong participant/user — surface it loudly.
            if done == 0 && skipped_gone > 1 {
                warn!(
                    "DvpProposal GC: entire cycle ({}) skipped as already-gone — \
                     verify prepare path / participant routing",
                    skipped_gone,
                );
            }
        }

        if shutdown.sleep(Duration::from_secs(*GC_REFRESH_SECS)).await {
            info!("DvpProposal GC shutting down");
            return;
        }
    }
}

async fn create_client(config: &BaseConfig) -> anyhow::Result<DAppProviderClient> {
    DAppProviderClient::new(
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
}

/// Fetch the party's active DvpProposals and classify them. Returns the
/// eligible queue (oldest settleBefore first) and the total scanned count.
async fn scan(
    client: &mut DAppProviderClient,
    config: &BaseConfig,
) -> anyhow::Result<(Vec<GcItem>, usize)> {
    let contracts = client
        .get_active_contracts(&[TEMPLATE_DVP_PROPOSAL.to_string()])
        .await?;
    let scanned = contracts.len();

    let now = now_micros();
    let margin = (*GC_SAFETY_MARGIN_SECS as i64).saturating_mul(1_000_000);

    let mut queue: Vec<GcItem> = contracts
        .into_iter()
        .filter_map(|c| {
            let args = prost_struct_to_json(c.create_arguments.as_ref()?);
            let (action, settle_before_micros) = classify_proposal(
                &args, &config.party_id, now, margin, *GC_REJECT_ENABLED,
            )?;
            Some(GcItem { cid: c.contract_id, action, settle_before_micros })
        })
        .collect();

    queue.sort_by_key(|i| i.settle_before_micros);
    Ok((queue, scanned))
}

async fn archive_one(
    client: &mut DAppProviderClient,
    config: &BaseConfig,
    item: &GcItem,
) -> anyhow::Result<()> {
    let (req, expectation) = match item.action {
        GcAction::Cancel => (
            PrepareTransactionRequest {
                operation: TransactionOperation::CancelDvpProposal as i32,
                params: Some(Params::CancelDvpProposal(CancelDvpProposalParams {
                    dvp_proposal_cid: item.cid.clone(),
                })),
                request_signature: None,
            },
            OperationExpectation::CancelDvpProposal {
                party: config.party_id.clone(),
                dvp_proposal_cid: item.cid.clone(),
            },
        ),
        GcAction::Reject => (
            PrepareTransactionRequest {
                operation: TransactionOperation::RejectDvpProposal as i32,
                params: Some(Params::RejectDvpProposal(RejectDvpProposalParams {
                    dvp_proposal_cid: item.cid.clone(),
                    reason: REJECT_REASON.to_string(),
                })),
                request_signature: None,
            },
            OperationExpectation::RejectDvpProposal {
                party: config.party_id.clone(),
                dvp_proposal_cid: item.cid.clone(),
            },
        ),
    };

    let resp = client
        .submit_transaction(req, &expectation, false, false, false)
        .await?;
    if !resp.success {
        anyhow::bail!(
            "execute failed: {}",
            resp.error_message.unwrap_or_else(|| "unknown".to_string())
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LP: &str = "15c7a79bbb0dfb67::1220af9f";
    const OTHER: &str = "0b415857869e4865::122070e6";
    const NOW: i64 = 1_784_405_386_000_000; // 2026-07-18
    const HOUR: i64 = 3_600_000_000;

    /// Payload shaped like the real mainnet ACS dump (micros as integers).
    fn payload(proposer: &str, counterparty: &str, settle_before: i64) -> Value {
        json!({
            "operator": "silvana-orderbook::1220997446",
            "proposer": proposer,
            "counterparty": counterparty,
            "proposerIsBuyer": true,
            "terms": {
                "id": "019c668b-007d-7bf3-9d63-f48de91061bf",
                "createdAt": settle_before - 2 * 24 * HOUR,
                "allocateBefore": settle_before - 24 * HOUR,
                "settleBefore": settle_before,
            }
        })
    }

    #[test]
    fn cancels_own_expired_proposal() {
        let p = payload(LP, OTHER, NOW - 24 * HOUR);
        assert_eq!(
            classify_proposal(&p, LP, NOW, HOUR, true),
            Some((GcAction::Cancel, NOW - 24 * HOUR)),
        );
    }

    #[test]
    fn rejects_foreign_expired_proposal_when_enabled() {
        let p = payload(OTHER, LP, NOW - 24 * HOUR);
        assert_eq!(
            classify_proposal(&p, LP, NOW, HOUR, true),
            Some((GcAction::Reject, NOW - 24 * HOUR)),
        );
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, false), None);
    }

    #[test]
    fn leaves_live_and_margin_window_proposals() {
        // Still live
        let p = payload(LP, OTHER, NOW + HOUR);
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, true), None);
        // Expired but inside the safety margin
        let p = payload(LP, OTHER, NOW - HOUR / 2);
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, true), None);
        // Exactly at the margin boundary is still excluded (strict <)
        let p = payload(LP, OTHER, NOW - HOUR);
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, true), None);
    }

    #[test]
    fn leaves_unrelated_and_unparseable_proposals() {
        // Neither proposer nor counterparty
        let p = payload(OTHER, "third-party::1220aaaa", NOW - 24 * HOUR);
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, true), None);
        // Missing terms.settleBefore → never archive
        let p = json!({ "proposer": LP, "counterparty": OTHER, "terms": {} });
        assert_eq!(classify_proposal(&p, LP, NOW, HOUR, true), None);
    }

    #[test]
    fn parses_string_and_float_timestamps() {
        let mut p = payload(LP, OTHER, 0);
        p["terms"]["settleBefore"] = json!((NOW - 24 * HOUR).to_string());
        assert_eq!(
            classify_proposal(&p, LP, NOW, HOUR, true),
            Some((GcAction::Cancel, NOW - 24 * HOUR)),
        );
        p["terms"]["settleBefore"] = json!((NOW - 24 * HOUR) as f64);
        assert_eq!(
            classify_proposal(&p, LP, NOW, HOUR, true),
            Some((GcAction::Cancel, NOW - 24 * HOUR)),
        );
    }
}
