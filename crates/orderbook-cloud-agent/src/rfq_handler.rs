//! RFQ Handler for LP cloud agents
//!
//! Handles incoming RFQ requests from the orderbook server and responds
//! with quotes or rejections based on market configuration and mid-prices.
//!
//! When a LiquidityManager is configured, the handler:
//! 1. Rejects RFQs when the LP lacks sufficient balance for the allocation + fees
//! 2. Widens spreads based on token depletion rate (depletion coefficient)

use orderbook_agent_logic::config::{BaseConfig, LiquidityProviderConfig, MarketConfig};
use orderbook_agent_logic::liquidity::LiquidityManager;
use orderbook_agent_logic::runner::QuotedTrade;
use rust_decimal::Decimal;
use std::collections::HashMap;
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

    /// Handle an incoming RFQ request
    pub async fn handle_rfq_request(&self, request: RfqRequest) -> RfqResponse {
        let rfq_id = request.rfq_id.clone();

        // Find market config
        let market_config = self.markets.iter().find(|m| m.market_id == request.market_id);
        let market_config = match market_config {
            Some(m) if m.enabled => m,
            _ => {
                debug!("RFQ {}: market {} not configured or disabled", rfq_id, request.market_id);
                return RfqResponse::Reject(RfqReject {
                    rfq_id,
                    lp_party_id: self.party_id.clone(),
                    lp_name: self.lp_config.name.clone(),
                    reason: RfqRejectionReason::MarketNotSupported as i32,
                    reason_detail: Some(format!("Market {} not supported", request.market_id)),
                    rejected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                    min_quantity: None,
                    max_quantity: None,
                });
            }
        };

        // Check RFQ market config
        let rfq_config = match &market_config.rfq {
            Some(rfq) if rfq.enabled => rfq,
            _ => {
                debug!("RFQ {}: RFQ not enabled for market {}", rfq_id, request.market_id);
                return RfqResponse::Reject(RfqReject {
                    rfq_id,
                    lp_party_id: self.party_id.clone(),
                    lp_name: self.lp_config.name.clone(),
                    reason: RfqRejectionReason::MarketNotSupported as i32,
                    reason_detail: Some("RFQ not enabled for this market".to_string()),
                    rejected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                    min_quantity: None,
                    max_quantity: None,
                });
            }
        };

        // Parse quantity
        let quantity: f64 = match request.quantity.parse() {
            Ok(q) => q,
            Err(_) => {
                return RfqResponse::Reject(RfqReject {
                    rfq_id,
                    lp_party_id: self.party_id.clone(),
                    lp_name: self.lp_config.name.clone(),
                    reason: RfqRejectionReason::Unspecified as i32,
                    reason_detail: Some("Invalid quantity".to_string()),
                    rejected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                    min_quantity: None,
                    max_quantity: None,
                });
            }
        };

        // Check quantity bounds
        let min_qty: f64 = rfq_config.min_quantity.parse().unwrap_or(0.0);
        let max_qty: f64 = rfq_config.max_quantity.parse().unwrap_or(f64::MAX);

        if quantity < min_qty {
            return RfqResponse::Reject(RfqReject {
                rfq_id,
                lp_party_id: self.party_id.clone(),
                lp_name: self.lp_config.name.clone(),
                reason: RfqRejectionReason::AmountTooSmall as i32,
                reason_detail: Some(format!("Min quantity: {}", min_qty)),
                rejected_at: Some(prost_types::Timestamp {
                    seconds: chrono::Utc::now().timestamp(),
                    nanos: 0,
                }),
                min_quantity: Some(rfq_config.min_quantity.clone()),
                max_quantity: Some(rfq_config.max_quantity.clone()),
            });
        }

        if quantity > max_qty {
            return RfqResponse::Reject(RfqReject {
                rfq_id,
                lp_party_id: self.party_id.clone(),
                lp_name: self.lp_config.name.clone(),
                reason: RfqRejectionReason::AmountTooLarge as i32,
                reason_detail: Some(format!("Max quantity: {}", max_qty)),
                rejected_at: Some(prost_types::Timestamp {
                    seconds: chrono::Utc::now().timestamp(),
                    nanos: 0,
                }),
                min_quantity: Some(rfq_config.min_quantity.clone()),
                max_quantity: Some(rfq_config.max_quantity.clone()),
            });
        }

        // Reject RFQs when sequencer is critically overloaded (coefficient < OVERLOAD_THRESHOLD - 0.1).
        // At this level even proposing new trades would fail with SEQUENCER_BACKPRESSURE.
        if orderbook_agent_logic::forecast::is_rfq_rejected_by_overload() {
            warn!("RFQ {}: rejected — sequencer critically overloaded", rfq_id);
            return RfqResponse::Reject(RfqReject {
                rfq_id,
                lp_party_id: self.party_id.clone(),
                lp_name: self.lp_config.name.clone(),
                reason: RfqRejectionReason::MarketConditions as i32,
                reason_detail: Some("High demand, try later".to_string()),
                rejected_at: Some(prost_types::Timestamp {
                    seconds: chrono::Utc::now().timestamp(),
                    nanos: 0,
                }),
                min_quantity: None,
                max_quantity: None,
            });
        }

        // Get mid-price
        let mid_prices = self.mid_prices.read().await;
        let mid_price = match mid_prices.get(&request.market_id) {
            Some(&price) if price > 0.0 => price,
            _ => {
                warn!("RFQ {}: no mid-price for market {}", rfq_id, request.market_id);
                return RfqResponse::Reject(RfqReject {
                    rfq_id,
                    lp_party_id: self.party_id.clone(),
                    lp_name: self.lp_config.name.clone(),
                    reason: RfqRejectionReason::TemporarilyUnavailable as i32,
                    reason_detail: Some("No mid-price available".to_string()),
                    rejected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                    min_quantity: None,
                    max_quantity: None,
                });
            }
        };
        drop(mid_prices);

        // Parse market_id into base/quote tokens (e.g. "CC-USDCx" → ["CC", "USDCx"])
        let market_parts: Vec<&str> = request.market_id.split('-').collect();
        let (base_token, quote_token) = if market_parts.len() == 2 {
            (market_parts[0], market_parts[1])
        } else {
            (request.market_id.as_str(), "")
        };

        // Compute price based on direction and spread.
        // Widen spreads when sequencer is under load:
        //   3x when coefficient < SEQUENCER_OVERLOAD_THRESHOLD (extreme load)
        //   2x when forecast is LOW (heavy load)
        //   1x otherwise
        // direction 1 = BUY (user buys, LP sells base → offer price = mid + spread)
        // direction 2 = SELL (user sells, LP buys base/sells quote → bid price = mid - spread)
        let spread_multiplier = if orderbook_agent_logic::forecast::is_fees_paused_by_overload() {
            3.0
        } else if orderbook_agent_logic::forecast::is_traffic_paused_by_forecast() {
            2.0
        } else {
            1.0
        };

        // Depletion coefficient: widen spread on the side that depletes a scarce token
        let depletion_coeff = if let Some(ref lm) = self.liquidity_manager {
            // The token being sold by the LP is the one that depletes
            let depleting_token = if request.direction == 1 {
                base_token // LP sells base (e.g. CC)
            } else {
                quote_token // LP sells quote (e.g. USDCx)
            };
            lm.depletion_coefficient(depleting_token).await
        } else {
            0.0
        };

        let price = if request.direction == 1 {
            // User is buying → LP offers at mid + spread
            mid_price * (1.0 + rfq_config.offer_spread_percent * spread_multiplier * (1.0 + depletion_coeff) / 100.0)
        } else {
            // User is selling → LP bids at mid - spread
            mid_price * (1.0 - rfq_config.bid_spread_percent * spread_multiplier * (1.0 + depletion_coeff) / 100.0)
        };

        let quote_quantity = quantity * price;

        // Guard against NaN/infinity from misconfigured spreads
        if !price.is_finite() || !quote_quantity.is_finite() || price <= 0.0 {
            warn!("RFQ {}: computed invalid price {:.6} or quantity {:.6}", rfq_id, price, quote_quantity);
            return RfqResponse::Reject(RfqReject {
                rfq_id,
                lp_party_id: self.party_id.clone(),
                lp_name: self.lp_config.name.clone(),
                reason: RfqRejectionReason::TemporarilyUnavailable as i32,
                reason_detail: Some("Price computation error".to_string()),
                rejected_at: Some(prost_types::Timestamp {
                    seconds: chrono::Utc::now().timestamp(),
                    nanos: 0,
                }),
                min_quantity: None,
                max_quantity: None,
            });
        }

        // Liquidity gate: reject if LP lacks sufficient balance for the allocation + estimated fees
        if let Some(ref lm) = self.liquidity_manager {
            // LP allocates base when user buys (dir=1), quote when user sells (dir=2)
            let (alloc_token, alloc_amount) = if request.direction == 1 {
                (base_token, quantity)     // LP sells base
            } else {
                (quote_token, quote_quantity) // LP sells quote
            };

            // Rough fee estimate: ~2 USD total for LP's dvp + allocation fees
            let fee_cc = lm.estimate_fee_cc(Decimal::TWO).await;
            let alloc_dec = Decimal::from_f64_retain(alloc_amount).unwrap_or_default();

            let available = lm.available(alloc_token).await;
            let needed = if alloc_token == orderbook_agent_logic::liquidity::CC_TOKEN {
                alloc_dec + fee_cc
            } else {
                alloc_dec
            };
            // Also check CC for fees when allocating non-CC
            let cc_ok = if alloc_token != orderbook_agent_logic::liquidity::CC_TOKEN {
                lm.available_cc().await >= fee_cc
            } else {
                true // already checked above
            };

            if available < needed || !cc_ok {
                warn!(
                    "RFQ {}: rejected — insufficient {} ({:.4} available, {:.4} needed){}",
                    rfq_id, alloc_token, available, needed,
                    if !cc_ok { format!(", CC for fees: {:.4}", lm.available_cc().await) } else { String::new() }
                );
                return RfqResponse::Reject(RfqReject {
                    rfq_id,
                    lp_party_id: self.party_id.clone(),
                    lp_name: self.lp_config.name.clone(),
                    reason: RfqRejectionReason::TemporarilyUnavailable as i32,
                    reason_detail: Some("Insufficient liquidity".to_string()),
                    rejected_at: Some(prost_types::Timestamp {
                        seconds: chrono::Utc::now().timestamp(),
                        nanos: 0,
                    }),
                    min_quantity: None,
                    max_quantity: None,
                });
            }
        }

        let quote_id = Uuid::now_v7().to_string();
        let valid_for_secs = rfq_config
            .quote_valid_secs
            .unwrap_or(self.lp_config.default_quote_valid_secs);

        let now = chrono::Utc::now();
        let valid_until = now + chrono::Duration::seconds(valid_for_secs as i64);

        let effective_spread = if request.direction == 1 {
            rfq_config.offer_spread_percent * spread_multiplier * (1.0 + depletion_coeff)
        } else {
            rfq_config.bid_spread_percent * spread_multiplier * (1.0 + depletion_coeff)
        };
        info!(
            "RFQ {}: quoting {} {} @ {:.6} (mid={:.6}, spread={:.2}%{}{})",
            rfq_id,
            quantity,
            request.market_id,
            price,
            mid_price,
            effective_spread,
            if spread_multiplier >= 3.0 { " OVERLOAD 3x" } else if spread_multiplier > 1.0 { " LOW-ISS 2x" } else { "" },
            if depletion_coeff > 0.0 { format!(" depl={:.1}", depletion_coeff) } else { String::new() }
        );

        // Record trade for settlement verification
        self.quoted_trades.lock().await.push(QuotedTrade {
            market_id: request.market_id.clone(),
            price: format!("{:.10}", price),
            base_quantity: format!("{:.10}", quantity),
            quote_quantity: format!("{:.10}", quote_quantity),
        });

        RfqResponse::Quote(RfqQuote {
            rfq_id,
            quote_id,
            market_id: request.market_id,
            direction: request.direction,
            quantity: format!("{:.10}", quantity),
            price: format!("{:.10}", price),
            quote_quantity: format!("{:.10}", quote_quantity),
            valid_for_secs,
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
            allocate_before_secs: Some(rfq_config.allocate_before_secs),
            settle_before_secs: Some(rfq_config.settle_before_secs),
        })
    }
}
