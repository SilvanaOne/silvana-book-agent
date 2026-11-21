//! Time-based EMA: a reading weighs more the longer it follows the last one.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ema {
    tau_secs: f64,
    value: Option<f64>,
    last: Option<Instant>,
    first: Option<Instant>,
}

impl Ema {
    pub(crate) fn new(tau_secs: f64) -> Self {
        Self { tau_secs, value: None, last: None, first: None }
    }

    /// alpha = 1 - exp(-dt / tau); the first reading sets the value.
    pub(crate) fn update(&mut self, x: f64, now: Instant) {
        if !x.is_finite() {
            return;
        }
        self.value = Some(match (self.value, self.last) {
            (Some(v), Some(last)) => {
                let dt = now.saturating_duration_since(last).as_secs_f64();
                let alpha = if self.tau_secs > 0.0 { 1.0 - (-dt / self.tau_secs).exp() } else { 1.0 };
                v + alpha * (x - v)
            }
            _ => {
                self.first = Some(now);
                x
            }
        });
        self.last = Some(now);
    }

    pub(crate) fn value(&self) -> Option<f64> {
        self.value
    }

    /// True once the readings span at least `warmup`.
    pub(crate) fn warm(&self, now: Instant, warmup: Duration) -> bool {
        self.first.is_some_and(|f| now.saturating_duration_since(f) >= warmup)
    }

    /// An EMA at `value`, first read at `first` and last at `last`.
    #[cfg(test)]
    pub(crate) fn primed(tau_secs: f64, value: f64, first: Instant, last: Instant) -> Self {
        Self { tau_secs, value: Some(value), last: Some(last), first: Some(first) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_follow_the_elapsed_time() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut e = Ema::new(100.0);
        assert_eq!(e.value(), None);
        e.update(1.0, t0);
        assert_eq!(e.value(), Some(1.0));
        // One tau later the gap closes by 1 - 1/e
        e.update(2.0, at(100));
        let expect = 1.0 + (1.0 - (-1.0f64).exp());
        assert!((e.value().unwrap() - expect).abs() < 1e-12);
        // No time passed: no weight; NaN is ignored
        let v = e.value();
        e.update(50.0, at(100));
        e.update(f64::NAN, at(150));
        assert_eq!(e.value(), v);
        // Two short steps equal one long step
        let (mut a, mut b) = (Ema::new(60.0), Ema::new(60.0));
        a.update(1.0, t0);
        b.update(1.0, t0);
        a.update(3.0, at(10));
        a.update(3.0, at(20));
        b.update(3.0, at(20));
        assert!((a.value().unwrap() - b.value().unwrap()).abs() < 1e-12);
        assert!(!e.warm(at(99), Duration::from_secs(100)) && e.warm(at(100), Duration::from_secs(100)));
    }
}
