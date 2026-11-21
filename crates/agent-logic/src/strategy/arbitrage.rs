//! arbitrage: takes one agent level priced through the reference while the
//! reference is fresh and stable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;

use super::sizing::MIN_ORDER_USD;
use super::{bps, BookNeed, Decide, Decision, MarketView, TakeTif, Want};
use crate::config::ArbitrageConfig;

/// Readings this recent judge whether the reference is stable...
const STABLE_WINDOW: Duration = Duration::from_secs(120);
/// ...once the strategy has watched it this long.
const STABLE_AFTER: Duration = Duration::from_secs(60);

pub(crate) struct Arbitrage {
    cfg: ArbitrageConfig,
    readings: VecDeque<(Instant, f64)>,
    first: Option<Instant>,
    /// Edge of the best agent offer and bid, in bps through the reference
    edges: (Option<f64>, Option<f64>),
    stable: bool,
}

impl Arbitrage {
    pub(crate) fn new(cfg: ArbitrageConfig) -> Self {
        Self { cfg, readings: VecDeque::new(), first: None, edges: (None, None), stable: false }
    }

    /// The recent readings stay within `ref_stable_bps` of each other.
    fn is_stable(&self, now: Instant) -> bool {
        if !self.first.is_some_and(|f| now.saturating_duration_since(f) >= STABLE_AFTER) {
            return false;
        }
        let (lo, hi) = self.readings.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (_, p)| (lo.min(*p), hi.max(*p)));
        lo > 0.0 && bps(hi, lo) <= self.cfg.ref_stable_bps
    }

    /// Moves every reading `by` into the past.
    #[cfg(test)]
    pub(crate) fn age(&mut self, by: Duration) {
        self.first = self.first.and_then(|f| f.checked_sub(by));
        for (at, _) in &mut self.readings {
            *at = at.checked_sub(by).unwrap_or(*at);
        }
    }
}

impl Decide for Arbitrage {
    fn observe(&mut self, price: f64, now: Instant) {
        self.first.get_or_insert(now);
        self.readings.push_back((now, price));
        while self.readings.len() > 1
            && self.readings.front().is_some_and(|(at, _)| now.saturating_duration_since(*at) > STABLE_WINDOW)
        {
            self.readings.pop_front();
        }
    }

    /// The book decides whether there is a trade at all.
    fn book_need(&self) -> BookNeed {
        BookNeed::Always
    }

    fn is_maker(&self) -> bool {
        false
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cfg.cooldown_secs)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        let c = &self.cfg;
        let r = v.reference;
        self.stable = self.is_stable(v.now);
        let Some(book) = v.book else { return Decision::Take(None) };
        if !v.inventory.usable || r.is_nan() || r <= 0.0 {
            return Decision::Take(None);
        }
        let buy_edge = book.agent_ask.as_ref().map(|l| -bps(l.price, r));
        let sell_edge = book.agent_bid.as_ref().map(|l| bps(l.price, r));
        self.edges = (buy_edge, sell_edge);
        if !self.stable || !v.can_take || v.activity.open_settlements >= self.max_inflight() {
            return Decision::Take(None);
        }
        // Past half the position limit only the reducing side trades
        let position = (v.inventory.base - v.start_base) * r;
        let limit = c.max_position_usd;
        let buy_room = if position > 0.5 * limit { 0.0 } else { limit - position };
        let sell_room = if position < -0.5 * limit { 0.0 } else { limit + position };
        let worth = |edge: Option<f64>, room: f64| edge.filter(|e| *e >= c.min_edge_bps && room >= MIN_ORDER_USD);
        let (side, room) = match (worth(buy_edge, buy_room), worth(sell_edge, sell_room)) {
            (Some(b), Some(s)) if s > b => (OrderType::Offer, sell_room),
            (Some(_), _) => (OrderType::Bid, buy_room),
            (None, Some(_)) => (OrderType::Offer, sell_room),
            (None, None) => return Decision::Take(None),
        };
        Decision::Take(Some(Want {
            side,
            max_quantity: room.min(c.clip_usd_max) / r,
            // The edge doubles as the price bound: a buy must sit that far under the reference
            max_deviation_percent: -c.min_edge_bps / 100.0,
            tif: TakeTif::Ioc,
        }))
    }

    fn status(&self) -> String {
        let edge = |e: Option<f64>| e.map_or_else(|| "none".to_string(), |e| format!("{e:+.1} bps"));
        let stable = if self.stable { "stable" } else { "not stable" };
        format!("best buy {} sell {} reference {}", edge(self.edges.0), edge(self.edges.1), stable)
    }
}

#[cfg(test)]
mod tests {
    use super::super::book::{BookTop, Level};
    use super::super::maker::tests::{lp_book, view};
    use super::*;

    fn level(price: f64) -> Level {
        Level { price, price_str: format!("{price}"), qty: 50_000.0, count: 1 }
    }

    /// Watched long enough to judge the reference
    fn watched(prices: &[f64]) -> Arbitrage {
        let mut a = Arbitrage::new(ArbitrageConfig::default());
        let now = Instant::now();
        for p in prices {
            a.observe(*p, now);
        }
        a.age(STABLE_AFTER);
        a
    }

    fn want(d: Decision) -> Want {
        match d {
            Decision::Take(Some(w)) => w,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn it_takes_only_levels_past_the_edge() {
        let mut book = lp_book(0.0016);
        let mut a = watched(&[0.0016]);
        // 7 bps under the reference: a buy of at most $100
        book.agent_ask = Some(level(0.0016 * (1.0 - 0.0007)));
        let w = want(a.decide(&view(Some(&book), 1e6, 1e6)));
        assert_eq!((w.side, w.tif), (OrderType::Bid, TakeTif::Ioc));
        assert!((w.max_quantity - 100.0 / 0.0016).abs() < 1e-6 && (w.max_deviation_percent + 0.06).abs() < 1e-12);
        // 5 bps is not enough on either side
        book.agent_ask = Some(level(0.0016 * (1.0 - 0.0005)));
        book.agent_bid = Some(level(0.0016 * (1.0 + 0.0005)));
        assert_eq!(a.decide(&view(Some(&book), 1e6, 1e6)), Decision::Take(None));
        // A bid 8 bps over it is a sell
        book.agent_bid = Some(level(0.0016 * (1.0 + 0.0008)));
        assert_eq!(want(a.decide(&view(Some(&book), 1e6, 1e6))).side, OrderType::Offer);
        assert!(a.status().contains("sell +8.0 bps") && a.status().ends_with("reference stable"), "{}", a.status());
        // No book, or the gate shut: nothing
        assert_eq!(a.decide(&view(None, 1e6, 1e6)), Decision::Take(None));
        let mut shut = view(Some(&book), 1e6, 1e6);
        shut.can_take = false;
        assert_eq!(a.decide(&shut), Decision::Take(None));
    }

    #[test]
    fn an_unsettled_or_new_reference_is_not_traded() {
        let book = BookTop { agent_ask: Some(level(0.00159)), ..lp_book(0.0016) };
        // 40 bps apart within the window
        let mut moving = watched(&[0.0016, 0.001606]);
        assert_eq!(moving.decide(&view(Some(&book), 1e6, 1e6)), Decision::Take(None));
        assert!(moving.status().ends_with("not stable"));
        // A minute of watching comes first
        let mut fresh = Arbitrage::new(ArbitrageConfig::default());
        let v = view(Some(&book), 1e6, 1e6);
        fresh.observe(0.0016, v.now);
        assert_eq!(fresh.decide(&v), Decision::Take(None));
        // An old reading leaves the window, so a later steady level counts as stable
        moving.age(STABLE_WINDOW);
        moving.observe(0.0016, Instant::now());
        assert_eq!(want(moving.decide(&view(Some(&book), 1e6, 1e6))).side, OrderType::Bid);
    }

    #[test]
    fn past_half_the_limit_it_only_reduces() {
        let mut book = lp_book(0.0016);
        book.agent_ask = Some(level(0.00159));
        book.agent_bid = Some(level(0.00161));
        let mut a = watched(&[0.0016]);
        // Long $80 of $150: buys stop, the sell edge is taken
        let long = view(Some(&book), 1e6 + 50_000.0, 1e6);
        assert_eq!(want(a.decide(&long)).side, OrderType::Offer);
        let mut only_ask = book.clone();
        only_ask.agent_bid = None;
        assert_eq!(a.decide(&view(Some(&only_ask), 1e6 + 50_000.0, 1e6)), Decision::Take(None));
        // Short $140: sells have $10 of room left, under the minimum; buys are capped at $100
        let short = view(Some(&book), 1e6 - 87_500.0, 1e6);
        let w = want(a.decide(&short));
        assert_eq!(w.side, OrderType::Bid);
        assert!((w.max_quantity - 100.0 / 0.0016).abs() < 1e-6);
        // Long $40: room to the limit is $110, so the clip stays at $100
        let w = want(a.decide(&view(Some(&only_ask), 1e6 + 25_000.0, 1e6)));
        assert!((w.max_quantity - 100.0 / 0.0016).abs() < 1e-6);
        // Long $70: a buy may add only $80
        let w = want(a.decide(&view(Some(&only_ask), 1e6 + 43_750.0, 1e6)));
        assert!((w.max_quantity - 80.0 / 0.0016).abs() < 1e-6, "{}", w.max_quantity);
    }
}
