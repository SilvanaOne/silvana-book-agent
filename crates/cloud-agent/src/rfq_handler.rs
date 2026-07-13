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

use agent_logic::config::{BaseConfig, LiquidityProviderConfig, MarketConfig};
use agent_logic::liquidity::LiquidityManager;
use agent_logic::runner::QuotedTrade;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use orderbook_proto::settlement::{
    RfqRequest, RfqQuote, RfqReject, RfqRejectionReason,
};

/// RFQ handler that computes quotes based on market config and mid-prices
pub struct RfqHandler {
    lp_config: LiquidityProviderConfig,
    markets: Vec<MarketConfig>,
    /// Market mid-prices: market_id -> mid_price
    mid_prices: Arc<RwLock<HashMap<String, f64>>>,
    party_id: String,
    /// Trades we quoted (for settlement verification)
    quoted_trades: Arc<Mutex<Vec<QuotedTrade>>>,
    /// Liquidity manager for balance checks and depletion-based spread adjustment
    liquidity_manager: Option<Arc<LiquidityManager>>,
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
}

/// A rejection from the shared pricing pipeline.
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

impl RfqHandler {
    pub fn new(config: &BaseConfig) -> Option<Self> {
        let lp_config = config.liquidity_provider.clone()?;

        Some(Self {
            lp_config,
            markets: config.markets.clone(),
            mid_prices: Arc::new(RwLock::new(HashMap::new())),
            party_id: config.party_id.clone(),
            quoted_trades: Arc::new(Mutex::new(Vec::new())),
            liquidity_manager: None,
        })
    }

    /// Set the liquidity manager for balance checks and spread adjustment
    pub fn set_liquidity_manager(&mut self, lm: Arc<LiquidityManager>) {
        self.liquidity_manager = Some(lm);
    }

    /// Get a reference to mid_prices for external updates
    pub fn mid_prices(&self) -> Arc<RwLock<HashMap<String, f64>>> {
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
            if let Some(&p) = mids.get(&format!("{token}-{stable}")) {
                if p > 0.0 {
                    return Some(p);
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
    pub(crate) async fn price_rfq(
        &self,
        rfq_id: &str,
        market_id: &str,
        direction: i32,
        quantity_str: &str,
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

        // Check RFQ market config
        let rfq_config = match &market_config.rfq {
            Some(rfq) if rfq.enabled => rfq,
            _ => {
                debug!("RFQ {}: RFQ not enabled for market {}", rfq_id, market_id);
                return Err(RejectInfo::new(
                    RfqRejectionReason::MarketNotSupported,
                    "RFQ not enabled for this market",
                ));
            }
        };

        // Parse quantity
        let quantity: f64 = match quantity_str.parse() {
            Ok(q) => q,
            Err(_) => {
                return Err(RejectInfo::new(
                    RfqRejectionReason::Unspecified,
                    "Invalid quantity",
                ));
            }
        };

        // Check quantity bounds
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

        // Get mid-price
        let mid_prices = self.mid_prices.read().await;
        let mid_price = match mid_prices.get(market_id) {
            Some(&price) if price > 0.0 => price,
            _ => {
                warn!("RFQ {}: no mid-price for market {}", rfq_id, market_id);
                return Err(RejectInfo::new(
                    RfqRejectionReason::TemporarilyUnavailable,
                    "No mid-price available",
                ));
            }
        };
        drop(mid_prices);

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
        let spread_multiplier = if agent_logic::forecast::is_fees_paused_by_overload() {
            3.0
        } else if agent_logic::forecast::is_traffic_paused_by_forecast() {
            2.0
        } else {
            1.0
        };

        // Depletion coefficient: widen spread on the side that depletes a scarce token
        let depletion_coeff = if let Some(ref lm) = self.liquidity_manager {
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

        let price = if direction == 1 {
            // User is buying → LP offers at mid + spread
            mid_price * (1.0 + rfq_config.offer_spread_percent * (spread_multiplier + depletion_coeff) / 100.0)
        } else {
            // User is selling → LP bids at mid - spread
            mid_price * (1.0 - rfq_config.bid_spread_percent * (spread_multiplier + depletion_coeff) / 100.0)
        };

        let quote_quantity = quantity * price;

        // Guard against NaN/infinity from misconfigured spreads
        if !price.is_finite() || !quote_quantity.is_finite() || price <= 0.0 {
            warn!("RFQ {}: computed invalid price {:.6} or quantity {:.6}", rfq_id, price, quote_quantity);
            return Err(RejectInfo::new(
                RfqRejectionReason::TemporarilyUnavailable,
                "Price computation error",
            ));
        }

        // USD reference for the quote leg (feeds the min-notional floor and the
        // V2 ticket-threshold decision).
        let usd_per_quote = self.token_usd_price(quote_token).await;
        let notional_usd = usd_per_quote.map(|p| quote_quantity * p);

        // USD minimum-value floor (global LP default, per-market override).
        // Refuse to quote RFQs whose USD value falls below the configured minimum.
        let min_notional_usd = rfq_config
            .min_notional_usd
            .unwrap_or(self.lp_config.min_notional_usd);
        if min_notional_usd > 0.0 {
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

            // Rough fee estimate: ~2 USD total for LP's dvp + allocation fees
            let fee_cc = lm.estimate_fee_cc(Decimal::TWO).await;
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

        let effective_spread = if direction == 1 {
            rfq_config.offer_spread_percent * (spread_multiplier + depletion_coeff)
        } else {
            rfq_config.bid_spread_percent * (spread_multiplier + depletion_coeff)
        };
        info!(
            "RFQ {}: quoting {} {} @ {:.6} (mid={:.6}, spread={:.2}%{}{})",
            rfq_id,
            quantity,
            market_id,
            price,
            mid_price,
            effective_spread,
            if spread_multiplier >= 3.0 { " OVERLOAD 3x" } else if spread_multiplier > 1.0 { " LOW-ISS 2x" } else { "" },
            if depletion_coeff > 0.0 { format!(" depl={:.1}", depletion_coeff) } else { String::new() }
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

        let priced = match self
            .price_rfq(&rfq_id, &request.market_id, request.direction, &request.quantity)
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
