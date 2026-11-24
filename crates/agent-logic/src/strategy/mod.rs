//! Per-market trading strategies chosen by `[markets.strategy]`; each decides once
//! per grid cycle, returning a ladder to rest (makers) or a take (takers).

pub mod config;

pub(crate) mod arbitrage;
pub(crate) mod book;
pub(crate) mod ema;
pub(crate) mod gate;
pub(crate) mod maker;
pub(crate) mod mean_reversion;
pub(crate) mod momentum;
pub(crate) mod noise;
pub(crate) mod rebalancer;
pub(crate) mod report;
pub(crate) mod rng;
pub(crate) mod sizing;
pub(crate) mod twap;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orderbook_proto::orderbook::{OrderType, Settlement};
use rust_decimal::prelude::ToPrimitive;

use crate::config::{CommonParams, PriceLevel, StrategyConfig};
use crate::order_tracker::{Fill, MarketActivity};

use self::book::BookTop;
use self::gate::TakeGate;

/// `x` within [lo, hi]; unlike `f64::clamp` it never panics.
pub(crate) fn clampf(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

/// Basis points of `a` over `b`.
pub(crate) fn bps(a: f64, b: f64) -> f64 {
    (a / b - 1.0) * 10_000.0
}

/// Balances in one market, with matched but unsettled amounts applied.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Inventory {
    pub base: f64,
    pub quote: f64,
    /// Unlocked, after the CC fee reserve
    pub base_avail: f64,
    pub quote_avail: f64,
    /// Balances fresh enough to trade on
    pub usable: bool,
}

/// What a strategy sees in one cycle.
pub(crate) struct MarketView<'a> {
    pub now: Instant,
    /// The same moment on the wall clock
    pub wall: DateTime<Utc>,
    pub reference: f64,
    pub tick: f64,
    /// Least order value the server takes, in quote units
    pub floor_value: f64,
    /// Quantities are whole lots of 10^lot_exp base units
    pub lot_exp: i32,
    pub inventory: Inventory,
    /// Base held when the strategy started
    pub start_base: f64,
    pub book: Option<&'a BookTop>,
    pub activity: MarketActivity,
    /// Own fills in this market over the last hour, and their quote value
    pub fills_last_hour: usize,
    pub usd_last_hour: f64,
    /// The take gate is open
    pub can_take: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BookNeed {
    Never,
    WhenTaking,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TakeTif {
    Ioc,
    Gtd { ttl_secs: u64 },
}

/// A taker's intent; the order manager prices and sizes it against the book.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Want {
    pub side: OrderType,
    /// Base units
    pub max_quantity: f64,
    pub max_deviation_percent: f64,
    pub tif: TakeTif,
}

/// A maker's target: rungs as percent from the reference, with base quantities.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Ladder {
    pub bids: Vec<PriceLevel>,
    pub offers: Vec<PriceLevel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// Keep exactly this ladder resting
    Rest(Ladder),
    /// A take now, or none; other resting orders are withdrawn
    Take(Option<Want>),
    /// Change nothing this cycle
    Hold,
}

pub(crate) trait Decide {
    /// Once per fresh reference reading.
    fn observe(&mut self, _price: f64, _now: Instant) {}
    /// Each own fill in this market, once.
    fn on_fill(&mut self, _fill: &Fill) {}
    fn book_need(&self) -> BookNeed {
        BookNeed::Never
    }
    /// Makers rest a ladder; takers keep nothing resting but their take.
    fn is_maker(&self) -> bool {
        self.book_need() == BookNeed::Always
    }
    /// Own settlements since this time are needed before the first trade.
    fn history_from(&self) -> Option<DateTime<Utc>> {
        None
    }
    /// Those settlements; `party` tells which side was ours.
    fn on_history(&mut self, _settlements: &[Settlement], _party: &str) {}
    fn decide(&mut self, view: &MarketView<'_>) -> Decision;
    /// Open settlements at which new orders wait.
    fn max_inflight(&self) -> usize;
    /// Least time between two takes.
    fn cooldown(&self) -> Duration {
        Duration::ZERO
    }
    /// Strategy state for the report line.
    fn status(&self) -> String {
        String::new()
    }
}

/// One market's strategy; `Pending` stands for the grid, which runs elsewhere.
pub(crate) enum StrategyState {
    MarketMaker(maker::MarketMaker),
    Rebalancer(rebalancer::Rebalancer),
    Momentum(momentum::Momentum),
    MeanReversion(mean_reversion::MeanReversion),
    NoiseTaker(noise::NoiseTaker),
    Twap(twap::Twap),
    Arbitrage(arbitrage::Arbitrage),
    Pending,
}

impl StrategyState {
    pub(crate) fn new(config: &StrategyConfig, market_id: &str) -> Self {
        match config {
            StrategyConfig::Grid(_) => Self::Pending,
            StrategyConfig::MarketMaker(c) => Self::MarketMaker(maker::MarketMaker::new(c.clone())),
            StrategyConfig::Rebalancer(c) => Self::Rebalancer(rebalancer::Rebalancer::new(c.clone())),
            StrategyConfig::Momentum(c) => Self::Momentum(momentum::Momentum::new(c.clone())),
            StrategyConfig::MeanReversion(c) => {
                Self::MeanReversion(mean_reversion::MeanReversion::new(c.clone(), market_id))
            }
            StrategyConfig::NoiseTaker(c) => Self::NoiseTaker(noise::NoiseTaker::new(c.clone(), market_id)),
            StrategyConfig::Twap(c) => Self::Twap(twap::Twap::new(c.clone(), market_id)),
            StrategyConfig::Arbitrage(c) => Self::Arbitrage(arbitrage::Arbitrage::new(c.clone())),
        }
    }
}

macro_rules! dispatch {
    ($self:ident, $s:ident => $e:expr, $pending:expr) => {
        match $self {
            StrategyState::MarketMaker($s) => $e,
            StrategyState::Rebalancer($s) => $e,
            StrategyState::Momentum($s) => $e,
            StrategyState::MeanReversion($s) => $e,
            StrategyState::NoiseTaker($s) => $e,
            StrategyState::Twap($s) => $e,
            StrategyState::Arbitrage($s) => $e,
            StrategyState::Pending => $pending,
        }
    };
}

impl Decide for StrategyState {
    fn observe(&mut self, price: f64, now: Instant) {
        dispatch!(self, s => s.observe(price, now), ())
    }
    fn on_fill(&mut self, fill: &Fill) {
        dispatch!(self, s => s.on_fill(fill), ())
    }
    fn book_need(&self) -> BookNeed {
        dispatch!(self, s => s.book_need(), BookNeed::Never)
    }
    fn is_maker(&self) -> bool {
        dispatch!(self, s => s.is_maker(), false)
    }
    fn history_from(&self) -> Option<DateTime<Utc>> {
        dispatch!(self, s => s.history_from(), None)
    }
    fn on_history(&mut self, settlements: &[Settlement], party: &str) {
        dispatch!(self, s => s.on_history(settlements, party), ())
    }
    fn decide(&mut self, view: &MarketView<'_>) -> Decision {
        dispatch!(self, s => s.decide(view), Decision::Take(None))
    }
    fn max_inflight(&self) -> usize {
        dispatch!(self, s => s.max_inflight(), 1)
    }
    fn cooldown(&self) -> Duration {
        dispatch!(self, s => s.cooldown(), Duration::ZERO)
    }
    fn status(&self) -> String {
        dispatch!(self, s => s.status(), String::new())
    }
}

// ---- reference guard ----

/// A reading older than this is stale, unless `ref_max_age_secs` says otherwise.
pub(crate) const REF_MAX_AGE: Duration = Duration::from_secs(90);
/// Makers keep their quotes through stale readings, and withdraw once they last this long.
pub(crate) const REF_STALE_WITHDRAW: Duration = Duration::from_secs(300);
/// A move larger than this from the last accepted reading pauses it.
pub(crate) const REF_MAX_JUMP_BPS: f64 = 100.0;
/// Consistent readings that accept a new level after a jump.
const REF_CONFIRM: u32 = 3;
/// Readings this close to a configured oracle bound are not trusted.
const REF_BOUND_MARGIN: f64 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RefReject {
    /// `stale_for`: how long the readings have been stale
    Stale { age: Duration, stale_for: Duration },
    Jump(f64),
    NearBound,
}

impl RefReject {
    /// Whether a maker withdraws its quotes, rather than holding them.
    pub(crate) fn withdraws(&self) -> bool {
        match self {
            Self::Stale { stale_for, .. } => *stale_for >= REF_STALE_WITHDRAW,
            Self::Jump(_) | Self::NearBound => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RefGuard {
    accepted: Option<f64>,
    candidate: Option<(f64, u32)>,
    floor: Option<f64>,
    cap: Option<f64>,
    max_age: Duration,
    pub(crate) stale_since: Option<Instant>,
}

impl RefGuard {
    pub(crate) fn new(common: Option<CommonParams>) -> Self {
        Self {
            accepted: None,
            candidate: None,
            floor: common.and_then(|c| c.ref_floor),
            cap: common.and_then(|c| c.ref_cap),
            max_age: common.and_then(|c| c.ref_max_age_secs).map_or(REF_MAX_AGE, Duration::from_secs),
            stale_since: None,
        }
    }

    pub(crate) fn max_age(&self) -> Duration {
        self.max_age
    }

    /// Accepts a fresh, stable reading; a jump stands once it repeats.
    pub(crate) fn check(&mut self, price: f64, age: Duration, now: Instant) -> Result<f64, RefReject> {
        if age > self.max_age {
            let since = *self.stale_since.get_or_insert(now);
            let stale_for = now.saturating_duration_since(since).max(age.saturating_sub(self.max_age));
            return Err(RefReject::Stale { age, stale_for });
        }
        self.stale_since = None;
        let near_floor = self.floor.is_some_and(|f| price <= f * (1.0 + REF_BOUND_MARGIN));
        let near_cap = self.cap.is_some_and(|c| price >= c * (1.0 - REF_BOUND_MARGIN));
        if near_floor || near_cap {
            return Err(RefReject::NearBound);
        }
        let Some(last) = self.accepted else {
            self.accepted = Some(price);
            return Ok(price);
        };
        let jump = bps(price, last).abs();
        if jump <= REF_MAX_JUMP_BPS {
            self.accepted = Some(price);
            self.candidate = None;
            return Ok(price);
        }
        let seen = match self.candidate {
            Some((c, n)) if bps(price, c).abs() <= REF_MAX_JUMP_BPS => n.saturating_add(1),
            _ => 1,
        };
        if seen >= REF_CONFIRM {
            self.accepted = Some(price);
            self.candidate = None;
            return Ok(price);
        }
        self.candidate = Some((price, seen));
        Err(RefReject::Jump(jump))
    }
}

// ---- settlements-per-hour cap ----

/// How long the first hit of the cap pauses the strategy.
pub(crate) const CAP_PAUSE: Duration = Duration::from_secs(900);
const HOUR: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapState {
    Running,
    Paused { new: bool },
    Stopped { new: bool },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SettleCap {
    max_per_hour: usize,
    since: Option<Instant>,
    pub(crate) paused_until: Option<Instant>,
    pub(crate) strikes: u32,
}

impl SettleCap {
    pub(crate) fn new(max_per_hour: u32) -> Self {
        Self { max_per_hour: usize::try_from(max_per_hour).unwrap_or(usize::MAX), ..Self::default() }
    }

    /// Counts the fills of the last hour since the last resume; a second hit stops for good.
    pub(crate) fn check(&mut self, fills: &VecDeque<RecentFill>, now: Instant) -> CapState {
        if self.strikes >= 2 {
            return CapState::Stopped { new: false };
        }
        if let Some(until) = self.paused_until {
            if now < until {
                return CapState::Paused { new: false };
            }
            self.paused_until = None;
            self.since = Some(until);
        }
        let counted = fills
            .iter()
            .filter(|f| now.saturating_duration_since(f.at) < HOUR && self.since.is_none_or(|s| f.at >= s))
            .count();
        if self.max_per_hour == 0 || counted < self.max_per_hour {
            return CapState::Running;
        }
        self.strikes = self.strikes.saturating_add(1);
        if self.strikes >= 2 {
            return CapState::Stopped { new: true };
        }
        self.paused_until = now.checked_add(CAP_PAUSE);
        CapState::Paused { new: true }
    }
}

// ---- per-market runtime ----

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RecentFill {
    pub at: Instant,
    pub side: OrderType,
    pub base: f64,
    pub quote: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Stats {
    pub takes: u32,
    pub takes_filled: u32,
    /// Takes that ran their course without a match
    pub takes_empty: u32,
    pub fills: u32,
    pub bought: f64,
    pub sold: f64,
}

/// Balances and price when the strategy started, for the report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Start {
    pub base: f64,
    pub quote: f64,
    pub price: f64,
}

/// Recent take order ids, and how each ended.
const TAKE_IDS_KEPT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TakeMark {
    Open,
    Filled,
    Empty,
}
/// How often the report line is written.
pub(crate) const REPORT_EVERY: Duration = Duration::from_secs(300);

/// One market's strategy and everything it remembers between cycles.
pub(crate) struct StrategyRuntime {
    pub kind: &'static str,
    pub state: StrategyState,
    pub guard: RefGuard,
    pub gate: TakeGate,
    pub cap: SettleCap,
    pub stats: Stats,
    pub fill_seq: u64,
    pub recent: VecDeque<RecentFill>,
    pub take_ids: VecDeque<(u64, TakeMark)>,
    pub start: Option<Start>,
    pub last_report: Instant,
    pub warned_at: Option<Instant>,
    pub bid_placed_at: Option<Instant>,
    pub offer_placed_at: Option<Instant>,
    pub last_reading: Option<(f64, Option<i64>)>,
    /// Maker cycles in a row without book depth
    pub depth_fails: u32,
    /// The reference age was logged at INFO once
    pub age_logged: bool,
    /// A resting take and when it is cancelled if still listed
    pub rest_until: Option<(u64, Instant)>,
    /// When a failed settlement-history lookup is tried again
    pub history_retry_at: Option<Instant>,
}

impl StrategyRuntime {
    pub(crate) fn new(config: &StrategyConfig, market_id: &str, fill_seq: u64, now: Instant) -> Self {
        let common = config.common();
        Self {
            kind: config.kind(),
            state: StrategyState::new(config, market_id),
            guard: RefGuard::new(common),
            gate: TakeGate::default(),
            cap: SettleCap::new(common.map_or(0, |c| c.max_settlements_per_hour)),
            stats: Stats::default(),
            fill_seq,
            recent: VecDeque::new(),
            take_ids: VecDeque::new(),
            start: None,
            last_report: now,
            warned_at: None,
            bid_placed_at: None,
            offer_placed_at: None,
            last_reading: None,
            depth_fails: 0,
            age_logged: false,
            rest_until: None,
            history_retry_at: None,
        }
    }

    /// New fills of this market, each counted once.
    pub(crate) fn absorb(&mut self, fills: &[Fill], market_id: &str) {
        for fill in fills {
            self.fill_seq = self.fill_seq.max(fill.seq);
            if fill.market_id != market_id {
                continue;
            }
            let side = if fill.side == OrderType::Bid as i32 { OrderType::Bid } else { OrderType::Offer };
            let base = fill.quantity.to_f64().unwrap_or(0.0);
            let quote = base * fill.price.to_f64().unwrap_or(0.0);
            self.recent.push_back(RecentFill { at: fill.at, side, base, quote });
            self.stats.fills = self.stats.fills.saturating_add(1);
            if side == OrderType::Bid {
                self.stats.bought += base;
            } else {
                self.stats.sold += base;
            }
            if let Some(entry) = self.take_ids.iter_mut().find(|(id, m)| *id == fill.order_id && *m != TakeMark::Filled) {
                // A late match corrects an earlier empty verdict
                if entry.1 == TakeMark::Empty {
                    self.stats.takes_empty = self.stats.takes_empty.saturating_sub(1);
                }
                entry.1 = TakeMark::Filled;
                self.stats.takes_filled = self.stats.takes_filled.saturating_add(1);
            }
            self.state.on_fill(fill);
        }
    }

    /// Drop fills older than an hour.
    pub(crate) fn prune(&mut self, now: Instant) {
        while self.recent.front().is_some_and(|f| now.saturating_duration_since(f.at) >= HOUR) {
            self.recent.pop_front();
        }
    }

    pub(crate) fn usd_last_hour(&self) -> f64 {
        self.recent.iter().fold(0.0, |acc, f| acc + f.quote)
    }

    pub(crate) fn note_take(&mut self, order_id: u64) {
        self.stats.takes = self.stats.takes.saturating_add(1);
        if self.take_ids.len() >= TAKE_IDS_KEPT {
            self.take_ids.pop_front();
        }
        self.take_ids.push_back((order_id, TakeMark::Open));
    }

    /// A take left the book; without any match it counts as empty.
    pub(crate) fn note_resolved(&mut self, order_id: u64, matched: bool) {
        if matched {
            return;
        }
        if let Some(entry) = self.take_ids.iter_mut().find(|(id, m)| *id == order_id && *m == TakeMark::Open) {
            entry.1 = TakeMark::Empty;
            self.stats.takes_empty = self.stats.takes_empty.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn the_reference_guard_pauses_on_stale_jumping_or_bounded_readings() {
        let now = Instant::now();
        let secs = Duration::from_secs;
        let fresh = secs(1);
        let mut g = RefGuard::new(None);
        assert_eq!(g.check(0.0016, fresh, now), Ok(0.0016));
        // A served stamp up to a minute old is normal; past 90 s it is stale
        assert_eq!(g.check(0.0016, secs(60), now), Ok(0.0016));
        let stale = g.check(0.0016, secs(91), now);
        assert_eq!(stale, Err(RefReject::Stale { age: secs(91), stale_for: secs(1) }));
        assert!(stale.is_err_and(|r| !r.withdraws()), "a maker holds its quotes");
        assert_eq!(g.check(0.00161, fresh, now), Ok(0.00161));
        // A 2% jump is refused until it repeats three times
        assert!(matches!(g.check(0.001642, fresh, now), Err(RefReject::Jump(_))));
        assert!(g.check(0.001643, fresh, now).is_err_and(|r| r.withdraws()));
        assert_eq!(g.check(0.001642, fresh, now), Ok(0.001642));
        assert_eq!(g.check(0.001641, fresh, now), Ok(0.001641));

        // Stale for 300 s by the local clock, or by the stamp itself: makers withdraw
        assert!(g.check(0.001641, secs(100), now).is_err_and(|r| !r.withdraws()));
        let later = now + REF_STALE_WITHDRAW;
        assert!(g.check(0.001641, secs(100), later).is_err_and(|r| r.withdraws()));
        assert_eq!(g.check(0.001641, fresh, later), Ok(0.001641));
        assert!(g.check(0.001641, secs(390), later).is_err_and(|r| r.withdraws()));

        let common = CommonParams {
            max_settlements_per_hour: 1,
            ref_floor: Some(0.0014),
            ref_cap: Some(0.0056),
            ref_max_age_secs: Some(20),
        };
        let mut g = RefGuard::new(Some(common));
        assert_eq!(g.max_age(), secs(20));
        assert!(matches!(g.check(0.0016, secs(21), now), Err(RefReject::Stale { .. })));
        assert!(g.check(0.001413, fresh, now).is_err_and(|r| r == RefReject::NearBound && r.withdraws()));
        assert_eq!(g.check(0.00555, fresh, now), Err(RefReject::NearBound));
        assert_eq!(g.check(0.0016, fresh, now), Ok(0.0016));
    }

    fn recent(at: Instant) -> RecentFill {
        RecentFill { at, side: OrderType::Bid, base: 1.0, quote: 1.0 }
    }

    #[test]
    fn the_settlement_cap_pauses_once_then_stops() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut cap = SettleCap::new(3);
        let mut fills: VecDeque<RecentFill> = [at(0), at(1)].into_iter().map(recent).collect();
        assert_eq!(cap.check(&fills, at(2)), CapState::Running);
        fills.push_back(recent(at(3)));
        assert_eq!(cap.check(&fills, at(4)), CapState::Paused { new: true });
        assert_eq!(cap.check(&fills, at(500)), CapState::Paused { new: false });
        // After the pause only fills since the resume count
        assert_eq!(cap.check(&fills, at(905)), CapState::Running);
        for s in [910, 911, 912] {
            fills.push_back(recent(at(s)));
        }
        assert_eq!(cap.check(&fills, at(913)), CapState::Stopped { new: true });
        assert_eq!(cap.check(&fills, at(99_999)), CapState::Stopped { new: false });
    }

    #[test]
    fn the_runtime_counts_each_fill_of_its_market_once() {
        let now = Instant::now();
        let config = StrategyConfig::NoiseTaker(Default::default());
        let mut rt = StrategyRuntime::new(&config, "A-B", 0, now);
        rt.note_take(7);
        let fill = |seq: u64, market: &str, order_id: u64| Fill {
            seq,
            at: now,
            order_id,
            market_id: market.into(),
            side: OrderType::Offer as i32,
            price: Decimal::new(2, 0),
            quantity: Decimal::new(5, 0),
            proposal_id: format!("p{seq}"),
        };
        rt.absorb(&[fill(1, "A-B", 7), fill(2, "C-D", 8), fill(3, "A-B", 7)], "A-B");
        assert_eq!((rt.fill_seq, rt.stats.fills, rt.stats.takes, rt.stats.takes_filled), (3, 2, 1, 1));
        assert_eq!((rt.stats.sold, rt.usd_last_hour()), (10.0, 20.0));
        rt.prune(now + HOUR);
        assert!(rt.recent.is_empty());

        // A take gone without a match is empty; a filled one or a matched one is not
        rt.note_resolved(7, false);
        rt.note_take(9);
        rt.note_take(10);
        rt.note_resolved(9, true);
        rt.note_resolved(10, false);
        rt.note_resolved(10, false);
        assert_eq!((rt.stats.takes, rt.stats.takes_filled, rt.stats.takes_empty), (3, 1, 1));
        // A match seen late turns an empty take into a filled one
        rt.absorb(&[fill(4, "A-B", 10)], "A-B");
        assert_eq!((rt.stats.takes_filled, rt.stats.takes_empty), (2, 0));
    }
}
