//! ACS worker — periodically refreshes the holdings cache from the ledger
//!
//! Runs every 30 seconds, queries the DAppProviderService for unlocked CC
//! amulets AND CIP-56 Holdings in ONE `GetActiveContracts` call, and refreshes
//! the shared `HoldingsCache` (authoritative reconciliation; the updates
//! watcher is the fast path). Also cleans up expired reservations each cycle.

use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::liquidity::LiquidityManager;
use agent_logic::shutdown::Shutdown;

use crate::holdings_cache::{
    instrument_key, CachedHolding, HoldingsCache, CC_INSTRUMENT, TEMPLATE_AMULET, TEMPLATE_HOLDING,
};
use crate::ledger_client::DAppProviderClient;

/// ACS refresh interval
const REFRESH_INTERVAL_SECS: u64 = 30;

/// Minimum request timeout for the streaming ACS snapshot fetch. The
/// channel-wide request timeout (default 120s) bounds the WHOLE streamed
/// response, so a large ACS could time out mid-stream on every refresh —
/// and a truncated snapshot must never drive eviction (it once evicted the
/// entire denomination ladder every tick: the mainnet split storm).
const ACS_REQUEST_TIMEOUT_SECS: u64 = 600;

/// Spawn the ACS worker background task
pub fn spawn_acs_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    liquidity_manager: Arc<LiquidityManager>,
    shutdown: Shutdown,
) {
    tokio::spawn(async move {
        info!("ACS worker started (refresh every {}s)", REFRESH_INTERVAL_SECS);

        loop {
            if shutdown.is_shutting_down() {
                info!("ACS worker shutting down");
                return;
            }

            if let Err(e) = refresh_holdings(&config, &cache, &liquidity_manager).await {
                warn!("ACS worker refresh failed: {:#}", e);
            }

            cache.cleanup_expired_reservations().await;

            if shutdown.sleep(Duration::from_secs(REFRESH_INTERVAL_SECS)).await {
                info!("ACS worker shutting down");
                return;
            }
        }
    });
}

/// Fetch amulets + CIP-56 holdings from the ledger, refresh the cache, and
/// update the liquidity manager's CC balance + CC/USD rate.
async fn refresh_holdings(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    lm: &Arc<LiquidityManager>,
) -> anyhow::Result<()> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key_bytes,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        // Streaming ACS snapshots need headroom beyond the channel default —
        // this worker's client is only used for the refresh RPCs.
        Some(config.request_timeout_secs.max(ACS_REQUEST_TIMEOUT_SECS)),
    )
    .await?;

    // Snapshot start marks the merge boundary: cache entries discovered after
    // this instant (updates watcher / own tx results racing the snapshot)
    // survive the refresh.
    let snapshot_start = std::time::Instant::now();

    match client
        .get_active_contracts_partial(&[TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()])
        .await
    {
        Ok((contracts, complete)) => {
            let holdings = parse_acs_holdings(contracts, &config.party_id);
            debug!(
                "ACS worker: fetched {} holdings (complete={})",
                holdings.len(),
                complete
            );
            if complete {
                cache.refresh_from_acs_snapshot(holdings, snapshot_start).await;
                // A complete snapshot supersedes the optimistic post-split
                // rungs recorded before it started (they are either in
                // `available` now or never materialized).
                cache.clear_pending_splits_before(snapshot_start).await;
            } else {
                // Truncated snapshot: the contracts we DID receive exist on
                // the ledger — merge them additively (backfills blobs and
                // refreshes TTLs) but never evict from partial data.
                warn!(
                    "ACS worker: snapshot incomplete ({} holdings) — merging additively, skipping eviction",
                    holdings.len()
                );
                cache.add_created(holdings).await;
            }
        }
        Err(e) => {
            // CC fallback: the lightweight GetAmulets RPC (no blobs — CC never
            // needs blobs for v1; V2 CC disclosure waits for the next full refresh)
            debug!(
                "GetActiveContracts holdings refresh failed ({}), falling back to GetAmulets",
                e
            );
            let amulets = client.get_amulets().await?;
            let now = std::time::Instant::now();
            let cached: Vec<crate::holdings_cache::CachedAmulet> = amulets
                .into_iter()
                .map(|a| crate::holdings_cache::CachedAmulet {
                    contract_id: a.contract_id,
                    amount: a.amount,
                    discovered_at: now,
                })
                .collect();
            debug!("ACS worker: fetched {} amulets (fallback)", cached.len());
            cache.cc().refresh_from_acs(cached).await;
        }
    }

    // Update liquidity manager with total CC (available minus consumed minus
    // V2-reserved; v1-reserved amulets are still on the ledger and their
    // commitment is tracked separately by LM).
    let total_cc = cache.total_available_amount(CC_INSTRUMENT).await;
    lm.update_cc_balance(total_cc).await;

    // Update CC/USD rate for fee estimation
    match client.get_dso_rates().await {
        Ok(rates) => {
            if let Ok(rate) = rates.cc_usd_rate.parse::<Decimal>() {
                if rate > Decimal::ZERO {
                    lm.update_cc_usd_rate(rate).await;
                }
            }
        }
        Err(e) => {
            debug!("ACS worker: failed to fetch CC/USD rate: {:#}", e);
        }
    }

    Ok(())
}

/// Parse a mixed Amulet + Holding ACS snapshot into cache entries.
/// Amulet: skip Locked templates, amount = `amount.initialAmount`.
/// Holding: `owner == party` and `lock` null; amount = `amount`;
/// instrument = `instrument.{source,id}` (`InstrumentIdentifier`: admin lives
/// in `source`).
pub fn parse_acs_holdings(
    contracts: Vec<orderbook_proto::ledger::ActiveContractInfo>,
    party_id: &str,
) -> Vec<CachedHolding> {
    let now = std::time::Instant::now();
    contracts
        .into_iter()
        .filter_map(|c| {
            let args = c.create_arguments.as_ref()?;
            let json = crate::prost_struct_to_json(args);
            let blob = (!c.created_event_blob.is_empty()).then(|| c.created_event_blob.clone());

            if c.template_id.contains("Splice.Amulet:Amulet")
                && !c.template_id.contains("Locked")
            {
                let amount: Decimal = json
                    .pointer("/amount/initialAmount")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())?;
                Some(CachedHolding {
                    contract_id: c.contract_id,
                    template_id: c.template_id,
                    instrument: CC_INSTRUMENT.to_string(),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id: c.synchronizer_id,
                    discovered_at: now,
                })
            } else if c.template_id.contains("Utility.Registry.Holding.V0.Holding:Holding") {
                let owner_ok = json.get("owner").and_then(|o| o.as_str()) == Some(party_id);
                let lock = json.pointer("/lock");
                let unlocked = lock.is_none() || lock.is_some_and(|l| l.is_null());
                if !owner_ok || !unlocked {
                    return None;
                }
                let amount: Decimal = json
                    .get("amount")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())?;
                let admin = json.pointer("/instrument/source").and_then(|v| v.as_str())?;
                let id = json.pointer("/instrument/id").and_then(|v| v.as_str())?;
                Some(CachedHolding {
                    contract_id: c.contract_id,
                    template_id: c.template_id,
                    instrument: instrument_key(admin, id),
                    amount,
                    created_event_blob: blob,
                    synchronizer_id: c.synchronizer_id,
                    discovered_at: now,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Simple amulet info returned from ledger queries
pub struct AmuletInfo {
    pub contract_id: String,
    pub amount: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_prost_value(v: &serde_json::Value) -> prost_types::Value {
        use prost_types::value::Kind;
        let kind = match v {
            serde_json::Value::Null => Kind::NullValue(0),
            serde_json::Value::Bool(b) => Kind::BoolValue(*b),
            serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap()),
            serde_json::Value::String(s) => Kind::StringValue(s.clone()),
            serde_json::Value::Array(a) => Kind::ListValue(prost_types::ListValue {
                values: a.iter().map(to_prost_value).collect(),
            }),
            serde_json::Value::Object(_) => Kind::StructValue(to_prost_struct(v)),
        };
        prost_types::Value { kind: Some(kind) }
    }

    fn to_prost_struct(v: &serde_json::Value) -> prost_types::Struct {
        prost_types::Struct {
            fields: v
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), to_prost_value(v)))
                .collect(),
        }
    }

    fn holding_contract(payload: serde_json::Value) -> orderbook_proto::ledger::ActiveContractInfo {
        orderbook_proto::ledger::ActiveContractInfo {
            contract_id: "00cid".to_string(),
            template_id:
                "112742269c282ab77490b7933f65582bc223e3bf6c120d81e0799cf0d99ecd9e:Utility.Registry.Holding.V0.Holding:Holding"
                    .to_string(),
            entity_name: "Holding".to_string(),
            create_arguments: Some(to_prost_struct(&payload)),
            created_event_blob: "blob".to_string(),
            synchronizer_id: "sync".to_string(),
            network_id: String::new(),
        }
    }

    /// Real devnet Holding shape: `instrument: {id, scheme, source}` (the
    /// admin party lives in `source`); unlocked holdings omit `lock`.
    #[test]
    fn parses_real_holding_shape() {
        let lp = "lp::1220aa";
        let unlocked = holding_contract(serde_json::json!({
            "operator": "op::1220bb",
            "provider": "reg::1220cc",
            "registrar": "reg::1220cc",
            "owner": lp,
            "instrument": {
                "id": "USDC",
                "scheme": "RegistrarInternalScheme",
                "source": "test-token-1::122034"
            },
            "label": "",
            "amount": "1234.0000000000"
        }));
        let locked = holding_contract(serde_json::json!({
            "operator": "op::1220bb",
            "provider": "reg::1220cc",
            "registrar": "reg::1220cc",
            "owner": lp,
            "instrument": {
                "id": "USDC",
                "scheme": "RegistrarInternalScheme",
                "source": "test-token-1::122034"
            },
            "label": "",
            "amount": "1.6101587500",
            "lock": { "context": "alloc", "lockers": { "map": [] } }
        }));
        let other_owner = holding_contract(serde_json::json!({
            "owner": "someone-else::1220dd",
            "instrument": { "id": "USDC", "scheme": "RegistrarInternalScheme", "source": "test-token-1::122034" },
            "amount": "9.0"
        }));

        let parsed = parse_acs_holdings(vec![unlocked, locked, other_owner], lp);
        assert_eq!(parsed.len(), 1, "only the unlocked own holding survives");
        assert_eq!(parsed[0].instrument, "test-token-1::122034::USDC");
        assert_eq!(parsed[0].amount, Decimal::new(1234, 0));
    }
}
