//! market_maker: a two-sided ladder around an inventory-skewed fair price.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;

use super::book::{BookTop, Level};
use super::sizing::{level_ticks, order_qty, ticks_price, to_ticks, MIN_ORDER_USD};
use super::{bps, clampf, BookNeed, Decide, Decision, Ladder, MarketView, Rung};
use crate::config::{MarketMakerConfig, PriceLevel};
use crate::num::Px;

/// After a jump the ladder stays withdrawn for this many cycles.
const PULL_CYCLES: u32 = 2;
/// How long a burst of fills widens the spread.
const THROTTLE_FOR: Duration = Duration::from_secs(600);
/// Funding checks keep this sliver of headroom against float noise.
const FUNDS_SLACK: f64 = 1e-9;

/// P5 for one maker price, in whole ticks: never through the LP, and through an
/// agent quote only when `cross_ok`, then exactly at it so one level can match.
pub(crate) fn make_ticks(side: OrderType, target: i64, book: &BookTop, tick: f64, cross_ok: bool) -> i64 {
    // Bids round down, offers up, as the order itself does
    let ticks = |l: &Option<Level>| l.as_ref().and_then(|l| level_ticks(l, tick, side == OrderType::Offer));
    let mut n = target;
    if side == OrderType::Bid {
        if let Some(lp_ask) = ticks(&book.lp_ask) {
            n = n.min(lp_ask.saturating_sub(1));
        }
        if let Some(ask) = ticks(&book.agent_ask).filter(|a| n >= *a) {
            n = if cross_ok { ask } else { ask.saturating_sub(1) };
        }
    } else {
        if let Some(lp_bid) = ticks(&book.lp_bid) {
            n = n.max(lp_bid.saturating_add(1));
        }
        if let Some(bid) = ticks(&book.agent_bid).filter(|b| n <= *b) {
            n = if cross_ok { bid } else { bid.saturating_add(1) };
        }
    }
    n
}

/// [`make_ticks`] for a target price; None when the price is under one tick.
pub(crate) fn make_price(side: OrderType, target: f64, book: &BookTop, tick: f64, cross_ok: bool) -> Option<i64> {
    let target = to_ticks(target / tick, side == OrderType::Offer)?;
    Some(make_ticks(side, target, book, tick, cross_ok)).filter(|n| *n >= 1)
}

/// Percent from `reference` that the order manager rounds back to exactly this tick.
pub(crate) fn rung_delta(side: OrderType, price: f64, tick: f64, reference: f64) -> f64 {
    let n = (price / tick).round();
    let mid = if side == OrderType::Bid { n + 0.5 } else { n - 0.5 };
    (mid * tick / reference - 1.0) * 100.0
}

pub(crate) struct MarketMaker {
    cfg: MarketMakerConfig,
    last_quote_ref: Option<f64>,
    skip: u32,
    throttled_until: Option<Instant>,
    last: Option<(f64, f64, f64)>,
}

impl MarketMaker {
    pub(crate) fn new(cfg: MarketMakerConfig) -> Self {
        Self { cfg, last_quote_ref: None, skip: 0, throttled_until: None, last: None }
    }
}

/// One side's rungs under a funds and a position budget; outer rungs go first.
struct SideBuilder {
    side: OrderType,
    levels: Vec<Rung>,
    need: f64,
    value: f64,
    prev: Option<i64>,
    done: bool,
}

impl SideBuilder {
    fn new(side: OrderType) -> Self {
        Self { side, levels: Vec::new(), need: 0.0, value: 0.0, prev: None, done: false }
    }

    /// Adds a rung unless it breaks the side's order or budgets; a rung under $12 is
    /// skipped, and the first funds or position miss ends the side. Values in quote units.
    fn push(&mut self, ticks: Option<i64>, value: f64, v: &MarketView<'_>, funds: f64, room: f64) {
        let min_order = v.in_quote(MIN_ORDER_USD);
        if self.done || value < min_order {
            return;
        }
        let tick = v.tick;
        let n = match (self.side, self.prev, ticks) {
            (OrderType::Bid, Some(prev), Some(n)) => Some(n.min(prev.saturating_sub(1))),
            (_, Some(prev), Some(n)) => Some(n.max(prev.saturating_add(1))),
            (_, _, n) => n,
        };
        let Some((n, exact)) = n.and_then(|n| Some((n, ticks_price(n, tick)?))) else {
            self.done = true;
            return;
        };
        let price = n as f64 * tick;
        let Some(qty) = order_qty(value, min_order, price, v.floor_value, v.lot_exp) else {
            self.done = true;
            return;
        };
        let qf = qty.to_f64().unwrap_or(f64::INFINITY);
        let delta = rung_delta(self.side, price, tick, v.reference);
        // The same formula grid funding checks, so a trimmed side never reads as short
        let need = if self.side == OrderType::Bid { qf * v.reference * (1.0 + delta / 100.0) } else { qf };
        let order_value = qf * price;
        if self.need + need > funds * (1.0 - FUNDS_SLACK) || self.value + order_value > room {
            self.done = true;
            return;
        }
        self.need += need;
        self.value += order_value;
        self.prev = Some(n);
        let level = PriceLevel { delta_percent: delta, quantity: qty.to_string() };
        self.levels.push(Rung { level, price: exact });
    }
}

impl Decide for MarketMaker {
    fn book_need(&self) -> BookNeed {
        BookNeed::Always
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        let c = &self.cfg;
        let Some(book) = v.book else { return Decision::Hold };
        if !v.inventory.usable || !(v.tick > 0.0 && v.reference > 0.0) {
            return Decision::Hold;
        }
        let r = v.reference;
        if self.last_quote_ref.is_some_and(|last| bps(r, last).abs() > c.pull_jump_bps) {
            self.skip = PULL_CYCLES;
        }
        self.last_quote_ref = Some(r);
        if self.skip > 0 {
            self.skip = self.skip.saturating_sub(1);
            return Decision::Rest(Ladder::default());
        }
        if v.fills_last_hour > usize::try_from(c.max_fills_per_hour).unwrap_or(usize::MAX) {
            self.throttled_until = v.now.checked_add(THROTTLE_FOR);
        }
        let throttled = self.throttled_until.is_some_and(|t| v.now < t);
        let h = if throttled { c.throttle_spread_bps } else { c.half_spread_bps };

        // Inventory ratio in quarters, so the ladder holds still between fills
        let position = (v.inventory.base - v.start_base) * r;
        let max_inventory = v.in_quote(c.max_inventory_usd);
        let q = (clampf(position / max_inventory, -1.0, 1.0) * 4.0).round() / 4.0;
        let fair = r * (1.0 - c.skew_bps / 10_000.0 * q);
        self.last = Some((q, fair, h));

        let (tick, improve) = (v.tick, i64::from(c.lp_improve_ticks));
        let edge = c.min_edge_bps / 10_000.0;
        let mut bids = SideBuilder::new(OrderType::Bid);
        let mut offers = SideBuilder::new(OrderType::Offer);
        let bid_room = max_inventory - position;
        let offer_room = max_inventory + position;
        for i in 0..c.levels {
            let i_f = f64::from(i);
            let dist = (h + i_f * c.level_step_bps) / 10_000.0;
            let (bid, offer) = (fair * (1.0 - dist), fair * (1.0 + dist));
            let mut bid_t = to_ticks(bid / tick, false);
            let mut offer_t = to_ticks(offer / tick, true);
            // While throttled the wide spread wins over joining the LP; joins count whole ticks
            if i == 0 && c.lp_inside && !throttled {
                if let Some(lp) = book.lp_bid.as_ref().filter(|b| bid <= b.price && q <= 0.5) {
                    let joined = level_ticks(lp, tick, false).map(|n| n.saturating_add(improve));
                    bid_t = joined.zip(to_ticks(r * (1.0 - edge) / tick, false)).map(|(j, cap)| j.min(cap));
                }
                if let Some(lp) = book.lp_ask.as_ref().filter(|a| offer >= a.price && q >= -0.5) {
                    let joined = level_ticks(lp, tick, true).map(|n| n.saturating_sub(improve));
                    offer_t = joined.zip(to_ticks(r * (1.0 + edge) / tick, true)).map(|(j, cap)| j.max(cap));
                }
            }
            let size = v.in_quote(c.level_usd) * c.size_growth.powi(i32::try_from(i).unwrap_or(0));
            // Shrink the side that would grow the position; drop it at the limit
            if q < 1.0 {
                let value = if q > 0.0 { size * (1.0 - q) } else { size };
                let n = bid_t.map(|t| make_ticks(OrderType::Bid, t, book, tick, q < 0.0));
                bids.push(n, value, v, v.inventory.quote_avail, bid_room);
            }
            if q > -1.0 {
                let value = if q < 0.0 { size * (1.0 + q) } else { size };
                let n = offer_t.map(|t| make_ticks(OrderType::Offer, t, book, tick, q > 0.0));
                offers.push(n, value, v, v.inventory.base_avail, offer_room);
            }
        }
        Decision::Rest(Ladder { bids: bids.levels, offers: offers.levels })
    }

    fn status(&self) -> String {
        match self.last {
            Some((q, fair, h)) => format!("inventory {q:+.2} fair {} half spread {h} bps", Px(fair, 6)),
            None => "waiting".to_string(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::book::Level;
    use super::super::Inventory;
    use super::*;
    use crate::order_tracker::MarketActivity;
    use rust_decimal::Decimal;

    pub(crate) type LpPrice = Level;

    pub(crate) fn lp_of(price: f64, exact: &str) -> LpPrice {
        Level { price, price_str: exact.to_string(), qty: 1e4, count: 1 }
    }

    pub(crate) fn lp_px(price: f64) -> Level {
        lp_of(price, &price.to_string())
    }

    pub(crate) fn lp_book(r: f64) -> BookTop {
        BookTop { agent_bid: None, agent_ask: None, lp_bid: Some(lp_px(r * 0.9985)), lp_ask: Some(lp_px(r * 1.0015)) }
    }

    pub(crate) fn view<'a>(book: Option<&'a BookTop>, base: f64, start_base: f64) -> MarketView<'a> {
        MarketView {
            now: Instant::now(),
            wall: crate::clock::now_utc(),
            reference: 0.0016,
            tick: 1e-7,
            floor_value: 10.0,
            lot_exp: 2,
            inventory: Inventory { base, quote: 2000.0, base_avail: base, quote_avail: 2000.0, usable: true },
            start_base,
            book,
            activity: MarketActivity::default(),
            fills_last_hour: 0,
            usd_last_hour: 0.0,
            can_take: true,
            quote_usd: 1.0,
        }
    }

    /// CC at $0.0746, so $1 is about 13.4 CC.
    pub(crate) const CC_USD: f64 = 0.0746;

    /// TUSDT-CC at 13.4 CC a TUSDT, $2000 on each side unless set otherwise.
    pub(crate) fn cc_view<'a>(book: Option<&'a BookTop>, base: f64, start_base: f64) -> MarketView<'a> {
        let mut v = view(book, base, start_base);
        (v.reference, v.tick, v.lot_exp) = (13.4, 1e-10, super::super::sizing::lot_exp(13.4));
        (v.floor_value, v.quote_usd) = (10.0 / CC_USD, CC_USD);
        (v.inventory.quote, v.inventory.quote_avail) = (2000.0 / CC_USD, 2000.0 / CC_USD);
        v
    }

    pub(crate) fn cc_flat_base() -> f64 {
        2000.0 / CC_USD / 13.4
    }

    #[test]
    fn a_cc_quoted_maker_sizes_its_rungs_and_limits_in_usd() {
        let book = lp_book(13.4);
        let base = cc_flat_base();
        let ladder = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&cc_view(Some(&book), base, base)));
        assert_eq!((ladder.bids.len(), ladder.offers.len()), (2, 2), "{ladder:?}");
        let usd = |r: &Rung| r.level.quantity.parse::<f64>().unwrap() * 13.4 * CC_USD;
        // $40 then $60 a rung, about 536 and 804 CC
        for side in [&ladder.bids, &ladder.offers] {
            assert!((usd(&side[0]) - 40.0).abs() < 0.2 && (usd(&side[1]) - 60.0).abs() < 0.2, "{side:?}");
        }
        // Long $225 of the $300 limit: the bid side shrinks to a quarter, and a rung under $12 goes
        let long = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&cc_view(Some(&book), base + 225.0 / CC_USD / 13.4, base)));
        assert_eq!(long.bids.len(), 1, "{long:?}");
        assert!((usd(&long.bids[0]) - 15.0).abs() < 0.2, "{long:?}");
    }

    fn rest(d: Decision) -> Ladder {
        match d {
            Decision::Rest(l) => l,
            other => panic!("{other:?}"),
        }
    }

    /// Where the grid's delta rounding puts a level.
    fn delta_price(side: OrderType, level: &PriceLevel, r: f64, tick: f64) -> f64 {
        let steps = r * (1.0 + level.delta_percent / 100.0) / tick;
        (if side == OrderType::Bid { steps.floor() } else { steps.ceil() }) * tick
    }

    /// The rungs' exact prices; each delta rounds back to its own price.
    fn prices(side: OrderType, rungs: &[Rung], r: f64, tick: f64) -> Vec<f64> {
        rungs
            .iter()
            .map(|rung| {
                let exact = rung.price.to_f64().unwrap();
                assert!((delta_price(side, &rung.level, r, tick) - exact).abs() < 1e-12, "{side:?} {rung:?}");
                exact
            })
            .collect()
    }

    #[test]
    fn a_flat_maker_quotes_level_zero_inside_the_lp_on_both_sides() {
        let book = lp_book(0.0016);
        let mut mm = MarketMaker::new(MarketMakerConfig::default());
        let ladder = rest(mm.decide(&view(Some(&book), 1_250_000.0, 1_250_000.0)));
        assert_eq!((ladder.bids.len(), ladder.offers.len()), (2, 2));
        let bids = prices(OrderType::Bid, &ladder.bids, 0.0016, 1e-7);
        let offers = prices(OrderType::Offer, &ladder.offers, 0.0016, 1e-7);
        // Level 0 is 10 bps out, inside the LP at 15 bps; level 1 is 20 bps out
        assert!((bids[0] - 0.0015984).abs() < 1e-12 && (offers[0] - 0.0016016).abs() < 1e-12, "{bids:?} {offers:?}");
        assert!(bids[1] < bids[0] && offers[1] > offers[0]);
        assert!(bids[0] > book.lp_bid.as_ref().unwrap().price && offers[0] < book.lp_ask.as_ref().unwrap().price);
        // $40 then $60 a rung
        assert_eq!((ladder.bids[0].level.quantity.as_str(), ladder.bids[1].level.quantity.as_str()), ("25100", "37600"));
        // An identical cycle gives the identical ladder
        assert_eq!(rest(mm.decide(&view(Some(&book), 1_250_000.0, 1_250_000.0))), ladder);
    }

    #[test]
    fn level_zero_outside_the_lp_joins_one_tick_inside_capped_by_min_edge() {
        let cfg = MarketMakerConfig { half_spread_bps: 20.0, ..Default::default() };
        let level0 = |book: &BookTop, cfg: &MarketMakerConfig| {
            let ladder = rest(MarketMaker::new(cfg.clone()).decide(&view(Some(book), 1e6, 1e6)));
            let bid = prices(OrderType::Bid, &ladder.bids, 0.0016, 1e-7)[0];
            (bid, prices(OrderType::Offer, &ladder.offers, 0.0016, 1e-7)[0])
        };
        // 20 bps sits outside the LP's 15 bps: LP bid + 1 tick, LP offer - 1 tick
        let (bid, offer) = level0(&lp_book(0.0016), &cfg);
        assert!((bid - 0.0015977).abs() < 1e-12 && (offer - 0.0016023).abs() < 1e-12, "{bid} {offer}");
        let (bid, offer) = level0(&lp_book(0.0016), &MarketMakerConfig { lp_inside: false, ..cfg.clone() });
        assert!((bid - 0.0015968).abs() < 1e-12 && (offer - 0.0016032).abs() < 1e-12, "{bid} {offer}");
        // With the LP only 2 bps out, joining stops at the 3 bps edge from the reference
        let tight = BookTop { lp_bid: Some(lp_px(0.0016 * 0.9998)), lp_ask: Some(lp_px(0.0016 * 1.0002)), ..lp_book(0.0016) };
        let (bid, offer) = level0(&tight, &cfg);
        assert!((bid - 0.0015995).abs() < 1e-12 && (offer - 0.0016005).abs() < 1e-12, "{bid} {offer}");
    }

    #[test]
    fn a_rung_under_twelve_dollars_is_skipped_not_the_whole_side() {
        let book = lp_book(0.0016);
        // Long $225 of $300: bids at a quarter, so level 0 ($10) goes and level 1 ($15) stays
        let ladder = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&view(Some(&book), 1_390_625.0, 1_250_000.0)));
        assert_eq!(ladder.bids.len(), 1, "{ladder:?}");
        assert_eq!(ladder.bids[0].level.quantity, "9500");
        let bid = prices(OrderType::Bid, &ladder.bids, 0.0016, 1e-7)[0];
        assert!((bid - 0.001595).abs() < 1e-12, "{bid}");
        assert_eq!(ladder.offers.len(), 2);
    }

    #[test]
    fn a_reducing_offer_crosses_exactly_at_the_agent_bid() {
        let mut book = lp_book(0.0016);
        book.agent_bid = Some(Level { price: 0.001601, price_str: "0.001601".into(), qty: 1e4, count: 1 });
        // Long $150: offers reduce the position, and level 0 sits below that bid
        let ladder = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&view(Some(&book), 1_343_750.0, 1_250_000.0)));
        let offers = prices(OrderType::Offer, &ladder.offers, 0.0016, 1e-7);
        assert!((offers[0] - 0.001601).abs() < 1e-12, "{offers:?}");
        assert!(offers[1] > 0.001601, "{offers:?}");
    }

    #[test]
    fn inventory_skews_shrinks_and_drops_the_growing_side() {
        let book = lp_book(0.0016);
        let mut mm = MarketMaker::new(MarketMakerConfig::default());
        // Long $150 of a $300 limit: bids at half size, fair shifted down
        let long = rest(mm.decide(&view(Some(&book), 1_250_000.0 + 93_750.0, 1_250_000.0)));
        let flat = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&view(Some(&book), 1_250_000.0, 1_250_000.0)));
        assert_eq!(long.bids.first().map(|r| r.level.quantity.as_str()), Some("12600"));
        assert!(long.offers[0].level.delta_percent < flat.offers[0].level.delta_percent);
        // At the limit: no bids at all
        let full = rest(mm.decide(&view(Some(&book), 1_250_000.0 + 187_500.0, 1_250_000.0)));
        assert!(full.bids.is_empty() && !full.offers.is_empty());
        let short = rest(mm.decide(&view(Some(&book), 1_250_000.0 - 187_500.0, 1_250_000.0)));
        assert!(short.offers.is_empty() && !short.bids.is_empty());
    }

    #[test]
    fn a_jump_withdraws_the_ladder_for_two_cycles() {
        let mut mm = MarketMaker::new(MarketMakerConfig::default());
        let book = lp_book(0.0016);
        assert!(!rest(mm.decide(&view(Some(&book), 1e6, 1e6))).bids.is_empty());
        let mut jumped = view(Some(&book), 1e6, 1e6);
        jumped.reference = 0.0016 * 1.004;
        assert_eq!(rest(mm.decide(&jumped)), Ladder::default());
        assert_eq!(rest(mm.decide(&jumped)), Ladder::default());
        assert!(!rest(mm.decide(&jumped)).bids.is_empty());
        // No book, no usable balances: hold
        assert_eq!(mm.decide(&view(None, 1e6, 1e6)), Decision::Hold);
    }

    #[test]
    fn funds_trim_outer_rungs_and_fills_widen_the_spread() {
        let book = lp_book(0.0016);
        let mut v = view(Some(&book), 1e6, 1e6);
        v.inventory.quote_avail = 50.0;
        v.inventory.base_avail = 30_000.0;
        let ladder = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&v));
        assert_eq!((ladder.bids.len(), ladder.offers.len()), (1, 1));
        let mut v = view(Some(&book), 1e6, 1e6);
        v.fills_last_hour = 61;
        let mut mm = MarketMaker::new(MarketMakerConfig::default());
        let wide = rest(mm.decide(&v));
        assert!(wide.bids[0].level.delta_percent < -0.39, "{:?}", wide.bids[0]);
        assert!(mm.status().contains("half spread 40"));
    }

    /// Exact grid prices just outside +-15 bps of `p`, as the server sends them.
    pub(crate) fn lp_strings(p: f64, tick: f64) -> (Decimal, Decimal, Decimal) {
        let t = Decimal::from_f64_retain(tick).unwrap().round_dp(10);
        let at = |x: f64, up: bool| {
            let n = Decimal::from_f64_retain(x).unwrap() / t;
            (if up { n.ceil() } else { n.floor() }) * t
        };
        (at(p * 0.9985, false), at(p * 1.0015, true), t)
    }

    pub(crate) fn lp_level(d: Decimal) -> LpPrice {
        lp_of(d.to_f64().unwrap(), &d.normalize().to_string())
    }

    /// Prices from 0.0016 to 1.2e6 on ticks from 1e-10 to 1e-2, where the LP spans many ticks.
    pub(crate) fn scales() -> Vec<(f64, f64)> {
        let ticks = (2..=10).map(|e| 10f64.powi(-e));
        [0.0016, 1658.0941225, 1.2e6]
            .into_iter()
            .flat_map(|p| ticks.clone().map(move |t| (p, t)))
            .filter(|(p, t)| p / t >= 1e4)
            .collect()
    }

    #[test]
    fn one_tick_inside_the_lp_survives_the_round_trip_at_every_scale() {
        use crate::order_manager::{grid_ops, GridOp, SideAction};
        for (p, tick) in scales() {
            let (lp_bid, lp_ask, t) = lp_strings(p, tick);
            let book = BookTop { lp_bid: Some(lp_level(lp_bid)), lp_ask: Some(lp_level(lp_ask)), ..Default::default() };
            let cfg = MarketMakerConfig { half_spread_bps: 20.0, levels: 1, ..Default::default() };
            let base = 1000.0 / p;
            let mut v = view(Some(&book), base, base);
            (v.reference, v.tick, v.lot_exp) = (p, tick, super::super::sizing::lot_exp(p));
            let ladder = rest(MarketMaker::new(cfg).decide(&v));
            let plan = (SideAction::Replace, SideAction::Replace);
            let placed: Vec<(OrderType, Decimal)> = grid_ops(&ladder.bids, &ladder.offers, p, tick, &[], plan)
                .into_iter()
                .filter_map(|op| match op {
                    GridOp::Place { side, price, .. } => Some((side, Decimal::from_str_exact(&price).unwrap())),
                    GridOp::Cancel { .. } => None,
                })
                .collect();
            let at = |side| placed.iter().find(|(s, _)| *s == side).map(|(_, d)| *d);
            assert_eq!(placed.len(), 2, "{p} {tick}");
            assert_eq!((at(OrderType::Bid), at(OrderType::Offer)), (Some(lp_bid + t), Some(lp_ask - t)), "{p} {tick}");
        }
    }

    #[test]
    fn maker_prices_follow_p5() {
        let tick = 1e-7;
        let mut book = lp_book(0.0016);
        book.agent_ask = Some(Level { price: 0.0016010, price_str: "0.001601".into(), qty: 1e4, count: 1 });
        book.agent_bid = Some(Level { price: 0.0015990, price_str: "0.001599".into(), qty: 1e4, count: 1 });
        let make_price = |side, target, book: &BookTop, tick: f64, cross_ok| {
            make_price(side, target, book, tick, cross_ok).map_or(f64::NAN, |n| n as f64 * tick)
        };
        // Through an agent offer only when reducing: then exactly at it
        assert!((make_price(OrderType::Bid, 0.00161, &book, tick, true) - 0.001601).abs() < 1e-12);
        assert!((make_price(OrderType::Bid, 0.00161, &book, tick, false) - 0.0016009).abs() < 1e-12);
        assert!((make_price(OrderType::Offer, 0.00159, &book, tick, false) - 0.0015991).abs() < 1e-12);
        // A reducing offer through an agent bid sits exactly at it, so it cannot sweep lower bids
        assert!((make_price(OrderType::Offer, 0.00159, &book, tick, true) - 0.001599).abs() < 1e-12);
        // Never through the LP
        let lp_only = lp_book(0.0016);
        assert!(make_price(OrderType::Bid, 0.0017, &lp_only, tick, true) < lp_only.lp_ask.unwrap().price);
        // The delta rounds back to the same tick
        for p in [0.0015984, 0.0016016, 0.0015001] {
            for side in [OrderType::Bid, OrderType::Offer] {
                let level = PriceLevel { delta_percent: rung_delta(side, p, tick, 0.0016), quantity: "1".into() };
                let back = delta_price(side, &level, 0.0016, tick);
                assert!((back - p).abs() < 1e-12, "{side:?} {p} {back}");
            }
        }
    }
}
