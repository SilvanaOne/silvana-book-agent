//! RFQ V2 (AtomicDVP) LP-side quote state machine (design §5.5, §5.7).
//!
//! Transitions per quote_id:
//! `Indicative{availability check only}` → (confirm) → `Confirmed{LM
//! commitment, holdings, ticket, envelope}` → (SettleObserved) → `Settled`,
//! and → `Expired` from either live state via the sweep.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tracing::{debug, info, warn};

use agent_logic::config::{RfqV2Config, RfqV2MarketConfig};
use agent_logic::liquidity::LiquidityManager;
use agent_logic::net_position::NetPositionTracker;
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
const CONFIRM_RESPONSE_MARGIN: Duration = Duration::from_millis(1200);
/// Hard ceiling on the confirm-time split wait — defensive bound against a
/// bogus/far-future `respond_by` (the relay itself clamps `timeout_secs` to 30).
/// Shared with the taker-side selection re-poll (atomic_swap.rs).
pub(crate) const MAX_CONFIRM_SPLIT_WAIT: Duration = Duration::from_secs(28);
/// Fallback wait when `respond_by` is absent/unparseable (pre-`respond_by`
/// relay) — preserves the historical bounded 6 s wait.
const FALLBACK_CONFIRM_SPLIT_WAIT: Duration = Duration::from_secs(6);

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
    splits_in_flight: Arc<Mutex<std::collections::HashSet<InstrumentKey>>>,
    /// Live mid-price map shared with the RFQ poller (None in tests). Confirm
    /// re-checks it so a stale indicative is never signed into an envelope.
    mid_prices: Option<Arc<tokio::sync::RwLock<HashMap<String, MarketMid>>>>,
    /// Trailing net-position accumulator. None in tests / non-LP.
    net_positions: Option<Arc<NetPositionTracker>>,
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
            splits_in_flight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            mid_prices: None,
            net_positions: None,
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
        let Some((split_instr, rungs)) = self.split_rungs.get(instrument).cloned() else {
            return;
        };
        {
            let mut in_flight = self.splits_in_flight.lock().unwrap();
            if !in_flight.insert(instrument.clone()) {
                return; // already splitting this instrument
            }
        }
        info!(
            "On-demand split kicked for {} (indicative-time selection empty)",
            instrument
        );
        let config = self.base_config.clone();
        let cache = self.cache.clone();
        let in_flight = self.splits_in_flight.clone();
        let key = instrument.clone();
        // ensure_denominations applies the shared cooldown + fail-stop budget,
        // so on-demand kicks are governed together with the maintenance tick.
        let v2 = self.v2.clone();
        tokio::spawn(async move {
            let result = async {
                let mut client = crate::atomic_swap::create_atomic_client(&config).await?;
                crate::split_worker::ensure_denominations(
                    &config, &cache, &mut client, &split_instr, &rungs, &v2,
                )
                .await
            }
            .await;
            if let Err(e) = result {
                warn!("On-demand split for {} failed: {:#}", key, e);
            }
            in_flight.lock().unwrap().remove(&key);
        });
    }

    fn split_in_flight(&self, instrument: &InstrumentKey) -> bool {
        self.splits_in_flight.lock().unwrap().contains(instrument)
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

    fn lm_key(quote_id: &str) -> String {
        format!("rfqv2:{quote_id}")
    }

    /// Test-only visibility into the per-quote state machine.
    #[cfg(test)]
    pub(crate) fn pending_kind(&self, quote_id: &str) -> Option<&'static str> {
        self.pending.lock().unwrap().get(quote_id).map(|e| match e {
            PendingV2::Indicative { .. } => "Indicative",
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
                "insufficient {lp_pays_token} ({avail:.4} available, {lp_pays_amount:.4} needed)"
            ));
        }

        let valid_until = Instant::now() + Duration::from_secs(priced.valid_for_secs as u64);
        self.pending.lock().unwrap().insert(
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
        if self
            .cache
            .select_for_disclosure(&lp_pays_key, lp_pays_amount, max_inputs, is_cc)
            .await
            .is_none()
        {
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
                seconds: chrono::Utc::now().timestamp(),
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
        match req.respond_by.as_ref() {
            Some(ts) => {
                let remaining_ms = (ts.seconds * 1000 + (ts.nanos as i64) / 1_000_000)
                    - chrono::Utc::now().timestamp_millis()
                    - CONFIRM_RESPONSE_MARGIN.as_millis() as i64;
                let remaining_ms = remaining_ms
                    .clamp(0, MAX_CONFIRM_SPLIT_WAIT.as_millis() as i64);
                start + Duration::from_millis(remaining_ms as u64)
            }
            None => start + FALLBACK_CONFIRM_SPLIT_WAIT,
        }
    }

    /// Release everything a failed confirm acquired, and drop the pending
    /// entry. `holding_cids`/`ticket` are whatever the pipeline had acquired
    /// by the failure point.
    async fn release_on_reject(&self, quote_id: &str, holding_cids: &[String], ticket_assigned: bool) {
        self.liquidity_manager.release(&Self::lm_key(quote_id)).await;
        if !holding_cids.is_empty() {
            self.cache.release_reservations(holding_cids).await;
        }
        if ticket_assigned {
            if let Some(pool) = &self.ticket_pool {
                pool.unassign(quote_id);
            }
        }
        self.pending.lock().unwrap().remove(quote_id);
    }

    /// Phase-2 confirm handler (design §5.5 steps 1-7).
    pub async fn handle_confirm(
        &self,
        req: RfqConfirmRequest,
    ) -> Result<AtomicQuoteEnvelope, RfqConfirmReject> {
        let quote_id = req.quote_id.clone();
        let now = Instant::now();

        // Step 1 — lookup + guards (idempotent re-send for already-Confirmed)
        enum Lookup {
            NotFound,
            Resend(Box<AtomicQuoteEnvelope>),
            Restored,
            Dead,
            ExpiredIndicative,
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
            let pending = self.pending.lock().unwrap();
            match pending.get(&quote_id) {
                None => Lookup::NotFound,
                Some(PendingV2::Confirmed { envelope: Some(env), .. }) => {
                    Lookup::Resend(Box::new(env.clone()))
                }
                Some(PendingV2::Confirmed { envelope: None, .. }) => Lookup::Restored,
                Some(PendingV2::Settled { .. }) | Some(PendingV2::Expired { .. }) => Lookup::Dead,
                Some(PendingV2::Indicative {
                    market_id,
                    side,
                    base_amount,
                    quote_amount,
                    lp_pays,
                    lp_pays_token,
                    notional_usd,
                    valid_until,
                    settlement_fee,
                    user_party,
                    pool_inputs,
                }) => {
                    if now >= *valid_until {
                        Lookup::ExpiredIndicative
                    } else {
                        Lookup::Live(Box::new(LiveQuote {
                            market_id: market_id.clone(),
                            side: *side,
                            base_amount: *base_amount,
                            quote_amount: *quote_amount,
                            lp_pays: lp_pays.clone(),
                            lp_pays_token: lp_pays_token.clone(),
                            notional_usd: *notional_usd,
                            settlement_fee: settlement_fee.clone(),
                            user_party: user_party.clone(),
                            pool_inputs: pool_inputs.clone(),
                        }))
                    }
                }
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

        // Defense-in-depth (design §14 D18): the confirm echoes the cached fee —
        // it must equal what this LP received with the RFQ fan-out.
        if req.settlement_fee != settlement_fee {
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InternalError,
                "confirm settlement_fee does not match the RFQ-time fee",
            ));
        }

        // The indicative may have been priced off a mid that has since
        // vanished, so reject rather than sign it. Nothing is acquired yet.
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
            if let Some(inputs) = pool_inputs.as_ref().filter(|i| i.impact_pct > 0.0) {
                let depth_now = current.as_ref().and_then(|m| m.pool_depth.as_ref());
                let cfg = self
                    .base_config
                    .markets
                    .iter()
                    .find(|mk| mk.market_id == market_id)
                    .and_then(|mk| mk.rfq.as_ref())
                    .and_then(|r| r.pool_impact.as_ref());
                if let (Some(depth), Some(cfg), Some(mid_now)) =
                    (depth_now, cfg, current.as_ref().map(|m| m.mid))
                {
                    let q = base_amount.to_f64().unwrap_or(0.0);
                    let net_now = match (&self.net_positions, &user_party) {
                        (Some(t), Some(p)) => {
                            t.net(p, market_id.split('-').next().unwrap_or(""))
                        }
                        _ => 0.0,
                    };
                    let (impact_side, sign) = match side {
                        QuoteSide::Buy => (ImpactSide::UserBuys, 1.0),
                        QuoteSide::Sell => (ImpactSide::UserSells, -1.0),
                    };
                    let impact_now =
                        pool_impact::pool_impact_percent(impact_side, q, net_now, depth, cfg);
                    let fair_now =
                        mid_now * (1.0 + sign * (inputs.eff_spread_base + impact_now) / 100.0);
                    let held = if base_amount > Decimal::ZERO {
                        (quote_amount / base_amount).to_f64().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    let tol = cfg.confirm_tolerance_percent.max(0.0) / 100.0;
                    // A non-positive fair price means the term saturated past
                    // 100%: reject rather than pass silently.
                    let saturated = !(fair_now.is_finite() && fair_now > 0.0);
                    let taker_favourable = held > 0.0
                        && (saturated
                            || match side {
                                QuoteSide::Buy => held < fair_now * (1.0 - tol),
                                QuoteSide::Sell => held > fair_now * (1.0 + tol),
                            });
                    if saturated {
                        warn!(
                            "Confirm {}: impact saturated (fair price {:.10} <= 0 at net {:.0}) \
                             — rejecting rather than failing open",
                            quote_id, fair_now, net_now
                        );
                    }
                    if taker_favourable {
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
                }
            }
        }

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
        let expires_at = now
            + Duration::from_secs(self.v2.atomic_quote_valid_secs + self.v2.settle_grace_secs);
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
        let Some(picks) = picks else {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InsufficientHoldings,
                "insufficient disclosable holdings",
            ));
        };
        let holding_cids: Vec<String> = picks.iter().map(|h| h.contract_id.clone()).collect();
        if !self.cache.reserve_v2(&holding_cids, &quote_id, expires_at).await {
            self.release_on_reject(&quote_id, &[], false).await;
            return Err(self.reject(
                &req,
                RfqConfirmRejectReason::InsufficientHoldings,
                "holdings reservation raced",
            ));
        }

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
        let now_micros = chrono::Utc::now().timestamp_micros();
        let created_at_micros = now_micros - 10_000_000;
        let valid_until_micros =
            now_micros + (self.v2.atomic_quote_valid_secs as i64) * 1_000_000;

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
        let signature = match sign_quote_scalar(&self.quote_key.scalar(), &canonical) {
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
        {
            let mut corr = self.correlation.lock().unwrap();
            for cid in &holding_cids {
                corr.insert(cid.clone(), quote_id.clone());
            }
            if let Some(tc) = &ticket_cid {
                corr.insert(tc.clone(), quote_id.clone());
            }
        }
        self.pending.lock().unwrap().insert(
            quote_id.clone(),
            PendingV2::Confirmed {
                holding_cids,
                ticket_id: ticket_id.clone(),
                ticket_cid,
                envelope: Some(envelope.clone()),
                valid_until_micros,
                lp_pays_token,
                lp_pays_amount: lp_pays.1,
                expires_at,
                market_id: market_id.clone(),
            },
        );

        // A binding envelope now exists: count the signed base delta. Last, so
        // no reject path ever counts.
        if let Some(tracker) = &self.net_positions {
            let base_token = market_id.split('-').next().unwrap_or("");
            let delta = base_amount.to_f64().unwrap_or(0.0)
                * match side {
                    QuoteSide::Buy => 1.0,
                    QuoteSide::Sell => -1.0,
                };
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
        let action = {
            let mut pending = self.pending.lock().unwrap();
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
            let mut pending = self.pending.lock().unwrap();
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
                    PendingV2::Settled { since } | PendingV2::Expired { since }
                        if now.duration_since(*since) > TOMBSTONE_TTL =>
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
            let mut corr = self.correlation.lock().unwrap();
            corr.retain(|_, qid| !gc.contains(qid));
        }

        for r in releases {
            info!("V2 sweep: expiring quote {}", r.quote_id);
            if r.lm {
                self.liquidity_manager.release(&Self::lm_key(&r.quote_id)).await;
                // A Confirmed quote expired unfilled: reverse its confirm-time
                // net-position soft count (no-op for quote_ids never counted).
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
        self.pending
            .lock()
            .unwrap()
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
    /// back a possibly-live envelope.
    pub async fn restore_pending(&self, saved: Vec<SavedPendingV2>) {
        let now_micros = chrono::Utc::now().timestamp_micros();
        let grace_micros = (self.v2.settle_grace_secs as i64) * 1_000_000;
        for p in saved {
            let remaining_micros = p.valid_until_micros + grace_micros - now_micros;
            if remaining_micros <= 0 {
                debug!("Skipping expired saved V2 quote {}", p.quote_id);
                continue;
            }
            let expires_at =
                Instant::now() + Duration::from_micros(remaining_micros as u64);
            self.cache
                .restore_v2_reservation(&p.holding_cids, &p.quote_id, expires_at)
                .await;
            {
                let mut corr = self.correlation.lock().unwrap();
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
            self.pending.lock().unwrap().insert(
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
        self.correlation.lock().unwrap().get(cid).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rfq_handler::PricedQuote;
    use crate::venue_registry::VenueRegistry;
    use orderbook_proto::rfqv2::RfqConfirmRequest;
    use std::str::FromStr;

    const MARKET: &str = "EDELx-USDCx";
    const USDCX_KEY: &str = "reg::USDCx";

    /// State harness: one market, empty venue registry (confirm's Step 2
    /// venue lookup fails — deliberate: everything up to and including the
    /// Step-1.5 funds commitment is exercisable without a signable venue).
    fn state_with(lm: Arc<LiquidityManager>) -> RfqV2State {
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
        let quote_key = agent_logic::config::AtomicQuoteKey::from_scalar_hex(
            &atomic_quote::gen_keypair().unwrap().priv_scalar_hex,
        )
        .unwrap();
        RfqV2State::new(
            "lp-party::1220test".to_string(),
            "LP Test".to_string(),
            "sync::test".to_string(),
            quote_key,
            RfqV2Config::default(),
            HashMap::new(),
            market_instruments,
            crate::holdings_cache::HoldingsCache::new(false),
            None,
            Arc::new(VenueRegistry::new(
                "lp-party::1220test".to_string(),
                String::new(),
                HashMap::new(),
            )),
            lm,
            agent_logic::config::BaseConfig::test_minimal(),
            &[],
        )
    }

    async fn lm_with_usdcx(balance: u32) -> Arc<LiquidityManager> {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(balance)).await;
        lm
    }

    /// User Sell ⇒ LP pays the quote leg (USDCx).
    fn priced_sell(lp_pays_usdcx: u32, valid_for_secs: u32) -> PricedQuote {
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

    fn confirm_req(quote_id: &str) -> RfqConfirmRequest {
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
}
