//! ACS worker — periodically refreshes the amulet cache from the ledger
//!
//! Runs every 30 seconds, queries the DAppProviderService for unlocked amulet
//! contracts, and refreshes the `AmuletCache` available pool.
//! Also cleans up expired reservations on each cycle.

use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use orderbook_agent_logic::config::BaseConfig;
use orderbook_agent_logic::liquidity::LiquidityManager;

use crate::amulet_cache::{AmuletCache, CachedAmulet};
use crate::ledger_client::DAppProviderClient;

/// ACS refresh interval
const REFRESH_INTERVAL_SECS: u64 = 30;

/// Spawn the ACS worker background task
pub fn spawn_acs_worker(config: BaseConfig, cache: Arc<AmuletCache>, liquidity_manager: Arc<LiquidityManager>) {
    tokio::spawn(async move {
        info!("ACS worker started (refresh every {}s)", REFRESH_INTERVAL_SECS);

        // Initial delay to let connections establish
        tokio::time::sleep(Duration::from_secs(5)).await;

        loop {
            if let Err(e) = refresh_amulets(&config, &cache, &liquidity_manager).await {
                warn!("ACS worker refresh failed: {:#}", e);
            }

            cache.cleanup_expired_reservations().await;

            tokio::time::sleep(Duration::from_secs(REFRESH_INTERVAL_SECS)).await;
        }
    });
}

/// Fetch amulets from ledger, refresh cache, and update liquidity manager
async fn refresh_amulets(config: &BaseConfig, cache: &Arc<AmuletCache>, lm: &Arc<LiquidityManager>) -> anyhow::Result<()> {
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

    // Try GetAmulets RPC first (lightweight), fall back to GetActiveContracts
    let amulets = match client.get_amulets().await {
        Ok(amulets) => amulets,
        Err(e) => {
            debug!("GetAmulets RPC not available ({}), falling back to GetActiveContracts", e);
            client.get_amulets_via_acs().await?
        }
    };

    let now = std::time::Instant::now();
    let cached: Vec<CachedAmulet> = amulets
        .into_iter()
        .map(|a| CachedAmulet {
            contract_id: a.contract_id,
            amount: a.amount,
            discovered_at: now,
        })
        .collect();

    debug!("ACS worker: fetched {} amulets", cached.len());
    cache.refresh_from_acs(cached).await;

    // Update liquidity manager with selectable CC total
    let selectable: Decimal = cache.get_selectable_amulets().await
        .iter()
        .map(|a| a.amount)
        .sum();
    lm.update_cc_balance(selectable).await;

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

/// Simple amulet info returned from ledger queries
pub struct AmuletInfo {
    pub contract_id: String,
    pub amount: Decimal,
}
