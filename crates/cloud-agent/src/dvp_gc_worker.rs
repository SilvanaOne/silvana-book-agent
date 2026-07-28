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
//!
//! That gate makes this worker wholly dependent on
//! `agent_logic::forecast::spawn_forecast_poller` keeping the coefficient
//! fresh. If the poller dies the coefficient freezes, and a frozen low value is
//! indistinguishable from a real one at the gate — so the gate warns on a stale
//! forecast rather than waiting quietly, and gives up on a cycle after
//! `DVP_GC_MAX_PAUSE_SECS` instead of parking forever holding a queue of
//! contract IDs that is aging out from under it.

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
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
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
    LazyLock::new(|| env_parse("DVP_GC_MIN_COEFFICIENT", 0.68));
static GC_DELAY_SECS: LazyLock<u64> = LazyLock::new(|| env_parse("DVP_GC_DELAY_SECS", 2));
static GC_REJECT_ENABLED: LazyLock<bool> =
    LazyLock::new(|| env_flag("DVP_GC_REJECT_ENABLED", true));
static GC_REFRESH_SECS: LazyLock<u64> = LazyLock::new(|| env_parse("DVP_GC_REFRESH_SECS", 3600));
static GC_SAFETY_MARGIN_SECS: LazyLock<u64> =
    LazyLock::new(|| env_parse("DVP_GC_SAFETY_MARGIN_SECS", 3600));
/// Abandon the cycle and rescan after the gate has been shut this long. The
/// queue is a snapshot of contract IDs; holding one for hours is worse than
/// spending ~2s re-deriving it. Floored at 60: a value of 0 would mean
/// "abandon instantly", turning the cap into a switch that disables the drain
/// whenever the gate is shut — an operator trying to disable the CAP should
/// set it large, not zero.
static GC_MAX_PAUSE_SECS: LazyLock<u64> =
    LazyLock::new(|| env_parse("DVP_GC_MAX_PAUSE_SECS", 900).max(60));
/// Warn when the coefficient the gate is reading is older than this.
static GC_STALE_FORECAST_SECS: LazyLock<u64> =
    LazyLock::new(|| env_parse("DVP_GC_STALE_FORECAST_SECS", 300));

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
             safety_margin={}s, max_pause={}s, reject_enabled={}",
            *GC_MIN_COEFFICIENT,
            *GC_DELAY_SECS,
            *GC_REFRESH_SECS,
            *GC_SAFETY_MARGIN_SECS,
            *GC_MAX_PAUSE_SECS,
            *GC_REJECT_ENABLED,
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

        let cancels = queue
            .iter()
            .filter(|i| i.action == GcAction::Cancel)
            .count();
        let rejects = queue.len() - cancels;
        info!(
            "DvpProposal GC cycle: scanned={}, eligible={} (cancel={}, reject={}), coefficient={:.4}",
            scanned,
            queue.len(),
            cancels,
            rejects,
            agent_logic::forecast::coefficient_value(),
        );

        // --- Drain: one archival tx per proposal, coefficient-gated, throttled ---
        let mut done: u64 = 0;
        let mut skipped_gone: u64 = 0;
        let mut backpressured: u64 = 0;
        let mut gate_timed_out = false;
        let mut consecutive_failures: u32 = 0;
        'drain: for item in &queue {
            match await_gate(
                &shutdown,
                delay,
                done,
                queue.len(),
                agent_logic::forecast::coefficient_value,
            )
            .await
            {
                GateResult::Proceed => {}
                GateResult::Shutdown => {
                    info!("DvpProposal GC shutting down");
                    return;
                }
                GateResult::Timeout => {
                    gate_timed_out = true;
                    info!(
                        "DvpProposal GC gate shut for {}s — abandoning cycle, rescanning now \
                         ({}/{} archived)",
                        *GC_MAX_PAUSE_SECS,
                        done,
                        queue.len(),
                    );
                    break 'drain;
                }
            }

            match archive_one(&mut client, &config, item).await {
                Ok(()) => {
                    done += 1;
                    consecutive_failures = 0;
                    info!(
                        "DvpProposal GC: {} {} ({}/{})",
                        if item.action == GcAction::Cancel {
                            "cancelled"
                        } else {
                            "rejected"
                        },
                        &item.cid[..item.cid.len().min(16)],
                        done,
                        queue.len(),
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
                            &item.cid[..item.cid.len().min(16)],
                            msg,
                        );
                    } else if msg.contains("SEQUENCER_BACKPRESSURE") {
                        // Backpressure is "try again later", not a fault, so it
                        // must not feed the breaker: 20 of these in a row would
                        // abandon the cycle for GC_REFRESH_SECS during exactly
                        // the stretch where the backlog is worst. The proposal
                        // stays active in the ACS and the next scan re-queues
                        // it. Like the already-gone skip, the failure counter is
                        // left untouched — neither success nor failure.
                        backpressured += 1;
                        debug!(
                            "DvpProposal GC: {} deferred by sequencer backpressure",
                            &item.cid[..item.cid.len().min(16)],
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
                "DvpProposal GC cycle done: archived={}, already_gone={}, backpressured={}, remaining={}",
                done,
                skipped_gone,
                backpressured,
                queue.len() as u64 - done - skipped_gone,
            );
            // A cycle where everything was "already gone" but the scan still
            // returned it is only plausible when the prepare path is looking
            // at the wrong participant/user — surface it loudly. Only when the
            // WHOLE queue was walked and every item was gone: a cycle cut
            // short by a gate timeout or drowned in backpressure also ends
            // with done == 0, and must not send anyone chasing routing bugs.
            if done == 0
                && backpressured == 0
                && !gate_timed_out
                && skipped_gone == queue.len() as u64
                && skipped_gone > 1
            {
                warn!(
                    "DvpProposal GC: entire cycle ({}) skipped as already-gone — \
                     verify prepare path / participant routing",
                    skipped_gone,
                );
            }
        }

        // A gate timeout abandoned a queue that is still (mostly) live — rescan
        // promptly instead of idling out the refresh interval: the coefficient
        // may have recovered seconds after the cutoff. No busy-spin is
        // possible: if the gate is still shut, the next cycle's first
        // await_gate blocks another GC_MAX_PAUSE_SECS, so the worst case is
        // one ~2s scan per pause window.
        if gate_timed_out {
            if shutdown.sleep(delay).await {
                info!("DvpProposal GC shutting down");
                return;
            }
            continue;
        }

        if shutdown.sleep(Duration::from_secs(*GC_REFRESH_SECS)).await {
            info!("DvpProposal GC shutting down");
            return;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateResult {
    /// The coefficient cleared the threshold — archive the next proposal.
    Proceed,
    /// Shutdown fired while waiting.
    Shutdown,
    /// The gate stayed shut for `DVP_GC_MAX_PAUSE_SECS`; abandon the cycle.
    Timeout,
}

/// Block until the issuance coefficient clears `DVP_GC_MIN_COEFFICIENT`.
///
/// Logs on the shut->open *edges* only. The predecessor logged one `debug!` per
/// poll, which the default `cloud_agent=info` filter drops — so a worker parked
/// here produced no output whatsoever and looked identical to a worker that had
/// finished its work. Anything that can block indefinitely has to say so once,
/// at a level that is actually on.
///
/// Edge logging needs no rate limiting: the server republishes the coefficient
/// only about every 10 minutes, so the gate cannot flap faster than that no
/// matter how quickly the drain loop cycles.
///
/// `coefficient` is injected rather than read directly so the gate can be
/// tested against a scripted sequence instead of the process-global forecast.
async fn await_gate<F: Fn() -> f64>(
    shutdown: &Shutdown,
    delay: Duration,
    done: u64,
    total: usize,
    coefficient: F,
) -> GateResult {
    // Shutdown wins over a clear coefficient — checked FIRST so no new
    // archival starts after the flag is set (a SIGTERM landing during the
    // multi-second ACS scan must not be followed by one more prepare/execute).
    if shutdown.is_shutting_down() {
        return GateResult::Shutdown;
    }
    let coeff = coefficient();
    if coeff >= *GC_MIN_COEFFICIENT {
        return GateResult::Proceed;
    }

    info!(
        "DvpProposal GC gate shut: coefficient {:.4} < {:.2} — pausing drain ({}/{} archived)",
        coeff, *GC_MIN_COEFFICIENT, done, total,
    );
    warn_if_forecast_stale(coeff);

    // `tokio::time::Instant`, not `std::time::Instant`: it honours a paused
    // clock, so the timeout is reachable in tests without waiting 15 real
    // minutes. Identical behaviour in production.
    let started = tokio::time::Instant::now();
    let max_pause = Duration::from_secs(*GC_MAX_PAUSE_SECS);
    loop {
        if shutdown.sleep(delay).await {
            return GateResult::Shutdown;
        }
        let coeff = coefficient();
        if coeff >= *GC_MIN_COEFFICIENT {
            info!(
                "DvpProposal GC gate open: coefficient {:.4} >= {:.2} — resuming after {}s paused",
                coeff,
                *GC_MIN_COEFFICIENT,
                started.elapsed().as_secs(),
            );
            return GateResult::Proceed;
        }
        if started.elapsed() >= max_pause {
            return GateResult::Timeout;
        }
    }
}

/// Separate "the sequencer is genuinely busy" from "nothing has updated this
/// number in hours". Only the first is a reason to wait quietly; the second
/// means every consumer of the coefficient in this process — the RFQ overload
/// gates and the fee-dispatch pause, not just this worker — is deciding on a
/// frozen value, and needs a human.
fn warn_if_forecast_stale(coeff: f64) {
    match agent_logic::forecast::forecast_age_secs() {
        None => warn!(
            "DvpProposal GC: no issuance forecast has ever been received — the gate will stay \
             shut indefinitely; is the forecast poller running?"
        ),
        Some(age) if age > *GC_STALE_FORECAST_SECS => warn!(
            "DvpProposal GC: issuance forecast is stale ({}s old, coefficient {:.4}) — the gate \
             is deciding on a frozen value",
            age, coeff,
        ),
        Some(_) => {}
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
            let (action, settle_before_micros) =
                classify_proposal(&args, &config.party_id, now, margin, *GC_REJECT_ENABLED)?;
            Some(GcItem {
                cid: c.contract_id,
                action,
                settle_before_micros,
            })
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

    // --- gate ---
    //
    // The coefficient is injected, so these are independent of the
    // process-global forecast and of each other.

    const TEST_DELAY: Duration = Duration::from_secs(1);

    fn above() -> f64 {
        *GC_MIN_COEFFICIENT + 0.05
    }
    fn below() -> f64 {
        *GC_MIN_COEFFICIENT - 0.05
    }

    #[tokio::test(start_paused = true)]
    async fn gate_proceeds_immediately_when_coefficient_clears() {
        let shutdown = Shutdown::new();
        assert_eq!(
            await_gate(&shutdown, TEST_DELAY, 0, 10, above).await,
            GateResult::Proceed,
        );
    }

    /// Regression for the failure this replaced: the old gate had no timeout, so
    /// a coefficient that stayed below the threshold parked the worker forever
    /// on a queue of contract IDs that kept aging, and it never rescanned.
    #[tokio::test(start_paused = true)]
    async fn gate_times_out_instead_of_parking_forever() {
        let shutdown = Shutdown::new();
        assert_eq!(
            await_gate(&shutdown, TEST_DELAY, 0, 10, below).await,
            GateResult::Timeout,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn gate_resumes_when_the_coefficient_recovers() {
        let shutdown = Shutdown::new();
        let polls = std::sync::atomic::AtomicU32::new(0);
        let result = await_gate(&shutdown, TEST_DELAY, 0, 10, || {
            if polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                below()
            } else {
                above()
            }
        })
        .await;
        assert_eq!(result, GateResult::Proceed);
        assert!(
            polls.load(std::sync::atomic::Ordering::Relaxed) >= 4,
            "gate must re-read the coefficient while waiting, not latch the first value",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn gate_reports_shutdown_rather_than_waiting_out_the_pause() {
        let shutdown = Shutdown::new();
        shutdown.signal();
        assert_eq!(
            await_gate(&shutdown, TEST_DELAY, 0, 10, below).await,
            GateResult::Shutdown,
        );
        // Pins the check ORDERING: even a clear coefficient must not win over
        // an already-signalled shutdown — otherwise one more archival tx
        // starts after SIGTERM. (With `below` alone, either ordering passes.)
        assert_eq!(
            await_gate(&shutdown, TEST_DELAY, 0, 10, above).await,
            GateResult::Shutdown,
        );
    }

    /// Shutdown signalled *during* the wait must also break out — otherwise a
    /// Ctrl-C lands behind up to DVP_GC_MAX_PAUSE_SECS of gate wait.
    #[tokio::test(start_paused = true)]
    async fn gate_wakes_on_shutdown_signalled_mid_wait() {
        let shutdown = Shutdown::new();
        let waker = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            waker.signal();
        });
        assert_eq!(
            await_gate(&shutdown, TEST_DELAY, 0, 10, below).await,
            GateResult::Shutdown,
        );
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
