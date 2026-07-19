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
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// TTL for available holdings (refreshed by ACS worker every 30s)
const AVAILABLE_TTL_SECS: u64 = 180; // 3 minutes

/// TTL for v1 payment reservations (workers should complete within this time)
const V1_RESERVATION_TTL_SECS: u64 = 60;

/// TTL for split-job reservations
const SPLIT_RESERVATION_TTL_SECS: u64 = 300;

/// TTL for optimistic post-split pending rungs: counted by
/// [`HoldingsCache::count_in_band`] until a COMPLETE ACS snapshot taken after
/// the split confirms them ([`HoldingsCache::clear_pending_splits_before`]) or
/// this many seconds pass.
const PENDING_SPLIT_TTL_SECS: u64 = 300;

/// Window for the per-instrument split-op budget (fail-stop).
const SPLIT_OP_WINDOW_SECS: u64 = 3600;

/// Budget for the opportunistic dust-merge sweep, independent of the funding
/// cap. Consolidation is a nice-to-have; each disclosed holding costs ~1.9 KB
/// of request body, so an unbudgeted sweep can pad a one-input settle out to
/// `max_inputs` and blow the submission size limit. Excess dust drains over
/// subsequent settles.
const DUST_MERGE_MAX_INPUTS: usize = 10;

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

/// Rungs a committed split created but the ACS has not confirmed yet
/// (the atomic split response carries no output cids/amounts).
#[derive(Debug, Clone)]
struct PendingSplit {
    instrument: InstrumentKey,
    denom: Decimal,
    count: u32,
    recorded_at: Instant,
}

impl PendingSplit {
    fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.recorded_at).as_secs() > PENDING_SPLIT_TTL_SECS
    }
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
    /// Per-instrument dust threshold (= the instrument's smallest ladder
    /// rung). Holdings strictly below it are swept as EXTRA inputs into V2
    /// settles so the settle's change output consolidates them.
    dust_below: RwLock<HashMap<InstrumentKey, Decimal>>,
    /// Optimistic post-split rungs, counted by [`Self::count_in_band`] until a
    /// complete ACS snapshot confirms them or they expire (split-storm guard).
    pending_splits: RwLock<Vec<PendingSplit>>,
    /// Per-instrument split-op submission times within the budget window —
    /// shared state for the cooldown + fail-stop budget, so the maintenance
    /// tick and the on-demand kicks are governed together.
    split_op_times: RwLock<HashMap<InstrumentKey, Vec<Instant>>>,
}

impl HoldingsCache {
    pub fn new(splitter_reserve_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            available: RwLock::new(HashMap::new()),
            consumed: RwLock::new(HashMap::new()),
            reserved: RwLock::new(HashMap::new()),
            splitter_reserve_enabled,
            dust_below: RwLock::new(HashMap::new()),
            pending_splits: RwLock::new(Vec::new()),
            split_op_times: RwLock::new(HashMap::new()),
        })
    }

    /// Set the per-instrument dust-merge thresholds (smallest ladder rung per
    /// instrument). Called once at startup from the split-target derivation.
    pub async fn set_dust_thresholds(&self, thresholds: HashMap<InstrumentKey, Decimal>) {
        *self.dust_below.write().await = thresholds;
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
            Self::splitter_reserve_cid(&available, &consumed, now, instrument)
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
    ///
    /// Honors consumed + TTL: a consumed or TTL-stale holding is never the
    /// reserve, so a stale cache HALTS splitting (no funding source) instead
    /// of funding a split loop off a holding whose true state is unknown.
    pub async fn splitter_reserve(&self, instrument: &str) -> Option<CachedHolding> {
        if !self.splitter_reserve_enabled {
            return None;
        }
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let cid = Self::splitter_reserve_cid(&available, &consumed, now, instrument)?;
        available.get(&cid).cloned()
    }

    /// Largest fresh (non-consumed, non-TTL-stale) holding of the instrument.
    fn splitter_reserve_cid(
        available: &HashMap<String, CachedHolding>,
        consumed: &HashMap<String, ConsumedEntry>,
        now: Instant,
        instrument: &str,
    ) -> Option<String> {
        available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| !consumed.contains_key(&h.contract_id))
            .filter(|h| now.duration_since(h.discovered_at).as_secs() <= AVAILABLE_TTL_SECS)
            .max_by(|a, b| {
                a.amount
                    .cmp(&b.amount)
                    // deterministic tie-break so the reserve is stable
                    .then_with(|| a.contract_id.cmp(&b.contract_id))
            })
            .map(|h| h.contract_id.clone())
    }

    /// EXISTENCE count of holdings with `lo <= amount < hi` for the ladder
    /// deficit computation: available − consumed, INCLUDING TTL-stale,
    /// reserved, and blob-pending entries (a rung that exists on the ledger
    /// but is temporarily unusable still exists — counting only *usable*
    /// rungs turned invisibility into phantom deficits, the mainnet split
    /// storm). The splitter reserve is excluded (it funds the ladder, it is
    /// not part of it). Unexpired [`Self::record_pending_split`] rungs are
    /// included so a just-committed split satisfies the count before the ACS
    /// confirms its outputs.
    pub async fn count_in_band(&self, instrument: &str, lo: Decimal, hi: Decimal) -> u32 {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let splitter_cid = if self.splitter_reserve_enabled {
            Self::splitter_reserve_cid(&available, &consumed, now, instrument)
        } else {
            None
        };

        let existing = available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| !consumed.contains_key(&h.contract_id))
            .filter(|h| Some(&h.contract_id) != splitter_cid.as_ref())
            .filter(|h| h.amount >= lo && h.amount < hi)
            .count() as u32;

        let pending: u32 = self
            .pending_splits
            .read()
            .await
            .iter()
            .filter(|p| p.instrument == instrument && !p.is_expired(now))
            .filter(|p| p.denom >= lo && p.denom < hi)
            .map(|p| p.count)
            .sum();

        existing.saturating_add(pending)
    }

    /// Record the rungs a just-committed split created (optimistic post-split
    /// accounting): counted by [`Self::count_in_band`] until a complete ACS
    /// snapshot taken after the split confirms them or they expire.
    pub async fn record_pending_split(&self, instrument: &str, splits: &[(Decimal, u32)]) {
        let now = Instant::now();
        let mut pending = self.pending_splits.write().await;
        pending.retain(|p| !p.is_expired(now));
        for (denom, count) in splits {
            pending.push(PendingSplit {
                instrument: instrument.to_string(),
                denom: *denom,
                count: *count,
                recorded_at: now,
            });
        }
    }

    /// Drop pending-split entries recorded before `snapshot_start`. Called by
    /// the ACS worker after a COMPLETE snapshot refresh — that snapshot either
    /// carries the split outputs (now in `available`) or proves they never
    /// materialized; either way the optimistic entries are superseded.
    pub async fn clear_pending_splits_before(&self, snapshot_start: Instant) {
        let mut pending = self.pending_splits.write().await;
        let before = pending.len();
        pending.retain(|p| p.recorded_at >= snapshot_start);
        let removed = before - pending.len();
        if removed > 0 {
            debug!("Cleared {} pending-split entr(ies) confirmed by ACS snapshot", removed);
        }
    }

    /// Record a split-op submission for the per-instrument cooldown + hourly
    /// fail-stop budget (attempts count too — a storm of failing submissions
    /// must also self-stop).
    pub async fn record_split_op(&self, instrument: &str) {
        let now = Instant::now();
        let mut ops = self.split_op_times.write().await;
        let entry = ops.entry(instrument.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t).as_secs() < SPLIT_OP_WINDOW_SECS);
        entry.push(now);
    }

    /// Split-op submission times within the budget window (pruned), for the
    /// pure gate decision in the split worker.
    pub async fn split_ops(&self, instrument: &str) -> Vec<Instant> {
        let now = Instant::now();
        self.split_op_times
            .read()
            .await
            .get(instrument)
            .map(|ops| {
                ops.iter()
                    .filter(|t| now.duration_since(**t).as_secs() < SPLIT_OP_WINDOW_SECS)
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// V2 disclosure selection (design §5.1). `target` = `amount` for utility
    /// instruments, `amount * 1.02 + 1` for CC (holding-fee decay margin).
    /// Smallest-single-fit, else greedy largest-first capped at `max_inputs`
    /// with a tighten-last pass. The splitter reserve is NEVER included.
    /// Only blob-ready holdings are candidates — a blob-pending holding (a
    /// fresh watcher-observed output awaiting ACS backfill) cannot be
    /// disclosed, and must not poison combinations blob-ready coverage can
    /// fund. Once coverage is met, up to [`DUST_MERGE_MAX_INPUTS`] sub-rung
    /// holdings are swept in for consolidation — a budget separate from (and
    /// bounded by) `max_inputs`, so a one-input settle stays a small request.
    /// The caller reserves the returned cids via [`Self::reserve_v2`].
    pub async fn select_for_disclosure(
        &self,
        instrument: &InstrumentKey,
        amount: Decimal,
        max_inputs: usize,
        is_cc: bool,
    ) -> Option<Vec<CachedHolding>> {
        self.select_for_disclosure_with(instrument, amount, max_inputs, is_cc, false)
            .await
    }

    /// [`Self::select_for_disclosure`] with the splitter reserve opted IN
    /// (`include_reserve`) — the taker path spends the user's own funds, and
    /// the reserve is just their largest holding, not LP ladder inventory.
    pub async fn select_for_disclosure_with(
        &self,
        instrument: &InstrumentKey,
        amount: Decimal,
        max_inputs: usize,
        is_cc: bool,
        include_reserve: bool,
    ) -> Option<Vec<CachedHolding>> {
        if amount <= Decimal::ZERO || max_inputs == 0 {
            return None;
        }
        let target = if is_cc {
            amount * Decimal::new(102, 2) + Decimal::ONE
        } else {
            amount
        };

        let mut selectable = self.get_selectable(instrument, !include_reserve).await;
        selectable.retain(|h| h.created_event_blob.is_some());

        let mut picks = select_from(&selectable, target, max_inputs)?;

        // Dust merge: sweep sub-rung holdings into the settle as extra inputs
        // (smallest first) so the change output consolidates them. Only after
        // coverage is met, only blob-ready holdings, never past max_inputs and
        // never more than DUST_MERGE_MAX_INPUTS in one settle.
        let dust_below = self.dust_below.read().await.get(instrument).copied();
        if let Some(threshold) = dust_below {
            let picked: std::collections::HashSet<&str> =
                picks.iter().map(|h| h.contract_id.as_str()).collect();
            let dust: Vec<CachedHolding> = selectable
                .iter() // ascending by amount
                .filter(|h| {
                    h.amount < threshold
                        && h.created_event_blob.is_some()
                        && !picked.contains(h.contract_id.as_str())
                })
                .take(DUST_MERGE_MAX_INPUTS.min(max_inputs.saturating_sub(picks.len())))
                .cloned()
                .collect();
            if !dust.is_empty() {
                debug!(
                    "select_for_disclosure({instrument}): sweeping {} dust holding(s) (< {threshold}) for consolidation",
                    dust.len()
                );
                picks.extend(dust);
            }
        }
        Some(picks)
    }

    /// [`Self::select_for_disclosure_with`] retried on `poll` cadence until
    /// `deadline` — an empty selection is often a seconds-long gap (fresh
    /// proceeds blob-pending until the next ACS snapshot, holdings
    /// momentarily reserved), not depletion. Shared by the taker pay/fee
    /// legs; the LP confirm path runs its own loop interleaved with the
    /// on-demand split kick.
    pub async fn select_for_disclosure_until(
        &self,
        instrument: &InstrumentKey,
        amount: Decimal,
        max_inputs: usize,
        is_cc: bool,
        include_reserve: bool,
        deadline: Instant,
        poll: Duration,
    ) -> Option<Vec<CachedHolding>> {
        let mut picks = self
            .select_for_disclosure_with(instrument, amount, max_inputs, is_cc, include_reserve)
            .await;
        while picks.is_none() && Instant::now() < deadline {
            tokio::time::sleep(poll).await;
            picks = self
                .select_for_disclosure_with(instrument, amount, max_inputs, is_cc, include_reserve)
                .await;
        }
        picks
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

    /// Total amount [`Self::select_for_disclosure_with`] could actually cover
    /// right now: selectable (fresh, unconsumed, unreserved, splitter reserve
    /// per `include_reserve`) AND blob-ready, counting only the largest
    /// `max_inputs` holdings (selection is input-capped, so a fragmented
    /// cache must not report coverage the selection cannot assemble). Lets a
    /// caller size work against the cache's spendable view instead of a
    /// server-side balance that still counts reserved/blob-pending/stale
    /// holdings.
    pub async fn selectable_blob_ready_total(
        &self,
        instrument: &str,
        max_inputs: usize,
        include_reserve: bool,
    ) -> Decimal {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;
        let splitter_cid = if !include_reserve && self.splitter_reserve_enabled {
            Self::splitter_reserve_cid(&available, &consumed, now, instrument)
        } else {
            None
        };
        let mut amounts: Vec<Decimal> = available
            .values()
            .filter(|h| h.instrument == instrument)
            .filter(|h| now.duration_since(h.discovered_at).as_secs() <= AVAILABLE_TTL_SECS)
            .filter(|h| !consumed.contains_key(&h.contract_id))
            .filter(|h| match reserved.get(&h.contract_id) {
                Some(entry) => entry.is_expired(now),
                None => true,
            })
            .filter(|h| Some(&h.contract_id) != splitter_cid.as_ref())
            .filter(|h| h.created_event_blob.is_some())
            .map(|h| h.amount)
            .collect();
        amounts.sort_unstable_by(|a, b| b.cmp(a));
        amounts.into_iter().take(max_inputs).sum()
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

    /// Per-holding amounts for one instrument, for the LIQUIDITY histogram log.
    /// Returns `(total, reserved, [(amount, is_reserved)])` over available −
    /// consumed holdings (age-agnostic — reserved holdings are still on-ledger
    /// so they belong in the total, flagged). `is_reserved` = an unexpired
    /// reservation of any kind holds the cid (mirrors the `stats()` filters).
    pub async fn holdings_for_instrument(
        &self,
        instrument: &str,
    ) -> (usize, usize, Vec<(Decimal, bool)>) {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;

        let mut out: Vec<(Decimal, bool)> = Vec::new();
        let mut reserved_count = 0usize;
        for h in available.values() {
            if h.instrument != instrument {
                continue;
            }
            if consumed.contains_key(&h.contract_id) {
                continue;
            }
            let is_reserved = matches!(
                reserved.get(&h.contract_id),
                Some(entry) if !entry.is_expired(now)
            );
            if is_reserved {
                reserved_count += 1;
            }
            out.push((h.amount, is_reserved));
        }
        (out.len(), reserved_count, out)
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

    fn holding_at(
        cid: &str,
        instrument: &str,
        amount: &str,
        blob: bool,
        discovered_at: Instant,
    ) -> CachedHolding {
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
            discovered_at,
        }
    }

    fn holding(cid: &str, instrument: &str, amount: &str, blob: bool) -> CachedHolding {
        holding_at(cid, instrument, amount, blob, Instant::now())
    }

    /// An Instant `AVAILABLE_TTL_SECS`+20s in the past, or `None` when the
    /// platform's monotonic clock is too young (freshly-booted machine).
    fn stale_instant() -> Option<Instant> {
        Instant::now().checked_sub(std::time::Duration::from_secs(AVAILABLE_TTL_SECS + 20))
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
    async fn dust_merge_appends_sub_rung_holdings_up_to_cap() {
        let cache = HoldingsCache::new(true);
        cache
            .set_dust_thresholds(
                [(USDC.to_string(), "15".parse().unwrap())].into_iter().collect(),
            )
            .await;
        cache
            .add_created(vec![
                holding("reserve", USDC, "1000", true), // splitter reserve — excluded
                holding("rung", USDC, "25", true),      // covers the target alone
                holding("dust1", USDC, "0.5", true),
                holding("dust2", USDC, "3", true),
                holding("dust3", USDC, "7", true),
                holding("dust-noblob", USDC, "1", false), // blob-pending — never swept
            ])
            .await;

        // Coverage from "rung", then dust ascending up to the cap.
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "20".parse().unwrap(), 4, false)
            .await
            .unwrap();
        let cids: Vec<&str> = picks.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(cids[0], "rung");
        assert_eq!(&cids[1..], &["dust1", "dust2", "dust3"]);

        // Cap respected: max_inputs 2 → coverage + one dust only.
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "20".parse().unwrap(), 2, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 2);

        // No threshold configured → no sweep.
        cache.set_dust_thresholds(Default::default()).await;
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "20".parse().unwrap(), 4, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 1);
    }

    /// The dust budget binds before `max_inputs` does: a one-input settle with
    /// a deep dust pile must not pad out to the funding cap (that inflated a
    /// real settle's disclosed contracts to 100 and blew the request size).
    #[tokio::test]
    async fn dust_merge_is_capped_at_dust_budget() {
        let cache = HoldingsCache::new(true);
        cache
            .set_dust_thresholds(
                [(USDC.to_string(), "15".parse().unwrap())].into_iter().collect(),
            )
            .await;
        let mut holdings = vec![
            holding("reserve", USDC, "1000", true), // splitter reserve — excluded
            holding("rung", USDC, "25", true),      // covers the target alone
        ];
        // 20 dust holdings at 0.1 .. 2.0 — all under the 15 threshold, and
        // ascending in cid order so the expected sweep is easy to state.
        for i in 1..=20 {
            holdings.push(holding(
                &format!("dust{i:02}"),
                USDC,
                &format!("{}", Decimal::new(i, 1)),
                true,
            ));
        }
        cache.add_created(holdings).await;

        // max_inputs is the full protocol bound; the dust budget is what binds.
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "20".parse().unwrap(), 100, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 1 + DUST_MERGE_MAX_INPUTS);
        let cids: Vec<&str> = picks.iter().map(|h| h.contract_id.as_str()).collect();
        assert_eq!(cids[0], "rung");
        // The 10 smallest dust, ascending.
        let expected: Vec<String> =
            (1..=DUST_MERGE_MAX_INPUTS).map(|i| format!("dust{i:02}")).collect();
        assert_eq!(&cids[1..], expected.as_slice());

        // A max_inputs tighter than the dust budget still wins.
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "20".parse().unwrap(), 4, false)
            .await
            .unwrap();
        assert_eq!(picks.len(), 4);
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
    async fn blob_pending_pick_falls_back_to_blob_ready_subset() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                // Smallest single fit for 50 — but blob-pending.
                holding("fresh", USDC, "100", false),
                holding("rung", USDC, "200", true),
                holding("reserve", USDC, "10000", true),
            ])
            .await;
        let picks = cache
            .select_for_disclosure(&USDC.to_string(), "50".parse().unwrap(), 20, false)
            .await
            .expect("blob-ready coverage exists");
        assert_eq!(
            picks.iter().map(|h| h.contract_id.as_str()).collect::<Vec<_>>(),
            vec!["rung"]
        );
    }

    #[tokio::test]
    async fn spend_reserve_selection_includes_the_largest_holding() {
        let cache = HoldingsCache::new(true);
        cache.add_created(vec![holding("only", USDC, "100", true)]).await;
        // Default view: the single holding IS the splitter reserve → None.
        assert!(cache
            .select_for_disclosure(&USDC.to_string(), "50".parse().unwrap(), 20, false)
            .await
            .is_none());
        // Taker view (spend_reserve): the party's own largest holding spends.
        let picks = cache
            .select_for_disclosure_with(&USDC.to_string(), "50".parse().unwrap(), 20, false, true)
            .await
            .expect("reserve included");
        assert_eq!(picks[0].contract_id, "only");
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

    #[tokio::test]
    async fn count_in_band_counts_existence_not_usability() {
        let Some(stale_at) = stale_instant() else { return };
        let cache = HoldingsCache::new(true);
        let lo: Decimal = "100".parse().unwrap();
        let hi: Decimal = "200".parse().unwrap();
        cache
            .add_created(vec![
                holding("reserve", USDC, "10000", true), // splitter reserve — out of band anyway
                holding("fresh", USDC, "100", true),
                holding_at("stale", USDC, "150", true, stale_at), // TTL-stale: still EXISTS
                holding("quoted", USDC, "120", true),             // gets a live V2 reservation
                holding("spent", USDC, "110", true),              // marked consumed
                holding("noblob", USDC, "130", false),            // blob-pending: still exists
                holding("outband", USDC, "50", true),
            ])
            .await;
        cache
            .reserve_v2(
                &["quoted".to_string()],
                "q1",
                Instant::now() + std::time::Duration::from_secs(120),
            )
            .await;
        cache.mark_consumed(&["spent".to_string()], "settle").await;

        // stale + reserved + blob-pending count (existence ≠ usability);
        // consumed and out-of-band don't — the old get_selectable-based count
        // dropped "stale" and "quoted" here and manufactured phantom deficits.
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 4);
        let selectable_in_band = cache
            .get_selectable(USDC, true)
            .await
            .into_iter()
            .filter(|h| h.amount >= lo && h.amount < hi)
            .count();
        assert_eq!(selectable_in_band, 2, "usability view stays strict (fresh + noblob)");

        // optimistic post-split rungs count until confirmed…
        let before_split = Instant::now();
        cache
            .record_pending_split(USDC, &[("100".parse().unwrap(), 3), ("500".parse().unwrap(), 2)])
            .await;
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 7);

        // …a snapshot started BEFORE the split cannot confirm them…
        cache.clear_pending_splits_before(before_split).await;
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 7);

        // …but a complete snapshot started after the split supersedes them.
        cache.clear_pending_splits_before(Instant::now()).await;
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 4);
    }

    #[tokio::test]
    async fn count_in_band_excludes_the_splitter_reserve() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "100", true),
                holding("b", USDC, "150", true), // largest fresh = splitter reserve
            ])
            .await;
        let lo: Decimal = "100".parse().unwrap();
        let hi: Decimal = "200".parse().unwrap();
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 1);

        // splitter reserve disabled → both count
        let cache = HoldingsCache::new(false);
        cache
            .add_created(vec![
                holding("a", USDC, "100", true),
                holding("b", USDC, "150", true),
            ])
            .await;
        assert_eq!(cache.count_in_band(USDC, lo, hi).await, 2);
    }

    #[tokio::test]
    async fn splitter_reserve_honors_consumed_and_ttl() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("big", USDC, "1000", true),
                holding("mid", USDC, "500", true),
            ])
            .await;
        assert_eq!(cache.splitter_reserve(USDC).await.unwrap().contract_id, "big");

        // consumed holdings can no longer be the reserve
        cache.mark_consumed(&["big".to_string()], "split").await;
        assert_eq!(cache.splitter_reserve(USDC).await.unwrap().contract_id, "mid");

        // a fully TTL-stale cache HALTS splitting: no reserve at all
        let Some(stale_at) = stale_instant() else { return };
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding_at("big", USDC, "1000", true, stale_at),
                holding_at("mid", USDC, "500", true, stale_at),
            ])
            .await;
        assert!(cache.splitter_reserve(USDC).await.is_none());
    }

    #[tokio::test]
    async fn split_op_recording_feeds_the_gate() {
        let cache = HoldingsCache::new(true);
        assert!(cache.split_ops(USDC).await.is_empty());
        cache.record_split_op(USDC).await;
        cache.record_split_op(USDC).await;
        assert_eq!(cache.split_ops(USDC).await.len(), 2);
        // per-instrument isolation
        assert!(cache.split_ops("reg::CBTC").await.is_empty());
    }

    #[tokio::test]
    async fn holdings_for_instrument_totals_and_reserved_flag() {
        let cache = HoldingsCache::new(true);
        cache
            .add_created(vec![
                holding("a", USDC, "12", true),
                holding("b", USDC, "20", true),
                holding("c", USDC, "60", true),
                holding("x", "reg::CBTC", "0.0002", true), // different instrument
            ])
            .await;
        // Reserve "b" as a live V2 quote.
        cache
            .reserve_v2(
                &["b".to_string()],
                "quote-1",
                Instant::now() + std::time::Duration::from_secs(120),
            )
            .await;

        let (total, reserved, holdings) = cache.holdings_for_instrument(USDC).await;
        assert_eq!(total, 3, "only the 3 USDC holdings");
        assert_eq!(reserved, 1, "b is reserved");
        // The reserved flag tracks "b" (amount 20), not a/c.
        let flag_for = |amt: &str| {
            holdings
                .iter()
                .find(|(a, _)| *a == amt.parse::<Decimal>().unwrap())
                .map(|(_, r)| *r)
        };
        assert_eq!(flag_for("20"), Some(true));
        assert_eq!(flag_for("12"), Some(false));
        assert_eq!(flag_for("60"), Some(false));
    }
}
