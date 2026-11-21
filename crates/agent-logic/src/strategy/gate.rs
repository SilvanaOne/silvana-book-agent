//! When a taker may send its next take.

use std::time::{Duration, Instant};

/// A take younger than this still counts as in flight.
pub(crate) const MIN_TAKE_AGE: Duration = Duration::from_secs(15);
/// The same after a submit whose outcome is unknown.
pub(crate) const AMBIGUOUS_TAKE_AGE: Duration = Duration::from_secs(120);
/// A take still listed after this is released locally.
pub(crate) const STUCK_AFTER: Duration = Duration::from_secs(900);
/// The balance snapshot must be this much newer than the last fill.
pub(crate) const SNAPSHOT_AFTER_FILL: Duration = Duration::from_secs(5);
/// Pause after the server refused a take.
pub(crate) const REFUSED_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LastTake {
    pub order_id: Option<u64>,
    pub at: Instant,
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateWhy {
    Backoff,
    Listed,
    Young,
    Settling,
    Snapshot,
    Cooldown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Open,
    Closed(GateWhy),
    /// Still listed past `STUCK_AFTER`: released, the caller marks it inactive
    Stuck(LastTake),
}

pub(crate) struct GateInputs {
    pub now: Instant,
    /// The last take is in the current order listing
    pub listed: bool,
    pub open_settlements: usize,
    pub balances_at: Option<Instant>,
    pub last_fill_at: Option<Instant>,
    pub cooldown: Duration,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TakeGate {
    pub last: Option<LastTake>,
    pub last_placed_at: Option<Instant>,
    pub backoff_until: Option<Instant>,
}

impl TakeGate {
    /// Judges the gate; a take that has run its course is cleared and returned.
    pub(crate) fn check(&mut self, i: &GateInputs) -> (Verdict, Option<LastTake>) {
        if self.backoff_until.is_some_and(|t| i.now < t) {
            return (Verdict::Closed(GateWhy::Backoff), None);
        }
        let mut resolved = None;
        if let Some(last) = self.last {
            let age = i.now.saturating_duration_since(last.at);
            if i.listed {
                if age >= STUCK_AFTER {
                    self.last = None;
                    return (Verdict::Stuck(last), None);
                }
                return (Verdict::Closed(GateWhy::Listed), None);
            }
            let min_age = if last.ambiguous { AMBIGUOUS_TAKE_AGE } else { MIN_TAKE_AGE };
            if age < min_age {
                return (Verdict::Closed(GateWhy::Young), None);
            }
            self.last = None;
            resolved = Some(last);
        }
        if i.open_settlements > 0 {
            return (Verdict::Closed(GateWhy::Settling), resolved);
        }
        if let Some(fill) = i.last_fill_at {
            let need = fill.checked_add(SNAPSHOT_AFTER_FILL);
            if !need.is_some_and(|need| i.balances_at.is_some_and(|b| b >= need)) {
                return (Verdict::Closed(GateWhy::Snapshot), resolved);
            }
        }
        if self.last_placed_at.is_some_and(|t| i.now.saturating_duration_since(t) < i.cooldown) {
            return (Verdict::Closed(GateWhy::Cooldown), resolved);
        }
        (Verdict::Open, resolved)
    }

    pub(crate) fn placed(&mut self, order_id: u64, now: Instant) {
        self.last = Some(LastTake { order_id: Some(order_id), at: now, ambiguous: false });
        self.last_placed_at = Some(now);
    }

    /// The submit failed in a way that may still have booked the order.
    pub(crate) fn ambiguous(&mut self, now: Instant) {
        self.last = Some(LastTake { order_id: None, at: now, ambiguous: true });
        self.last_placed_at = Some(now);
    }

    /// The server refused the order outright.
    pub(crate) fn refused(&mut self, now: Instant) {
        self.backoff_until = now.checked_add(REFUSED_BACKOFF);
    }

    pub(crate) fn order_id(&self) -> Option<u64> {
        self.last.and_then(|l| l.order_id)
    }

    /// The last take left the book just now; it still counts as in flight for `MIN_TAKE_AGE`.
    pub(crate) fn restart_hold(&mut self, now: Instant) {
        if let Some(last) = self.last.as_mut() {
            last.at = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(now: Instant) -> GateInputs {
        GateInputs { now, listed: false, open_settlements: 0, balances_at: Some(now), last_fill_at: None, cooldown: Duration::ZERO }
    }

    #[test]
    fn gate_truth_table() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut g = TakeGate::default();
        assert_eq!(g.check(&inputs(t0)).0, Verdict::Open);

        g.placed(7, t0);
        assert_eq!(g.order_id(), Some(7));
        let listed = GateInputs { listed: true, ..inputs(at(5)) };
        assert_eq!(g.check(&listed).0, Verdict::Closed(GateWhy::Listed));
        assert_eq!(g.check(&inputs(at(5))).0, Verdict::Closed(GateWhy::Young));
        // Gone from the listing and old enough: resolved, then the settlement holds the gate
        let settling = GateInputs { open_settlements: 1, ..inputs(at(16)) };
        let (v, resolved) = g.check(&settling);
        assert_eq!((v, resolved.and_then(|l| l.order_id)), (Verdict::Closed(GateWhy::Settling), Some(7)));
        assert_eq!(g.last, None);
        // Settled: wait for a balance snapshot 5 s past the fill
        let fill = GateInputs { last_fill_at: Some(at(20)), balances_at: Some(at(22)), ..inputs(at(23)) };
        assert_eq!(g.check(&fill).0, Verdict::Closed(GateWhy::Snapshot));
        let fresh = GateInputs { last_fill_at: Some(at(20)), balances_at: Some(at(25)), ..inputs(at(26)) };
        assert_eq!(g.check(&fresh).0, Verdict::Open);
        // Cooldown counts from the placement
        let cool = GateInputs { cooldown: Duration::from_secs(150), ..inputs(at(100)) };
        assert_eq!(g.check(&cool).0, Verdict::Closed(GateWhy::Cooldown));
        assert_eq!(g.check(&GateInputs { cooldown: Duration::from_secs(150), ..inputs(at(150)) }).0, Verdict::Open);

        // An ambiguous submit holds for 120 s
        g.ambiguous(at(200));
        assert_eq!(g.check(&inputs(at(300))).0, Verdict::Closed(GateWhy::Young));
        assert_eq!(g.check(&inputs(at(320))).0, Verdict::Open);

        // A refusal backs off 60 s
        g.refused(at(400));
        assert_eq!(g.check(&inputs(at(459))).0, Verdict::Closed(GateWhy::Backoff));
        assert_eq!(g.check(&inputs(at(460))).0, Verdict::Open);

        // A resting take that left the book holds for 15 s from then
        g.placed(9, at(500));
        g.restart_hold(at(600));
        assert_eq!(g.check(&inputs(at(610))).0, Verdict::Closed(GateWhy::Young));
        assert_eq!(g.check(&inputs(at(615))).0, Verdict::Open);

        // Listed for 15 min: released as stuck
        g.placed(8, at(1000));
        let stuck = GateInputs { listed: true, ..inputs(at(1900)) };
        assert!(matches!(g.check(&stuck).0, Verdict::Stuck(LastTake { order_id: Some(8), .. })));
        assert_eq!(g.last, None);
    }
}
