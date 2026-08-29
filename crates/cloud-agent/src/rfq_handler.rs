//! RFQ Handler for LP cloud agents
//!
//! Handles incoming RFQ requests from the orderbook server and responds
//! with quotes or rejections based on market configuration and mid-prices.
//!
//! When a LiquidityManager is configured, the handler:
//! 1. Rejects RFQs when the LP lacks sufficient balance for the allocation + fees
//! 2. Widens spreads based on token depletion rate (depletion coefficient)
//!
//! The pricing + gating pipeline is shared between RFQ v1
//! (`handle_rfq_request`) and the RFQ V2 atomic stream (`price_rfq`).

use agent_logic::config::{
    resolve_rfq_config, BaseConfig, LiquidityProviderConfig, MarketConfig, VenueOverride,
};
use agent_logic::liquidity::LiquidityManager;
use agent_logic::net_position::NetPositionTracker;
use agent_logic::pool_impact::{self, ImpactSide, MarketMid};
use agent_logic::runner::QuotedTrade;
use rust_decimal::Decimal;
use std::borrow::Cow;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

static STALE_WARN_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Log a stale-balance rejection at most once per minute.
fn warn_stale_rate_limited(token: &str, age_secs: u64) {
    let mut last = STALE_WARN_AT.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60)) {
        *last = Some(std::time::Instant::now());
        warn!("rejecting RFQs: {} balance stale for {}s", token, age_secs);
    }
}

use orderbook_proto::settlement::{
    RfqRequest, RfqQuote, RfqReject, RfqRejectionReason,
};

/// RFQ handler that computes quotes based on market config and mid-prices
pub struct RfqHandler {
    lp_config: LiquidityProviderConfig,
    markets: Vec<MarketConfig>,
    /// Venue/branch-scoped `[markets.rfq]` overlays (`[[venue_overrides]]`),
    /// resolved per request in `price_rfq` — RFQ V2 only (V1 has no venue).
    venue_overrides: Vec<VenueOverride>,
    /// Market mid-prices: market_id -> mid + the pool depth it arrived with
    /// (one entry, so depth can never outlive its mid)
    mid_prices: Arc<RwLock<HashMap<String, MarketMid>>>,
    party_id: String,
    /// Trades we quoted (for settlement verification)
    quoted_trades: Arc<Mutex<Vec<QuotedTrade>>>,
    /// Liquidity manager for balance checks and depletion-based spread adjustment
    liquidity_manager: Option<Arc<LiquidityManager>>,
    /// Trailing per-counterparty net tracker feeding the size term.
    /// None = no per-party/size term is ever applied.
    net_positions: Option<Arc<NetPositionTracker>>,
    /// rfq_v2_only mode: the V1 stream is never opened, so no V1 RfqRequest
    /// should ever reach this handler — belt-and-braces reject if one does.
    /// `price_rfq` is NOT gated (the V2 atomic stream shares it).
    rfq_v2_only: bool,
}

/// Result of handling an RFQ request
pub enum RfqResponse {
    Quote(RfqQuote),
    Reject(RfqReject),
}

/// A priced (accepted) RFQ — shared output of the v1/V2 pricing pipeline.
/// The `*_str` fields are the exact v1 wire strings (`{:.10}` renders of the
/// f64 computation); the Decimals are parsed from those strings so both
/// representations agree digit-for-digit.
pub(crate) struct PricedQuote {
    #[allow(dead_code)] // consumed by the user-mode phase (atomic_swap)
    pub market_id: String,
    #[allow(dead_code)] // consumed by the user-mode phase (atomic_swap)
    pub price: Decimal,
    pub quantity: Decimal,
    pub quote_quantity: Decimal,
    pub price_str: String,
    pub quantity_str: String,
    pub quote_quantity_str: String,
    /// (token symbol, amount) the LP pays/allocates on this trade
    pub lp_pays: (String, Decimal),
    /// USD notional of the quote leg, when a USD reference price exists
    pub notional_usd: Option<f64>,
    pub valid_for_secs: u32,
    pub allocate_before_secs: u32,
    pub settle_before_secs: u32,
    /// Present iff the size adjustment was applied: the inputs the
    /// confirm-time re-check needs to recompute a fair price.
    pub pool_pricing: Option<PoolPricingSnapshot>,
}

/// Pricing inputs captured at quote time so `handle_confirm` can detect a
/// stale, taker-favourable held price. Confirm cannot re-price, so it rejects.
#[derive(Debug, Clone)]
pub(crate) struct PoolPricingSnapshot {
    /// Size reference used at pricing time.
    pub base_reserve: f64,
    /// Trailing signed net used at pricing time.
    pub net_used: f64,
    /// Impact percent applied on top of the stress-composed spread.
    pub impact_pct: f64,
    /// `side_spread × stress` at pricing time (percent, before impact) — the
    /// re-check rebuilds `fair = mid_now·(1 ± (this + impact_now)/100)`.
    pub eff_spread_base: f64,
}

/// A rejection from the shared pricing pipeline.
#[derive(Debug)]
pub(crate) struct RejectInfo {
    pub reason: RfqRejectionReason,
    pub reason_detail: Option<String>,
    pub min_quantity: Option<String>,
    pub max_quantity: Option<String>,
}

impl RejectInfo {
    fn new(reason: RfqRejectionReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            reason_detail: Some(detail.into()),
            min_quantity: None,
            max_quantity: None,
        }
    }
}

/// The overload+depletion stress coefficient may only WIDEN the LP's edge, never
/// shrink it. A protective spread (`>= 0`, quote on the LP-favourable side of
/// mid) receives the full `multiplier`; a negative spread — an intentional
/// aggressive quote on the same side of mid as the opposite leg (e.g. an offer
/// below mid to offload inventory) — is honoured raw (`1.0`) so overload/
/// depletion can never push it further against the LP into a fire-sale.
///
/// Relies on `multiplier >= 1` (spread_multiplier ∈ {1,2,3}, depletion ≥ 0): for
/// a protective spread the coefficient can only move the quote further onto the
/// favourable side, so no clamp is needed there.
fn stress_coefficient(spread_percent: f64, multiplier: f64) -> f64 {
    if spread_percent >= 0.0 {
        multiplier
    } else {
        1.0
    }
}

impl RfqHandler {
    pub fn new(config: &BaseConfig) -> Option<Self> {
        let lp_config = config.liquidity_provider.clone()?;

        Some(Self {
            lp_config,
            markets: config.markets.clone(),
            venue_overrides: config.venue_overrides.clone(),
            mid_prices: Arc::new(RwLock::new(HashMap::new())),
            party_id: config.party_id.clone(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: config.rfq_v2_only,
        })
    }

    /// Set the liquidity manager for balance checks and spread adjustment
    pub fn set_liquidity_manager(&mut self, lm: Arc<LiquidityManager>) {
        self.liquidity_manager = Some(lm);
    }

    /// Set the net-position tracker feeding the pool-impact pricing term.
    pub fn set_net_positions(&mut self, tracker: Arc<NetPositionTracker>) {
        self.net_positions = Some(tracker);
    }

    /// Get a reference to mid_prices for external updates
    pub fn mid_prices(&self) -> Arc<RwLock<HashMap<String, MarketMid>>> {
        self.mid_prices.clone()
    }

    /// Get a reference to quoted trades for settlement verification
    pub fn quoted_trades(&self) -> Arc<Mutex<Vec<QuotedTrade>>> {
        self.quoted_trades.clone()
    }

    /// USD price of a token from mid_prices. USDC/USDCx ≈ $1; others resolve
    /// from a `{token}-USDCx` or `{token}-USDC` market (mainnet/devnet naming).
    /// Returns None if no USD reference is available.
    async fn token_usd_price(&self, token: &str) -> Option<f64> {
        if token.starts_with("USDC") {
            return Some(1.0);
        }
        let mids = self.mid_prices.read().await;
        for stable in ["USDCx", "USDC"] {
            if let Some(m) = mids.get(&format!("{token}-{stable}")) {
                if m.mid > 0.0 {
                    return Some(m.mid);
                }
            }
        }
        None
    }

    /// Shared pricing + gating pipeline (v1 semantics, byte-identical outputs).
    ///
    /// `direction`: 1 = BUY (user buys base, LP sells base), 2 = SELL (user
    /// sells base, LP buys base / sells quote) — the v1 `RfqRequest.direction`
    /// enum values. `rfq_id` is used for logging only.
    /// The taker sizes the trade in EITHER the base (`quantity_str`) OR the
    /// quote instrument (`quote_quantity_str`, empty when unused). In quote
    /// mode the base is derived as `quote_quantity / price` after pricing, and
    /// the base min/max bounds are checked against that derived value.
    ///
    /// `enforce_min_notional`: apply the USD `min_notional_usd` floor. TRUE on
    /// the RFQ V1 path — the LP pays its own dvp+allocation fees on a V1
    /// settle, and a dust trade would cost more in fees than it moves. FALSE
    /// on RFQ V2: the user pays every V2 fee
    /// (the server charges 3x below `min_order_value_usd`), the LP pays none,
    /// so the LP quotes any size; the base `min_quantity` bound still applies.
    ///
    /// `venue`/`venue_branch`: the requesting swap venue (RFQ V2:
    /// `AtomicRfqRequest.venue_name` — falling back to the VA2
    /// `quote_id_prefix` on older servers — and `.venue_branch`) — selects
    /// any matching `[[venue_overrides]]` overlay of the pair's RFQ config.
    /// V1 carries no venue identity and passes `(None, None)` (pair defaults).
    ///
    /// `user_party`: keys the trailing net accumulator. V1 passes `None`,
    /// which prices from net = 0.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn price_rfq(
        &self,
        rfq_id: &str,
        market_id: &str,
        direction: i32,
        quantity_str: &str,
        quote_quantity_str: &str,
        enforce_min_notional: bool,
        venue: Option<&str>,
        venue_branch: Option<&str>,
        user_party: Option<&str>,
    ) -> Result<PricedQuote, RejectInfo> {
        // Find market config
        let market_config = self.markets.iter().find(|m| m.market_id == market_id);
        let market_config = match market_config {
            Some(m) if m.enabled => m,
            _ => {
                debug!("RFQ {}: market {} not configured or disabled", rfq_id, market_id);
                return Err(RejectInfo::new(
                    RfqRejectionReason::MarketNotSupported,
                    format!("Market {} not supported", market_id),
                ));
            }
        };

        // RFQ market config + venue/branch overlay. The `enabled` check runs
        // on the EFFECTIVE config, so an overlay can close a pair to one
        // venue (`enabled = false`) — and a more specific entry can re-open
        // after a broader entry's close. It can NOT open a pair-disabled
        // market: the V2 stream never subscribes those, so no venue request
        // reaches this point (assemble() rejects overrides attempting it).
        let pair_rfq = match &market_config.rfq {
            Some(rfq) => rfq,
            None => {
                debug!("RFQ {}: RFQ not enabled for market {}", rfq_id, market_id);
                return Err(RejectInfo::new(
                    RfqRejectionReason::MarketNotSupported,
                    "RFQ not enabled for this market",
                ));
            }
        };
        let rfq_config = resolve_rfq_config(
            pair_rfq,
            &self.venue_overrides,
            market_id,
            venue,
            venue_branch,
        );
        let venue_override_applied = matches!(rfq_config, Cow::Owned(_));
        let rfq_config = rfq_config.as_ref();
        if !rfq_config.enabled {
            debug!(
                "RFQ {}: RFQ not enabled for market {} (venue {:?}/{:?})",
                rfq_id, market_id, venue, venue_branch
            );
            return Err(RejectInfo::new(
                RfqRejectionReason::MarketNotSupported,
                if pair_rfq.enabled {
                    "RFQ not available for this venue"
                } else {
                    "RFQ not enabled for this market"
                },
            ));
        }

        // Size mode: the taker gives EITHER a base `quantity` OR a
        // `quote_quantity` (quote-instrument amount, e.g. "pay 50 USDC"). In
        // quote mode the base is unknown until price is computed below, so the
        // base min/max bounds check is deferred until after the inversion.
        let requested_quote_quantity: Option<f64> = if !quote_quantity_str.trim().is_empty() {
            match quote_quantity_str.parse::<f64>() {
                Ok(q) if q > 0.0 && q.is_finite() => Some(q),
                _ => {
                    return Err(RejectInfo::new(
                        RfqRejectionReason::Unspecified,
                        "Invalid quote_quantity",
                    ));
                }
            }
        } else {
            None
        };

        // Base mode: parse now. Quote mode: derived after pricing (placeholder).
        let mut quantity: f64 = if requested_quote_quantity.is_none() {
            match quantity_str.parse() {
                Ok(q) => q,
                Err(_) => {
                    return Err(RejectInfo::new(
                        RfqRejectionReason::Unspecified,
                        "Invalid quantity",
                    ));
                }
            }
        } else {
            0.0
        };

        // Reject RFQs when ledger submission is failing (sequencer unreachable /
        // SEQUENCER_REQUEST_FAILED). Quoting into an outage reserves inventory that
        // can never settle at CreateDvp/allocate, saturating the reservation pool
        // and pinning available liquidity at ~0. Ledger-wide, since a sequencer
        // outage is instrument-agnostic. Self-clears when submissions recover or
        // the cooldown elapses (time-based probe). Quote-time only — do not reject
        // already-accepted proposals here (their reject path itself submits to the
        // down ledger); the settlement watchdog drains those internally.
        if agent_logic::ledger_health::is_unhealthy() {
            warn!("RFQ {}: rejected — ledger temporarily unavailable (sequencer submission failing)", rfq_id);
            return Err(RejectInfo::new(
                RfqRejectionReason::TemporarilyUnavailable,
                "Ledger temporarily unavailable",
            ));
        }

        // Reject RFQs when sequencer is critically overloaded (coefficient < OVERLOAD_THRESHOLD - 0.1).
        // At this level even proposing new trades would fail with SEQUENCER_BACKPRESSURE.
        if agent_logic::forecast::is_rfq_rejected_by_overload() {
            warn!("RFQ {}: rejected — sequencer critically overloaded", rfq_id);
            return Err(RejectInfo::new(
                RfqRejectionReason::MarketConditions,
                "High demand, try later",
            ));
        }

        // Get the mid-price and the pool depth that arrived WITH it (one map
        // entry, so depth can never outlive its mid).
        let (mid_price, pool_depth) = {
            let mid_prices = self.mid_prices.read().await;
            match mid_prices.get(market_id) {
                Some(m) if m.mid > 0.0 => (m.mid, m.pool_depth.clone()),
                _ => {
                    warn!("RFQ {}: no mid-price for market {}", rfq_id, market_id);
                    return Err(RejectInfo::new(
                        RfqRejectionReason::TemporarilyUnavailable,
                        "No mid-price available",
                    ));
                }
            }
        };

        // Parse market_id into base/quote tokens (e.g. "CC-USDCx" → ["CC", "USDCx"])
        let market_parts: Vec<&str> = market_id.split('-').collect();
        let (base_token, quote_token) = if market_parts.len() == 2 {
            (market_parts[0], market_parts[1])
        } else {
            (market_id, "")
        };

        // Compute price based on direction and spread.
        // Widen spreads when sequencer is under load:
        //   3x when coefficient < SEQUENCER_OVERLOAD_THRESHOLD (extreme load)
        //   2x when forecast is LOW (heavy load)
        //   1x otherwise
        // direction 1 = BUY (user buys, LP sells base → offer price = mid + spread)
        // direction 2 = SELL (user sells, LP buys base/sells quote → bid price = mid - spread)
        // Per-market opt-out: `disable_overload_spread_widening` pins this factor
        // at 1x so the market always quotes its raw configured spread.
        let spread_multiplier = if rfq_config.disable_overload_spread_widening {
            1.0
        } else if agent_logic::forecast::is_fees_paused_by_overload() {
            3.0
        } else if agent_logic::forecast::is_traffic_paused_by_forecast() {
            2.0
        } else {
            1.0
        };

        // Depletion coefficient: widen spread on the side that depletes a scarce
        // token. Per-market opt-out: `disable_depletion_spread_widening` pins it
        // at 0 (and skips the liquidity lookup entirely).
        let depletion_coeff = if rfq_config.disable_depletion_spread_widening {
            0.0
        } else if let Some(ref lm) = self.liquidity_manager {
            // The token being sold by the LP is the one that depletes
            let depleting_token = if direction == 1 {
                base_token // LP sells base (e.g. CC)
            } else {
                quote_token // LP sells quote (e.g. USDCx)
            };
            lm.depletion_coefficient(depleting_token).await
        } else {
            0.0
        };

        // The active side's configured spread (offer for a buy, bid for a sell).
        let side_spread = if direction == 1 {
            rfq_config.offer_spread_percent
        } else {
            rfq_config.bid_spread_percent
        };
        // Stress-widening may only improve the LP's price: apply the multiplier to
        // a protective (>= 0) spread; honour a negative (aggressive) spread raw so
        // overload/depletion never deepens it. See `stress_coefficient`.
        let stress = stress_coefficient(side_spread, spread_multiplier + depletion_coeff);
        // Base spread WITHOUT the impact term: impact composes outside and
        // additive to stress, which returns 1.0 for a negative spread.
        let eff_spread_base = side_spread * stress;
        let impact_side = if direction == 1 {
            ImpactSide::UserBuys
        } else {
            ImpactSide::UserSells
        };
        // Both the config section and a live size reference must be present;
        // either absent = no adjustment. Not gated by the widening flags.
        let impact_ctx = match (&rfq_config.pool_impact, &pool_depth) {
            (Some(cfg), Some(depth)) => Some((cfg, depth)),
            _ => None,
        };
        // Trailing signed net of this counterparty (+ = they net-bought from
        // us). No party or no tracker ⇒ 0, i.e. a pure size term.
        let signed_net = match (impact_ctx, user_party, &self.net_positions) {
            (Some(_), Some(party), Some(tracker)) => tracker.net(party, base_token),
            _ => 0.0,
        };

        // Audit-only captures, kept off the pricing path so the fast path
        // stays byte-identical.
        let pool_ctx_present = impact_ctx.is_some();
        let audit_reserve = impact_ctx.map(|(_, d)| d.base_reserve).unwrap_or(f64::NAN);
        let mut shadow_impact_pct = 0.0_f64;

        let price: f64;
        let quote_quantity: f64;
        let applied_impact_pct: f64;
        match impact_ctx {
            Some((cfg, depth)) if cfg.enabled => {
                let dir_price = |eff: f64| {
                    if direction == 1 {
                        // User is buying → LP offers at mid + spread + impact
                        mid_price * (1.0 + eff / 100.0)
                    } else {
                        // User is selling → LP bids at mid - spread - impact
                        mid_price * (1.0 - eff / 100.0)
                    }
                };
                match requested_quote_quantity {
                    None => {
                        // Base-denominated: one marginal-impact evaluation,
                        // charged from the party's trailing net.
                        applied_impact_pct = pool_impact::pool_impact_percent(
                            impact_side,
                            quantity,
                            signed_net,
                            depth,
                            cfg,
                        );
                        price = dir_price(eff_spread_base + applied_impact_pct);
                        quote_quantity = quantity * price;
                    }
                    Some(q_quote) => {
                        // Quote-denominated: price depends on base quantity, so
                        // solve the fixed point by bisection, not iteration.
                        let price_at = |b: f64| {
                            dir_price(
                                eff_spread_base
                                    + pool_impact::pool_impact_percent(
                                        impact_side,
                                        b,
                                        signed_net,
                                        depth,
                                        cfg,
                                    ),
                            )
                        };
                        // Monotonic in b, bounded by the unadjusted price on one
                        // side and the capped price on the other.
                        let p0 = price_at(0.0);
                        let p_cap =
                            dir_price(eff_spread_base + cfg.max_impact_percent.max(0.0))
                                .max(mid_price * 1e-9);
                        let (floor, ceil) = (p0.min(p_cap), p0.max(p_cap));
                        match pool_impact::solve_base_for_quote(q_quote, floor, ceil, price_at)
                        {
                            Some(b) if b.is_finite() && b > 0.0 => {
                                applied_impact_pct = pool_impact::pool_impact_percent(
                                    impact_side,
                                    b,
                                    signed_net,
                                    depth,
                                    cfg,
                                );
                                price = dir_price(eff_spread_base + applied_impact_pct);
                                quantity = q_quote / price;
                                quote_quantity = q_quote;
                            }
                            _ => {
                                warn!(
                                    "RFQ {}: quote-mode impact solve failed (Q={}, R={}, net={})",
                                    rfq_id, q_quote, depth.base_reserve, signed_net
                                );
                                return Err(RejectInfo::new(
                                    RfqRejectionReason::TemporarilyUnavailable,
                                    "Price computation error",
                                ));
                            }
                        }
                    }
                }
            }
            ctx => {
                // Fast path (no section or no reference) and the shadow arm
                // (present but disabled): compute + log, apply nothing.
                price = if direction == 1 {
                    // User is buying → LP offers at mid + spread
                    mid_price * (1.0 + eff_spread_base / 100.0)
                } else {
                    // User is selling → LP bids at mid - spread
                    mid_price * (1.0 - eff_spread_base / 100.0)
                };
                match requested_quote_quantity {
                    Some(q) => {
                        quantity = q / price;
                        quote_quantity = q;
                    }
                    None => quote_quantity = quantity * price,
                }
                applied_impact_pct = 0.0;
                if let Some((cfg, depth)) = ctx {
                    shadow_impact_pct = pool_impact::pool_impact_percent(
                        impact_side,
                        quantity,
                        signed_net,
                        depth,
                        cfg,
                    );
                }
            }
        }
        let effective_spread = eff_spread_base + applied_impact_pct;

        // Audit line on its own tracing target, so the quote is re-derivable
        // from one record. In shadow mode the would-be value rides alongside.
        if pool_ctx_present {
            info!(
                target: "pool_impact_audit",
                rfq_id = %rfq_id,
                market = %market_id,
                direction,
                party = user_party.unwrap_or("<none>"),
                mid = mid_price,
                spread_base = eff_spread_base,
                impact_pct = applied_impact_pct,
                shadow_impact_pct,
                effective_spread,
                price,
                base_qty = quantity,
                quote_qty = quote_quantity,
                net_used = signed_net,
                pool_base_reserve = audit_reserve,
                enforced = applied_impact_pct > 0.0,
                "rfq pool impact"
            );
        }

        // Guard against NaN/infinity from misconfigured spreads (or a
        // non-positive price that would make the quote-mode division blow up).
        if !price.is_finite()
            || !quote_quantity.is_finite()
            || !quantity.is_finite()
            || price <= 0.0
        {
            warn!("RFQ {}: computed invalid price {:.6} or quantity {:.6}", rfq_id, price, quote_quantity);
            return Err(RejectInfo::new(
                RfqRejectionReason::TemporarilyUnavailable,
                "Price computation error",
            ));
        }

        // Base min/max bounds — checked against the (possibly derived) base
        // quantity, so quote-denominated requests are validated too. (Reject
        // bounds stay base-denominated; the client converts for display.)
        let min_qty: f64 = rfq_config.min_quantity.parse().unwrap_or(0.0);
        let max_qty: f64 = rfq_config.max_quantity.parse().unwrap_or(f64::MAX);
        if quantity < min_qty {
            return Err(RejectInfo {
                reason: RfqRejectionReason::AmountTooSmall,
                reason_detail: Some(format!("Min quantity: {}", min_qty)),
                min_quantity: Some(rfq_config.min_quantity.clone()),
                max_quantity: Some(rfq_config.max_quantity.clone()),
            });
        }
        if quantity > max_qty {
            return Err(RejectInfo {
                reason: RfqRejectionReason::AmountTooLarge,
                reason_detail: Some(format!("Max quantity: {}", max_qty)),
                min_quantity: Some(rfq_config.min_quantity.clone()),
                max_quantity: Some(rfq_config.max_quantity.clone()),
            });
        }

        // USD reference for the quote leg (feeds the min-notional floor and the
        // V2 ticket-threshold decision).
        let usd_per_quote = self.token_usd_price(quote_token).await;
        let notional_usd = usd_per_quote.map(|p| quote_quantity * p);

        // USD minimum-value floor (global LP default, per-market override).
        // V1 ONLY (`enforce_min_notional`): refuse to quote RFQs whose USD
        // value falls below the configured minimum — on V1 the LP pays its own
        // settle fees. V2 skips this floor: the user pays all V2 fees (3x for
        // dust, server-side), the LP pays none.
        let min_notional_usd = rfq_config
            .min_notional_usd
            .unwrap_or(self.lp_config.min_notional_usd);
        if enforce_min_notional && min_notional_usd > 0.0 {
            match notional_usd {
                Some(value_usd) => {
                    if value_usd < min_notional_usd {
                        info!(
                            "RFQ {}: value ${:.2} < min ${:.2} — rejecting",
                            rfq_id, value_usd, min_notional_usd
                        );
                        return Err(RejectInfo::new(
                            RfqRejectionReason::AmountTooSmall,
                            format!("Min notional: ${:.2}", min_notional_usd),
                        ));
                    }
                }
                // Missing cross price → fail open (don't block trading on a transient gap).
                None => warn!(
                    "RFQ {}: no USD price for quote asset {} — skipping min-notional check",
                    rfq_id, quote_token
                ),
            }
        }

        // LP allocates base when user buys (dir=1), quote when user sells (dir=2)
        let (alloc_token, alloc_amount) = if direction == 1 {
            (base_token, quantity)     // LP sells base
        } else {
            (quote_token, quote_quantity) // LP sells quote
        };

        // Liquidity gate: reject if LP lacks sufficient balance for the allocation + estimated fees
        if let Some(ref lm) = self.liquidity_manager {
            // Reject early if balances haven't been loaded yet (e.g. just after restart),
            // otherwise lm.available() returns 0 and we'd report "insufficient" when we
            // simply don't know the balance yet.
            if !lm.is_ready().await {
                warn!(
                    "RFQ {}: rejecting — liquidity manager not yet ready (balances loading)",
                    rfq_id
                );
                return Err(RejectInfo::new(
                    RfqRejectionReason::TemporarilyUnavailable,
                    "Balances loading",
                ));
            }

            // Reject on a stale balance rather than quote from it.
            if let Some(age) = lm.is_stale(alloc_token).await {
                warn_stale_rate_limited(alloc_token, age.as_secs());
                return Err(RejectInfo::new(
                    RfqRejectionReason::TemporarilyUnavailable,
                    "Balances stale",
                ));
            }

            // Fee headroom the LP itself needs for this settle:
            //  - V1 (enforce_min_notional): the LP pays its own dvp +
            //    allocation fees in CC. Worst non-dust share-count multiplier
            //    (2026-08 rule) is x4 on ($0.3 dvp + max($0.7, 0.1% x
            //    notional) alloc); unknown notional falls back to the $0.7
            //    alloc minimum.
            //  - V2: ZERO — the user submits the settle and pays every fee
            //    leg (see rfq_v2.rs: try_commit passes a zero fee term), so
            //    requiring CC here would spuriously reject V2 quotes on a
            //    CC-poor LP.
            let fee_cc = if enforce_min_notional {
                let alloc = Decimal::from_f64_retain(notional_usd.unwrap_or(0.0) * 0.001)
                    .unwrap_or_default()
                    .max(Decimal::new(7, 1));
                lm.estimate_fee_cc(Decimal::from(4) * (Decimal::new(3, 1) + alloc))
                    .await
            } else {
                Decimal::ZERO
            };
            let alloc_dec = Decimal::from_f64_retain(alloc_amount).unwrap_or_default();

            let available = lm.available(alloc_token).await;
            let needed = if alloc_token == agent_logic::liquidity::CC_TOKEN {
                alloc_dec + fee_cc
            } else {
                alloc_dec
            };
            // Also check CC for fees when allocating non-CC
            let cc_ok = if alloc_token != agent_logic::liquidity::CC_TOKEN {
                lm.available_cc().await >= fee_cc
            } else {
                true // already checked above
            };

            if available < needed || !cc_ok {
                if available < needed && !cc_ok {
                    warn!(
                        "RFQ {}: rejected — insufficient {} ({:.4} available, {:.4} needed) AND insufficient CC for fees ({:.4} available, {:.4} needed)",
                        rfq_id, alloc_token, available, needed, lm.available_cc().await, fee_cc
                    );
                } else if !cc_ok {
                    warn!(
                        "RFQ {}: rejected — insufficient CC for fees ({:.4} available, {:.4} needed), {} OK ({:.4} available)",
                        rfq_id, lm.available_cc().await, fee_cc, alloc_token, available
                    );
                } else {
                    warn!(
                        "RFQ {}: rejected — insufficient {} ({:.4} available, {:.4} needed)",
                        rfq_id, alloc_token, available, needed
                    );
                }
                return Err(RejectInfo::new(
                    RfqRejectionReason::TemporarilyUnavailable,
                    "Insufficient liquidity",
                ));
            }
        }

        let valid_for_secs = rfq_config
            .quote_valid_secs
            .unwrap_or(self.lp_config.default_quote_valid_secs);

        // `effective_spread` (side_spread × applied coefficient) was computed with
        // the price above. Annotations reflect only the multiplier that was
        // actually applied — a stress-capped aggressive quote (side_spread < 0)
        // must not falsely advertise OVERLOAD/depletion widening.
        info!(
            "RFQ {}: quoting {} {} @ {:.6} (mid={:.6}, spread={:.2}%{}{}{}{})",
            rfq_id,
            quantity,
            market_id,
            price,
            mid_price,
            effective_spread,
            if side_spread >= 0.0 && spread_multiplier >= 3.0 { " OVERLOAD 3x" } else if side_spread >= 0.0 && spread_multiplier > 1.0 { " LOW-ISS 2x" } else { "" },
            if side_spread >= 0.0 && depletion_coeff > 0.0 { format!(" depl={:.1}", depletion_coeff) } else { String::new() },
            if applied_impact_pct > 0.0 {
                format!(" impact={applied_impact_pct:.3}% (net={signed_net:.0})")
            } else {
                String::new()
            },
            if venue_override_applied {
                format!(
                    " venue-ovr({}{})",
                    venue.unwrap_or("?"),
                    venue_branch.map(|b| format!("/{b}")).unwrap_or_default()
                )
            } else {
                String::new()
            }
        );

        // The exact v1 wire strings; the Decimals mirror them digit-for-digit.
        let price_str = format!("{:.10}", price);
        let quantity_dec_str = format!("{:.10}", quantity);
        let quote_quantity_str = format!("{:.10}", quote_quantity);
        let lp_pays_amount = Decimal::from_str(if direction == 1 {
            &quantity_dec_str
        } else {
            &quote_quantity_str
        })
        .unwrap_or_default();

        Ok(PricedQuote {
            market_id: market_id.to_string(),
            price: Decimal::from_str(&price_str).unwrap_or_default(),
            quantity: Decimal::from_str(&quantity_dec_str).unwrap_or_default(),
            quote_quantity: Decimal::from_str(&quote_quantity_str).unwrap_or_default(),
            price_str,
            quantity_str: quantity_dec_str,
            quote_quantity_str,
            lp_pays: (alloc_token.to_string(), lp_pays_amount),
            notional_usd,
            valid_for_secs,
            allocate_before_secs: rfq_config.allocate_before_secs,
            settle_before_secs: rfq_config.settle_before_secs,
            // Present only when the adjustment applied; its presence is what
            // arms the confirm-time re-check.
            pool_pricing: match impact_ctx {
                Some((cfg, depth)) if cfg.enabled => Some(PoolPricingSnapshot {
                    base_reserve: depth.base_reserve,
                    net_used: signed_net,
                    impact_pct: applied_impact_pct,
                    eff_spread_base,
                }),
                _ => None,
            },
        })
    }

    fn build_reject(&self, rfq_id: String, r: RejectInfo) -> RfqReject {
        RfqReject {
            rfq_id,
            lp_party_id: self.party_id.clone(),
            lp_name: self.lp_config.name.clone(),
            reason: r.reason as i32,
            reason_detail: r.reason_detail,
            rejected_at: Some(prost_types::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
            min_quantity: r.min_quantity,
            max_quantity: r.max_quantity,
        }
    }

    /// LP display name (used by the V2 stream handshake / messages).
    pub fn lp_name(&self) -> &str {
        &self.lp_config.name
    }

    /// Handle an incoming (v1) RFQ request
    pub async fn handle_rfq_request(&self, request: RfqRequest) -> RfqResponse {
        let rfq_id = request.rfq_id.clone();

        if self.rfq_v2_only {
            tracing::warn!(
                "RFQ V1 request {} (market {}) received while rfq_v2_only=true — refusing to quote",
                rfq_id, request.market_id
            );
            return RfqResponse::Reject(self.build_reject(
                rfq_id,
                RejectInfo::new(
                    RfqRejectionReason::TemporarilyUnavailable,
                    "LP is RFQ V2 (AtomicDVP) only",
                ),
            ));
        }

        let priced = match self
            // v1 RFQ is base-only (no quote-denominated sizing) and carries
            // no venue identity and no user party, so no per-party term.
            .price_rfq(&rfq_id, &request.market_id, request.direction, &request.quantity, "", true, None, None, None)
            .await
        {
            Ok(p) => p,
            Err(reject) => return RfqResponse::Reject(self.build_reject(rfq_id, reject)),
        };

        let quote_id = Uuid::now_v7().to_string();
        let now = chrono::Utc::now();
        let valid_until = now + chrono::Duration::seconds(priced.valid_for_secs as i64);

        // Record trade for settlement verification (v1 only — V2 settles are
        // watcher-verified, never proposal-verified)
        self.quoted_trades.lock().await.push(QuotedTrade {
            market_id: request.market_id.clone(),
            price: priced.price_str.clone(),
            base_quantity: priced.quantity_str.clone(),
            quote_quantity: priced.quote_quantity_str.clone(),
        });

        RfqResponse::Quote(RfqQuote {
            rfq_id,
            quote_id,
            market_id: request.market_id,
            direction: request.direction,
            quantity: priced.quantity_str,
            price: priced.price_str,
            quote_quantity: priced.quote_quantity_str,
            valid_for_secs: priced.valid_for_secs,
            valid_until: Some(prost_types::Timestamp {
                seconds: valid_until.timestamp(),
                nanos: 0,
            }),
            lp_party_id: self.party_id.clone(),
            lp_name: self.lp_config.name.clone(),
            quoted_at: Some(prost_types::Timestamp {
                seconds: now.timestamp(),
                nanos: 0,
            }),
            allocate_before_secs: Some(priced.allocate_before_secs),
            settle_before_secs: Some(priced.settle_before_secs),
        })
    }
}

#[cfg(test)]
mod price_rfq_tests {
    use super::*;

    const MID: f64 = 0.0136; // EDELx ≈ $0.0136 (the pathological cheap-base case)

    /// Mid-map entry without pool depth (the pre-impact shape).
    fn mm(mid: f64) -> MarketMid {
        MarketMid { mid, pool_depth: None }
    }

    /// Zero-spread EDELx-USDC LP so price == mid, min base bound 50, min
    /// notional $10. `liquidity_manager: None` skips the balance gate.
    fn handler() -> RfqHandler {
        let lp_config: LiquidityProviderConfig =
            serde_json::from_str(r#"{"name":"LP test","min_notional_usd":10.0}"#).unwrap();
        let market: MarketConfig = serde_json::from_str(
            r#"{"market_id":"EDELx-USDC","rfq":{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":0.0,"offer_spread_percent":0.0}}"#,
        )
        .unwrap();
        let mut mids = HashMap::new();
        mids.insert("EDELx-USDC".to_string(), mm(MID));
        RfqHandler {
            lp_config,
            markets: vec![market],
            venue_overrides: Vec::new(),
            mid_prices: Arc::new(RwLock::new(mids)),
            party_id: "lp::test".to_string(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: false,
        }
    }

    fn f(s: &str) -> f64 {
        s.parse().unwrap()
    }

    /// rfq_v2_only rejects every V1 request before pricing (belt-and-braces —
    /// the V1 stream is never opened in that mode), while the same request
    /// quotes normally with the switch off.
    #[tokio::test]
    async fn rfq_v2_only_rejects_v1_requests() {
        let req = || RfqRequest {
            rfq_id: "rfq-1".to_string(),
            market_id: "EDELx-USDC".to_string(),
            direction: 1,
            quantity: "1000".to_string(),
            ..Default::default()
        };

        let mut h = handler();
        h.rfq_v2_only = true;
        match h.handle_rfq_request(req()).await {
            RfqResponse::Reject(r) => {
                assert_eq!(r.reason, RfqRejectionReason::TemporarilyUnavailable as i32);
                assert!(r.reason_detail.unwrap().contains("V2"));
            }
            RfqResponse::Quote(_) => panic!("must not quote V1 in rfq_v2_only mode"),
        }

        // Same request with the switch off quotes (1000 EDELx ≈ $13.6 > $10).
        match handler().handle_rfq_request(req()).await {
            RfqResponse::Quote(_) => {}
            RfqResponse::Reject(r) => {
                panic!("expected quote with switch off: {:?}", r.reason_detail)
            }
        }
    }

    /// Quote-denominated buy: "pay 50 USDC" prices at $50 (clears the $10
    /// floor) and derives base = 50 / price.
    #[tokio::test]
    async fn quote_denominated_inverts_and_passes_min_notional() {
        // direction 1 = buy.
        let priced = match handler().price_rfq("t", "EDELx-USDC", 1, "", "50", true, None, None, None).await {
            Ok(p) => p,
            Err(e) => panic!("quote-denominated 50 USDC must be quotable (>$10): {:?}", e.reason_detail),
        };
        // Quote leg is the exact taker input.
        assert!((f(&priced.quote_quantity_str) - 50.0).abs() < 1e-6);
        // Base derived from price (== mid at zero spread).
        assert!((f(&priced.quantity_str) - 50.0 / MID).abs() < 1e-3, "{}", priced.quantity_str);
    }

    /// Base-denominated path is unchanged: quote = base * price. 1000 EDELx ≈
    /// $13.6 clears the $10 floor.
    #[tokio::test]
    async fn base_denominated_unchanged() {
        let priced = match handler().price_rfq("t", "EDELx-USDC", 1, "1000", "", true, None, None, None).await {
            Ok(p) => p,
            Err(e) => panic!("1000 EDELx base ($13.6) is within [50, 10000] and >$10: {:?}", e.reason_detail),
        };
        assert!((f(&priced.quantity_str) - 1000.0).abs() < 1e-6);
        assert!((f(&priced.quote_quantity_str) - 1000.0 * MID).abs() < 1e-6);
    }

    /// A quote-denominated request that inverts to a base below the base
    /// min_quantity is rejected on the (moved) base bound, not silently taken.
    #[tokio::test]
    async fn quote_denominated_below_base_min_rejected() {
        // 0.5 USDC / 0.0136 ≈ 36.8 EDELx < min 50.
        let err = match handler().price_rfq("t", "EDELx-USDC", 1, "", "0.5", true, None, None, None).await {
            Ok(_) => panic!("derived base below min_quantity must reject"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::AmountTooSmall));
        assert!(err.reason_detail.as_deref().unwrap_or("").contains("Min quantity"));
    }

    /// V2 (`enforce_min_notional = false`) quotes dust the V1 path refuses:
    /// 100 EDELx ≈ $1.36 is under the $10 floor but above the base min (50).
    /// The server charges the user the 3x dust fee; the LP pays nothing on V2,
    /// so the floor must not fire. The base `min_quantity` bound still applies
    /// on both paths.
    #[tokio::test]
    async fn v2_skips_min_notional_floor_but_keeps_base_bound() {
        // V1: $1.36 < $10 → refused on the notional floor.
        let err = match handler().price_rfq("t", "EDELx-USDC", 1, "100", "", true, None, None, None).await {
            Ok(_) => panic!("V1 dust must still reject on min_notional_usd"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::AmountTooSmall));
        assert!(err.reason_detail.as_deref().unwrap_or("").contains("Min notional"));

        // V2: same request quotes.
        let priced = match handler().price_rfq("t", "EDELx-USDC", 1, "100", "", false, None, None, None).await {
            Ok(p) => p,
            Err(e) => panic!("V2 dust must be quotable: {:?}", e.reason_detail),
        };
        assert!((f(&priced.quantity_str) - 100.0).abs() < 1e-6);

        // V2 still enforces the base min_quantity bound (49 < 50).
        let err = match handler().price_rfq("t", "EDELx-USDC", 1, "49", "", false, None, None, None).await {
            Ok(_) => panic!("V2 below base min_quantity must reject"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::AmountTooSmall));
        assert!(err.reason_detail.as_deref().unwrap_or("").contains("Min quantity"));
    }

    /// A stale non-CC balance is rejected with its own reason, not quoted from.
    #[tokio::test]
    async fn stale_balances_rejected_with_distinct_reason() {
        let lm = agent_logic::liquidity::LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("EDELx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::new(1, 1)).await;
        let mut h = handler();
        h.liquidity_manager = Some(Arc::clone(&lm));

        lm.set_stale_after(std::time::Duration::ZERO);
        let err = match h.price_rfq("t", "EDELx-USDC", 1, "1000", "", false, None, None, None).await {
            Ok(_) => panic!("stale balance must reject"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::TemporarilyUnavailable));
        assert_eq!(err.reason_detail.as_deref(), Some("Balances stale"));

        lm.set_stale_after(agent_logic::liquidity::DEFAULT_BALANCE_STALE_AFTER);
        if let Err(e) = h.price_rfq("t", "EDELx-USDC", 1, "1000", "", false, None, None, None).await {
            panic!("fresh balance must quote: {:?}", e.reason_detail);
        }
    }

    /// The liquidity gate's CC fee headroom is V1-only: the LP pays its own
    /// dvp+alloc fees on a V1 settle (~$4 worst non-dust ⇒ 44 CC at $0.10 ×
    /// 1.1 margin), but pays NOTHING on V2 — a CC-poor LP must still quote V2.
    #[tokio::test]
    async fn v1_fee_headroom_requires_cc_but_v2_needs_none() {
        let lm = agent_logic::liquidity::LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(6)).await; // 1 CC free above the 5 reserve
        lm.update_token_balance("EDELx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::new(1, 1)).await; // $0.10 per CC
        let mut h = handler();
        h.liquidity_manager = Some(lm);

        // V1 (enforce_min_notional=true): fee_cc = 4×(0.3+0.7)/0.10×1.1 = 44 CC
        // needed for fees, 1 CC free ⇒ rejected on liquidity, not min-notional
        // (1000 EDELx ≈ $13.6 clears the $10 floor).
        let err = match h.price_rfq("t", "EDELx-USDC", 1, "1000", "", true, None, None, None).await {
            Ok(_) => panic!("V1 must reject on CC fee headroom"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::TemporarilyUnavailable));
        assert_eq!(err.reason_detail.as_deref(), Some("Insufficient liquidity"));

        // V2 (false): fee headroom is ZERO — same 1 free CC quotes fine.
        if let Err(e) = h.price_rfq("t", "EDELx-USDC", 1, "1000", "", false, None, None, None).await {
            panic!("V2 must quote with no CC headroom: {:?}", e.reason_detail);
        }
    }

    /// The stress coefficient widens a protective (>= 0) spread by the full
    /// multiplier but leaves a negative (aggressive) spread raw, so overload/
    /// depletion can only ever move the LP quote in its favour, never worsen it.
    #[test]
    fn stress_coefficient_only_widens_protective_spreads() {
        // Protective spread: full multiplier applied (e.g. overload 3 + depl 10).
        assert_eq!(stress_coefficient(2.5, 13.0), 13.0);
        assert_eq!(stress_coefficient(0.5, 3.0), 3.0);
        // Zero spread: multiplier returned but effect is nil (0 * m == 0).
        assert_eq!(stress_coefficient(0.0, 5.0), 5.0);
        // Aggressive (below-mid) spread: honoured raw — NOT amplified (no fire-sale).
        assert_eq!(stress_coefficient(-2.4, 13.0), 1.0);
        assert_eq!(stress_coefficient(-0.1, 3.0), 1.0);
    }

    /// A negative offer_spread (aggressive below-mid sell to offload inventory)
    /// is accepted — not rejected by the `price <= 0` guard — and prices exactly
    /// `mid * (1 + offer_spread/100)`, i.e. below mid. With `liquidity_manager:
    /// None` and no overload the coefficient is 1, pinning down that the negative
    /// spread is honoured at its raw configured value.
    #[tokio::test]
    async fn negative_offer_spread_sells_below_mid() {
        let lp_config: LiquidityProviderConfig =
            serde_json::from_str(r#"{"name":"LP test","min_notional_usd":10.0}"#).unwrap();
        // bid 2.5 + offer -2.4 → the EDELx/cETH 0.1%-spread offload config.
        let market: MarketConfig = serde_json::from_str(
            r#"{"market_id":"EDELx-USDC","rfq":{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":-2.4}}"#,
        )
        .unwrap();
        let mut mids = HashMap::new();
        mids.insert("EDELx-USDC".to_string(), mm(MID));
        let handler = RfqHandler {
            lp_config,
            markets: vec![market],
            venue_overrides: Vec::new(),
            mid_prices: Arc::new(RwLock::new(mids)),
            party_id: "lp::test".to_string(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: false,
        };
        // direction 1 = buy (LP sells base); 1000 EDELx ≈ $13.3 clears the $10 floor.
        let priced = match handler.price_rfq("t", "EDELx-USDC", 1, "1000", "", true, None, None, None).await {
            Ok(p) => p,
            Err(e) => panic!("negative offer_spread must be quotable, not rejected: {:?}", e.reason_detail),
        };
        let expected = MID * (1.0 - 0.024);
        assert!((f(&priced.price_str) - expected).abs() < 1e-9, "price {} vs expected {}", priced.price_str, expected);
        assert!(f(&priced.price_str) < MID, "aggressive offer must be below mid");
    }

    /// `disable_overload_spread_widening` pins the spread at its raw configured
    /// value under real sequencer overload, while an unflagged market still
    /// widens 3x. Coefficient 0.45 ∈ [0.4, 0.5) triggers fee-overload (3x
    /// multiplier) WITHOUT tripping the RFQ-reject threshold (0.4). Safe with
    /// concurrent tests: they use zero spreads (0 × 3 = 0) or a negative spread
    /// (stress-gated to raw), so a transient global overload cannot move them.
    /// Serializes the tests that read or flip the PROCESS-GLOBAL overload
    /// forecast (`agent_logic::forecast`), so one test's overload window can
    /// never widen another test's positive-spread quote mid-assertion.
    static OVERLOAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn disable_overload_widening_pins_raw_spread() {
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let lp_config: LiquidityProviderConfig =
            serde_json::from_str(r#"{"name":"LP test","min_notional_usd":10.0}"#).unwrap();
        // Same bid spread on both markets; only the flag differs. The absent
        // flag on the second market also proves the serde default is `false`.
        let pinned: MarketConfig = serde_json::from_str(
            r#"{"market_id":"EDELx-USDC","rfq":{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":-2.4,"disable_overload_spread_widening":true,"disable_depletion_spread_widening":true}}"#,
        )
        .unwrap();
        let widening: MarketConfig = serde_json::from_str(
            r#"{"market_id":"EDELx-USDCx","rfq":{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":-2.4}}"#,
        )
        .unwrap();
        let mut mids = HashMap::new();
        mids.insert("EDELx-USDC".to_string(), mm(MID));
        mids.insert("EDELx-USDCx".to_string(), mm(MID));
        let handler = RfqHandler {
            lp_config,
            markets: vec![pinned, widening],
            venue_overrides: Vec::new(),
            mid_prices: Arc::new(RwLock::new(mids)),
            party_id: "lp::test".to_string(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: false,
        };

        agent_logic::forecast::update_forecast(0, Some("0.45".to_string()));
        assert!(agent_logic::forecast::is_fees_paused_by_overload(), "0.45 < 0.5 must count as overload");
        assert!(!agent_logic::forecast::is_rfq_rejected_by_overload(), "0.45 >= 0.4 must still quote");

        // direction 2 = user sells → LP bids mid - bid_spread. 1000 EDELx ≈ $13 clears the floor.
        let pinned_bid = handler.price_rfq("t", "EDELx-USDC", 2, "1000", "", true, None, None, None).await;
        let widened_bid = handler.price_rfq("t", "EDELx-USDCx", 2, "1000", "", true, None, None, None).await;
        // Reset global overload state BEFORE asserting so a failure cannot leak it.
        agent_logic::forecast::update_forecast(0, None);
        assert!(!agent_logic::forecast::is_fees_paused_by_overload());

        let pinned_bid = match pinned_bid {
            Ok(p) => p,
            Err(e) => panic!("pinned market must quote under overload: {:?}", e.reason_detail),
        };
        let widened_bid = match widened_bid {
            Ok(p) => p,
            Err(e) => panic!("widening market must quote under overload: {:?}", e.reason_detail),
        };
        // Flagged market: raw 2.5% regardless of overload.
        let expected_raw = MID * (1.0 - 0.025);
        assert!((f(&pinned_bid.price_str) - expected_raw).abs() < 1e-9, "pinned {} vs {}", pinned_bid.price_str, expected_raw);
        // Unflagged market: 2.5% × 3 = 7.5% under overload (also proves flag default = false).
        let expected_widened = MID * (1.0 - 0.075);
        assert!((f(&widened_bid.price_str) - expected_widened).abs() < 1e-9, "widened {} vs {}", widened_bid.price_str, expected_widened);
    }

    /// Build the zero-spread handler() fixture with `[[venue_overrides]]`
    /// entries (JSON mirrors the TOML shape 1:1).
    fn handler_with_overrides(overrides_json: &str) -> RfqHandler {
        let mut h = handler();
        h.venue_overrides = serde_json::from_str(overrides_json).unwrap();
        h
    }

    /// A venue override re-prices ONLY the matching venue: the overlaid offer
    /// spread applies to that venue's quotes while another venue and the
    /// venue-less (V1) path keep the pair's raw config.
    #[tokio::test]
    async fn venue_override_changes_spread_for_matching_venue_only() {
        // Positive spread ⇒ overload-sensitive ⇒ hold the global-state lock.
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let h = handler_with_overrides(
            r#"[{"venue":"walley","rfq":{"offer_spread_percent":2.0}}]"#,
        );

        // Matching venue: pair offer spread 0.0 overridden to 2.0.
        let priced = h
            .price_rfq("t", "EDELx-USDC", 1, "1000", "", true, Some("walley"), None, None)
            .await
            .expect("override venue must quote");
        let expected = MID * 1.02;
        assert!((f(&priced.price_str) - expected).abs() < 1e-9, "walley {} vs {}", priced.price_str, expected);

        // Non-matching venue and no venue: raw pair config (price == mid).
        for venue in [Some("lattice"), None] {
            let priced = h
                .price_rfq("t", "EDELx-USDC", 1, "1000", "", true, venue, None, None)
                .await
                .expect("non-override path must quote");
            assert!((f(&priced.price_str) - MID).abs() < 1e-9, "{:?} {} vs mid {}", venue, priced.price_str, MID);
        }
    }

    /// A venue override can pin widening for one venue while the pair keeps
    /// widening for everyone else — the venue-scoped mirror of
    /// `disable_overload_widening_pins_raw_spread`.
    #[tokio::test]
    async fn venue_override_pins_widening_for_matching_venue() {
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let lp_config: LiquidityProviderConfig =
            serde_json::from_str(r#"{"name":"LP test","min_notional_usd":10.0}"#).unwrap();
        // Pair has NO disable flags — widens 3x under overload by default.
        let market: MarketConfig = serde_json::from_str(
            r#"{"market_id":"EDELx-USDC","rfq":{"min_quantity":"50","max_quantity":"10000","bid_spread_percent":2.5,"offer_spread_percent":-2.4}}"#,
        )
        .unwrap();
        let mut mids = HashMap::new();
        mids.insert("EDELx-USDC".to_string(), mm(MID));
        let handler = RfqHandler {
            lp_config,
            markets: vec![market],
            venue_overrides: serde_json::from_str(
                r#"[{"venue":"walley","rfq":{"disable_overload_spread_widening":true,"disable_depletion_spread_widening":true}}]"#,
            )
            .unwrap(),
            mid_prices: Arc::new(RwLock::new(mids)),
            party_id: "lp::test".to_string(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: false,
        };

        agent_logic::forecast::update_forecast(0, Some("0.45".to_string()));
        assert!(agent_logic::forecast::is_fees_paused_by_overload());

        // direction 2 = user sells → LP bids mid - bid_spread.
        let pinned = handler.price_rfq("t", "EDELx-USDC", 2, "1000", "", true, Some("walley"), None, None).await;
        let widened = handler.price_rfq("t", "EDELx-USDC", 2, "1000", "", true, Some("lattice"), None, None).await;
        // Reset global overload state BEFORE asserting so a failure cannot leak it.
        agent_logic::forecast::update_forecast(0, None);

        let pinned = pinned.expect("override venue must quote under overload");
        let widened = widened.expect("other venue must quote under overload");
        // Override venue: raw 2.5% regardless of overload.
        let expected_raw = MID * (1.0 - 0.025);
        assert!((f(&pinned.price_str) - expected_raw).abs() < 1e-9, "pinned {} vs {}", pinned.price_str, expected_raw);
        // Everyone else: 2.5% × 3 = 7.5%.
        let expected_widened = MID * (1.0 - 0.075);
        assert!((f(&widened.price_str) - expected_widened).abs() < 1e-9, "widened {} vs {}", widened.price_str, expected_widened);
    }

    /// `enabled = false` in an override closes the pair to that venue only;
    /// the venue-less path (and other venues) still quote.
    #[tokio::test]
    async fn venue_override_disabled_venue_rejected() {
        let h = handler_with_overrides(r#"[{"venue":"walley","rfq":{"enabled":false}}]"#);

        let err = match h
            .price_rfq("t", "EDELx-USDC", 1, "1000", "", true, Some("walley"), None, None)
            .await
        {
            Ok(_) => panic!("venue-disabled override must reject"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::MarketNotSupported));
        assert!(err.reason_detail.as_deref().unwrap_or("").contains("venue"));

        if let Err(e) = h.price_rfq("t", "EDELx-USDC", 1, "1000", "", true, None, None, None).await {
            panic!("venue-less path must still quote: {:?}", e.reason_detail);
        }
    }

    /// A venue-scoped `min_quantity` bounds ONLY that venue's requests.
    #[tokio::test]
    async fn venue_override_min_quantity_bound() {
        let h = handler_with_overrides(r#"[{"venue":"walley","rfq":{"min_quantity":"2000"}}]"#);

        let err = match h
            .price_rfq("t", "EDELx-USDC", 1, "1000", "", true, Some("walley"), None, None)
            .await
        {
            Ok(_) => panic!("1000 < venue min 2000 must reject"),
            Err(e) => e,
        };
        assert!(matches!(err.reason, RfqRejectionReason::AmountTooSmall));
        assert_eq!(err.min_quantity.as_deref(), Some("2000"), "reject must carry the VENUE bound");

        // Pair default min (50) still governs the venue-less path.
        if let Err(e) = h.price_rfq("t", "EDELx-USDC", 1, "1000", "", true, None, None, None).await {
            panic!("1000 > pair min 50 must quote without the override: {:?}", e.reason_detail);
        }
    }

    // Size-aware pricing

    use agent_logic::pool_impact::PoolDepth;

    const R: f64 = 1_000_000.0; // synthetic size reference

    /// Market with a NEGATIVE offer spread, positive bid spread, wide bounds
    /// and a `pool_impact` section; enabled/depth are per-test knobs.
    fn impact_handler(enabled: bool, with_depth: bool) -> RfqHandler {
        let lp_config: LiquidityProviderConfig =
            serde_json::from_str(r#"{"name":"LP test","min_notional_usd":0.0}"#).unwrap();
        let market: MarketConfig = serde_json::from_str(&format!(
            r#"{{"market_id":"TKN-USDCx","rfq":{{"min_quantity":"1","max_quantity":"10000000",
                "bid_spread_percent":2.55,"offer_spread_percent":-1.95,
                "pool_impact":{{"enabled":{enabled},"max_impact_percent":50.0,
                                "max_pool_fraction":0.9,"impact_multiplier":1.0,
                                "free_zone_base":0.0,"window_hours":24.0}}}}}}"#,
        ))
        .unwrap();
        let mut mids = HashMap::new();
        mids.insert(
            "TKN-USDCx".to_string(),
            MarketMid {
                mid: MID,
                pool_depth: with_depth.then(|| PoolDepth {
                    base_reserve: R,
                }),
            },
        );
        RfqHandler {
            lp_config,
            markets: vec![market],
            venue_overrides: Vec::new(),
            mid_prices: Arc::new(RwLock::new(mids)),
            party_id: "lp::test".to_string(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
            net_positions: None,
            rfq_v2_only: false,
        }
    }

    fn tracker() -> Arc<agent_logic::net_position::NetPositionTracker> {
        let path = std::env::temp_dir()
            .join("silvana-rfq-impact-tests")
            .join(format!("{}.json", Uuid::now_v7()));
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        agent_logic::net_position::NetPositionTracker::load_or_new(
            path,
            24.0,
            agent_logic::config::RfqV2Config::default().stale_pending_after(),
        )
    }

    /// INVARIANT 1: a negative offer spread still receives the adjustment,
    /// because it composes outside the stress coefficient.
    #[tokio::test]
    async fn negative_offer_spread_still_receives_impact() {
        let h = impact_handler(true, true);
        // Buy 100k against a 1M reference from net 0.
        let priced = h
            .price_rfq("t", "TKN-USDCx", 1, "100000", "", false, None, None, Some("party-a::1"))
            .await
            .expect("must quote");
        let impact = 100.0 * 0.1 / 0.9;
        let expected = MID * (1.0 + (-1.95 + impact) / 100.0);
        assert!(
            (f(&priced.price_str) - expected).abs() < 1e-9,
            "price {} vs expected {} (impact must survive the negative spread)",
            priced.price_str,
            expected
        );
        assert!(f(&priced.price_str) > MID, "11.1% impact must overwhelm the -1.95% shift");
        let pp = priced.pool_pricing.expect("applied impact must be carried on PricedQuote");
        assert!((pp.impact_pct - impact).abs() < 1e-9);
        assert_eq!(pp.base_reserve, R);
        assert_eq!(pp.eff_spread_base, -1.95);
    }

    /// INVARIANT 2: no depth produces byte-identical wire strings to a handler
    /// with no config at all. Same for shadow mode with depth.
    #[tokio::test]
    async fn no_depth_and_shadow_are_byte_identical_to_legacy() {
        // dir=2 prices the POSITIVE 2.55 bid spread ⇒ overload-sensitive.
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Legacy = section enabled but no depth arriving (the pre-A3 server).
        let legacy = impact_handler(true, false);
        let shadow = impact_handler(false, true);
        for (dir, qty, qq) in [(1, "100000", ""), (2, "100000", ""), (1, "", "1360")] {
            let l = legacy
                .price_rfq("t", "TKN-USDCx", dir, qty, qq, false, None, None, Some("p::1"))
                .await
                .expect("legacy must quote");
            let s = shadow
                .price_rfq("t", "TKN-USDCx", dir, qty, qq, false, None, None, Some("p::1"))
                .await
                .expect("shadow must quote");
            assert_eq!(l.price_str, s.price_str, "dir={dir}");
            assert_eq!(l.quantity_str, s.quantity_str, "dir={dir}");
            assert_eq!(l.quote_quantity_str, s.quote_quantity_str, "dir={dir}");
            assert!(l.pool_pricing.is_none(), "no-depth must not arm the re-check");
            assert!(s.pool_pricing.is_none(), "shadow must not arm the re-check");
            // And both equal the raw -1.95/2.55 spread math.
            let expected = if dir == 1 { MID * (1.0 - 0.0195) } else { MID * (1.0 - 0.0255) };
            assert!((f(&l.price_str) - expected).abs() < 1e-12);
        }
    }

    /// Buy X then sell X returns the net to ≈0, so the next quote on either
    /// side carries only that trade's own size term.
    #[tokio::test]
    async fn round_trip_ends_with_fresh_counterparty_pricing() {
        // dir=2 legs price the POSITIVE bid spread ⇒ overload-sensitive.
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = impact_handler(true, true);
        let t = tracker();
        h.net_positions = Some(t.clone());

        // Fresh-counterparty reference prices for a small follow-up trade.
        let fresh_buy = h
            .price_rfq("t", "TKN-USDCx", 1, "1000", "", false, None, None, Some("bob::1"))
            .await
            .unwrap();
        let fresh_sell = h
            .price_rfq("t", "TKN-USDCx", 2, "1000", "", false, None, None, Some("bob::1"))
            .await
            .unwrap();

        // Alice round-trips 200k.
        t.record_confirm("q-buy", Some("alice::1"), "TKN", 200_000.0);
        t.settle("q-buy");
        t.record_confirm("q-sell", Some("alice::1"), "TKN", -200_000.0);
        t.settle("q-sell");
        assert!(t.net("alice::1", "TKN").abs() < 1.0);

        let rt_buy = h
            .price_rfq("t", "TKN-USDCx", 1, "1000", "", false, None, None, Some("alice::1"))
            .await
            .unwrap();
        let rt_sell = h
            .price_rfq("t", "TKN-USDCx", 2, "1000", "", false, None, None, Some("alice::1"))
            .await
            .unwrap();
        // Decay over test microseconds is negligible; prices agree to <1e-9.
        assert!((f(&rt_buy.price_str) - f(&fresh_buy.price_str)).abs() < 1e-9);
        assert!((f(&rt_sell.price_str) - f(&fresh_sell.price_str)).abs() < 1e-9);
    }

    /// With the net accumulating, each successive same-size buy quotes a
    /// strictly higher offer price.
    #[tokio::test]
    async fn buy_and_hold_widens_progressively() {
        let mut h = impact_handler(true, true);
        let t = tracker();
        h.net_positions = Some(t.clone());

        let mut last = 0.0;
        for i in 0..5 {
            let priced = h
                .price_rfq("t", "TKN-USDCx", 1, "50000", "", false, None, None, Some("party-b::1"))
                .await
                .unwrap();
            let p = f(&priced.price_str);
            assert!(p > last, "step {i}: {p} must exceed {last}");
            last = p;
            let qid = format!("q{i}");
            t.record_confirm(&qid, Some("party-b::1"), "TKN", 50_000.0);
            t.settle(&qid);
        }
    }

    /// Quote-denominated mode: the solution satisfies b·price = Q, checked
    /// numerically for consistency and monotonicity.
    #[tokio::test]
    async fn quote_denominated_solves_fixed_point() {
        let h = impact_handler(true, true);
        // Pay 1,360 USDCx (≈ 100k base at the unadjusted mid).
        let priced = h
            .price_rfq("t", "TKN-USDCx", 1, "", "1360", false, None, None, Some("party-b::1"))
            .await
            .expect("must quote");
        let b = f(&priced.quantity_str);
        let p = f(&priced.price_str);
        // The two legs are consistent to the bisection tolerance plus the
        // {:.10} wire-string rounding of the price (~4e-9 relative here).
        assert!((b * p - 1360.0).abs() / 1360.0 < 1e-7, "b·p = {} vs Q = 1360", b * p);
        // The derived base is STRICTLY LESS than the impact-free inversion —
        // slicing the request cannot manufacture extra base.
        let naive_b = 1360.0 / (MID * (1.0 - 0.0195));
        assert!(b < naive_b, "impact must shrink the base leg: {b} vs naive {naive_b}");
        // And the impact actually applied is the marginal charge at b.
        let pp = priced.pool_pricing.expect("applied");
        let expect_impact = {
            let u = (b / R).clamp(0.0, 0.9);
            100.0 * u / (1.0 - u)
        };
        assert!((pp.impact_pct - expect_impact).abs() < 1e-6);
    }

    /// The sell side keys on negative net: a net-seller gets a lower bid, a
    /// net-buyer selling back gets the plain bid.
    #[tokio::test]
    async fn sell_side_sign_conventions() {
        // dir=2 prices the POSITIVE bid spread ⇒ overload-sensitive.
        let _overload_guard = OVERLOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = impact_handler(true, true);
        let t = tracker();
        h.net_positions = Some(t.clone());

        // seller::1 has net-sold us 300k (net = -300k).
        t.record_confirm("s1", Some("seller::1"), "TKN", -300_000.0);
        t.settle("s1");
        // buyer::1 holds +300k (round-tripping back).
        t.record_confirm("b1", Some("buyer::1"), "TKN", 300_000.0);
        t.settle("b1");

        let widened = h
            .price_rfq("t", "TKN-USDCx", 2, "10000", "", false, None, None, Some("seller::1"))
            .await
            .unwrap();
        let plain = h
            .price_rfq("t", "TKN-USDCx", 2, "10000", "", false, None, None, Some("buyer::1"))
            .await
            .unwrap();
        assert!(
            f(&widened.price_str) < f(&plain.price_str),
            "persistent net-seller must be bid lower: {} vs {}",
            widened.price_str,
            plain.price_str
        );
        // The round-tripping buyer pays no sell-side penalty at all: the plain
        // bid equals the raw 2.55% spread.
        let raw_bid = MID * (1.0 - 0.0255);
        assert!((f(&plain.price_str) - raw_bid).abs() < 1e-9);
    }
}
