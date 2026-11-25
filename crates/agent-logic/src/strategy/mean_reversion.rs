//! mean_reversion: fades moves away from an EMA, and steps aside when a move turns into a trend.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;
use tracing::warn;

use super::ema::Ema;
use super::maker::{make_price, rung_delta};
use super::sizing::{order_qty, ticks_price, MIN_ORDER_USD};
use super::{bps, BookNeed, Decide, Decision, Ladder, MarketView, Rung, TakeTif, Want};
use crate::config::{MeanReversionConfig, PriceLevel, StrategyMode};

/// Target position as a share of the limit, from the deviation in bps; in eighths,
/// so the target holds still while the deviation wanders inside one step.
pub(crate) fn target_share(dev: f64, entry: f64, full: f64) -> f64 {
    if dev.is_nan() || dev.abs() < entry || full <= entry {
        return 0.0;
    }
    let share = ((dev.abs() - entry) / (full - entry)).min(1.0);
    -dev.signum() * (share * 8.0).round() / 8.0
}

pub(crate) struct MeanReversion {
    cfg: MeanReversionConfig,
    market_id: String,
    ema: Ema,
    paused_until: Option<Instant>,
    stops: u32,
    last: Option<(f64, f64)>,
    /// Side of the maker's last rung, if it asked for one
    rested: Option<OrderType>,
}

impl MeanReversion {
    pub(crate) fn new(cfg: MeanReversionConfig, market_id: &str) -> Self {
        let ema = Ema::new(cfg.ema_secs as f64);
        Self { cfg, market_id: market_id.to_string(), ema, paused_until: None, stops: 0, last: None, rested: None }
    }

    fn idle(&self) -> Decision {
        match self.cfg.mode {
            StrategyMode::Taker => Decision::Take(None),
            StrategyMode::Maker => Decision::Rest(Ladder::default()),
        }
    }

    /// An EMA at `mean` that has been warm since `since`.
    #[cfg(test)]
    pub(crate) fn prime(&mut self, mean: f64, since: Instant, now: Instant) {
        self.ema = Ema::primed(self.cfg.ema_secs as f64, mean, since, now);
    }

    fn choose(&mut self, v: &MarketView<'_>) -> Decision {
        let c = &self.cfg;
        let r = v.reference;
        if !self.ema.warm(v.now, Duration::from_secs(c.warmup_secs)) {
            return self.idle();
        }
        let Some(mean) = self.ema.value().filter(|m| *m > 0.0) else { return self.idle() };
        if !v.inventory.usable || r.is_nan() || r <= 0.0 {
            return Decision::Hold;
        }
        let dev = bps(r, mean);
        let paused = self.paused_until.is_some_and(|t| v.now < t);
        // Too far to be noise: flatten, and open nothing new for a while
        if dev.abs() > c.stop_bps {
            if !paused {
                self.stops = self.stops.saturating_add(1);
                warn!(
                    "Strategy {} [mean_reversion]: a {:+.1} bps move past the {} bps stop; flattening and pausing {} s",
                    self.market_id, dev, c.stop_bps, c.pause_after_stop_secs
                );
            }
            self.paused_until = v.now.checked_add(Duration::from_secs(c.pause_after_stop_secs));
        }
        let paused = self.paused_until.is_some_and(|t| v.now < t);
        let target_usd = if paused { 0.0 } else { target_share(dev, c.entry_bps, c.full_bps) * c.max_position_usd };
        self.last = Some((dev, target_usd));
        let gap = v.start_base + v.in_quote(target_usd) / r - v.inventory.base;
        let min_order = v.in_quote(MIN_ORDER_USD);
        if gap.abs() * r < min_order {
            return self.idle();
        }
        let side = if gap > 0.0 { OrderType::Bid } else { OrderType::Offer };
        let clip = (gap.abs() * r).min(v.in_quote(c.clip_usd));
        match c.mode {
            StrategyMode::Taker => {
                if !v.can_take || v.activity.open_settlements >= self.max_inflight() {
                    return Decision::Take(None);
                }
                Decision::Take(Some(Want {
                    side,
                    max_quantity: clip / r,
                    max_deviation_percent: c.max_slip_bps / 100.0,
                    tif: TakeTif::Ioc,
                }))
            }
            StrategyMode::Maker => {
                // A clip waits for the last one to settle; after a stop or a turn of side it goes
                if !v.can_take {
                    let withdraw = paused || self.rested != Some(side);
                    return if withdraw { Decision::Rest(Ladder::default()) } else { Decision::Hold };
                }
                let Some(book) = v.book else { return Decision::Hold };
                if v.tick.is_nan() || v.tick <= 0.0 {
                    return Decision::Hold;
                }
                let offset = c.maker_offset_bps / 10_000.0;
                let aim = if side == OrderType::Bid { r * (1.0 - offset) } else { r * (1.0 + offset) };
                // Through an agent quote only when the order brings the position back
                let (base, start) = (v.inventory.base, v.start_base);
                let reducing = if side == OrderType::Bid { base < start } else { base > start };
                let Some(n) = make_price(side, aim, book, v.tick, reducing) else { return self.idle() };
                let Some(exact) = ticks_price(n, v.tick) else { return self.idle() };
                let price = n as f64 * v.tick;
                let Some(qty) = order_qty(clip, min_order, price, v.floor_value, v.lot_exp) else { return self.idle() };
                let qf = qty.to_f64().unwrap_or(f64::INFINITY);
                let delta = rung_delta(side, price, v.tick, r);
                let level = PriceLevel { delta_percent: delta, quantity: qty.to_string() };
                let rung = vec![Rung { level, price: exact }];
                if side == OrderType::Bid {
                    if qf * r * (1.0 + delta / 100.0) > v.inventory.quote_avail {
                        return self.idle();
                    }
                    Decision::Rest(Ladder { bids: rung, offers: Vec::new() })
                } else {
                    if qf > v.inventory.base_avail {
                        return self.idle();
                    }
                    Decision::Rest(Ladder { bids: Vec::new(), offers: rung })
                }
            }
        }
    }
}

impl Decide for MeanReversion {
    fn observe(&mut self, price: f64, now: Instant) {
        self.ema.update(price, now);
    }

    fn book_need(&self) -> BookNeed {
        match self.cfg.mode {
            StrategyMode::Taker => BookNeed::WhenTaking,
            StrategyMode::Maker => BookNeed::Always,
        }
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        let decision = self.choose(v);
        if let Decision::Rest(ladder) = &decision {
            self.rested = match (ladder.bids.is_empty(), ladder.offers.is_empty()) {
                (false, _) => Some(OrderType::Bid),
                (true, false) => Some(OrderType::Offer),
                (true, true) => None,
            };
        }
        decision
    }

    fn status(&self) -> String {
        let paused = if self.paused_until.is_some() { format!(" stops {}", self.stops) } else { String::new() };
        match self.last {
            Some((dev, target)) => format!("deviation {dev:+.1} bps target {target:+.2}{paused}"),
            None => "warming up".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::book::Level;
    use super::super::maker::tests::{lp_book, view};
    use super::*;

    fn mr(mode: StrategyMode) -> MeanReversion {
        MeanReversion::new(MeanReversionConfig { mode, ..Default::default() }, "HTEST-USDC")
    }

    /// The EMA `dev` bps under the reference of `view`, warm for 700 s.
    fn at_dev(m: &mut MeanReversion, dev: f64, now: Instant) {
        m.prime(0.0016 / (1.0 + dev / 10_000.0), now.checked_sub(Duration::from_secs(700)).unwrap(), now);
    }

    fn want(d: Decision) -> Want {
        match d {
            Decision::Take(Some(w)) => w,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_target_grows_from_entry_to_full_in_eighths() {
        assert_eq!(target_share(14.9, 15.0, 60.0), 0.0);
        assert_eq!(target_share(37.5, 15.0, 60.0), -0.5);
        assert_eq!(target_share(-37.5, 15.0, 60.0), 0.5);
        assert_eq!(target_share(-90.0, 15.0, 60.0), 1.0);
        assert_eq!(target_share(20.0, 15.0, 60.0), -0.125);
        assert_eq!(target_share(f64::NAN, 15.0, 60.0), 0.0);
    }

    #[test]
    fn nothing_trades_before_the_warm_up() {
        let mut m = mr(StrategyMode::Taker);
        let mut v = view(None, 1e6, 1e6);
        m.observe(0.0015, v.now);
        v.now += Duration::from_secs(599);
        assert_eq!(m.decide(&v), Decision::Take(None));
        assert_eq!(m.status(), "warming up");
        v.now += Duration::from_secs(1);
        // 0.0016 over a mean near 0.0015 is far past the stop: flat, so nothing to trade
        assert_eq!(m.decide(&v), Decision::Take(None));
        assert_eq!(m.stops, 1);
    }

    #[test]
    fn a_taker_fades_the_deviation_with_clips() {
        let mut m = mr(StrategyMode::Taker);
        let v = view(None, 1e6, 1e6);
        // 37.5 bps over the mean: a $100 short, sold in $30 clips
        at_dev(&mut m, 37.5, v.now);
        let w = want(m.decide(&v));
        assert_eq!((w.side, w.tif), (OrderType::Offer, TakeTif::Ioc));
        assert!((w.max_quantity - 30.0 / 0.0016).abs() < 1e-6);
        // Short $95 already: the last $5 is under the $12 minimum
        let mut near = view(None, 1e6 - 95.0 / 0.0016, 1e6);
        near.now = v.now;
        assert_eq!(m.decide(&near), Decision::Take(None));
        // Inside the entry band the target is flat again, so the short is bought back
        at_dev(&mut m, 10.0, v.now);
        assert_eq!(want(m.decide(&near)).side, OrderType::Bid);
        let mut shut = view(None, 1e6, 1e6);
        shut.can_take = false;
        at_dev(&mut m, 37.5, shut.now);
        assert_eq!(m.decide(&shut), Decision::Take(None));
    }

    #[test]
    fn a_trend_past_the_stop_flattens_and_pauses() {
        let mut m = mr(StrategyMode::Taker);
        // Short $100 from fading a rise, then the rise runs to 160 bps
        let mut v = view(None, 1e6 - 62_500.0, 1e6);
        let t0 = v.now;
        at_dev(&mut m, 160.0, t0);
        let w = want(m.decide(&v));
        assert_eq!(w.side, OrderType::Bid, "flattening buys the short back");
        assert_eq!(m.stops, 1);
        // Back to 40 bps within the pause: still flat, so no new short
        v.inventory.base = 1e6;
        v.now = t0 + Duration::from_secs(600);
        at_dev(&mut m, 40.0, v.now);
        assert_eq!(m.decide(&v), Decision::Take(None));
        assert!(m.status().contains("stops 1"), "{}", m.status());
        // After 15 min it fades again
        v.now = t0 + Duration::from_secs(901);
        at_dev(&mut m, 40.0, v.now);
        assert_eq!(want(m.decide(&v)).side, OrderType::Offer);
        assert_eq!(m.stops, 1);
    }

    #[test]
    fn a_maker_rests_one_clip_four_bps_inside() {
        let book = lp_book(0.0016);
        let mut m = mr(StrategyMode::Maker);
        assert_eq!(m.book_need(), BookNeed::Always);
        let v = view(Some(&book), 1e6, 1e6);
        at_dev(&mut m, -37.5, v.now);
        let Decision::Rest(ladder) = m.decide(&v) else { panic!() };
        assert!(ladder.offers.is_empty() && ladder.bids.len() == 1, "{ladder:?}");
        // 4 bps under 0.0016 is 0.00159936, on the tick grid 0.0015993
        let price = 0.0016 * (1.0 + ladder.bids[0].level.delta_percent / 100.0);
        assert!(((price / 1e-7).floor() * 1e-7 - 0.0015993).abs() < 1e-12, "{price}");
        assert_eq!(ladder.bids[0].price.normalize().to_string(), "0.0015993");
        assert_eq!(ladder.bids[0].level.quantity, "18800");
        // Settling: the clip stays as it is
        let mut wait = view(Some(&book), 1e6, 1e6);
        wait.can_take = false;
        assert_eq!(m.decide(&wait), Decision::Hold);
        // Back inside the band: nothing rests
        at_dev(&mut m, 5.0, v.now);
        assert_eq!(m.decide(&v), Decision::Rest(Ladder::default()));
    }

    // A clip used to stay while settling even after a stop or a turn of side
    #[test]
    fn a_shut_gate_keeps_the_clip_unless_the_side_turned_or_a_stop_hit() {
        let book = lp_book(0.0016);
        let mut m = mr(StrategyMode::Maker);
        let v = view(Some(&book), 1e6, 1e6);
        let mut shut = view(Some(&book), 1e6, 1e6);
        (shut.now, shut.can_take) = (v.now, false);
        // Under the mean a bid rests, and stays while it settles
        at_dev(&mut m, -37.5, v.now);
        assert!(matches!(m.decide(&v), Decision::Rest(l) if l.bids.len() == 1 && l.offers.is_empty()));
        assert_eq!(m.decide(&shut), Decision::Hold);
        // Over it the bid would grow the wrong side: withdrawn
        at_dev(&mut m, 37.5, v.now);
        assert_eq!(m.decide(&shut), Decision::Rest(Ladder::default()));
        // Long $100 in the band, an offer reduces; past the stop it is withdrawn too
        let mut long = view(Some(&book), 1e6 + 62_500.0, 1e6);
        long.now = v.now;
        at_dev(&mut m, 5.0, v.now);
        assert!(matches!(m.decide(&long), Decision::Rest(l) if l.offers.len() == 1 && l.bids.is_empty()));
        long.can_take = false;
        assert_eq!(m.decide(&long), Decision::Hold);
        at_dev(&mut m, 160.0, v.now);
        assert_eq!(m.decide(&long), Decision::Rest(Ladder::default()));
        assert_eq!(m.stops, 1);
    }

    // Flat at the start neither side reduces, so an offer used to cross an agent bid
    #[test]
    fn at_the_start_an_offer_stays_a_tick_above_an_agent_bid() {
        let mut book = lp_book(0.0016);
        book.agent_bid = Some(Level { price: 0.0016008, price_str: "0.0016008".into(), qty: 50_000.0, count: 1 });
        let mut m = mr(StrategyMode::Maker);
        let mut offer_at = |base: f64| {
            let v = view(Some(&book), base, 1e6);
            at_dev(&mut m, 37.5, v.now);
            match m.decide(&v) {
                Decision::Rest(l) if l.bids.is_empty() && l.offers.len() == 1 => {
                    (0.0016 * (1.0 + l.offers[0].level.delta_percent / 100.0) / 1e-7).ceil() * 1e-7
                }
                other => panic!("{other:?}"),
            }
        };
        assert!((offer_at(1e6) - 0.0016009).abs() < 1e-12);
        // Long, the offer reduces and goes through at the agent bid
        assert!((offer_at(1e6 + 62_500.0) - 0.0016008).abs() < 1e-12);
    }
}
