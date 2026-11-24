//! momentum: an EMA crossover that holds a long, flat or short target position.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;

use super::ema::Ema;
use super::sizing::MIN_ORDER_USD;
use super::{bps, BookNeed, Decide, Decision, MarketView, TakeTif, Want};
use crate::config::MomentumConfig;

const HOUR: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trend {
    Flat,
    Long,
    Short,
}

impl Trend {
    fn sign(self) -> f64 {
        match self {
            Self::Flat => 0.0,
            Self::Long => 1.0,
            Self::Short => -1.0,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Long => "long",
            Self::Short => "short",
        }
    }
}

pub(crate) struct Momentum {
    cfg: MomentumConfig,
    fast: Ema,
    slow: Ema,
    trend: Trend,
    entered_at: Option<Instant>,
    /// Entries into a position over the last hour
    entries: VecDeque<Instant>,
    signal: Option<f64>,
}

impl Momentum {
    pub(crate) fn new(cfg: MomentumConfig) -> Self {
        let (fast, slow) = (Ema::new(cfg.fast_secs as f64), Ema::new(cfg.slow_secs as f64));
        Self { cfg, fast, slow, trend: Trend::Flat, entered_at: None, entries: VecDeque::new(), signal: None }
    }

    /// Fast over slow EMA, in bps.
    fn signal_bps(&self) -> Option<f64> {
        let (fast, slow) = (self.fast.value()?, self.slow.value()?);
        (slow > 0.0).then(|| bps(fast, slow))
    }

    /// Flat enters on `enter_bps`; a position returns to flat under `exit_bps` after the
    /// minimum hold, never straight to the other side; entries are capped per hour.
    fn step(&mut self, s: f64, now: Instant) {
        let c = &self.cfg;
        while self.entries.front().is_some_and(|t| now.saturating_duration_since(*t) >= HOUR) {
            self.entries.pop_front();
        }
        let held = self
            .entered_at
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(c.min_hold_secs));
        let next = match self.trend {
            Trend::Flat if s > c.enter_bps => Trend::Long,
            Trend::Flat if s < -c.enter_bps => Trend::Short,
            Trend::Long if s < c.exit_bps && held => Trend::Flat,
            Trend::Short if s > -c.exit_bps && held => Trend::Flat,
            same => same,
        };
        if next == self.trend {
            return;
        }
        if next != Trend::Flat {
            if self.entries.len() >= usize::try_from(c.max_flips_per_hour).unwrap_or(usize::MAX) {
                return;
            }
            self.entries.push_back(now);
        }
        self.trend = next;
        self.entered_at = (next != Trend::Flat).then_some(now);
    }
}

impl Decide for Momentum {
    fn observe(&mut self, price: f64, now: Instant) {
        self.fast.update(price, now);
        self.slow.update(price, now);
    }

    fn book_need(&self) -> BookNeed {
        BookNeed::WhenTaking
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cfg.clip_interval_secs)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        // No trades, and no state changes, before both averages have warmed up
        if !self.slow.warm(v.now, Duration::from_secs(self.cfg.warmup_secs)) {
            return Decision::Take(None);
        }
        let Some(s) = self.signal_bps() else { return Decision::Take(None) };
        self.signal = Some(s);
        self.step(s, v.now);
        let c = &self.cfg;
        let r = v.reference;
        if !v.inventory.usable || r.is_nan() || r <= 0.0 {
            return Decision::Take(None);
        }
        let target = v.start_base + self.trend.sign() * c.max_position_usd / r;
        let gap = target - v.inventory.base;
        if gap.abs() * r < MIN_ORDER_USD || !v.can_take || v.activity.open_settlements >= self.max_inflight() {
            return Decision::Take(None);
        }
        Decision::Take(Some(Want {
            side: if gap > 0.0 { OrderType::Bid } else { OrderType::Offer },
            max_quantity: gap.abs().min(c.clip_usd / r),
            max_deviation_percent: c.max_slip_bps / 100.0,
            tif: TakeTif::Ioc,
        }))
    }

    fn status(&self) -> String {
        match self.signal {
            Some(s) => format!("{} signal {:+.1} bps entries {} in the last hour", self.trend.name(), s, self.entries.len()),
            None => "warming up".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::maker::tests::view;
    use super::*;

    /// A warm strategy whose fast EMA sits `s` bps over the slow one.
    fn primed(m: &mut Momentum, s: f64, now: Instant) {
        let first = now.checked_sub(Duration::from_secs(700)).unwrap();
        m.fast = Ema::primed(180.0, 1.0 + s / 10_000.0, first, now);
        m.slow = Ema::primed(900.0, 1.0, first, now);
    }

    fn side(d: Decision) -> Option<OrderType> {
        match d {
            Decision::Take(Some(w)) => Some(w.side),
            Decision::Take(None) => None,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_trades_during_the_warm_up() {
        let mut m = Momentum::new(MomentumConfig::default());
        let v = view(None, 1e6, 1e6);
        let t0 = v.now;
        m.observe(1.0, t0);
        m.observe(1.01, t0 + Duration::from_secs(300));
        let mut later = view(None, 1e6, 1e6);
        later.now = t0 + Duration::from_secs(599);
        assert_eq!(m.decide(&later), Decision::Take(None));
        assert_eq!(m.status(), "warming up");
        // Warm, the rise reads as a trend and the strategy buys
        later.now = t0 + Duration::from_secs(600);
        m.observe(1.01, later.now);
        assert_eq!(side(m.decide(&later)), Some(OrderType::Bid));
        assert_eq!(m.trend, Trend::Long);
    }

    #[test]
    fn hysteresis_and_the_minimum_hold() {
        let mut m = Momentum::new(MomentumConfig::default());
        let mut v = view(None, 1e6, 1e6);
        let t0 = v.now;
        let at = |s: u64| t0 + Duration::from_secs(s);
        // 8 bps is between exit and enter: still flat
        primed(&mut m, 8.0, t0);
        assert_eq!(m.decide(&v), Decision::Take(None));
        primed(&mut m, 13.0, t0);
        let Decision::Take(Some(w)) = m.decide(&v) else { panic!() };
        // One $30 clip toward a $200 long
        assert_eq!((w.side, w.tif), (OrderType::Bid, TakeTif::Ioc));
        assert!((w.max_quantity - 30.0 / 0.0016).abs() < 1e-6 && (w.max_deviation_percent - 0.15).abs() < 1e-12);
        // Under exit before the 600 s hold: still long
        v.now = at(300);
        primed(&mut m, 2.0, v.now);
        assert_eq!(side(m.decide(&v)), Some(OrderType::Bid));
        // At the long target there is nothing to do
        v.inventory.base = 1e6 + 200.0 / 0.0016;
        assert_eq!(m.decide(&v), Decision::Take(None));
        // After the hold it goes flat and sells back to the start
        v.now = at(600);
        primed(&mut m, 2.0, v.now);
        assert_eq!(side(m.decide(&v)), Some(OrderType::Offer));
        assert_eq!(m.trend, Trend::Flat);
    }

    #[test]
    fn long_turns_short_only_through_flat_and_entries_are_capped() {
        let mut m = Momentum::new(MomentumConfig::default());
        let mut v = view(None, 1e6, 1e6);
        let t0 = v.now;
        primed(&mut m, 20.0, t0);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Long);
        // A sharp reversal inside the hold changes nothing
        v.now = t0 + Duration::from_secs(100);
        primed(&mut m, -20.0, v.now);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Long);
        // After the hold: flat first, short on the next cycle
        v.now = t0 + Duration::from_secs(600);
        primed(&mut m, -20.0, v.now);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Flat);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Short);
        assert_eq!(side(m.decide(&v)), Some(OrderType::Offer));
        // A third entry within the hour is the limit; the fourth waits for the hour to pass
        v.now = t0 + Duration::from_secs(1200);
        primed(&mut m, 0.0, v.now);
        m.decide(&v);
        primed(&mut m, 20.0, v.now);
        m.decide(&v);
        assert_eq!((m.trend, m.entries.len()), (Trend::Long, 3));
        v.now = t0 + Duration::from_secs(1800);
        primed(&mut m, 0.0, v.now);
        m.decide(&v);
        primed(&mut m, -20.0, v.now);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Flat, "flip cap");
        v.now = t0 + Duration::from_secs(3601);
        m.decide(&v);
        assert_eq!(m.trend, Trend::Short);
        // The gate holds clips back
        v.can_take = false;
        assert_eq!(m.decide(&v), Decision::Take(None));
    }
}
