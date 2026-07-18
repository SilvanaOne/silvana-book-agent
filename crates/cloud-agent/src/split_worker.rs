//! RFQ V2 maintenance worker (design §5.3 + §5.4): pre-splitting for
//! concurrency and SettlementTicket batch refill, on one
//! `split_poll_interval_secs` tick.
//!
//! - Ticket refill: when the pool's free count drops below `ticket_low_water`,
//!   issue `ticket_batch_size` client-generated UUIDv7 tickets via
//!   `IssueTicketsParams` (the request signature binds the ids), then
//!   reconcile the pool (blob backfill) from `GetAtomicContracts`.
//! - Denominations: per rfq_v2 market and LP-pays instrument, keep
//!   `count` selectable holdings in `[denom, 2*denom)` per configured rung;
//!   deficits are split off the splitter-reserve holding — utility instruments
//!   via `SplitHoldingsParams`, CC via the existing v1 `SplitCc` operation.
//!   The splitter-reserve holding is consumed ONLY here, so split-vs-quote
//!   contention is impossible by construction.

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

use agent_logic::config::{BaseConfig, RfqV2Config};
use agent_logic::shutdown::Shutdown;
use orderbook_proto::ledger::{
    prepare_transaction_request::Params, PrepareTransactionRequest, SplitCcParams,
    TransactionOperation,
};
use orderbook_proto::rfqv2::{
    prepare_atomic_transaction_request::Params as AtomicParams, IssueTicketsParams,
    PrepareAtomicTransactionRequest, SplitHoldingsParams, SplitSpec,
};
use tx_verifier::OperationExpectation;

use crate::holdings_cache::{HoldingsCache, InstrumentKey};
use crate::ledger_client::{AtomicProviderClient, DAppProviderClient};
use crate::ticket_pool::{TicketAcsInfo, TicketPool};
use crate::venue_registry::TEMPLATE_SETTLEMENT_TICKET;

/// Max explicit outputs in one split transaction. The Amulet transfer config
/// caps `transfer.outputs` at `maxNumOutputs` (100 on devnet+mainnet, confirmed
/// live via scan `/api/scan/v0/dso`; `AmuletRules.daml` `checkTransferConstraints`
/// rejects `length transfer.outputs > maxNumOutputs` with `maximum-outputs-exceeded`).
/// Sender-change is computed separately and not counted, but we keep headroom for
/// any change/fee output the `SplitCc` builder may emit. Deficits above this fill
/// over successive ticks (the worker recomputes per-rung deficits every tick).
const MAX_SPLIT_OUTPUTS_PER_TX: u32 = 90;

/// Budget window for the per-instrument fail-stop split cap.
const SPLIT_BUDGET_WINDOW: Duration = Duration::from_secs(3600);

/// One LP-pays instrument, resolved for splitting.
#[derive(Debug, Clone)]
pub struct SplitInstrument {
    pub key: InstrumentKey,
    pub is_cc: bool,
    /// on-chain instrument id (e.g. "USDC"; "Amulet" for CC)
    pub on_chain_id: String,
    /// registry/admin party
    pub admin: String,
}

/// Per-INSTRUMENT split policy: one global ladder per instrument, shared by
/// every market that pays it (from `[liquidity_provider.rfq_v2.denominations]`,
/// with a legacy per-market fallback derived at startup).
#[derive(Debug, Clone)]
pub struct SplitTarget {
    pub instrument: SplitInstrument,
    /// "AMOUNTxCOUNT" specs
    pub denominations: Vec<String>,
}

/// Parse "AMOUNTxCOUNT" split specs (port of the dvp `parse_splits`).
pub fn parse_splits(specs: &[String]) -> Result<Vec<(Decimal, u32)>> {
    specs
        .iter()
        .map(|s| {
            let (amount, count) = s
                .split_once(['x', 'X'])
                .ok_or_else(|| anyhow!("invalid split spec '{s}' (expected AMOUNTxCOUNT)"))?;
            let amount: Decimal = amount
                .trim()
                .parse()
                .with_context(|| format!("invalid split amount in '{s}'"))?;
            let count: u32 = count
                .trim()
                .parse()
                .with_context(|| format!("invalid split count in '{s}'"))?;
            if amount <= Decimal::ZERO || count == 0 {
                return Err(anyhow!("split spec '{s}' must be positive"));
            }
            Ok((amount, count))
        })
        .collect()
}

/// Truncate `splits` (in order) so the cumulative output count ≤ `max`, keeping
/// earlier rungs whole and partially truncating the rung that crosses the
/// boundary (later rungs are dropped). Any remaining deficit is picked up on the
/// next tick. Pure so it can be unit-tested.
fn cap_split_outputs(splits: &[(Decimal, u32)], max: u32) -> Vec<(Decimal, u32)> {
    let mut out: Vec<(Decimal, u32)> = Vec::new();
    let mut used = 0u32;
    for (denom, count) in splits {
        if used >= max {
            break;
        }
        let take = (*count).min(max - used);
        if take == 0 {
            continue;
        }
        out.push((*denom, take));
        used += take;
    }
    out
}

/// Low-water hysteresis (split-storm guard): a rung refills only once it
/// falls below `max(1, count / divisor)`, and a triggered refill tops the
/// WHOLE ladder back up to its full counts in one operation — so ops are
/// rare and big instead of frequent per-settle top-ups. Returns the
/// per-rung deficits to split, or empty when no rung breached its
/// low-water mark. Pure so it can be unit-tested.
fn plan_ladder_refill(
    rungs: &[(Decimal, u32)],
    haves: &[u32],
    low_water_divisor: u32,
) -> Vec<(Decimal, u32)> {
    let divisor = low_water_divisor.max(1);
    let triggered = rungs.iter().zip(haves).any(|((_, count), have)| {
        *have < (*count / divisor).max(1)
    });
    if !triggered {
        return Vec::new();
    }
    rungs
        .iter()
        .zip(haves)
        .filter(|((_, count), have)| **have < *count)
        .map(|((denom, count), have)| (*denom, count - have))
        .collect()
}

/// Fail-stop sanity check: the ladder already holds >= 2x its target count —
/// the coverage counter and the ledger disagree, so adding MORE rungs can
/// only feed a runaway. Pure so it can be unit-tested.
fn ladder_overfull(rungs: &[(Decimal, u32)], haves: &[u32]) -> bool {
    let target: u32 = rungs.iter().map(|(_, c)| c).sum();
    let have: u32 = haves.iter().sum();
    target > 0 && have >= target.saturating_mul(2)
}

/// Cooldown + hourly budget gate over the recorded split-op times.
#[derive(Debug, PartialEq, Eq)]
enum SplitGate {
    Allow,
    /// Last op was `secs_since_last` seconds ago, under the minimum interval.
    Cooldown { secs_since_last: u64 },
    /// `ops_in_window` ops in the budget window already meet the hourly cap.
    BudgetExhausted { ops_in_window: usize },
}

/// Evaluate the per-instrument split gate: hard budget first (loud,
/// self-stopping), then the minimum-interval cooldown. Pure so it can be
/// unit-tested.
fn split_gate(
    op_times: &[Instant],
    now: Instant,
    min_interval: Duration,
    max_ops_per_window: u32,
) -> SplitGate {
    let ops_in_window = op_times
        .iter()
        .filter(|t| now.duration_since(**t) < SPLIT_BUDGET_WINDOW)
        .count();
    if ops_in_window >= max_ops_per_window as usize {
        return SplitGate::BudgetExhausted { ops_in_window };
    }
    if let Some(last) = op_times.iter().max() {
        let since = now.duration_since(*last);
        if since < min_interval {
            return SplitGate::Cooldown { secs_since_last: since.as_secs() };
        }
    }
    SplitGate::Allow
}

/// Spawn the maintenance worker (LP mode with rfq_v2 enabled only).
pub fn spawn_maintenance_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    ticket_pool: Option<Arc<TicketPool>>,
    targets: Vec<SplitTarget>,
    v2: RfqV2Config,
    shutdown: Shutdown,
) {
    tokio::spawn(async move {
        info!(
            "V2 maintenance worker started (tick {}s, {} split target(s), tickets={})",
            v2.split_poll_interval_secs,
            targets.len(),
            ticket_pool.is_some(),
        );

        // Small initial delay so the first ACS refresh can populate the cache
        if shutdown.sleep(Duration::from_secs(10)).await {
            return;
        }

        loop {
            if shutdown.is_shutting_down() {
                info!("V2 maintenance worker shutting down");
                return;
            }

            if let Err(e) = tick(&config, &cache, &ticket_pool, &targets, &v2).await {
                warn!("V2 maintenance tick failed: {:#}", e);
            }

            if shutdown
                .sleep(Duration::from_secs(v2.split_poll_interval_secs))
                .await
            {
                info!("V2 maintenance worker shutting down");
                return;
            }
        }
    });
}

async fn tick(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    ticket_pool: &Option<Arc<TicketPool>>,
    targets: &[SplitTarget],
    v2: &RfqV2Config,
) -> Result<()> {
    let mut atomic_client = create_atomic_client(config).await?;

    // --- (a) ticket pool: reconcile every tick + refill when below low-water ---
    if let Some(pool) = ticket_pool {
        reconcile_tickets(&mut atomic_client, pool, &config.party_id).await?;

        let free = pool.free_count();
        if free < v2.ticket_low_water {
            let ticket_ids: Vec<String> = (0..v2.ticket_batch_size)
                .map(|_| uuid::Uuid::now_v7().to_string())
                .collect();
            info!(
                "Ticket refill: free={} < low_water={} — issuing {} tickets",
                free,
                v2.ticket_low_water,
                ticket_ids.len()
            );
            let expectation = OperationExpectation::IssueTickets {
                lp_party: config.party_id.clone(),
                ticket_count: ticket_ids.len(),
            };
            match atomic_client
                .submit_atomic_transaction(
                    PrepareAtomicTransactionRequest {
                        params: Some(AtomicParams::IssueTickets(IssueTicketsParams {
                            ticket_ids: ticket_ids.clone(),
                        })),
                        request_signature: None,
                    },
                    &expectation,
                    false,
                    false,
                    false,
                )
                .await
            {
                Ok(resp) => {
                    info!("Issued {} tickets (update {})", ticket_ids.len(), resp.update_id);
                    // Blob backfill for the fresh tickets
                    reconcile_tickets(&mut atomic_client, pool, &config.party_id).await?;
                }
                Err(e) => warn!("Ticket issue failed: {:#}", e),
            }
        }
    }

    // --- (b) denomination coverage per LP-pays instrument ---
    for target in targets {
        let rungs: Vec<(Decimal, u32)> = match parse_splits(&target.denominations) {
            Ok(r) => r,
            Err(e) => {
                warn!("{}: bad denominations: {:#}", target.instrument.key, e);
                continue;
            }
        };
        if rungs.is_empty() {
            continue;
        }

        if let Err(e) = ensure_denominations(
            config, cache, &mut atomic_client, &target.instrument, &rungs, v2,
        )
        .await
        {
            warn!("Split for {} failed: {:#}", target.instrument.key, e);
        }
    }

    Ok(())
}

/// Count coverage per rung and split deficits off the splitter reserve.
/// `pub(crate)` so the RFQ V2 indicative path can trigger an on-demand split
/// before signing when selection comes up empty.
///
/// Split-storm guards (both entry points — the maintenance tick and the
/// rfq_v2 on-demand kicks — funnel through here, and concurrent calls for
/// one instrument are serialized by the all-or-nothing splitter-reserve
/// reservation):
/// - coverage is EXISTENCE via [`HoldingsCache::count_in_band`] (TTL-stale /
///   reserved / blob-pending rungs and optimistic post-split rungs count), so
///   cache invisibility cannot create phantom deficits;
/// - low-water hysteresis: split only when a rung drops below
///   `max(1, count / split_low_water_divisor)`, then refill the full ladder;
/// - fail-stop: refuse (loudly) when the ladder already holds >= 2x its
///   target or `split_max_ops_per_hour` ops were already submitted;
/// - cooldown: at most one split op per `split_min_interval_secs`.
pub(crate) async fn ensure_denominations(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    atomic_client: &mut AtomicProviderClient,
    instrument: &SplitInstrument,
    rungs: &[(Decimal, u32)],
    v2: &RfqV2Config,
) -> Result<()> {
    // Coverage per rung in [denom, 2*denom): existence, not usability.
    let mut haves: Vec<u32> = Vec::with_capacity(rungs.len());
    for (denom, _) in rungs {
        haves.push(
            cache
                .count_in_band(&instrument.key, *denom, *denom * Decimal::TWO)
                .await,
        );
    }

    let deficits = plan_ladder_refill(rungs, &haves, v2.split_low_water_divisor);
    if deficits.is_empty() {
        return Ok(());
    }

    // Fail-stop: some band looks starved while the ladder as a whole holds
    // >= 2x its target — the runaway signature. Never add more rungs to it.
    if ladder_overfull(rungs, &haves) {
        error!(
            "{}: REFUSING split — {} holdings in-band vs ladder target {} (>= 2x): \
             coverage counter and ledger disagree (haves={:?}, rungs={:?})",
            instrument.key,
            haves.iter().sum::<u32>(),
            rungs.iter().map(|(_, c)| c).sum::<u32>(),
            haves,
            rungs,
        );
        return Ok(());
    }

    // Cooldown + hourly fail-stop budget, shared across both entry points.
    match split_gate(
        &cache.split_ops(&instrument.key).await,
        Instant::now(),
        Duration::from_secs(v2.split_min_interval_secs),
        v2.split_max_ops_per_hour,
    ) {
        SplitGate::Allow => {}
        SplitGate::Cooldown { secs_since_last } => {
            debug!(
                "{}: split cooldown — last op {}s ago (< {}s), deficits {:?} wait for the next tick",
                instrument.key, secs_since_last, v2.split_min_interval_secs, deficits,
            );
            return Ok(());
        }
        SplitGate::BudgetExhausted { ops_in_window } => {
            error!(
                "{}: REFUSING split — {} split ops in the last hour >= budget {} \
                 (haves={:?}, rungs={:?}, deficits={:?}): possible split runaway, halting until the window clears",
                instrument.key, ops_in_window, v2.split_max_ops_per_hour, haves, rungs, deficits,
            );
            return Ok(());
        }
    }

    let Some(reserve) = cache.splitter_reserve(&instrument.key).await else {
        debug!("No splitter-reserve holding for {} — skipping split", instrument.key);
        return Ok(());
    };

    // Cap counts to what the reserve can fund, keeping ~5% as change so the
    // reserve keeps existing.
    let mut budget = reserve.amount * Decimal::from_str("0.95").unwrap();
    let mut splits: Vec<(Decimal, u32)> = Vec::new();
    for (denom, deficit) in deficits {
        if budget < denom {
            continue;
        }
        let affordable = (budget / denom).trunc().to_u32().unwrap_or(0);
        let count = deficit.min(affordable);
        if count == 0 {
            continue;
        }
        budget -= denom * Decimal::from(count);
        splits.push((denom, count));
    }

    // Cap outputs per transaction under the Amulet `maxNumOutputs` limit. A
    // deficit larger than the cap fills over successive ticks rather than in one
    // over-large (and rejected) transfer.
    let total_out: u32 = splits.iter().map(|(_, c)| c).sum();
    if total_out > MAX_SPLIT_OUTPUTS_PER_TX {
        splits = cap_split_outputs(&splits, MAX_SPLIT_OUTPUTS_PER_TX);
        debug!(
            "{}: capping split to {} outputs/tx (deficit {} fills over next tick(s))",
            instrument.key, MAX_SPLIT_OUTPUTS_PER_TX, total_out,
        );
    }

    if splits.is_empty() {
        debug!(
            "Splitter reserve for {} too small ({}) for the deficits — skipping",
            instrument.key, reserve.amount
        );
        return Ok(());
    }

    let job_id = format!("split-{}", uuid::Uuid::now_v7());
    let input_cids = vec![reserve.contract_id.clone()];
    if !cache.reserve_split(&input_cids, &job_id).await {
        debug!("Splitter reserve for {} busy — skipping this tick", instrument.key);
        return Ok(());
    }

    info!(
        "Splitting {}: {:?} off reserve {} ({})",
        instrument.key,
        splits
            .iter()
            .map(|(d, c)| format!("{d}x{c}"))
            .collect::<Vec<_>>(),
        reserve.contract_id,
        reserve.amount,
    );

    // The submission ATTEMPT counts toward the cooldown + hourly budget: a
    // storm of failing (or wrongly-reported-failing) submissions must
    // self-stop just like a storm of committed ones.
    cache.record_split_op(&instrument.key).await;

    let result: Result<String> = if instrument.is_cc {
        split_cc(config, &input_cids, &splits).await
    } else {
        split_utility(config, atomic_client, instrument, &input_cids, &splits).await
    };

    match result {
        Ok(update_id) => {
            // Input consumed; outputs enter the cache via the updates watcher /
            // ACS refresh (the atomic response carries no amounts). Until a
            // complete ACS snapshot confirms them, the created rungs count
            // optimistically so the next tick sees no phantom deficit.
            cache.mark_consumed(&input_cids, &update_id).await;
            cache.record_pending_split(&instrument.key, &splits).await;
            info!("Split committed for {} (update {})", instrument.key, update_id);
            Ok(())
        }
        Err(e) => {
            cache.release_reservations(&input_cids).await;
            Err(e)
        }
    }
}

/// CC split via the existing v1 SplitCc operation (mirror of merge_worker).
async fn split_cc(
    config: &BaseConfig,
    input_cids: &[String],
    splits: &[(Decimal, u32)],
) -> Result<String> {
    let mut output_amounts: Vec<String> = Vec::new();
    for (denom, count) in splits {
        for _ in 0..*count {
            output_amounts.push(denom.to_string());
        }
    }

    let mut client = create_v1_client(config).await?;
    let expectation = OperationExpectation::SplitCc {
        party: config.party_id.clone(),
        output_amounts: output_amounts.clone(),
    };
    let resp = client
        .submit_transaction(
            PrepareTransactionRequest {
                operation: TransactionOperation::SplitCc as i32,
                params: Some(Params::SplitCc(SplitCcParams {
                    output_amounts,
                    amulet_cids: input_cids.to_vec(),
                })),
                request_signature: None,
            },
            &expectation,
            false,
            false,
            false,
        )
        .await?;
    Ok(resp.update_id)
}

/// Utility split via `AtomicDVPService_SplitHoldings` (CIP-56 self-transfer
/// plan resolved by the ledger service, which discovers + discloses the
/// provider-signed singleton itself — fa-design G2).
async fn split_utility(
    config: &BaseConfig,
    atomic_client: &mut AtomicProviderClient,
    instrument: &SplitInstrument,
    input_cids: &[String],
    splits: &[(Decimal, u32)],
) -> Result<String> {
    let split_specs: Vec<SplitSpec> = splits
        .iter()
        .map(|(denom, count)| SplitSpec {
            amount: denom.to_string(),
            count: *count,
        })
        .collect();
    let expectation = OperationExpectation::SplitHoldings {
        lp_party: config.party_id.clone(),
        instrument_id: instrument.on_chain_id.clone(),
        split_count: split_specs.len(),
        input_cids: input_cids.to_vec(),
    };
    let resp = atomic_client
        .submit_atomic_transaction(
            PrepareAtomicTransactionRequest {
                params: Some(AtomicParams::SplitHoldings(SplitHoldingsParams {
                    instrument_id: instrument.on_chain_id.clone(),
                    instrument_admin: instrument.admin.clone(),
                    input_holding_cids: input_cids.to_vec(),
                    splits: split_specs,
                })),
                request_signature: None,
            },
            &expectation,
            false,
            false,
            false,
        )
        .await?;
    Ok(resp.update_id)
}

/// Reconcile the ticket pool from the on-ledger SettlementTicket set.
async fn reconcile_tickets(
    client: &mut AtomicProviderClient,
    pool: &Arc<TicketPool>,
    party_id: &str,
) -> Result<()> {
    let resp = client
        .get_atomic_contracts(&[TEMPLATE_SETTLEMENT_TICKET.to_string()], &[])
        .await?;
    let mut on_ledger: Vec<TicketAcsInfo> = Vec::new();
    for c in resp.contracts {
        if !c.template_id.contains("AtomicDVP:SettlementTicket") {
            continue;
        }
        let payload: serde_json::Value = match serde_json::from_str(&c.payload_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let lp = payload.get("lp").and_then(|v| v.as_str());
        if lp.is_some() && lp != Some(party_id) {
            continue;
        }
        let Some(ticket_id) = payload.get("ticketId").and_then(|v| v.as_str()) else {
            continue;
        };
        on_ledger.push(TicketAcsInfo {
            ticket_id: ticket_id.to_string(),
            contract_id: c.contract_id.clone(),
            template_id: Some(c.template_id.clone()),
            created_event_blob: (!c.created_event_blob.is_empty())
                .then(|| c.created_event_blob.clone()),
            payload_json: Some(c.payload_json.clone()),
        });
    }
    pool.reconcile_from_acs(on_ledger);
    Ok(())
}

async fn create_atomic_client(config: &BaseConfig) -> Result<AtomicProviderClient> {
    AtomicProviderClient::new(
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

async fn create_v1_client(config: &BaseConfig) -> Result<DAppProviderClient> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_specs() {
        let parsed = parse_splits(&["25x20".to_string(), "100X10".to_string()]).unwrap();
        assert_eq!(parsed, vec![("25".parse().unwrap(), 20), ("100".parse().unwrap(), 10)]);
        assert!(parse_splits(&["25".to_string()]).is_err());
        assert!(parse_splits(&["0x5".to_string()]).is_err());
        assert!(parse_splits(&["5x0".to_string()]).is_err());
        assert!(parse_splits(&["ax5".to_string()]).is_err());
    }

    #[test]
    fn cap_split_outputs_truncates_to_max() {
        let d = |s: &str| Decimal::from_str(s).unwrap();

        // single over-cap rung → truncated to `max` (the CC 150x120 case)
        assert_eq!(cap_split_outputs(&[(d("150"), 120)], 90), vec![(d("150"), 90)]);

        // multi-rung: first rung whole, second partially truncated, rest dropped
        assert_eq!(
            cap_split_outputs(&[(d("150"), 50), (d("300"), 60), (d("600"), 10)], 90),
            vec![(d("150"), 50), (d("300"), 40)],
        );

        // already under the cap → returned unchanged
        assert_eq!(
            cap_split_outputs(&[(d("150"), 40), (d("300"), 20)], 90),
            vec![(d("150"), 40), (d("300"), 20)],
        );

        // exactly at the cap → unchanged
        assert_eq!(cap_split_outputs(&[(d("150"), 90)], 90), vec![(d("150"), 90)]);
    }

    #[test]
    fn low_water_hysteresis_only_triggers_below_quarter() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        let rungs = vec![(d("25"), 20), (d("100"), 10)];

        // full ladder → nothing
        assert!(plan_ladder_refill(&rungs, &[20, 10], 4).is_empty());

        // partial but above low-water (low-water: 20/4=5, 10/4=2) → nothing —
        // this is the per-settle top-up case (count<=10 deficits) that used to
        // fire an op per settle
        assert!(plan_ladder_refill(&rungs, &[6, 3], 4).is_empty());

        // exactly AT low-water is not below it → nothing
        assert!(plan_ladder_refill(&rungs, &[5, 2], 4).is_empty());

        // one rung below low-water → the WHOLE ladder refills to full counts
        assert_eq!(
            plan_ladder_refill(&rungs, &[4, 9], 4),
            vec![(d("25"), 16), (d("100"), 1)],
        );

        // over-full rungs never produce negative/zero deficits
        assert_eq!(
            plan_ladder_refill(&rungs, &[0, 15], 4),
            vec![(d("25"), 20)],
        );

        // small counts: low-water = max(1, count/4) = 1, so only have=0 triggers
        let small = vec![(d("500"), 2)];
        assert!(plan_ladder_refill(&small, &[1], 4).is_empty());
        assert_eq!(plan_ladder_refill(&small, &[0], 4), vec![(d("500"), 2)]);

        // divisor 1 restores the old eager behavior (any deficit splits)…
        assert_eq!(
            plan_ladder_refill(&rungs, &[19, 10], 1),
            vec![(d("25"), 1)],
        );
        // …and divisor 0 clamps to 1 instead of dividing by zero
        assert_eq!(
            plan_ladder_refill(&rungs, &[19, 10], 0),
            vec![(d("25"), 1)],
        );
    }

    #[test]
    fn overfull_ladder_is_detected() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        let rungs = vec![(d("25"), 20), (d("100"), 10)]; // target 30

        assert!(!ladder_overfull(&rungs, &[20, 10])); // exactly full
        assert!(!ladder_overfull(&rungs, &[50, 9])); // 59 < 60
        // the runaway signature: one band starved, total ballooned
        assert!(ladder_overfull(&rungs, &[60, 0]));
        assert!(ladder_overfull(&rungs, &[50, 10])); // 60 >= 2x30
        assert!(!ladder_overfull(&[], &[])); // no ladder → never "overfull"
    }

    #[test]
    fn split_gate_budget_and_cooldown() {
        // Base far in the future so subtracting offsets never underflows the
        // platform Instant epoch.
        let now = Instant::now() + Duration::from_secs(7 * 24 * 3600);
        let ago = |secs: u64| now - Duration::from_secs(secs);
        let min_interval = Duration::from_secs(120);

        // no history → allowed
        assert_eq!(split_gate(&[], now, min_interval, 6), SplitGate::Allow);

        // last op 60s ago → cooldown
        assert_eq!(
            split_gate(&[ago(60)], now, min_interval, 6),
            SplitGate::Cooldown { secs_since_last: 60 },
        );

        // last op exactly at the interval → allowed
        assert_eq!(split_gate(&[ago(120)], now, min_interval, 6), SplitGate::Allow);

        // 6 ops inside the hour with budget 6 → exhausted (even though the
        // last one is past the cooldown)
        let six: Vec<Instant> = (0..6).map(|i| ago(300 + i * 300)).collect();
        assert_eq!(
            split_gate(&six, now, min_interval, 6),
            SplitGate::BudgetExhausted { ops_in_window: 6 },
        );

        // ops older than the window don't count toward the budget…
        let old: Vec<Instant> = (0..6).map(|i| ago(3700 + i * 300)).collect();
        assert_eq!(split_gate(&old, now, min_interval, 6), SplitGate::Allow);

        // …and a mix counts only the in-window ones
        let mut mixed = old.clone();
        mixed.push(ago(3599));
        mixed.push(ago(200));
        assert_eq!(split_gate(&mixed, now, min_interval, 2), SplitGate::BudgetExhausted { ops_in_window: 2 });
        assert_eq!(split_gate(&mixed, now, min_interval, 6), SplitGate::Allow);

        // budget outranks cooldown: both tripped → BudgetExhausted (loud)
        let recent: Vec<Instant> = vec![ago(10), ago(400)];
        assert_eq!(
            split_gate(&recent, now, min_interval, 2),
            SplitGate::BudgetExhausted { ops_in_window: 2 },
        );
    }
}
