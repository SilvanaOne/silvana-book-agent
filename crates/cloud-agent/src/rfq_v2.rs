//! RFQ V2 (AtomicDVP) LP-side quote state machine (design §5.5, §5.7).
//!
//! Transitions per quote_id:
//! `Indicative{availability check only}` → (confirm) → `Confirming` →
//! `Confirmed{LM commitment, holdings, ticket, envelope}` → (SettleObserved) →
//! `Settled`, and → `Expired` from any live state via the sweep.
//!
//! Indicative quotes are non-binding and hold NO LiquidityManager commitment
//! (reserving the full LP-pays leg per ~90s indicative quote saturated the
//! whole balance under concurrent RFQ load); the atomic check-and-reserve is
//! confirm-phase Step 1.5, held until settle/expiry.
//!
//! Reject-path invariant: EVERY confirm reject releases everything the
//! pipeline acquired before returning — the Step-1.5 LiquidityManager
//! commitment (`"rfqv2:"+quote_id`), any hard-reserved holdings, and any
//! assigned ticket — and drops the pending entry. The sweep is the backstop,
//! not the mechanism.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::{
    RfqV2Config, RfqV2MarketConfig, RFQ_V2_MAX_QUOTE_VALID_SECS, RFQ_V2_MAX_SETTLE_GRACE_SECS,
};
use agent_logic::liquidity::LiquidityManager;
use agent_logic::net_position::NetPositionTracker;
use agent_logic::num::Dp;
use agent_logic::{clock, supervise, sync};
use agent_logic::pool_impact::{self, ImpactSide, MarketMid};
use agent_logic::state::SavedPendingV2;
use atomic_quote::envelope::{
    canonical_from_dvp, InstrumentIdJson, LpFeeJson, QuoteJson, ENVELOPE_VERSION,
};
use atomic_quote::{render_decimal, sign_quote_scalar, verify_quote, QuoteSide};
use orderbook_proto::rfqv2::{
    AtomicAcsContract, AtomicDisclosedContract, AtomicFeeSpec, AtomicQuote, AtomicQuoteEnvelope,
    RfqConfirmReject, RfqConfirmRejectReason, RfqConfirmRequest,
};

use crate::holdings_cache::{HoldingsCache, InstrumentKey};
use crate::rfq_handler::{PoolPricingSnapshot, PricedQuote};
use crate::ticket_pool::TicketPool;
use crate::venue_registry::VenueRegistry;

/// Consumption of a V2-reserved holding observed by the updates watcher.
#[derive(Debug, Clone)]
pub struct SettleObserved {
    pub quote_id: String,
    pub update_id: String,
}

/// How long Settled/Expired tombstones (and their correlation entries) are
/// retained before GC.
const TOMBSTONE_TTL: Duration = Duration::from_secs(300);

/// Poll cadence while a confirm waits for an on-demand denomination split to
/// land (fine-grained — the historical 1 s step used to overshoot). Shared
/// with the taker-side selection re-poll (atomic_swap.rs).
pub(crate) const CONFIRM_SPLIT_POLL: Duration = Duration::from_millis(400);
/// Slack subtracted from the relay's forwarded `respond_by` so the LP's
/// envelope/reject reaches the relay before its own confirm timeout fires
/// (handlers/rfqv2.rs `tokio::time::timeout(timeout_secs, rx)`).
const CONFIRM_RESPONSE_MARGIN_MS: i64 = 1_200;
/// Hard ceiling on the confirm-time split wait — defensive bound against a
/// bogus/far-future `respond_by` (the relay itself clamps `timeout_secs` to 30).
/// Shared with the taker-side selection re-poll (atomic_swap.rs).
pub(crate) const MAX_CONFIRM_SPLIT_WAIT: Duration = Duration::from_secs(28);
/// Fallback wait when `respond_by` is absent/unparseable (pre-`respond_by`
/// relay) — preserves the historical bounded 6 s wait.
const FALLBACK_CONFIRM_SPLIT_WAIT: Duration = Duration::from_secs(6);
/// An on-demand split still running after this long is logged; it is never cut short.
const ON_DEMAND_SPLIT_SLOW_AFTER: Duration = Duration::from_secs(120);
/// A confirm still running after this long is treated as abandoned by the sweep.
const CONFIRM_ABANDONED_AFTER: Duration = Duration::from_secs(300);
/// Reservation attempts of one confirm when concurrent confirms take its picks.
const RESERVE_ATTEMPTS: u32 = 3;

/// Marks an instrument as having an on-demand split in flight; dropping it
/// clears the mark on every exit, including a panic or cancellation.
struct InFlightGuard {
    set: Arc<Mutex<HashSet<InstrumentKey>>>,
    key: InstrumentKey,
}

impl InFlightGuard {
    /// `None` when a split for `key` is already in flight.
    fn claim(set: &Arc<Mutex<HashSet<InstrumentKey>>>, key: &InstrumentKey) -> Option<Self> {
        let inserted = sync::lock(set).insert(key.clone());
        inserted.then(|| Self {
            set: Arc::clone(set),
            key: key.clone(),
        })
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        sync::lock(&self.set).remove(&self.key);
    }
}

/// Runs one on-demand split to its end, then releases its in-flight mark;
/// logs once if it is still running after `slow_after`.
async fn run_on_demand_split<F>(guard: InFlightGuard, slow_after: Duration, job: F)
where
    F: Future<Output = anyhow::Result<()>>,
{
    let on_slow = || {
        warn!(
            "On-demand split for {} still running after {}s; it continues",
            guard.key,
            slow_after.as_secs()
        );
    };
    if let (Err(e), _) = supervise::run_to_end(job, slow_after, on_slow).await {
        warn!("On-demand split for {} failed: {:#}", guard.key, e);
    }
}

/// How long a confirm may wait for an on-demand split, given the relay's
/// `respond_by` and the wall clock in epoch milliseconds.
fn confirm_split_wait(respond_by: Option<&prost_types::Timestamp>, now_ms: i64) -> Duration {
    let Some(ts) = respond_by else {
        return FALLBACK_CONFIRM_SPLIT_WAIT;
    };
    let max_ms = i64::try_from(MAX_CONFIRM_SPLIT_WAIT.as_millis()).unwrap_or(i64::MAX);
    let remaining_ms = ts
        .seconds
        .saturating_mul(1000)
        .saturating_add(i64::from(ts.nanos) / 1_000_000)
        .saturating_sub(now_ms)
        .saturating_sub(CONFIRM_RESPONSE_MARGIN_MS)
        .clamp(0, max_ms);
    Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(0))
}

/// Reservation expiry and signed validity end of one confirm, both counted
/// from its start.
struct QuoteWindow {
    expires_at: Instant,
    valid_until_micros: i64,
}

/// Per-market instrument resolution for the V2 paths.
#[derive(Debug, Clone)]
pub struct MarketInstruments {
    pub base_key: InstrumentKey,
    pub base_is_cc: bool,
    pub quote_key: InstrumentKey,
    pub quote_is_cc: bool,
}

enum PendingV2 {
    Indicative {
        market_id: String,
        side: QuoteSide,
        base_amount: Decimal,
        quote_amount: Decimal,
        /// (instrument key, amount) the LP pays on settle
        lp_pays: (InstrumentKey, Decimal),
        /// LM token symbol for the same leg (aliases resolve inside LM)
        lp_pays_token: String,
        notional_usd: Option<f64>,
        valid_until: Instant,
        /// The AUTHORITATIVE settlement fee received with the RFQ fan-out —
        /// signed into Quote.lpFees at confirm (design §14 D20). None = zero-fee pair.
        settlement_fee: Option<AtomicFeeSpec>,
        /// Requesting user's party id from the fan-out (None on older
        /// servers) — keys the net-position accumulator at confirm.
        user_party: Option<String>,
        /// Pricing inputs, present iff the adjustment applied — this is what
        /// arms the confirm-time staleness re-check.
        pool_inputs: Option<PoolPricingSnapshot>,
    },
    /// A confirm for this quote is running; a duplicate confirm is refused.
    Confirming {
        since: Instant,
    },
    Confirmed {
        holding_cids: Vec<String>,
        ticket_id: String,
        /// kept for symmetry with the correlation index (not read directly)
        #[allow(dead_code)]
        ticket_cid: Option<String>,
        /// None after restart-restore (the envelope is not persisted; the
        /// reservation still backs the possibly-live envelope out there)
        envelope: Option<AtomicQuoteEnvelope>,
        #[allow(dead_code)]
        valid_until_micros: i64,
        lp_pays_token: String,
        lp_pays_amount: Decimal,
        /// valid_until + settle_grace — reservation/ticket TTL
        expires_at: Instant,
        market_id: String,
    },
    Settled {
        since: Instant,
    },
    Expired {
        since: Instant,
    },
}

pub struct RfqV2State {
    party_id: String,
    lp_name: String,
    synchronizer_id: String,
    quote_key: agent_logic::config::AtomicQuoteKey,
    v2: RfqV2Config,
    /// rfq_v2-enabled markets only
    market_v2: HashMap<String, RfqV2MarketConfig>,
    market_instruments: HashMap<String, MarketInstruments>,
    cache: Arc<HoldingsCache>,
    ticket_pool: Option<Arc<TicketPool>>,
    venue_registry: Arc<VenueRegistry>,
    liquidity_manager: Arc<LiquidityManager>,
    /// quote_id -> state. std Mutex: sections are short and never held across await.
    pending: Mutex<HashMap<String, PendingV2>>,
    /// contract id (holding or ticket) -> quote_id
    correlation: Mutex<HashMap<String, String>>,
    /// On-demand split support: agent config + per-instrument ladder rungs.
    /// When indicative-time selection finds no disclosable holdings but the
    /// splitter reserve could fund the leg, a single-flight background split
    /// is kicked so the confirm (where the quote is SIGNED) can succeed.
    base_config: agent_logic::config::BaseConfig,
    split_rungs: HashMap<InstrumentKey, (crate::split_worker::SplitInstrument, Vec<(Decimal, u32)>)>,
    splits_in_flight: Arc<Mutex<HashSet<InstrumentKey>>>,
    /// Live mid-price map shared with the RFQ poller (None in tests). Confirm
    /// re-checks it so a stale indicative is never signed into an envelope.
    mid_prices: Option<Arc<tokio::sync::RwLock<HashMap<String, MarketMid>>>>,
    /// Trailing net-position accumulator. None in tests / non-LP.
    net_positions: Option<Arc<NetPositionTracker>>,
    /// Competing quote ids that reserve a confirm's picks just before it does.
    #[cfg(test)]
    reserve_races: Mutex<Vec<String>>,
}

impl RfqV2State {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        party_id: String,
        lp_name: String,
        synchronizer_id: String,
        quote_key: agent_logic::config::AtomicQuoteKey,
        v2: RfqV2Config,
        market_v2: HashMap<String, RfqV2MarketConfig>,
        market_instruments: HashMap<String, MarketInstruments>,
        cache: Arc<HoldingsCache>,
        ticket_pool: Option<Arc<TicketPool>>,
        venue_registry: Arc<VenueRegistry>,
        liquidity_manager: Arc<LiquidityManager>,
        base_config: agent_logic::config::BaseConfig,
        split_targets: &[crate::split_worker::SplitTarget],
    ) -> Self {
        let split_rungs = split_targets
            .iter()
            .filter_map(|t| {
                crate::split_worker::parse_splits(&t.denominations)
                    .ok()
                    .map(|rungs| (t.instrument.key.clone(), (t.instrument.clone(), rungs)))
            })
            .collect();
        Self {
            party_id,
            lp_name,
            synchronizer_id,
            quote_key,
            v2,
            market_v2,
            market_instruments,
            cache,
            ticket_pool,
            venue_registry,
            liquidity_manager,
            pending: Mutex::new(HashMap::new()),
            correlation: Mutex::new(HashMap::new()),
            base_config,
            split_rungs,
            splits_in_flight: Arc::new(Mutex::new(HashSet::new())),
            mid_prices: None,
            net_positions: None,
            #[cfg(test)]
            reserve_races: Mutex::new(Vec::new()),
        }
    }

    /// Lets the next queued competitor reserve `cids` first.
    #[cfg(test)]
    async fn run_reserve_race_for_tests(&self, cids: &[String], expires_at: Instant) {
        let competitor = sync::lock(&self.reserve_races).pop();
        if let Some(competitor) = competitor {
            self.cache.reserve_v2(cids, &competitor, expires_at).await;
        }
    }

    /// Attach the poller's shared mid-price map so confirm can refuse to sign
    /// when the market has gone priceless since the indicative quote.
    pub fn with_mid_prices(
        mut self,
        mid_prices: Arc<tokio::sync::RwLock<HashMap<String, MarketMid>>>,
    ) -> Self {
        self.mid_prices = Some(mid_prices);
        self
    }

    /// Attach the net-position tracker: confirms soft-count into it, observed
    /// settles finalize, the expiry sweep releases.
    pub fn with_net_positions(mut self, tracker: Arc<NetPositionTracker>) -> Self {
        self.net_positions = Some(tracker);
        self
    }

    /// Kick a single-flight background denomination split for `instrument`
    /// (no-op when no ladder is configured or a split is already in flight).
    /// Called from the indicative phase so rungs exist by confirm time —
    /// the LP must not sign a quote it cannot fund.
    fn kick_on_demand_split(&self, instrument: &InstrumentKey) {
        self.kick_split_with(instrument, |state, split_instr, rungs| {
            let config = state.base_config.clone();
            let cache = state.cache.clone();
            // ensure_denominations applies the shared cooldown + fail-stop budget,
            // so on-demand kicks are governed together with the maintenance tick.
            let v2 = state.v2.clone();
            async move {
                let mut client = crate::atomic_swap::create_atomic_client(&config).await?;
                crate::split_worker::ensure_denominations(
                    &config, &cache, &mut client, &split_instr, &rungs, &v2,
                )
                .await
            }
        });
    }

    /// [`Self::kick_on_demand_split`] with the split job built by `job`.
    fn kick_split_with<F, Fut>(&self, instrument: &InstrumentKey, job: F)
    where
        F: FnOnce(&Self, crate::split_worker::SplitInstrument, Vec<(Decimal, u32)>) -> Fut,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Some((split_instr, rungs)) = self.split_rungs.get(instrument).cloned() else {
            return;
        };
        let Some(guard) = InFlightGuard::claim(&self.splits_in_flight, instrument) else {
            return; // already splitting this instrument
        };
        info!(
            "On-demand split kicked for {} (indicative-time selection empty)",
            instrument
        );
        let job = job(self, split_instr, rungs);
        supervise::try_spawn(
            "on-demand split",
            run_on_demand_split(guard, ON_DEMAND_SPLIT_SLOW_AFTER, job),
        );
    }

    fn split_in_flight(&self, instrument: &InstrumentKey) -> bool {
        sync::lock(&self.splits_in_flight).contains(instrument)
    }

    pub fn lp_name(&self) -> &str {
        &self.lp_name
    }

    pub fn party_id(&self) -> &str {
        &self.party_id
    }

    pub fn config(&self) -> &RfqV2Config {
        &self.v2
    }

    /// Longest reservation a restart restores: the largest accepted validity
    /// plus grace, whatever the configuration that signed the quote.
    pub fn restore_ttl_cap() -> Duration {
        Duration::from_secs(RFQ_V2_MAX_QUOTE_VALID_SECS.saturating_add(RFQ_V2_MAX_SETTLE_GRACE_SECS))
    }

    /// The window of a confirm that started at `now` (`start_micros` on the
    /// wall clock); `None` when it does not fit an `Instant` or the signed micros.
    fn quote_window(&self, now: Instant, start_micros: i64) -> Option<QuoteWindow> {
        let ttl_secs = self
            .v2
            .atomic_quote_valid_secs
            .checked_add(self.v2.settle_grace_secs)?;
        let expires_at = now.checked_add(Duration::from_secs(ttl_secs))?;
        let valid_micros = i64::try_from(self.v2.atomic_quote_valid_secs)
            .ok()?
            .checked_mul(1_000_000)?;
        Some(QuoteWindow {
            expires_at,
            valid_until_micros: start_micros.checked_add(valid_micros)?,
        })
    }

    fn lm_key(quote_id: &str) -> String {
        format!("rfqv2:{quote_id}")
    }

    /// Test-only visibility into the per-quote state machine.
    #[cfg(test)]
    pub(crate) fn pending_kind(&self, quote_id: &str) -> Option<&'static str> {
        sync::lock(&self.pending).get(quote_id).map(|e| match e {
            PendingV2::Indicative { .. } => "Indicative",
            PendingV2::Confirming { .. } => "Confirming",
            PendingV2::Confirmed { .. } => "Confirmed",
            PendingV2::Settled { .. } => "Settled",
            PendingV2::Expired { .. } => "Expired",
        })
    }

    /// Is this market quotable over the atomic stream right now?
    pub fn quotable(&self, market_id: &str) -> bool {
        self.market_v2.contains_key(market_id)
            && self.venue_registry.validated(market_id).is_some()
    }

    /// Markets with validated venues (for the stream handshake).
    pub fn validated_market_ids(&self) -> Vec<String> {
        self.venue_registry.validated_market_ids()
    }

    // ------------------------------------------------------------------
    // Phase 1 — indicative quote + availability check
    // ------------------------------------------------------------------

    /// Advisory availability check plus the Indicative entry; commitment
    /// happens at confirm. Nothing is counted into the accumulator here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn register_indicative(
        &self,
        quote_id: &str,
        market_id: &str,
        side: QuoteSide,
        priced: &PricedQuote,
        settlement_fee: Option<AtomicFeeSpec>,
        user_party: Option<&str>,
    ) -> Result<(), String> {
        let mi = self
            .market_instruments
            .get(market_id)
            .ok_or_else(|| format!("market {market_id} not configured for atomic RFQ"))?;
        let lp_pays_key = match side {
            QuoteSide::Buy => mi.base_key.clone(),
            QuoteSide::Sell => mi.quote_key.clone(),
        };
        let (lp_pays_token, lp_pays_amount) = priced.lp_pays.clone();

        // Advisory only: reserving per indicative saturates the balance under
        // load. Over-quoting is by design, with try_commit at confirm.
        let avail = self.liquidity_manager.available(&lp_pays_token).await;
        if avail < lp_pays_amount {
            return Err(format!(
                "insufficient {lp_pays_token} ({} available, {} needed)",
                Dp(avail, 4),
                Dp(lp_pays_amount, 4)
            ));
        }

        let Some(valid_until) =
            Instant::now().checked_add(Duration::from_secs(u64::from(priced.valid_for_secs)))
        else {
            return Err(format!("quote validity {}s out of range", priced.valid_for_secs));
        };

        // Dry-run the cid-level selection the confirm will need. If it comes
        // up empty (e.g. everything sits in the splitter reserve), kick a
        // background split NOW so rungs exist before the quote is signed.
        let is_cc = match side {
            QuoteSide::Buy => mi.base_is_cc,
            QuoteSide::Sell => mi.quote_is_cc,
        };
        let max_inputs = self
            .market_v2
            .get(market_id)
            .map(|m| m.max_input_holdings)
            .unwrap_or(100);
        let needs_split = self
            .cache
            .select_for_disclosure(&lp_pays_key, lp_pays_amount, max_inputs, is_cc)
            .await
            .is_none();

        // Recorded after the last await, so a cancelled call leaves no entry.
        sync::lock(&self.pending).insert(
            quote_id.to_string(),
            PendingV2::Indicative {
                market_id: market_id.to_string(),
                side,
                base_amount: priced.quantity,
                quote_amount: priced.quote_quantity,
                lp_pays: (lp_pays_key.clone(), lp_pays_amount),
                lp_pays_token,
                notional_usd: priced.notional_usd,
                valid_until,
                settlement_fee,
                user_party: user_party.map(|p| p.to_string()),
                pool_inputs: priced.pool_pricing.clone(),
            },
        );
        if needs_split {
            self.kick_on_demand_split(&lp_pays_key);
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Phase 2 — confirm: hard reserve + ticket + sign + envelope
    // ------------------------------------------------------------------

    fn reject(
        &self,
        req: &RfqConfirmRequest,
        reason: RfqConfirmRejectReason,
        detail: impl Into<String>,
    ) -> RfqConfirmReject {
        RfqConfirmReject {
            rfq_id: req.rfq_id.clone(),
            quote_id: req.quote_id.clone(),
            lp_party_id: self.party_id.clone(),
            reason: reason as i32,
            reason_detail: Some(detail.into()),
            rejected_at: Some(prost_types::Timestamp {
                seconds: clock::now_secs_i64(),
                nanos: 0,
            }),
        }
    }

    /// Deadline for the confirm-time wait on an on-demand split. Honors the
    /// `respond_by` the relay forwarded in the confirm (minus a response margin
    /// so our answer beats the relay's own timeout), clamped to
    /// `MAX_CONFIRM_SPLIT_WAIT`. A `respond_by` already in the past yields a
    /// zero-length wait (the relay has given up — reject immediately). When the
    /// field is absent (older relay) we fall back to the historical bounded wait.
    fn confirm_split_deadline(&self, req: &RfqConfirmRequest) -> Instant {
        let start = Instant::now();
        let now_ms = i64::try_from(clock::now_millis()).unwrap_or(i64::MAX);
        let wait = confirm_split_wait(req.respond_by.as_ref(), now_ms);
        start.checked_add(wait).unwrap_or(start)
    }

    /// Release everything a failed confirm acquired, and drop the pending
    /// entry. `holding_cids`/`ticket` are whatever the pipeline had acquired
    /// by the failure point.
    async fn release_on_reject(&self, quote_id: &str, holding_cids: &[String], ticket_assigned: bool) {
        self.liquidity_manager.release(&Self::lm_key(quote_id)).await;
        if let Some(tracker) = &self.net_positions {
            tracker.release(quote_id);
        }
        if !holding_cids.is_empty() {
            self.cache.release_reservations(holding_cids).await;
        }
        if ticket_assigned {
            if let Some(pool) = &self.ticket_pool {
                pool.unassign(quote_id);
            }
        }
        sync::lock(&self.pending).remove(quote_id);
    }

    /// Count a confirm's signed base delta if `accept` passes the user's current
    /// net, in one step; without a tracker `accept` sees 0 and nothing is counted.
    fn count_confirm(
        &self,
        quote_id: &str,
        user_party: Option<&str>,
        base_token: &str,
        delta: f64,
        accept: impl FnOnce(f64) -> bool,
    ) -> bool {
        match &self.net_positions {
            Some(tracker) => tracker.check_and_record(quote_id, user_party, base_token, delta, accept),
            None => accept(0.0),
        }
    }

    /// Phase-2 confirm handler (design §5.5 steps 1-7).
    pub async fn handle_confirm(
        &self,
        req: RfqConfirmRequest,
    ) -> Result<AtomicQuoteEnvelope, RfqConfirmReject> {
        let quote_id = req.quote_id.clone();
        #[cfg(test)]
        if quote_id == tests::PANIC_QUOTE_ID {
            panic!("injected confirm panic");
        }
        let now = Instant::now();
        let start_micros = clock::now_micros_i64();

        // Step 1 — lookup + guards (idempotent re-send for already-Confirmed)
        enum Lookup {
            NotFound,
            Resend(Box<AtomicQuoteEnvelope>),
            Restored,
            InProgress,
            Dead,
            ExpiredIndicative,
            FeeMismatch,
            Live(Box<LiveQuote>),
        }
        /// The Indicative entry's fields a live confirm needs (boxed to keep
        /// the Lookup enum small).
        struct LiveQuote {
            market_id: String,
            side: QuoteSide,
            base_amount: Decimal,
            quote_amount: Decimal,
            lp_pays: (InstrumentKey, Decimal),
            lp_pays_token: String,
            notional_usd: Option<f64>,
            settlement_fee: Option<AtomicFeeSpec>,
            user_party: Option<String>,
            pool_inputs: Option<PoolPricingSnapshot>,
        }
        let lookup = {
            let mut pending = sync::lock(&self.pending);
            match pending.get_mut(&quote_id) {
                None => Lookup::NotFound,
                Some(entry) => match *entry {
                    PendingV2::Confirmed { envelope: Some(ref env), .. } => {
                        Lookup::Resend(Box::new(env.clone()))
                    }
                    PendingV2::Confirmed { envelope: None, .. } => Lookup::Restored,
                    PendingV2::Confirming { .. } => Lookup::InProgress,
                    PendingV2::Settled { .. } | PendingV2::Expired { .. } => Lookup::Dead,
                    PendingV2::Indicative { ref valid_until, .. } if now >= *valid_until => {
                        Lookup::ExpiredIndicative
                    }
                    // Defense-in-depth (design §14 D18): the confirm echoes the
                    // cached fee — it must equal what this LP received with the RFQ fan-out.
                    PendingV2::Indicative { ref settlement_fee, .. }
                        if req.settlement_fee != *settlement_fee =>
                    {
                        Lookup::FeeMismatch
                    }
                    // Claim the quote, so a concurrent confirm sees it in progress.
                    PendingV2::Indicative { .. } => {
                        match std::mem::replace(entry, PendingV2::Confirming { since: now }) {
                            PendingV2::Indicative {
                                market_id,
                                side,
                                base_amount,
                                quote_amount,
                                lp_pays,
                                lp_pays_token,
                                notional_usd,
                                valid_until: _,
                                settlement_fee,
                                user_party,
                                pool_inputs,
                            } => Lookup::Live(Box::new(LiveQuote {
                                market_id,
                                side,
                                base_amount,
                                quote_amount,
                                lp_pays,
                                lp_pays_token,
                                notional_usd,
                                settlement_fee,
                                user_party,
                                pool_inputs,
                            })),
                            other => {
                                *entry = other;
                                Lookup::InProgress
                            }
                        }
                    }
                },
            }
        };
        let live: LiveQuote = match lookup {
            Lookup::NotFound => {
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::QuoteNotFound,
                    "unknown quote_id",
                ));
            }
            Lookup::Resend(env) => {
                debug!("Confirm {}: idempotent envelope re-send", quote_id);
                return Ok(*env);
            }
            Lookup::Restored => {
                // Restored after restart: the reservation backs a possibly
                // live envelope — do NOT release anything here.
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::QuoteNotFound,
                    "LP restarted — envelope state lost",
                ));
            }
            Lookup::InProgress => {
                info!("Confirm {}: duplicate while the first confirm is in progress", quote_id);
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::InternalError,
                    "confirm already in progress",
                ));
            }
            Lookup::FeeMismatch => {
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::InternalError,
                    "confirm settlement_fee does not match the RFQ-time fee",
                ));
            }
            Lookup::Dead => {
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::QuoteExpired,
                    "quote no longer live",
                ));
            }
            Lookup::ExpiredIndicative => {
                // eager entry drop (no LM commitment exists at indicative;
                // release_on_reject's LM part is an idempotent no-op)
                self.release_on_reject(&quote_id, &[], false).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::QuoteExpired,
                    "indicative quote expired",
                ));
            }
            Lookup::Live(live) => *live,
        };
        let LiveQuote {
            market_id,
            side,
            base_amount,
            quote_amount,
            lp_pays,
            lp_pays_token,
            notional_usd,
            settlement_fee,
            user_party,
            pool_inputs,
        } = live;

        // The signed base delta this confirm adds to the user's net position.
        let base_token = market_id.split('-').next().unwrap_or("");
        let delta = base_amount.to_f64().unwrap_or(0.0)
            * match side {
                QuoteSide::Buy => 1.0,
                QuoteSide::Sell => -1.0,
            };
        let impact_cfg = self
            .base_config
            .markets
            .iter()
            .find(|mk| mk.market_id == market_id)
            .and_then(|mk| mk.rfq.as_ref())
            .and_then(|r| r.pool_impact.as_ref());
        let recheck_inputs = pool_inputs.as_ref().filter(|i| i.impact_pct > 0.0);
        // Counted at once so concurrent confirms and new quotes see it; a reject takes it back.
        if self.mid_prices.is_none() || recheck_inputs.is_none() || impact_cfg.is_none() {
            self.count_confirm(&quote_id, user_party.as_deref(), base_token, delta, |_| true);
        }

        // The indicative may have been priced off a mid that has since
        // vanished, so reject rather than sign it.
        if let Some(mids) = &self.mid_prices {
            let current = mids.read().await.get(&market_id).cloned();
            let mid_ok = current
                .as_ref()
                .is_some_and(|m| m.mid.is_finite() && m.mid > 0.0);
            if !mid_ok {
                warn!(
                    "Confirm {}: refusing to sign — no reliable mid for {}",
                    quote_id, market_id
                );
                self.release_on_reject(&quote_id, &[], false).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::QuoteExpired,
                    format!("no reliable price for {market_id} — quote withdrawn"),
                ));
            }

            // Staleness re-check, armed only when the adjustment applied — a
            // zero-adjustment quote must not inherit a rejection surface.
            if let (Some(inputs), Some(cfg)) = (recheck_inputs, impact_cfg) {
                let depth_now = current.as_ref().and_then(|m| m.pool_depth.as_ref());
                if let (Some(depth), Some(mid_now)) = (depth_now, current.as_ref().map(|m| m.mid)) {
                    let q = base_amount.to_f64().unwrap_or(0.0);
                    let (impact_side, sign) = match side {
                        QuoteSide::Buy => (ImpactSide::UserBuys, 1.0),
                        QuoteSide::Sell => (ImpactSide::UserSells, -1.0),
                    };
                    let held = if base_amount > Decimal::ZERO {
                        match quote_amount.checked_div(base_amount).and_then(|p| p.to_f64()) {
                            Some(p) => p,
                            None => {
                                warn!(
                                    "Confirm {}: held price {}/{} out of range — rejecting",
                                    quote_id, quote_amount, base_amount
                                );
                                self.release_on_reject(&quote_id, &[], false).await;
                                return Err(self.reject(
                                    &req,
                                    RfqConfirmRejectReason::QuoteExpired,
                                    format!(
                                        "price out of range for {market_id} — quote withdrawn"
                                    ),
                                ));
                            }
                        }
                    } else {
                        0.0
                    };
                    let tol = cfg.confirm_tolerance_percent.max(0.0) / 100.0;
                    // Checked against the net with every other confirm counted,
                    // and counted in the same step; nothing awaits in between.
                    let mut seen = (0.0, 0.0, 0.0);
                    let mut saturated = false;
                    let accepted = self.count_confirm(&quote_id, user_party.as_deref(), base_token, delta, |net_now| {
                        let impact_now =
                            pool_impact::pool_impact_percent(impact_side, q, net_now, depth, cfg);
                        let fair_now =
                            mid_now * (1.0 + sign * (inputs.eff_spread_base + impact_now) / 100.0);
                        // A non-positive fair price means the term saturated past
                        // 100%: reject rather than pass silently.
                        saturated = !(fair_now.is_finite() && fair_now > 0.0);
                        seen = (net_now, impact_now, fair_now);
                        let taker_favourable = held > 0.0
                            && (saturated
                                || match side {
                                    QuoteSide::Buy => held < fair_now * (1.0 - tol),
                                    QuoteSide::Sell => held > fair_now * (1.0 + tol),
                                });
                        !taker_favourable
                    });
                    let (net_now, impact_now, fair_now) = seen;
                    if saturated {
                        warn!(
                            "Confirm {}: impact saturated (fair price {:.10} <= 0 at net {:.0}) \
                             — rejecting rather than failing open",
                            quote_id, fair_now, net_now
                        );
                    }
                    if !accepted {
                        warn!(
                            "Confirm {}: held price {:.10} is taker-favourable vs fair {:.10} \
                             (mid_now={}, R now={} was={}, net now={:.0} was={:.0}, \
                             impact now={:.3}% was={:.3}%) — rejecting stale quote",
                            quote_id,
                            held,
                            fair_now,
                            mid_now,
                            depth.base_reserve,
                            inputs.base_reserve,
                            net_now,
                            inputs.net_used,
                            impact_now,
                            inputs.impact_pct,
                        );
                        self.release_on_reject(&quote_id, &[], false).await;
                        return Err(self.reject(
                            &req,
                            RfqConfirmRejectReason::QuoteExpired,
                            format!(
                                "market moved since quote for {market_id} — quote withdrawn"
                            ),
                        ));
                    }
                } else {
                    // No depth to re-check against: counted without a check.
                    self.count_confirm(&quote_id, user_party.as_deref(), base_token, delta, |_| true);
                }
            }
        }

        // The validity window must be representable before anything is committed.
        let Some(window) = self.quote_window(now, start_micros) else {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InternalError,
                "quote validity window out of range",
            ));
        };

        // Step 1.5 — funds commitment. Indicative quotes only CHECK
        // availability (register_indicative); the atomic check-and-reserve
        // happens here, first — try_commit re-checks under the LM write lock,
        // failing fast on the contended resource before any expensive work
        // (venue, cid selection, split wait, ticket, signing). Held through
        // Confirmed until settle-observed or the expiry sweep releases it.
        // Every later failure exit goes through release_on_reject, whose
        // first act is the LM release. (CC legs are conservatively excluded
        // twice — cache totals feed update_cc_balance AND this commitment —
        // for the bounded confirm→settle window; accepted.)
        // Refuse to commit from a stale balance, mirroring the price
        // staleness re-check above.
        if let Some(age) = self.liquidity_manager.is_stale(&lp_pays_token).await {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::QuoteExpired,
                format!(
                    "balances stale for {}s — quote withdrawn",
                    age.as_secs()
                ),
            ));
        }
        if let Err(e) = self
            .liquidity_manager
            .try_commit(&Self::lm_key(&quote_id), &lp_pays_token, lp_pays.1, Decimal::ZERO)
            .await
        {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InsufficientHoldings,
                format!("liquidity commit failed: {e}"),
            ));
        }

        // Step 2 — venue (leg orientation was fixed at phase 1: user Buy ⇒ LP
        // pays base; user Sell ⇒ LP pays quote)
        let Some(venue) = self.venue_registry.validated(&market_id) else {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::VenueUnavailable,
                "no validated AtomicDVP venue for market",
            ));
        };
        let mi = self.market_instruments.get(&market_id).cloned();
        let Some(mi) = mi else {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::VenueUnavailable,
                "market instruments unresolved",
            ));
        };
        let is_cc = match side {
            QuoteSide::Buy => mi.base_is_cc,
            QuoteSide::Sell => mi.quote_is_cc,
        };

        // Step 3 — hard reserve (physical, cid-level)
        let max_inputs = self
            .market_v2
            .get(&market_id)
            .map(|m| m.max_input_holdings)
            .unwrap_or(100);
        let expires_at = window.expires_at;
        let mut picks = self
            .cache
            .select_for_disclosure(&lp_pays.0, lp_pays.1, max_inputs, is_cc)
            .await;
        // The disclosable rungs this leg needs may still sit in the splitter
        // reserve (a prior swap consumed the last pre-split rung, or the
        // maintenance split lagged behind ledger instability). Kick an on-demand
        // split if one isn't already running — reusing the single-flight
        // quote-time kicker — then wait for it to land, up to the deadline the
        // relay forwarded in `respond_by`. We must not sign a quote we cannot
        // fund, but neither should we reject one whose split is seconds away (the
        // old fixed 6 s wait missed ~8 s devnet splits by a hair). A no-op kick
        // (no ladder/reserve configured) leaves `split_in_flight` false, so an
        // unfundable leg still fast-rejects instead of waiting out the deadline.
        if picks.is_none() {
            if !self.split_in_flight(&lp_pays.0) {
                self.kick_on_demand_split(&lp_pays.0);
            }
            if self.split_in_flight(&lp_pays.0) {
                let deadline = self.confirm_split_deadline(&req);
                while picks.is_none() && Instant::now() < deadline {
                    tokio::time::sleep(CONFIRM_SPLIT_POLL).await;
                    picks = self
                        .cache
                        .select_for_disclosure(&lp_pays.0, lp_pays.1, max_inputs, is_cc)
                        .await;
                }
            }
        }
        // Confirms run concurrently: when another one reserves the picked
        // holdings first, select again from what is left.
        let mut lost_races: u32 = 0;
        let (picks, holding_cids) = loop {
            let Some(found) = picks else {
                self.release_on_reject(&quote_id, &[], false).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::InsufficientHoldings,
                    "insufficient disclosable holdings",
                ));
            };
            let cids: Vec<String> = found.iter().map(|h| h.contract_id.clone()).collect();
            #[cfg(test)]
            self.run_reserve_race_for_tests(&cids, expires_at).await;
            if self.cache.reserve_v2(&cids, &quote_id, expires_at).await {
                break (found, cids);
            }
            lost_races = lost_races.saturating_add(1);
            if lost_races >= RESERVE_ATTEMPTS {
                self.release_on_reject(&quote_id, &[], false).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::InsufficientHoldings,
                    "holdings reservation raced",
                ));
            }
            picks = self
                .cache
                .select_for_disclosure(&lp_pays.0, lp_pays.1, max_inputs, is_cc)
                .await;
        };

        // Step 4 — ticket decision (D1): USD notional >= threshold ⇒ ticketed;
        // USD rate unavailable ⇒ fail CLOSED to ticketed. Threshold unset ⇒
        // always ticketless.
        let ticket = match self.v2.ticket_threshold_usd {
            None => None,
            Some(threshold) => {
                let ticketed = notional_usd.map_or(true, |n| n >= threshold);
                if !ticketed {
                    None
                } else {
                    let assigned = self
                        .ticket_pool
                        .as_ref()
                        .and_then(|pool| pool.assign(&quote_id, expires_at));
                    match assigned {
                        Some(t) => Some(t),
                        None => {
                            self.release_on_reject(&quote_id, &holding_cids, false).await;
                            return Err(self.reject(
                                &req,
                                RfqConfirmRejectReason::NoTicketAvailable,
                                "ticket pool empty",
                            ));
                        }
                    }
                }
            }
        };
        let ticket_id = ticket.as_ref().map(|(tid, _)| tid.clone()).unwrap_or_default();

        // Step 5 — build the DAML Quote from the HELD indicative price (no
        // re-pricing; the relay rejects amount drift anyway).
        // createdAtMicros is backdated 10 s: assertDeadlineExceeded on-ledger
        // needs createdAt strictly in the past across clock skew.
        let now_micros = clock::now_micros_i64();
        let created_at_micros = now_micros.saturating_sub(10_000_000);
        // Counted from the confirm start, like the reservations backing it.
        let valid_until_micros = window.valid_until_micros;
        if now_micros >= valid_until_micros {
            self.release_on_reject(&quote_id, &holding_cids, ticket.is_some()).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::QuoteExpired,
                "quote validity elapsed during confirm",
            ));
        }

        let (base_amount_str, quote_amount_str) =
            match (render_decimal(base_amount), render_decimal(quote_amount)) {
                (Ok(b), Ok(q)) => (b, q),
                _ => {
                    self.release_on_reject(&quote_id, &holding_cids, ticket.is_some()).await;
                    return Err(self.reject(
                        &req,
                        RfqConfirmRejectReason::InternalError,
                        "amount rendering failed",
                    ));
                }
            };

        // The settlement fee is signed INTO the quote (Quote.lpFees -> message
        // v4); the atomic-quote crate selects v3/v4 by lp_fees presence.
        let lp_fees_json: Option<Vec<LpFeeJson>> = settlement_fee.as_ref().map(|f| {
            vec![LpFeeJson {
                receiver: f.receiver.clone(),
                instrument_id: InstrumentIdJson {
                    admin: f.instrument_admin.clone(),
                    id: f.instrument_id.clone(),
                },
                amount: f.amount.clone(),
            }]
        });
        let quote_json = QuoteJson {
            quote_id: quote_id.clone(),
            ticket_id: ticket_id.clone(),
            user: req.user_party.clone(),
            side: side.daml().to_string(),
            base_amount: base_amount_str.clone(),
            quote_amount: quote_amount_str.clone(),
            created_at_micros: created_at_micros.to_string(),
            valid_until_micros: valid_until_micros.to_string(),
            lp_fees: lp_fees_json,
        };

        // Step 6 — canonical + sign + SELF-VERIFY against the venue's
        // on-ledger quotePublicKey: a mismatch means the venue key rotated
        // under us — reject VENUE_UNAVAILABLE (registry re-validates via the
        // updates watcher).
        let canonical = match canonical_from_dvp(&venue.payload, &quote_json) {
            Ok(c) => c,
            Err(e) => {
                self.release_on_reject(&quote_id, &holding_cids, ticket.is_some()).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::VenueUnavailable,
                    format!("canonical build failed: {e}"),
                ));
            }
        };
        let signed = self.quote_key.scalar().map_err(anyhow::Error::from).and_then(|k| sign_quote_scalar(&k, &canonical));
        let signature = match signed {
            Ok(s) => s,
            Err(e) => {
                self.release_on_reject(&quote_id, &holding_cids, ticket.is_some()).await;
                return Err(self.reject(
                    &req,
                    RfqConfirmRejectReason::InternalError,
                    format!("quote signing failed: {e}"),
                ));
            }
        };
        if !verify_quote(&signature, &canonical, &venue.quote_public_key) {
            self.release_on_reject(&quote_id, &holding_cids, ticket.is_some()).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::VenueUnavailable,
                "quote key does not match on-ledger venue key (rotated?)",
            ));
        }

        // Step 7 — envelope assembly
        let mut disclosed = vec![AtomicDisclosedContract {
            contract_id: venue.contract_id.clone(),
            template_id: venue.template_id.clone(),
            created_event_blob: venue.created_event_blob.clone(),
            synchronizer_id: venue.synchronizer_id.clone(),
        }];
        let ticket_acs = ticket.as_ref().map(|(_, entry)| {
            // assign() only returns disclosable entries — fields present
            let template_id = entry.template_id.clone().unwrap_or_default();
            let blob = entry.created_event_blob.clone().unwrap_or_default();
            disclosed.push(AtomicDisclosedContract {
                contract_id: entry.contract_id.clone(),
                template_id: template_id.clone(),
                created_event_blob: blob.clone(),
                synchronizer_id: self.synchronizer_id.clone(),
            });
            AtomicAcsContract {
                contract_id: entry.contract_id.clone(),
                template_id,
                created_event_blob: blob,
                payload_json: entry.payload_json.clone().unwrap_or_default(),
            }
        });
        for h in &picks {
            disclosed.push(AtomicDisclosedContract {
                contract_id: h.contract_id.clone(),
                template_id: h.template_id.clone(),
                created_event_blob: h.created_event_blob.clone().unwrap_or_default(),
                synchronizer_id: h.synchronizer_id.clone(),
            });
        }

        let envelope = AtomicQuoteEnvelope {
            version: ENVELOPE_VERSION.to_string(),
            synchronizer_id: self.synchronizer_id.clone(),
            dvp: Some(AtomicAcsContract {
                contract_id: venue.contract_id.clone(),
                template_id: venue.template_id.clone(),
                created_event_blob: venue.created_event_blob.clone(),
                payload_json: serde_json::to_string(&venue.payload).unwrap_or_default(),
            }),
            quote: Some(AtomicQuote {
                quote_id: quote_id.clone(),
                ticket_id: ticket_id.clone(),
                user: req.user_party.clone(),
                side: side.daml().to_string(),
                base_amount: base_amount_str,
                quote_amount: quote_amount_str,
                created_at_micros,
                valid_until_micros,
                lp_fees: settlement_fee.iter().cloned().collect(),
            }),
            canonical_message: canonical,
            quote_signature: signature,
            ticket: ticket_acs,
            lp_input_holding_cids: holding_cids.clone(),
            disclosed,
            rfq_id: req.rfq_id.clone(),
            quote_id: quote_id.clone(),
            lp_party_id: self.party_id.clone(),
            market_id: market_id.clone(),
            // Stamped downstream by orderbook-rpc (the LP has no HTTP/ledger
            // registry access); the per-instrument utility TransferRule lets
            // the taker build the two-step accept context seedlessly.
            utility_accept_refs: Vec::new(),
        };

        // The Step-1.5 LM commitment persists into Confirmed: cache cid
        // reservations feed the LiquidityManager only for CC (the ACS worker's
        // update_cc_balance) — non-CC balances come from GetBalances unlocked,
        // blind to cache reservations — so the commitment is the ONLY thing
        // excluding a confirmed non-CC leg from the balance gate. Released by
        // settle-observed or the Confirmed-expiry sweep.

        let ticket_cid = ticket.as_ref().map(|(_, e)| e.contract_id.clone());
        // Issue only if this confirm still owns the quote; the sweep may have
        // given it up as abandoned and released its commitment.
        let issued = {
            let mut pending = sync::lock(&self.pending);
            let owned = matches!(pending.get(&quote_id), Some(PendingV2::Confirming { .. }));
            if owned {
                pending.insert(
                    quote_id.clone(),
                    PendingV2::Confirmed {
                        holding_cids: holding_cids.clone(),
                        ticket_id: ticket_id.clone(),
                        ticket_cid: ticket_cid.clone(),
                        envelope: Some(envelope.clone()),
                        valid_until_micros,
                        lp_pays_token,
                        lp_pays_amount: lp_pays.1,
                        expires_at,
                        market_id: market_id.clone(),
                    },
                );
            }
            owned
        };
        if !issued {
            warn!("Confirm {}: abandoned while in progress — envelope withheld", quote_id);
            self.liquidity_manager.release(&Self::lm_key(&quote_id)).await;
            if let Some(tracker) = &self.net_positions {
                tracker.release(&quote_id);
            }
            self.cache.release_reservations(&holding_cids).await;
            if ticket.is_some() {
                if let Some(pool) = &self.ticket_pool {
                    pool.unassign(&quote_id);
                }
            }
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InternalError,
                "confirm abandoned — quote withdrawn",
            ));
        }
        {
            let mut corr = sync::lock(&self.correlation);
            for cid in &holding_cids {
                corr.insert(cid.clone(), quote_id.clone());
            }
            if let Some(tc) = &ticket_cid {
                corr.insert(tc.clone(), quote_id.clone());
            }
        }

        // Counted when the confirm was claimed or re-checked; a no-op then.
        if let Some(tracker) = &self.net_positions {
            tracker.record_confirm(&quote_id, user_party.as_deref(), base_token, delta);
        }

        info!(
            "Confirm {}: envelope issued (market={}, side={:?}, ticket={}, holdings={})",
            quote_id,
            market_id,
            side,
            if ticket_id.is_empty() { "none" } else { &ticket_id },
            envelope.lp_input_holding_cids.len()
        );
        Ok(envelope)
    }

    // ------------------------------------------------------------------
    // Settle detection + sweep
    // ------------------------------------------------------------------

    /// The updates watcher observed consumption of a V2-reserved contract.
    pub async fn handle_settle_observed(&self, quote_id: &str, update_id: &str) {
        #[cfg(test)]
        if quote_id == tests::PANIC_QUOTE_ID {
            panic!("injected settle panic");
        }
        let action = {
            let mut pending = sync::lock(&self.pending);
            let info = match pending.get(quote_id) {
                Some(PendingV2::Confirmed {
                    lp_pays_token,
                    lp_pays_amount,
                    ticket_id,
                    ..
                }) => Some((lp_pays_token.clone(), *lp_pays_amount, !ticket_id.is_empty())),
                Some(PendingV2::Settled { .. }) => {
                    // H5 monitoring for free: a second observed fill of the
                    // same quote_id is the repeated-quoteId alarm.
                    warn!(
                        "SettleObserved for already-Settled quote {} (update {}) — repeated fill?!",
                        quote_id, update_id
                    );
                    None
                }
                _ => {
                    debug!(
                        "SettleObserved for quote {} in non-Confirmed state (update {})",
                        quote_id, update_id
                    );
                    None
                }
            };
            if info.is_some() {
                pending.insert(
                    quote_id.to_string(),
                    PendingV2::Settled { since: Instant::now() },
                );
            }
            info
        };

        if let Some((token, amount, ticketed)) = action {
            info!(
                "V2 settle observed: quote={} update={} lp_pays={} {}",
                quote_id, update_id, amount, token
            );
            // Outflow for depletion pricing — at settle time (v1 records at
            // proposal time). Restored entries have no token info; skip.
            if !token.is_empty() {
                self.liquidity_manager
                    .record_outflow(&token, amount.to_f64().unwrap_or(0.0))
                    .await;
            }
            if ticketed {
                if let Some(pool) = &self.ticket_pool {
                    pool.mark_spent(quote_id);
                }
            }
            // Primary release of the Step-1.5 confirm-time commitment: the
            // settle landed, the funds have physically left.
            self.liquidity_manager.release(&Self::lm_key(quote_id)).await;
            // Net-position accounting: the confirm-time soft count becomes
            // AUTHORITATIVE (the settle is on-ledger) — finalize it.
            if let Some(tracker) = &self.net_positions {
                tracker.settle(quote_id);
            }
        }
    }

    /// Expire stale entries: Indicative past validity (drop entry — no LM
    /// commitment to release), Confirmed past valid_until+grace (release the
    /// confirm-time LM commitment + holdings + ticket), GC tombstones +
    /// correlation entries. Called every ~10 s from the stream task; the
    /// caches' own TTLs are the backstop when the stream is down.
    pub async fn sweep(&self, now: Instant) {
        struct Release {
            quote_id: String,
            lm: bool,
            holding_cids: Vec<String>,
            ticket: bool,
        }
        let mut releases: Vec<Release> = Vec::new();
        let mut gc: Vec<String> = Vec::new();

        {
            let mut pending = sync::lock(&self.pending);
            let mut transitions: Vec<(String, PendingV2)> = Vec::new();
            for (quote_id, entry) in pending.iter() {
                match entry {
                    PendingV2::Indicative { valid_until, .. } if now >= *valid_until => {
                        releases.push(Release {
                            quote_id: quote_id.clone(),
                            // Indicative quotes hold no LM commitment (advisory
                            // check only) — nothing to release.
                            lm: false,
                            holding_cids: Vec::new(),
                            ticket: false,
                        });
                        transitions
                            .push((quote_id.clone(), PendingV2::Expired { since: now }));
                    }
                    PendingV2::Confirmed {
                        expires_at,
                        holding_cids,
                        ticket_id,
                        ..
                    } if now >= *expires_at => {
                        releases.push(Release {
                            quote_id: quote_id.clone(),
                            lm: true, // releases the confirm-time commitment on expiry
                            holding_cids: holding_cids.clone(),
                            ticket: !ticket_id.is_empty(),
                        });
                        transitions
                            .push((quote_id.clone(), PendingV2::Expired { since: now }));
                    }
                    // A running confirm is left alone unless it outlived any
                    // legitimate confirm; then its commitment is released.
                    PendingV2::Confirming { since }
                        if now.saturating_duration_since(*since) > CONFIRM_ABANDONED_AFTER =>
                    {
                        warn!("V2 sweep: confirm {} treated as abandoned", quote_id);
                        releases.push(Release {
                            quote_id: quote_id.clone(),
                            lm: true,
                            holding_cids: Vec::new(),
                            ticket: true,
                        });
                        transitions
                            .push((quote_id.clone(), PendingV2::Expired { since: now }));
                    }
                    PendingV2::Settled { since } | PendingV2::Expired { since }
                        if now.saturating_duration_since(*since) > TOMBSTONE_TTL =>
                    {
                        gc.push(quote_id.clone());
                    }
                    _ => {}
                }
            }
            for (quote_id, next) in transitions {
                pending.insert(quote_id, next);
            }
            for quote_id in &gc {
                pending.remove(quote_id);
            }
        }

        if !gc.is_empty() {
            let mut corr = sync::lock(&self.correlation);
            corr.retain(|_, qid| !gc.contains(qid));
        }

        for r in releases {
            info!("V2 sweep: expiring quote {}", r.quote_id);
            if r.lm {
                self.liquidity_manager.release(&Self::lm_key(&r.quote_id)).await;
                // An expired or abandoned confirm: reverse its net-position
                // soft count (no-op for quote_ids never counted).
                if let Some(tracker) = &self.net_positions {
                    tracker.release(&r.quote_id);
                }
            }
            if !r.holding_cids.is_empty() {
                self.cache.release_reservations(&r.holding_cids).await;
            }
            if r.ticket {
                if let Some(pool) = &self.ticket_pool {
                    pool.unassign(&r.quote_id);
                }
            }
        }

        if let Some(pool) = &self.ticket_pool {
            pool.expire_assignments(now);
        }

        // Net-tracker housekeeping on the same sweep: reverse unreachable
        // pendings and checkpoint the state file.
        if let Some(tracker) = &self.net_positions {
            tracker.expire_stale_pending();
            tracker.checkpoint_if_dirty();
        }
    }

    // ------------------------------------------------------------------
    // Persistence (design §5.7)
    // ------------------------------------------------------------------

    /// Snapshot Confirmed quotes for SavedState (sync — called from the
    /// runner's save closure).
    pub fn snapshot_pending(&self) -> Vec<SavedPendingV2> {
        sync::lock(&self.pending)
            .iter()
            .filter_map(|(quote_id, entry)| match entry {
                PendingV2::Confirmed {
                    holding_cids,
                    ticket_id,
                    valid_until_micros,
                    market_id,
                    ..
                } => Some(SavedPendingV2 {
                    quote_id: quote_id.clone(),
                    market_id: market_id.clone(),
                    holding_cids: holding_cids.clone(),
                    ticket_id: ticket_id.clone(),
                    valid_until_micros: *valid_until_micros,
                }),
                _ => None,
            })
            .collect()
    }

    /// Restore Confirmed quotes after restart. MUST run before the first ACS
    /// refresh / worker start so the LP never double-discloses holdings that
    /// back a possibly-live envelope. The restored expiry is capped at the
    /// protocol maximum validity plus grace.
    pub async fn restore_pending(&self, saved: Vec<SavedPendingV2>) {
        let now_micros = clock::now_micros_i64();
        let grace_micros = i64::try_from(self.v2.settle_grace_secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000_000);
        let max_ttl = Self::restore_ttl_cap();
        for p in saved {
            let remaining_micros = p
                .valid_until_micros
                .saturating_add(grace_micros)
                .saturating_sub(now_micros);
            if remaining_micros <= 0 {
                debug!("Skipping expired saved V2 quote {}", p.quote_id);
                continue;
            }
            let remaining =
                Duration::from_micros(u64::try_from(remaining_micros).unwrap_or(u64::MAX))
                    .min(max_ttl);
            let Some(expires_at) = Instant::now().checked_add(remaining) else {
                warn!("Skipping saved V2 quote {}: expiry out of range", p.quote_id);
                continue;
            };
            self.cache
                .restore_v2_reservation(&p.holding_cids, &p.quote_id, expires_at)
                .await;
            {
                let mut corr = sync::lock(&self.correlation);
                for cid in &p.holding_cids {
                    corr.insert(cid.clone(), p.quote_id.clone());
                }
            }
            info!(
                "Restored Confirmed V2 quote {} ({} holdings, ticket='{}')",
                p.quote_id,
                p.holding_cids.len(),
                p.ticket_id
            );
            sync::lock(&self.pending).insert(
                p.quote_id.clone(),
                PendingV2::Confirmed {
                    holding_cids: p.holding_cids,
                    ticket_id: p.ticket_id,
                    ticket_cid: None,
                    envelope: None,
                    valid_until_micros: p.valid_until_micros,
                    lp_pays_token: String::new(),
                    lp_pays_amount: Decimal::ZERO,
                    expires_at,
                    market_id: p.market_id,
                },
            );
        }
    }

    /// quote_id correlated to a consumed contract id, if any (used by the
    /// updates watcher as a fallback to the cache's reservation kind).
    pub fn quote_for_cid(&self, cid: &str) -> Option<String> {
        sync::lock(&self.correlation).get(cid).cloned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::rfq_handler::PricedQuote;
    use crate::venue_registry::VenueRegistry;
    use orderbook_proto::rfqv2::RfqConfirmRequest;
    use std::str::FromStr;

    pub(crate) const MARKET: &str = "EDELx-USDCx";
    pub(crate) const USDCX_KEY: &str = "reg::USDCx";
    /// A quote id whose confirm or settle observation panics.
    pub(crate) const PANIC_QUOTE_ID: &str = "injected-panic";

    /// State harness: one market, empty venue registry (confirm's Step 2
    /// venue lookup fails — deliberate: everything up to and including the
    /// Step-1.5 funds commitment is exercisable without a signable venue).
    pub(crate) fn state_with(lm: Arc<LiquidityManager>) -> RfqV2State {
        state_with_v2(lm, RfqV2Config::default(), &[])
    }

    fn state_with_v2(
        lm: Arc<LiquidityManager>,
        v2: RfqV2Config,
        split_targets: &[crate::split_worker::SplitTarget],
    ) -> RfqV2State {
        let quote_key = agent_logic::config::AtomicQuoteKey::from_scalar_hex(
            &atomic_quote::gen_keypair().unwrap().priv_scalar_hex,
        )
        .unwrap();
        let registry = Arc::new(VenueRegistry::new(
            "lp-party::1220test".to_string(),
            String::new(),
            HashMap::new(),
        ));
        build_state(
            lm,
            v2,
            split_targets,
            quote_key,
            registry,
            crate::holdings_cache::HoldingsCache::new(false),
        )
    }

    fn build_state(
        lm: Arc<LiquidityManager>,
        v2: RfqV2Config,
        split_targets: &[crate::split_worker::SplitTarget],
        quote_key: agent_logic::config::AtomicQuoteKey,
        venue_registry: Arc<VenueRegistry>,
        cache: Arc<HoldingsCache>,
    ) -> RfqV2State {
        let mut market_instruments = HashMap::new();
        market_instruments.insert(
            MARKET.to_string(),
            MarketInstruments {
                base_key: "reg::EDELx".to_string(),
                base_is_cc: false,
                quote_key: USDCX_KEY.to_string(),
                quote_is_cc: false,
            },
        );
        RfqV2State::new(
            "lp-party::1220test".to_string(),
            "LP Test".to_string(),
            "sync::test".to_string(),
            quote_key,
            v2,
            HashMap::new(),
            market_instruments,
            cache,
            None,
            venue_registry,
            lm,
            agent_logic::config::BaseConfig::test_minimal().unwrap(),
            split_targets,
        )
    }

    pub(crate) const HOLDING_CID: &str = "00usdcx-holding";

    /// State whose venue validates and whose cache holds one disclosable
    /// 1000 USDCx holding, so a confirm can sign an envelope.
    pub(crate) async fn signable_state(
        lm: Arc<LiquidityManager>,
        v2: RfqV2Config,
        split_targets: &[crate::split_worker::SplitTarget],
    ) -> RfqV2State {
        let cache = HoldingsCache::new(false);
        cache.add_created(vec![usdcx_holding()]).await;
        venue_state(lm, v2, split_targets, cache)
    }

    /// The one disclosable 1000 USDCx holding of `signable_state`.
    pub(crate) fn usdcx_holding() -> crate::holdings_cache::CachedHolding {
        crate::holdings_cache::CachedHolding {
            contract_id: HOLDING_CID.to_string(),
            template_id: "#utility:Holding".to_string(),
            instrument: USDCX_KEY.to_string(),
            amount: Decimal::from(1000),
            created_event_blob: Some("holding-blob".to_string()),
            synchronizer_id: "sync::test".to_string(),
            discovered_at: Instant::now(),
        }
    }

    /// State whose venue validates, over the given holdings cache.
    pub(crate) fn venue_state(
        lm: Arc<LiquidityManager>,
        v2: RfqV2Config,
        split_targets: &[crate::split_worker::SplitTarget],
        cache: Arc<HoldingsCache>,
    ) -> RfqV2State {
        use crate::venue_registry::{ExpectedVenue, VenueEntry};
        let kf = atomic_quote::gen_keypair().unwrap();
        let quote_key =
            agent_logic::config::AtomicQuoteKey::from_scalar_hex(&kf.priv_scalar_hex).unwrap();
        let party = "lp-party::1220test";
        let expected = HashMap::from([(
            MARKET.to_string(),
            ExpectedVenue {
                base_id: "EDELx".to_string(),
                base_admin: "reg".to_string(),
                quote_id: "USDCx".to_string(),
                quote_admin: "reg".to_string(),
            },
        )]);
        let registry = Arc::new(VenueRegistry::new(
            party.to_string(),
            kf.pub_spki_hex.clone(),
            expected,
        ));
        registry.insert_for_tests(VenueEntry {
            contract_id: "00venue".to_string(),
            template_id: crate::venue_registry::TEMPLATE_ATOMIC_DVP.to_string(),
            created_event_blob: "venue-blob".to_string(),
            payload: serde_json::json!({
                "lp": party,
                "provider": "prov::1220test",
                "pairName": MARKET,
                "baseInstrumentId": {"admin": "reg", "id": "EDELx"},
                "quoteInstrumentId": {"admin": "reg", "id": "USDCx"},
                "quotePublicKey": kf.pub_spki_hex,
            }),
            synchronizer_id: "sync::test".to_string(),
            provider: "prov::1220test".to_string(),
            pair_name: MARKET.to_string(),
            base_admin: "reg".to_string(),
            base_id: "EDELx".to_string(),
            quote_admin: "reg".to_string(),
            quote_id: "USDCx".to_string(),
            quote_public_key: kf.pub_spki_hex.clone(),
        });
        build_state(lm, v2, split_targets, quote_key, registry, cache)
    }

    /// Makes the test market quotable over the atomic stream.
    pub(crate) fn quotable(mut state: RfqV2State) -> RfqV2State {
        state.market_v2.insert(
            MARKET.to_string(),
            serde_json::from_str(r#"{"enabled":true}"#).unwrap(),
        );
        state
    }

    /// Marks a split in flight, so a confirm without holdings waits for one.
    pub(crate) fn mark_split_in_flight(state: &RfqV2State, key: &str) {
        sync::lock(&state.splits_in_flight).insert(key.to_string());
    }

    pub(crate) async fn add_usdcx_holding(state: &RfqV2State) {
        state.cache.add_created(vec![usdcx_holding()]).await;
    }

    pub(crate) async fn lm_with_usdcx(balance: u32) -> Arc<LiquidityManager> {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(balance)).await;
        lm
    }

    /// User Sell ⇒ LP pays the quote leg (USDCx).
    pub(crate) fn priced_sell(lp_pays_usdcx: u32, valid_for_secs: u32) -> PricedQuote {
        let amount = Decimal::from(lp_pays_usdcx);
        PricedQuote {
            market_id: MARKET.to_string(),
            price: Decimal::from_str("0.01").unwrap(),
            quantity: amount * Decimal::from(100),
            quote_quantity: amount,
            price_str: "0.0100000000".to_string(),
            quantity_str: format!("{:.10}", lp_pays_usdcx as f64 * 100.0),
            quote_quantity_str: format!("{:.10}", lp_pays_usdcx as f64),
            lp_pays: ("USDCx".to_string(), amount),
            notional_usd: Some(lp_pays_usdcx as f64),
            valid_for_secs,
            allocate_before_secs: 0,
            settle_before_secs: 0,
            pool_pricing: None,
        }
    }

    pub(crate) fn confirm_req(quote_id: &str) -> RfqConfirmRequest {
        RfqConfirmRequest {
            rfq_id: "rfq-1".to_string(),
            quote_id: quote_id.to_string(),
            user_party: "user::1220test".to_string(),
            market_id: MARKET.to_string(),
            direction: "sell".to_string(),
            quantity: String::new(),
            quote_quantity: String::new(),
            price: String::new(),
            respond_by: None,
            settlement_fee: None,
        }
    }

    #[tokio::test]
    async fn indicative_checks_availability_without_committing() {
        let lm = lm_with_usdcx(1000).await;
        let state = state_with(lm.clone());

        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();

        // Advisory check only: nothing committed, full balance still available.
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
        assert_eq!(state.pending_kind("q1"), Some("Indicative"));

        // Over-quoting across concurrent indicatives is allowed by design:
        // a second 800 quote passes the check even though 500 + 800 > 1000.
        state
            .register_indicative("q2", MARKET, QuoteSide::Sell, &priced_sell(800, 90), None, None)
            .await
            .unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
    }

    #[tokio::test]
    async fn indicative_rejects_when_insufficient() {
        let lm = lm_with_usdcx(1000).await;
        let state = state_with(lm.clone());

        let err = state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(1500, 90), None, None)
            .await
            .unwrap_err();
        assert!(err.contains("insufficient"), "unexpected error: {err}");
        assert_eq!(state.pending_kind("q1"), None);
    }

    #[tokio::test]
    async fn confirm_commits_and_rejects_when_overcommitted() {
        let lm = lm_with_usdcx(1000).await;
        let state = state_with(lm.clone());

        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();

        // A competing commitment (e.g. another quote's confirm) shrinks
        // availability below the LP-pays leg: Step 1.5 must reject with
        // InsufficientHoldings and drop the entry, leaving the competitor's
        // commitment untouched.
        lm.try_commit("competitor", "USDCx", Decimal::from(600), Decimal::ZERO)
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InsufficientHoldings as i32);
        assert_eq!(state.pending_kind("q1"), None);
        assert_eq!(lm.available("USDCx").await, Decimal::from(400)); // competitor only

        // With headroom, Step 1.5 commits — the pipeline then fails at the
        // Step-2 venue lookup (empty registry) and release_on_reject must
        // restore the commitment (commit-then-release ordering).
        lm.release("competitor").await;
        state
            .register_indicative("q3", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q3")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::VenueUnavailable as i32);
        assert_eq!(state.pending_kind("q3"), None);
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
    }

    /// A held price gone taker-favourable is rejected before any commitment;
    /// an unchanged market passes; a quote with no adjustment is never checked.
    #[tokio::test]
    async fn confirm_recheck_rejects_taker_favourable_stale_quote() {
        use agent_logic::pool_impact::PoolDepth;

        let tracker_path = std::env::temp_dir()
            .join(format!("silvana-rfqv2-recheck-{}.json", uuid::Uuid::now_v7()));
        let tracker =
            agent_logic::net_position::NetPositionTracker::load_or_new(
                tracker_path,
                24.0,
                agent_logic::config::RfqV2Config::default().stale_pending_after(),
            );

        let mids: HashMap<String, MarketMid> = HashMap::from([(
            MARKET.to_string(),
            MarketMid {
                mid: 0.01,
                // Huge reserve: impact_now ≈ 0, isolating the mid-move check.
                pool_depth: Some(PoolDepth { base_reserve: 1e12 }),
            },
        )]);
        let mids = Arc::new(tokio::sync::RwLock::new(mids));

        let lm = lm_with_usdcx(10_000).await;
        let mut state = state_with(lm.clone())
            .with_mid_prices(mids.clone())
            .with_net_positions(tracker);
        // The market's pool_impact section (tolerance default 0.25%).
        state.base_config.markets = vec![serde_json::from_str(
            r#"{"market_id":"EDELx-USDCx",
                "rfq":{"min_quantity":"1","max_quantity":"100000000",
                       "pool_impact":{"enabled":true}}}"#,
        )
        .unwrap()];

        // Adjusted indicative: held sell price 0.01 at spread 0. impact_pct
        // must be > 0, since the re-check is scoped to adjusted quotes only.
        let mut priced = priced_sell(500, 90);
        priced.pool_pricing = Some(crate::rfq_handler::PoolPricingSnapshot {
            base_reserve: 1e12,
            net_used: 0.0,
            impact_pct: 0.01,
            eff_spread_base: 0.0,
        });

        // Control: market unchanged → re-check passes, pipeline proceeds to
        // the harness's empty venue registry (VenueUnavailable, NOT expired).
        state
            .register_indicative("q-ok", MARKET, QuoteSide::Sell, &priced, None, Some("user::1"))
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q-ok")).await.unwrap_err();
        assert_eq!(
            reject.reason,
            RfqConfirmRejectReason::VenueUnavailable as i32,
            "unchanged market must pass the re-check: {:?}",
            reject.reason_detail
        );

        // The mid drops 0.01 → 0.009, so the held 0.01 is taker-favourable
        // beyond tolerance and must expire.
        state
            .register_indicative("q-stale", MARKET, QuoteSide::Sell, &priced, None, Some("user::1"))
            .await
            .unwrap();
        mids.write().await.get_mut(MARKET).unwrap().mid = 0.009;
        let reject = state.handle_confirm(confirm_req("q-stale")).await.unwrap_err();
        assert_eq!(
            reject.reason,
            RfqConfirmRejectReason::QuoteExpired as i32,
            "a moved market must expire the held quote: {:?}",
            reject.reason_detail
        );
        assert!(reject.reason_detail.unwrap().contains("market moved"));
        assert_eq!(state.pending_kind("q-stale"), None, "entry dropped");
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000), "nothing committed");

        // Complement: a zero-adjustment quote must NOT expire on the same
        // move, or ordinary volatility would reject honest flow.
        let mut retail = priced_sell(500, 90);
        retail.pool_pricing = Some(crate::rfq_handler::PoolPricingSnapshot {
            base_reserve: 1e12,
            net_used: 0.0,
            impact_pct: 0.0,
            eff_spread_base: 0.0,
        });
        state
            .register_indicative("q-retail", MARKET, QuoteSide::Sell, &retail, None, Some("user::9"))
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q-retail")).await.unwrap_err();
        assert_ne!(
            reject.reason,
            RfqConfirmRejectReason::QuoteExpired as i32,
            "retail quote rejected as stale by a defence that never priced it: {:?}",
            reject.reason_detail
        );

        // Parity: a NON-impact quote (pool_inputs None) is not re-checked
        // even though the mid stays moved — behaves exactly like today.
        state
            .register_indicative(
                "q-legacy",
                MARKET,
                QuoteSide::Sell,
                &priced_sell(500, 90),
                None,
                None,
            )
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q-legacy")).await.unwrap_err();
        assert_eq!(
            reject.reason,
            RfqConfirmRejectReason::VenueUnavailable as i32,
            "legacy quote must skip the re-check: {:?}",
            reject.reason_detail
        );

        // Depth ABSENT at confirm ⇒ fail-open: the impact-priced quote passes.
        state
            .register_indicative("q-nodepth", MARKET, QuoteSide::Sell, &priced, None, Some("user::1"))
            .await
            .unwrap();
        mids.write().await.get_mut(MARKET).unwrap().pool_depth = None;
        let reject = state.handle_confirm(confirm_req("q-nodepth")).await.unwrap_err();
        assert_eq!(
            reject.reason,
            RfqConfirmRejectReason::VenueUnavailable as i32,
            "depth absent at confirm must fail open: {:?}",
            reject.reason_detail
        );
    }

    #[tokio::test]
    async fn sweep_expired_indicative_no_underflow() {
        let lm = lm_with_usdcx(1000).await;
        let state = state_with(lm.clone());

        // valid_for_secs = 0: expired the moment it is registered.
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 0), None, None)
            .await
            .unwrap();
        state.sweep(Instant::now()).await;

        assert_eq!(state.pending_kind("q1"), Some("Expired"));
        // No commitment existed; the sweep must not underflow availability.
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
    }

    fn big(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    // Amounts at or above 1e27 render in the reject text instead of panicking
    #[tokio::test]
    async fn indicative_reject_formats_huge_amounts() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", big("5000000000000000000000000000")).await;
        let state = state_with(lm);
        let mut priced = priced_sell(500, 90);
        priced.lp_pays = ("USDCx".to_string(), big("6000000000000000000000000000"));

        let err = state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced, None, None)
            .await
            .unwrap_err();
        assert!(
            err.starts_with("insufficient USDCx (5000000000000000000000000000"),
            "{err}"
        );
        assert!(err.contains("available, 6000000000000000000000000000"), "{err}");
        assert_eq!(state.pending_kind("q1"), None);
    }

    /// Mid map plus pool_impact config that arms the confirm-time re-check.
    async fn armed_recheck_state(lm: Arc<LiquidityManager>) -> RfqV2State {
        use agent_logic::pool_impact::PoolDepth;
        let mids: HashMap<String, MarketMid> = HashMap::from([(
            MARKET.to_string(),
            MarketMid {
                mid: 0.01,
                pool_depth: Some(PoolDepth { base_reserve: 1e12 }),
            },
        )]);
        let mut state = state_with(lm).with_mid_prices(Arc::new(tokio::sync::RwLock::new(mids)));
        state.base_config.markets = vec![serde_json::from_str(
            r#"{"market_id":"EDELx-USDCx",
                "rfq":{"min_quantity":"1","max_quantity":"100000000",
                       "pool_impact":{"enabled":true}}}"#,
        )
        .unwrap()];
        state
    }

    // A held price outside Decimal range rejects instead of passing the re-check
    #[tokio::test]
    async fn confirm_fails_closed_when_held_price_out_of_range() {
        let lm = lm_with_usdcx(10_000).await;
        let state = armed_recheck_state(lm.clone()).await;
        let mut priced = priced_sell(500, 90);
        priced.quantity = Decimal::new(1, 28);
        priced.quote_quantity = Decimal::from(10_000);
        priced.pool_pricing = Some(crate::rfq_handler::PoolPricingSnapshot {
            base_reserve: 1e12,
            net_used: 0.0,
            impact_pct: 0.01,
            eff_spread_base: 0.0,
        });
        state
            .register_indicative("q-ovf", MARKET, QuoteSide::Sell, &priced, None, None)
            .await
            .unwrap();

        let reject = state.handle_confirm(confirm_req("q-ovf")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::QuoteExpired as i32);
        assert!(reject.reason_detail.unwrap().contains("price out of range"));
        assert_eq!(state.pending_kind("q-ovf"), None, "entry dropped");
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000), "nothing committed");
    }

    // A validity window that does not fit the clock or the signed micros is
    // rejected before any commitment
    #[tokio::test]
    async fn confirm_rejects_unrepresentable_validity_window() {
        let windows = [
            (u64::MAX, 30),
            (60, u64::MAX),
            (u64::try_from(i64::MAX / 1_000_000).unwrap() + 1, 0),
        ];
        for (valid, grace) in windows {
            let lm = lm_with_usdcx(1000).await;
            let v2 = RfqV2Config {
                atomic_quote_valid_secs: valid,
                settle_grace_secs: grace,
                ..RfqV2Config::default()
            };
            let state = state_with_v2(lm.clone(), v2, &[]);
            state
                .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
                .await
                .unwrap();
            let reject = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
            assert_eq!(
                reject.reason,
                RfqConfirmRejectReason::InternalError as i32,
                "valid={valid} grace={grace}: {:?}",
                reject.reason_detail
            );
            assert!(reject.reason_detail.unwrap().contains("validity window"));
            assert_eq!(state.pending_kind("q1"), None);
            assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
        }
    }

    // Out-of-range respond_by values clamp to the wait bounds instead of overflowing
    #[test]
    fn confirm_split_wait_clamps_extreme_respond_by() {
        let now_ms: i64 = 1_800_000_000_000;
        let ts = |seconds: i64, nanos: i32| prost_types::Timestamp { seconds, nanos };
        assert_eq!(confirm_split_wait(None, now_ms), FALLBACK_CONFIRM_SPLIT_WAIT);
        assert_eq!(confirm_split_wait(Some(&ts(i64::MAX, 999_999_999)), now_ms), MAX_CONFIRM_SPLIT_WAIT);
        assert_eq!(confirm_split_wait(Some(&ts(i64::MIN, -999_999_999)), now_ms), Duration::ZERO);
        assert_eq!(confirm_split_wait(Some(&ts(0, 0)), i64::MIN), MAX_CONFIRM_SPLIT_WAIT);
        assert_eq!(confirm_split_wait(Some(&ts(0, 0)), i64::MAX), Duration::ZERO);
        // respond_by 10 s out: wait 10 s minus the response margin
        let by = ts(now_ms / 1000 + 10, 0);
        assert_eq!(confirm_split_wait(Some(&by), now_ms), Duration::from_millis(8_800));
        let by = ts(now_ms / 1000 + 10, -500_000_000);
        assert_eq!(confirm_split_wait(Some(&by), now_ms), Duration::from_millis(8_300));
    }

    #[tokio::test]
    async fn confirm_split_deadline_survives_extreme_respond_by() {
        let state = state_with(lm_with_usdcx(1000).await);
        let mut req = confirm_req("q1");
        req.respond_by = Some(prost_types::Timestamp { seconds: i64::MAX, nanos: i32::MAX });
        let before = Instant::now();
        let deadline = state.confirm_split_deadline(&req);
        assert!(deadline >= before);
        assert!(deadline <= Instant::now() + MAX_CONFIRM_SPLIT_WAIT);
    }

    fn saved(quote_id: &str, valid_until_micros: i64) -> SavedPendingV2 {
        SavedPendingV2 {
            quote_id: quote_id.to_string(),
            market_id: MARKET.to_string(),
            holding_cids: vec![format!("cid-{quote_id}")],
            ticket_id: String::new(),
            valid_until_micros,
        }
    }

    // Far-future saved expiries are capped at the protocol maximum
    #[tokio::test]
    async fn restore_pending_caps_far_future_expiry() {
        let state = state_with(lm_with_usdcx(1000).await);
        let ten_years = agent_logic::clock::now_micros_i64() + 10 * 365 * 24 * 3600 * 1_000_000;
        state
            .restore_pending(vec![saved("q-max", i64::MAX), saved("q-10y", ten_years)])
            .await;
        assert_eq!(state.pending_kind("q-max"), Some("Confirmed"));
        assert_eq!(state.pending_kind("q-10y"), Some("Confirmed"));
        assert_eq!(state.quote_for_cid("cid-q-max").as_deref(), Some("q-max"));

        let cap = RfqV2State::restore_ttl_cap();
        state.sweep(Instant::now() + cap - Duration::from_secs(5)).await;
        assert_eq!(state.pending_kind("q-max"), Some("Confirmed"), "still live inside the cap");
        state.sweep(Instant::now() + cap + Duration::from_secs(1)).await;
        assert_eq!(state.pending_kind("q-max"), Some("Expired"));
        assert_eq!(state.pending_kind("q-10y"), Some("Expired"));
    }

    // A restart with a shorter validity setting used to release a still-valid
    // envelope's holdings at the new setting's TTL
    #[tokio::test]
    async fn restore_pending_keeps_window_signed_under_longer_validity() {
        let v2 = RfqV2Config { atomic_quote_valid_secs: 60, settle_grace_secs: 30, ..RfqV2Config::default() };
        let state = state_with_v2(lm_with_usdcx(1000).await, v2, &[]);
        let valid_until = agent_logic::clock::now_micros_i64() + 500_000_000;
        state.restore_pending(vec![saved("q", valid_until)]).await;
        let now = Instant::now();
        state.sweep(now + Duration::from_secs(120)).await;
        assert_eq!(state.pending_kind("q"), Some("Confirmed"));
        assert_eq!(state.cache.v2_reservation_quote("cid-q").await.as_deref(), Some("q"));
        state.sweep(now + Duration::from_secs(531)).await;
        assert_eq!(state.pending_kind("q"), Some("Expired"));
        assert_eq!(state.cache.v2_reservation_quote("cid-q").await, None);
    }

    // Saved expiries far in the past are skipped without overflowing
    #[tokio::test]
    async fn restore_pending_skips_far_past_expiry() {
        let state = state_with(lm_with_usdcx(1000).await);
        state.restore_pending(vec![saved("q-min", i64::MIN)]).await;
        assert_eq!(state.pending_kind("q-min"), None);
        assert_eq!(state.quote_for_cid("cid-q-min"), None);
    }

    fn usdcx_split_target() -> crate::split_worker::SplitTarget {
        crate::split_worker::SplitTarget {
            instrument: crate::split_worker::SplitInstrument {
                key: USDCX_KEY.to_string(),
                is_cc: false,
                on_chain_id: "USDCx".to_string(),
                admin: "reg".to_string(),
            },
            denominations: vec!["10x5".to_string()],
        }
    }

    // A split job that panics still clears its in-flight mark
    #[tokio::test]
    async fn on_demand_split_clears_in_flight_after_panic() {
        let set = Arc::new(Mutex::new(HashSet::new()));
        let key = USDCX_KEY.to_string();
        let guard = InFlightGuard::claim(&set, &key).unwrap();
        assert!(InFlightGuard::claim(&set, &key).is_none(), "single flight");

        let fail = true;
        let job = async move {
            if fail {
                panic!("split job failed hard");
            }
            Ok(())
        };
        let joined = tokio::spawn(run_on_demand_split(guard, Duration::from_secs(60), job)).await;
        assert!(joined.unwrap_err().is_panic());
        assert!(!set.lock().unwrap().contains(&key), "in-flight mark cleared on panic");
    }

    // A slow split used to be dropped at 120s, possibly mid-submit, and its mark cleared early
    #[tokio::test(start_paused = true)]
    async fn a_slow_on_demand_split_runs_to_its_end_and_keeps_its_mark() {
        let set = Arc::new(Mutex::new(HashSet::new()));
        let key = USDCX_KEY.to_string();
        let guard = InFlightGuard::claim(&set, &key).unwrap();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let job = async move {
            tokio::time::sleep(ON_DEMAND_SPLIT_SLOW_AFTER + Duration::from_secs(30)).await;
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        };
        let split = tokio::spawn(run_on_demand_split(guard, ON_DEMAND_SPLIT_SLOW_AFTER, job));
        tokio::time::sleep(ON_DEMAND_SPLIT_SLOW_AFTER + Duration::from_secs(1)).await;
        assert!(set.lock().unwrap().contains(&key), "still in flight past the slow mark");
        assert!(InFlightGuard::claim(&set, &key).is_none(), "still single flight");
        split.await.unwrap();
        assert!(finished.load(std::sync::atomic::Ordering::SeqCst), "the job was not cut short");
        assert!(!set.lock().unwrap().contains(&key));
    }

    // The kick's own spawn: a split past the slow mark keeps its mark until it ends
    #[tokio::test(start_paused = true)]
    async fn a_kicked_split_past_the_slow_mark_keeps_its_mark_until_it_ends() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let lm = lm_with_usdcx(1000).await;
        let state = state_with_v2(lm, RfqV2Config::default(), &[usdcx_split_target()]);
        let key = USDCX_KEY.to_string();
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        state.kick_split_with(&key, move |_, _, _| async move {
            tokio::time::sleep(ON_DEMAND_SPLIT_SLOW_AFTER + Duration::from_secs(30)).await;
            done.store(true, SeqCst);
            Ok(())
        });
        tokio::time::sleep(ON_DEMAND_SPLIT_SLOW_AFTER + Duration::from_secs(1)).await;
        assert!(state.split_in_flight(&key), "still in flight past the slow mark");
        let kicked_again = AtomicBool::new(false);
        state.kick_split_with(&key, |_, _, _| {
            kicked_again.store(true, SeqCst);
            async { Ok(()) }
        });
        assert!(!kicked_again.load(SeqCst), "a second kick is a no-op");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(finished.load(SeqCst), "the job was not cut short");
        assert!(!state.split_in_flight(&key), "the mark clears when the job ends");
    }

    // Without a runtime the kick logs, spawns nothing and leaves no stale mark
    #[test]
    fn kick_without_runtime_leaves_no_in_flight_mark() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        let state = state_with_v2(lm, RfqV2Config::default(), &[usdcx_split_target()]);
        let key = USDCX_KEY.to_string();
        state.kick_on_demand_split(&key);
        assert!(!state.split_in_flight(&key));
    }

    // In a runtime the kick claims the instrument once and clears it when the job ends
    #[tokio::test]
    async fn kick_is_single_flight_and_clears_when_done() {
        let lm = lm_with_usdcx(1000).await;
        let mut state = state_with_v2(lm, RfqV2Config::default(), &[usdcx_split_target()]);
        state.base_config.orderbook_grpc_url = "http://127.0.0.1:1".to_string();
        let key = USDCX_KEY.to_string();
        state.kick_on_demand_split(&key);
        assert!(state.split_in_flight(&key));
        state.kick_on_demand_split(&key);
        tokio::time::timeout(Duration::from_secs(30), async {
            while state.split_in_flight(&key) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("in-flight mark must clear once the split job ends");
    }

    fn poison<T>(m: &Mutex<T>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = m.lock().unwrap();
            panic!("poison the lock");
        }));
        assert!(m.is_poisoned());
    }

    // The in-flight guard claims and releases through a poisoned lock
    #[test]
    fn in_flight_guard_tolerates_poison() {
        let set = Arc::new(Mutex::new(HashSet::new()));
        poison(&set);
        let key = USDCX_KEY.to_string();
        let guard = InFlightGuard::claim(&set, &key).unwrap();
        assert!(InFlightGuard::claim(&set, &key).is_none());
        drop(guard);
        assert!(!set.lock().unwrap_or_else(|e| e.into_inner()).contains(&key));
    }

    // A full confirm signs the configured validity and holds its reservations
    // for validity plus grace
    #[tokio::test]
    async fn confirm_signs_envelope_within_the_window() {
        let lm = lm_with_usdcx(1000).await;
        let state = signable_state(lm.clone(), RfqV2Config::default(), &[]).await;
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        let env = state.handle_confirm(confirm_req("q1")).await.unwrap();
        let quote = env.quote.unwrap();
        let v2 = RfqV2Config::default();
        let signed = quote.valid_until_micros - quote.created_at_micros;
        let full = i64::try_from(v2.atomic_quote_valid_secs).unwrap() * 1_000_000 + 10_000_000;
        assert!(signed <= full && signed > full - 1_000_000, "{signed} vs {full}");
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
        assert_eq!(state.quote_for_cid(HOLDING_CID).as_deref(), Some("q1"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(500));

        let ttl = Duration::from_secs(v2.atomic_quote_valid_secs + v2.settle_grace_secs);
        state.sweep(Instant::now() + ttl - Duration::from_secs(5)).await;
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
        state.sweep(Instant::now() + ttl + Duration::from_secs(1)).await;
        assert_eq!(state.pending_kind("q1"), Some("Expired"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
    }

    // A signed validity that overflows the clock rejects and releases what the
    // confirm had acquired
    #[tokio::test]
    async fn confirm_rejects_signed_validity_overflow_and_releases() {
        let lm = lm_with_usdcx(1000).await;
        let v2 = RfqV2Config {
            atomic_quote_valid_secs: u64::try_from(i64::MAX / 1_000_000).unwrap(),
            settle_grace_secs: 0,
            ..RfqV2Config::default()
        };
        let state = signable_state(lm.clone(), v2, &[]).await;
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        let reject = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InternalError as i32);
        assert!(reject.reason_detail.unwrap().contains("validity window"));
        assert_eq!(state.pending_kind("q1"), None);
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
        assert!(
            state
                .cache
                .select_for_disclosure(&USDCX_KEY.to_string(), Decimal::from(500), 10, false)
                .await
                .is_some(),
            "holding reservation released"
        );
    }

    // The state machine keeps working after a panic poisoned its locks
    #[tokio::test]
    async fn poisoned_locks_are_tolerated() {
        let lm = lm_with_usdcx(1000).await;
        let state =
            signable_state(lm.clone(), RfqV2Config::default(), &[usdcx_split_target()]).await;
        poison(&state.pending);
        poison(&state.correlation);
        poison(&state.splits_in_flight);

        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        assert_eq!(state.pending_kind("q1"), Some("Indicative"));
        state.handle_confirm(confirm_req("q1")).await.unwrap();
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
        assert_eq!(state.quote_for_cid(HOLDING_CID).as_deref(), Some("q1"));
        assert_eq!(state.snapshot_pending().len(), 1);
        state.handle_settle_observed("q1", "upd-1").await;
        assert_eq!(state.pending_kind("q1"), Some("Settled"));

        state
            .register_indicative("q-rej", MARKET, QuoteSide::Sell, &priced_sell(900, 90), None, None)
            .await
            .unwrap();
        let mut req = confirm_req("q-rej");
        req.respond_by = Some(prost_types::Timestamp {
            seconds: agent_logic::clock::now_secs_i64(),
            nanos: 0,
        });
        let reject = state.handle_confirm(req).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InsufficientHoldings as i32);
        assert_eq!(state.pending_kind("q-rej"), None);

        let far = agent_logic::clock::now_micros_i64() + 60_000_000;
        state.restore_pending(vec![saved("q2", far)]).await;
        assert_eq!(state.quote_for_cid("cid-q2").as_deref(), Some("q2"));
        state.sweep(Instant::now() + TOMBSTONE_TTL + Duration::from_secs(1)).await;
        assert_eq!(state.pending_kind("q1"), None, "tombstone collected");
        assert_eq!(state.quote_for_cid(HOLDING_CID), None);
        assert_eq!(state.pending_kind("q2"), Some("Expired"));

        state.kick_on_demand_split(&USDCX_KEY.to_string());
        let _ = state.split_in_flight(&USDCX_KEY.to_string());
    }

    /// A venue-validated state with no holdings yet, whose confirm for "q1"
    /// waits for a split until `respond_by`.
    async fn state_with_confirm_waiting(lm: Arc<LiquidityManager>) -> Arc<RfqV2State> {
        confirm_waiting_with(lm, RfqV2Config::default()).await
    }

    async fn confirm_waiting_with(lm: Arc<LiquidityManager>, v2: RfqV2Config) -> Arc<RfqV2State> {
        let state = venue_state(lm, v2, &[], HoldingsCache::new(false));
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        mark_split_in_flight(&state, USDCX_KEY);
        Arc::new(state)
    }

    fn confirm_answering_within(quote_id: &str, secs: i64) -> RfqConfirmRequest {
        let mut req = confirm_req(quote_id);
        req.respond_by = Some(prost_types::Timestamp {
            seconds: agent_logic::clock::now_secs_i64() + secs,
            nanos: 0,
        });
        req
    }

    async fn wait_for_kind(state: &RfqV2State, quote_id: &str, kind: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.pending_kind(quote_id) != Some(kind) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{quote_id} never reached {kind}"));
    }

    // A second confirm for a quote whose confirm is running is refused, not run twice
    #[tokio::test]
    async fn a_duplicate_confirm_while_the_first_runs_is_refused() {
        let lm = lm_with_usdcx(10_000).await;
        let state = state_with_confirm_waiting(lm.clone()).await;
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_answering_within("q1", 6)).await }
        });
        wait_for_kind(&state, "q1", "Confirming").await;

        let dup = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
        assert_eq!(dup.reason, RfqConfirmRejectReason::InternalError as i32);
        assert_eq!(dup.reason_detail.as_deref(), Some("confirm already in progress"));
        assert_eq!(state.pending_kind("q1"), Some("Confirming"), "the duplicate releases nothing");
        assert_eq!(lm.available("USDCx").await, Decimal::from(9_500));

        add_usdcx_holding(&state).await;
        let envelope = first.await.unwrap().unwrap();
        assert_eq!(envelope.quote_id, "q1");
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
    }

    // The signed validity used to start after the split wait, outliving the
    // reservations that back it by as long as the wait took
    #[tokio::test]
    async fn the_signed_validity_counts_from_the_confirm_start() {
        let lm = lm_with_usdcx(10_000).await;
        let v2 = RfqV2Config { settle_grace_secs: 0, ..RfqV2Config::default() };
        let state = confirm_waiting_with(lm, v2).await;
        let t0 = agent_logic::clock::now_micros_i64();
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_answering_within("q1", 6)).await }
        });
        wait_for_kind(&state, "q1", "Confirming").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        add_usdcx_holding(&state).await;
        let quote = first.await.unwrap().unwrap().quote.unwrap();
        assert!(quote.valid_until_micros <= t0 + 120_000_000 + 500_000, "{} vs {t0}", quote.valid_until_micros);
        assert!(quote.created_at_micros >= t0 + 2_000_000 - 10_000_000, "createdAt is read at signing");
    }

    // A confirm that outlives the validity rejects instead of signing a dead quote
    #[tokio::test]
    async fn a_confirm_past_its_validity_rejects_and_releases() {
        let lm = lm_with_usdcx(10_000).await;
        let v2 = RfqV2Config { atomic_quote_valid_secs: 1, settle_grace_secs: 0, ..RfqV2Config::default() };
        let state = confirm_waiting_with(lm.clone(), v2).await;
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_answering_within("q1", 6)).await }
        });
        wait_for_kind(&state, "q1", "Confirming").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        add_usdcx_holding(&state).await;
        let reject = first.await.unwrap().unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::QuoteExpired as i32);
        assert_eq!(state.pending_kind("q1"), None);
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000), "commitment released");
        assert_eq!(state.cache.v2_reservation_quote(HOLDING_CID).await, None, "holdings released");
        let usdcx = USDCX_KEY.to_string();
        assert!(state.cache.select_for_disclosure(&usdcx, Decimal::from(500), 10, false).await.is_some());
    }

    // The sweep leaves a running confirm alone, even past the indicative validity
    #[tokio::test]
    async fn the_sweep_skips_a_confirm_in_progress() {
        let lm = lm_with_usdcx(10_000).await;
        let state = state_with_confirm_waiting(lm.clone()).await;
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_answering_within("q1", 6)).await }
        });
        wait_for_kind(&state, "q1", "Confirming").await;

        state.sweep(Instant::now() + Duration::from_secs(200)).await;
        assert_eq!(state.pending_kind("q1"), Some("Confirming"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(9_500), "commitment kept");

        add_usdcx_holding(&state).await;
        first.await.unwrap().unwrap();
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
    }

    // A confirm that outlives any legitimate one is given up: its commitment is
    // released, and if it ever finishes no envelope is issued
    #[tokio::test]
    async fn an_abandoned_confirm_is_released_and_never_issues() {
        let lm = lm_with_usdcx(10_000).await;
        let state = state_with_confirm_waiting(lm.clone()).await;
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_answering_within("q1", 6)).await }
        });
        wait_for_kind(&state, "q1", "Confirming").await;
        assert_eq!(lm.available("USDCx").await, Decimal::from(9_500));

        state
            .sweep(Instant::now() + CONFIRM_ABANDONED_AFTER + Duration::from_secs(1))
            .await;
        assert_eq!(state.pending_kind("q1"), Some("Expired"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000), "commitment released");

        add_usdcx_holding(&state).await;
        let reject = first.await.unwrap().unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InternalError as i32);
        assert_eq!(state.pending_kind("q1"), Some("Expired"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000));
        assert_eq!(state.quote_for_cid(HOLDING_CID), None);
        assert!(
            state
                .cache
                .select_for_disclosure(&USDCX_KEY.to_string(), Decimal::from(500), 10, false)
                .await
                .is_some(),
            "the late confirm released its holding reservation"
        );
    }

    // A confirm whose fee differs from the RFQ-time fee leaves the quote live
    #[tokio::test]
    async fn a_fee_mismatch_leaves_the_indicative_live() {
        let lm = lm_with_usdcx(1000).await;
        let state = signable_state(lm.clone(), RfqV2Config::default(), &[]).await;
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        let mut req = confirm_req("q1");
        req.settlement_fee = Some(AtomicFeeSpec::default());
        let reject = state.handle_confirm(req).await.unwrap_err();
        assert!(reject.reason_detail.unwrap().contains("settlement_fee"));
        assert_eq!(state.pending_kind("q1"), Some("Indicative"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(1000));
        state.handle_confirm(confirm_req("q1")).await.unwrap();
    }

    fn second_usdcx_holding() -> crate::holdings_cache::CachedHolding {
        crate::holdings_cache::CachedHolding {
            contract_id: "00usdcx-holding-2".to_string(),
            ..usdcx_holding()
        }
    }

    // A confirm whose picks another confirm reserved first selects again
    #[tokio::test]
    async fn a_confirm_that_loses_its_picks_selects_again() {
        let lm = lm_with_usdcx(10_000).await;
        let state = signable_state(lm.clone(), RfqV2Config::default(), &[]).await;
        state.cache.add_created(vec![second_usdcx_holding()]).await;
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        sync::lock(&state.reserve_races).push("rival".to_string());
        let envelope = state.handle_confirm(confirm_req("q1")).await.unwrap();
        assert_eq!(envelope.lp_input_holding_cids.len(), 1);
        let rival_took = state.cache.select_for_disclosure(&USDCX_KEY.to_string(), Decimal::from(500), 10, false).await;
        assert!(rival_took.is_none(), "one holding went to the rival, the other to the confirm");
        assert_eq!(state.pending_kind("q1"), Some("Confirmed"));
    }

    // Losing every attempt rejects and releases the commitment
    #[tokio::test]
    async fn a_confirm_that_keeps_losing_its_picks_rejects_and_releases() {
        let lm = lm_with_usdcx(10_000).await;
        let state = signable_state(lm.clone(), RfqV2Config::default(), &[]).await;
        let mut extra = second_usdcx_holding();
        state.cache.add_created(vec![extra.clone()]).await;
        extra.contract_id = "00usdcx-holding-3".to_string();
        state.cache.add_created(vec![extra]).await;
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, None)
            .await
            .unwrap();
        sync::lock(&state.reserve_races).extend(["r1", "r2", "r3"].map(String::from));
        let reject = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InsufficientHoldings as i32);
        assert_eq!(reject.reason_detail.as_deref(), Some("holdings reservation raced"));
        assert_eq!(state.pending_kind("q1"), None);
        assert_eq!(lm.available("USDCx").await, Decimal::from(10_000));
    }

    fn net_tracker(name: &str) -> Arc<NetPositionTracker> {
        let path = std::env::temp_dir().join(format!("silvana-rfqv2-{name}-{}.json", uuid::Uuid::now_v7()));
        NetPositionTracker::load_or_new(path, 24.0, RfqV2Config::default().stale_pending_after())
    }

    fn pool_impact_market() -> agent_logic::config::MarketConfig {
        serde_json::from_str(
            r#"{"market_id":"EDELx-USDCx",
                "rfq":{"min_quantity":"1","max_quantity":"100000000",
                       "pool_impact":{"enabled":true}}}"#,
        )
        .unwrap()
    }

    /// A 50k EDELx user sell whose indicative carried a pool adjustment.
    fn adjusted_sell(base_reserve: f64) -> PricedQuote {
        let mut priced = priced_sell(500, 90);
        priced.pool_pricing = Some(crate::rfq_handler::PoolPricingSnapshot {
            base_reserve,
            net_used: 0.0,
            impact_pct: 0.01,
            eff_spread_base: 0.0,
        });
        priced
    }

    // Concurrent confirms used to re-check one stale net, so a burst all signed at the old price
    #[tokio::test]
    async fn concurrent_confirms_for_one_user_see_each_others_count() {
        use agent_logic::pool_impact::PoolDepth;
        // Once one 50k sell is counted, the next one's impact exceeds the 0.25% tolerance
        const RESERVE: f64 = 4e7;
        let lm = lm_with_usdcx(10_000).await;
        let tracker = net_tracker("burst");
        let mids = HashMap::from([(
            MARKET.to_string(),
            MarketMid { mid: 0.01, pool_depth: Some(PoolDepth { base_reserve: RESERVE }) },
        )]);
        let mut state = signable_state(lm.clone(), RfqV2Config::default(), &[])
            .await
            .with_mid_prices(Arc::new(tokio::sync::RwLock::new(mids)))
            .with_net_positions(Arc::clone(&tracker));
        state.base_config.markets = vec![pool_impact_market()];
        state.cache.add_created(vec![second_usdcx_holding()]).await;
        for quote_id in ["q-a", "q-b"] {
            state
                .register_indicative(quote_id, MARKET, QuoteSide::Sell, &adjusted_sell(RESERVE), None, Some("user::1"))
                .await
                .unwrap();
        }
        let state = Arc::new(state);
        let held = state.cache.block_selection_for_tests().await;
        let confirm = |quote_id: &'static str| {
            let state = Arc::clone(&state);
            tokio::spawn(async move { state.handle_confirm(confirm_req(quote_id)).await })
        };
        let (a, b) = (confirm("q-a"), confirm("q-b"));
        // Selection is held until both passed the re-check, or one was refused there
        tokio::time::timeout(Duration::from_secs(5), async {
            while !(a.is_finished() || b.is_finished() || lm.available("USDCx").await == Decimal::from(9_000)) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both confirms reach the selection, or one is refused");
        drop(held);

        let results = [a.await.unwrap(), b.await.unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "exactly one envelope");
        let reject = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert_eq!(reject.reason, RfqConfirmRejectReason::QuoteExpired as i32);
        let detail = reject.reason_detail.clone().unwrap_or_default();
        assert!(detail.contains("market moved"), "{detail}");
        let net = tracker.net("user::1", "EDELx");
        assert!((net + 50_000.0).abs() < 1.0, "only the issued confirm is counted: {net}");
    }

    // A confirm counted at its re-check and refused later takes its count back
    #[tokio::test]
    async fn a_refused_confirm_takes_back_its_count() {
        let lm = lm_with_usdcx(10_000).await;
        let tracker = net_tracker("refused");
        tracker.record_confirm("earlier", Some("user::1"), "EDELx", 10_000.0);
        let state = armed_recheck_state(lm.clone()).await.with_net_positions(Arc::clone(&tracker));
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &adjusted_sell(1e12), None, Some("user::1"))
            .await
            .unwrap();
        lm.try_commit("competitor", "USDCx", Decimal::from(9_800), Decimal::ZERO).await.unwrap();
        let reject = state.handle_confirm(confirm_req("q1")).await.unwrap_err();
        assert_eq!(reject.reason, RfqConfirmRejectReason::InsufficientHoldings as i32);
        let net = tracker.net("user::1", "EDELx");
        assert!((net - 10_000.0).abs() < 1.0, "net {net}");
    }

    // An unchecked confirm used to be counted only once issued, so quotes priced meanwhile missed it
    #[tokio::test]
    async fn a_confirm_in_flight_is_already_counted() {
        let lm = lm_with_usdcx(10_000).await;
        let tracker = net_tracker("in-flight");
        let state = signable_state(lm.clone(), RfqV2Config::default(), &[])
            .await
            .with_net_positions(Arc::clone(&tracker));
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced_sell(500, 90), None, Some("user::1"))
            .await
            .unwrap();
        let state = Arc::new(state);
        let held = state.cache.block_selection_for_tests().await;
        let first = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.handle_confirm(confirm_req("q1")).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while lm.available("USDCx").await != Decimal::from(9_500) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the confirm commits, then waits for the selection");
        let net = tracker.net("user::1", "EDELx");
        assert!((net + 50_000.0).abs() < 1.0, "counted while in flight: {net}");
        drop(held);
        first.await.unwrap().unwrap();
        let net = tracker.net("user::1", "EDELx");
        assert!((net + 50_000.0).abs() < 1.0, "counted once: {net}");
    }

    // Cutting an indicative registration short leaves no entry behind
    #[tokio::test]
    async fn a_cancelled_indicative_registration_records_nothing() {
        let lm = lm_with_usdcx(1000).await;
        let state = state_with(lm);
        let held = state.cache.block_selection_for_tests().await;
        let priced = priced_sell(500, 90);
        let registration =
            state.register_indicative("q1", MARKET, QuoteSide::Sell, &priced, None, None);
        assert!(tokio::time::timeout(Duration::from_millis(50), registration).await.is_err());
        assert_eq!(state.pending_kind("q1"), None);
        drop(held);
        state
            .register_indicative("q1", MARKET, QuoteSide::Sell, &priced, None, None)
            .await
            .unwrap();
        assert_eq!(state.pending_kind("q1"), Some("Indicative"));
    }
}
