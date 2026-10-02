//! ACS worker — periodically refreshes the holdings cache from the ledger
//!
//! Runs every 30 seconds, queries the DAppProviderService for unlocked CC
//! amulets AND CIP-56 Holdings in ONE `GetActiveContracts` call, and refreshes
//! the shared `HoldingsCache` (authoritative reconciliation; the updates
//! watcher is the fast path). Also cleans up expired reservations each cycle.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::BaseConfig;
use agent_logic::liquidity::LiquidityManager;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Bounded, Policy};
use orderbook_proto::ledger::ActiveContractInfo;

use crate::holdings_cache::{
    instrument_key, CachedAmulet, CachedHolding, HoldingsCache, CC_INSTRUMENT, TEMPLATE_AMULET,
    TEMPLATE_HOLDING,
};
use crate::ledger_client::{self, DAppProviderClient};

/// ACS refresh interval
const REFRESH_INTERVAL_SECS: u64 = 30;

/// Minimum request timeout for the streaming ACS snapshot fetch. The
/// channel-wide request timeout (default 120s) bounds the WHOLE streamed
/// response, so a large ACS could time out mid-stream on every refresh —
/// and a truncated snapshot must never drive eviction (it once evicted the
/// entire denomination ladder every tick: the mainnet split storm).
const ACS_REQUEST_TIMEOUT_SECS: u64 = 600;

/// Snapshot request timeout of the worker's client.
fn acs_request_timeout_secs(config: &BaseConfig) -> u64 {
    config.request_timeout_secs.max(ACS_REQUEST_TIMEOUT_SECS)
}

/// Bound on one fetch: a connect plus the snapshot stream, or the fallback's
/// two calls; above the client's own bounds for each.
fn fetch_budget(config: &BaseConfig) -> Duration {
    let request = acs_request_timeout_secs(config);
    ledger_client::connect_wait(config.connection_timeout_secs, request)
        .saturating_add(ledger_client::stream_wait(request))
}

/// Spawn the ACS worker background task; it restarts if it fails.
pub fn spawn_acs_worker(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    liquidity_manager: Arc<LiquidityManager>,
    shutdown: Shutdown,
) -> anyhow::Result<()> {
    let s = shutdown.clone();
    supervise::spawn_supervised("ACS worker", shutdown, Policy::Restart, move || {
        run(config.clone(), cache.clone(), liquidity_manager.clone(), s.clone())
    })?;
    Ok(())
}

async fn run(
    config: BaseConfig,
    cache: Arc<HoldingsCache>,
    liquidity_manager: Arc<LiquidityManager>,
    shutdown: Shutdown,
) {
    info!("ACS worker started (refresh every {}s)", REFRESH_INTERVAL_SECS);
    let budget = fetch_budget(&config);
    let rate_wait = ledger_client::call_wait(acs_request_timeout_secs(&config));

    loop {
        if shutdown.is_shutting_down() {
            info!("ACS worker shutting down");
            return;
        }

        let apply = |(mut client, fetched): (DAppProviderClient, Fetched)| {
            let (config, cache, lm, shutdown) = (&config, &cache, &liquidity_manager, &shutdown);
            async move {
                apply_holdings(config, cache, lm, fetched).await;
                refresh_cc_usd_rate(&mut client, lm, shutdown, rate_wait).await;
            }
        };
        if !cycle(&shutdown, budget, fetch_holdings(&config), apply, &cache).await {
            info!("ACS worker shutting down");
            return;
        }

        if shutdown.sleep(Duration::from_secs(REFRESH_INTERVAL_SECS)).await {
            info!("ACS worker shutting down");
            return;
        }
    }
}

/// One fetch within `budget`; its result is applied unbounded, as that changes the cache.
/// Reservation cleanup then runs on every outcome except shutdown, which returns false.
async fn cycle<T, F, A, Fut>(shutdown: &Shutdown, budget: Duration, fetch: F, apply: A, cache: &HoldingsCache) -> bool
where
    F: Future<Output = anyhow::Result<T>>,
    A: FnOnce(T) -> Fut,
    Fut: Future<Output = ()>,
{
    match supervise::bounded(shutdown, budget, fetch).await {
        Bounded::Done(Ok(fetched)) => apply(fetched).await,
        Bounded::Done(Err(e)) => warn!("ACS worker refresh failed: {:#}", e),
        Bounded::Elapsed => warn!(
            "ACS worker refresh failed: no result within {}s, retrying next cycle",
            budget.as_secs()
        ),
        Bounded::Shutdown => return false,
    }
    cache.cleanup_expired_reservations().await;
    true
}

/// What one fetch read from the ledger.
enum Fetched {
    /// The snapshot, whether it is complete, and when it started.
    Snapshot {
        contracts: Vec<ActiveContractInfo>,
        complete: bool,
        started: Instant,
    },
    /// The amulet-only fallback, after the snapshot call failed.
    Amulets(Vec<AmuletInfo>),
}

/// Fetch amulets + CIP-56 holdings from the ledger. It changes nothing, so
/// it may be dropped at any point.
async fn fetch_holdings(config: &BaseConfig) -> anyhow::Result<(DAppProviderClient, Fetched)> {
    let mut client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        // Streaming ACS snapshots need headroom beyond the channel default —
        // this worker's client is only used for the refresh RPCs.
        Some(acs_request_timeout_secs(config)),
    )
    .await?;

    // Snapshot start marks the merge boundary: cache entries discovered after
    // this instant (updates watcher / own tx results racing the snapshot)
    // survive the refresh.
    let started = Instant::now();
    let fetched = match client
        .get_active_contracts_partial(&[TEMPLATE_AMULET.to_string(), TEMPLATE_HOLDING.to_string()])
        .await
    {
        Ok((contracts, complete)) => Fetched::Snapshot { contracts, complete, started },
        Err(e) => {
            // CC fallback: the lightweight GetAmulets RPC (no blobs — CC never
            // needs blobs for v1; V2 CC disclosure waits for the next full refresh)
            warn!(
                "holdings refresh failed ({:#}); using amulet-only fallback this cycle",
                e
            );
            Fetched::Amulets(client.get_amulets().await?)
        }
    };
    Ok((client, fetched))
}

/// Merge a fetch into the cache, then update the liquidity manager's CC
/// balance. In-memory only.
async fn apply_holdings(
    config: &BaseConfig,
    cache: &Arc<HoldingsCache>,
    lm: &LiquidityManager,
    fetched: Fetched,
) {
    let cc_refreshed = match fetched {
        Fetched::Snapshot { contracts, complete, started } => {
            let holdings = parse_acs_holdings(contracts, &config.party_id);
            debug!(
                "ACS worker: fetched {} holdings (complete={})",
                holdings.len(),
                complete
            );
            if complete {
                cache.refresh_from_acs_snapshot(holdings, started).await;
                // A complete snapshot supersedes the optimistic post-split
                // rungs recorded before it started (they are either in
                // `available` now or never materialized).
                cache.clear_pending_splits_before(started).await;
                true
            } else {
                // Truncated snapshot: the contracts we DID receive exist on
                // the ledger — merge them additively (backfills blobs and
                // refreshes TTLs) but never evict from partial data.
                warn!(
                    "ACS worker: snapshot incomplete ({} holdings) — merging additively, skipping eviction",
                    holdings.len()
                );
                cache.add_created(holdings).await;
                // A partial view must not stamp the CC balance as current.
                false
            }
        }
        Fetched::Amulets(amulets) => {
            let now = Instant::now();
            let cached: Vec<CachedAmulet> = amulets
                .into_iter()
                .map(|a| CachedAmulet {
                    contract_id: a.contract_id,
                    amount: a.amount,
                    discovered_at: now,
                })
                .collect();
            debug!("ACS worker: fetched {} amulets (fallback)", cached.len());
            cache.cc().refresh_from_acs(cached).await;
            true
        }
    };

    // Update liquidity manager with total CC (available minus consumed minus
    // V2-reserved; v1-reserved amulets are still on the ledger and their
    // commitment is tracked separately by LM). Only after a full snapshot or
    // a successful amulet refresh — never from a partial view.
    if cc_refreshed {
        let total_cc = cache.total_available_amount(CC_INSTRUMENT).await;
        lm.update_cc_balance(total_cc).await;
    }
}

/// Update the CC/USD rate for fee estimation, waiting at most `wait`.
async fn refresh_cc_usd_rate(
    client: &mut DAppProviderClient,
    lm: &LiquidityManager,
    shutdown: &Shutdown,
    wait: Duration,
) {
    match supervise::bounded(shutdown, wait, client.get_dso_rates()).await {
        Bounded::Done(Ok(rates)) => {
            if let Ok(rate) = rates.cc_usd_rate.parse::<Decimal>() {
                if rate > Decimal::ZERO {
                    lm.update_cc_usd_rate(rate).await;
                }
            }
        }
        Bounded::Done(Err(e)) => debug!("ACS worker: failed to fetch CC/USD rate: {:#}", e),
        Bounded::Elapsed => debug!(
            "ACS worker: failed to fetch CC/USD rate: no answer within {}s",
            wait.as_secs()
        ),
        Bounded::Shutdown => {}
    }
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

    async fn cache_with_expired_reservation() -> Arc<HoldingsCache> {
        let cache = HoldingsCache::new(false);
        let now = std::time::Instant::now();
        assert!(cache.reserve_v2(&["00gone".to_string()], "q", now).await);
        assert_eq!(cache.stats(CC_INSTRUMENT).await.2, 1);
        cache
    }

    fn nothing_to_apply(_: ()) -> std::future::Ready<()> {
        std::future::ready(())
    }

    // A refresh that never answers ends at the budget; the cleanup still runs
    #[tokio::test(start_paused = true)]
    async fn a_hung_refresh_is_bounded_and_the_cleanup_still_runs() {
        let cache = cache_with_expired_reservation().await;
        let hung = std::future::pending::<anyhow::Result<()>>();
        let ran = tokio::time::timeout(
            Duration::from_secs(3600),
            cycle(&Shutdown::new(), Duration::from_secs(60), hung, nothing_to_apply, &cache),
        )
        .await
        .expect("the budget ends the refresh");
        assert!(ran);
        assert_eq!(cache.stats(CC_INSTRUMENT).await.2, 0, "expired reservation cleaned");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_ends_a_refresh_in_progress() {
        let cache = cache_with_expired_reservation().await;
        let shutdown = Shutdown::new();
        shutdown.signal();
        let hung = std::future::pending::<anyhow::Result<()>>();
        assert!(!cycle(&shutdown, Duration::MAX, hung, nothing_to_apply, &cache).await);
    }

    // The cache update used to share the fetch budget, which could stop it halfway
    #[tokio::test(start_paused = true)]
    async fn the_cache_update_runs_outside_the_fetch_budget() {
        let cache = cache_with_expired_reservation().await;
        let applied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = applied.clone();
        let fetched = async { Ok::<_, anyhow::Error>(()) };
        let apply = |()| async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        assert!(cycle(&Shutdown::new(), Duration::from_secs(60), fetched, apply, &cache).await);
        assert!(applied.load(std::sync::atomic::Ordering::SeqCst), "the update ran to its end");
        assert_eq!(cache.stats(CC_INSTRUMENT).await.2, 0, "the cleanup ran after it");
    }

    // The budget used to leave out the stream open deadline, so a slow open
    // followed by a long stream dropped the partial snapshot every cycle
    #[test]
    fn the_fetch_budget_covers_the_client_bounds() {
        let mut config = BaseConfig::test_minimal().unwrap();
        for connect_secs in [1, 30, 300] {
            for request in [30, 120, 600, 3600] {
                config.connection_timeout_secs = connect_secs;
                config.request_timeout_secs = request;
                let r = acs_request_timeout_secs(&config);
                let connect = Duration::from_secs(connect_secs + 15);
                let (open, stream_total) = (Duration::from_secs(r + 5), Duration::from_secs(r + 30));
                let budget = fetch_budget(&config);
                assert!(budget > connect + open + stream_total, "connect {connect_secs} request {request}");
                assert!(budget > connect + open + open, "the amulet fallback fits too");
            }
        }
    }

    #[test]
    fn spawning_outside_a_runtime_is_an_error() {
        let config = BaseConfig::test_minimal().unwrap();
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        assert!(spawn_acs_worker(config, HoldingsCache::new(false), lm, Shutdown::new()).is_err());
    }
}
