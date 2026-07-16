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
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

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
            config, cache, &mut atomic_client, &target.instrument, &rungs,
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
pub(crate) async fn ensure_denominations(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    atomic_client: &mut AtomicProviderClient,
    instrument: &SplitInstrument,
    rungs: &[(Decimal, u32)],
) -> Result<()> {
    let selectable = cache.get_selectable(&instrument.key, true).await;

    // Deficit per rung: want `count` holdings in [denom, 2*denom)
    let mut deficits: Vec<(Decimal, u32)> = Vec::new();
    for (denom, count) in rungs {
        let have = selectable
            .iter()
            .filter(|h| h.amount >= *denom && h.amount < *denom * Decimal::TWO)
            .count() as u32;
        if have < *count {
            deficits.push((*denom, count - have));
        }
    }
    if deficits.is_empty() {
        return Ok(());
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

    let result: Result<String> = if instrument.is_cc {
        split_cc(config, &input_cids, &splits).await
    } else {
        split_utility(config, atomic_client, instrument, &input_cids, &splits).await
    };

    match result {
        Ok(update_id) => {
            // Input consumed; outputs enter the cache via the updates watcher /
            // ACS refresh (the atomic response carries no amounts).
            cache.mark_consumed(&input_cids, &update_id).await;
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
}
