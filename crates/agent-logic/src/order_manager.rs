//! Order management for the orderbook agent
//!
//! Handles order placement and cancellation via the orderbook service.
//! All orders are signed and tracked for settlement verification.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use orderbook_proto::orderbook::{Order, OrderType};
use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::{BaseConfig, MarketConfig, PriceLevel};
use crate::net_position::NetPositionTracker;
use crate::order_tracker::OrderTracker;
use crate::pool_impact::{self, ImpactSide, PoolDepth};

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
    /// Markets currently without a reliable server price. Tracks the
    /// transition so the cancel + warn happen once, not every 5s cycle.
    priceless: HashSet<String>,
    /// Consecutive failed price fetches per market — the grid is only torn
    /// down after 2 strikes, so a single blip (server negative-cache window,
    /// one flaky upstream response) doesn't churn N cancels + N re-places.
    price_fail_streak: HashMap<String, u32>,
    /// Consecutive successful fetches while a market is priceless. Resuming
    /// also takes 2 strikes, so a flapping feed does not re-grid each time.
    price_restore_streak: HashMap<String, u32>,
    /// Trailing net tracker for grid shaping: resting rungs are anonymous, so
    /// the desk-level net shapes them. None = unchanged grid behaviour.
    net_positions: Option<Arc<NetPositionTracker>>,
    /// Last pool depth seen per market — rides the same `get_price` response
    /// the grid already fetches every cycle, so shaping needs no extra RPC.
    pool_depths: HashMap<String, PoolDepth>,
    /// Shaped offer set at the last placement, so a desk-net move can act as
    /// a refresh trigger.
    last_shaped_offers: HashMap<String, Vec<PriceLevel>>,
}

/// Shaped rungs below this fraction of their configured size are dropped, not
/// placed. Overridable via `pool_impact.grid_min_rung_base`.
const DEFAULT_MIN_RUNG_FRACTION: f64 = 0.10;

/// Materiality thresholds for the shape-driven refresh, set far above ordinary
/// decay drift so the book is not re-placed every cycle.
const SHAPE_REFRESH_QTY_FRACTION: f64 = 0.005;
const SHAPE_REFRESH_DELTA_PCT: f64 = 0.01;

impl OrderManager {
    /// Create a new order manager with shared order tracker
    pub fn new(config: BaseConfig, client: OrderbookClient, tracker: Arc<Mutex<OrderTracker>>) -> Self {
        Self {
            config,
            client,
            tracker,
            last_grid_prices: HashMap::new(),
            balances: Vec::new(),
            tick_sizes: HashMap::new(),
            priceless: HashSet::new(),
            price_fail_streak: HashMap::new(),
            price_restore_streak: HashMap::new(),
            net_positions: None,
            pool_depths: HashMap::new(),
            last_shaped_offers: HashMap::new(),
        }
    }

    /// Wire the trailing net-position tracker for desk-net offer shaping.
    pub fn set_net_positions(&mut self, tracker: Arc<NetPositionTracker>) {
        self.net_positions = Some(tracker);
    }

    /// A market lost its price: cancel every resting order (stale levels must
    /// not stay executable) and clear the grid anchor. Idempotent — repeat
    /// cycles find no active orders and only debug-log.
    async fn handle_priceless(&mut self, market_id: &str, reason: &str) {
        if self.priceless.insert(market_id.to_string()) {
            warn!(
                "Market {} has NO reliable price ({}); cancelling all orders and pausing quoting",
                market_id, reason
            );
        } else {
            debug!("Market {} still priceless ({})", market_id, reason);
        }
        self.last_grid_prices.remove(market_id);
        // Depth came with the (now-gone) price — drop it with the grid.
        self.pool_depths.remove(market_id);
        if let Err(e) = self.cancel_all_orders(market_id).await {
            warn!("Failed to cancel orders for priceless market {}: {}", market_id, e);
        }
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

    /// Update cached balances (called before update_cycle).
    ///
    /// Ledger balances carry ON-CHAIN wire ids; market configs and the grid
    /// affordability checks use the orderbook-internal ids (issuer-minted
    /// instruments can carry an opaque UUID on-chain). Normalize once on the
    /// way in so `find_unlocked_token(<internal id>)` matches. No-op for
    /// legacy tokens, whose two ids coincide.
    pub fn set_balances(&mut self, mut balances: Vec<TokenBalance>) {
        for b in &mut balances {
            if let Some(internal) = self.config.internal_id_for_wire(&b.instrument_id) {
                b.instrument_id = internal;
            }
        }
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

    /// Current price for a market. Any size reference on the same response is
    /// cached for grid shaping; absence clears it and means no adjustment.
    pub async fn get_price(&mut self, market_id: &str) -> Result<f64> {
        let response = self.client.get_price(market_id).await?;
        match response.pool_depth.as_ref().and_then(PoolDepth::from_proto) {
            Some(d) => {
                self.pool_depths.insert(market_id.to_string(), d);
            }
            None => {
                self.pool_depths.remove(market_id);
            }
        }
        Ok(response.last)
    }

    /// Whether the shaped offer set differs MATERIALLY from what is resting.
    /// Exact comparison would churn every cycle, since the net decays.
    fn shaped_offers_changed(&self, market: &MarketConfig) -> bool {
        let now = self.shaped_offer_levels(market, false);
        match self.last_shaped_offers.get(&market.market_id) {
            None => false, // nothing placed yet; placement will record it
            Some(prev) => {
                if prev.len() != now.len() {
                    return true; // a rung appeared or was dropped
                }
                shaped_sets_differ_materially(prev, &now)
            }
        }
    }

    /// Scale offer rung quantities and widen their deltas from the desk net.
    /// Bids untouched; levels returned verbatim when shaping is unavailable.
    fn shaped_offer_levels(&self, market: &MarketConfig, log: bool) -> Vec<PriceLevel> {
        let raw = market.offer_levels.clone();
        let (Some(cfg), Some(depth), Some(tracker)) = (
            market.rfq.as_ref().and_then(|r| r.pool_impact.as_ref()),
            self.pool_depths.get(&market.market_id),
            self.net_positions.as_ref(),
        ) else {
            return raw;
        };
        let base_token = market.market_id.split('-').next().unwrap_or("");
        let desk = tracker.desk_net(base_token);
        let shaped = shape_offer_levels(&raw, cfg, depth.base_reserve, desk);
        if log {
            shaped.log(&market.market_id, cfg.enabled, desk, depth.base_reserve);
        }
        shaped.levels
    }

    /// Place grid orders for a market based on config.
    ///
    /// `place_bids` / `place_offers` control which sides are placed, allowing
    /// the agent to place a partial grid when only one side is affordable.
    pub async fn place_grid_orders(
        &mut self,
        market_config: &MarketConfig,
        mid_price: f64,
        place_bids: bool,
        place_offers: bool,
    ) -> Result<()> {
        // The price comes from the caller's already-validated fetch — no
        // re-fetch here (a second fetch could race the no-price transition
        // and grid off 0/NaN).
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

        // Place offer orders — desk-net/depth-shaped (raw when no shaping
        // context; see shaped_offer_levels).
        if place_offers {
            let offer_levels = self.shaped_offer_levels(market_config, true);
            // Baseline for the shape-change refresh trigger.
            self.last_shaped_offers
                .insert(market_config.market_id.clone(), offer_levels.clone());
            for (i, level) in offer_levels.iter().enumerate() {
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
    /// tokens are needed. `offer_levels` is the (possibly shaped) offer set
    /// the caller is about to place, so affordability matches reality.
    fn check_grid_balance(
        &self,
        market_config: &MarketConfig,
        mid_price: f64,
        offer_levels: &[PriceLevel],
    ) -> GridAffordability {
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

        debug!(
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
        let total_offer_base: f64 = offer_levels
            .iter()
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
        // One shaped-offer computation per refresh: affordability, placement
        // and the interleaved replacement below all see the same rungs.
        let offer_levels = self.shaped_offer_levels(market, true);
        self.last_shaped_offers
            .insert(market.market_id.clone(), offer_levels.clone());
        let affordability = self.check_grid_balance(market, current_price, &offer_levels);
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
            if let Err(e) = self.place_grid_orders(market, current_price, affordability.can_bid, affordability.can_offer).await {
                warn!("Failed to place grid for {}: {}", market_id, e);
            } else {
                self.last_grid_prices.insert(market_id.clone(), current_price);
            }
            return Ok(());
        }

        let tick = self.get_tick_size(market_id).await;
        let mid_price = current_price;
        debug!("Refreshing grid for {} (cancel→place): mid={}, tick={}", market_id, mid_price, tick);

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
            for level in &offer_levels {
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

        debug!(
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

            // 1. Fetch price. NO PRICE = NO QUOTES, so cancel and pause
            // rather than rest at stale levels. Two-strike debounce.
            let (current_price, restored) = match self.get_price(market_id).await {
                Ok(p) if p.is_finite() && p > 0.0 => {
                    self.price_fail_streak.remove(market_id);
                    if self.priceless.contains(market_id) {
                        // Restore-side two-strike (mirror of the teardown
                        // debounce): a feed oscillating around its timeout
                        // must not re-grid on every lucky fetch — require two
                        // consecutive successes before resuming (+5s latency).
                        let streak =
                            self.price_restore_streak.entry(market_id.clone()).or_insert(0);
                        *streak += 1;
                        if *streak < 2 {
                            debug!(
                                "Market {} price back ({}); awaiting a second consecutive \
                                 success before resuming",
                                market_id, p
                            );
                            continue;
                        }
                        self.price_restore_streak.remove(market_id);
                        self.priceless.remove(market_id);
                        info!("Market {} price restored ({}); resuming quoting", market_id, p);
                        (p, true)
                    } else {
                        debug!("Market {} price: {}", market_id, p);
                        (p, false)
                    }
                }
                other => {
                    let reason = match other {
                        Ok(p) => format!("non-positive price {p}"),
                        Err(e) => e.to_string(),
                    };
                    self.price_restore_streak.remove(market_id);
                    let streak = self.price_fail_streak.entry(market_id.clone()).or_insert(0);
                    *streak += 1;
                    if *streak >= 2 || self.priceless.contains(market_id) {
                        self.handle_priceless(market_id, &reason).await;
                    } else {
                        warn!(
                            "Market {} price fetch failed ({}); grid unchanged, will tear down \
                             on a second consecutive failure",
                            market_id, reason
                        );
                    }
                    continue;
                }
            };

            // 2. Fetch active orders. Per-market failure must not abort the
            // cycle — a `?` here let one broken market (e.g. deactivated
            // server-side, GetOrders → not_found) silently freeze grid
            // maintenance for every market ordered after it, every cycle.
            let orders = match self.get_active_orders(market_id).await {
                Ok(o) => o,
                Err(e) => {
                    warn!(
                        "Market {} order fetch failed ({}); skipping this market this cycle",
                        market_id, e
                    );
                    continue;
                }
            };
            // Count against the SHAPED set, or an omitted rung reads as
            // "order missing" and churns a refresh.
            let expected_count =
                market.bid_levels.len() + self.shaped_offer_levels(market, false).len();
            let partial_fills = Self::has_partial_fills(&orders);

            // 3. Refresh? A restore always refreshes, since cancels may have
            // failed during the outage. The shaped set needs its own trigger.
            let shape_changed = self.shaped_offers_changed(market);

            if restored || orders.len() < expected_count || partial_fills || shape_changed {
                info!(
                    "Market {} needs refresh: {}/{} active, partial_fills={}, shape_changed={}",
                    market_id, orders.len(), expected_count, partial_fills, shape_changed
                );
                self.refresh_grid(market, current_price, &orders).await?;
            } else if let Some(&last_price) = self.last_grid_prices.get(market_id) {
                // 4. Check if price moved beyond threshold
                let change_pct = ((current_price - last_price).abs() / last_price) * 100.0;
                if change_pct >= market.price_change_threshold_percent {
                    debug!(
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

/// Outcome of shaping one market's offer ladder (pure — see [`shape_offer_levels`]).
pub struct ShapedOffers {
    pub levels: Vec<PriceLevel>,
    /// Quantity scale factor applied (1.0 = untouched).
    pub factor: f64,
    /// Largest marginal impact added to any rung's delta, percent.
    pub max_impact: f64,
    /// Rungs dropped for landing below the dust floor.
    pub dropped: usize,
}

impl ShapedOffers {
    fn log(&self, market_id: &str, enabled: bool, desk: f64, reserve: f64) {
        if self.factor >= 1.0 && self.max_impact <= 0.0 {
            return;
        }
        if enabled {
            info!(
                "Grid impact {}: offers x{:.3}, delta +{:.3}% max, {} rung(s) dropped (desk_net={:.0}, R={:.0})",
                market_id, self.factor, self.max_impact, self.dropped, desk, reserve
            );
        } else {
            info!(
                "SHADOW pool impact (grid) {}: would scale offers x{:.3} and raise deltas up to {:.3}% (desk_net={:.0}, R={:.0}) — NOT applied",
                market_id, self.factor, self.max_impact, desk, reserve
            );
        }
    }
}

/// Shape an offer ladder against the desk's trailing net. Pure, so the
/// byte-identical and dust-drop guarantees can be tested without a client.
pub fn shape_offer_levels(
    raw: &[PriceLevel],
    cfg: &crate::config::PoolImpactConfig,
    base_reserve: f64,
    desk_net: f64,
) -> ShapedOffers {
    let fz = cfg.grid_free_zone_base.unwrap_or(cfg.free_zone_base).max(0.0);
    let excess = (desk_net - fz).max(0.0);
    let span = cfg
        .grid_offer_scale_base
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(cfg.max_pool_fraction.clamp(0.01, 0.99) * base_reserve);
    let factor = if span > 0.0 { (1.0 - excess / span).clamp(0.0, 1.0) } else { 1.0 };

    let mut levels = Vec::with_capacity(raw.len());
    let mut max_impact = 0.0f64;
    let mut dropped = 0usize;
    for level in raw {
        let qty: f64 = level.quantity.parse().unwrap_or(0.0);
        let scaled = qty * factor;
        let impact = pool_impact::marginal_impact_percent(
            ImpactSide::UserBuys,
            scaled.max(0.0),
            desk_net,
            base_reserve,
            fz,
            cfg.max_pool_fraction,
            cfg.impact_multiplier,
            cfg.max_impact_percent,
        );
        max_impact = max_impact.max(impact);
        if !cfg.enabled {
            levels.push(level.clone()); // SHADOW: raw rung
            continue;
        }
        if factor >= 1.0 && impact <= 0.0 {
            levels.push(level.clone()); // no-op: keep the ORIGINAL strings
            continue;
        }
        let min_rung = cfg
            .grid_min_rung_base
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(qty * DEFAULT_MIN_RUNG_FRACTION);
        if !(scaled.is_finite() && scaled >= min_rung) {
            dropped += 1;
            continue;
        }
        levels.push(PriceLevel {
            delta_percent: level.delta_percent + impact,
            quantity: format!("{scaled:.10}"),
        });
    }
    ShapedOffers { levels, factor, max_impact, dropped }
}

/// Whether two shaped offer sets differ MATERIALLY (see the thresholds).
/// Pure, so the churn regression is testable without a client.
pub fn shaped_sets_differ_materially(prev: &[PriceLevel], now: &[PriceLevel]) -> bool {
    if prev.len() != now.len() {
        return true; // a rung appeared or was dropped
    }
    prev.iter().zip(now.iter()).any(|(a, b)| {
        let qa: f64 = a.quantity.parse().unwrap_or(0.0);
        let qb: f64 = b.quantity.parse().unwrap_or(0.0);
        let qty_moved = if qa > 0.0 {
            ((qb - qa).abs() / qa) > SHAPE_REFRESH_QTY_FRACTION
        } else {
            qa != qb
        };
        qty_moved || (a.delta_percent - b.delta_percent).abs() > SHAPE_REFRESH_DELTA_PCT
    })
}

#[cfg(test)]
mod grid_shaping_tests {
    use super::*;
    use crate::config::PoolImpactConfig;

    const R: f64 = 45_500_000.0;

    fn ladder() -> Vec<PriceLevel> {
        vec![
            PriceLevel { delta_percent: -1.95, quantity: "12000".to_string() },
            PriceLevel { delta_percent: -1.95, quantity: "12000".to_string() },
            PriceLevel { delta_percent: -1.95, quantity: "12000".to_string() },
        ]
    }

    fn cfg(enabled: bool) -> PoolImpactConfig {
        PoolImpactConfig { enabled, free_zone_base: 100_000.0, ..Default::default() }
    }


    /// THE churn regression: the net decays continuously, so an exact
    /// comparison re-places the whole grid every cycle.
    #[test]
    fn decay_drift_between_cycles_does_not_trigger_a_refresh() {
        let mut c = cfg(true);
        c.grid_free_zone_base = Some(0.0);
        let desk0 = 8_000_000.0;
        // One 5s grid cycle of exponential decay on a 24h window.
        let desk1 = desk0 * (-5.0f64 / (24.0 * 3600.0)).exp();
        let a = shape_offer_levels(&ladder(), &c, R, desk0).levels;
        let b = shape_offer_levels(&ladder(), &c, R, desk1).levels;
        assert!(a[0].quantity != b[0].quantity, "precondition: the raw strings DO differ");
        assert!(
            !shaped_sets_differ_materially(&a, &b),
            "one cycle of desk decay triggered a full grid refresh: {} -> {}",
            a[0].quantity, b[0].quantity
        );
    }

    /// ...but a real desk-net move must still refresh the book, or the trigger
    /// is pointless.
    #[test]
    fn a_material_desk_move_does_trigger_a_refresh() {
        let mut c = cfg(true);
        c.grid_free_zone_base = Some(0.0);
        let a = shape_offer_levels(&ladder(), &c, R, 8_000_000.0).levels;
        let b = shape_offer_levels(&ladder(), &c, R, 9_000_000.0).levels;
        assert!(
            shaped_sets_differ_materially(&a, &b),
            "a 1M desk-net move must re-place the book"
        );
    }

    /// A rung being dropped or restored is always material.
    #[test]
    fn rung_count_change_always_triggers() {
        let a = shape_offer_levels(&ladder(), &cfg(false), R, 0.0).levels;
        let b: Vec<PriceLevel> = a.iter().skip(1).cloned().collect();
        assert!(shaped_sets_differ_materially(&a, &b));
    }

    /// Shadow mode must be BYTE-IDENTICAL to the configured ladder — the whole
    /// point of shipping disabled is that nothing moves.
    #[test]
    fn shadow_mode_is_byte_identical() {
        let raw = ladder();
        let out = shape_offer_levels(&raw, &cfg(false), R, 5_000_000.0);
        assert_eq!(out.levels.len(), raw.len());
        for (a, b) in raw.iter().zip(out.levels.iter()) {
            assert_eq!(a.quantity, b.quantity, "quantity string must not be reformatted");
            assert_eq!(a.delta_percent, b.delta_percent);
        }
        assert_eq!(out.dropped, 0);
    }

    /// Enabled but with the desk inside the free zone: also byte-identical, so
    /// ordinary operation never reformats or nudges a rung.
    #[test]
    fn enabled_but_inside_the_free_zone_is_byte_identical() {
        let raw = ladder();
        let out = shape_offer_levels(&raw, &cfg(true), R, 0.0);
        for (a, b) in raw.iter().zip(out.levels.iter()) {
            assert_eq!(a.quantity, b.quantity);
            assert_eq!(a.delta_percent, b.delta_percent);
        }
    }

    /// A rung scaled into dust must be DROPPED: the server would reject it
    /// while it still counted toward expected_count.
    #[test]
    fn dust_rungs_are_dropped_not_placed() {
        let raw = ladder();
        let mut c = cfg(true);
        c.grid_free_zone_base = Some(0.0);
        // Desk net just under the span, so factor is tiny but > 0.
        let span = c.max_pool_fraction * R;
        let out = shape_offer_levels(&raw, &c, R, span * 0.999);
        assert!(out.factor > 0.0 && out.factor < 0.01, "factor {}", out.factor);
        assert_eq!(out.levels.len(), 0, "dust rungs must be omitted");
        assert_eq!(out.dropped, raw.len());
        // And every surviving rung is always >= its floor.
        for lvl in &out.levels {
            let q: f64 = lvl.quantity.parse().unwrap();
            assert!(q >= 12_000.0 * DEFAULT_MIN_RUNG_FRACTION);
        }
    }

    /// Past the free zone the offers shrink and widen — never the reverse.
    #[test]
    fn shaping_only_shrinks_and_widens_offers() {
        let raw = ladder();
        let mut c = cfg(true);
        c.grid_free_zone_base = Some(0.0);
        let out = shape_offer_levels(&raw, &c, R, 2_000_000.0);
        assert!(out.factor < 1.0, "offers must shrink as desk net grows");
        for (a, b) in raw.iter().zip(out.levels.iter()) {
            let qa: f64 = a.quantity.parse().unwrap();
            let qb: f64 = b.quantity.parse().unwrap();
            assert!(qb <= qa, "rung grew: {qb} > {qa}");
            assert!(b.delta_percent >= a.delta_percent, "offer moved toward the taker");
        }
    }

    #[test]
    fn absurd_depth_cannot_produce_nan_or_negative_rungs() {
        let raw = ladder();
        let mut c = cfg(true);
        c.grid_free_zone_base = Some(0.0);
        for reserve in [0.0, f64::NAN, -1.0, 1.0] {
            let out = shape_offer_levels(&raw, &c, reserve, 1_000.0);
            for lvl in &out.levels {
                let q: f64 = lvl.quantity.parse().unwrap();
                assert!(q.is_finite() && q > 0.0, "bad rung {q} at reserve {reserve}");
                assert!(lvl.delta_percent.is_finite());
            }
        }
    }
}
