//! twap: buys or sells a total over a duration in jittered slices; each slice
//! rests a GTD order first, then takes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use orderbook_proto::orderbook::{OrderType, Settlement};
use rust_decimal::prelude::ToPrimitive;
use tracing::{info, warn};

use super::config::parse_quantity;
use super::rng::{seed_for, SplitMix64};
use super::sizing::MIN_ORDER_USD;
use super::{clampf, BookNeed, Decide, Decision, MarketView, TakeTif, Want};
use crate::config::{TradeSide, TwapConfig};
use crate::order_tracker::Fill;

/// Past the scheduled end, slices go on this long to catch up; then the twap stops.
pub(crate) const OVERTIME_SECS: f64 = 600.0;
/// The shortest life a slice's resting order is given.
const MIN_REST_SECS: u64 = 5;

struct Slice {
    /// Progress this slice works toward, in base units
    goal: f64,
    /// Set on the first cycle the slice may act, so it always rests `passive_secs` first
    passive_until: Option<Instant>,
    rested: bool,
}

pub(crate) struct Twap {
    cfg: TwapConfig,
    market_id: String,
    side: OrderType,
    total: f64,
    seed: u64,
    rng: SplitMix64,
    /// Schedule start; without `start_at` it is set on the first cycle
    start: Option<DateTime<Utc>>,
    /// With `start_at`, earlier settlements are counted before any trade
    needs_history: bool,
    filled: f64,
    /// Settlements already counted, from the history or from fills
    counted: HashSet<String>,
    slice: Option<Slice>,
    next_slice_at: Option<Instant>,
    slices: u32,
    done: f64,
    finished: bool,
}

impl Twap {
    pub(crate) fn new(cfg: TwapConfig, market_id: &str) -> Self {
        let seed = cfg.seed.unwrap_or_else(|| seed_for(market_id));
        let side = if cfg.side == Some(TradeSide::Sell) { OrderType::Offer } else { OrderType::Bid };
        let total = cfg.total_quantity.as_deref().and_then(parse_quantity).and_then(|d| d.to_f64()).unwrap_or(0.0);
        let start = cfg
            .start_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        info!(
            "Strategy {} [twap]: {} {} over {} s, seed {}, start {}",
            market_id,
            side_name(side),
            total,
            cfg.duration_secs,
            seed,
            start.map_or_else(|| format!("in {} s", cfg.start_delay_secs), |s| s.to_rfc3339())
        );
        Self {
            needs_history: start.is_some(),
            cfg,
            market_id: market_id.to_string(),
            side,
            total,
            seed,
            rng: SplitMix64::new(seed),
            start,
            filled: 0.0,
            counted: HashSet::new(),
            slice: None,
            next_slice_at: None,
            slices: 0,
            done: 0.0,
            finished: false,
        }
    }

    /// The next slice: what the schedule wants done by its end, at most `max_catchup`
    /// nominal slices, jittered, and never past the total.
    fn start_slice(&mut self, elapsed: f64, done: f64, v: &MarketView<'_>) {
        let c = &self.cfg;
        // Every slice draws the same two numbers, so a skipped one keeps the sequence
        let (u_time, u_size) = (self.rng.next_f64(), self.rng.next_f64());
        let slice_secs = c.slice_secs as f64;
        let duration = (c.duration_secs as f64).max(1.0);
        let gap = slice_secs * (1.0 + c.time_jitter * (2.0 * u_time - 1.0));
        self.next_slice_at = Duration::try_from_secs_f64(gap).ok().and_then(|d| v.now.checked_add(d));
        self.slices = self.slices.saturating_add(1);
        let r = v.reference;
        let remaining = self.total - done;
        let behind = self.total * clampf((elapsed + gap) / duration, 0.0, 1.0) - done;
        let nominal = self.total * slice_secs / duration;
        let mut qty = (behind.min(c.max_catchup * nominal) * (1.0 + c.size_jitter * (2.0 * u_size - 1.0)))
            .max(v.in_quote(MIN_ORDER_USD) / r)
            .min(remaining);
        // Never leave less than one order's worth behind
        if (remaining - qty) * r < v.in_quote(MIN_ORDER_USD) {
            qty = remaining;
        }
        self.slice = (behind > 0.0 && qty * r >= v.floor_value).then_some(Slice {
            goal: done + qty,
            passive_until: None,
            rested: c.passive_secs == 0,
        });
    }

    /// Moves the current slice's clocks `by` into the past.
    #[cfg(test)]
    pub(crate) fn age(&mut self, by: Duration) {
        if let Some(slice) = self.slice.as_mut() {
            slice.passive_until = slice.passive_until.map(|t| t.checked_sub(by).unwrap_or(t));
        }
        self.next_slice_at = self.next_slice_at.and_then(|t| t.checked_sub(by));
    }

    fn finish(&mut self, remaining: f64) {
        self.finished = true;
        self.slice = None;
        let side = side_name(self.side);
        if remaining > self.total * 1e-9 {
            warn!(
                "Strategy {} [twap]: finished with a shortfall of {:.2}: {} {:.2} of {:.2}",
                self.market_id, remaining, side, self.done, self.total
            );
        } else {
            info!("Strategy {} [twap]: finished: {} {:.2} of {:.2}", self.market_id, side, self.done, self.total);
        }
    }
}

fn side_name(side: OrderType) -> &'static str {
    if side == OrderType::Bid { "buy" } else { "sell" }
}

impl Decide for Twap {
    fn on_fill(&mut self, fill: &Fill) {
        if fill.side != self.side as i32 {
            return;
        }
        if fill.proposal_id.is_empty() || self.counted.insert(fill.proposal_id.clone()) {
            self.filled += fill.quantity.to_f64().unwrap_or(0.0);
        }
    }

    fn book_need(&self) -> BookNeed {
        BookNeed::WhenTaking
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn history_from(&self) -> Option<DateTime<Utc>> {
        self.start.filter(|_| self.needs_history)
    }

    fn on_history(&mut self, settlements: &[Settlement], party: &str) {
        let since = self.start.map_or(i64::MIN, |s| s.timestamp());
        let mut found = 0u32;
        for s in settlements {
            let ours = if self.side == OrderType::Bid { s.buyer == party } else { s.seller == party };
            let other_market = !s.market_id.is_empty() && s.market_id != self.market_id;
            let before = s.settled_at.as_ref().is_some_and(|t| t.seconds < since);
            if !ours || other_market || before || s.proposal_id.is_empty() || !self.counted.insert(s.proposal_id.clone()) {
                continue;
            }
            self.filled += parse_quantity(&s.base_quantity).and_then(|d| d.to_f64()).unwrap_or(0.0);
            found = found.saturating_add(1);
        }
        self.needs_history = false;
        info!(
            "Strategy {} [twap]: resuming with {} earlier settlement(s): {} {:.2} of {:.2}",
            self.market_id,
            found,
            side_name(self.side),
            self.filled,
            self.total
        );
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        if self.finished || self.needs_history || self.total <= 0.0 {
            return Decision::Take(None);
        }
        let start = match self.start {
            Some(s) => s,
            None => {
                let delay = i64::try_from(self.cfg.start_delay_secs).ok().and_then(TimeDelta::try_seconds);
                let Some(s) = delay.and_then(|d| v.wall.checked_add_signed(d)) else { return Decision::Take(None) };
                self.start = Some(s);
                s
            }
        };
        let elapsed = v.wall.signed_duration_since(start).num_milliseconds() as f64 / 1000.0;
        let r = v.reference;
        if elapsed < 0.0 || !v.inventory.usable || r.is_nan() || r <= 0.0 {
            return Decision::Take(None);
        }
        let pending = if self.side == OrderType::Bid { v.activity.pending_bid_base } else { v.activity.pending_offer_base };
        let done = self.filled + pending;
        self.done = done;
        let remaining = self.total - done;
        // Done, out of time, or left with less than any order can carry: stop once settled
        let hard_end = elapsed > self.cfg.duration_secs as f64 + OVERTIME_SECS;
        if hard_end || remaining * r < v.floor_value {
            if v.activity.open_settlements == 0 {
                self.finish(remaining);
            }
            return Decision::Take(None);
        }
        if self.next_slice_at.is_none_or(|t| v.now >= t) {
            self.start_slice(elapsed, done, v);
        }
        let Some(goal) = self.slice.as_ref().map(|s| s.goal) else { return Decision::Take(None) };
        let left = (goal - done).min(remaining);
        if left * r < v.floor_value {
            self.slice = None;
            return Decision::Take(None);
        }
        let cap = self.max_inflight();
        if !v.can_take || v.activity.open_settlements >= cap {
            return Decision::Take(None);
        }
        let (side, max_deviation_percent) = (self.side, self.cfg.max_slip_bps / 100.0);
        let passive = Duration::from_secs(self.cfg.passive_secs);
        let Some(slice) = self.slice.as_mut() else { return Decision::Take(None) };
        let passive_until = *slice.passive_until.get_or_insert_with(|| v.now.checked_add(passive).unwrap_or(v.now));
        // First a resting order for the passive part of the slice, then takes
        let tif = if v.now < passive_until {
            if slice.rested {
                return Decision::Take(None);
            }
            slice.rested = true;
            let ttl_secs = passive_until.saturating_duration_since(v.now).as_secs().max(MIN_REST_SECS);
            TakeTif::Gtd { ttl_secs }
        } else {
            TakeTif::Ioc
        };
        Decision::Take(Some(Want { side, max_quantity: left, max_deviation_percent, tif }))
    }

    fn status(&self) -> String {
        let pct = if self.total > 0.0 { self.done / self.total * 100.0 } else { 0.0 };
        let state = if self.finished {
            "finished"
        } else if self.needs_history {
            "loading its settlement history"
        } else if self.start.is_none() {
            "not started"
        } else {
            "running"
        };
        format!(
            "{} {:.2} of {:.2} ({:.1}%) slices {} seed {} {}",
            side_name(self.side),
            self.done,
            self.total,
            pct,
            self.slices,
            self.seed,
            state
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::maker::tests::view;
    use super::super::sizing::lots;
    use super::*;
    use rust_decimal::Decimal;

    fn cfg(side: TradeSide, start_at: Option<String>) -> TwapConfig {
        TwapConfig {
            side: Some(side),
            total_quantity: Some("250000".into()),
            start_at,
            start_delay_secs: 0,
            seed: Some(3),
            ..Default::default()
        }
    }

    fn fill(side: OrderType, qty: f64, id: &str) -> Fill {
        Fill {
            seq: 1,
            at: Instant::now(),
            order_id: 1,
            market_id: "HTEST-USDC".into(),
            side: side as i32,
            price: Decimal::ONE,
            quantity: Decimal::from_f64_retain(qty).unwrap(),
            proposal_id: id.into(),
        }
    }

    fn settlement(id: &str, buyer: &str, seller: &str, qty: &str) -> Settlement {
        Settlement {
            proposal_id: id.into(),
            market_id: "HTEST-USDC".into(),
            buyer: buyer.into(),
            seller: seller.into(),
            base_quantity: qty.into(),
            ..Default::default()
        }
    }

    // Each slice rests first and takes after 60 s; filling every take, the run ends
    // exactly at the total and never asks for more than is left
    #[test]
    fn a_full_run_reaches_the_total_and_never_passes_it() {
        let mut t = Twap::new(cfg(TradeSide::Buy, None), "HTEST-USDC");
        let mut v = view(None, 1e6, 1e6);
        let (t0, w0) = (v.now, v.wall);
        let (mut gtd, mut ioc, mut n) = (0, 0, 0);
        let mut last_gtd: Option<Instant> = None;
        for step in 0..1300u64 {
            v.now = t0 + Duration::from_secs(5 * step);
            v.wall = w0 + TimeDelta::seconds(i64::try_from(5 * step).unwrap());
            let Decision::Take(Some(w)) = t.decide(&v) else { continue };
            assert!(w.max_quantity <= 250_000.0 - t.filled + 1e-6, "{} after {}", w.max_quantity, t.filled);
            assert_eq!(w.side, OrderType::Bid);
            match w.tif {
                // Resting orders never fill in this run
                TakeTif::Gtd { ttl_secs } => {
                    assert!((5..=60).contains(&ttl_secs));
                    gtd += 1;
                    last_gtd = Some(v.now);
                }
                TakeTif::Ioc => {
                    assert!(last_gtd.is_some_and(|g| v.now.duration_since(g) >= Duration::from_secs(60)));
                    ioc += 1;
                    n += 1;
                    let q = lots(w.max_quantity, 2, false).unwrap().to_f64().unwrap();
                    t.on_fill(&fill(OrderType::Bid, q, &format!("p{n}")));
                }
            }
        }
        assert!(t.finished, "{}", t.status());
        assert_eq!(t.filled, 250_000.0);
        assert!(gtd >= 15 && ioc >= gtd, "{gtd} {ioc}");
        assert!(t.status().starts_with("buy 250000.00 of 250000.00 (100.0%)"), "{}", t.status());
    }

    // With start_at, nothing trades until the settlement history is in; each settlement
    // counts once, from the history or from a fill
    #[test]
    fn a_restart_resumes_from_the_settlement_history() {
        let v0 = view(None, 1e6, 1e6);
        let start = v0.wall - TimeDelta::seconds(2400);
        let mut t = Twap::new(cfg(TradeSide::Buy, Some(start.to_rfc3339())), "HTEST-USDC");
        assert_eq!(t.history_from().map(|s| s.timestamp()), Some(start.timestamp()));
        assert_eq!(t.decide(&v0), Decision::Take(None));
        assert!(t.status().contains("loading its settlement history"));
        // A fill seen before the history arrives is the same settlement as p1
        t.on_fill(&fill(OrderType::Bid, 50_000.0, "p1"));
        t.on_fill(&fill(OrderType::Offer, 9_000.0, "p9"));
        let old = settlement("p0", "us", "them", "70000");
        let mut old_one = old.clone();
        old_one.settled_at = Some(prost_types::Timestamp { seconds: start.timestamp() - 60, nanos: 0 });
        t.on_history(
            &[
                settlement("p1", "us", "them", "50000"),
                settlement("p2", "us", "them", "30000"),
                settlement("p2", "us", "them", "30000"),
                settlement("p3", "them", "us", "40000"),
                settlement("p4", "them", "else", "40000"),
                old_one,
            ],
            "us",
        );
        assert_eq!((t.filled, t.history_from()), (80_000.0, None));
        // Half way through, 125000 is due: the slice catches up by at most two nominal slices
        let Decision::Take(Some(w)) = t.decide(&v0) else { panic!("{}", t.status()) };
        assert!(matches!(w.tif, TakeTif::Gtd { .. }));
        assert!((21_250.0..=28_750.0).contains(&w.max_quantity), "{}", w.max_quantity);
    }

    // A slice begun while the gate was shut used to skip its rest once the gate opened
    #[test]
    fn a_slice_rests_its_passive_time_from_when_it_may_act() {
        let mut t = Twap::new(cfg(TradeSide::Buy, None), "HTEST-USDC");
        let mut v = view(None, 1e6, 1e6);
        let (t0, w0) = (v.now, v.wall);
        let at = |v: &mut MarketView<'_>, secs: u64| {
            v.now = t0 + Duration::from_secs(secs);
            v.wall = w0 + TimeDelta::seconds(i64::try_from(secs).unwrap());
        };
        v.can_take = false;
        assert_eq!(t.decide(&v), Decision::Take(None));
        assert_eq!(t.slices, 1);
        // The gate opens 90 s into the slice: the full 60 s of rest come first
        at(&mut v, 90);
        v.can_take = true;
        let tif = |d: Decision| match d {
            Decision::Take(Some(w)) => w.tif,
            other => panic!("{other:?}"),
        };
        assert_eq!(tif(t.decide(&v)), TakeTif::Gtd { ttl_secs: 60 });
        at(&mut v, 149);
        assert_eq!(t.decide(&v), Decision::Take(None));
        at(&mut v, 150);
        assert_eq!(tif(t.decide(&v)), TakeTif::Ioc);
        assert_eq!(t.slices, 1);
    }

    // Without start_at it starts after its delay; with nothing filled it stops at the
    // hard end and reports the shortfall
    #[test]
    fn the_start_delay_and_a_shortfall_at_the_hard_end() {
        let mut c = cfg(TradeSide::Sell, None);
        c.start_delay_secs = 300;
        let mut t = Twap::new(c, "HTEST-USDC");
        let mut v = view(None, 1e6, 1e6);
        let (t0, w0) = (v.now, v.wall);
        assert_eq!(t.decide(&v), Decision::Take(None));
        v.now = t0 + Duration::from_secs(300);
        v.wall = w0 + TimeDelta::seconds(300);
        assert!(matches!(t.decide(&v), Decision::Take(Some(Want { side: OrderType::Offer, tif: TakeTif::Gtd { .. }, .. }))));
        // A pending settlement holds the end; once settled the twap stops
        v.now = t0 + Duration::from_secs(300 + 4800 + 601);
        v.wall = w0 + TimeDelta::seconds(300 + 4800 + 601);
        v.activity.open_settlements = 1;
        assert_eq!(t.decide(&v), Decision::Take(None));
        assert!(!t.finished);
        v.activity.open_settlements = 0;
        assert_eq!(t.decide(&v), Decision::Take(None));
        assert!(t.finished && t.status().ends_with("finished"), "{}", t.status());
        assert_eq!(t.decide(&v), Decision::Take(None));
    }
}
