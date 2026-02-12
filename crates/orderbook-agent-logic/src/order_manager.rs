//! Order management for the orderbook agent
//!
//! Handles order placement and cancellation via the orderbook service.
//! All orders are signed and tracked for settlement verification.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use orderbook_proto::orderbook::{Order, OrderType};
use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::{BaseConfig, MarketConfig};
use crate::order_tracker::OrderTracker;

/// Per-side result of balance check
#[derive(Debug, Clone, Copy)]
pub struct GridAffordability {
    pub can_bid: bool,
    pub can_offer: bool,
}

/// Order manager handles order placement and tracking
pub struct OrderManager {
    config: BaseConfig,
    client: OrderbookClient,
    tracker: Arc<Mutex<OrderTracker>>,
    last_grid_prices: HashMap<String, f64>,
    balances: Vec<TokenBalance>,
    tick_sizes: HashMap<String, f64>,
}

impl OrderManager {
    /// Create a new order manager with shared order tracker
    pub fn new(config: BaseConfig, client: OrderbookClient, tracker: Arc<Mutex<OrderTracker>>) -> Self {
        Self { config, client, tracker, last_grid_prices: HashMap::new(), balances: Vec::new(), tick_sizes: HashMap::new() }
    }

    /// Get tick size for a market, fetching from server if not cached
    async fn get_tick_size(&mut self, market_id: &str) -> f64 {
        if let Some(&ts) = self.tick_sizes.get(market_id) {
            return ts;
        }
        if let Ok(markets) = self.client.get_markets().await {
            for m in markets {
                if let Ok(ts) = m.tick_size.parse::<f64>() {
                    if ts > 0.0 {
                        self.tick_sizes.insert(m.market_id.clone(), ts);
                    }
                }
            }
        }
        self.tick_sizes.get(market_id).copied().unwrap_or(0.0000000100)
    }

    /// Update cached balances (called before update_cycle)
    pub fn set_balances(&mut self, balances: Vec<TokenBalance>) {
        self.balances = balances;
    }

    /// Place a bid order (signed and tracked)
    pub async fn place_bid(
        &mut self,
        market_id: &str,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
    ) -> Result<u64> {
        debug!("Placing bid: {} {} @ {}", quantity, market_id, price);

        // Sign the order
        let (signature, signed_data, nonce) = {
            let tracker = self.tracker.lock().await;
            tracker.sign_order(market_id, "bid", price, quantity)
        };

        let response = self.client.submit_order(
            market_id,
            OrderType::Bid,
            price.to_string(),
            quantity.to_string(),
            order_ref,
            Some(signature.clone()),
            signed_data.clone(),
            nonce,
        ).await?;

        if response.success {
            let order_id = response.order.as_ref().map(|o| o.order_id).unwrap_or(0);
            debug!("Bid placed: order_id={}", order_id);

            // Track the order
            let mut tracker = self.tracker.lock().await;
            tracker.track_order(
                order_id, market_id, OrderType::Bid as i32,
                price, quantity, nonce, &signature, &signed_data,
            );

            Ok(order_id)
        } else {
            anyhow::bail!("Failed to place bid: {}", response.message)
        }
    }

    /// Place an offer order (signed and tracked)
    pub async fn place_offer(
        &mut self,
        market_id: &str,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
    ) -> Result<u64> {
        debug!("Placing offer: {} {} @ {}", quantity, market_id, price);

        // Sign the order
        let (signature, signed_data, nonce) = {
            let tracker = self.tracker.lock().await;
            tracker.sign_order(market_id, "offer", price, quantity)
        };

        let response = self.client.submit_order(
            market_id,
            OrderType::Offer,
            price.to_string(),
            quantity.to_string(),
            order_ref,
            Some(signature.clone()),
            signed_data.clone(),
            nonce,
        ).await?;

        if response.success {
            let order_id = response.order.as_ref().map(|o| o.order_id).unwrap_or(0);
            debug!("Offer placed: order_id={}", order_id);

            // Track the order
            let mut tracker = self.tracker.lock().await;
            tracker.track_order(
                order_id, market_id, OrderType::Offer as i32,
                price, quantity, nonce, &signature, &signed_data,
            );

            Ok(order_id)
        } else {
            anyhow::bail!("Failed to place offer: {}", response.message)
        }
    }

    /// Cancel an order
    pub async fn cancel_order(&mut self, order_id: u64) -> Result<()> {
        debug!("Cancelling order: {}", order_id);

        let response = self.client.cancel_order(order_id).await?;

        if response.success {
            debug!("Order cancelled: {}", order_id);
            let mut tracker = self.tracker.lock().await;
            tracker.cancel_order(order_id);
            Ok(())
        } else {
            anyhow::bail!("Failed to cancel order {}: {}", order_id, response.message)
        }
    }

    /// Get active orders for a market
    pub async fn get_active_orders(&mut self, market_id: &str) -> Result<Vec<Order>> {
        self.client.get_active_orders(market_id).await
    }

    /// Cancel all orders for a market
    pub async fn cancel_all_orders(&mut self, market_id: &str) -> Result<()> {
        let orders = self.get_active_orders(market_id).await?;

        for order in orders {
            if let Err(e) = self.cancel_order(order.order_id).await {
                warn!("Failed to cancel order {}: {}", order.order_id, e);
            }
        }

        Ok(())
    }

    /// Cancel all orders for all configured markets
    pub async fn cancel_all_market_orders(&mut self) -> Result<()> {
        // Collect market IDs first to avoid borrow issues
        let market_ids: Vec<String> = self.config.markets.iter()
            .filter(|m| m.enabled)
            .map(|m| m.market_id.clone())
            .collect();

        for market_id in market_ids {
            if let Err(e) = self.cancel_all_orders(&market_id).await {
                warn!("Failed to cancel orders for {}: {}", market_id, e);
            }
        }
        Ok(())
    }

    /// Get current price for a market
    pub async fn get_price(&mut self, market_id: &str) -> Result<f64> {
        let response = self.client.get_price(market_id).await?;
        Ok(response.last)
    }

    /// Place grid orders for a market based on config.
    ///
    /// `place_bids` / `place_offers` control which sides are placed, allowing
    /// the agent to place a partial grid when only one side is affordable.
    pub async fn place_grid_orders(
        &mut self,
        market_config: &MarketConfig,
        place_bids: bool,
        place_offers: bool,
    ) -> Result<()> {
        let mid_price = self.get_price(&market_config.market_id).await?;
        let tick = self.get_tick_size(&market_config.market_id).await;
        let mut placed: Vec<String> = Vec::new();

        // Place bid orders
        if place_bids {
            for (i, level) in market_config.bid_levels.iter().enumerate() {
                let raw_price = mid_price * (1.0 + level.delta_percent / 100.0);
                let price = (raw_price / tick).floor() * tick;
                let price_str = format!("{:.10}", price);
                let order_ref = uuid::Uuid::now_v7().to_string();

                match self.place_bid(
                    &market_config.market_id,
                    &price_str,
                    &level.quantity,
                    Some(order_ref),
                ).await {
                    Ok(id) => placed.push(format!("bid #{}@{}", id, price_str)),
                    Err(e) => warn!("Failed to place bid at level {}: {}", i, e),
                }
            }
        }

        // Place offer orders
        if place_offers {
            for (i, level) in market_config.offer_levels.iter().enumerate() {
                let raw_price = mid_price * (1.0 + level.delta_percent / 100.0);
                let price = (raw_price / tick).ceil() * tick;
                let price_str = format!("{:.10}", price);
                let order_ref = uuid::Uuid::now_v7().to_string();

                match self.place_offer(
                    &market_config.market_id,
                    &price_str,
                    &level.quantity,
                    Some(order_ref),
                ).await {
                    Ok(id) => placed.push(format!("offer #{}@{}", id, price_str)),
                    Err(e) => warn!("Failed to place offer at level {}: {}", i, e),
                }
            }
        }

        if !placed.is_empty() {
            info!("Placed {} orders for {} (mid={}, tick={}): [{}]",
                placed.len(), market_config.market_id, mid_price, tick, placed.join(", "));
        }

        Ok(())
    }

    /// Check per-side balance sufficiency for placing the grid.
    ///
    /// Returns `GridAffordability` indicating which sides (bids, offers) the
    /// agent can afford.  When balance data is unavailable, both sides are
    /// assumed affordable.  Parses market_id "BASE-QUOTE" to determine which
    /// tokens are needed.
    fn check_grid_balance(&self, market_config: &MarketConfig, mid_price: f64) -> GridAffordability {
        if self.balances.is_empty() {
            return GridAffordability { can_bid: true, can_offer: true };
        }

        let parts: Vec<&str> = market_config.market_id.split('-').collect();
        if parts.len() != 2 {
            return GridAffordability { can_bid: true, can_offer: true };
        }
        let (base, quote) = (parts[0], parts[1]);
        let fee_reserve = self.config.fee_reserve_cc;

        // Find unlocked balances
        let cc_unlocked = self.find_unlocked_cc();
        let base_unlocked = if base == "CC" { cc_unlocked } else { self.find_unlocked_token(base) };
        let quote_unlocked = if quote == "CC" { cc_unlocked } else { self.find_unlocked_token(quote) };

        info!(
            "Balance check for {}: CC={:.4}, {}={:.4}, {}={:.4}, fee_reserve={:.2}",
            market_config.market_id, cc_unlocked, base, base_unlocked, quote, quote_unlocked, fee_reserve
        );

        // CC fee reserve is a prerequisite for either side
        if cc_unlocked < fee_reserve {
            warn!(
                "Insufficient CC for fees: {:.4} < {:.2} reserve",
                cc_unlocked, fee_reserve
            );
            return GridAffordability { can_bid: false, can_offer: false };
        }

        let mut can_bid = true;
        let mut can_offer = true;

        // Total quote needed for bids (buying base with quote)
        let total_bid_quote: f64 = market_config.bid_levels.iter()
            .map(|l| {
                let qty: f64 = l.quantity.parse().unwrap_or(0.0);
                let price = mid_price * (1.0 + l.delta_percent / 100.0);
                qty * price
            })
            .sum();

        // Total base needed for offers (selling base)
        let total_offer_base: f64 = market_config.offer_levels.iter()
            .map(|l| l.quantity.parse::<f64>().unwrap_or(0.0))
            .sum();

        // Available amounts after reserving CC for fees
        let available_quote = if quote == "CC" { quote_unlocked - fee_reserve } else { quote_unlocked };
        let available_base = if base == "CC" { base_unlocked - fee_reserve } else { base_unlocked };

        if total_bid_quote > 0.0 && available_quote < total_bid_quote {
            warn!(
                "Insufficient {} for bids: {:.8} available < {:.8} needed",
                quote, available_quote, total_bid_quote
            );
            can_bid = false;
        }

        if total_offer_base > 0.0 && available_base < total_offer_base {
            warn!(
                "Insufficient {} for offers: {:.8} available < {:.8} needed",
                base, available_base, total_offer_base
            );
            can_offer = false;
        }

        GridAffordability { can_bid, can_offer }
    }

    /// Find unlocked CC balance (is_canton_coin flag)
    fn find_unlocked_cc(&self) -> f64 {
        self.balances.iter()
            .find(|b| b.is_canton_coin)
            .and_then(|b| b.unlocked_amount.parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    /// Find unlocked balance for a token by instrument_id
    fn find_unlocked_token(&self, instrument_id: &str) -> f64 {
        self.balances.iter()
            .find(|b| b.instrument_id == instrument_id)
            .and_then(|b| b.unlocked_amount.parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    /// Check if any active orders have been partially filled.
    ///
    /// An order with `filled_quantity > 0` has been partially matched but is
    /// still on the book.  The agent should cancel and replace it at the full
    /// configured quantity so the grid always offers full-size orders.
    fn has_partial_fills(orders: &[Order]) -> bool {
        orders.iter().any(|o| {
            let filled: f64 = o.filled_quantity.parse().unwrap_or(0.0);
            filled > 0.0
        })
    }

    /// Refresh grid with interleaved cancel/place (tightest spread first).
    ///
    /// Instead of cancelling all orders then placing all new ones (which leaves
    /// the agent naked), this cancels one old order and immediately places its
    /// replacement before moving to the next level.  Orders are processed in
    /// order of increasing abs(delta_percent) so the tightest-spread orders
    /// (most likely to fill) are refreshed first.
    async fn refresh_grid(
        &mut self,
        market: &MarketConfig,
        current_price: f64,
        active_orders: &[Order],
    ) -> Result<()> {
        let market_id = &market.market_id;
        let affordability = self.check_grid_balance(market, current_price);
        if !affordability.can_bid && !affordability.can_offer {
            warn!("Insufficient balance for any side on {}, skipping", market_id);
            return Ok(());
        }

        if !affordability.can_bid || !affordability.can_offer {
            info!(
                "Market {} partial affordability: can_bid={}, can_offer={}",
                market_id, affordability.can_bid, affordability.can_offer
            );
        }

        // If no existing orders, just place the grid
        if active_orders.is_empty() {
            if let Err(e) = self.place_grid_orders(market, affordability.can_bid, affordability.can_offer).await {
                warn!("Failed to place grid for {}: {}", market_id, e);
            } else {
                self.last_grid_prices.insert(market_id.clone(), current_price);
            }
            return Ok(());
        }

        let tick = self.get_tick_size(market_id).await;
        let mid_price = current_price;
        info!("Refreshing grid for {} (cancel→place): mid={}, tick={}", market_id, mid_price, tick);

        // Build list of new orders: (abs_delta, side, price_str, quantity)
        // Sorted by abs(delta_percent) ascending so tightest-spread orders are replaced first
        let mut new_orders: Vec<(f64, &str, String, String)> = Vec::new();

        if affordability.can_bid {
            for level in &market.bid_levels {
                let raw = mid_price * (1.0 + level.delta_percent / 100.0);
                let price = (raw / tick).floor() * tick;
                new_orders.push((level.delta_percent.abs(), "bid", format!("{:.10}", price), level.quantity.clone()));
            }
        }
        if affordability.can_offer {
            for level in &market.offer_levels {
                let raw = mid_price * (1.0 + level.delta_percent / 100.0);
                let price = (raw / tick).ceil() * tick;
                new_orders.push((level.delta_percent.abs(), "offer", format!("{:.10}", price), level.quantity.clone()));
            }
        }

        new_orders.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Sort old orders by distance from mid price ascending (tightest first)
        let mut old_sorted: Vec<(f64, u64)> = active_orders.iter().map(|o| {
            let p: f64 = o.price.parse().unwrap_or(0.0);
            ((p - mid_price).abs(), o.order_id)
        }).collect();
        old_sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut old_ids: std::collections::VecDeque<u64> =
            old_sorted.into_iter().map(|(_, id)| id).collect();

        // Interleave: cancel one old, place one new (tightest spread first)
        let mut cancelled_ids: Vec<u64> = Vec::new();
        let mut placed: Vec<String> = Vec::new(); // "bid #id@price" or "offer #id@price"

        for (_delta, side, price, qty) in &new_orders {
            if let Some(old_id) = old_ids.pop_front() {
                if let Err(e) = self.cancel_order(old_id).await {
                    warn!("Failed to cancel order {}: {}", old_id, e);
                } else {
                    cancelled_ids.push(old_id);
                }
            }
            let order_ref = Some(uuid::Uuid::now_v7().to_string());
            let result = match *side {
                "bid" => self.place_bid(market_id, price, qty, order_ref).await,
                _ => self.place_offer(market_id, price, qty, order_ref).await,
            };
            match result {
                Ok(id) => placed.push(format!("{} #{}@{}", side, id, price)),
                Err(e) => warn!("Failed to place {} at {}: {}", side, price, e),
            }
        }

        // Cancel any leftover old orders
        for old_id in &old_ids {
            if let Err(e) = self.cancel_order(*old_id).await {
                warn!("Failed to cancel leftover order {}: {}", old_id, e);
            } else {
                cancelled_ids.push(*old_id);
            }
        }

        info!(
            "Grid refreshed for {}: cancelled [{}], placed [{}]",
            market_id,
            cancelled_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "),
            placed.join(", "),
        );

        self.last_grid_prices.insert(market_id.clone(), current_price);
        Ok(())
    }

    /// Update cycle — fetch price, detect partial fills, refresh on threshold.
    ///
    /// The grid is refreshed when:
    /// 1. Active order count is below expected (order fully filled / missing)
    /// 2. Any order is partially filled (cancel + replace at full quantity)
    /// 3. Price moved beyond the configured threshold
    ///
    /// On refresh, only the side(s) with sufficient balance are placed.
    pub async fn update_cycle(&mut self) -> Result<()> {
        let markets: Vec<MarketConfig> = self.config.enabled_markets()
            .into_iter().cloned().collect();

        for market in &markets {
            let market_id = &market.market_id;

            // 1. Fetch current price
            let current_price = match self.get_price(market_id).await {
                Ok(p) => {
                    debug!("Market {} price: {}", market_id, p);
                    p
                }
                Err(e) => {
                    warn!("Failed to get price for {}: {}", market_id, e);
                    continue;
                }
            };

            // 2. Fetch active orders
            let orders = self.get_active_orders(market_id).await?;
            let expected_count = market.bid_levels.len() + market.offer_levels.len();
            let partial_fills = Self::has_partial_fills(&orders);

            // 3. Determine if grid needs refresh
            if orders.len() < expected_count || partial_fills {
                info!(
                    "Market {} needs refresh: {}/{} active, partial_fills={}",
                    market_id, orders.len(), expected_count, partial_fills
                );
                self.refresh_grid(market, current_price, &orders).await?;
            } else if let Some(&last_price) = self.last_grid_prices.get(market_id) {
                // 4. Check if price moved beyond threshold
                let change_pct = ((current_price - last_price).abs() / last_price) * 100.0;
                if change_pct >= market.price_change_threshold_percent {
                    info!(
                        "Price moved {:.2}% (threshold {:.2}%), refreshing grid for {}",
                        change_pct, market.price_change_threshold_percent, market_id
                    );
                    self.refresh_grid(market, current_price, &orders).await?;
                }
            } else {
                // Orders exist but no tracked grid price (e.g. from before restart)
                self.last_grid_prices.insert(market_id.clone(), current_price);
            }
        }
        Ok(())
    }
}
