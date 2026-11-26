//! noise_taker: random arrivals that take, or now and then rest a short GTD order.

use std::time::{Duration, Instant};

use orderbook_proto::orderbook::OrderType;
use tracing::info;

use super::rng::{seed_for, SplitMix64};
use super::{clampf, BookNeed, Decide, Decision, MarketView, TakeTif, Want};
use crate::config::NoiseTakerConfig;

pub(crate) struct NoiseTaker {
    cfg: NoiseTakerConfig,
    rng: SplitMix64,
    seed: u64,
    next_at: Option<Instant>,
    arrivals: u32,
    skipped: u32,
}

impl NoiseTaker {
    pub(crate) fn new(cfg: NoiseTakerConfig, market_id: &str) -> Self {
        let seed = cfg.seed.unwrap_or_else(|| seed_for(market_id));
        info!("Strategy {} [noise_taker]: seed {}", market_id, seed);
        Self { cfg, rng: SplitMix64::new(seed), seed, next_at: None, arrivals: 0, skipped: 0 }
    }

    /// Exponential gaps, clamped to a quarter and four times the mean.
    fn schedule(&mut self, now: Instant) {
        let mean = self.cfg.mean_interval_secs;
        let secs = clampf(self.rng.exponential(mean), mean / 4.0, mean * 4.0);
        self.next_at = Duration::try_from_secs_f64(secs).ok().and_then(|d| now.checked_add(d));
    }
}

impl Decide for NoiseTaker {
    fn book_need(&self) -> BookNeed {
        BookNeed::WhenTaking
    }

    fn max_inflight(&self) -> usize {
        usize::try_from(self.cfg.max_inflight).unwrap_or(1)
    }

    fn decide(&mut self, v: &MarketView<'_>) -> Decision {
        let Some(next) = self.next_at else {
            self.schedule(v.now);
            return Decision::Take(None);
        };
        if v.now < next {
            return Decision::Take(None);
        }
        self.schedule(v.now);
        self.arrivals = self.arrivals.saturating_add(1);
        // Every arrival draws the same numbers, so a skip keeps the sequence
        let (u_side, u_size, u_rest) = (self.rng.next_f64(), self.rng.next_f64(), self.rng.next_f64());
        let c = &self.cfg;
        let r = v.reference;
        let ratio = (v.inventory.base - v.start_base) * r / v.in_quote(c.max_position_usd);
        let at_cap = v.activity.open_settlements >= self.max_inflight();
        if !v.can_take || at_cap || !v.inventory.usable || r.is_nan() || r <= 0.0 {
            self.skipped = self.skipped.saturating_add(1);
            return Decision::Take(None);
        }
        let p_buy = clampf(0.5 - c.inventory_bias * ratio, 0.1, 0.9);
        // At the position limit only the reducing side trades
        let side = if ratio >= 1.0 {
            OrderType::Offer
        } else if ratio <= -1.0 || u_side < p_buy {
            OrderType::Bid
        } else {
            OrderType::Offer
        };
        let clip = v.in_quote(c.clip_usd_min + u_size * (c.clip_usd_max - c.clip_usd_min));
        let tif = if u_rest < c.rest_probability { TakeTif::Gtd { ttl_secs: c.rest_ttl_secs } } else { TakeTif::Ioc };
        Decision::Take(Some(Want { side, max_quantity: clip / r, max_deviation_percent: c.max_slip_bps / 100.0, tif }))
    }

    fn status(&self) -> String {
        format!("seed {} arrivals {} skipped {}", self.seed, self.arrivals, self.skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::super::maker::tests::{cc_flat_base, cc_view, ceth_flat_base, ceth_view, view, CC_USD, CETH_REF, CETH_USD};
    use super::*;

    fn run(seed: u64, steps: usize) -> Vec<Decision> {
        let cfg = NoiseTakerConfig { seed: Some(seed), ..Default::default() };
        let mut nt = NoiseTaker::new(cfg, "HTEST-USDC");
        let mut v = view(None, 1e6, 1e6);
        let t0 = v.now;
        (0..steps)
            .map(|i| {
                v.now = t0 + Duration::from_secs(5 * u64::try_from(i).unwrap());
                nt.decide(&v)
            })
            .collect()
    }

    #[test]
    fn a_seed_replays_the_same_arrivals() {
        let a = run(8, 400);
        assert_eq!(a, run(8, 400));
        assert_ne!(a, run(9, 400));
        let takes: Vec<Want> = a.iter().filter_map(|d| if let Decision::Take(Some(w)) = d { Some(*w) } else { None }).collect();
        // About one arrival a minute over 2000 s, each within the clip range
        assert!((15..=60).contains(&takes.len()), "{}", takes.len());
        for w in &takes {
            let usd = w.max_quantity * 0.0016;
            assert!((15.0..=40.0).contains(&usd), "{usd}");
            assert!((w.max_deviation_percent - 0.2).abs() < 1e-12);
        }
        let rests = takes.iter().filter(|w| matches!(w.tif, TakeTif::Gtd { ttl_secs: 45 })).count();
        assert!(rests > 0 && rests < takes.len(), "{rests} of {}", takes.len());
        assert!(takes.iter().any(|w| w.side == OrderType::Bid) && takes.iter().any(|w| w.side == OrderType::Offer));
    }

    #[test]
    fn cc_quoted_clips_are_usd() {
        let cfg = NoiseTakerConfig { seed: Some(8), ..Default::default() };
        let mut nt = NoiseTaker::new(cfg, "TUSDT-CC");
        let base = cc_flat_base();
        let mut v = cc_view(None, base, base);
        let t0 = v.now;
        let mut takes = 0;
        for i in 0..400u64 {
            v.now = t0 + Duration::from_secs(5 * i);
            if let Decision::Take(Some(w)) = nt.decide(&v) {
                takes += 1;
                let usd = w.max_quantity * 13.4 * CC_USD;
                assert!((15.0 - 1e-9..=40.0 + 1e-9).contains(&usd), "{usd}");
            }
        }
        assert!(takes > 10, "{takes}");
    }

    #[test]
    fn ceth_quoted_clips_are_usd() {
        let cfg = NoiseTakerConfig { seed: Some(8), ..Default::default() };
        let mut nt = NoiseTaker::new(cfg, "EDELx-cETH");
        let base = ceth_flat_base();
        let mut v = ceth_view(None, base, base);
        let t0 = v.now;
        let mut takes = 0;
        for i in 0..400u64 {
            v.now = t0 + Duration::from_secs(5 * i);
            if let Decision::Take(Some(w)) = nt.decide(&v) {
                takes += 1;
                let usd = w.max_quantity * CETH_REF * CETH_USD;
                assert!((15.0 - 1e-9..=40.0 + 1e-9).contains(&usd), "{usd}");
            }
        }
        assert!(takes > 10, "{takes}");
    }

    #[test]
    fn arrivals_lean_toward_flat_and_only_reduce_at_the_limits() {
        let cfg = NoiseTakerConfig { seed: Some(1), mean_interval_secs: 5.0, ..Default::default() };
        let mut nt = NoiseTaker::new(cfg, "HTEST-USDC");
        // Long $200 of $250: buys are rare
        let mut v = view(None, 1e6 + 125_000.0, 1e6);
        let t0 = v.now;
        let mut sides = (0, 0);
        for i in 0..2000u64 {
            v.now = t0 + Duration::from_secs(30 * i);
            if let Decision::Take(Some(w)) = nt.decide(&v) {
                if w.side == OrderType::Bid { sides.0 += 1 } else { sides.1 += 1 }
            }
        }
        assert!(sides.0 * 3 < sides.1, "{sides:?}");
        // Past the position limit every arrival reduces; with the gate shut each is skipped
        let mut long = view(None, 1e6 + 200_000.0, 1e6);
        let mut short = view(None, 1e6 - 200_000.0, 1e6);
        let mut w = view(None, 1e6, 1e6);
        w.can_take = false;
        let (mut sells, mut buys, skipped) = (0, 0, nt.skipped);
        // Each call lands past the last one's next arrival (at most 20 s on)
        let start = t0 + Duration::from_secs(120_000);
        for i in 0..200u64 {
            long.now = start + Duration::from_secs(90 * i);
            short.now = long.now + Duration::from_secs(30);
            w.now = long.now + Duration::from_secs(60);
            if let Decision::Take(Some(want)) = nt.decide(&long) {
                assert_eq!(want.side, OrderType::Offer);
                sells += 1;
            }
            if let Decision::Take(Some(want)) = nt.decide(&short) {
                assert_eq!(want.side, OrderType::Bid);
                buys += 1;
            }
            assert_eq!(nt.decide(&w), Decision::Take(None));
        }
        assert_eq!((sells, buys, nt.skipped - skipped), (200, 200, 200));
    }
}
