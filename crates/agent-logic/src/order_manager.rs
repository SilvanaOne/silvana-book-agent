//! Order management for the orderbook agent
//!
//! Handles order placement and cancellation via the orderbook service.
//! All orders are signed and tracked for settlement verification.

use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use orderbook_proto::orderbook::{Order, OrderType, SubmitOrderResponse, TimeInForce};
use orderbook_proto::ledger::TokenBalance;

use crate::client::OrderbookClient;
use crate::config::{BaseConfig, MarketConfig, PriceLevel};
use crate::net_position::NetPositionTracker;
use crate::order_tracker::{FailedSubmit, MarketActivity, OrderTracker, PlacementGuard};
use crate::pool_impact::{self, ImpactSide, PoolDepth};
use crate::shutdown::Shutdown;
use crate::state::{Held, StrategyStarts};
use crate::strategy::book::{book_top, BookTop};
use crate::strategy::gate::{GateInputs, Verdict};
use crate::strategy::report::{format_report, ReportLine};
use crate::strategy::{
    sizing, BookNeed, CapState, Decide, Decision, Inventory, Ladder, MarketView, SettleCap, Start, StrategyRuntime,
    StrategyState, TakeTif, Want, REPORT_EVERY,
};

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
    FeeCapacity,
}

/// What the grid's fee prerequisite reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FeeHeadroom {
    /// No prepaid figure is fed: wallet CC must cover the fee reserve.
    Wallet,
    /// Prepaid fee capacity in CC (`None` while unknown or stale) and what one fill needs.
    Capacity { capacity: Option<f64>, per_fill: f64 },
}

impl FeeHeadroom {
    /// This headroom for a market whose fill needs `need` (when a fee was seen there).
    fn for_market(self, need: Option<f64>) -> Self {
        match (self, need) {
            (Self::Capacity { capacity, .. }, Some(per_fill)) => Self::Capacity { capacity, per_fill },
            (headroom, _) => headroom,
        }
    }

    /// Whether the fees of one fill can be paid.
    fn covers_fees(self, balances: &[TokenBalance], fee_reserve_cc: f64) -> bool {
        match self {
            Self::Wallet => unlocked_cc(balances) >= fee_reserve_cc,
            Self::Capacity { capacity, per_fill } => capacity.is_some_and(|c| c > 0.0 && c >= per_fill),
        }
    }
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
    /// Fee prerequisite: wallet CC until a prepaid capacity is fed.
    fee_headroom: FeeHeadroom,
    /// One fill's fee need per market where a fee was seen; others use the headroom's own.
    fill_needs: HashMap<String, f64>,
    /// Rate limit for the missing-order-id warning.
    order_ref_warned_at: Option<Instant>,
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
    /// Bid ladder at the last placement, per market (offers: `last_shaped_offers`).
    last_bids: HashMap<String, Vec<PriceLevel>>,
    /// Server minimum order size per market, at the price it was fetched for.
    min_base: HashMap<String, MinBase>,
    /// Grid rungs (offer side, index) left out for the minimum order value, per market.
    min_dropped: HashMap<String, HashSet<(bool, usize)>>,
    /// Server timestamp (unix seconds) of the last price per market.
    price_stamps: HashMap<String, i64>,
    /// Markets run by a `[markets.strategy]` table, created on their first visit.
    strategies: HashMap<String, StrategyRuntime>,
    /// Fills and start balances of grid markets, for their report line.
    grid_reports: HashMap<String, StrategyRuntime>,
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
    #[cfg(test)]
    fail_order_ref: bool,
    #[cfg(test)]
    stub_depth: Option<(orderbook_proto::orderbook::OrderbookDepth, orderbook_proto::orderbook::OrderbookDepth)>,
    #[cfg(test)]
    stub_min_base: Option<Option<f64>>,
    #[cfg(test)]
    stub_stamp: Option<i64>,
    #[cfg(test)]
    stub_refuse: Option<String>,
    #[cfg(test)]
    submit_log: Vec<(OrderType, String, String, TimeInForce)>,
    #[cfg(test)]
    stub_settlements: Option<Vec<orderbook_proto::orderbook::Settlement>>,
    #[cfg(test)]
    stub_decision: Option<Decision>,
}

/// Server minimum order size for a market, as last fetched.
#[derive(Debug, Clone, Copy)]
struct MinBase {
    /// Least base quantity at `at_price`; None when the server gives none
    qty: Option<f64>,
    at_price: f64,
    fetched: Instant,
    warned_at: Option<Instant>,
}

impl MinBase {
    /// Least order value in quote units with 1% headroom; 0 when unknown.
    fn min_value(&self) -> f64 {
        self.qty.map_or(0.0, |q| q * self.at_price * 1.01)
    }
}

/// The minimum is fetched again after this long, or after a 2% price move.
const MIN_BASE_REFRESH: Duration = Duration::from_secs(600);
const MIN_BASE_MOVE: f64 = 0.02;
/// After a failed lookup the minimum is retried this often.
const MIN_BASE_RETRY: Duration = Duration::from_secs(60);
/// Book levels fetched for a strategy.
const BOOK_DEPTH: u32 = 20;
/// A strategy side is re-quoted once its target moves this far (percent of the reference)...
const REQUOTE_PCT: f64 = 0.02;
/// ...or, once its orders are this old, by half a tick.
const REQUOTE_AGE: Duration = Duration::from_secs(120);
/// A grid rung left out for the minimum order value returns at this multiple of it.
const MIN_READD: f64 = 1.05;
/// A maker withdraws after this many cycles in a row without book depth.
const DEPTH_FAILS_WITHDRAW: u32 = 6;
/// A resting take still listed this long past its ttl is cancelled.
const REST_GRACE: Duration = Duration::from_secs(5);
/// A failed settlement-history lookup is tried again after this.
const HISTORY_RETRY: Duration = Duration::from_secs(30);

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
            fee_headroom: FeeHeadroom::Wallet,
            fill_needs: HashMap::new(),
            order_ref_warned_at: None,
            last_seen_price: HashMap::new(),
            last_resting: HashMap::new(),
            last_probe: HashMap::new(),
            parked: HashSet::new(),
            short_sides: HashSet::new(),
            force_refresh: HashSet::new(),
            resume_from: 0,
            pending_cancels: HashMap::new(),
            last_bids: HashMap::new(),
            min_base: HashMap::new(),
            min_dropped: HashMap::new(),
            price_stamps: HashMap::new(),
            strategies: HashMap::new(),
            grid_reports: HashMap::new(),
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
            #[cfg(test)]
            fail_order_ref: false,
            #[cfg(test)]
            stub_depth: None,
            #[cfg(test)]
            stub_min_base: None,
            #[cfg(test)]
            stub_stamp: None,
            #[cfg(test)]
            stub_refuse: None,
            #[cfg(test)]
            submit_log: Vec::new(),
            #[cfg(test)]
            stub_settlements: None,
            #[cfg(test)]
            stub_decision: None,
        }
    }

    /// Client reference for a new grid order; `None` (with a rate-limited
    /// warning) when no id can be made, so the order is skipped this cycle.
    fn new_order_ref(&mut self) -> Option<String> {
        #[cfg(test)]
        let made = if self.fail_order_ref {
            Err(anyhow::anyhow!("order ids disabled for test"))
        } else {
            crate::clock::uuid_v7()
        };
        #[cfg(not(test))]
        let made = crate::clock::uuid_v7();
        match made {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                if rate_limit(&mut self.order_ref_warned_at, FUNDING_WARN_EVERY) {
                    warn!("Grid order skipped: cannot make an order id ({e:#})");
                }
                None
            }
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

    /// Fee capacity from prepaid credit and one fill's need (by market where a fee was seen);
    /// replaces the wallet CC prerequisite.
    pub fn set_fee_capacity(&mut self, capacity: Option<f64>, per_fill: f64, by_market: HashMap<String, f64>) {
        self.fee_headroom = FeeHeadroom::Capacity { capacity, per_fill };
        self.fill_needs = by_market;
    }

    /// The fee prerequisite for one market.
    fn headroom_for(&self, market_id: &str) -> FeeHeadroom {
        self.fee_headroom.for_market(self.fill_needs.get(market_id).copied())
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
        self.place(market_id, OrderType::Bid, price, quantity, order_ref, TimeInForce::Gtc, None).await
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
        self.place(market_id, OrderType::Offer, price, quantity, order_ref, TimeInForce::Gtc, None).await
    }

    /// Place an immediate-or-cancel order, signed and tracked like the grid's.
    pub async fn place_ioc(
        &mut self,
        market_id: &str,
        side: OrderType,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
    ) -> Result<u64> {
        debug!("Placing IOC {:?}: {} {} @ {}", side, quantity, market_id, price);
        self.place(market_id, side, price, quantity, order_ref, TimeInForce::Ioc, None).await
    }

    /// Place an order that expires `ttl_secs` from now.
    pub async fn place_gtd(
        &mut self,
        market_id: &str,
        side: OrderType,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
        ttl_secs: u64,
    ) -> Result<u64> {
        debug!("Placing GTD {:?} ({}s): {} {} @ {}", side, ttl_secs, quantity, market_id, price);
        let ttl = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
        let expires_at = prost_types::Timestamp { seconds: crate::clock::now_secs_i64().saturating_add(ttl), nanos: 0 };
        self.place(market_id, side, price, quantity, order_ref, TimeInForce::Gtd, Some(expires_at)).await
    }

    /// Sign, submit and track one order; the time in force is not signed.
    #[allow(clippy::too_many_arguments)]
    async fn place(
        &mut self,
        market_id: &str,
        side: OrderType,
        price: &str,
        quantity: &str,
        order_ref: Option<String>,
        tif: TimeInForce,
        expires_at: Option<prost_types::Timestamp>,
    ) -> Result<u64> {
        let label = if side == OrderType::Bid { "bid" } else { "offer" };

        // Sign the order; the guard holds matching proposals until it is tracked
        let (placement, (signature, signed_data, nonce)) = {
            let tracker = self.tracker.lock().await;
            let placement = tracker.begin_placement(market_id);
            (placement, tracker.sign_order(market_id, label, price, quantity)?)
        };

        let submitted = self
            .submit_signed(market_id, side, price, quantity, order_ref, &signature, &signed_data, nonce, tif, expires_at)
            .await;
        let response = match submitted {
            Ok(response) => response,
            Err(e) => {
                if submit_outcome_unknown(&e) {
                    // May have been booked: a later withdraw must list the market again
                    self.last_resting.remove(market_id);
                    let submit = FailedSubmit {
                        market_id: market_id.to_string(),
                        order_type: side as i32,
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
            // Something rests now, so the next withdraw lists the market
            self.last_resting.remove(market_id);
            let order_id = response.order.as_ref().map(|o| o.order_id).unwrap_or(0);
            debug!("{} placed: order_id={}", if side == OrderType::Bid { "Bid" } else { "Offer" }, order_id);

            // Track the order
            let mut tracker = self.tracker.lock().await;
            tracker.track_order(
                order_id, market_id, side as i32,
                price, quantity, nonce, &signature, &signed_data,
            );
            drop(placement);

            Ok(order_id)
        } else {
            anyhow::bail!("Failed to place {}: {}", label, response.message)
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
        tif: TimeInForce,
        expires_at: Option<prost_types::Timestamp>,
    ) -> Result<SubmitOrderResponse> {
        #[cfg(test)]
        if let Some(message) = self.stub_refuse.clone() {
            self.submit_log.push((side, price.to_string(), quantity.to_string(), tif));
            return Ok(SubmitOrderResponse { success: false, message, ..Default::default() });
        }
        #[cfg(test)]
        if let Some(stub) = self.stub_submit.clone() {
            self.submit_log.push((side, price.to_string(), quantity.to_string(), tif));
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
            .submit_order_tif(
                market_id,
                side,
                price.to_string(),
                quantity.to_string(),
                order_ref,
                Some(signature.to_string()),
                signed_data.to_vec(),
                nonce,
                tif,
                expires_at,
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
            match self.stub_stamp {
                Some(ts) => self.price_stamps.insert(market_id.to_string(), ts),
                None => self.price_stamps.remove(market_id),
            };
            return Ok(price);
        }
        let response = self.client.get_price(market_id).await?;
        match response.timestamp.as_ref() {
            Some(ts) => {
                self.price_stamps.insert(market_id.to_string(), ts.seconds);
            }
            None => {
                self.price_stamps.remove(market_id);
            }
        }
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
        let (cfg, reserve, desk) = self.shape_inputs(market)?;
        Some(OfferShape {
            shaped: shape_offer_levels(&market.offer_levels, cfg, reserve, desk),
            enabled: cfg.enabled,
            desk,
            reserve,
        })
    }

    /// Impact config, pool base reserve and desk net that shape a market's offers.
    fn shape_inputs<'m>(&self, market: &'m MarketConfig) -> Option<(&'m crate::config::PoolImpactConfig, f64, f64)> {
        let (Some(cfg), Some(depth), Some(tracker)) = (
            market.rfq.as_ref().and_then(|r| r.pool_impact.as_ref()),
            self.pool_depths.get(&market.market_id),
            self.net_positions.as_ref(),
        ) else {
            return None;
        };
        let base_token = market.market_id.split('-').next().unwrap_or("");
        Some((cfg, depth.base_reserve, tracker.desk_net(base_token)))
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
            self.headroom_for(&market_config.market_id),
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
            Some(UnknownWhy::FeeCapacity) if rate_limit(&mut self.fee_warned_at, FUNDING_WARN_EVERY) => {
                warn!("Grid paused for {}: prepaid fee capacity unknown or short of one fill's fees", market_id);
            }
            Some(UnknownWhy::FeeCapacity) => {}
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
        let bid_levels = &market.bid_levels;
        self.execute_ladder(&market.market_id, mid_price, active_orders, bid_levels, offer_levels, plan, stop, report)
            .await
    }

    /// [`Self::execute_plan`] for any ladder; each placed side becomes its shape baseline.
    #[allow(clippy::too_many_arguments)]
    async fn execute_ladder(
        &mut self,
        market_id: &str,
        mid_price: f64,
        active_orders: &[Order],
        bid_levels: &[PriceLevel],
        offer_levels: &[PriceLevel],
        plan: (SideAction, SideAction),
        stop: &Shutdown,
        report: &mut CycleReport,
    ) -> Vec<u64> {
        // The shutdown path withdraws the whole grid; not even the tick lookup runs
        if stop.is_shutting_down() {
            return Vec::new();
        }
        let (bid_action, offer_action) = plan;
        let tick = if bid_action == SideAction::Replace || offer_action == SideAction::Replace {
            self.get_tick_size(market_id).await
        } else {
            0.0
        };
        debug!("Refreshing grid for {} (cancel→place): mid={}, tick={}", market_id, mid_price, tick);

        let offer_levels: &[PriceLevel] =
            if offer_action == SideAction::Replace { offer_levels } else { &[] };
        let ops = grid_ops(bid_levels, offer_levels, mid_price, tick, active_orders, plan);

        let mut cancelled_ids: Vec<u64> = Vec::new();
        let mut failed_cancels: Vec<u64> = Vec::new();
        let mut offer_placed = false;
        let mut bid_placed = false;
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
                    let Some(order_ref) = self.new_order_ref() else { continue };
                    let order_ref = Some(order_ref);
                    let (label, result) = if side == OrderType::Bid {
                        ("bid", self.place_bid(market_id, &price, &quantity, order_ref).await)
                    } else {
                        ("offer", self.place_offer(market_id, &price, &quantity, order_ref).await)
                    };
                    match result {
                        Ok(id) => {
                            offer_placed |= side == OrderType::Offer;
                            bid_placed |= side == OrderType::Bid;
                            placed.push(format!("{label} #{id}@{price}"));
                        }
                        Err(e) => warn!("Failed to place {} at {}: {}", label, price, e),
                    }
                }
            }
        }
        // The placed set is the shape trigger's baseline; an all-failed reshape keeps the old one
        if offer_placed {
            self.last_shaped_offers.insert(market_id.to_string(), offer_levels.to_vec());
        }
        if bid_placed {
            self.last_bids.insert(market_id.to_string(), bid_levels.to_vec());
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

    /// One market of a grid cycle: the grid, or the market's strategy.
    async fn visit_market(&mut self, market: &MarketConfig, stop: &Shutdown, report: &mut CycleReport) {
        if self.config.strategy_for(&market.market_id).is_grid() {
            self.report_grid(market).await;
            self.visit_grid(market, stop, report).await;
        } else {
            self.visit_strategy(market, stop, report).await;
        }
    }

    /// One grid market.
    async fn visit_grid(&mut self, market: &MarketConfig, stop: &Shutdown, report: &mut CycleReport) {
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

        // Rungs worth less than the server minimum would only be refused
        if !stop.is_shutting_down() {
            self.refresh_min_base(market_id, current_price).await;
        }
        let kept = self.rungs_above_min(market, current_price);
        let market = kept.as_ref().unwrap_or(market);
        let bid_expected = market.bid_levels.len();

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
        self.withdraw_all(market_id, "no rungs configured", stop, report).await;
    }

    /// [`Self::withdraw_unquoted`] with the reason it logs.
    async fn withdraw_all(&mut self, market_id: &str, why: &str, stop: &Shutdown, report: &mut CycleReport) {
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
            info!("Grid {}: {}; cancelling {} resting order(s)", market_id, why, orders.len());
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

impl OrderManager {
    /// Fetch the server minimum when due: every 10 min, after a 2% move, or a minute after a failure.
    async fn refresh_min_base(&mut self, market_id: &str, price: f64) {
        let now = Instant::now();
        let warned_at = match self.min_base.get(market_id) {
            Some(m) => {
                let age = now.saturating_duration_since(m.fetched);
                let due = match m.qty {
                    Some(_) => age >= MIN_BASE_REFRESH || (price / m.at_price - 1.0).abs() > MIN_BASE_MOVE,
                    None => age >= MIN_BASE_RETRY,
                };
                if !due {
                    return;
                }
                m.warned_at
            }
            None => None,
        };
        let qty = self.fetch_min_base(market_id, price).await;
        self.min_base.insert(market_id.to_string(), MinBase { qty, at_price: price, fetched: now, warned_at });
    }

    /// Least base quantity at `price`; None (no minimum) when the server gives none.
    async fn fetch_min_base(&mut self, market_id: &str, price: f64) -> Option<f64> {
        #[cfg(test)]
        if let Some(stub) = self.stub_min_base {
            return stub;
        }
        let price = rust_decimal::Decimal::from_f64_retain(price).map(|d| d.round_dp(10).to_string());
        match self.client.get_min_order_value(market_id, price).await {
            Ok(r) => r
                .min_base_quantity
                .and_then(|q| q.parse::<f64>().ok())
                .filter(|q| q.is_finite() && *q > 0.0),
            Err(e) => {
                debug!("Market {} minimum order value unavailable ({}); using none", market_id, e);
                None
            }
        }
    }

    /// Least order value in quote units for a market; 0 when unknown.
    fn min_value(&self, market_id: &str) -> f64 {
        self.min_base.get(market_id).map_or(0.0, MinBase::min_value)
    }

    /// The market without rungs worth less than the server minimum, offers judged as shaped;
    /// a rung left out returns at `MIN_READD` times the minimum. None when every rung clears it.
    fn rungs_above_min(&mut self, market: &MarketConfig, mid: f64) -> Option<MarketConfig> {
        let market_id = market.market_id.as_str();
        let floor = self.min_value(market_id);
        if floor <= 0.0 {
            self.min_dropped.remove(market_id);
            return None;
        }
        let value = |l: &PriceLevel| l.quantity.parse::<f64>().map_or(0.0, |q| q * mid * (1.0 + l.delta_percent / 100.0));
        let shape = self.shape_inputs(market);
        let mut dropped: HashSet<(bool, usize)> = HashSet::new();
        let was = self.min_dropped.get(market_id);
        for (offer, levels) in [(false, &market.bid_levels), (true, &market.offer_levels)] {
            for (i, level) in levels.iter().enumerate() {
                // Shaping is per rung; a rung it drops entirely is left to it
                let worth = match shape.filter(|_| offer) {
                    Some((cfg, reserve, desk)) => shape_offer_levels(std::slice::from_ref(level), cfg, reserve, desk)
                        .levels
                        .first()
                        .map_or(f64::INFINITY, value),
                    None => value(level),
                };
                let out = was.is_some_and(|w| w.contains(&(offer, i)));
                let clears = worth >= if out { floor * MIN_READD } else { floor };
                if !clears {
                    dropped.insert((offer, i));
                }
            }
        }
        if dropped.is_empty() {
            self.min_dropped.remove(market_id);
            return None;
        }
        let warn_now = self.min_base.get_mut(market_id).is_some_and(|m| rate_limit(&mut m.warned_at, FUNDING_WARN_EVERY));
        if warn_now {
            warn!(
                "Grid {}: {} rung(s) below the minimum order value {:.2} are skipped",
                market_id,
                dropped.len(),
                floor
            );
        }
        let kept = |offer: bool, levels: &[PriceLevel]| -> Vec<PriceLevel> {
            let mut out = Vec::with_capacity(levels.len());
            for (i, level) in levels.iter().enumerate() {
                if !dropped.contains(&(offer, i)) {
                    out.push(level.clone());
                }
            }
            out
        };
        let filtered = MarketConfig {
            bid_levels: kept(false, &market.bid_levels),
            offer_levels: kept(true, &market.offer_levels),
            ..market.clone()
        };
        self.min_dropped.insert(market_id.to_string(), dropped);
        Some(filtered)
    }

    /// One market run by its strategy; its runtime lives across cycles.
    async fn visit_strategy(&mut self, market: &MarketConfig, stop: &Shutdown, report: &mut CycleReport) {
        report.markets = report.markets.saturating_add(1);
        let market_id = market.market_id.clone();
        let mut rt = match self.strategies.remove(&market_id) {
            Some(rt) => rt,
            None => {
                let config = self.config.strategy_for(&market_id).clone();
                let fill_seq = self.tracker.lock().await.last_fill_seq();
                let rt = StrategyRuntime::new(&config, &market_id, fill_seq, Instant::now());
                if matches!(rt.state, StrategyState::Pending) {
                    warn!(
                        "Strategy {} [{}]: not available in this build yet; placing nothing",
                        market_id, rt.kind
                    );
                } else {
                    info!("Strategy {} [{}]: started", market_id, rt.kind);
                }
                rt
            }
        };
        self.run_strategy(&market_id, &mut rt, stop, report).await;
        self.strategies.insert(market_id, rt);
    }

    /// Price, guard, orders, gate, decide, act.
    async fn run_strategy(
        &mut self,
        market_id: &str,
        rt: &mut StrategyRuntime,
        stop: &Shutdown,
        report: &mut CycleReport,
    ) {
        let now = Instant::now();
        let (fills, held) = {
            let tracker = self.tracker.lock().await;
            (tracker.fills_after(rt.fill_seq), tracker.market_activity(market_id))
        };
        rt.absorb(&fills, market_id);
        rt.prune(now);
        // Before the checks below, so a paused or waiting strategy still reports
        let status = with_cap_note(rt.state.status(), &rt.cap, now);
        self.report_market(market_id, rt, &held, now, &status);

        // The settlements-per-hour cap pauses once, then stops
        match rt.cap.check(&rt.recent, now) {
            CapState::Running => {}
            CapState::Paused { new } => {
                if new {
                    warn!("Strategy {} [{}]: settlements-per-hour cap hit; paused 15 min", market_id, rt.kind);
                }
                self.withdraw_all(market_id, "strategy paused", stop, report).await;
                return;
            }
            CapState::Stopped { new } => {
                if new {
                    warn!("Strategy {} [{}]: settlements-per-hour cap hit again; stopped", market_id, rt.kind);
                }
                self.withdraw_all(market_id, "strategy stopped", stop, report).await;
                return;
            }
        }

        let Some(price) = self.fetch_grid_price(market_id).await else { return };
        let maker = rt.state.is_maker();
        let stamp = self.price_stamps.get(market_id).copied();
        let age = stamp.map_or(Duration::ZERO, |ts| {
            Duration::from_secs(u64::try_from(crate::clock::now_secs_i64().saturating_sub(ts)).unwrap_or(0))
        });
        log_ref_age(market_id, rt, price, stamp.map(|_| age));
        if let Err(why) = rt.guard.check(price, age, now) {
            // Quotes stay through a short staleness; a jump, a bound or a long one withdraws them
            let withdraw = maker && why.withdraws();
            if rate_limit(&mut rt.warned_at, FUNDING_WARN_EVERY) {
                let then = if withdraw { "withdrawing quotes" } else if maker { "holding quotes" } else { "waiting" };
                warn!("Strategy {} [{}]: reference {} not used ({:?}); {}", market_id, rt.kind, price, why, then);
            }
            if withdraw {
                self.withdraw_all(market_id, "reference not usable", stop, report).await;
            }
            return;
        }
        if rt.last_reading != Some((price, stamp)) {
            rt.last_reading = Some((price, stamp));
            rt.state.observe(price, now);
        }
        if stop.is_shutting_down() {
            return;
        }
        self.refresh_min_base(market_id, price).await;
        if stop.is_shutting_down() {
            return;
        }
        let orders = match self.get_active_orders(market_id).await {
            Ok(o) => o,
            Err(e) => {
                warn!("Market {} order fetch failed ({}); skipping this market this cycle", market_id, e);
                return;
            }
        };
        let orders = self.retry_failed_cancels(market_id, orders, stop, report).await;
        if stop.is_shutting_down() {
            return;
        }
        if let Some(from) = rt.state.history_from() {
            self.load_history(market_id, from, rt, now, stop).await;
        }
        let tick = self.get_tick_size(market_id).await;
        let usable = balances_usable(self.balances_set_at, self.config.balance_stale_after_secs, now);
        let (activity, inventory) = {
            let mut tracker = self.tracker.lock().await;
            let activity = tracker.market_activity(market_id);
            let inventory = market_inventory(&self.balances, usable, market_id, self.config.fee_reserve_cc, self.headroom_for(market_id), &activity);
            if inventory.usable {
                note_start(tracker.strategy_starts_mut(), market_id, rt, &inventory, price);
            }
            (activity, inventory)
        };

        // A taker keeps only its take in flight; anything else resting is withdrawn
        if !maker {
            self.cancel_leftovers(market_id, &orders, rt.gate.order_id(), stop, report).await;
        }
        let mut listed = rt.gate.order_id().is_some_and(|id| orders.iter().any(|o| o.order_id == id));
        if let Some((id, until)) = rt.rest_until {
            if rt.gate.order_id() != Some(id) {
                rt.rest_until = None;
            } else if !listed {
                // A resting take left the book: a late match gets the usual time to show up
                rt.rest_until = None;
                rt.gate.restart_hold(now);
            } else if now >= until && !stop.is_shutting_down() {
                // One the server has not expired yet is cancelled here
                match self.cancel_order(id).await {
                    Ok(()) => {
                        report.cancelled = report.cancelled.saturating_add(1);
                        rt.rest_until = None;
                        rt.gate.restart_hold(now);
                        listed = false;
                    }
                    Err(e) => warn!("Market {}: failed to cancel expired take {}: {}", market_id, id, e),
                }
            }
        }
        let (verdict, resolved) = rt.gate.check(&GateInputs {
            now,
            listed,
            open_settlements: activity.open_settlements,
            balances_at: self.balances_set_at,
            last_fill_at: activity.last_fill_at,
            cooldown: rt.state.cooldown(),
        });
        let gone = match verdict {
            Verdict::Stuck(take) => take.order_id,
            _ => resolved.and_then(|take| take.order_id),
        };
        if let Some(id) = gone {
            let matched = self.tracker.lock().await.order_matched(id);
            rt.note_resolved(id, matched);
        }
        if let Verdict::Stuck(take) = verdict {
            warn!("Strategy {} [{}]: take {:?} still listed after 15 min; released", market_id, rt.kind, take.order_id);
            if let Some(id) = take.order_id {
                self.tracker.lock().await.cancel_order(id);
            }
        }
        let book = if maker {
            match self.fetch_book(market_id, &orders, stop).await {
                Some(b) => {
                    rt.depth_fails = 0;
                    Some(b)
                }
                None => {
                    // Quotes stay through a short depth outage; after about 30 s they go
                    if !stop.is_shutting_down() {
                        rt.depth_fails = rt.depth_fails.saturating_add(1);
                        if rt.depth_fails == DEPTH_FAILS_WITHDRAW {
                            warn!("Strategy {} [{}]: no book depth for {} cycles; withdrawing quotes", market_id, rt.kind, rt.depth_fails);
                        }
                        if rt.depth_fails >= DEPTH_FAILS_WITHDRAW {
                            self.withdraw_all(market_id, "book depth unavailable", stop, report).await;
                        }
                    }
                    return;
                }
            }
        } else if rt.state.book_need() == BookNeed::Always && verdict == Verdict::Open {
            // A taker that reads the book to decide fetches it only when it may take
            self.fetch_book(market_id, &orders, stop).await
        } else {
            None
        };
        let floor_value = self.min_value(market_id).max(sizing::SERVER_FLOOR_USD);
        let lot_exp = sizing::lot_exp(price);
        let view = MarketView {
            now,
            wall: crate::clock::now_utc(),
            reference: price,
            tick,
            floor_value,
            lot_exp,
            inventory,
            start_base: rt.start.map_or(inventory.base, |s| s.base),
            book: book.as_ref(),
            activity,
            fills_last_hour: rt.recent.len(),
            usd_last_hour: rt.usd_last_hour(),
            can_take: verdict == Verdict::Open,
        };
        let decision = rt.state.decide(&view);
        #[cfg(test)]
        let decision = self.stub_decision.clone().unwrap_or(decision);
        match decision {
            Decision::Hold | Decision::Take(None) => {}
            Decision::Rest(ladder) => {
                let at_cap = activity.open_settlements >= rt.state.max_inflight();
                self.apply_ladder(market_id, &ladder, price, tick, &orders, at_cap, rt, stop, report).await;
            }
            // One take at a time, whatever the strategy asks for
            Decision::Take(Some(want)) if verdict == Verdict::Open => {
                let take = TakeContext { price, tick, floor_value, lot_exp, inventory };
                self.take(market_id, want, book, &orders, &take, rt, stop, report).await;
            }
            Decision::Take(Some(_)) => {
                debug!("Strategy {} [{}]: take skipped, the gate is shut", market_id, rt.kind);
            }
        }
    }

    /// Both depth views, as the agents' best levels and the LP's prices.
    async fn fetch_book(&mut self, market_id: &str, own: &[Order], stop: &Shutdown) -> Option<BookTop> {
        if stop.is_shutting_down() {
            return None;
        }
        #[cfg(test)]
        if let Some((full, lp)) = self.stub_depth.clone() {
            return Some(book_top(&full, &lp, own));
        }
        let full = self.client.get_orderbook_depth(market_id, BOOK_DEPTH, false).await;
        if stop.is_shutting_down() {
            return None;
        }
        let lp = self.client.get_orderbook_depth(market_id, BOOK_DEPTH, true).await;
        match (full, lp) {
            (Ok(full), Ok(lp)) => Some(book_top(&full, &lp, own)),
            (Err(e), _) | (_, Err(e)) => {
                debug!("Market {} depth unavailable ({}); strategy skips this cycle", market_id, e);
                None
            }
        }
    }

    /// Cancel resting orders with something left to match, except `keep`.
    async fn cancel_leftovers(
        &mut self,
        market_id: &str,
        orders: &[Order],
        keep: Option<u64>,
        stop: &Shutdown,
        report: &mut CycleReport,
    ) {
        let open = |o: &&Order| o.remaining_quantity.parse::<f64>().map_or(true, |r| r > 0.0);
        let ids: Vec<u64> = orders.iter().filter(open).map(|o| o.order_id).filter(|id| Some(*id) != keep).collect();
        for order_id in ids {
            if stop.is_shutting_down() {
                break;
            }
            match self.cancel_order(order_id).await {
                Ok(()) => report.cancelled = report.cancelled.saturating_add(1),
                Err(e) => warn!("Market {}: failed to cancel order {}: {}", market_id, order_id, e),
            }
        }
    }

    /// Price and size a take against the book, then send it as IOC or GTD.
    #[allow(clippy::too_many_arguments)]
    async fn take(
        &mut self,
        market_id: &str,
        want: Want,
        book: Option<BookTop>,
        orders: &[Order],
        ctx: &TakeContext,
        rt: &mut StrategyRuntime,
        stop: &Shutdown,
        report: &mut CycleReport,
    ) {
        let book = match book {
            Some(b) => b,
            None => match self.fetch_book(market_id, orders, stop).await {
                Some(b) => b,
                None => return,
            },
        };
        let sized = match want.tif {
            TakeTif::Ioc => sizing::size_take(&want, &book, &ctx.inventory, ctx.floor_value, ctx.price, ctx.lot_exp),
            TakeTif::Gtd { .. } => {
                sizing::price_rest(&want, &book, &ctx.inventory, ctx.floor_value, ctx.price, ctx.tick, ctx.lot_exp)
            }
        };
        let order = match sized {
            Ok(order) => order,
            Err(why) => {
                debug!("Strategy {} [{}]: no take this cycle ({:?})", market_id, rt.kind, why);
                return;
            }
        };
        if stop.is_shutting_down() {
            return;
        }
        let Some(order_ref) = self.new_order_ref() else { return };
        let quantity = order.quantity.to_string();
        let now = Instant::now();
        let (label, result) = match want.tif {
            TakeTif::Ioc => ("IOC", self.place_ioc(market_id, order.side, &order.price, &quantity, Some(order_ref)).await),
            TakeTif::Gtd { ttl_secs } => (
                "GTD",
                self.place_gtd(market_id, order.side, &order.price, &quantity, Some(order_ref), ttl_secs).await,
            ),
        };
        let side = if order.side == OrderType::Bid { "buy" } else { "sell" };
        match result {
            Ok(id) => {
                rt.gate.placed(id, now);
                rt.note_take(id);
                rt.rest_until = match want.tif {
                    TakeTif::Gtd { ttl_secs } => {
                        now.checked_add(Duration::from_secs(ttl_secs).saturating_add(REST_GRACE)).map(|t| (id, t))
                    }
                    TakeTif::Ioc => None,
                };
                report.placed = report.placed.saturating_add(1);
                info!("Strategy {} [{}]: {} {} {} @ {} #{}", market_id, rt.kind, label, side, quantity, order.price, id);
            }
            // Only a transport status leaves the outcome open; a refusal or signing failure is definite
            Err(e) if e.downcast_ref::<tonic::Status>().is_some() && submit_outcome_unknown(&e) => {
                rt.gate.ambiguous(now);
                warn!("Strategy {} [{}]: {} {} may have been booked ({:#})", market_id, rt.kind, label, side, e);
            }
            Err(e) => {
                rt.gate.refused(now);
                // A refusal is often the minimum order value: look it up again
                self.min_base.remove(market_id);
                if rate_limit(&mut rt.warned_at, FUNDING_WARN_EVERY) {
                    warn!("Strategy {} [{}]: {} {} refused ({:#}); backing off", market_id, rt.kind, label, side, e);
                }
            }
        }
    }

    /// Place a maker ladder with the grid's plan, uncross and execute steps.
    #[allow(clippy::too_many_arguments)]
    async fn apply_ladder(
        &mut self,
        market_id: &str,
        ladder: &Ladder,
        price: f64,
        tick: f64,
        orders: &[Order],
        at_cap: bool,
        rt: &mut StrategyRuntime,
        stop: &Shutdown,
        report: &mut CycleReport,
    ) {
        let now = Instant::now();
        let usable = balances_usable(self.balances_set_at, self.config.balance_stale_after_secs, now);
        let funding = ladder_funding(
            &self.balances,
            usable,
            market_id,
            Some(price),
            &ladder.bids,
            &ladder.offers,
            self.config.fee_reserve_cc,
            self.headroom_for(market_id),
        );
        if funding.bid == Funding::Unknown && funding.offer == Funding::Unknown {
            report.held_unknown = report.held_unknown.saturating_add(1);
            return;
        }
        let resting = strategy_side_counts(orders, sizing::MIN_ORDER_USD);
        let anchors = self.grid_anchors.get(market_id).copied().unwrap_or_default();
        let restore = self.force_refresh.remove(market_id);
        let tick_pct = if price > 0.0 { 50.0 * tick / price } else { 0.0 };
        let side_moved = |anchor: Option<f64>, prev: Option<&Vec<PriceLevel>>, levels: &[PriceLevel], at: Option<Instant>, rests: usize| {
            match (anchor, prev) {
                (Some(anchor), Some(prev)) => {
                    let old = at.is_some_and(|t| now.saturating_duration_since(t) >= REQUOTE_AGE);
                    ladder_moved(anchor, prev, price, levels, if old { tick_pct } else { REQUOTE_PCT })
                }
                // Nothing of ours placed yet: whatever rests makes way for the ladder
                _ => rests > 0,
            }
        };
        let force_bid =
            restore || side_moved(anchors.bid, self.last_bids.get(market_id), &ladder.bids, rt.bid_placed_at, resting.bids);
        let force_offer = restore
            || side_moved(anchors.offer, self.last_shaped_offers.get(market_id), &ladder.offers, rt.offer_placed_at, resting.offers);
        let mut plan = plan_market(&PlanInput {
            bid: funding.bid,
            offer: funding.offer,
            bid_expected: ladder.bids.len(),
            offer_expected: ladder.offers.len(),
            resting,
            force_bid,
            force_offer,
            shape_changed: false,
        });
        let fundable = |f: Funding, expected: usize| f == Funding::Funded && expected > 0;
        let one_sided = matches!(plan, (SideAction::Replace, SideAction::Keep) | (SideAction::Keep, SideAction::Replace));
        if one_sided && !stop.is_shutting_down() {
            let sides = (fundable(funding.bid, ladder.bids.len()), fundable(funding.offer, ladder.offers.len()));
            plan = uncross(plan, &ladder.bids, &ladder.offers, price, tick, orders, sides);
        }
        // At the in-flight cap nothing new is placed; withdrawals still go ahead
        if at_cap {
            let hold = |a: SideAction| if a == SideAction::Replace { SideAction::Keep } else { a };
            plan = (hold(plan.0), hold(plan.1));
        }
        let anchored = next_anchors(anchors, plan, resting, price);
        if anchored != anchors {
            self.grid_anchors.insert(market_id.to_string(), anchored);
        }
        if plan == (SideAction::Keep, SideAction::Keep) {
            return;
        }
        info!(
            "Strategy {} [{}] re-quoting: bids {}/{} {:?}, offers {}/{} {:?}",
            market_id,
            rt.kind,
            resting.bids,
            ladder.bids.len(),
            plan.0,
            resting.offers,
            ladder.offers.len(),
            plan.1
        );
        let failed = self
            .execute_ladder(market_id, price, orders, &ladder.bids, &ladder.offers, plan, stop, report)
            .await;
        if !failed.is_empty() {
            self.pending_cancels.entry(market_id.to_string()).or_default().extend(failed);
        }
        report.refreshed = report.refreshed.saturating_add(1);
        for (action, placed_at, baseline) in [
            (plan.0, &mut rt.bid_placed_at, &mut self.last_bids),
            (plan.1, &mut rt.offer_placed_at, &mut self.last_shaped_offers),
        ] {
            match action {
                SideAction::Replace => *placed_at = Some(now),
                SideAction::Cancel => {
                    *placed_at = None;
                    baseline.remove(market_id);
                }
                SideAction::Keep => {}
            }
        }
    }
}

impl OrderManager {
    /// A strategy's own settlements since `from`; it places nothing until they load.
    async fn load_history(
        &mut self,
        market_id: &str,
        from: chrono::DateTime<chrono::Utc>,
        rt: &mut StrategyRuntime,
        now: Instant,
        stop: &Shutdown,
    ) {
        if stop.is_shutting_down() || rt.history_retry_at.is_some_and(|t| now < t) {
            return;
        }
        match self.fetch_settlements(market_id, from).await {
            Ok(settlements) => {
                rt.history_retry_at = None;
                rt.state.on_history(&settlements, &self.config.party_id);
            }
            Err(e) => {
                rt.history_retry_at = now.checked_add(HISTORY_RETRY);
                if rate_limit(&mut rt.warned_at, FUNDING_WARN_EVERY) {
                    warn!("Strategy {} [{}]: settlement history unavailable ({:#}); not trading yet", market_id, rt.kind, e);
                }
            }
        }
    }

    async fn fetch_settlements(
        &mut self,
        market_id: &str,
        from: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<orderbook_proto::orderbook::Settlement>> {
        #[cfg(test)]
        if let Some(stub) = self.stub_settlements.clone() {
            return Ok(stub);
        }
        let from_time = prost_types::Timestamp { seconds: from.timestamp(), nanos: 0 };
        self.client.get_settlements(Some(market_id), Some(from_time)).await
    }

    /// A grid market's fills and start balances, kept for its report line; no RPC.
    async fn report_grid(&mut self, market: &MarketConfig) {
        let market_id = market.market_id.as_str();
        let now = Instant::now();
        let (fills, activity, seq) = {
            let tracker = self.tracker.lock().await;
            let seq = self.grid_reports.get(market_id).map_or_else(|| tracker.last_fill_seq(), |rt| rt.fill_seq);
            (tracker.fills_after(seq), tracker.market_activity(market_id), seq)
        };
        let config = self.config.strategy_for(market_id).clone();
        let mut rt = self.grid_reports.remove(market_id).unwrap_or_else(|| StrategyRuntime::new(&config, market_id, seq, now));
        rt.absorb(&fills, market_id);
        rt.prune(now);
        if let Some(price) = self.last_seen_price.get(market_id).copied().filter(|_| rt.start.is_none()) {
            let usable = balances_usable(self.balances_set_at, self.config.balance_stale_after_secs, now);
            let inv = market_inventory(&self.balances, usable, market_id, self.config.fee_reserve_cc, self.headroom_for(market_id), &activity);
            if inv.usable {
                rt.start = Some(Start { base: inv.base, quote: inv.quote, price });
            }
        }
        let status = format!("rungs {} bid / {} offer", market.bid_levels.len(), market.offer_levels.len());
        self.report_market(market_id, &mut rt, &activity, now, &status);
        self.grid_reports.insert(market_id.to_string(), rt);
    }

    /// The report line every 5 min, valued at the last price seen.
    fn report_market(&self, market_id: &str, rt: &mut StrategyRuntime, activity: &MarketActivity, now: Instant, status: &str) {
        if now.saturating_duration_since(rt.last_report) < REPORT_EVERY {
            return;
        }
        let Some(price) = self.last_seen_price.get(market_id).copied() else { return };
        rt.last_report = now;
        let usable = balances_usable(self.balances_set_at, self.config.balance_stale_after_secs, now);
        let inv = market_inventory(&self.balances, usable, market_id, self.config.fee_reserve_cc, self.headroom_for(market_id), activity);
        info!(
            "{}",
            format_report(&ReportLine {
                market_id,
                kind: rt.kind,
                base: inv.base,
                quote: inv.quote,
                price,
                start: rt.start,
                stats: rt.stats,
                takes: rt.kind != "grid",
                status,
            })
        );
    }
}

/// Records the balances a strategy holds, and its start the first time.
fn note_start(
    starts: &mut StrategyStarts,
    market_id: &str,
    rt: &mut StrategyRuntime,
    inventory: &Inventory,
    price: f64,
) {
    let (kind, held) = (rt.kind, Held { base: inventory.base, quote: inventory.quote });
    let start = *rt.start.get_or_insert_with(|| first_start(starts, market_id, kind, held, price));
    starts.set(market_id, kind, start, held);
}

/// The start saved at the last stop, moved by what came in or went out meanwhile; else these balances.
fn first_start(starts: &mut StrategyStarts, market_id: &str, kind: &str, held: Held, price: f64) -> Start {
    match starts.take(market_id, kind, held, crate::clock::now_millis()) {
        Some((s, shift)) if shift != Held::default() => {
            warn!(
                "Strategy {} [{}]: balances changed while stopped (base {:+.2}, quote {:+.2}); \
                 start moved by as much (now base {:.2}, quote {:.2}), so only the position carries over",
                market_id, kind, shift.base, shift.quote, s.base, s.quote
            );
            s
        }
        Some((s, _)) => {
            info!(
                "Strategy {} [{}]: start balances from the last stop (base {:.2}, quote {:.2})",
                market_id, kind, s.base, s.quote
            );
            s
        }
        None => Start { base: held.base, quote: held.quote, price },
    }
}

/// The strategy's status, led by the settlements cap when it holds the strategy.
fn with_cap_note(status: String, cap: &SettleCap, now: Instant) -> String {
    let note = if cap.strikes >= 2 {
        "stopped by the settlements cap"
    } else if cap.paused_until.is_some_and(|t| now < t) {
        "paused by the settlements cap"
    } else {
        return status;
    };
    if status.is_empty() { note.to_string() } else { format!("{note}; {status}") }
}

/// The reference age at debug each cycle, and once at INFO when the strategy starts.
fn log_ref_age(market_id: &str, rt: &mut StrategyRuntime, price: f64, age: Option<Duration>) {
    let limit = rt.guard.max_age().as_secs();
    let age = age.map_or_else(|| "has no server time".to_string(), |a| format!("is {}s old", a.as_secs()));
    if rt.age_logged {
        debug!("Strategy {} [{}]: reference {} {} (limit {}s)", market_id, rt.kind, price, age, limit);
    } else {
        rt.age_logged = true;
        info!("Strategy {} [{}]: reference {} {} (limit {}s)", market_id, rt.kind, price, age, limit);
    }
}

/// What a take is priced and sized with.
struct TakeContext {
    price: f64,
    tick: f64,
    floor_value: f64,
    lot_exp: i32,
    inventory: Inventory,
}

fn balance_of<'a>(balances: &'a [TokenBalance], token: &str) -> Option<&'a TokenBalance> {
    balances.iter().find(|b| if token == "CC" { b.is_canton_coin } else { b.instrument_id == token })
}

fn amount(b: Option<&TokenBalance>, field: fn(&TokenBalance) -> &str) -> f64 {
    b.and_then(|b| field(b).parse::<f64>().ok()).unwrap_or(0.0)
}

/// Balances of a BASE-QUOTE market with matched, unsettled amounts applied (pure);
/// what is available leaves the CC fee reserve, as grid funding does.
pub(crate) fn market_inventory(
    balances: &[TokenBalance],
    usable: bool,
    market_id: &str,
    fee_reserve_cc: f64,
    headroom: FeeHeadroom,
    activity: &MarketActivity,
) -> Inventory {
    let Some((base, quote)) = market_id.split_once('-').filter(|(_, q)| !q.contains('-')) else {
        return Inventory::default();
    };
    let (b, q) = (balance_of(balances, base), balance_of(balances, quote));
    let reserve = |token: &str| if token == "CC" { fee_reserve_cc } else { 0.0 };
    Inventory {
        base: amount(b, |t| &t.total_amount) + activity.pending_bid_base - activity.pending_offer_base,
        quote: amount(q, |t| &t.total_amount) - activity.pending_bid_quote + activity.pending_offer_quote,
        base_avail: (amount(b, |t| &t.unlocked_amount) - reserve(base)).max(0.0),
        quote_avail: (amount(q, |t| &t.unlocked_amount) - reserve(quote)).max(0.0),
        usable: usable && !balances.is_empty() && headroom.covers_fees(balances, fee_reserve_cc),
    }
}

/// Resting orders per side for a strategy; `partial` marks an order with under
/// `min_value` left (quote units), so its slot is refilled.
pub(crate) fn strategy_side_counts(orders: &[Order], min_value: f64) -> SideCounts {
    let mut counts = SideCounts::default();
    for o in orders {
        let left = o.remaining_quantity.parse::<f64>().ok().zip(o.price.parse::<f64>().ok()).map(|(r, p)| r * p);
        let low = left.is_some_and(|v| v < min_value);
        if o.order_type == OrderType::Bid as i32 {
            counts.bids = counts.bids.saturating_add(1);
            counts.bid_partial |= low;
        } else if o.order_type == OrderType::Offer as i32 {
            counts.offers = counts.offers.saturating_add(1);
            counts.offer_partial |= low;
        }
    }
    counts
}

/// Whether a ladder side moved past `threshold_pct` of `price` since it was placed at `anchor`,
/// or a rung was added, dropped or resized.
pub(crate) fn ladder_moved(anchor: f64, prev: &[PriceLevel], price: f64, now: &[PriceLevel], threshold_pct: f64) -> bool {
    if prev.len() != now.len() || price.is_nan() || price <= 0.0 {
        return true;
    }
    prev.iter().zip(now).any(|(a, b)| {
        let placed = anchor * (1.0 + a.delta_percent / 100.0);
        let target = price * (1.0 + b.delta_percent / 100.0);
        let moved = ((target - placed) / price).abs() * 100.0 > threshold_pct;
        let (qa, qb) = (a.quantity.parse::<f64>().unwrap_or(0.0), b.quantity.parse::<f64>().unwrap_or(0.0));
        let resized = if qa > 0.0 { ((qb - qa) / qa).abs() > SHAPE_REFRESH_QTY_FRACTION } else { qa != qb };
        moved || resized
    })
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
    headroom: FeeHeadroom,
) -> GridFunding {
    ladder_funding(balances, usable, &market.market_id, mid, &market.bid_levels, offer_levels, fee_reserve_cc, headroom)
}

/// [`grid_funding`] for any ladder.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ladder_funding(
    balances: &[TokenBalance],
    usable: bool,
    market_id: &str,
    mid: Option<f64>,
    bid_levels: &[PriceLevel],
    offer_levels: &[PriceLevel],
    fee_reserve_cc: f64,
    headroom: FeeHeadroom,
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
    let Some((base, quote)) = market_id
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

    // Paying fees is a prerequisite for either side
    if !headroom.covers_fees(balances, fee_reserve_cc) {
        out.why = Some(match headroom {
            FeeHeadroom::Wallet => UnknownWhy::FeeReserve,
            FeeHeadroom::Capacity { .. } => UnknownWhy::FeeCapacity,
        });
        return out;
    }

    // Available amounts after reserving CC for fees
    out.bid_avail = if quote == "CC" { quote_unlocked - fee_reserve_cc } else { quote_unlocked };
    out.offer_avail = if base == "CC" { base_unlocked - fee_reserve_cc } else { base_unlocked };

    // Total base needed for offers (selling base)
    out.offer_need = offer_levels
        .iter()
        .map(|l| l.quantity.parse::<f64>().unwrap_or(0.0))
        .fold(0.0, |total, q| total + q);
    out.offer = side_funding(out.offer_need, out.offer_avail);

    // Total quote needed for bids (buying base with quote)
    if let Some(mid) = mid.filter(|m| m.is_finite() && *m > 0.0) {
        out.bid_need = bid_levels.iter()
            .map(|l| {
                let qty: f64 = l.quantity.parse().unwrap_or(0.0);
                qty * mid * (1.0 + l.delta_percent / 100.0)
            })
            .fold(0.0, |total, q| total + q);
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
            dropped = dropped.saturating_add(1);
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
        let f = grid_funding(&[cc("100")], false, &m, Some(700.0), &offers, 5.0, FeeHeadroom::Wallet);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::Aged)));
        let f = grid_funding(&[], true, &m, Some(700.0), &offers, 5.0, FeeHeadroom::Wallet);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::Aged)));
    }

    #[test]
    fn cc_below_the_fee_reserve_is_unknown() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("1"), token("SPYe", "10"), token("USDCx", "10000")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
        assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::FeeReserve)));
        assert_eq!(f.cc_avail, 1.0);
    }

    /// With prepaid fee capacity wallet CC is no prerequisite; unknown or used-up capacity holds both sides.
    #[test]
    fn fee_capacity_replaces_the_wallet_cc_prerequisite() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("0"), token("SPYe", "10"), token("USDCx", "10000")];
        let funded = FeeHeadroom::Capacity { capacity: Some(94.0), per_fill: 14.7 };
        let f = ladder_funding(&balances, true, &m.market_id, Some(700.0), &m.bid_levels, &m.offer_levels, 5.0, funded);
        assert_eq!((f.bid, f.offer, f.why), (Funded, Funded, None));
        // Unknown, used up, or short of one fill's fee
        for (capacity, per_fill) in [(None, 0.0), (Some(0.0), 0.0), (Some(3.0), 14.7)] {
            let held = FeeHeadroom::Capacity { capacity, per_fill };
            let f = ladder_funding(&balances, true, &m.market_id, Some(700.0), &m.bid_levels, &m.offer_levels, 5.0, held);
            assert_eq!((f.bid, f.offer, f.why), (Unknown, Unknown, Some(UnknownWhy::FeeCapacity)), "{capacity:?}");
            let activity = MarketActivity::default();
            assert!(!market_inventory(&balances, true, "SPYe-USDCx", 5.0, held, &activity).usable, "{capacity:?}");
        }
        let activity = MarketActivity::default();
        assert!(market_inventory(&balances, true, "SPYe-USDCx", 5.0, funded, &activity).usable);
        // Without a rate the per-fill estimate is zero: any capacity counts
        let no_rate = FeeHeadroom::Capacity { capacity: Some(3.0), per_fill: 0.0 };
        assert!(market_inventory(&balances, true, "SPYe-USDCx", 5.0, no_rate, &activity).usable);
        assert!(!market_inventory(&balances, true, "SPYe-USDCx", 5.0, FeeHeadroom::Wallet, &activity).usable);
        // The reserve still stays out of CC inventory
        let ccm = market("CC-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "100"));
        let cc_rich = [cc("50"), token("USDCx", "10000")];
        let f = ladder_funding(&cc_rich, true, "CC-USDCx", Some(0.1), &ccm.bid_levels, &ccm.offer_levels, 5.0, funded);
        assert_eq!(f.offer_avail, 45.0);
        assert_eq!(market_inventory(&cc_rich, true, "CC-USDCx", 5.0, funded, &activity).base_avail, 45.0);
    }

    /// A market where a fee was seen needs that fee per fill, more or less than the default.
    #[test]
    fn a_market_with_a_seen_fee_needs_that_fee_per_fill() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("0"), token("SPYe", "10"), token("USDCx", "10000")];
        let fund = |headroom| ladder_funding(&balances, true, &m.market_id, Some(700.0), &m.bid_levels, &m.offer_levels, 5.0, headroom);
        // $2.00 per side seen here: 30.5 CC a fill, above 20 CC of capacity
        let default = FeeHeadroom::Capacity { capacity: Some(20.0), per_fill: 15.87 };
        assert_eq!(fund(default).why, None);
        assert_eq!(fund(default.for_market(Some(30.5))).why, Some(UnknownWhy::FeeCapacity));
        // A discounted fee: 3 CC of capacity covers a 2.67 CC fill
        let low = FeeHeadroom::Capacity { capacity: Some(3.0), per_fill: 15.87 };
        assert_eq!(fund(low).why, Some(UnknownWhy::FeeCapacity));
        assert_eq!((fund(low.for_market(Some(2.67))).bid, fund(low.for_market(Some(2.67))).why), (Funded, None));
        assert_eq!(low.for_market(None), low);
        assert_eq!(FeeHeadroom::Wallet.for_market(Some(1.0)), FeeHeadroom::Wallet);
    }

    /// Offers far short of their rungs while bids are funded.
    #[test]
    fn spye_offers_short_while_bids_are_funded() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "0.001"), token("USDCx", "1000")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
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
        let f = grid_funding(&balances, true, &m, Some(3.0), &[], 5.0, FeeHeadroom::Wallet);
        assert_eq!(f.bid, Funded);
        assert_eq!(f.bid_avail, 95.0);
        // 30 base at 3.2 = 96 > 95
        let f = grid_funding(&balances, true, &m, Some(3.2), &[], 5.0, FeeHeadroom::Wallet);
        assert_eq!(f.bid, Short);

        let m = market("CC-USDCx", Vec::new(), flat());
        let balances = [cc("100"), token("USDCx", "0")];
        let f = grid_funding(&balances, true, &m, Some(0.15), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
        assert_eq!((f.offer, f.offer_avail), (Funded, 95.0));
        let heavy = vec![level(0.0, "32"), level(0.0, "32"), level(0.0, "32")];
        let f = grid_funding(&balances, true, &m, Some(0.15), &heavy, 5.0, FeeHeadroom::Wallet);
        assert_eq!(f.offer, Short);
    }

    #[test]
    fn non_base_quote_market_is_funded() {
        let balances = [cc("100")];
        for id in ["SPYE", "A-B-C"] {
            let m = market(id, ladder(-1.0, "1"), ladder(1.0, "1"));
            let f = grid_funding(&balances, true, &m, Some(1.0), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
            assert_eq!((f.bid, f.offer), (Funded, Funded), "market {id}");
        }
    }

    #[test]
    fn no_mid_leaves_bids_unknown() {
        let m = market("SPYe-USDCx", ladder(-1.0, "0.05"), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "1"), token("USDCx", "1000")];
        let f = grid_funding(&balances, true, &m, None, &m.offer_levels, 5.0, FeeHeadroom::Wallet);
        assert_eq!((f.bid, f.offer), (Unknown, Funded));
        let f = grid_funding(&balances, true, &m, Some(f64::NAN), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
        assert_eq!(f.bid, Unknown);
    }

    #[test]
    fn a_side_needing_nothing_is_funded() {
        let m = market("SPYe-USDCx", Vec::new(), ladder(1.0, "0.05"));
        let balances = [cc("100"), token("SPYe", "0"), token("USDCx", "0")];
        let f = grid_funding(&balances, true, &m, Some(700.0), &m.offer_levels, 5.0, FeeHeadroom::Wallet);
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
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = url;
        config.markets = markets;
        let client = OrderbookClient::lazy_for_tests(&config).unwrap();
        let tracker = OrderTracker::new(0, Secret::seal(&mut [7u8; 32]).unwrap());
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

    // Without an order id the placement is skipped, not sent
    #[tokio::test]
    async fn a_grid_order_without_an_order_id_is_skipped() {
        let aaa = market("AAA-USD", ladder(-1.0, "1"), ladder(1.0, "1"));
        let mut om = offline_manager(vec![aaa.clone()]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        om.stub_submit = Some(Ok(9));
        om.fail_order_ref = true;
        let stop = Shutdown::new();
        let mut report = CycleReport::default();
        let run = om.execute_plan(&aaa, 100.0, &[], &aaa.offer_levels, (Replace, Replace), &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        assert_eq!(report.placed, 0);
        assert!(om.order_ref_warned_at.is_some(), "the skip is reported");

        om.fail_order_ref = false;
        let run = om.execute_plan(&aaa, 100.0, &[], &aaa.offer_levels, (Replace, Replace), &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        assert_eq!(report.placed, 6, "three rungs a side");
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

#[cfg(test)]
mod strategy_visit_tests {
    use super::*;
    use crate::config::{
        ArbitrageConfig, MarketMakerConfig, MeanReversionConfig, RebalancerConfig, StrategyConfig, StrategyMode, TradeSide,
        TwapConfig,
    };
    use crate::order_tracker::VerifyResult;
    use crate::secret::Secret;
    use crate::strategy::book::tests::{depth, lvl};
    use orderbook_proto::orderbook::OrderbookDepth;

    const ID: &str = "HTEST-USDC";

    fn bare(id: &str) -> MarketConfig {
        MarketConfig {
            market_id: id.to_string(),
            enabled: true,
            base_order_size: None,
            bid_levels: Vec::new(),
            offer_levels: Vec::new(),
            price_change_threshold_percent: 1.0,
            rfq: None,
        }
    }

    fn token(id: &str, amount: &str, cc: bool) -> TokenBalance {
        TokenBalance {
            instrument_id: id.to_string(),
            total_amount: amount.to_string(),
            unlocked_amount: amount.to_string(),
            is_canton_coin: cc,
            ..Default::default()
        }
    }

    fn closed_port_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}")
    }

    fn manager(url: String, markets: Vec<MarketConfig>, strategy: Option<StrategyConfig>) -> OrderManager {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = url;
        if let (Some(s), Some(m)) = (strategy, markets.first()) {
            config.strategies.insert(m.market_id.clone(), s);
        }
        config.markets = markets;
        let client = OrderbookClient::lazy_for_tests(&config).unwrap();
        let tracker = OrderTracker::new(0, Secret::seal(&mut [7u8; 32]).unwrap());
        let mut om = OrderManager::new(config, client, Arc::new(Mutex::new(tracker)));
        om.tick_sizes.insert(ID.to_string(), 1e-7);
        om.stub_min_base = Some(None);
        om.set_balances(vec![token("Amulet", "100", true), token("HTEST", "1250000", false), token("USDC", "2000", false)]);
        om
    }

    fn strategy_manager(strategy: StrategyConfig) -> OrderManager {
        manager(closed_port_url(), vec![bare(ID)], Some(strategy))
    }

    /// The LP at +-15 bps around 0.0016; `agent_bid` adds another agent's bid at 0.001599.
    fn book(agent_bid: bool) -> (OrderbookDepth, OrderbookDepth) {
        let lp = depth(vec![lvl("0.0015976", "10000", 1)], vec![lvl("0.0016024", "10000", 1)]);
        let mut full = lp.clone();
        if agent_bid {
            full.bids.insert(0, lvl("0.0015990000", "30000", 1));
        }
        (full, lp)
    }

    fn resting(id: u64, side: OrderType, price: &str, remaining: &str) -> Order {
        Order {
            order_id: id,
            market_id: ID.to_string(),
            order_type: side as i32,
            price: price.to_string(),
            quantity: remaining.to_string(),
            remaining_quantity: remaining.to_string(),
            filled_quantity: "0".to_string(),
            ..Default::default()
        }
    }

    async fn visit(om: &mut OrderManager) -> CycleReport {
        let market = bare(ID);
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        let run = om.visit_market(&market, &stop, &mut report);
        tokio::time::timeout(Duration::from_secs(10), run).await.unwrap();
        report
    }

    fn back(om: &mut OrderManager, secs: u64) {
        let rt = om.strategies.get_mut(ID).unwrap();
        let earlier = |t: Instant| t.checked_sub(Duration::from_secs(secs)).unwrap();
        if let Some(last) = rt.gate.last.as_mut() {
            last.at = earlier(last.at);
        }
        rt.gate.last_placed_at = rt.gate.last_placed_at.map(earlier);
    }

    fn rebalancer() -> StrategyConfig {
        StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.25), mode: StrategyMode::Taker, ..Default::default() })
    }

    fn market_maker() -> StrategyConfig {
        StrategyConfig::MarketMaker(MarketMakerConfig::default())
    }

    /// The listing once every logged submit rests, with ids from 1.
    fn placed(log: &[(OrderType, String, String, TimeInForce)]) -> Vec<Order> {
        (1u64..).zip(log).map(|(id, (side, p, q, _))| resting(id, *side, p, q)).collect()
    }

    fn level(delta_percent: f64, quantity: &str) -> PriceLevel {
        PriceLevel { delta_percent, quantity: quantity.to_string() }
    }

    // The maker places its two-level ladder once, then leaves it alone on an identical visit
    #[tokio::test]
    async fn a_market_maker_places_then_keeps_its_ladder() {
        let mut om = strategy_manager(StrategyConfig::MarketMaker(MarketMakerConfig::default()));
        om.stub_depth = Some(book(false));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(9));
        om.stub_cancels = Some(Vec::new());
        let report = visit(&mut om).await;
        assert_eq!(report.placed, 4);
        let log = om.submit_log.clone();
        assert!(log.iter().all(|(_, _, _, tif)| *tif == TimeInForce::Gtc));
        let price = |p: &str| p.parse::<f64>().unwrap();
        let bids: Vec<f64> = log.iter().filter(|e| e.0 == OrderType::Bid).map(|e| price(&e.1)).collect();
        let offers: Vec<f64> = log.iter().filter(|e| e.0 == OrderType::Offer).map(|e| price(&e.1)).collect();
        assert_eq!((bids.len(), offers.len()), (2, 2));
        // Inside the LP on both sides
        assert!(bids.iter().all(|b| *b < 0.0016) && bids.iter().any(|b| *b > 0.0015976), "{bids:?}");
        assert!(offers.iter().all(|o| *o > 0.0016) && offers.iter().any(|o| *o < 0.0016024), "{offers:?}");

        let book_now: Vec<Order> = (1u64..)
            .zip(&log)
            .map(|(id, (side, p, q, _))| resting(id, *side, p, q))
            .collect();
        om.stub_orders = Some(book_now);
        let report = visit(&mut om).await;
        assert_eq!((report.placed, report.cancelled, report.refreshed), (0, 0, 0));
        assert_eq!(om.submit_log.len(), 4);
        assert_eq!(om.stub_cancels.as_deref(), Some(&[][..]));
    }

    // One IOC at exactly the agent bid, then nothing until the gate opens
    #[tokio::test]
    async fn a_take_sends_one_ioc_at_the_top_price_until_the_gate_opens() {
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(42));
        visit(&mut om).await;
        let first = (OrderType::Offer, "0.0015990000".to_string(), "12500".to_string(), TimeInForce::Ioc);
        assert_eq!(om.submit_log, vec![first.clone()]);

        // Listed, then gone but young: no second take
        om.stub_orders = Some(vec![resting(42, OrderType::Offer, "0.0015990000", "0")]);
        visit(&mut om).await;
        om.stub_orders = Some(Vec::new());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);

        // Past the 15 s hold and the 150 s cooldown, the gate opens again
        back(&mut om, 151);
        visit(&mut om).await;
        assert_eq!(om.submit_log, vec![first.clone(), first]);
        assert_eq!(om.strategies.get(ID).map(|rt| rt.stats.takes), Some(2));
    }

    // A take asked for while the gate is shut is not sent; the gate itself decides
    #[tokio::test]
    async fn a_take_asked_for_while_the_gate_is_shut_sends_nothing() {
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(42));
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
        let side = OrderType::Offer;
        let want = Want { side, max_quantity: 12_500.0, max_deviation_percent: 0.15, tif: TakeTif::Ioc };
        om.stub_decision = Some(Decision::Take(Some(want)));
        om.stub_orders = Some(vec![resting(42, OrderType::Offer, "0.0015990000", "12500")]);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1, "the last take is still listed");
        om.stub_orders = Some(Vec::new());
        back(&mut om, 151);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 2, "sent once the gate opens");
    }

    // Start balances saved at a stop within 6 hours are kept; older ones are taken again
    #[tokio::test]
    async fn a_restart_keeps_start_balances_saved_within_six_hours() {
        use crate::state::{SavedStrategyStart, STRATEGY_START_MAX_AGE_MS};
        let in_band = StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.5), ..Default::default() });
        let saved_start = Start { base: 1_000_000.0, quote: 2400.0, price: 0.0015 };
        let fresh = Start { base: 1_250_000.0, quote: 2000.0, price: 0.0016 };
        let held = Held { base: fresh.base, quote: fresh.quote };
        let now = crate::clock::now_millis();
        for (age_ms, expected) in [(3_600_000, saved_start), (STRATEGY_START_MAX_AGE_MS + 60_000, fresh)] {
            let mut om = strategy_manager(in_band.clone());
            let saved = SavedStrategyStart::new(ID, "rebalancer", saved_start, held, now - age_ms).unwrap();
            om.tracker.lock().await.strategy_starts_mut().restore(&[saved], now);
            om.stub_book(0.0016, Vec::new());
            om.stub_cancels = Some(Vec::new());
            visit(&mut om).await;
            assert!(om.submit_log.is_empty());
            assert_eq!(om.strategies.get(ID).and_then(|rt| rt.start), Some(expected));
            // The next save keeps them, stamped with its own time
            let out = om.tracker.lock().await.strategy_starts().export(now + 5);
            let out: Vec<_> = out.iter().map(|s| (s.saved_at_ms, s.start_at(now + 5))).collect();
            assert_eq!(out, [(now + 5, Some(expected))]);
        }
    }

    // Funds moved while stopped move the restored start, with one warning; a file without
    // the balances at the save keeps the start as saved
    #[tokio::test]
    async fn a_restart_after_a_withdrawal_keeps_the_position_not_the_old_base() {
        use crate::state::SavedStrategyStart;
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::INFO);
        let in_band = StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.5), ..Default::default() });
        let saved_start = Start { base: 1_000_000.0, quote: 2400.0, price: 0.0015 };
        // Long 500k at the stop; the restart finds 1.25M base and 2000 quote
        let at_save = Held { base: 1_500_000.0, quote: 1650.0 };
        let now = crate::clock::now_millis();
        let saved = SavedStrategyStart::new(ID, "rebalancer", saved_start, at_save, now - 60_000).unwrap();
        let older = SavedStrategyStart { base_at_save: None, quote_at_save: None, ..saved.clone() };
        let moved = Start { base: 750_000.0, quote: 2750.0, price: 0.0015 };
        for (entry, expected) in [(saved, moved), (older, saved_start)] {
            let mut om = strategy_manager(in_band.clone());
            om.tracker.lock().await.strategy_starts_mut().restore(&[entry], now);
            om.stub_book(0.0016, Vec::new());
            om.stub_cancels = Some(Vec::new());
            visit(&mut om).await;
            assert_eq!(om.strategies.get(ID).and_then(|rt| rt.start), Some(expected));
            // The next save holds the balances of the last cycle
            let out = om.tracker.lock().await.strategy_starts().export(now + 5);
            let written = (out[0].base_at_save.as_deref(), out[0].quote_at_save.as_deref());
            assert_eq!(written, (Some("1250000"), Some("2000")));
        }
        assert_eq!(1_250_000.0 - moved.base, at_save.base - saved_start.base, "the position, not the old base");
        assert_eq!(logs.count("balances changed while stopped (base -250000.00, quote +350.00)"), 1);
        assert_eq!(logs.count("start moved by as much (now base 750000.00, quote 2750.00)"), 1);
        assert_eq!(logs.count("start balances from the last stop (base 1000000.00, quote 2400.00)"), 1);
    }

    // The IOC is signed and tracked like a grid order, so its match verifies
    #[tokio::test]
    async fn an_ioc_order_passes_settlement_verification() {
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(42));
        visit(&mut om).await;
        let proposal = orderbook_proto::orderbook::SettlementProposal {
            market_id: ID.to_string(),
            buyer: "counterparty".to_string(),
            seller: "our-party".to_string(),
            base_quantity: "12500".to_string(),
            order_match: Some(orderbook_proto::orderbook::OrderMatch { offer_order_id: 42, ..Default::default() }),
            ..Default::default()
        };
        let verdict = om.tracker.lock().await.verify_settlement(&proposal, "our-party");
        assert!(matches!(verdict, VerifyResult::Accepted { order_id: 42 }));
    }

    // A refusal backs off; an unknown outcome holds the gate for 120 s
    #[tokio::test]
    async fn refused_and_ambiguous_takes_close_the_gate() {
        use tonic::Status;
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Err(Status::invalid_argument("below the minimum order value")));
        visit(&mut om).await;
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1, "backing off");
        assert!(om.strategies.get(ID).is_some_and(|rt| rt.gate.backoff_until.is_some()));

        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Err(Status::unavailable("x")));
        visit(&mut om).await;
        back(&mut om, 20);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1, "held after an ambiguous submit");
        back(&mut om, 200);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 2);
    }

    // A taker withdraws orders left from another kind, and keeps none of its own
    #[tokio::test]
    async fn a_taker_withdraws_leftover_orders() {
        let in_band = StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.5), ..Default::default() });
        let mut om = strategy_manager(in_band);
        om.stub_book(0.0016, vec![resting(5, OrderType::Bid, "0.0015", "1000"), resting(6, OrderType::Offer, "0.0017", "0")]);
        om.stub_cancels = Some(Vec::new());
        om.stub_submit = Some(Ok(1));
        visit(&mut om).await;
        assert_eq!(om.stub_cancels.as_deref(), Some(&[5u64][..]), "a matched-only order has nothing to cancel");
        assert!(om.submit_log.is_empty());
    }

    // After shutdown the visit sends nothing; any RPC here would hang
    #[tokio::test]
    async fn a_strategy_visit_issues_no_rpc_after_shutdown() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mut om = manager(url, vec![bare(ID)], Some(rebalancer()));
        om.stub_min_base = None;
        om.stub_price = Some(0.0016);
        let stop = Shutdown::new();
        om.stop_on_price = Some(stop.clone());
        let mut report = CycleReport::default();
        let market = bare(ID);
        let run = om.visit_market(&market, &stop, &mut report);
        assert!(tokio::time::timeout(Duration::from_secs(2), run).await.is_ok());
        assert!(om.submit_log.is_empty());
        drop(listener);
    }

    // Grid rungs worth less than the server minimum are not placed
    #[tokio::test]
    async fn grid_rungs_below_the_minimum_order_value_are_skipped() {
        let level = |delta_percent: f64, quantity: &str| PriceLevel { delta_percent, quantity: quantity.to_string() };
        let mut m = bare("AAA-USD");
        m.bid_levels = vec![level(-0.5, "1"), level(-1.0, "2")];
        m.offer_levels = vec![level(0.5, "1"), level(1.0, "2")];
        let mut om = manager(closed_port_url(), vec![m.clone()], None);
        om.set_balances(vec![token("Amulet", "100", true), token("AAA", "10", false), token("USD", "1000", false)]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        om.stub_min_base = Some(Some(1.5));
        om.stub_book(100.0, Vec::new());
        om.stub_submit = Some(Ok(9));
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        tokio::time::timeout(Duration::from_secs(10), om.visit_market(&m, &stop, &mut report)).await.unwrap();
        // The minimum is looked up right after the price, so the first visit applies it
        let quantities: Vec<&str> = om.submit_log.iter().map(|e| e.2.as_str()).collect();
        assert_eq!(quantities, ["2", "2"], "{:?}", om.submit_log);
        assert!(om.config.strategy_for("AAA-USD").is_grid());
    }

    // An offer shaped below the minimum is not placed, though its configured size clears it
    #[tokio::test]
    async fn grid_offers_shaped_below_the_minimum_order_value_are_skipped() {
        let mut m = bare("AAA-USD");
        m.bid_levels = vec![level(-0.5, "1")];
        m.offer_levels = vec![level(0.5, "1"), level(1.0, "4")];
        let rfq = "min_quantity = \"1\"\nmax_quantity = \"10\"\n[pool_impact]\nenabled = true\ngrid_offer_scale_base = 4.0\n";
        m.rfq = Some(toml::from_str(rfq).unwrap());
        let mut om = manager(closed_port_url(), vec![m.clone()], None);
        om.set_balances(vec![token("Amulet", "100", true), token("AAA", "10", false), token("USD", "1000", false)]);
        om.tick_sizes.insert("AAA-USD".to_string(), 0.01);
        // A desk net of 3 against a scale of 4 shapes offers to a quarter
        let path = std::env::temp_dir().join(format!("agent-logic-shape-{}.json", std::process::id()));
        let nets = crate::net_position::NetPositionTracker::load_or_new(path, 24.0, Duration::from_secs(600));
        nets.record_confirm("q1", Some("someone"), "AAA", 3.0);
        om.set_net_positions(nets);
        om.pool_depths.insert("AAA-USD".to_string(), PoolDepth { base_reserve: 1e9 });
        om.stub_min_base = Some(Some(0.5));
        om.stub_book(100.0, Vec::new());
        om.stub_submit = Some(Ok(9));
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        tokio::time::timeout(Duration::from_secs(10), om.visit_market(&m, &stop, &mut report)).await.unwrap();
        // $25 of the first offer is under the $50.50 floor; the second shapes to $101
        let offers: Vec<f64> =
            om.submit_log.iter().filter(|e| e.0 == OrderType::Offer).map(|e| e.2.parse::<f64>().unwrap()).collect();
        assert_eq!(offers.len(), 1, "{:?}", om.submit_log);
        assert!((offers[0] - 1.0).abs() < 1e-6, "{offers:?}");
        assert_eq!(om.submit_log.iter().filter(|e| e.0 == OrderType::Bid).count(), 1);
    }

    // A rung left out for the minimum returns only 5% above it, so the side does not flap
    #[tokio::test]
    async fn a_grid_rung_near_the_minimum_returns_only_past_the_band() {
        let mut m = bare("AAA-USD");
        m.bid_levels = vec![level(0.0, "1"), level(-1.0, "2")];
        let mut om = manager(closed_port_url(), vec![m.clone()], None);
        let min = MinBase { qty: Some(1.0), at_price: 10.0, fetched: Instant::now(), warned_at: None };
        om.min_base.insert("AAA-USD".to_string(), min);
        let mut bids = |mid: f64| om.rungs_above_min(&m, mid).map_or(2, |k| k.bid_levels.len());
        // The floor is 10.10 with its 1% headroom
        assert_eq!(bids(10.2), 2);
        assert_eq!(bids(10.0), 1, "under the floor");
        assert_eq!(bids(10.3), 1, "above the floor, inside the band");
        assert_eq!(bids(10.7), 2, "past the band");
        assert_eq!(bids(10.15), 2, "kept down to the floor");
    }

    // A withdraw seen empty skips the listing for a minute, but not once something was placed
    #[tokio::test]
    async fn a_withdraw_after_a_placement_lists_the_market_again() {
        let mut om = strategy_manager(market_maker());
        om.stub_book(0.0016, Vec::new());
        om.stub_cancels = Some(Vec::new());
        om.stub_submit = Some(Ok(7));
        let (stop, mut report) = (Shutdown::new(), CycleReport::default());
        om.withdraw_all(ID, "test", &stop, &mut report).await;
        assert_eq!(om.last_resting.get(ID), Some(&SideCounts::default()));
        om.place_bid(ID, "0.0015", "10000", None).await.unwrap();
        om.stub_orders = Some(vec![resting(7, OrderType::Bid, "0.0015", "10000")]);
        om.withdraw_all(ID, "test", &stop, &mut report).await;
        assert_eq!(om.stub_cancels.as_deref(), Some(&[7u64][..]));
        // A submit that may have been booked counts as a placement too
        om.stub_submit = Some(Err(tonic::Status::unavailable("x")));
        assert!(om.place_ioc(ID, OrderType::Bid, "0.0015", "10000", None).await.is_err());
        assert!(!om.last_resting.contains_key(ID));
    }

    // A stale reading keeps a maker's quotes; stale for 5 min they are withdrawn
    #[tokio::test]
    async fn a_stale_reference_holds_maker_quotes_then_withdraws_them() {
        let mut om = strategy_manager(market_maker());
        om.stub_depth = Some(book(false));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(9));
        om.stub_cancels = Some(Vec::new());
        let now = crate::clock::now_secs_i64();
        // A served stamp under a minute old is normal
        om.stub_stamp = Some(now - 50);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 4);
        om.stub_orders = Some(placed(&om.submit_log));

        om.stub_stamp = Some(now - 120);
        let report = visit(&mut om).await;
        assert_eq!((report.placed, report.cancelled), (0, 0), "held");
        let rt = om.strategies.get_mut(ID).unwrap();
        rt.guard.stale_since = rt.guard.stale_since.and_then(|t| t.checked_sub(Duration::from_secs(300)));
        assert_eq!(visit(&mut om).await.cancelled, 4, "stale for 300 s by the clock");
        assert_eq!(om.submit_log.len(), 4);

        // Fresh again, the quotes return; a stamp 400 s old has been stale over 300 s
        om.stub_stamp = Some(now);
        om.stub_orders = Some(Vec::new());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 8);
        om.stub_orders = Some(placed(&om.submit_log[4..]));
        om.stub_stamp = Some(now - 400);
        assert_eq!(visit(&mut om).await.cancelled, 4, "stale for 310 s by the stamp");
    }

    // A taker sends nothing on a stale reading and takes once it is fresh
    #[tokio::test]
    async fn a_taker_waits_out_a_stale_reference() {
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(42));
        om.stub_cancels = Some(Vec::new());
        om.stub_stamp = Some(crate::clock::now_secs_i64() - 400);
        visit(&mut om).await;
        assert!(om.submit_log.is_empty());
        assert_eq!(om.stub_cancels.as_deref(), Some(&[][..]));
        om.stub_stamp = Some(crate::clock::now_secs_i64());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
    }

    // success=false and a signing failure are refusals; only a timeout status leaves it open
    #[tokio::test]
    async fn refused_or_unsigned_takes_back_off_and_only_a_timeout_holds() {
        for refuse in [true, false] {
            let mut om = strategy_manager(rebalancer());
            om.stub_depth = Some(book(true));
            om.stub_book(0.0016, Vec::new());
            om.stub_submit = Some(Ok(42));
            if refuse {
                om.stub_refuse = Some("below the minimum order value".to_string());
            } else {
                om.tracker.lock().await.set_fail_signing(true);
            }
            visit(&mut om).await;
            let rt = om.strategies.get(ID).unwrap();
            assert!(rt.gate.backoff_until.is_some() && rt.gate.last.is_none(), "refuse={refuse}");
            assert!(!om.min_base.contains_key(ID), "the minimum is looked up again");
            assert_eq!(om.submit_log.len(), usize::from(refuse));
        }
        let mut om = strategy_manager(rebalancer());
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Err(tonic::Status::deadline_exceeded("client deadline exceeded")));
        visit(&mut om).await;
        let rt = om.strategies.get(ID).unwrap();
        assert!(rt.gate.backoff_until.is_none() && rt.gate.last.is_some_and(|l| l.ambiguous));
    }

    // A placement-based cooldown used to pin a resting maker clip to a stale price
    #[tokio::test]
    async fn a_rebalancer_maker_reprices_its_clip_when_the_reference_moves() {
        let maker = RebalancerConfig { target_share: Some(0.65), mode: StrategyMode::Maker, ..Default::default() };
        let mut om = strategy_manager(StrategyConfig::Rebalancer(maker));
        om.stub_depth = Some(book(false));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(9));
        om.stub_cancels = Some(Vec::new());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
        let (side, price, quantity, _) = om.submit_log[0].clone();
        // 40 bps lower, inside the jump guard: the clip is cancelled and placed again at once
        om.stub_book(0.0015936, vec![resting(9, side, &price, &quantity)]);
        visit(&mut om).await;
        assert_eq!(om.stub_cancels.as_deref(), Some(&[9u64][..]));
        assert_eq!(om.submit_log.len(), 2);
        assert_ne!(om.submit_log[1].1, price);
    }

    // With depth failing the maker keeps its quotes for five cycles and withdraws on the sixth
    #[tokio::test]
    async fn a_maker_withdraws_after_six_depth_failures() {
        let mut om = strategy_manager(market_maker());
        let quotes = vec![resting(1, OrderType::Bid, "0.0015984", "25100"), resting(2, OrderType::Offer, "0.0016016", "25000")];
        om.stub_book(0.0016, quotes);
        om.stub_cancels = Some(Vec::new());
        om.stub_submit = Some(Ok(9));
        for _ in 0..5 {
            visit(&mut om).await;
        }
        assert_eq!(om.stub_cancels.as_deref(), Some(&[][..]));
        visit(&mut om).await;
        assert_eq!(om.stub_cancels.as_deref(), Some(&[1u64, 2][..]));
        assert!(om.submit_log.is_empty());
    }

    // An arbitrage takes an agent offer 10 bps under the reference once it has watched it a minute
    #[tokio::test]
    async fn an_arbitrage_takes_an_agent_offer_through_the_reference() {
        let mut om = strategy_manager(StrategyConfig::Arbitrage(ArbitrageConfig::default()));
        let (mut full, lp) = book(false);
        full.offers.insert(0, lvl("0.0015984000", "20000", 1));
        om.stub_depth = Some((full, lp));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(77));
        visit(&mut om).await;
        assert!(om.submit_log.is_empty(), "a new reference is not judged stable yet");
        if let StrategyState::Arbitrage(a) = &mut om.strategies.get_mut(ID).unwrap().state {
            a.age(Duration::from_secs(61));
        }
        visit(&mut om).await;
        // At exactly the agent offer, capped by its one order
        let first = (OrderType::Bid, "0.0015984000".to_string(), "20000".to_string(), TimeInForce::Ioc);
        assert_eq!(om.submit_log, vec![first.clone()]);
        // Gone but young: nothing more; past the hold and the 10 s cooldown it takes again
        om.stub_orders = Some(Vec::new());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
        back(&mut om, 16);
        visit(&mut om).await;
        assert_eq!(om.submit_log, vec![first.clone(), first]);
        // The first take matched nothing, so it counts as empty
        let stats = om.strategies.get(ID).map(|rt| (rt.stats.takes, rt.stats.takes_filled, rt.stats.takes_empty));
        assert_eq!(stats, Some((2, 0, 1)));
    }

    // A mean-reversion maker rests one clip 4 bps inside, keeps it, and withdraws it back at its mean
    #[tokio::test]
    async fn a_mean_reversion_maker_rests_one_clip_and_withdraws_it_at_the_mean() {
        let mr = MeanReversionConfig { mode: StrategyMode::Maker, ..Default::default() };
        let mut om = strategy_manager(StrategyConfig::MeanReversion(mr));
        om.stub_depth = Some(book(false));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(9));
        om.stub_cancels = Some(Vec::new());
        visit(&mut om).await;
        assert!(om.submit_log.is_empty(), "warming up");
        let prime = |om: &mut OrderManager, mean: f64| {
            let now = Instant::now();
            if let StrategyState::MeanReversion(m) = &mut om.strategies.get_mut(ID).unwrap().state {
                m.prime(mean, now.checked_sub(Duration::from_secs(700)).unwrap(), now);
            }
        };
        // 37.5 bps over its mean: a $100 short, one $30 offer 4 bps over the reference
        prime(&mut om, 0.0016 / 1.00375);
        visit(&mut om).await;
        let offer = (OrderType::Offer, "0.0016007000".to_string(), "18800".to_string(), TimeInForce::Gtc);
        assert_eq!(om.submit_log, vec![offer]);
        om.stub_orders = Some(placed(&om.submit_log));
        let report = visit(&mut om).await;
        assert_eq!((report.placed, report.cancelled), (0, 0), "kept");
        prime(&mut om, 0.0016);
        let report = visit(&mut om).await;
        assert_eq!((report.placed, report.cancelled), (0, 1), "withdrawn");
        assert_eq!(om.stub_cancels.as_deref(), Some(&[1u64][..]));
    }

    // A twap with start_at waits for its history, rests a slice, cancels it past its ttl, then takes
    #[tokio::test]
    async fn a_twap_loads_its_history_then_rests_and_takes() {
        let start = crate::clock::now_utc() - chrono::TimeDelta::seconds(60);
        let twap = TwapConfig {
            side: Some(TradeSide::Sell),
            total_quantity: Some("250000".into()),
            start_at: Some(start.to_rfc3339()),
            seed: Some(5),
            ..Default::default()
        };
        let mut om = strategy_manager(StrategyConfig::Twap(twap));
        om.stub_depth = Some(book(true));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(31));
        om.stub_cancels = Some(Vec::new());
        // The lookup goes to a closed port: nothing is placed until it succeeds
        visit(&mut om).await;
        assert!(om.submit_log.is_empty());
        assert!(om.strategies.get(ID).is_some_and(|rt| rt.history_retry_at.is_some()));
        let earlier = orderbook_proto::orderbook::Settlement {
            proposal_id: "p1".into(),
            market_id: ID.into(),
            buyer: "someone".into(),
            seller: om.config.party_id.clone(),
            base_quantity: "10000".into(),
            ..Default::default()
        };
        om.stub_settlements = Some(vec![earlier]);
        om.strategies.get_mut(ID).unwrap().history_retry_at = None;
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
        let (side, price, quantity, tif) = om.submit_log[0].clone();
        assert_eq!((side, price.as_str(), tif), (OrderType::Offer, "0.0016023", TimeInForce::Gtd));
        // Still listed past its ttl: cancelled here, and the slice takes the agent bid
        om.stub_orders = Some(vec![resting(31, OrderType::Offer, &price, &quantity)]);
        let rt = om.strategies.get_mut(ID).unwrap();
        rt.rest_until = rt.rest_until.map(|(id, _)| (id, Instant::now()));
        if let StrategyState::Twap(t) = &mut rt.state {
            t.age(Duration::from_secs(61));
        }
        back(&mut om, 61);
        visit(&mut om).await;
        assert_eq!(om.stub_cancels.as_deref(), Some(&[31u64][..]));
        // A match of the rest may still arrive: the take waits the usual 15 s
        om.stub_orders = Some(Vec::new());
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 1);
        back(&mut om, 16);
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 2);
        let (side, price, _, tif) = om.submit_log[1].clone();
        assert_eq!((side, price.as_str(), tif), (OrderType::Offer, "0.0015990000", TimeInForce::Ioc));
        assert_eq!(om.strategies.get(ID).map(|rt| rt.stats.takes_empty), Some(1));
    }

    // The settlements cap withdraws the quotes when it pauses, and again when it stops
    #[tokio::test]
    async fn the_settlement_cap_withdraws_on_pause_and_on_stop() {
        let mm = MarketMakerConfig { max_settlements_per_hour: 1, ..Default::default() };
        let mut om = strategy_manager(StrategyConfig::MarketMaker(mm));
        om.stub_depth = Some(book(false));
        om.stub_book(0.0016, Vec::new());
        om.stub_submit = Some(Ok(9));
        om.stub_cancels = Some(Vec::new());
        let fill = |om: &mut OrderManager, secs_ago: u64| {
            let at = Instant::now().checked_sub(Duration::from_secs(secs_ago)).unwrap();
            let fill = crate::strategy::RecentFill { at, side: OrderType::Bid, base: 1.0, quote: 1.0 };
            om.strategies.get_mut(ID).unwrap().recent.push_back(fill);
        };
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 4);
        om.stub_orders = Some(placed(&om.submit_log));
        fill(&mut om, 10);
        let report = visit(&mut om).await;
        assert_eq!((report.cancelled, report.placed), (4, 0), "paused");
        assert!(om.strategies.get(ID).is_some_and(|rt| rt.cap.paused_until.is_some()));

        // The pause runs out and the maker quotes again; a second hit stops it
        om.stub_orders = Some(Vec::new());
        om.strategies.get_mut(ID).unwrap().cap.paused_until = Instant::now().checked_sub(Duration::from_secs(1));
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 8);
        om.stub_orders = Some(placed(&om.submit_log[4..]));
        fill(&mut om, 0);
        let report = visit(&mut om).await;
        assert_eq!((report.cancelled, report.placed), (4, 0), "stopped");
        assert!(om.strategies.get(ID).is_some_and(|rt| rt.cap.strikes == 2));
        visit(&mut om).await;
        assert_eq!(om.submit_log.len(), 8, "stays stopped");
    }
}
