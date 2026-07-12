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
        Some(config.request_timeout_secs),
    )
    .await?;

    // Snapshot start marks the merge boundary: cache entries discovered after
    // this instant (updates watcher / own tx results racing the snapshot)
    // survive the refresh.
    let snapshot_start = std::time::Instant::now();

    match client
        .get_active_contracts(&[TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()])
        .await
    {
        Ok(contracts) => {
            let holdings = parse_acs_holdings(contracts, &config.party_id);
            debug!("ACS worker: fetched {} holdings", holdings.len());
            cache.refresh_from_acs_snapshot(holdings, snapshot_start).await;
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
/// instrument = `instrumentId.{admin,id}`.
pub(crate) fn parse_acs_holdings(
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
                let admin = json.pointer("/instrumentId/admin").and_then(|v| v.as_str())?;
                let id = json.pointer("/instrumentId/id").and_then(|v| v.as_str())?;
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
