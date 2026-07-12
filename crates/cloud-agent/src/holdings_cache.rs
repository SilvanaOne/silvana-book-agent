//! Multi-instrument holdings cache — the ONE shared pool used by RFQ v1
//! (CC amulets, via the [`CcView`] handle) and RFQ V2 (any instrument,
//! via [`HoldingsCache::select_for_disclosure`]). Generalization of the old
//! `AmuletCache` three-pool structure:
//!
//! - **available**: holdings known from ACS / updates watcher / own tx results
//! - **consumed**: holdings used in committed transactions (until ACS confirms gone)
//! - **reserved**: holdings tentatively assigned to in-flight work, with a
//!   per-reservation expiry ([`ReservationKind`])
//!
//! Per instrument, the single largest available holding is the **splitter
//! reserve** (only when enabled, i.e. RFQ V2 mode): excluded from V2 disclosure
//! selection unconditionally and from v1 selection with fail-open (v1 settle
//! success outranks reserve preservation), consumed only by the split worker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// TTL for available holdings (refreshed by ACS worker every 30s)
const AVAILABLE_TTL_SECS: u64 = 180; // 3 minutes

/// TTL for v1 payment reservations (workers should complete within this time)
const V1_RESERVATION_TTL_SECS: u64 = 60;

/// TTL for split-job reservations
const SPLIT_RESERVATION_TTL_SECS: u64 = 300;

/// CIP-56 Holding template as uploaded on this participant (matches the SDK's
/// `transactions::TEMPLATE_HOLDING`).
pub const TEMPLATE_HOLDING: &str =
    "#utility-registry-holding-v0:Utility.Registry.Holding.V0.Holding:Holding";

/// Canton Coin amulet template.
pub const TEMPLATE_AMULET: &str = "#splice-amulet:Splice.Amulet:Amulet";

/// Instrument pool key: `"CC"` for Canton Coin, `"{admin}::{id}"` otherwise.
pub type InstrumentKey = String;

/// The CC instrument key.
pub const CC_INSTRUMENT: &str = "CC";

/// Build the instrument key for a utility instrument.
pub fn instrument_key(admin: &str, id: &str) -> InstrumentKey {
    format!("{}::{}", admin, id)
}

/// A cached holding (CC amulet or CIP-56 Holding).
#[derive(Debug, Clone)]
pub struct CachedHolding {
    pub contract_id: String,
    pub template_id: String,
    pub instrument: InstrumentKey,
    /// CC: `amount.initialAmount`; utility: `amount`
    pub amount: Decimal,
    /// None = blob-pending (watcher-discovered / own-tx-created); such holdings
    /// are selectable by v1 (never needs blobs) but not disclosable by V2 until
    /// the next ACS refresh backfills the blob.
    pub created_event_blob: Option<String>,
    pub synchronizer_id: String,
    pub discovered_at: Instant,
}

/// Legacy CC view of a holding — kept so v1 call sites migrate by renaming only.
#[derive(Debug, Clone)]
pub struct CachedAmulet {
    pub contract_id: String,
    pub amount: Decimal,
    pub discovered_at: Instant,
}

/// Why a holding is reserved, with per-kind expiry.
#[derive(Debug, Clone)]
pub enum ReservationKind {
    /// v1 payment queue / multicall settle (fixed 60 s TTL, as before)
    V1Payment { payment_id: String },
    /// V2 hard reserve backing a signed quote (TTL = valid_until + grace)
    V2Quote { quote_id: String, expires_at: Instant },
    /// Split worker job input
    Split { job_id: String },
}

#[derive(Debug, Clone)]
struct ConsumedEntry {
    #[allow(dead_code)]
    reason: String,
    #[allow(dead_code)]
    consumed_at: Instant,
}

#[derive(Debug, Clone)]
struct ReservedEntry {
    kind: ReservationKind,
    reserved_at: Instant,
}

impl ReservedEntry {
    fn is_expired(&self, now: Instant) -> bool {
        match &self.kind {
            ReservationKind::V1Payment { .. } => {
                now.duration_since(self.reserved_at).as_secs() > V1_RESERVATION_TTL_SECS
            }
            ReservationKind::V2Quote { expires_at, .. } => now >= *expires_at,
            ReservationKind::Split { .. } => {
                now.duration_since(self.reserved_at).as_secs() > SPLIT_RESERVATION_TTL_SECS
            }
        }
    }
}

/// Multi-instrument three-pool cache. Pools are flat maps keyed by contract id;
/// the instrument partition lives on each [`CachedHolding`].
pub struct HoldingsCache {
    available: RwLock<HashMap<String, CachedHolding>>,
    consumed: RwLock<HashMap<String, ConsumedEntry>>,
    reserved: RwLock<HashMap<String, ReservedEntry>>,
    /// Splitter reserve active only in RFQ V2 mode — otherwise v1 behavior is
    /// unchanged (no permanent exclusion of the largest amulet).
    splitter_reserve_enabled: bool,
}

impl HoldingsCache {
    pub fn new(splitter_reserve_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            available: RwLock::new(HashMap::new()),
            consumed: RwLock::new(HashMap::new()),
            reserved: RwLock::new(HashMap::new()),
            splitter_reserve_enabled,
        })
    }

    /// The exact legacy `AmuletCache` method surface, scoped to CC.
    pub fn cc(self: &Arc<Self>) -> CcView {
        CcView(self.clone())
    }

    /// Refresh from a full ACS snapshot covering ALL tracked instruments.
    ///
    /// MERGE, not replace: entries discovered after `snapshot_start` (watcher /
    /// own-tx additions racing the snapshot) are kept even if absent from the
    /// snapshot. Consumed entries no longer in the ACS are pruned (the ledger
    /// has fully processed them). Blobs backfill onto blob-pending entries.
    pub async fn refresh_from_acs_snapshot(
        &self,
        holdings: Vec<CachedHolding>,
        snapshot_start: Instant,
    ) {
        let new_available: HashMap<String, CachedHolding> = holdings
            .into_iter()
            .map(|h| (h.contract_id.clone(), h))
            .collect();
        let new_count = new_available.len();

        {
            let mut consumed = self.consumed.write().await;
            let before = consumed.len();
            consumed.retain(|cid, _| new_available.contains_key(cid));
            let removed = before - consumed.len();
            if removed > 0 {
                debug!("Cleaned {} consumed entries no longer in ACS", removed);
            }
        }

        {
            let mut available = self.available.write().await;
            available.retain(|_, h| h.discovered_at > snapshot_start);
            let kept_recent = available.len();
            for (cid, h) in new_available {
                // The ACS snapshot is authoritative (and carries blobs) — it
                // wins over any racing watcher entry for the same cid.
                available.insert(cid, h);
            }
            if kept_recent > 0 {
                debug!(
                    "ACS refresh: kept {} entr(ies) discovered after snapshot start",
                    kept_recent
                );
            }
        }

        debug!("ACS refresh: {} holdings available", new_count);
    }

    /// Selectable holdings for one instrument: available − consumed − reserved
    /// (unexpired), sorted ascending by amount. `exclude_splitter_reserve`
    /// removes the single largest available holding of the instrument.
    pub async fn get_selectable(
        &self,
        instrument: &str,
        exclude_splitter_reserve: bool,
    ) -> Vec<CachedHolding> {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;

        let splitter_cid = if exclude_splitter_reserve && self.splitter_reserve_enabled {
            Self::splitter_reserve_cid(&available, instrument)
        } else {
            None
        };

        let mut selectable: Vec<CachedHolding> = available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| {
                if now.duration_since(h.discovered_at).as_secs() > AVAILABLE_TTL_SECS {
                    return false;
                }
                if consumed.contains_key(&h.contract_id) {
                    return false;
                }
                if let Some(entry) = reserved.get(&h.contract_id) {
                    if !entry.is_expired(now) {
                        return false;
                    }
                }
                if Some(&h.contract_id) == splitter_cid.as_ref() {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        selectable.sort_by(|a, b| a.amount.cmp(&b.amount));
        selectable
    }

    /// The splitter-reserve holding (largest available, ignoring reservations)
    /// for an instrument, if the reserve is enabled.
    pub async fn splitter_reserve(&self, instrument: &str) -> Option<CachedHolding> {
        if !self.splitter_reserve_enabled {
            return None;
        }
        let available = self.available.read().await;
        let cid = Self::splitter_reserve_cid(&available, instrument)?;
        available.get(&cid).cloned()
    }

    fn splitter_reserve_cid(
        available: &HashMap<String, CachedHolding>,
        instrument: &str,
    ) -> Option<String> {
        available
            .values()
            .filter(|h| h.instrument == instrument)
            .max_by(|a, b| {
                a.amount
                    .cmp(&b.amount)
                    // deterministic tie-break so the reserve is stable
                    .then_with(|| a.contract_id.cmp(&b.contract_id))
            })
            .map(|h| h.contract_id.clone())
    }

    /// V2 disclosure selection (design §5.1). `target` = `amount` for utility
    /// instruments, `amount * 1.02 + 1` for CC (holding-fee decay margin).
    /// Smallest-single-fit, else greedy largest-first capped at `max_inputs`
    /// with a tighten-last pass. The splitter reserve is NEVER included.
    /// Every pick must carry a `created_event_blob` — blob-pending picks make
    /// the combination unavailable (None; the ACS refresh backfills blobs).
    /// The caller reserves the returned cids via [`Self::reserve_v2`].
    pub async fn select_for_disclosure(
        &self,
        instrument: &InstrumentKey,
        amount: Decimal,
        max_inputs: usize,
        is_cc: bool,
    ) -> Option<Vec<CachedHolding>> {
        if amount <= Decimal::ZERO || max_inputs == 0 {
            return None;
        }
        let target = if is_cc {
            amount * Decimal::new(102, 2) + Decimal::ONE
        } else {
            amount
        };

        let selectable = self.get_selectable(instrument, true).await;

        let picks = select_from(&selectable, target, max_inputs)?;
        if picks.iter().any(|h| h.created_event_blob.is_none()) {
            debug!(
                "select_for_disclosure({instrument}): combination includes blob-pending holdings — unavailable until ACS backfill"
            );
            return None;
        }
        Some(picks)
    }

    /// Reserve holdings for a V2 quote (all-or-nothing).
    pub async fn reserve_v2(
        &self,
        contract_ids: &[String],
        quote_id: &str,
        expires_at: Instant,
    ) -> bool {
        self.reserve_with_kind(
            contract_ids,
            ReservationKind::V2Quote {
                quote_id: quote_id.to_string(),
                expires_at,
            },
        )
        .await
    }

    /// Reserve holdings for a split job (all-or-nothing).
    pub async fn reserve_split(&self, contract_ids: &[String], job_id: &str) -> bool {
        self.reserve_with_kind(
            contract_ids,
            ReservationKind::Split {
                job_id: job_id.to_string(),
            },
        )
        .await
    }

    async fn reserve_with_kind(&self, contract_ids: &[String], kind: ReservationKind) -> bool {
        let now = Instant::now();
        let consumed = self.consumed.read().await;
        let mut reserved = self.reserved.write().await;

        for cid in contract_ids {
            if consumed.contains_key(cid) {
                debug!("Cannot reserve {}: already consumed", cid);
                return false;
            }
            if let Some(entry) = reserved.get(cid) {
                if !entry.is_expired(now) {
                    debug!("Cannot reserve {}: already reserved ({:?})", cid, entry.kind);
                    return false;
                }
            }
        }

        for cid in contract_ids {
            reserved.insert(
                cid.clone(),
                ReservedEntry {
                    kind: kind.clone(),
                    reserved_at: now,
                },
            );
        }
        debug!("Reserved {} holdings ({:?})", contract_ids.len(), kind);
        true
    }

    /// Restore a V2 reservation from saved state (restart inside the signed
    /// validity window). Unconditional — the cids may not be in `available`
    /// yet (first ACS refresh hasn't run); the reservation must exist BEFORE it.
    pub async fn restore_v2_reservation(
        &self,
        contract_ids: &[String],
        quote_id: &str,
        expires_at: Instant,
    ) {
        let now = Instant::now();
        let mut reserved = self.reserved.write().await;
        for cid in contract_ids {
            reserved.insert(
                cid.clone(),
                ReservedEntry {
                    kind: ReservationKind::V2Quote {
                        quote_id: quote_id.to_string(),
                        expires_at,
                    },
                    reserved_at: now,
                },
            );
        }
    }

    /// The quote_id of an unexpired V2 reservation on `cid`, if any.
    pub async fn v2_reservation_quote(&self, cid: &str) -> Option<String> {
        let now = Instant::now();
        let reserved = self.reserved.read().await;
        match reserved.get(cid) {
            Some(entry) if !entry.is_expired(now) => match &entry.kind {
                ReservationKind::V2Quote { quote_id, .. } => Some(quote_id.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Mark holdings as consumed (after a committed transaction or an observed
    /// archive). Moves them from reserved to consumed.
    pub async fn mark_consumed(&self, contract_ids: &[String], reason: &str) {
        let now = Instant::now();
        let mut consumed = self.consumed.write().await;
        let mut reserved = self.reserved.write().await;
        for cid in contract_ids {
            reserved.remove(cid);
            consumed.insert(
                cid.clone(),
                ConsumedEntry {
                    reason: reason.to_string(),
                    consumed_at: now,
                },
            );
        }
        debug!("Marked {} holdings as consumed ({})", contract_ids.len(), reason);
    }

    /// Release reservations (on failure / expiry / reject paths).
    pub async fn release_reservations(&self, contract_ids: &[String]) {
        let mut reserved = self.reserved.write().await;
        for cid in contract_ids {
            reserved.remove(cid);
        }
        debug!("Released {} holding reservations", contract_ids.len());
    }

    /// Add newly created holdings (own tx results / updates watcher).
    pub async fn add_created(&self, holdings: Vec<CachedHolding>) {
        if holdings.is_empty() {
            return;
        }
        let count = holdings.len();
        let mut available = self.available.write().await;
        for h in holdings {
            available.insert(h.contract_id.clone(), h);
        }
        debug!("Added {} newly created holdings to cache", count);
    }

    /// Drop expired reservations (called periodically by the ACS worker).
    pub async fn cleanup_expired_reservations(&self) {
        let now = Instant::now();
        let mut reserved = self.reserved.write().await;
        let before = reserved.len();
        reserved.retain(|_, entry| !entry.is_expired(now));
        let removed = before - reserved.len();
        if removed > 0 {
            info!("Cleaned up {} expired holding reservations", removed);
        }
    }

    /// Total available amount for one instrument, excluding consumed and
    /// excluding unexpired **V2** reservations (their outflow is committed —
    /// design §5.5 step 7: an amount must be excluded from availability exactly
    /// once). V1/Split reservations stay counted: those holdings are still on
    /// the ledger and their commitments are tracked by the LiquidityManager.
    pub async fn total_available_amount(&self, instrument: &str) -> Decimal {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;
        available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| !consumed.contains_key(&h.contract_id))
            .filter(|h| match reserved.get(&h.contract_id) {
                Some(entry) if !entry.is_expired(now) => {
                    !matches!(entry.kind, ReservationKind::V2Quote { .. })
                }
                _ => true,
            })
            .map(|h| h.amount)
            .sum()
    }

    /// Cache statistics for one instrument: (available, consumed, reserved, selectable).
    pub async fn stats(&self, instrument: &str) -> (usize, usize, usize, usize) {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;

        let avail_count = available
            .values()
            .filter(|h| h.instrument == instrument)
            .count();
        let selectable = available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| {
                if now.duration_since(h.discovered_at).as_secs() > AVAILABLE_TTL_SECS {
                    return false;
                }
                if consumed.contains_key(&h.contract_id) {
                    return false;
                }
                if let Some(entry) = reserved.get(&h.contract_id) {
                    if !entry.is_expired(now) {
                        return false;
                    }
                }
                true
            })
            .count();

        (avail_count, consumed.len(), reserved.len(), selectable)
    }

    /// Look up a cached holding by contract id.
    pub async fn get(&self, cid: &str) -> Option<CachedHolding> {
        self.available.read().await.get(cid).cloned()
    }
}

/// Smallest-single-fit, else greedy largest-first (capped) + tighten-last.
fn select_from(
    selectable_asc: &[CachedHolding],
    target: Decimal,
    max_inputs: usize,
) -> Option<Vec<CachedHolding>> {
    // Smallest single holding covering the target (ascending scan).
    for h in selectable_asc {
        if h.amount >= target {
            return Some(vec![h.clone()]);
        }
    }

    // Greedy largest-first, capped at max_inputs.
    let mut picks: Vec<CachedHolding> = Vec::new();
    let mut total = Decimal::ZERO;
    for h in selectable_asc.iter().rev() {
        if picks.len() >= max_inputs {
            break;
        }
        picks.push(h.clone());
        total += h.amount;
        if total >= target {
            break;
        }
    }
    if total < target {
        return None;
    }

    // Tighten-last: replace the last (smallest) pick with the smallest single
    // holding covering the residual — near-optimal at minimal count.
    if picks.len() >= 2 {
        let last = picks.last().cloned()
            .expect("picks.len() >= 2 checked above");
        let residual = target - (total - last.amount);
        let picked_cids: std::collections::HashSet<&str> =
            picks[..picks.len() - 1].iter().map(|h| h.contract_id.as_str()).collect();
        if let Some(replacement) = selectable_asc.iter().find(|h| {
            h.amount >= residual && !picked_cids.contains(h.contract_id.as_str())
        }) {
            let n = picks.len();
            picks[n - 1] = replacement.clone();
        }
    }

    Some(picks)
}

// ============================================================================
// CcView — the legacy AmuletCache surface (CC pool only)
// ============================================================================

/// CC-scoped handle over the shared [`HoldingsCache`], exposing the exact
/// legacy `AmuletCache` async method surface so v1 call sites migrate by
/// renaming only.
#[derive(Clone)]
pub struct CcView(Arc<HoldingsCache>);

impl CcView {
    pub fn inner(&self) -> &Arc<HoldingsCache> {
        &self.0
    }

    fn amulet_to_holding(a: CachedAmulet) -> CachedHolding {
        CachedHolding {
            contract_id: a.contract_id,
            template_id: TEMPLATE_AMULET.to_string(),
            instrument: CC_INSTRUMENT.to_string(),
            amount: a.amount,
            created_event_blob: None,
            synchronizer_id: String::new(),
            discovered_at: a.discovered_at,
        }
    }

    fn holding_to_amulet(h: CachedHolding) -> CachedAmulet {
        CachedAmulet {
            contract_id: h.contract_id,
            amount: h.amount,
            discovered_at: h.discovered_at,
        }
    }

    /// CC-only ACS refresh (fallback path — the multi-instrument snapshot
    /// refresh in acs_worker is the primary feed). Merge semantics: only the
    /// CC portion of the pool is replaced.
    pub async fn refresh_from_acs(&self, amulets: Vec<CachedAmulet>) {
        let snapshot_start = Instant::now();
        let now = Instant::now();
        let holdings: Vec<CachedHolding> = amulets
            .into_iter()
            .map(|mut a| {
                a.discovered_at = now;
                Self::amulet_to_holding(a)
            })
            .collect();
        // CC-only snapshot: preserve every non-CC entry regardless of age.
        let non_cc: Vec<CachedHolding> = {
            let available = self.0.available.read().await;
            available
                .values()
                .filter(|h| h.instrument != CC_INSTRUMENT)
                .cloned()
                .collect()
        };
        let mut all = holdings;
        all.extend(non_cc);
        self.0.refresh_from_acs_snapshot(all, snapshot_start).await;
    }

    /// Selectable CC amulets, ascending by amount. Excludes the splitter
    /// reserve when it is enabled — use
    /// [`Self::get_selectable_amulets_incl_reserve`] for the v1 fail-open retry.
    pub async fn get_selectable_amulets(&self) -> Vec<CachedAmulet> {
        self.0
            .get_selectable(CC_INSTRUMENT, true)
            .await
            .into_iter()
            .map(Self::holding_to_amulet)
            .collect()
    }

    /// Selectable CC amulets INCLUDING the splitter reserve. v1 fail-open: if
    /// a selection over `get_selectable_amulets` fails, retry over this set —
    /// v1 settle success outranks splitter-reserve preservation.
    pub async fn get_selectable_amulets_incl_reserve(&self) -> Vec<CachedAmulet> {
        self.0
            .get_selectable(CC_INSTRUMENT, false)
            .await
            .into_iter()
            .map(Self::holding_to_amulet)
            .collect()
    }

    pub async fn reserve(&self, contract_ids: &[String], payment_id: &str) -> bool {
        self.0
            .reserve_with_kind(
                contract_ids,
                ReservationKind::V1Payment {
                    payment_id: payment_id.to_string(),
                },
            )
            .await
    }

    pub async fn mark_consumed(&self, contract_ids: &[String], reason: &str) {
        self.0.mark_consumed(contract_ids, reason).await
    }

    pub async fn release_reservations(&self, contract_ids: &[String]) {
        self.0.release_reservations(contract_ids).await
    }

    pub async fn add_created_amulets(&self, amulets: Vec<CachedAmulet>) {
        self.0
            .add_created(amulets.into_iter().map(Self::amulet_to_holding).collect())
            .await
    }

    pub async fn cleanup_expired_reservations(&self) {
        self.0.cleanup_expired_reservations().await
    }

    pub async fn total_available_amount(&self) -> Decimal {
        self.0.total_available_amount(CC_INSTRUMENT).await
    }

    pub async fn stats(&self) -> (usize, usize, usize, usize) {
        self.0.stats(CC_INSTRUMENT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holding(cid: &str, instrument: &str, amount: &str, blob: bool) -> CachedHolding {
        CachedHolding {
            contract_id: cid.to_string(),
            template_id: if instrument == CC_INSTRUMENT {
                TEMPLATE_AMULET.to_string()
            } else {
                TEMPLATE_HOLDING.to_string()
            },
            instrument: instrument.to_string(),
            amount: amount.parse().unwrap(),
            created_event_blob: blob.then(|| format!("blob-{cid}")),
            synchronizer_id: "sync::1".to_string(),
            discovered_at: Instant::now(),
        }
    }

    const USDC: &str = "reg::USDC";

    #[tokio::test]
    async fn select_single_fit() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "10", true),
                holding("b", USDC, "50", true),
                holding("c", USDC, "100", true),
                holding("reserve", USDC, "1000", true),
            ])
            .await;
        // smallest single covering 40 is "b" (50); "reserve" (largest) excluded
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "40".parse().unwrap(), 20, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].contract_id, "b");
    }

    #[tokio::test]
    async fn select_multi_with_tighten() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "10", true),
                holding("b", USDC, "20", true),
                holding("c", USDC, "60", true),
                holding("d", USDC, "70", true),
                holding("reserve", USDC, "10000", true),
            ])
            .await;
        // target 100 > any single (reserve excluded). Greedy largest-first:
        // d(70) + c(60) = 130 >= 100. Tighten-last: residual = 100 - 70 = 30
        // → smallest single >= 30 that isn't d → c(60) stays (a=10, b=20 too small).
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "100".parse().unwrap(), 20, false)
            .await
            .unwrap();
        let cids: Vec<&str> = picks.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(cids, vec!["d", "c"]);

        // target 75: greedy d(70) + a? no — largest-first: d(70), then c(60)
        // total 130 >= 75 after two picks... tighten: residual = 75 - 70 = 5 →
        // smallest single >= 5 that isn't d → a(10). Final: d + a.
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "75".parse().unwrap(), 20, false)
            .await
            .unwrap();
        let cids: Vec<&str> = picks.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(cids, vec!["d", "a"]);
    }

    #[tokio::test]
    async fn select_respects_cap_and_insufficiency() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "10", true),
                holding("b", USDC, "10", true),
                holding("c", USDC, "10", true),
                holding("reserve", USDC, "10000", true),
            ])
            .await;
        // cap 2 → max coverage 20 < 25 → None (reserve never fail-opens for V2)
        assert!(cache
            .select_for_disclosure(&USDC.to_string(), "25".parse().unwrap(), 2, false)
            .await
            .is_none());
        // cap 3 → 30 >= 25
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "25".parse().unwrap(), 3, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 3);
    }

    #[tokio::test]
    async fn splitter_reserve_excluded_for_v2_included_by_fail_open() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("small", CC_INSTRUMENT, "5", true),
                holding("big", CC_INSTRUMENT, "500", true),
            ])
            .await;
        // V2: only "big" covers 100·1.02+1 but it IS the splitter reserve → None
        assert!(cache
            .select_for_disclosure(&CC_INSTRUMENT.to_string(), "100".parse().unwrap(), 20, true)
            .await
            .is_none());
        // v1 fail-open surface: excluded from the default set, present in _incl_reserve
        let cc = cache.cc();
        let without = cc.get_selectable_amulets().await;
        assert_eq!(without.len(), 1);
        assert_eq!(without[0].contract_id, "small");
        let with = cc.get_selectable_amulets_incl_reserve().await;
        assert_eq!(with.len(), 2);
    }

    #[tokio::test]
    async fn splitter_reserve_disabled_means_v1_unchanged() {
        let cache = HoldingsCache::new(false);
        cache
            .add_created(vec![
                holding("small", CC_INSTRUMENT, "5", true),
                holding("big", CC_INSTRUMENT, "500", true),
            ])
            .await;
        let cc = cache.cc();
        assert_eq!(cc.get_selectable_amulets().await.len(), 2);
    }

    #[tokio::test]
    async fn blob_pending_pick_returns_none() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "100", false), // blob-pending
                holding("reserve", USDC, "10000", true),
            ])
            .await;
        assert!(cache
            .select_for_disclosure(&USDC.to_string(), "50".parse().unwrap(), 20, false)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn v2_reservation_lifecycle() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "100", true),
                holding("b", USDC, "100", true),
                holding("reserve", USDC, "10000", true),
            ])
            .await;
        let expires = Instant::now() + std::time::Duration::from_secs(60);
        assert!(cache.reserve_v2(&["a".into()], "q1", expires).await);
        assert_eq!(cache.v2_reservation_quote("a").await.as_deref(), Some("q1"));
        // reserved cid is not selectable
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "50".parse().unwrap(), 20, false)
            .await
            .unwrap();
        assert_eq!(picks[0].contract_id, "b");
        // all-or-nothing: overlapping reserve fails
        assert!(!cache.reserve_v2(&["a".into(), "b".into()], "q2", expires).await);
        // V2-reserved amount excluded from availability totals (design §5.5 step 7)
        assert_eq!(
            cache.total_available_amount(USDC).await,
            "10100".parse::<Decimal>().unwrap()
        );
        cache.release_reservations(&["a".into()]).await;
        assert!(cache.v2_reservation_quote("a").await.is_none());
    }

    #[tokio::test]
    async fn refresh_merge_keeps_entries_discovered_after_snapshot() {
        let cache = HoldingsCache::new(false);
        let snapshot_start = Instant::now();
        // watcher discovers "new" AFTER the snapshot was taken
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        cache.add_created(vec![holding("new", USDC, "10", false)]).await;
        // snapshot (taken earlier) doesn't include "new"
        cache
            .refresh_from_acs_snapshot(vec![holding("old", USDC, "20", true)], snapshot_start)
            .await;
        let selectable = cache.get_selectable(USDC, false).await;
        let cids: std::collections::HashSet<&str> =
            selectable.iter().map(|h| h.contract_id.as_str()).collect();
        assert!(cids.contains("new"));
        assert!(cids.contains("old"));
    }
}
