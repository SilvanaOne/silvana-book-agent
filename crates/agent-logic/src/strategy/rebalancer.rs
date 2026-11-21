//! rebalancer: keeps the base share of portfolio value near a target.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;

use super::maker::{make_price, rung_delta};
use super::sizing::{order_qty, MIN_ORDER_USD};
use super::{BookNeed, Decide, Decision, Ladder, MarketView, TakeTif, Want};
use crate::config::{PriceLevel, RebalancerConfig, StrategyMode};
use crate::order_tracker::Fill;

pub(crate) struct Rebalancer {
    cfg: RebalancerConfig,
    active: bool,
    last_fill: Option<(OrderType, Instant)>,
    last_gap: Option<f64>,
}

impl Rebalancer {
    pub(crate) fn new(cfg: RebalancerConfig) -> Self {
        Self { cfg, active: false, last_fill: None, last_gap: None }
    }

    fn idle(&self) -> Decision {
        match self.cfg.mode {
            StrategyMode::Taker => Decision::Take(None),
            StrategyMode::Maker => Decision::Rest(Ladder::default()),
        }
    }
}

impl Decide for Rebalancer {
    fn on_fill(&mut self, fill: &Fill) {
        let side = if fill.side == OrderType::Bid as i32 { OrderType::Bid } else { OrderType::Offer };
        self.last_fill = Some((side, fill.at));
    }

    fn book_need(&self) -> BookNeed {
        match self.cfg.mode {
            StrategyMode::Taker => BookNeed::WhenTaking,
            StrategyMode::Maker => BookNeed::Always,
        }
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.inflight_cap()).unwrap_or(1)
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cfg.cooldown_secs)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        let c = &self.cfg;
        let r = v.reference;
        let target = c.target_share.unwrap_or(0.5);
        let value = v.inventory.base * r + v.inventory.quote;
        if !v.inventory.usable || !(value > 0.0 && r > 0.0) {
            return Decision::Hold;
        }
        let gap = v.inventory.base * r / value - target;
        self.last_gap = Some(gap);
        if !self.active && gap.abs() > c.band_enter {
            self.active = true;
        } else if self.active && gap.abs() < c.band_exit {
            self.active = false;
        }
        if !self.active {
            return self.idle();
        }
        let side = if gap > 0.0 { OrderType::Offer } else { OrderType::Bid };
        let flip_wait = Duration::from_secs(c.flip_cooldown_secs);
        if self.last_fill.is_some_and(|(s, at)| s != side && v.now.saturating_duration_since(at) < flip_wait) {
            return self.idle();
        }
        let budget = c.max_usd_per_hour - v.usd_last_hour;
        if budget < MIN_ORDER_USD {
            return self.idle();
        }
        let usd = (gap.abs() * value).min(c.clip_usd).min(budget).max(MIN_ORDER_USD);
        match c.mode {
            StrategyMode::Taker => {
                if !v.can_take {
                    return Decision::Take(None);
                }
                Decision::Take(Some(Want {
                    side,
                    max_quantity: usd / r,
                    max_deviation_percent: c.max_slip_bps / 100.0,
                    tif: TakeTif::Ioc,
                }))
            }
            StrategyMode::Maker => {
                // The gate holds a clip until the last one settles; the cooldown runs from the
                // last fill, so a resting clip can still be re-priced
                let cooling = self.last_fill.is_some_and(|(_, at)| v.now.saturating_duration_since(at) < self.cooldown());
                if !v.can_take || cooling {
                    return Decision::Hold;
                }
                let Some(book) = v.book else { return Decision::Hold };
                if v.tick.is_nan() || v.tick <= 0.0 {
                    return Decision::Hold;
                }
                let offset = c.maker_offset_bps / 10_000.0;
                let target_price = if side == OrderType::Bid { r * (1.0 - offset) } else { r * (1.0 + offset) };
                let price = make_price(side, target_price, book, v.tick, true);
                let Some(qty) = order_qty(usd, price, v.floor_value, v.lot_exp) else { return self.idle() };
                let qf = qty.to_f64().unwrap_or(f64::INFINITY);
                let delta = rung_delta(side, price, v.tick, r);
                let rung = vec![PriceLevel { delta_percent: delta, quantity: qty.to_string() }];
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

    fn status(&self) -> String {
        let target = self.cfg.target_share.unwrap_or(0.5);
        match self.last_gap {
            Some(gap) => format!(
                "base share {:.2}% target {:.2}% {}",
                (target + gap) * 100.0,
                target * 100.0,
                if self.active { "trading" } else { "in band" }
            ),
            None => "waiting".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::maker::tests::{lp_book, view};
    use super::*;

    fn cfg(target: f64, mode: StrategyMode) -> RebalancerConfig {
        RebalancerConfig { target_share: Some(target), mode, ..Default::default() }
    }

    fn want(d: Decision) -> Want {
        match d {
            Decision::Take(Some(w)) => w,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_taker_trades_toward_the_target_with_hysteresis() {
        // 1.25M HTEST at 0.0016 = $2000 next to $2000: a 50% share
        let mut rb = Rebalancer::new(cfg(0.25, StrategyMode::Taker));
        let w = want(rb.decide(&view(None, 1_250_000.0, 1_250_000.0)));
        assert_eq!((w.side, w.tif), (OrderType::Offer, TakeTif::Ioc));
        assert!((w.max_quantity - 12_500.0).abs() < 1e-6, "{}", w.max_quantity);
        assert!((w.max_deviation_percent - 0.15).abs() < 1e-12);
        // Inside the exit band it stops; back above the entry band it starts again
        let near = 0.254 * 4000.0 / 0.0016;
        let mut v = view(None, near, near);
        v.inventory.quote = 4000.0 - near * 0.0016;
        assert_eq!(rb.decide(&v), Decision::Take(None));
        let mut v = view(None, 0.26 * 4000.0 / 0.0016, 0.0);
        v.inventory.quote = 4000.0 * 0.74;
        assert_eq!(rb.decide(&v), Decision::Take(None), "0.26 is inside the 2% entry band");
        // Buys when under the target; the gate and the hourly budget hold it back
        let mut buyer = Rebalancer::new(cfg(0.65, StrategyMode::Taker));
        assert_eq!(want(buyer.decide(&view(None, 1_250_000.0, 0.0))).side, OrderType::Bid);
        let mut v = view(None, 1_250_000.0, 0.0);
        v.can_take = false;
        assert_eq!(buyer.decide(&v), Decision::Take(None));
        let mut v = view(None, 1_250_000.0, 0.0);
        v.usd_last_hour = 590.0;
        assert_eq!(buyer.decide(&v), Decision::Take(None));
    }

    #[test]
    fn no_flip_within_the_flip_cooldown() {
        let mut rb = Rebalancer::new(cfg(0.65, StrategyMode::Taker));
        rb.on_fill(&Fill {
            seq: 1,
            at: Instant::now(),
            order_id: 1,
            market_id: "HTEST-USDC".into(),
            side: OrderType::Offer as i32,
            price: rust_decimal::Decimal::ONE,
            quantity: rust_decimal::Decimal::ONE,
            proposal_id: String::new(),
        });
        assert_eq!(rb.decide(&view(None, 1_250_000.0, 0.0)), Decision::Take(None));
    }

    #[test]
    fn a_maker_rests_one_order_inside_the_lp() {
        let book = lp_book(0.0016);
        let mut rb = Rebalancer::new(cfg(0.65, StrategyMode::Maker));
        assert_eq!(rb.book_need(), BookNeed::Always);
        let Decision::Rest(ladder) = rb.decide(&view(Some(&book), 1_250_000.0, 0.0)) else { panic!() };
        assert!(ladder.offers.is_empty() && ladder.bids.len() == 1);
        // 3 bps under the reference rounds down to 0.0015995, well inside the LP's 15 bps
        assert!((ladder.bids[0].delta_percent + 0.028125).abs() < 1e-9, "{:?}", ladder.bids);
        assert_eq!(ladder.bids[0].quantity, "12600");
        // Waiting for a settlement: hold what rests
        let mut v = view(Some(&book), 1_250_000.0, 0.0);
        v.can_take = false;
        assert_eq!(rb.decide(&v), Decision::Hold);
    }

    #[test]
    fn a_maker_clip_waits_out_the_cooldown_after_a_fill() {
        let book = lp_book(0.0016);
        let mut rb = Rebalancer::new(cfg(0.65, StrategyMode::Maker));
        let v = view(Some(&book), 1_250_000.0, 0.0);
        let ago = |secs: u64| v.now.checked_sub(Duration::from_secs(secs)).unwrap();
        let fill = |at: Instant| Fill {
            seq: 1,
            at,
            order_id: 1,
            market_id: "HTEST-USDC".into(),
            side: OrderType::Bid as i32,
            price: rust_decimal::Decimal::ONE,
            quantity: rust_decimal::Decimal::ONE,
            proposal_id: String::new(),
        };
        rb.on_fill(&fill(ago(10)));
        assert_eq!(rb.decide(&v), Decision::Hold, "150 s cooldown from the fill");
        rb.on_fill(&fill(ago(151)));
        assert!(matches!(rb.decide(&v), Decision::Rest(l) if l.bids.len() == 1));
    }
}
