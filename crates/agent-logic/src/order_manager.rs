//! Order management for the orderbook agent
//!
//! Handles order placement and cancellation via the orderbook service.
//! All orders are signed and tracked for settlement verification.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use orderbook_proto::orderbook::{Order, OrderType, SubmitOrderResponse};
use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::{BaseConfig, MarketConfig, PriceLevel};
use crate::net_position::NetPositionTracker;
use crate::order_tracker::{FailedSubmit, OrderTracker, PlacementGuard};
use crate::pool_impact::{self, ImpactSide, PoolDepth};
use crate::shutdown::Shutdown;

/// Whether one grid side can be funded from the current balances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Funding {
    Funded,
    Short,
    /// Balance or price data not good enough to judge; the side is left as is
    Unknown,
}

/// Why both sides of a market read `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownWhy {
    Aged,
    FeeReserve,
}

/// Per-side funding of a market's grid and the amounts behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GridFunding {
    pub bid: Funding,
    pub offer: Funding,
    pub why: Option<UnknownWhy>,
    pub cc_avail: f64,
    pub bid_need: f64,
    pub bid_avail: f64,
    pub offer_need: f64,
    pub offer_avail: f64,
}

/// Resting orders per side, and whether any on that side is partially filled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SideCounts {
    pub bids: usize,
    pub offers: usize,
    pub bid_partial: bool,
    pub offer_partial: bool,
}

impl SideCounts {
    fn any_resting(&self) -> bool {
        self.bids > 0 || self.offers > 0
    }
}

/// What a grid cycle does to one side of a market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideAction {
    Keep,
    Replace,
    Cancel,
}

/// Inputs to [`plan_market`]; `force_*` is a restore or a price move past that side's anchor.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlanInput {
    pub bid: Funding,
    pub offer: Funding,
    pub bid_expected: usize,
    pub offer_expected: usize,
    pub resting: SideCounts,
    pub force_bid: bool,
    pub force_offer: bool,
    pub shape_changed: bool,
}

/// Price each grid side was last placed at; a move past the threshold refreshes that side.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SideAnchors {
    pub bid: Option<f64>,
    pub offer: Option<f64>,
}

/// One RPC of a grid refresh, in execution order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GridOp {
    /// `leftover`: an old order with no new rung to pair with
    Cancel { order_id: u64, leftover: bool },
    Place { side: OrderType, price: String, quantity: String },
}

/// What one grid cycle did; `deferred` markets resume on the next cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CycleReport {
    pub markets: u32,
    pub no_levels: u32,
    pub parked: u32,
    pub held_unknown: u32,
    pub refreshed: u32,
    pub placed: u32,
    pub cancelled: u32,
    pub deferred: u32,
}

/// A parked market (no fundable side, nothing resting) is re-probed this often.
pub(crate) const UNFUNDED_PROBE: Duration = Duration::from_secs(60);

/// Rate limit for the repeated balance warnings.
const FUNDING_WARN_EVERY: Duration = Duration::from_secs(60);

/// Whether a balance snapshot taken at `set_at` may still size the grid.
/// `stale_after_secs == 0` disables the age check (a snapshot is still
/// required). Pure so it can be unit-tested.
fn balances_usable(
    set_at: Option<std::time::Instant>,
    stale_after_secs: u64,
    now: std::time::Instant,
) -> bool {
    match set_at {
        None => false,
        Some(_) if stale_after_secs == 0 => true,
        Some(t) => now.saturating_duration_since(t).as_secs() < stale_after_secs,
    }
}

/// Order manager handles order placement and tracking
pub struct OrderManager {
    config: BaseConfig,
    client: OrderbookClient,
    tracker: Arc<Mutex<OrderTracker>>,
    grid_anchors: HashMap<String, SideAnchors>,
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
    /// When `balances` was last replaced; grid sizing refuses aged data.
    balances_set_at: Option<Instant>,
    /// Rate limit for the aged-balances warning.
    balance_age_warned_at: Option<Instant>,
    /// Rate limit for the CC fee-reserve warning.
    fee_warned_at: Option<Instant>,
    /// Last valid price per market, so the park check can size bids without an RPC.
    last_seen_price: HashMap<String, f64>,
    /// Resting counts from the last visit that took no action, or that emptied a
    /// market without rungs; cleared by any other action.
    last_resting: HashMap<String, SideCounts>,
    /// When each market was last probed with a price fetch.
    last_probe: HashMap<String, Instant>,
    /// Markets skipped without RPCs: no fundable side and nothing resting.
    parked: HashSet<String>,
    /// (market, side) pairs paused for lack of balance, for transition logs.
    short_sides: HashSet<(String, &'static str)>,
    /// Markets whose price came back; kept until a visit plans them.
    force_refresh: HashSet<String>,
    /// Market index a cycle cut short by its deadline resumes from.
    resume_from: usize,
    /// Orders whose cancel failed, per market; retried while still listed.
    pending_cancels: HashMap<String, HashSet<u64>>,
    #[cfg(test)]
    stub_price: Option<f64>,
    #[cfg(test)]
    stub_orders: Option<Vec<Order>>,
    #[cfg(test)]
    stub_cancels: Option<Vec<u64>>,
    #[cfg(test)]
    stub_submit: Option<std::result::Result<u64, tonic::Status>>,
    #[cfg(test)]
    stop_on_cancel: Option<Shutdown>,
    #[cfg(test)]
    stop_on_price: Option<Shutdown>,
}

/// Shaped rungs below this fraction of their configured size are dropped, not
/// placed. Overridable via `pool_impact.grid_min_rung_base`.
const DEFAULT_MIN_RUNG_FRACTION: f64 = 0.10;

/// Materiality thresholds for the shape-driven refresh, set far above ordinary
/// decay drift so the book is not re-placed every cycle.
const SHAPE_REFRESH_QTY_FRACTION: f64 = 0.005;
const SHAPE_REFRESH_DELTA_PCT: f64 = 0.01;

/// Whether a failed submit may still have been booked: transport and unclassified
/// failures may have been; a definite refusal from the server was not.
pub(crate) fn submit_outcome_unknown(e: &anyhow::Error) -> bool {
    e.downcast_ref::<tonic::Status>().is_none_or(|s| {
        matches!(
            s.code(),
            tonic::Code::DeadlineExceeded
                | tonic::Code::Cancelled
                | tonic::Code::Unknown
                | tonic::Code::Internal
                | tonic::Code::Unavailable
        )
    })
}

impl OrderManager {
    /// Create a new order manager with shared order tracker
    pub fn new(config: BaseConfig, client: OrderbookClient, tracker: Arc<Mutex<OrderTracker>>) -> Self {
        Self {
            config,
            client,
            tracker,
            grid_anchors: HashMap::new(),
            balances: Vec::new(),
            tick_sizes: HashMap::new(),
            priceless: HashSet::new(),
            price_fail_streak: HashMap::new(),
            price_restore_streak: HashMap::new(),
            net_positions: None,
            pool_depths: HashMap::new(),
            last_shaped_offers: HashMap::new(),
            balances_set_at: None,
            balance_age_warned_at: None,
            fee_warned_at: None,
            last_seen_price: HashMap::new(),
            last_resting: HashMap::new(),
            last_probe: HashMap::new(),
            parked: HashSet::new(),
            short_sides: HashSet::new(),
            force_refresh: HashSet::new(),
            resume_from: 0,
            pending_cancels: HashMap::new(),
            #[cfg(test)]
            stub_price: None,
            #[cfg(test)]
            stub_orders: None,
            #[cfg(test)]
            stub_cancels: None,
            #[cfg(test)]
            stub_submit: None,
            #[cfg(test)]
            stop_on_cancel: None,
            #[cfg(test)]
            stop_on_price: None,
        }
    }

    /// Serve prices and order listings from these values instead of the server.
    #[cfg(test)]
    pub(crate) fn stub_book(&mut self, price: f64, orders: Vec<Order>) {
        self.stub_price = Some(price);
        self.stub_orders = Some(orders);
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
        self.grid_anchors.remove(market_id);
        self.last_resting.remove(market_id);
        self.pending_cancels.remove(market_id);
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

    /// Update cached balances. On-chain wire ids are normalized to the internal
    /// ids that grid funding looks up; a no-op where the two ids coincide.
    pub fn set_balances(&mut self, mut balances: Vec<TokenBalance>) {
        for b in &mut balances {
            if let Some(internal) = self.config.internal_id_for_wire(&b.instrument_id) {
                b.instrument_id = internal;
            }
        }
        self.balances = balances;
        self.balances_set_at = Some(Instant::now());
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

        // Sign the order; the guard holds matching proposals until it is tracked
        let (placement, (signature, signed_data, nonce)) = {
            let tracker = self.tracker.lock().await;
            let placement = tracker.begin_placement(market_id);
            (placement, tracker.sign_order(market_id, "bid", price, quantity)?)
        };

        let response = match self.submit_signed(
            market_id,
            OrderType::Bid,
            price,
            quantity,
            order_ref,
            &signature,
            &signed_data,
            nonce,
        ).await {
            Ok(response) => response,
            Err(e) => {
                if submit_outcome_unknown(&e) {
                    let submit = FailedSubmit {
                        market_id: market_id.to_string(),
                        order_type: OrderType::Bid as i32,
                        price: price.to_string(),
                        quantity: quantity.to_string(),
                        nonce,
                        signature,
                        signed_data,
                    };
                    self.note_submit_failed(submit, placement).await;
                } else {
                    drop(placement);
                }
                return Err(e);
            }
        };

        if response.success {
            let order_id = response.order.as_ref().map(|o| o.order_id).unwrap_or(0);
            debug!("Bid placed: order_id={}", order_id);

            // Track the order
            let mut tracker = self.tracker.lock().await;
            tracker.track_order(
                order_id, market_id, OrderType::Bid as i32,
                price, quantity, nonce, &signature, &signed_data,
            );
            drop(placement);

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

        // Sign the order; the guard holds matching proposals until it is tracked
        let (placement, (signature, signed_data, nonce)) = {
            let tracker = self.tracker.lock().await;
            let placement = tracker.begin_placement(market_id);
            (placement, tracker.sign_order(market_id, "offer", price, quantity)?)
        };

        let response = match self.submit_signed(
            market_id,
            OrderType::Offer,
            price,
            quantity,
            order_ref,
            &signature,
            &signed_data,
            nonce,
        ).await {
            Ok(response) => response,
            Err(e) => {
                if submit_outcome_unknown(&e) {
                    let submit = FailedSubmit {
                        market_id: market_id.to_string(),
                        order_type: OrderType::Offer as i32,
                        price: price.to_string(),
                        quantity: quantity.to_string(),
                        nonce,
                        signature,
                        signed_data,
                    };
                    self.note_submit_failed(submit, placement).await;
                } else {
                    drop(placement);
                }
                return Err(e);
            }
        };

        if response.success {
            let order_id = response.order.as_ref().map(|o| o.order_id).unwrap_or(0);
            debug!("Offer placed: order_id={}", order_id);

            // Track the order
            let mut tracker = self.tracker.lock().await;
            tracker.track_order(
                order_id, market_id, OrderType::Offer as i32,
                price, quantity, nonce, &signature, &signed_data,
            );
            drop(placement);

            Ok(order_id)
        } else {
            anyhow::bail!("Failed to place offer: {}", response.message)
        }
    }

    /// Submit a signed order.
    #[allow(clippy::too_many_arguments)]
    async fn submit_signed(
        &mut self,
        market_id: &str,
        side: OrderType,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
        signature: &str,
        signed_data: &[u8],
        nonce: u64,
    ) -> Result<SubmitOrderResponse> {
        #[cfg(test)]
        if let Some(stub) = self.stub_submit.clone() {
            return match stub {
                Ok(order_id) => Ok(SubmitOrderResponse {
                    success: true,
                    order: Some(Order { order_id, ..Default::default() }),
                    ..Default::default()
                }),
                Err(status) => Err(crate::client::submit_order_error(status)),
            };
        }
        self.client
            .submit_order(
                market_id,
                side,
                price.to_string(),
                quantity.to_string(),
                order_ref,
                Some(signature.to_string()),
                signed_data.to_vec(),
                nonce,
            )
            .await
    }

    /// Stamp a failed submit under the tracker lock, then release its guard.
    async fn note_submit_failed(&self, submit: FailedSubmit, placement: PlacementGuard) {
        let mut tracker = self.tracker.lock().await;
        tracker.note_submit_failed(submit, Instant::now());
        drop(placement);
    }

    /// Cancel an order
    pub async fn cancel_order(&mut self, order_id: u64) -> Result<()> {
        debug!("Cancelling order: {}", order_id);

        #[cfg(test)]
        if let Some(cancels) = self.stub_cancels.as_mut() {
            cancels.push(order_id);
            if let Some(stop) = &self.stop_on_cancel {
                stop.signal();
            }
            self.tracker.lock().await.cancel_order(order_id);
            return Ok(());
        }
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

    /// Get active orders for a market; any booked by a failed submit are tracked
    /// first, so a later cancel cannot turn their matches into rejects.
    pub async fn get_active_orders(&mut self, market_id: &str) -> Result<Vec<Order>> {
        #[cfg(test)]
        let orders = match self.stub_orders.clone() {
            Some(orders) => orders,
            None => self.client.get_active_orders(market_id).await?,
        };
        #[cfg(not(test))]
        let orders = self.client.get_active_orders(market_id).await?;
        self.tracker.lock().await.adopt_listed(&orders, Instant::now());
        Ok(orders)
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
        #[cfg(test)]
        if let Some(price) = self.stub_price {
            if let Some(stop) = &self.stop_on_price {
                stop.signal();
            }
            return Ok(price);
        }
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
        let now = self.shaped_offer_levels(market);
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
    fn shaped_offer_levels(&self, market: &MarketConfig) -> Vec<PriceLevel> {
        self.offer_shape(market)
            .map_or_else(|| market.offer_levels.clone(), |shape| shape.shaped.levels)
    }

    /// The shaped offer ladder with its log inputs; None when shaping is unavailable.
    fn offer_shape(&self, market: &MarketConfig) -> Option<OfferShape> {
        let (Some(cfg), Some(depth), Some(tracker)) = (
            market.rfq.as_ref().and_then(|r| r.pool_impact.as_ref()),
            self.pool_depths.get(&market.market_id),
            self.net_positions.as_ref(),
        ) else {
            return None;
        };
        let base_token = market.market_id.split('-').next().unwrap_or("");
        let desk = tracker.desk_net(base_token);
        Some(OfferShape {
            shaped: shape_offer_levels(&market.offer_levels, cfg, depth.base_reserve, desk),
            enabled: cfg.enabled,
            desk,
            reserve: depth.base_reserve,
        })
    }

    /// Per-side funding for placing the grid (see [`grid_funding`]).
    /// `offer_levels` is the (possibly shaped) offer set that would be placed.
    fn check_grid_balance(
        &self,
        market_config: &MarketConfig,
        mid_price: Option<f64>,
        offer_levels: &[PriceLevel],
    ) -> GridFunding {
        let usable = balances_usable(
            self.balances_set_at,
            self.config.balance_stale_after_secs,
            Instant::now(),
        );
        grid_funding(
            &self.balances,
            usable,
            market_config,
            mid_price,
            offer_levels,
            self.config.fee_reserve_cc,
        )
    }

    /// Rate-limited warnings for unknown funding, plus a log line whenever a
    /// side becomes short or recovers.
    fn note_funding(&mut self, market_id: &str, funding: &GridFunding) {
        match funding.why {
            Some(UnknownWhy::Aged) => {
                if rate_limit(&mut self.balance_age_warned_at, FUNDING_WARN_EVERY) {
                    warn!("Grid paused for {}: balances missing or aged", market_id);
                }
            }
            Some(UnknownWhy::FeeReserve) => {
                if rate_limit(&mut self.fee_warned_at, FUNDING_WARN_EVERY) {
                    warn!(
                        "Insufficient CC for fees: {:.4} < {:.2} reserve",
                        funding.cc_avail, self.config.fee_reserve_cc
                    );
                }
            }
            None => {}
        }
        let (base, quote) = market_id.split_once('-').unwrap_or((market_id, market_id));
        self.note_side(market_id, "bids", funding.bid, quote, funding.bid_avail, funding.bid_need);
        self.note_side(market_id, "offers", funding.offer, base, funding.offer_avail, funding.offer_need);
    }

    fn note_side(
        &mut self,
        market_id: &str,
        side: &'static str,
        funding: Funding,
        token: &str,
        avail: f64,
        need: f64,
    ) {
        let key = (market_id.to_string(), side);
        match funding {
            Funding::Short => {
                if self.short_sides.insert(key) {
                    warn!("Grid {}: {} paused, {} {:.8} < {:.8}", market_id, side, token, avail, need);
                }
            }
            Funding::Funded => {
                if self.short_sides.remove(&key) {
                    info!("Grid {}: {} resumed", market_id, side);
                }
            }
            Funding::Unknown => {}
        }
    }

    /// Apply a market plan by executing its [`grid_ops`] in order, placing `offer_levels`
    /// for replaced offers. Returns the ids whose cancel failed.
    #[allow(clippy::too_many_arguments)]
    async fn execute_plan(
        &mut self,
        market: &MarketConfig,
        mid_price: f64,
        active_orders: &[Order],
        offer_levels: &[PriceLevel],
        plan: (SideAction, SideAction),
        stop: &Shutdown,
        report: &mut CycleReport,
    ) -> Vec<u64> {
        // The shutdown path withdraws the whole grid; not even the tick lookup runs
        if stop.is_shutting_down() {
            return Vec::new();
        }
        let market_id = market.market_id.as_str();
        let (bid_action, offer_action) = plan;
        let tick = if bid_action == SideAction::Replace || offer_action == SideAction::Replace {
            self.get_tick_size(market_id).await
        } else {
            0.0
        };
        debug!("Refreshing grid for {} (cancel→place): mid={}, tick={}", market_id, mid_price, tick);

        let offer_levels: &[PriceLevel] =
            if offer_action == SideAction::Replace { offer_levels } else { &[] };
        let ops = grid_ops(&market.bid_levels, offer_levels, mid_price, tick, active_orders, plan);

        let mut cancelled_ids: Vec<u64> = Vec::new();
        let mut failed_cancels: Vec<u64> = Vec::new();
        let mut offer_placed = false;
        let mut placed: Vec<String> = Vec::new(); // "bid #id@price" or "offer #id@price"
        for op in ops {
            // Checked between RPCs; the shutdown path withdraws the whole grid
            if stop.is_shutting_down() {
                break;
            }
            match op {
                GridOp::Cancel { order_id, leftover } => match self.cancel_order(order_id).await {
                    Ok(()) => cancelled_ids.push(order_id),
                    Err(e) => {
                        if leftover {
                            warn!("Failed to cancel leftover order {}: {}", order_id, e);
                        } else {
                            warn!("Failed to cancel order {}: {}", order_id, e);
                        }
                        failed_cancels.push(order_id);
                    }
                },
                GridOp::Place { side, price, quantity } => {
                    let order_ref = Some(uuid::Uuid::now_v7().to_string());
                    let (label, result) = if side == OrderType::Bid {
                        ("bid", self.place_bid(market_id, &price, &quantity, order_ref).await)
                    } else {
                        ("offer", self.place_offer(market_id, &price, &quantity, order_ref).await)
                    };
                    match result {
                        Ok(id) => {
                            offer_placed |= side == OrderType::Offer;
                            placed.push(format!("{label} #{id}@{price}"));
                        }
                        Err(e) => warn!("Failed to place {} at {}: {}", label, price, e),
                    }
                }
            }
        }
        // The placed set is the shape trigger's baseline; an all-failed reshape keeps the old one
        if offer_placed {
            self.last_shaped_offers.insert(market.market_id.clone(), offer_levels.to_vec());
        }

        report.placed = report.placed.saturating_add(count_u32(placed.len()));
        report.cancelled = report.cancelled.saturating_add(count_u32(cancelled_ids.len()));

        if !placed.is_empty() && cancelled_ids.is_empty() {
            info!("Placed {} orders for {} (mid={}, tick={}): [{}]",
                placed.len(), market_id, mid_price, tick, placed.join(", "));
        }
        debug!(
            "Grid refreshed for {}: cancelled [{}], placed [{}]",
            market_id,
            cancelled_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "),
            placed.join(", "),
        );
        failed_cancels
    }

    /// Price with the two-strike teardown and restore debounce (no price = no
    /// quotes). None means skip the market this cycle.
    async fn fetch_grid_price(&mut self, market_id: &str) -> Option<f64> {
        match self.get_price(market_id).await {
            Ok(p) if p.is_finite() && p > 0.0 => self.accept_price(market_id, p),
            other => {
                let reason = match other {
                    Ok(p) => format!("non-positive price {p}"),
                    Err(e) => e.to_string(),
                };
                self.price_restore_streak.remove(market_id);
                let streak = self.price_fail_streak.entry(market_id.to_string()).or_insert(0);
                *streak = streak.saturating_add(1);
                if *streak >= 2 || self.priceless.contains(market_id) {
                    self.handle_priceless(market_id, &reason).await;
                } else {
                    warn!(
                        "Market {} price fetch failed ({}); grid unchanged, will tear down \
                         on a second consecutive failure",
                        market_id, reason
                    );
                }
                None
            }
        }
    }

    /// A valid price. A priceless market resumes on the second success in a row.
    fn accept_price(&mut self, market_id: &str, p: f64) -> Option<f64> {
        self.price_fail_streak.remove(market_id);
        self.last_seen_price.insert(market_id.to_string(), p);
        if self.priceless.contains(market_id) {
            // Resuming also takes two consecutive successes, so a feed
            // flapping around its timeout does not re-grid each time.
            let streak =
                self.price_restore_streak.entry(market_id.to_string()).or_insert(0);
            *streak = streak.saturating_add(1);
            if *streak < 2 {
                debug!(
                    "Market {} price back ({}); awaiting a second consecutive \
                     success before resuming",
                    market_id, p
                );
                return None;
            }
            self.price_restore_streak.remove(market_id);
            self.priceless.remove(market_id);
            info!("Market {} price restored ({}); resuming quoting", market_id, p);
            // A restore always refreshes, since cancels may have failed during the outage
            self.force_refresh.insert(market_id.to_string());
        } else {
            debug!("Market {} price: {}", market_id, p);
        }
        Some(p)
    }

    /// One grid pass over the enabled markets; each visit plans with plan_market and may park the market.
    /// Stops between markets on shutdown or past `deadline`; the next cycle resumes there.
    pub async fn update_cycle(&mut self, stop: &Shutdown, deadline: Instant) -> Result<CycleReport> {
        let markets: Vec<MarketConfig> = self.config.enabled_markets()
            .into_iter().cloned().collect();
        let total = markets.len();
        let start = if self.resume_from < total { self.resume_from } else { 0 };
        self.resume_from = 0;
        let mut report = CycleReport::default();

        let rotation = markets.iter().enumerate().cycle().skip(start).take(total);
        for (visited, (index, market)) in rotation.enumerate() {
            if stop.is_shutting_down() {
                report.deferred = count_u32(total.saturating_sub(visited));
                break;
            }
            if past_soft_deadline(visited, Instant::now(), deadline) {
                self.resume_from = index;
                report.deferred = count_u32(total.saturating_sub(visited));
                warn!(
                    "Grid cycle past its deadline; {} of {} market(s) deferred to the next cycle",
                    report.deferred, total
                );
                break;
            }
            self.visit_market(market, stop, &mut report).await;
        }
        debug!("Grid cycle: {:?}", report);
        Ok(report)
    }

    /// One market of a grid cycle.
    async fn visit_market(&mut self, market: &MarketConfig, stop: &Shutdown, report: &mut CycleReport) {
        let market_id = market.market_id.as_str();
        report.markets = report.markets.saturating_add(1);

        // 1. No rungs configured: nothing to quote; leftovers are withdrawn.
        if market.bid_levels.is_empty() && market.offer_levels.is_empty() {
            report.no_levels = report.no_levels.saturating_add(1);
            self.withdraw_unquoted(market_id, stop, report).await;
            return;
        }
        let bid_expected = market.bid_levels.len();

        // 2. Park check, before any RPC.
        let now = Instant::now();
        let offer_levels = self.shaped_offer_levels(market);
        let seen_price = self.last_seen_price.get(market_id).copied();
        let funding = self.check_grid_balance(market, seen_price, &offer_levels);
        let probe_age = self.last_probe.get(market_id).map(|t| now.saturating_duration_since(*t));
        let last_resting = self.last_resting.get(market_id).copied();
        if can_park(&funding, bid_expected, offer_levels.len(), last_resting, probe_age) {
            if self.parked.insert(market_id.to_string()) {
                info!(
                    "Grid {}: parked, no fundable side; re-probing every {}s",
                    market_id, UNFUNDED_PROBE.as_secs()
                );
            }
            report.parked = report.parked.saturating_add(1);
            return;
        }

        // 3. Price. Stamped first so a failing probe still waits a full interval.
        self.last_probe.insert(market_id.to_string(), now);
        let Some(current_price) = self.fetch_grid_price(market_id).await else {
            return;
        };

        // 4. Funding at the fresh price, from the one SHAPED offer set uncross and placement use;
        // counting the raw ladder would read an omitted rung as "order missing" and churn.
        let shape = self.offer_shape(market);
        let offer_levels =
            shape.as_ref().map_or_else(|| market.offer_levels.clone(), |s| s.shaped.levels.clone());
        let offer_expected = offer_levels.len();
        let funding = self.check_grid_balance(market, Some(current_price), &offer_levels);
        self.note_funding(market_id, &funding);
        let fundable = |f: Funding, expected: usize| f == Funding::Funded && expected > 0;
        if (fundable(funding.bid, bid_expected) || fundable(funding.offer, offer_expected))
            && self.parked.remove(market_id)
        {
            info!("Grid {}: unparked", market_id);
        }
        if funding.bid == Funding::Unknown && funding.offer == Funding::Unknown {
            report.held_unknown = report.held_unknown.saturating_add(1);
            self.last_resting.remove(market_id);
            return;
        }

        if stop.is_shutting_down() {
            return;
        }

        // 5. Active orders. A failure skips only this market, so one broken
        // market cannot freeze grid maintenance for the markets after it.
        let orders = match self.get_active_orders(market_id).await {
            Ok(o) => o,
            Err(e) => {
                warn!(
                    "Market {} order fetch failed ({}); skipping this market this cycle",
                    market_id, e
                );
                return;
            }
        };
        let orders = self.retry_failed_cancels(market_id, orders, stop, report).await;
        let resting = side_counts(&orders);

        // 6. Plan. Each side refreshes on a move past its own anchor; a restore forces both.
        let visit = self.plan_visit(market, current_price, &funding, offer_expected, resting);
        let mut plan = visit.plan;

        if plan == (SideAction::Keep, SideAction::Keep) {
            self.stamp_anchors(market_id, &visit, plan, resting, current_price);
            self.last_resting.insert(market_id.to_string(), resting);
            return;
        }

        // 7. A one-sided refresh must not place into the kept side's resting orders.
        let one_sided = matches!(
            plan,
            (SideAction::Replace, SideAction::Keep) | (SideAction::Keep, SideAction::Replace)
        );
        if one_sided && !stop.is_shutting_down() {
            let tick = self.get_tick_size(market_id).await;
            let sides = (fundable(funding.bid, bid_expected), fundable(funding.offer, offer_expected));
            let uncrossed =
                uncross(plan, &market.bid_levels, &offer_levels, current_price, tick, &orders, sides);
            if uncrossed != plan {
                debug!("Grid {}: new rungs would cross resting orders; refreshing both sides", market_id);
                plan = uncrossed;
            }
        }

        info!(
            "Market {} needs refresh: bids {}/{} {:?}, offers {}/{} {:?}, partial={}/{}, force={}/{}, shape_changed={}",
            market_id, resting.bids, bid_expected, plan.0, resting.offers, offer_expected, plan.1,
            resting.bid_partial, resting.offer_partial, visit.force_bid, visit.force_offer,
            visit.shape_changed
        );
        if plan.1 == SideAction::Replace {
            if let Some(shape) = &shape {
                shape.log(market_id);
            }
        }
        self.last_resting.remove(market_id);
        let failed = self.execute_plan(market, current_price, &orders, &offer_levels, plan, stop, report).await;
        if !failed.is_empty() {
            self.pending_cancels.entry(market_id.to_string()).or_default().extend(failed);
        }
        report.refreshed = report.refreshed.saturating_add(1);
        self.stamp_anchors(market_id, &visit, plan, resting, current_price);
    }

    /// Cancel a market's earlier failed cancels that are still listed; returns the
    /// listing without the orders cancelled now.
    async fn retry_failed_cancels(
        &mut self,
        market_id: &str,
        mut orders: Vec<Order>,
        stop: &Shutdown,
        report: &mut CycleReport,
    ) -> Vec<Order> {
        let Some(pending) = self.pending_cancels.get_mut(market_id) else {
            return orders;
        };
        pending.retain(|id| orders.iter().any(|o| o.order_id == *id));
        let ids: Vec<u64> = pending.iter().copied().collect();
        for order_id in ids {
            // Checked between RPCs; the shutdown path withdraws the whole grid
            if stop.is_shutting_down() {
                break;
            }
            match self.cancel_order(order_id).await {
                Ok(()) => {
                    report.cancelled = report.cancelled.saturating_add(1);
                    orders.retain(|o| o.order_id != order_id);
                    if let Some(pending) = self.pending_cancels.get_mut(market_id) {
                        pending.remove(&order_id);
                    }
                }
                Err(e) => warn!("Failed to cancel leftover order {}: {}", order_id, e),
            }
        }
        if self.pending_cancels.get(market_id).is_some_and(HashSet::is_empty) {
            self.pending_cancels.remove(market_id);
        }
        orders
    }

    /// Cancel whatever rests in a market without rungs. Once it is seen empty, it is
    /// listed again only every `UNFUNDED_PROBE`.
    async fn withdraw_unquoted(&mut self, market_id: &str, stop: &Shutdown, report: &mut CycleReport) {
        self.pending_cancels.remove(market_id);
        let now = Instant::now();
        let probe_age = self.last_probe.get(market_id).map(|t| now.saturating_duration_since(*t));
        let seen_empty = self.last_resting.get(market_id).is_some_and(|r| !r.any_resting());
        if (seen_empty && probe_age.is_some_and(|age| age < UNFUNDED_PROBE)) || stop.is_shutting_down() {
            return;
        }
        self.last_probe.insert(market_id.to_string(), now);
        let orders = match self.get_active_orders(market_id).await {
            Ok(o) => o,
            Err(e) => {
                warn!("Market {} order fetch failed ({}); skipping this market this cycle", market_id, e);
                return;
            }
        };
        if !orders.is_empty() {
            info!("Grid {}: no rungs configured; cancelling {} resting order(s)", market_id, orders.len());
        }
        let mut all_cancelled = true;
        for order in &orders {
            // Checked between RPCs; the shutdown path withdraws the whole grid
            if stop.is_shutting_down() {
                all_cancelled = false;
                break;
            }
            match self.cancel_order(order.order_id).await {
                Ok(()) => report.cancelled = report.cancelled.saturating_add(1),
                Err(e) => {
                    warn!("Failed to cancel order {}: {}", order.order_id, e);
                    all_cancelled = false;
                }
            }
        }
        if all_cancelled {
            self.last_resting.insert(market_id.to_string(), SideCounts::default());
        } else {
            self.last_resting.remove(market_id);
        }
    }

    /// Record a visit's anchors for the plan it carried out, which may differ from
    /// the planned one: Replaced sides take `price`, a resting side lacking one adopts it.
    fn stamp_anchors(
        &mut self,
        market_id: &str,
        visit: &VisitPlan,
        plan: (SideAction, SideAction),
        resting: SideCounts,
        price: f64,
    ) {
        let anchored = if plan == visit.plan {
            visit.anchored
        } else {
            next_anchors(visit.anchors, plan, resting, price)
        };
        if anchored != visit.anchors {
            self.grid_anchors.insert(market_id.to_string(), anchored);
        }
    }

    /// Plan both sides of a visited market. A pending restore forces both and is consumed here.
    fn plan_visit(
        &mut self,
        market: &MarketConfig,
        current_price: f64,
        funding: &GridFunding,
        offer_expected: usize,
        resting: SideCounts,
    ) -> VisitPlan {
        let market_id = market.market_id.as_str();
        let anchors = self.grid_anchors.get(market_id).copied().unwrap_or_default();
        let threshold = market.price_change_threshold_percent;
        let restore = self.force_refresh.remove(market_id);
        let bid_moved = moved_past(anchors.bid, current_price, threshold);
        let offer_moved = moved_past(anchors.offer, current_price, threshold);
        for (side, moved) in [("bids", bid_moved), ("offers", offer_moved)] {
            if let Some(change_pct) = moved {
                debug!(
                    "Price moved {:.2}% (threshold {:.2}%), refreshing {} for {}",
                    change_pct, threshold, side, market_id
                );
            }
        }
        let force_bid = restore || bid_moved.is_some();
        let force_offer = restore || offer_moved.is_some();
        let shape_changed = self.shaped_offers_changed(market);
        let plan = plan_market(&PlanInput {
            bid: funding.bid,
            offer: funding.offer,
            bid_expected: market.bid_levels.len(),
            offer_expected,
            resting,
            force_bid,
            force_offer,
            shape_changed,
        });
        let anchored = next_anchors(anchors, plan, resting, current_price);
        VisitPlan { plan, anchors, anchored, force_bid, force_offer, shape_changed }
    }
}

/// The plan for one visited market, with what drove it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VisitPlan {
    plan: (SideAction, SideAction),
    anchors: SideAnchors,
    anchored: SideAnchors,
    force_bid: bool,
    force_offer: bool,
    shape_changed: bool,
}

/// A shaped offer ladder with what its impact log reports.
struct OfferShape {
    shaped: ShapedOffers,
    enabled: bool,
    desk: f64,
    reserve: f64,
}

impl OfferShape {
    fn log(&self, market_id: &str) {
        self.shaped.log(market_id, self.enabled, self.desk, self.reserve);
    }
}

/// Unlocked CC balance (is_canton_coin flag)
fn unlocked_cc(balances: &[TokenBalance]) -> f64 {
    balances.iter()
        .find(|b| b.is_canton_coin)
        .and_then(|b| b.unlocked_amount.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Unlocked balance for a token by instrument_id
fn unlocked_token(balances: &[TokenBalance], instrument_id: &str) -> f64 {
    balances.iter()
        .find(|b| b.instrument_id == instrument_id)
        .and_then(|b| b.unlocked_amount.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn side_funding(need: f64, avail: f64) -> Funding {
    if need > 0.0 && avail < need { Funding::Short } else { Funding::Funded }
}

/// Per-side grid funding from unlocked balances (pure). Aged data or CC under the
/// fee reserve: both Unknown; no mid: bids Unknown; not BASE-QUOTE: both Funded.
pub(crate) fn grid_funding(
    balances: &[TokenBalance],
    usable: bool,
    market: &MarketConfig,
    mid: Option<f64>,
    offer_levels: &[PriceLevel],
    fee_reserve_cc: f64,
) -> GridFunding {
    let mut out = GridFunding {
        bid: Funding::Unknown,
        offer: Funding::Unknown,
        why: None,
        cc_avail: 0.0,
        bid_need: 0.0,
        bid_avail: 0.0,
        offer_need: 0.0,
        offer_avail: 0.0,
    };
    if !usable || balances.is_empty() {
        out.why = Some(UnknownWhy::Aged);
        return out;
    }
    let Some((base, quote)) = market
        .market_id
        .split_once('-')
        .filter(|(_, quote)| !quote.contains('-'))
    else {
        out.bid = Funding::Funded;
        out.offer = Funding::Funded;
        return out;
    };

    let cc_unlocked = unlocked_cc(balances);
    let base_unlocked = if base == "CC" { cc_unlocked } else { unlocked_token(balances, base) };
    let quote_unlocked = if quote == "CC" { cc_unlocked } else { unlocked_token(balances, quote) };
    out.cc_avail = cc_unlocked;

    // CC fee reserve is a prerequisite for either side
    if cc_unlocked < fee_reserve_cc {
        out.why = Some(UnknownWhy::FeeReserve);
        return out;
    }

    // Available amounts after reserving CC for fees
    out.bid_avail = if quote == "CC" { quote_unlocked - fee_reserve_cc } else { quote_unlocked };
    out.offer_avail = if base == "CC" { base_unlocked - fee_reserve_cc } else { base_unlocked };

    // Total base needed for offers (selling base)
    out.offer_need = offer_levels
        .iter()
        .map(|l| l.quantity.parse::<f64>().unwrap_or(0.0))
        .sum();
    out.offer = side_funding(out.offer_need, out.offer_avail);

    // Total quote needed for bids (buying base with quote)
    if let Some(mid) = mid.filter(|m| m.is_finite() && *m > 0.0) {
        out.bid_need = market.bid_levels.iter()
            .map(|l| {
                let qty: f64 = l.quantity.parse().unwrap_or(0.0);
                qty * mid * (1.0 + l.delta_percent / 100.0)
            })
            .sum();
        out.bid = side_funding(out.bid_need, out.bid_avail);
    }
    out
}

/// Count resting orders per side (by `order_type`) and flag partial fills.
pub(crate) fn side_counts(orders: &[Order]) -> SideCounts {
    let mut counts = SideCounts::default();
    for o in orders {
        let partial = o.filled_quantity.parse::<f64>().is_ok_and(|f| f > 0.0);
        if o.order_type == OrderType::Bid as i32 {
            counts.bids = counts.bids.saturating_add(1);
            counts.bid_partial |= partial;
        } else if o.order_type == OrderType::Offer as i32 {
            counts.offers = counts.offers.saturating_add(1);
            counts.offer_partial |= partial;
        }
    }
    counts
}

/// Per-side plan (bids, offers): Unknown keeps, fundable replaces on its own trigger
/// (including a rung count off in either direction), short or rung-less cancels whatever rests.
pub(crate) fn plan_market(input: &PlanInput) -> (SideAction, SideAction) {
    let bid_fundable = input.bid == Funding::Funded && input.bid_expected > 0;
    let offer_fundable = input.offer == Funding::Funded && input.offer_expected > 0;
    let bid_trigger = input.force_bid
        || input.resting.bids != input.bid_expected
        || input.resting.bid_partial;
    let offer_trigger = input.force_offer
        || input.resting.offers != input.offer_expected
        || input.resting.offer_partial
        || input.shape_changed;

    let side = |funding: Funding, fundable: bool, trigger: bool, resting: usize| {
        if funding == Funding::Unknown {
            SideAction::Keep
        } else if fundable {
            if trigger { SideAction::Replace } else { SideAction::Keep }
        } else if resting > 0 {
            SideAction::Cancel
        } else {
            SideAction::Keep
        }
    };
    (
        side(input.bid, bid_fundable, bid_trigger, input.resting.bids),
        side(input.offer, offer_fundable, offer_trigger, input.resting.offers),
    )
}

/// Percent move of `price` from `anchor` once it reaches `threshold_pct`; None otherwise.
pub(crate) fn moved_past(anchor: Option<f64>, price: f64, threshold_pct: f64) -> Option<f64> {
    let last = anchor?;
    let change_pct = ((price - last).abs() / last) * 100.0;
    (change_pct >= threshold_pct).then_some(change_pct)
}

/// Anchors after a visit: Replaced sides take `price`; a kept side with orders
/// resting but no anchor adopts it (e.g. orders from before a restart).
pub(crate) fn next_anchors(
    prev: SideAnchors,
    plan: (SideAction, SideAction),
    resting: SideCounts,
    price: f64,
) -> SideAnchors {
    let next = |anchor: Option<f64>, action: SideAction, count: usize| match action {
        SideAction::Replace => Some(price),
        SideAction::Keep if anchor.is_none() && count > 0 => Some(price),
        _ => anchor,
    };
    SideAnchors {
        bid: next(prev.bid, plan.0, resting.bids),
        offer: next(prev.offer, plan.1, resting.offers),
    }
}

/// Price of a new rung as placed: bids round down to the tick, offers up.
fn rung_price(side: OrderType, mid: f64, delta_percent: f64, tick: f64) -> String {
    let steps = mid * (1.0 + delta_percent / 100.0) / tick;
    let price = if side == OrderType::Bid { steps.floor() } else { steps.ceil() } * tick;
    format!("{price:.10}")
}

/// Ops for a market plan: Cancel sides withdrawn first; Replace sides re-placed tightest
/// first, each paired with a cancel from its own side; Keep sides untouched.
pub(crate) fn grid_ops(
    bid_levels: &[PriceLevel],
    offer_levels: &[PriceLevel],
    mid: f64,
    tick: f64,
    orders: &[Order],
    plan: (SideAction, SideAction),
) -> Vec<GridOp> {
    let (bid_action, offer_action) = plan;
    // New orders as (abs_delta, side, price, quantity), tightest spread first
    let mut new_orders: Vec<(f64, OrderType, String, String)> = Vec::new();
    if bid_action == SideAction::Replace {
        for level in bid_levels {
            let price = rung_price(OrderType::Bid, mid, level.delta_percent, tick);
            new_orders.push((level.delta_percent.abs(), OrderType::Bid, price, level.quantity.clone()));
        }
    }
    if offer_action == SideAction::Replace {
        for level in offer_levels {
            let price = rung_price(OrderType::Offer, mid, level.delta_percent, tick);
            new_orders.push((level.delta_percent.abs(), OrderType::Offer, price, level.quantity.clone()));
        }
    }
    new_orders.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut bid_old = side_queue(orders, OrderType::Bid, mid);
    let mut offer_old = side_queue(orders, OrderType::Offer, mid);
    let mut ops = Vec::new();
    // A withdrawn side has no replacement to wait for, so it goes first
    for (action, queue) in [(bid_action, &mut bid_old), (offer_action, &mut offer_old)] {
        if action == SideAction::Cancel {
            ops.extend(queue.drain(..).map(|order_id| GridOp::Cancel { order_id, leftover: true }));
        }
    }
    // Interleave: cancel one old order of the same side, place one new
    for (_delta, side, price, quantity) in new_orders {
        let old = if side == OrderType::Bid { bid_old.pop_front() } else { offer_old.pop_front() };
        if let Some(order_id) = old {
            ops.push(GridOp::Cancel { order_id, leftover: false });
        }
        ops.push(GridOp::Place { side, price, quantity });
    }
    // Leftovers on Replace sides
    for (action, leftovers) in [(bid_action, bid_old), (offer_action, offer_old)] {
        if action == SideAction::Replace {
            ops.extend(leftovers.into_iter().map(|order_id| GridOp::Cancel { order_id, leftover: true }));
        }
    }
    ops
}

/// A one-sided refresh whose tightest new rung would reach the kept side's best resting
/// order also refreshes the kept side (`fundable` per side), or withdraws it if unfundable.
pub(crate) fn uncross(
    plan: (SideAction, SideAction),
    bid_levels: &[PriceLevel],
    offer_levels: &[PriceLevel],
    mid: f64,
    tick: f64,
    orders: &[Order],
    fundable: (bool, bool),
) -> (SideAction, SideAction) {
    let escalate = |fundable: bool| if fundable { SideAction::Replace } else { SideAction::Cancel };
    match plan {
        (SideAction::Replace, SideAction::Keep) => {
            let new_bid = tightest_new(bid_levels, OrderType::Bid, mid, tick);
            match (new_bid, best_resting(orders, OrderType::Offer)) {
                (Some(bid), Some(offer)) if bid >= offer => (SideAction::Replace, escalate(fundable.1)),
                _ => plan,
            }
        }
        (SideAction::Keep, SideAction::Replace) => {
            let new_offer = tightest_new(offer_levels, OrderType::Offer, mid, tick);
            match (best_resting(orders, OrderType::Bid), new_offer) {
                (Some(bid), Some(offer)) if offer <= bid => (escalate(fundable.0), SideAction::Replace),
                _ => plan,
            }
        }
        _ => plan,
    }
}

/// Tightest new rung on one side (highest bid, lowest offer) as [`grid_ops`] prices it.
fn tightest_new(levels: &[PriceLevel], side: OrderType, mid: f64, tick: f64) -> Option<f64> {
    let prices = levels
        .iter()
        .filter_map(|l| rung_price(side, mid, l.delta_percent, tick).parse::<f64>().ok());
    if side == OrderType::Bid { prices.reduce(f64::max) } else { prices.reduce(f64::min) }
}

/// Best resting price on one side (highest bid, lowest offer).
fn best_resting(orders: &[Order], side: OrderType) -> Option<f64> {
    let prices = orders
        .iter()
        .filter(|o| o.order_type == side as i32)
        .filter_map(|o| o.price.parse::<f64>().ok());
    if side == OrderType::Bid { prices.reduce(f64::max) } else { prices.reduce(f64::min) }
}

/// Whether a market can be skipped without any RPC: nothing rested at its last
/// no-action visit, no side is fundable or Unknown, and it was probed recently.
pub(crate) fn can_park(
    funding: &GridFunding,
    bid_expected: usize,
    offer_expected: usize,
    last_resting: Option<SideCounts>,
    probe_age: Option<Duration>,
) -> bool {
    let unfundable = |f: Funding, expected: usize| {
        f == Funding::Short || (f == Funding::Funded && expected == 0)
    };
    last_resting.is_some_and(|r| !r.any_resting())
        && unfundable(funding.bid, bid_expected)
        && unfundable(funding.offer, offer_expected)
        && probe_age.is_some_and(|age| age < UNFUNDED_PROBE)
}

/// True when a cycle should stop before its next market. The first market is
/// always visited, so a cycle makes progress however late it starts.
pub(crate) fn past_soft_deadline(visited: usize, now: Instant, deadline: Instant) -> bool {
    visited > 0 && now >= deadline
}

/// `budget` from now; a budget too large to represent becomes a far-future instant.
pub(crate) fn deadline_after(budget: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(budget)
        .or_else(|| now.checked_add(FAR_FUTURE))
        .unwrap_or(now)
}

/// Stand-in for an unrepresentable deadline.
const FAR_FUTURE: Duration = Duration::from_secs(31_536_000);

/// Ids of one side's resting orders, nearest to `mid` first.
fn side_queue(orders: &[Order], side: OrderType, mid: f64) -> VecDeque<u64> {
    let mut by_distance: Vec<(f64, u64)> = orders
        .iter()
        .filter(|o| o.order_type == side as i32)
        .map(|o| {
            let p: f64 = o.price.parse().unwrap_or(0.0);
            ((p - mid).abs(), o.order_id)
        })
        .collect();
    by_distance.sort_by(|a, b| a.0.total_cmp(&b.0));
    by_distance.into_iter().map(|(_, id)| id).collect()
}

/// True (and restamps `slot`) when at least `every` has passed since the last stamp.
fn rate_limit(slot: &mut Option<Instant>, every: Duration) -> bool {
    let now = Instant::now();
    if slot.is_none_or(|t| now.saturating_duration_since(t) >= every) {
        *slot = Some(now);
        true
    } else {
        false
    }
}

fn count_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
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
mod balance_age_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn balances_usable_requires_a_snapshot_and_respects_the_window() {
        let now = Instant::now();
        assert!(!balances_usable(None, 120, now), "no snapshot: unusable");
        assert!(!balances_usable(None, 0, now), "no snapshot even when disabled");
        let old = now.checked_sub(Duration::from_secs(200));
        assert!(balances_usable(Some(now), 120, now), "fresh snapshot usable");
        if let Some(old) = old {
            assert!(!balances_usable(Some(old), 120, now), "aged snapshot unusable");
            assert!(balances_usable(Some(old), 0, now), "0 disables the age check");
        }
    }
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

#[cfg(test)]
mod grid_plan_tests {
    use super::*;
    use crate::secret::Secret;

    fn level(delta_percent: f64, quantity: &str) -> PriceLevel {
        PriceLevel { delta_percent, quantity: quantity.to_string() }
    }

    fn ladder(sign: f64, quantity: &str) -> Vec<PriceLevel> {
        vec![level(0.5 * sign, quantity), level(1.0 * sign, quantity), level(1.5 * sign, quantity)]
    }

    fn market(id: &str, bid_levels: Vec<PriceLevel>, offer_levels: Vec<PriceLevel>) -> MarketConfig {
        MarketConfig {
            market_id: id.to_string(),
            enabled: true,
            base_order_size: None,
            bid_levels,
            offer_levels,
            price_change_threshold_percent: 1.0,
            rfq: None,
        }
    }

    fn token(id: &str, unlocked: &str) -> TokenBalance {
        TokenBalance {
            instrument_id: id.to_string(),
            unlocked_amount: unlocked.to_string(),
            ..Default::default()
        }
    }

    fn cc(unlocked: &str) -> TokenBalance {
        TokenBalance {
            instrument_id: "Amulet".to_string(),
            unlocked_amount: unlocked.to_string(),
            is_canton_coin: true,
            ..Default::default()
        }
    }

    fn funding(bid: Funding, offer: Funding) -> GridFunding {
        GridFunding {
            bid,
            offer,
            why: None,
            cc_avail: 0.0,
            bid_need: 0.0,
            bid_avail: 0.0,
            offer_need: 0.0,
            offer_avail: 0.0,
        }
    }

    fn resting(bids: usize, offers: usize) -> SideCounts {
        SideCounts { bids, offers, ..Default::default() }
    }

    fn input(bid: Funding, offer: Funding, resting: SideCounts) -> PlanInput {
        PlanInput {
            bid,
            offer,
            bid_expected: 3,
            offer_expected: 3,
            resting,
            force_bid: false,
            force_offer: false,
            shape_changed: false,
        }
    }

    fn forced(i: PlanInput) -> PlanInput {
        PlanInput { force_bid: true, force_offer: true, ..i }
    }

    use Funding::{Funded, Short, Unknown};
    use SideAction::{Cancel, Keep, Replace};

    // ---- grid_funding ----

    #[test]
    fn aged_or_missing_balances_are_unknown() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let offers = m.offer_levels.clone();
        let f = grid_funding(&[cc("100")], false, &m, Some(700.0), &offers, 5.0);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::Aged)));
        let f = grid_funding(&[], true, &m, Some(700.0), &offers, 5.0);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::Aged)));
    }

    #[test]
    fn cc_below_the_fee_reserve_is_unknown() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("1"), token("SPYe", "10"), token("USDCx", "10000")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::FeeReserve)));
        assert_eq!(f.cc_avail, 1.0);
    }

    /// Offers far short of their rungs while bids are funded.
    #[test]
    fn spye_offers_short_while_bids_are_funded() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "0.001"), token("USDCx", "1000")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0);
        assert_eq!((f.bid, f.offer, f.why), (Funded, Short, None));
        assert!((f.offer_need - 0.15).abs() < 1e-12, "offer_need {}", f.offer_need);
        assert_eq!(f.offer_avail, 0.001);
        assert!(f.bid_need > 100.0 && f.bid_need < 1000.0, "bid_need {}", f.bid_need);
    }

    #[test]
    fn cc_as_quote_or_base_subtracts_the_reserve() {
        let flat = || vec![level(0.0, "10"), level(0.0, "10"), level(0.0, "10")];
        let m = market("EDELx-CC", flat(), Vec::new());
        let balances = [cc("100"), token("EDELx", "0")];
        // 30 base at 3.0 = 90 quote needed, 95 available after the reserve
        let f = grid_funding(&balances, true, &m, Some(3.0), &[], 5.0);
        assert_eq!(f.bid, Funded);
        assert_eq!(f.bid_avail, 95.0);
        // 30 base at 3.2 = 96 > 95
        let f = grid_funding(&balances, true, &m, Some(3.2), &[], 5.0);
        assert_eq!(f.bid, Short);

        let m = market("CC-USDCx", Vec::new(), flat());
        let balances = [cc("100"), token("USDCx", "0")];
        let f = grid_funding(&balances, true, &m, Some(0.15), &m.offer_levels, 5.0);
        assert_eq!((f.offer, f.offer_avail), (Funded, 95.0));
        let heavy = vec![level(0.0, "32"), level(0.0, "32"), level(0.0, "32")];
        let f = grid_funding(&balances, true, &m, Some(0.15), &heavy, 5.0);
        assert_eq!(f.offer, Short);
    }

    #[test]
    fn non_base_quote_market_is_funded() {
        let balances = [cc("100")];
        for id in ["SPYE", "A-B-C"] {
            let m = market(id, ladder(-1.0, "1"), ladder(1.0, "1"));
            let f = grid_funding(&balances, true, &m, Some(1.0), &m.offer_levels, 5.0);
            assert_eq!((f.bid, f.offer), (Funded, Funded), "market {id}");
        }
    }

    #[test]
    fn no_mid_leaves_bids_unknown() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "1"), token("USDCx", "1000")];
        let f = grid_funding(&balances, true, &m, None, &m.offer_levels, 5.0);
        assert_eq!((f.bid, f.offer), (Unknown, Funded));
        let f = grid_funding(&balances, true, &m, Some(f64::NAN), &m.offer_levels, 5.0);
        assert_eq!(f.bid, Unknown);
    }

    #[test]
    fn a_side_needing_nothing_is_funded() {
        let m = market("SPYe-USDCx", Vec::new(), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "0"), token("USDCx", "0")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0);
        assert_eq!((f.bid, f.bid_need), (Funded, 0.0));
        assert_eq!(f.offer, Short);
    }

    // ---- plan_market ----

    /// Funded bids resting 3/3 and short offers with nothing resting cost no RPC.
    #[test]
    fn short_side_with_nothing_resting_is_left_alone() {
        let i = input(Funded, Short, resting(3, 0));
        assert_eq!(plan_market(&i), (Keep, Keep));
        // A re-shape only triggers funded offers
        let i = PlanInput { shape_changed: true, ..i };
        assert_eq!(plan_market(&i), (Keep, Keep));
    }

    #[test]
    fn short_side_with_resting_orders_is_cancelled_immediately() {
        for offers in [1, 3] {
            let i = input(Funded, Short, resting(3, offers));
            assert_eq!(plan_market(&i), (Keep, Cancel), "{offers} offer(s) resting");
        }
        let i = input(Short, Funded, resting(2, 3));
        assert_eq!(plan_market(&i), (Cancel, Keep));
    }

    #[test]
    fn force_replaces_both_funded_sides() {
        let i = forced(input(Funded, Funded, resting(3, 3)));
        assert_eq!(plan_market(&i), (Replace, Replace));
        let i = forced(input(Funded, Short, resting(3, 0)));
        assert_eq!(plan_market(&i), (Replace, Keep));
        let i = forced(input(Funded, Short, resting(3, 1)));
        assert_eq!(plan_market(&i), (Replace, Cancel));
    }

    #[test]
    fn a_forced_side_replaces_only_itself() {
        let i = PlanInput { force_bid: true, ..input(Funded, Funded, resting(3, 3)) };
        assert_eq!(plan_market(&i), (Replace, Keep));
        let i = PlanInput { force_offer: true, ..input(Funded, Funded, resting(3, 3)) };
        assert_eq!(plan_market(&i), (Keep, Replace));
    }

    #[test]
    fn missing_rung_partial_fill_or_reshape_replaces_only_that_side() {
        assert_eq!(plan_market(&input(Funded, Funded, resting(2, 3))), (Replace, Keep));
        let mut counts = resting(3, 3);
        counts.bid_partial = true;
        assert_eq!(plan_market(&input(Funded, Funded, counts)), (Replace, Keep));
        let mut counts = resting(3, 3);
        counts.offer_partial = true;
        assert_eq!(plan_market(&input(Funded, Funded, counts)), (Keep, Replace));
        let i = PlanInput { shape_changed: true, ..input(Funded, Funded, resting(3, 3)) };
        assert_eq!(plan_market(&i), (Keep, Replace));
        // A full, untouched grid is left alone
        assert_eq!(plan_market(&input(Funded, Funded, resting(3, 3))), (Keep, Keep));
    }

    /// An extra order (e.g. one whose paired cancel failed) re-places its side.
    #[test]
    fn a_side_with_more_orders_than_rungs_is_replaced() {
        assert_eq!(plan_market(&input(Funded, Funded, resting(4, 3))), (Replace, Keep));
        assert_eq!(plan_market(&input(Funded, Funded, resting(3, 4))), (Keep, Replace));
    }

    #[test]
    fn unknown_sides_are_never_touched() {
        let i = forced(input(Unknown, Unknown, resting(2, 1)));
        assert_eq!(plan_market(&i), (Keep, Keep));
        let i = input(Unknown, Funded, resting(1, 1));
        assert_eq!(plan_market(&i), (Keep, Replace));
    }

    #[test]
    fn a_side_without_rungs_withdraws_what_rests() {
        let i = PlanInput { offer_expected: 0, ..input(Funded, Funded, resting(3, 2)) };
        assert_eq!(plan_market(&i), (Keep, Cancel));
        let i = PlanInput { offer_expected: 0, ..input(Funded, Funded, resting(3, 0)) };
        assert_eq!(plan_market(&i), (Keep, Keep));
    }

    // ---- anchors ----

    #[test]
    fn a_price_move_past_only_the_offer_anchor_replaces_offers_only() {
        // Offers last placed at 95, bids at 100; the price is now 99.5 with a 1% threshold
        let anchors = SideAnchors { bid: Some(100.0), offer: Some(95.0) };
        let bid_moved = moved_past(anchors.bid, 99.5, 1.0);
        let offer_moved = moved_past(anchors.offer, 99.5, 1.0);
        assert_eq!(bid_moved, None);
        assert!(offer_moved.is_some_and(|pct| (pct - 100.0 * 4.5 / 95.0).abs() < 1e-9));
        let i = PlanInput {
            force_bid: bid_moved.is_some(),
            force_offer: offer_moved.is_some(),
            ..input(Funded, Funded, resting(3, 3))
        };
        let plan = plan_market(&i);
        assert_eq!(plan, (Keep, Replace));
        let next = next_anchors(anchors, plan, resting(3, 3), 99.5);
        assert_eq!(next, SideAnchors { bid: Some(100.0), offer: Some(99.5) });
    }

    #[test]
    fn moved_past_needs_an_anchor_and_the_threshold() {
        assert_eq!(moved_past(None, 200.0, 1.0), None);
        assert_eq!(moved_past(Some(100.0), 100.99, 1.0), None);
        assert!(moved_past(Some(100.0), 101.0, 1.0).is_some());
        assert!(moved_past(Some(100.0), 99.0, 1.0).is_some());
    }

    #[test]
    fn next_anchors_stamp_replaced_sides_and_fill_missing_resting_ones() {
        let none = SideAnchors::default();
        // No-action visit: a side with orders resting adopts the price, an empty one does not
        let next = next_anchors(none, (Keep, Keep), resting(3, 0), 10.0);
        assert_eq!(next, SideAnchors { bid: Some(10.0), offer: None });
        // An existing anchor is kept on Keep and Cancel, restamped on Replace
        let prev = SideAnchors { bid: Some(8.0), offer: Some(9.0) };
        assert_eq!(next_anchors(prev, (Keep, Keep), resting(3, 3), 10.0), prev);
        assert_eq!(next_anchors(prev, (Cancel, Keep), resting(3, 3), 10.0), prev);
        let next = next_anchors(prev, (Keep, Replace), resting(3, 3), 10.0);
        assert_eq!(next, SideAnchors { bid: Some(8.0), offer: Some(10.0) });
        let next = next_anchors(none, (Replace, Keep), resting(0, 0), 10.0);
        assert_eq!(next, SideAnchors { bid: Some(10.0), offer: None });
    }

    // ---- grid_ops ----

    fn place(side: OrderType, price: &str, quantity: &str) -> GridOp {
        GridOp::Place { side, price: price.to_string(), quantity: quantity.to_string() }
    }

    fn cancel(order_id: u64) -> GridOp {
        GridOp::Cancel { order_id, leftover: false }
    }

    fn leftover(order_id: u64) -> GridOp {
        GridOp::Cancel { order_id, leftover: true }
    }

    fn book() -> Vec<Order> {
        vec![
            order(1, OrderType::Bid, "74", "0"),
            order(2, OrderType::Bid, "60", "0"),
            order(3, OrderType::Offer, "101", "0"),
            order(4, OrderType::Offer, "160", "0"),
            order(5, OrderType::Offer, "170", "0"),
        ]
    }

    fn grid() -> (Vec<PriceLevel>, Vec<PriceLevel>) {
        (vec![level(-25.0, "1"), level(-50.0, "2")], vec![level(25.0, "3"), level(50.0, "4")])
    }

    #[test]
    fn grid_ops_leave_keep_sides_untouched() {
        let (bids, offers) = grid();
        assert_eq!(grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Keep, Keep)), Vec::new());
        let ops = grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Keep, Replace));
        assert_eq!(
            ops,
            vec![
                cancel(3),
                place(OrderType::Offer, "125.0000000000", "3"),
                cancel(4),
                place(OrderType::Offer, "150.0000000000", "4"),
                leftover(5),
            ]
        );
    }

    #[test]
    fn grid_ops_pair_each_cancel_within_its_side() {
        // Offer 3 rests nearest the mid, yet the tightest bid replaces bid 1
        let (bids, offers) = grid();
        let ops = grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Replace, Replace));
        assert_eq!(
            ops,
            vec![
                cancel(1),
                place(OrderType::Bid, "75.0000000000", "1"),
                cancel(3),
                place(OrderType::Offer, "125.0000000000", "3"),
                cancel(2),
                place(OrderType::Bid, "50.0000000000", "2"),
                cancel(4),
                place(OrderType::Offer, "150.0000000000", "4"),
                leftover(5),
            ]
        );
    }

    #[test]
    fn grid_ops_withdraw_a_cancel_side_fully() {
        let (bids, offers) = grid();
        let ops = grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Cancel, Keep));
        assert_eq!(ops, vec![leftover(1), leftover(2)]);
        // The withdrawn side goes before any placement on the other side
        let ops = grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Replace, Cancel));
        assert_eq!(
            ops,
            vec![
                leftover(3),
                leftover(4),
                leftover(5),
                cancel(1),
                place(OrderType::Bid, "75.0000000000", "1"),
                cancel(2),
                place(OrderType::Bid, "50.0000000000", "2"),
            ]
        );
        let ops = grid_ops(&bids, &offers, 100.0, 1.0, &book(), (Cancel, Replace));
        assert_eq!(
            ops,
            vec![
                leftover(1),
                leftover(2),
                cancel(3),
                place(OrderType::Offer, "125.0000000000", "3"),
                cancel(4),
                place(OrderType::Offer, "150.0000000000", "4"),
                leftover(5),
            ]
        );
    }

    #[test]
    fn grid_ops_replace_one_side_rounding_away_from_the_mid() {
        // Tick 10: bids round down (75 -> 70), offers round up (125 -> 130)
        let (bids, offers) = grid();
        let one_bid = vec![order(1, OrderType::Bid, "74", "0"), order(3, OrderType::Offer, "101", "0")];
        let ops = grid_ops(&bids, &offers, 100.0, 10.0, &one_bid, (Replace, Keep));
        assert_eq!(
            ops,
            vec![
                cancel(1),
                place(OrderType::Bid, "70.0000000000", "1"),
                place(OrderType::Bid, "50.0000000000", "2"),
            ]
        );
        let ops = grid_ops(&bids, &offers, 100.0, 10.0, &[], (Keep, Replace));
        assert_eq!(
            ops,
            vec![
                place(OrderType::Offer, "130.0000000000", "3"),
                place(OrderType::Offer, "150.0000000000", "4"),
            ]
        );
    }

    #[test]
    fn grid_ops_sort_a_nan_rung_last_without_panicking() {
        // Outermost rung first, enough rungs to pass the small-sort threshold
        let rungs = |sign: f64| -> Vec<PriceLevel> {
            (1..=11).rev().map(|i| level(sign * 0.5 * f64::from(i), "1")).collect()
        };
        for nan_at in 0..22 {
            let (mut bids, mut offers) = (rungs(-1.0), rungs(1.0));
            let side = if nan_at < 11 { &mut bids } else { &mut offers };
            side[nan_at % 11].delta_percent = f64::NAN;
            let ops = grid_ops(&bids, &offers, 100.0, 0.01, &[], (Replace, Replace));
            assert_eq!(ops.len(), 22);
            assert!(
                matches!(ops.last(), Some(GridOp::Place { price, .. }) if price == "NaN"),
                "NaN rung at {nan_at} not placed last"
            );
        }
    }

    #[test]
    fn side_queue_sorts_an_unparseable_distance_last() {
        // Farthest first, enough orders to pass the small-sort threshold
        for nan_at in 0..24u64 {
            let orders: Vec<Order> = (0..24u64)
                .map(|id| {
                    let price = if id == nan_at { "NaN".to_string() } else { format!("{}", 76 + id) };
                    order(id, OrderType::Bid, &price, "0")
                })
                .collect();
            let queue = Vec::from(side_queue(&orders, OrderType::Bid, 100.0));
            assert_eq!(queue.len(), 24);
            assert_eq!(queue.last(), Some(&nan_at), "NaN order at {nan_at}");
        }
    }

    // ---- uncross ----

    fn readme_market() -> MarketConfig {
        let bids = vec![level(-0.015, "10"), level(-0.2, "10"), level(-0.3, "10")];
        let offers = vec![level(0.015, "10"), level(0.2, "10"), level(0.3, "10")];
        MarketConfig { price_change_threshold_percent: 0.1, ..market("CC-USDC", bids, offers) }
    }

    /// One side's orders as a refresh at `anchor` placed them.
    fn placed_at(m: &MarketConfig, side: OrderType, anchor: f64, tick: f64, first_id: u64) -> Vec<Order> {
        let plan = if side == OrderType::Bid { (Replace, Keep) } else { (Keep, Replace) };
        grid_ops(&m.bid_levels, &m.offer_levels, anchor, tick, &[], plan)
            .into_iter()
            .zip(first_id..)
            .filter_map(|(op, id)| match op {
                GridOp::Place { side, price, .. } => Some(order(id, side, &price, "0")),
                GridOp::Cancel { .. } => None,
            })
            .collect()
    }

    /// (plan, plan after uncross) for a visit at `price` over a full book placed at `anchors`.
    fn visit_plans(
        m: &MarketConfig,
        anchors: SideAnchors,
        price: f64,
        fundable: (bool, bool),
    ) -> ((SideAction, SideAction), (SideAction, SideAction)) {
        let tick = 0.0001;
        let mut book = placed_at(m, OrderType::Bid, anchors.bid.unwrap(), tick, 1);
        book.extend(placed_at(m, OrderType::Offer, anchors.offer.unwrap(), tick, 10));
        let threshold = m.price_change_threshold_percent;
        let i = PlanInput {
            force_bid: moved_past(anchors.bid, price, threshold).is_some(),
            force_offer: moved_past(anchors.offer, price, threshold).is_some(),
            ..input(Funded, Funded, side_counts(&book))
        };
        let plan = plan_market(&i);
        let after = uncross(plan, &m.bid_levels, &m.offer_levels, price, tick, &book, fundable);
        (plan, after)
    }

    #[test]
    fn a_one_sided_refresh_into_the_kept_side_refreshes_both() {
        let m = readme_market();
        // Bids last placed at 99.95, offers at 100.0: new bids at 100.05 reach the resting offers
        let anchors = SideAnchors { bid: Some(99.95), offer: Some(100.0) };
        assert_eq!(visit_plans(&m, anchors, 100.05, (true, true)), ((Replace, Keep), (Replace, Replace)));
        // Mirror: new offers at 99.95 reach the bids resting from 100.0
        let anchors = SideAnchors { bid: Some(100.0), offer: Some(100.06) };
        assert_eq!(visit_plans(&m, anchors, 99.95, (true, true)), ((Keep, Replace), (Replace, Replace)));
        // A kept side that cannot be funded is withdrawn instead
        let anchors = SideAnchors { bid: Some(99.95), offer: Some(100.0) };
        assert_eq!(visit_plans(&m, anchors, 100.05, (true, false)), ((Replace, Keep), (Replace, Cancel)));
    }

    #[test]
    fn a_one_sided_refresh_clear_of_the_kept_side_stays_one_sided() {
        let m = readme_market();
        let anchors = SideAnchors { bid: Some(100.1), offer: Some(100.0) };
        assert_eq!(visit_plans(&m, anchors, 99.99, (true, true)), ((Replace, Keep), (Replace, Keep)));
        // Nothing resting on the kept side: nothing to cross
        let (bids, offers) = (m.bid_levels.clone(), m.offer_levels.clone());
        let bids_only = placed_at(&m, OrderType::Bid, 100.0, 0.0001, 1);
        assert_eq!(uncross((Keep, Replace), &bids, &offers, 200.0, 0.0001, &[], (true, true)), (Keep, Replace));
        assert_eq!(uncross((Replace, Keep), &bids, &offers, 200.0, 0.0001, &bids_only, (true, true)), (Replace, Keep));
        // Two-sided and no-action plans are left as they are
        for plan in [(Replace, Replace), (Keep, Keep), (Replace, Cancel)] {
            assert_eq!(uncross(plan, &bids, &offers, 200.0, 0.0001, &bids_only, (true, true)), plan);
        }
    }

    // ---- submit failures ----

    #[test]
    fn only_ambiguous_submit_failures_may_have_been_booked() {
        use crate::client::submit_order_error;
        use tonic::Status;
        let refused = [
            Status::invalid_argument("x"),
            Status::not_found("x"),
            Status::already_exists("x"),
            Status::unauthenticated("x"),
            Status::failed_precondition("x"),
        ];
        for status in refused {
            let code = status.code();
            let e = submit_order_error(status);
            assert_eq!(e.to_string(), "submit_order failed: x");
            assert!(!submit_outcome_unknown(&e), "{code:?}");
        }
        for status in [Status::deadline_exceeded("x"), Status::unavailable("x"), Status::internal("x")] {
            let code = status.code();
            assert!(submit_outcome_unknown(&submit_order_error(status)), "{code:?}");
        }
        assert!(submit_outcome_unknown(&anyhow::anyhow!("x")));
    }

    // ---- can_park ----

    #[test]
    fn can_park_truth_table() {
        let fresh = Some(Duration::from_secs(10));
        let empty = Some(SideCounts::default());
        let short = funding(Short, Short);
        assert!(can_park(&short, 3, 3, empty, fresh));

        // Something rested, or no no-action visit yet
        assert!(!can_park(&short, 3, 3, None, fresh));
        assert!(!can_park(&short, 3, 3, Some(resting(0, 1)), fresh));
        // Probe age: never probed, or due again
        assert!(!can_park(&short, 3, 3, empty, None));
        assert!(!can_park(&short, 3, 3, empty, Some(UNFUNDED_PROBE)));
        assert!(can_park(&short, 3, 3, empty, Some(UNFUNDED_PROBE - Duration::from_millis(1))));
        // Unknown never parks
        assert!(!can_park(&funding(Unknown, Short), 3, 3, empty, fresh));
        assert!(!can_park(&funding(Short, Unknown), 0, 3, empty, fresh));
        // A side without rungs counts as unfundable; a fundable one does not
        assert!(can_park(&funding(Funded, Short), 0, 3, empty, fresh));
        assert!(!can_park(&funding(Funded, Short), 3, 3, empty, fresh));
        assert!(!can_park(&funding(Short, Funded), 3, 3, empty, fresh));
    }

    // ---- side_counts / side_queue ----

    fn order(order_id: u64, order_type: OrderType, price: &str, filled: &str) -> Order {
        Order {
            order_id,
            order_type: order_type as i32,
            price: price.to_string(),
            filled_quantity: filled.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn side_counts_split_by_order_type() {
        let orders = vec![
            order(1, OrderType::Bid, "99", "0"),
            order(2, OrderType::Bid, "98", "0.5"),
            order(3, OrderType::Offer, "101", "0"),
            order(4, OrderType::Offer, "102", ""),
            order(5, OrderType::Offer, "103", "junk"),
            order(6, OrderType::Unspecified, "100", "1"),
        ];
        let c = side_counts(&orders);
        assert_eq!(
            c,
            SideCounts { bids: 2, offers: 3, bid_partial: true, offer_partial: false }
        );
        assert_eq!(side_counts(&[]), SideCounts::default());
    }

    #[test]
    fn side_queue_keeps_to_one_side_nearest_first() {
        let orders = vec![
            order(1, OrderType::Bid, "97", "0"),
            order(2, OrderType::Offer, "101", "0"),
            order(3, OrderType::Bid, "99", "0"),
            order(4, OrderType::Offer, "103", "0"),
        ];
        assert_eq!(Vec::from(side_queue(&orders, OrderType::Bid, 100.0)), vec![3, 1]);
        assert_eq!(Vec::from(side_queue(&orders, OrderType::Offer, 100.0)), vec![2, 4]);
    }

    // ---- deadline ----

    #[test]
    fn soft_deadline_always_allows_the_first_market() {
        let now = Instant::now();
        let later = deadline_after(Duration::from_secs(60));
        assert!(!past_soft_deadline(0, now, now));
        assert!(past_soft_deadline(1, now, now));
        assert!(!past_soft_deadline(5, now, later));
    }

    #[test]
    fn an_unrepresentable_soft_deadline_never_passes() {
        let d = deadline_after(Duration::MAX);
        assert!(d > Instant::now());
        assert!(!past_soft_deadline(5, Instant::now(), d));
    }

    // ---- OrderManager against a closed local port (every RPC fails fast) ----

    fn closed_port_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    fn manager_at(url: String, markets: Vec<MarketConfig>) -> OrderManager {
        let mut config = BaseConfig::test_minimal();
        config.orderbook_grpc_url = url;
        config.markets = markets;
        let client = OrderbookClient::lazy_for_tests(&config).unwrap();
        let tracker = OrderTracker::new(0, Secret::seal(&mut [7u8; 32]));
        OrderManager::new(config, client, Arc::new(Mutex::new(tracker)))
    }

    fn offline_manager(markets: Vec<MarketConfig>) -> OrderManager {
        manager_at(closed_port_url(), markets)
    }

    /// Accepts TCP in the kernel backlog but never answers HTTP/2, so any RPC hangs.
    fn silent_listener() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, format!("http://127.0.0.1:{port}"))
    }

    fn probed(om: &OrderManager, market_id: &str) -> u32 {
        om.price_fail_streak.get(market_id).copied().unwrap_or(0)
    }

    #[tokio::test]
    async fn update_cycle_stops_at_the_soft_deadline_and_resumes_there() {
        let ids = ["AAA-USD", "BBB-USD", "CCC-USD"];
        let markets = ids.iter().map(|id| market(id, ladder(-1.0, "1"), ladder(1.0, "1"))).collect();
        let mut om = offline_manager(markets);
        let stop = Shutdown::new();
        let probes = |om: &OrderManager| ids.map(|id| probed(om, id));

        // Deadline already passed: one market, the rest deferred
        let r = om.update_cycle(&stop, Instant::now()).await.unwrap();
        assert_eq!((r.markets, r.deferred), (1, 2));
        assert_eq!(probes(&om), [1, 0, 0]);

        // The next cycle resumes at the first deferred market
        let r = om.update_cycle(&stop, Instant::now()).await.unwrap();
        assert_eq!((r.markets, r.deferred), (1, 2));
        assert_eq!(probes(&om), [1, 1, 0]);

        // A roomy deadline finishes the rotation, wrapping around
        let r = om.update_cycle(&stop, deadline_after(Duration::from_secs(30))).await.unwrap();
        assert_eq!((r.markets, r.deferred), (3, 0));
        assert_eq!(probes(&om), [2, 2, 1]);
        assert_eq!(om.resume_from, 0);
    }

    #[tokio::test]
    async fn update_cycle_stops_on_shutdown_before_any_market() {
        let markets = vec![market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"))];
        let mut om = offline_manager(markets);
        let stop = Shutdown::new();
        stop.signal();
        let r = om.update_cycle(&stop, deadline_after(Duration::from_secs(30))).await.unwrap();
        assert_eq!((r.markets, r.deferred), (0, 1));
        assert_eq!(probed(&om, "AAA-USD"), 0);
    }

    #[tokio::test]
    async fn markets_without_rungs_and_parked_markets_cost_no_rpc() {
        let spye = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let mut om = offline_manager(vec![market("NONE-USD", Vec::new(), Vec::new()), spye]);
        om.set_balances(vec![cc("100"), token("SPYe", "0.001"), token("USDCx", "1")]);
        om.last_seen_price.insert("SPYe-USDCx".to_string(), 700.0);
        let probed_at = Instant::now();
        for id in ["NONE-USD", "SPYe-USDCx"] {
            om.last_resting.insert(id.to_string(), SideCounts::default());
            om.last_probe.insert(id.to_string(), probed_at);
        }
        let stop = Shutdown::new();
        let deadline = || deadline_after(Duration::from_secs(30));

        let r = om.update_cycle(&stop, deadline()).await.unwrap();
        assert_eq!((r.markets, r.no_levels, r.parked), (2, 1, 1));
        assert_eq!(probed(&om, "NONE-USD") + probed(&om, "SPYe-USDCx"), 0);
        assert_eq!(om.last_probe.get("NONE-USD"), Some(&probed_at), "no listing for NONE-USD");
        assert!(om.parked.contains("SPYe-USDCx"));

        // Due for a re-probe: the price is fetched again
        let stale = Instant::now().checked_sub(UNFUNDED_PROBE + Duration::from_secs(1));
        if let Some(stale) = stale {
            om.last_probe.insert("SPYe-USDCx".to_string(), stale);
            let r = om.update_cycle(&stop, deadline()).await.unwrap();
            assert_eq!(r.parked, 0);
            assert_eq!(probed(&om, "SPYe-USDCx"), 1);
        }
    }

    // Orders left resting in a market without rungs are cancelled on its first visit
    #[tokio::test]
    async fn a_market_without_rungs_cancels_its_resting_orders() {
        let none = market("NONE-USD", Vec::new(), Vec::new());
        let mut om = offline_manager(vec![none.clone()]);
        om.stub_book(100.0, vec![order(1, OrderType::Bid, "99", "0")]);
        om.stub_cancels = Some(Vec::new());
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        let visit = om.visit_market(&none, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), visit).await.unwrap();
        assert_eq!((report.no_levels, report.cancelled), (1, 1));
        assert_eq!(om.stub_cancels.as_deref(), Some(&[1u64][..]));
        assert_eq!(om.last_resting.get("NONE-USD"), Some(&SideCounts::default()));
    }

    /// The cycle must be spawnable on the multi-threaded runtime.
    #[test]
    fn update_cycle_future_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        fn check(om: &mut OrderManager, stop: &Shutdown) {
            let cycle = om.update_cycle(stop, Instant::now());
            assert_send(&cycle);
        }
        let _ = check;
    }

    #[tokio::test]
    async fn placement_errors_release_the_guard() {
        use crate::order_tracker::VerifyResult;
        let mut om = offline_manager(Vec::new());

        // Signing fails before any RPC: nothing is left that holds a match
        om.tracker.lock().await.set_fail_signing(true);
        assert!(om.place_bid("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(!om.tracker.lock().await.placement_in_flight("AAA-USD"));
        assert!(om.place_offer("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(!om.tracker.lock().await.placement_in_flight("AAA-USD"));
        assert!(matches!(
            om.tracker.lock().await.verify_settlement(&proposal_for("AAA-USD", 42), "our-party"),
            VerifyResult::NeedServerLookup { order_id: 42 }
        ));

        // Submit fails at the transport
        om.tracker.lock().await.set_fail_signing(false);
        assert!(om.place_bid("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(!om.tracker.lock().await.placement_in_flight("AAA-USD"));
        assert!(om.place_offer("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(!om.tracker.lock().await.placement_in_flight("AAA-USD"));
    }

    fn proposal_for(market_id: &str, offer_order_id: u64) -> orderbook_proto::orderbook::SettlementProposal {
        orderbook_proto::orderbook::SettlementProposal {
            market_id: market_id.to_string(),
            buyer: "counterparty".to_string(),
            seller: "our-party".to_string(),
            base_quantity: "1".to_string(),
            order_match: Some(orderbook_proto::orderbook::OrderMatch {
                offer_order_id,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // A submit that errored may still be booked; its market's untracked matches are held
    #[tokio::test]
    async fn failed_submit_holds_matching_proposals_in_its_market() {
        use crate::order_tracker::VerifyResult;
        let mut om = offline_manager(Vec::new());
        let held = |om: &OrderManager, market: &str| {
            let tracker = om.tracker.try_lock().unwrap();
            matches!(
                tracker.verify_settlement(&proposal_for(market, 42), "our-party"),
                VerifyResult::PlacementInFlight { order_id: 42 }
            )
        };

        // Signing fails before any RPC: nothing is held
        om.tracker.lock().await.set_fail_signing(true);
        assert!(om.place_bid("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(!held(&om, "AAA-USD"));
        om.tracker.lock().await.set_fail_signing(false);

        assert!(om.place_bid("AAA-USD", "1.0", "1.0", None).await.is_err());
        assert!(held(&om, "AAA-USD"));
        assert!(!held(&om, "BBB-USD"));
        assert!(om.place_offer("BBB-USD", "1.0", "1.0", None).await.is_err());
        assert!(held(&om, "BBB-USD"));
        assert!(!om.tracker.lock().await.placement_in_flight("BBB-USD"));
    }

    // A priceless teardown tracks a booked failed submit before cancelling it
    #[tokio::test]
    async fn priceless_teardown_adopts_a_booked_failed_submit() {
        use crate::order_tracker::VerifyResult;
        let mut om = offline_manager(Vec::new());
        assert!(om.place_offer("AAA-USD", "1.0", "1.0", None).await.is_err());
        let submit = om.tracker.lock().await.failed_submits().pop().unwrap();
        om.stub_orders = Some(vec![Order {
            order_id: 42,
            market_id: "AAA-USD".to_string(),
            order_type: OrderType::Offer as i32,
            nonce: submit.nonce,
            signature: Some(submit.signature.clone()),
            signed_data: submit.signed_data.clone(),
            ..Default::default()
        }]);
        om.handle_priceless("AAA-USD", "test").await;
        let mut tracker = om.tracker.lock().await;
        tracker.expire_submit_holds();
        let verdict = tracker.verify_settlement(&proposal_for("AAA-USD", 42), "our-party");
        assert!(matches!(verdict, VerifyResult::Accepted { order_id: 42 }));
    }

    // The guard must stay alive across the submit await, for both sides
    #[tokio::test]
    async fn placement_guard_is_held_during_submit() {
        use crate::order_tracker::VerifyResult;
        for offer in [false, true] {
            let (listener, url) = silent_listener();
            let mut om = manager_at(url, Vec::new());
            let tracker = om.tracker.clone();
            let task = tokio::spawn(async move {
                let r = if offer {
                    om.place_offer("AAA-USD", "1.0", "1.0", None).await
                } else {
                    om.place_bid("AAA-USD", "1.0", "1.0", None).await
                };
                (om, r)
            });
            let mut seen = false;
            let until = Instant::now() + Duration::from_secs(10);
            while !seen && Instant::now() < until {
                let t = tracker.lock().await;
                if t.placement_in_flight("AAA-USD") {
                    // A match on the order being submitted is held, not looked up
                    let verdict = t.verify_settlement(&proposal_for("AAA-USD", 42), "our-party");
                    assert!(matches!(verdict, VerifyResult::PlacementInFlight { order_id: 42 }));
                    seen = true;
                } else {
                    drop(t);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            assert!(seen, "guard held during submit (offer={offer})");
            drop(listener);
            let (_om, r) = task.await.unwrap();
            assert!(r.is_err());
            assert!(!tracker.lock().await.placement_in_flight("AAA-USD"));
        }
    }

    // After shutdown no further place or cancel is issued; any RPC here would hang
    #[tokio::test]
    async fn execute_plan_issues_no_rpc_after_shutdown() {
        let (_listener, url) = silent_listener();
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = manager_at(url, vec![aaa.clone()]);
        let resting = vec![order(1, OrderType::Bid, "99", "0"), order(2, OrderType::Offer, "101", "0")];
        let stop = Shutdown::new();
        stop.signal();

        // Uncached tick size first: the lookup would be an RPC too
        for cached_tick in [false, true] {
            if cached_tick {
                om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
            }
            for plan in [(SideAction::Replace, SideAction::Replace), (SideAction::Cancel, SideAction::Cancel)] {
                let mut report = CycleReport::default();
                let run = om.execute_plan(&aaa, 100.0, &resting, &aaa.offer_levels, plan, &stop, &mut report);
                assert!(tokio::time::timeout(Duration::from_secs(2), run).await.is_ok(), "{plan:?} {cached_tick}");
                assert_eq!((report.placed, report.cancelled), (0, 0));
                assert!(!om.tracker.lock().await.placement_in_flight("AAA-USD"));
            }
        }
    }

    // Replaced offers are placed from the set passed in, which becomes the shape baseline once placed
    #[tokio::test]
    async fn execute_plan_places_the_given_offer_set() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        let resting = vec![order(1, OrderType::Bid, "99", "0"), order(2, OrderType::Offer, "101", "0")];
        let shaped = vec![level(2.0, "0.7")];
        let mut report = CycleReport::default();
        let stop = Shutdown::new();
        let run = om.execute_plan(&aaa, 100.0, &resting, &shaped, (Keep, Replace), &stop, &mut report);
        let failed = tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        assert_eq!(failed, [2]);
        let submits = om.tracker.lock().await.failed_submits();
        let placed: Vec<(i32, &str)> = submits.iter().map(|s| (s.order_type, s.quantity.as_str())).collect();
        assert_eq!(placed, [(OrderType::Offer as i32, "0.7")]);
        assert!(!om.last_shaped_offers.contains_key("AAA-USD"), "nothing placed, no baseline");

        om.stub_submit = Some(Ok(9));
        let run = om.execute_plan(&aaa, 100.0, &resting, &shaped, (Keep, Replace), &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        assert_eq!(report.placed, 1);
        let baseline = om.last_shaped_offers.get("AAA-USD").unwrap();
        let baseline: Vec<(f64, &str)> = baseline.iter().map(|l| (l.delta_percent, l.quantity.as_str())).collect();
        assert_eq!(baseline, [(2.0, "0.7")]);
    }

    #[test]
    fn replaced_sides_take_the_price_they_were_placed_at() {
        let prev = SideAnchors { bid: Some(100.0), offer: Some(100.0) };
        let next = next_anchors(prev, (Replace, Replace), resting(4, 3), 98.9);
        assert_eq!(next, SideAnchors { bid: Some(98.9), offer: Some(98.9) });
    }

    // A side re-placed at a new price is anchored there even when one of its cancels failed
    #[tokio::test]
    async fn a_visit_anchors_a_replaced_side_whose_cancel_failed() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.set_balances(vec![cc("100"), token("AAA", "10"), token("USD", "1000")]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: Some(100.0), offer: Some(100.0) });
        let resting = vec![
            order(1, OrderType::Bid, "99.5", "0"),
            order(2, OrderType::Bid, "99", "0"),
            order(3, OrderType::Offer, "100.5", "0"),
            order(4, OrderType::Offer, "101", "0"),
            order(5, OrderType::Offer, "101.5", "0"),
        ];
        om.stub_book(100.5, resting);

        // A bid rung is gone: bids are re-placed at 100.5 while their cancels fail
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        let visit = om.visit_market(&aaa, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), visit).await.unwrap();
        assert_eq!((report.refreshed, report.cancelled), (1, 0));
        let anchors = om.grid_anchors.get("AAA-USD").copied();
        assert_eq!(anchors, Some(SideAnchors { bid: Some(100.5), offer: Some(100.0) }));
    }

    // A side's pause and resume are each logged once, however many cycles repeat them
    #[tokio::test]
    async fn short_side_transitions_log_once_each() {
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::INFO);
        let mut om = offline_manager(Vec::new());
        let spye = "SPYe-USDCx";

        for _ in 0..3 {
            om.note_funding(spye, &funding(Funded, Short));
        }
        assert_eq!(logs.count("Grid SPYe-USDCx: offers paused, SPYe"), 1);

        // Unknown funding neither resumes nor re-pauses a side
        om.note_funding(spye, &funding(Unknown, Unknown));
        assert_eq!(logs.count("offers resumed"), 0);
        for _ in 0..3 {
            om.note_funding(spye, &funding(Funded, Funded));
        }
        assert_eq!(logs.count("Grid SPYe-USDCx: offers resumed"), 1);

        om.note_funding(spye, &funding(Funded, Short));
        assert_eq!(logs.count("offers paused"), 2);
        assert_eq!(logs.count("bids"), 0, "a side that stays funded never logs");
    }

    // A restore refreshes both sides once, even with every rung resting at an unmoved price
    #[tokio::test]
    async fn a_price_restore_forces_one_refresh_of_both_sides() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        let funded = funding(Funded, Funded);
        let plan = |om: &mut OrderManager| om.plan_visit(&aaa, 100.0, &funded, 3, resting(3, 3));

        // The teardown cleared the anchors; orders whose cancel failed still rest
        om.priceless.insert("AAA-USD".to_string());
        assert_eq!(plan(&mut om).plan, (Keep, Keep));

        // The first good price only counts toward the restore
        assert_eq!(om.accept_price("AAA-USD", 100.0), None);
        assert!(om.priceless.contains("AAA-USD"));
        assert!(!om.force_refresh.contains("AAA-USD"));
        assert_eq!(om.accept_price("AAA-USD", 100.0), Some(100.0));
        assert!(!om.priceless.contains("AAA-USD"));

        let visit = plan(&mut om);
        assert_eq!(visit.plan, (Replace, Replace));
        assert!(visit.force_bid && visit.force_offer);

        // The visit that planned the restore consumed it
        assert!(!om.force_refresh.contains("AAA-USD"));
        assert_eq!(plan(&mut om).plan, (Keep, Keep));
    }

    // A move past one side's anchor refreshes that side only, and restamps only it
    #[tokio::test]
    async fn plan_visit_forces_only_the_side_whose_anchor_the_price_left() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        let funded = funding(Funded, Funded);

        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: Some(100.0), offer: Some(95.0) });
        let v = om.plan_visit(&aaa, 99.5, &funded, 3, resting(3, 3));
        assert_eq!(v.plan, (Keep, Replace));
        assert!(!v.force_bid && v.force_offer);
        assert_eq!(v.anchored, SideAnchors { bid: Some(100.0), offer: Some(99.5) });

        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: Some(95.0), offer: Some(100.0) });
        let v = om.plan_visit(&aaa, 99.5, &funded, 3, resting(3, 3));
        assert_eq!(v.plan, (Replace, Keep));
        assert!(v.force_bid && !v.force_offer);
        assert_eq!(v.anchored, SideAnchors { bid: Some(99.5), offer: Some(100.0) });
    }

    #[tokio::test]
    async fn a_visit_stamps_the_anchors_of_the_plan_it_carried_out() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        let funded = funding(Funded, Funded);
        let anchors = |om: &OrderManager| om.grid_anchors.get("AAA-USD").copied();
        let before = SideAnchors { bid: Some(100.0), offer: Some(95.0) };

        // Offers only
        om.grid_anchors.insert("AAA-USD".into(), before);
        let v = om.plan_visit(&aaa, 99.5, &funded, 3, resting(3, 3));
        om.stamp_anchors("AAA-USD", &v, v.plan, resting(3, 3), 99.5);
        assert_eq!(anchors(&om), Some(SideAnchors { bid: Some(100.0), offer: Some(99.5) }));

        // Escalated to both sides by the uncross check
        om.grid_anchors.insert("AAA-USD".into(), before);
        let v = om.plan_visit(&aaa, 99.5, &funded, 3, resting(3, 3));
        om.stamp_anchors("AAA-USD", &v, (Replace, Replace), resting(3, 3), 99.5);
        assert_eq!(anchors(&om), Some(SideAnchors { bid: Some(99.5), offer: Some(99.5) }));

        // No action: a resting side without an anchor adopts the price, the other keeps its own
        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: None, offer: Some(99.0) });
        let v = om.plan_visit(&aaa, 99.5, &funded, 3, resting(3, 3));
        assert_eq!(v.plan, (Keep, Keep));
        om.stamp_anchors("AAA-USD", &v, v.plan, resting(3, 3), 99.5);
        assert_eq!(anchors(&om), Some(SideAnchors { bid: Some(99.5), offer: Some(99.0) }));
    }

    // Cancels that failed are retried on the next visit, even with every rung count unchanged
    #[tokio::test]
    async fn failed_cancels_are_retried_on_the_next_visit() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.set_balances(vec![cc("100"), token("AAA", "10"), token("USD", "1000")]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: Some(100.0), offer: Some(100.0) });
        let book = vec![
            order(1, OrderType::Bid, "99.5", "0"),
            order(2, OrderType::Bid, "99", "0"),
            order(3, OrderType::Bid, "98.5", "0"),
            order(4, OrderType::Offer, "100.5", "0"),
            order(5, OrderType::Offer, "101", "0"),
            order(6, OrderType::Offer, "101.5", "0"),
        ];
        om.stub_book(98.9, book);
        let stop = Shutdown::new();
        let sorted = |ids: Vec<u64>| {
            let mut ids = ids;
            ids.sort_unstable();
            ids
        };

        // Both sides moved past their anchors; every cancel and place fails
        let mut report = CycleReport::default();
        let visit = om.visit_market(&aaa, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), visit).await.unwrap();
        assert_eq!((report.refreshed, report.cancelled, report.placed), (1, 0, 0));
        let pending = om.pending_cancels.get("AAA-USD").cloned().unwrap_or_default();
        assert_eq!(sorted(pending.into_iter().collect()), [1, 2, 3, 4, 5, 6]);

        // The same book is listed again: the old orders go and both sides are re-placed
        om.stub_cancels = Some(Vec::new());
        om.stub_submit = Some(Ok(77));
        let mut report = CycleReport::default();
        let visit = om.visit_market(&aaa, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), visit).await.unwrap();
        assert_eq!(sorted(om.stub_cancels.clone().unwrap_or_default()), [1, 2, 3, 4, 5, 6]);
        assert_eq!((report.refreshed, report.cancelled, report.placed), (1, 6, 6));
        assert!(!om.pending_cancels.contains_key("AAA-USD"));
    }

    // A failed cancel no longer listed is dropped; a teardown clears the rest
    #[tokio::test]
    async fn failed_cancels_are_dropped_once_unlisted_or_torn_down() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.stub_cancels = Some(Vec::new());
        om.pending_cancels.insert("AAA-USD".into(), HashSet::from([1, 2]));
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        let listed = vec![order(2, OrderType::Bid, "99", "0"), order(3, OrderType::Bid, "98", "0")];
        let left = om.retry_failed_cancels("AAA-USD", listed, &stop, &mut report).await;
        assert_eq!(left.iter().map(|o| o.order_id).collect::<Vec<_>>(), [3]);
        assert_eq!(om.stub_cancels.as_deref(), Some(&[2u64][..]));
        assert!(!om.pending_cancels.contains_key("AAA-USD"));

        om.stub_orders = Some(Vec::new());
        om.pending_cancels.insert("AAA-USD".into(), HashSet::from([4]));
        om.handle_priceless("AAA-USD", "test").await;
        assert!(!om.pending_cancels.contains_key("AAA-USD"));
    }

    // A shutdown seen after a cancel issues none of the market's remaining ops
    #[tokio::test]
    async fn execute_plan_stops_between_ops_on_shutdown() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        let stop = Shutdown::new();
        om.stub_cancels = Some(Vec::new());
        om.stop_on_cancel = Some(stop.clone());
        let resting = vec![order(1, OrderType::Bid, "99", "0"), order(2, OrderType::Bid, "98", "0")];
        let mut report = CycleReport::default();
        let run = om.execute_plan(&aaa, 100.0, &resting, &aaa.offer_levels, (Replace, Keep), &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        assert_eq!(om.stub_cancels.as_deref(), Some(&[1u64][..]));
        assert_eq!(report.placed, 0);
        assert!(om.tracker.lock().await.failed_submits().is_empty());
    }

    // Withdrawing a market without rungs stops between cancels on shutdown
    #[tokio::test]
    async fn withdrawing_an_unquoted_market_stops_between_cancels_on_shutdown() {
        let none = market("NONE-USD", Vec::new(), Vec::new());
        let mut om = offline_manager(vec![none.clone()]);
        om.stub_book(100.0, vec![order(1, OrderType::Bid, "99", "0"), order(2, OrderType::Offer, "101", "0")]);
        om.stub_cancels = Some(Vec::new());
        let stop = Shutdown::new();
        om.stop_on_cancel = Some(stop.clone());
        let mut report = CycleReport::default();
        let visit = om.visit_market(&none, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), visit).await.unwrap();
        assert_eq!(om.stub_cancels.as_deref(), Some(&[1u64][..]));
        assert!(!om.last_resting.contains_key("NONE-USD"));
    }

    // A shutdown seen at the price fetch lists no orders; a listing here would hang
    #[tokio::test]
    async fn a_visit_lists_no_orders_after_shutdown_at_the_price() {
        let (_listener, url) = silent_listener();
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = manager_at(url, vec![aaa.clone()]);
        om.set_balances(vec![cc("100"), token("AAA", "10"), token("USD", "1000")]);
        om.stub_price = Some(100.0);
        let stop = Shutdown::new();
        om.stop_on_price = Some(stop.clone());
        let mut report = CycleReport::default();
        let visit = om.visit_market(&aaa, &stop, &mut report);
        assert!(tokio::time::timeout(Duration::from_secs(2), visit).await.is_ok());
        assert!(stop.is_shutting_down());
    }

    async fn place_one(om: &mut OrderManager, offer: bool) -> Result<u64> {
        if offer {
            om.place_offer("AAA-USD", "1.0", "1.0", None).await
        } else {
            om.place_bid("AAA-USD", "1.0", "1.0", None).await
        }
    }

    // Only a submit whose outcome is unknown holds the market's matches
    #[tokio::test]
    async fn a_refused_submit_opens_no_hold() {
        use crate::order_tracker::VerifyResult;
        use tonic::Status;
        for offer in [false, true] {
            let side = if offer { OrderType::Offer } else { OrderType::Bid };
            let mut om = offline_manager(Vec::new());

            om.stub_submit = Some(Err(Status::invalid_argument("x")));
            assert!(place_one(&mut om, offer).await.is_err());
            {
                let tracker = om.tracker.lock().await;
                assert!(tracker.failed_submits().is_empty(), "offer={offer}");
                assert!(!tracker.placement_in_flight("AAA-USD"));
                let verdict = tracker.verify_settlement(&proposal_for("AAA-USD", 42), "our-party");
                assert!(matches!(verdict, VerifyResult::NeedServerLookup { order_id: 42 }), "offer={offer}");
            }

            om.stub_submit = Some(Err(Status::unavailable("x")));
            assert!(place_one(&mut om, offer).await.is_err());
            let tracker = om.tracker.lock().await;
            let submits = tracker.failed_submits();
            assert_eq!(submits.len(), 1, "offer={offer}");
            assert_eq!((submits[0].market_id.as_str(), submits[0].order_type), ("AAA-USD", side as i32));
            let verdict = tracker.verify_settlement(&proposal_for("AAA-USD", 42), "our-party");
            assert!(matches!(verdict, VerifyResult::PlacementInFlight { order_id: 42 }), "offer={offer}");
        }
    }

    #[tokio::test]
    async fn losing_the_price_clears_both_anchors() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.grid_anchors.insert("AAA-USD".into(), SideAnchors { bid: Some(100.0), offer: Some(95.0) });
        om.handle_priceless("AAA-USD", "test").await;
        let v = om.plan_visit(&aaa, 99.5, &funding(Funded, Funded), 3, resting(3, 3));
        assert_eq!(v.anchors, SideAnchors::default());
        assert!(!v.force_bid && !v.force_offer);
    }
}
