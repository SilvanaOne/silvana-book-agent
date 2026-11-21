//! SplitMix64: a small fixed generator, so a seeded run replays exactly.

/// 2^53, for uniform floats in [0, 1).
const TWO_POW_53: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ z.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ z.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ z.wrapping_shr(31)
    }

    /// Uniform in [0, 1).
    pub(crate) fn next_f64(&mut self) -> f64 {
        self.next_u64().wrapping_shr(11) as f64 / TWO_POW_53
    }

    /// Exponential with the given mean.
    pub(crate) fn exponential(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.next_f64()).ln()
    }
}

/// Seed from the start time and the market id, for runs without a configured seed.
pub(crate) fn seed_for(market_id: &str) -> u64 {
    let hash = market_id
        .bytes()
        .fold(0xCBF2_9CE4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3));
    (crate::clock::now_micros_i64() as u64) ^ hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_reference_sequence() {
        // splitmix64 with seed 0
        let mut r = SplitMix64::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(r.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn a_seed_replays_and_floats_stay_in_range() {
        let (mut a, mut b) = (SplitMix64::new(8), SplitMix64::new(8));
        for _ in 0..1000 {
            let x = a.next_f64();
            assert_eq!(x, b.next_f64());
            assert!((0.0..1.0).contains(&x));
        }
        assert_ne!(SplitMix64::new(9).next_u64(), SplitMix64::new(8).next_u64());
        let mut e = SplitMix64::new(1);
        let mean = (0..20_000).fold(0.0, |s, _| s + e.exponential(60.0)) / 20_000.0;
        assert!((mean - 60.0).abs() < 3.0, "{mean}");
        assert_ne!(seed_for("A-B"), seed_for("C-D"));
    }
}
