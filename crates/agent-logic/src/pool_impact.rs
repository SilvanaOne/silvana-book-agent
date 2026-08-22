//! Size-aware quote adjustment derived from a counterparty's trailing net.
//! Pure math — no locks, no IO.

use serde::{Deserialize, Serialize};

use crate::config::PoolImpactConfig;

/// Agent-local size reference for a market, mapped from the server's
/// `pricing::PoolDepth`. An unsupported or absent value means no adjustment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolDepth {
    /// Reference size in THIS market's base-token units.
    pub base_reserve: f64,
}

impl PoolDepth {
    /// Map the proto message; anything unsupported or non-positive is `None`
    /// so consumers fall back to no adjustment.
    pub fn from_proto(p: &orderbook_proto::pricing::PoolDepth) -> Option<Self> {
        if p.model != "constant_product" {
            return None;
        }
        if !(p.base_reserve.is_finite() && p.base_reserve > 0.0) {
            return None;
        }
        Some(Self { base_reserve: p.base_reserve })
    }
}

/// The mid and the size reference that arrived with it, kept in one entry so
/// the reference can never outlive its mid.
#[derive(Debug, Clone)]
pub struct MarketMid {
    pub mid: f64,
    pub pool_depth: Option<PoolDepth>,
}

/// Shared mid-price map (market_id → mid + depth), written only by the
/// cloud-agent's mid-price poller.
pub type MidPriceMap =
    std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, MarketMid>>>;

/// Which side the USER is on (mirrors the v1 RFQ direction enum: 1 = user
/// buys base / LP sells; 2 = user sells base / LP buys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactSide {
    UserBuys,
    UserSells,
}

/// Adjustment percent for a trade of `quantity_base` at a trailing
/// `signed_net`. Every knob is clamped, so the worst case is 0 or the cap.
#[allow(clippy::too_many_arguments)]
pub fn marginal_impact_percent(
    side: ImpactSide,
    quantity_base: f64,
    signed_net: f64,
    base_reserve: f64,
    free_zone_base: f64,
    max_pool_fraction: f64,
    impact_multiplier: f64,
    max_impact_percent: f64,
) -> f64 {
    if !(base_reserve.is_finite() && base_reserve > 0.0) {
        return 0.0;
    }
    let q = if quantity_base.is_finite() { quantity_base.max(0.0) } else { 0.0 };
    let net = if signed_net.is_finite() { signed_net } else { 0.0 };
    let fz = if free_zone_base.is_finite() { free_zone_base.max(0.0) } else { 0.0 };
    // u → 1 blows f_buy up; cap the cappable fraction strictly below 1.
    let frac = if max_pool_fraction.is_finite() {
        max_pool_fraction.clamp(0.0, 0.99)
    } else {
        0.99
    };
    let cap = if max_impact_percent.is_finite() { max_impact_percent.max(0.0) } else { 0.0 };
    let mult = if impact_multiplier.is_finite() { impact_multiplier.max(0.0) } else { 0.0 };

    // Charged position before/after the trade, past the free zone (see module
    // docs for the sign conventions).
    let (x0, x1) = match side {
        ImpactSide::UserBuys => ((net - fz).max(0.0), (net + q - fz).max(0.0)),
        ImpactSide::UserSells => ((-net - fz).max(0.0), (q - net - fz).max(0.0)),
    };
    // f(x) = the pool's average-execution MARKUP, as a percent, for moving a
    // cumulative x through it.
    let f = |x: f64| -> f64 {
        let u = (x / base_reserve).clamp(0.0, frac);
        match side {
            ImpactSide::UserBuys => 100.0 * u / (1.0 - u),
            ImpactSide::UserSells => 100.0 * u / (1.0 + u),
        }
    };
    // Increment in TOTAL cost spread over this trade. Must stay the
    // g-difference: the f-difference is not invariant to how a total is split.
    let g = |x: f64| -> f64 { x * f(x) };
    if q <= 0.0 {
        return 0.0;
    }
    (((g(x1) - g(x0)) / q).max(0.0) * mult).clamp(0.0, cap)
}

/// Config-driven wrapper of [`marginal_impact_percent`] using the market's
/// `[markets.rfq.pool_impact]` knobs and its pool depth.
pub fn pool_impact_percent(
    side: ImpactSide,
    quantity_base: f64,
    signed_net: f64,
    depth: &PoolDepth,
    cfg: &PoolImpactConfig,
) -> f64 {
    marginal_impact_percent(
        side,
        quantity_base,
        signed_net,
        depth.base_reserve,
        cfg.free_zone_base,
        cfg.max_pool_fraction,
        cfg.impact_multiplier,
        cfg.max_impact_percent,
    )
}

/// Solve `b · price_at(b) = quote_quantity` by bisection on the bracket
/// `[Q/price_ceil, Q/price_floor]`; callers must honour those bounds.
pub fn solve_base_for_quote(
    quote_quantity: f64,
    price_floor: f64,
    price_ceil: f64,
    price_at: impl Fn(f64) -> f64,
) -> Option<f64> {
    if !(quote_quantity.is_finite() && quote_quantity > 0.0) {
        return None;
    }
    if !(price_floor.is_finite() && price_ceil.is_finite()) {
        return None;
    }
    if !(price_floor > 0.0 && price_ceil >= price_floor) {
        return None;
    }
    let mut lo = quote_quantity / price_ceil;
    let mut hi = quote_quantity / price_floor;
    // The bound contract brackets the root from the start.
    for _ in 0..64 {
        if (hi - lo) <= 1e-12 * hi {
            break;
        }
        let mid = 0.5 * (lo + hi);
        let h = mid * price_at(mid) - quote_quantity;
        if h.is_nan() {
            return None;
        }
        if h > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f64 = 42_500_000.0;

    fn impact(side: ImpactSide, q: f64, net: f64) -> f64 {
        // No free zone, multiplier 1, generous cap, so the raw curve shows.
        marginal_impact_percent(side, q, net, R, 0.0, 0.99, 1.0, 1e9)
    }

    /// THE load-bearing property: a given total costs the same however it is
    /// split. If this fails the charge has become split-dependent.
    #[test]
    fn total_charge_is_invariant_to_split_count() {
        const TOTAL: f64 = 1_400_000.0;
        // Charge is (percent x size); sum it over n equal parts.
        let paid = |n: u32| -> f64 {
            let q = TOTAL / f64::from(n);
            let mut net = 0.0;
            let mut sum = 0.0;
            for _ in 0..n {
                sum += impact(ImpactSide::UserBuys, q, net) * q;
                net += q;
            }
            sum
        };
        let one_lot = paid(1);
        assert!(one_lot > 0.0);
        for n in [2u32, 5, 10, 30, 100, 250] {
            let rel = (paid(n) - one_lot).abs() / one_lot;
            assert!(
                rel < 1e-9,
                "splitting into {n} changed the total by {rel:.3e} \
                 (whole {one_lot:.2}, split {:.2})",
                paid(n)
            );
        }
    }

    /// The same invariance on the sell side.
    #[test]
    fn sell_side_charge_is_invariant_to_split_count() {
        const TOTAL: f64 = 900_000.0;
        let paid = |n: u32| -> f64 {
            let q = TOTAL / f64::from(n);
            let mut net = 0.0;
            let mut sum = 0.0;
            for _ in 0..n {
                sum += impact(ImpactSide::UserSells, q, net) * q;
                net -= q; // selling drives the signed net negative
            }
            sum
        };
        let one_lot = paid(1);
        assert!(one_lot > 0.0);
        for n in [5u32, 25, 100] {
            let rel = (paid(n) - one_lot).abs() / one_lot;
            assert!(rel < 1e-9, "sell-side split into {n} changed the total by {rel:.3e}");
        }
    }

    /// Ordinary-sized flow at net≈0 must not move a narrow quote materially.
    #[test]
    fn ordinary_flow_is_not_materially_widened() {
        let avg = impact(ImpactSide::UserBuys, 6_500.0, 0.0);
        assert!(avg < 0.02, "ordinary trade would widen by {avg:.4}%");
        // A free zone above ordinary churn zeroes it entirely.
        let with_free_zone =
            marginal_impact_percent(ImpactSide::UserBuys, 6_500.0, 50_000.0, R, 100_000.0, 0.99, 1.0, 1e9);
        assert_eq!(with_free_zone, 0.0, "free zone must fully exempt ordinary flow");
    }

    /// `impact(q=0, net=0) == 0` exactly — the parity invariant the fast path
    /// relies on.
    #[test]
    fn zero_trade_zero_net_is_exactly_zero() {
        assert_eq!(impact(ImpactSide::UserBuys, 0.0, 0.0), 0.0);
        assert_eq!(impact(ImpactSide::UserSells, 0.0, 0.0), 0.0);
    }

    /// Buy side matches the reference curve from a zero net.
    #[test]
    fn buy_matches_reference_curve() {
        let q = 0.1 * R;
        let expect = 100.0 * 0.1 / 0.9;
        assert!((impact(ImpactSide::UserBuys, q, 0.0) - expect).abs() < 1e-9);
    }

    /// Selling X back while net = +X keys the sell side on max(0, −net) = 0.
    #[test]
    fn closing_leg_is_free() {
        let x = 500_000.0;
        assert_eq!(impact(ImpactSide::UserSells, x, x), 0.0);
        // Selling past the free zone charges only the excess below net 0.
        assert!(impact(ImpactSide::UserSells, x + 1000.0, x) > 0.0);
    }

    /// Each successive same-size buy from a higher net pays strictly more.
    #[test]
    fn charge_increases_with_net() {
        let step = 200_000.0;
        let mut net = 0.0;
        let mut last = -1.0;
        for _ in 0..10 {
            let charge = impact(ImpactSide::UserBuys, step, net);
            assert!(charge > last, "marginal charge must increase with net");
            last = charge;
            net += step;
        }
    }

    /// Stated in cost, not percent: percentages of different sizes are not
    /// commensurable, and comparing them hides a split-dependent charge.
    #[test]
    fn parts_accumulate_like_the_whole() {
        let big = 1_400_000.0;
        let one_shot = impact(ImpactSide::UserBuys, big, 0.0) * big;
        let mut net = 0.0;
        let mut total = 0.0;
        for _ in 0..100 {
            total += impact(ImpactSide::UserBuys, 14_000.0, net) * 14_000.0;
            net += 14_000.0;
        }
        let rel = (total - one_shot).abs() / one_shot;
        assert!(rel < 1e-9, "split {total} vs whole {one_shot} (rel {rel:.3e})");
    }

    /// The free zone exempts the first `free_zone_base` of net on BOTH sides,
    /// and the sell side only starts charging below −free_zone.
    #[test]
    fn free_zone_and_sell_side_signs() {
        let fz = 100_000.0;
        let f = |side, q, net| marginal_impact_percent(side, q, net, R, fz, 0.99, 1.0, 1e9);
        // Buy fully inside the free zone: free.
        assert_eq!(f(ImpactSide::UserBuys, 50_000.0, 0.0), 0.0);
        // Sell from positive net: free until net < −fz.
        assert_eq!(f(ImpactSide::UserSells, 60_000.0, 20_000.0), 0.0); // net→−40k > −fz
        // Past −fz: only the excess is charged, spread over the whole trade,
        // so the rate is half what the excess alone would carry.
        let charged = f(ImpactSide::UserSells, 200_000.0, 0.0); // net→−200k, 100k past fz
        let excess = 100_000.0;
        let excess_rate = 100.0 * (excess / R) / (1.0 + excess / R);
        let expect = excess * excess_rate / 200_000.0;
        assert!((charged - expect).abs() < 1e-9, "{charged} vs {expect}");
    }

    /// Multiplier scales, cap clamps, and the result is never negative.
    #[test]
    fn multiplier_and_cap() {
        let base = impact(ImpactSide::UserBuys, 0.1 * R, 0.0);
        let doubled =
            marginal_impact_percent(ImpactSide::UserBuys, 0.1 * R, 0.0, R, 0.0, 0.99, 2.0, 1e9);
        assert!((doubled - 2.0 * base).abs() < 1e-9);
        let capped =
            marginal_impact_percent(ImpactSide::UserBuys, 0.5 * R, 0.0, R, 0.0, 0.99, 1.0, 50.0);
        assert_eq!(capped, 50.0);
        // Sell with a large positive net can never go negative.
        assert_eq!(impact(ImpactSide::UserSells, 1.0, 1_000_000.0), 0.0);
    }

    /// Degenerate inputs yield 0 or clamp, never panic/NaN.
    #[test]
    fn degenerate_inputs_yield_zero() {
        // NaN net is treated as 0 — same charge as a fresh counterparty.
        assert_eq!(
            impact(ImpactSide::UserBuys, 1000.0, f64::NAN),
            impact(ImpactSide::UserBuys, 1000.0, 0.0)
        );
        assert_eq!(
            marginal_impact_percent(ImpactSide::UserBuys, 1000.0, 0.0, 0.0, 0.0, 0.9, 1.0, 50.0),
            0.0
        );
        assert_eq!(
            marginal_impact_percent(ImpactSide::UserBuys, 1000.0, 0.0, f64::NAN, 0.0, 0.9, 1.0, 50.0),
            0.0
        );
        // Non-finite quantity yields 0; the pricing pipeline's own
        // finiteness guard rejects such a quote outright.
        assert_eq!(
            marginal_impact_percent(ImpactSide::UserBuys, f64::INFINITY, 0.0, R, 0.0, 0.9, 1.0, 50.0),
            0.0
        );
    }

    /// Bisection matches the closed-form root to < 1e-9 under zero spread,
    /// zero net and no binding cap.
    #[test]
    fn bisection_matches_closed_form() {
        let mid = 0.0187;
        let price_at = |b: f64| {
            mid * (1.0
                + marginal_impact_percent(
                    ImpactSide::UserBuys,
                    b,
                    0.0,
                    R,
                    0.0,
                    0.999_999,
                    1.0,
                    1e9,
                ) / 100.0)
        };
        for q_frac in [0.001, 0.05, 0.2, 0.4] {
            let quote = mid * R * q_frac; // quote units
            let ceil = mid / (1.0 - 0.999_999);
            let b = solve_base_for_quote(quote, mid, ceil, price_at).expect("must solve");
            let closed = quote * R / (mid * R + quote);
            assert!(
                (b - closed).abs() / closed < 1e-9,
                "q_frac={q_frac}: bisection {b} vs closed form {closed}"
            );
        }
    }

    /// Convergence at q = 0.5·R, where Q = mid·R makes the root exactly R/2.
    #[test]
    fn bisection_converges_at_half_reserve() {
        let mid = 0.0187;
        let price_at = |b: f64| {
            mid * (1.0
                + marginal_impact_percent(
                    ImpactSide::UserBuys,
                    b,
                    0.0,
                    R,
                    0.0,
                    0.999_999,
                    1.0,
                    1e9,
                ) / 100.0)
        };
        let quote = mid * R;
        let ceil = mid / (1.0 - 0.999_999);
        let b = solve_base_for_quote(quote, mid, ceil, price_at).expect("must solve");
        assert!(
            (b - R / 2.0).abs() / (R / 2.0) < 1e-9,
            "at Q=mid·R the root is R/2, got {b}"
        );
    }

    /// Proto mapping: only a supported model with a finite positive size maps.
    #[test]
    fn proto_mapping_rejects_bad_depth() {
        let good = orderbook_proto::pricing::PoolDepth {
            venue: String::new(),
            model: "constant_product".into(),
            base_reserve: R,
            quote_reserve: 0.0,
            quote_instrument: String::new(),
            fee_rate: 0.0,
        };
        assert_eq!(PoolDepth::from_proto(&good), Some(PoolDepth { base_reserve: R }));
        let mut bad = good.clone();
        bad.model = "other".into();
        assert_eq!(PoolDepth::from_proto(&bad), None);
        let mut bad = good.clone();
        bad.base_reserve = 0.0;
        assert_eq!(PoolDepth::from_proto(&bad), None);
        let mut bad = good;
        bad.base_reserve = f64::NAN;
        assert_eq!(PoolDepth::from_proto(&bad), None);
    }
}
