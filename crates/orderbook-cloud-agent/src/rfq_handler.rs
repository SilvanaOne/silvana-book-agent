//! RFQ Handler for LP cloud agents
//!
//! Handles incoming RFQ requests from the orderbook server and responds
//! with quotes or rejections based on market configuration and mid-prices.

use orderbook_agent_logic::config::{BaseConfig, LiquidityProviderConfig, MarketConfig};
use orderbook_agent_logic::runner::QuotedTrade;
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
        })
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

        // Compute price based on direction and spread.
        // Widen spreads 2x when issuance forecast is LOW — high demand period means
        // sequencer under heavy load, protect LP from adverse fills during volatility.
        // direction 1 = BUY (user buys, LP sells → offer price = mid + spread)
        // direction 2 = SELL (user sells, LP buys → bid price = mid - spread)
        let spread_multiplier = if orderbook_agent_logic::forecast::is_traffic_paused_by_forecast() {
            2.0
        } else {
            1.0
        };
        let price = if request.direction == 1 {
            // User is buying → LP offers at mid + spread
            mid_price * (1.0 + rfq_config.offer_spread_percent * spread_multiplier / 100.0)
        } else {
            // User is selling → LP bids at mid - spread
            mid_price * (1.0 - rfq_config.bid_spread_percent * spread_multiplier / 100.0)
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

        let quote_id = Uuid::now_v7().to_string();
        let valid_for_secs = rfq_config
            .quote_valid_secs
            .unwrap_or(self.lp_config.default_quote_valid_secs);

        let now = chrono::Utc::now();
        let valid_until = now + chrono::Duration::seconds(valid_for_secs as i64);

        let effective_spread = if request.direction == 1 {
            rfq_config.offer_spread_percent * spread_multiplier
        } else {
            rfq_config.bid_spread_percent * spread_multiplier
        };
        info!(
            "RFQ {}: quoting {} {} @ {:.6} (mid={:.6}, spread={}%{})",
            rfq_id,
            quantity,
            request.market_id,
            price,
            mid_price,
            effective_spread,
            if spread_multiplier > 1.0 { " LOW-ISS 2x" } else { "" }
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
