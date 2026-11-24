//! market_maker: a two-sided ladder around an inventory-skewed fair price.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;

use super::book::BookTop;
use super::sizing::{order_qty, MIN_ORDER_USD};
use super::{bps, clampf, BookNeed, Decide, Decision, Ladder, MarketView};
use crate::config::{MarketMakerConfig, PriceLevel};
use crate::num::Px;

/// After a jump the ladder stays withdrawn for this many cycles.
const PULL_CYCLES: u32 = 2;
/// How long a burst of fills widens the spread.
const THROTTLE_FOR: Duration = Duration::from_secs(600);
/// Funding checks keep this sliver of headroom against float noise.
const FUNDS_SLACK: f64 = 1e-9;

/// P5 for one maker price: on the tick grid, never through the LP, and through an
/// agent quote only when `cross_ok`, then exactly at it so one level can match.
pub(crate) fn make_price(side: OrderType, target: f64, book: &BookTop, tick: f64, cross_ok: bool) -> f64 {
    if side == OrderType::Bid {
        let mut p = target;
        if let Some(lp_ask) = book.lp_ask {
            p = p.min(lp_ask - tick);
        }
        if let Some(ask) = book.agent_ask.as_ref().map(|l| l.price).filter(|a| p >= *a) {
            p = if cross_ok { ask } else { ask - tick };
        }
        (p / tick + 1e-9).floor() * tick
    } else {
        let mut p = target;
        if let Some(lp_bid) = book.lp_bid {
            p = p.max(lp_bid + tick);
        }
        if let Some(bid) = book.agent_bid.as_ref().map(|l| l.price).filter(|b| p <= *b) {
            p = if cross_ok { bid } else { bid + tick };
        }
        (p / tick - 1e-9).ceil() * tick
    }
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
    levels: Vec<PriceLevel>,
    need: f64,
    usd: f64,
    prev: Option<f64>,
    done: bool,
}

impl SideBuilder {
    fn new(side: OrderType) -> Self {
        Self { side, levels: Vec::new(), need: 0.0, usd: 0.0, prev: None, done: false }
    }

    /// Adds a rung unless it breaks the side's order or budgets; a rung under $12 is
    /// skipped, and the first funds or position miss ends the side.
    fn push(&mut self, price: f64, usd: f64, v: &MarketView<'_>, funds: f64, room_usd: f64) {
        if self.done || usd < MIN_ORDER_USD {
            return;
        }
        let tick = v.tick;
        let price = match (self.side, self.prev) {
            (OrderType::Bid, Some(prev)) => price.min(prev - tick),
            (_, Some(prev)) => price.max(prev + tick),
            _ => price,
        };
        let Some(qty) = (price > 0.0).then(|| order_qty(usd, price, v.floor_value, v.lot_exp)).flatten() else {
            self.done = true;
            return;
        };
        let qf = qty.to_f64().unwrap_or(f64::INFINITY);
        let delta = rung_delta(self.side, price, tick, v.reference);
        // The same formula grid funding checks, so a trimmed side never reads as short
        let need = if self.side == OrderType::Bid { qf * v.reference * (1.0 + delta / 100.0) } else { qf };
        let order_usd = qf * price;
        if self.need + need > funds * (1.0 - FUNDS_SLACK) || self.usd + order_usd > room_usd {
            self.done = true;
            return;
        }
        self.need += need;
        self.usd += order_usd;
        self.prev = Some(price);
        self.levels.push(PriceLevel { delta_percent: delta, quantity: qty.to_string() });
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
        let position_usd = (v.inventory.base - v.start_base) * r;
        let q = (clampf(position_usd / c.max_inventory_usd, -1.0, 1.0) * 4.0).round() / 4.0;
        let fair = r * (1.0 - c.skew_bps / 10_000.0 * q);
        self.last = Some((q, fair, h));

        let improve = f64::from(c.lp_improve_ticks) * v.tick;
        let edge = c.min_edge_bps / 10_000.0;
        let mut bids = SideBuilder::new(OrderType::Bid);
        let mut offers = SideBuilder::new(OrderType::Offer);
        let bid_room = c.max_inventory_usd - position_usd;
        let offer_room = c.max_inventory_usd + position_usd;
        for i in 0..c.levels {
            let i_f = f64::from(i);
            let dist = (h + i_f * c.level_step_bps) / 10_000.0;
            let mut bid = fair * (1.0 - dist);
            let mut offer = fair * (1.0 + dist);
            // While throttled the wide spread wins over joining the LP
            if i == 0 && c.lp_inside && !throttled {
                if let Some(lp_bid) = book.lp_bid.filter(|b| bid <= *b && q <= 0.5) {
                    bid = (lp_bid + improve).min(r * (1.0 - edge));
                }
                if let Some(lp_ask) = book.lp_ask.filter(|a| offer >= *a && q >= -0.5) {
                    offer = (lp_ask - improve).max(r * (1.0 + edge));
                }
            }
            let size = c.level_usd * c.size_growth.powi(i32::try_from(i).unwrap_or(0));
            // Shrink the side that would grow the position; drop it at the limit
            if q < 1.0 {
                let usd = if q > 0.0 { size * (1.0 - q) } else { size };
                let p = make_price(OrderType::Bid, bid, book, v.tick, q < 0.0);
                bids.push(p, usd, v, v.inventory.quote_avail, bid_room);
            }
            if q > -1.0 {
                let usd = if q < 0.0 { size * (1.0 + q) } else { size };
                let p = make_price(OrderType::Offer, offer, book, v.tick, q > 0.0);
                offers.push(p, usd, v, v.inventory.base_avail, offer_room);
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

    pub(crate) fn lp_book(r: f64) -> BookTop {
        BookTop { agent_bid: None, agent_ask: None, lp_bid: Some(r * 0.9985), lp_ask: Some(r * 1.0015) }
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
        }
    }

    fn rest(d: Decision) -> Ladder {
        match d {
            Decision::Rest(l) => l,
            other => panic!("{other:?}"),
        }
    }

    fn prices(side: OrderType, levels: &[PriceLevel], r: f64, tick: f64) -> Vec<f64> {
        levels
            .iter()
            .map(|l| {
                let steps = r * (1.0 + l.delta_percent / 100.0) / tick;
                (if side == OrderType::Bid { steps.floor() } else { steps.ceil() }) * tick
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
        assert!(bids[0] > book.lp_bid.unwrap() && offers[0] < book.lp_ask.unwrap());
        // $40 then $60 a rung
        assert_eq!((ladder.bids[0].quantity.as_str(), ladder.bids[1].quantity.as_str()), ("25100", "37600"));
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
        let tight = BookTop { lp_bid: Some(0.0016 * 0.9998), lp_ask: Some(0.0016 * 1.0002), ..lp_book(0.0016) };
        let (bid, offer) = level0(&tight, &cfg);
        assert!((bid - 0.0015995).abs() < 1e-12 && (offer - 0.0016005).abs() < 1e-12, "{bid} {offer}");
    }

    #[test]
    fn a_rung_under_twelve_dollars_is_skipped_not_the_whole_side() {
        let book = lp_book(0.0016);
        // Long $225 of $300: bids at a quarter, so level 0 ($10) goes and level 1 ($15) stays
        let ladder = rest(MarketMaker::new(MarketMakerConfig::default()).decide(&view(Some(&book), 1_390_625.0, 1_250_000.0)));
        assert_eq!(ladder.bids.len(), 1, "{ladder:?}");
        assert_eq!(ladder.bids[0].quantity, "9500");
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
        assert_eq!(long.bids.first().map(|l| l.quantity.as_str()), Some("12600"));
        assert!(long.offers[0].delta_percent < flat.offers[0].delta_percent);
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
        assert!(wide.bids[0].delta_percent < -0.39, "{:?}", wide.bids[0]);
        assert!(mm.status().contains("half spread 40"));
    }

    #[test]
    fn maker_prices_follow_p5() {
        let tick = 1e-7;
        let mut book = lp_book(0.0016);
        book.agent_ask = Some(Level { price: 0.0016010, price_str: "0.001601".into(), qty: 1e4, count: 1 });
        book.agent_bid = Some(Level { price: 0.0015990, price_str: "0.001599".into(), qty: 1e4, count: 1 });
        // Through an agent offer only when reducing: then exactly at it
        assert!((make_price(OrderType::Bid, 0.00161, &book, tick, true) - 0.001601).abs() < 1e-12);
        assert!((make_price(OrderType::Bid, 0.00161, &book, tick, false) - 0.0016009).abs() < 1e-12);
        assert!((make_price(OrderType::Offer, 0.00159, &book, tick, false) - 0.0015991).abs() < 1e-12);
        // A reducing offer through an agent bid sits exactly at it, so it cannot sweep lower bids
        assert!((make_price(OrderType::Offer, 0.00159, &book, tick, true) - 0.001599).abs() < 1e-12);
        // Never through the LP
        let lp_only = lp_book(0.0016);
        assert!(make_price(OrderType::Bid, 0.0017, &lp_only, tick, true) < lp_only.lp_ask.unwrap());
        // The delta rounds back to the same tick
        for p in [0.0015984, 0.0016016, 0.0015001] {
            for side in [OrderType::Bid, OrderType::Offer] {
                let back = prices(side, &[PriceLevel { delta_percent: rung_delta(side, p, tick, 0.0016), quantity: "1".into() }], 0.0016, tick);
                assert!((back[0] - p).abs() < 1e-12, "{side:?} {p} {back:?}");
            }
        }
    }
}
