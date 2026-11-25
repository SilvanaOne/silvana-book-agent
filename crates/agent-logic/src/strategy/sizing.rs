//! Lot rounding, prices on the tick grid, and the size and price of a take.

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use super::book::{BookTop, Level};
use super::{clampf, Inventory, TakeTif, Want};

/// Strategy orders are worth at least this many USD.
pub(crate) const MIN_ORDER_USD: f64 = 12.0;
/// The server's floor in USD when it reports none.
pub(crate) const SERVER_FLOOR_USD: f64 = 10.0;
/// With no LP quote, buy only below ref + 4 bps and sell only above ref - 4 bps.
pub(crate) const NO_LP_BAND: f64 = 0.0004;
/// A resting take stays at least 1 bp from the reference.
pub(crate) const REST_REF_GAP: f64 = 0.0001;

/// Whole steps in `x`, rounded down or up; within float noise of a whole step it is
/// that step. A float resolves about 2e-16 of `x`, so the noise grows with `x`.
pub(crate) fn whole_steps(x: f64, up: bool) -> f64 {
    let near = x.round();
    if (x - near).abs() <= step_noise(x) {
        near
    } else if up {
        x.ceil()
    } else {
        x.floor()
    }
}

fn step_noise(x: f64) -> f64 {
    (8.0 * f64::EPSILON * x.abs()).max(1e-9)
}

/// `tick` as an exact decimal; a server tick has at most 10 places.
pub(crate) fn tick_decimal(tick: f64) -> Option<Decimal> {
    Decimal::from_f64_retain(tick).map(|t| t.round_dp(10)).filter(|t| *t > Decimal::ZERO)
}

/// `x` ticks as a whole number of them, rounded as [`whole_steps`]; None below one.
pub(crate) fn to_ticks(x: f64, up: bool) -> Option<i64> {
    let n = whole_steps(x, up);
    // Every whole float below 2^63 converts exactly
    (1.0..9.0e18).contains(&n).then_some(n as i64)
}

/// A book price in whole ticks, from its exact string when that parses, so a
/// price far past float precision still counts every tick.
pub(crate) fn level_ticks(level: &Level, tick: f64, up: bool) -> Option<i64> {
    let exact = Decimal::from_str_exact(level.price_str.trim()).ok().zip(tick_decimal(tick));
    let Some(x) = exact.and_then(|(p, t)| p.checked_div(t)) else {
        return to_ticks(level.price / tick, up);
    };
    let near = x.round();
    let noise = Decimal::from_f64_retain(step_noise(x.to_f64()?))?;
    let n = if x.checked_sub(near)?.abs() <= noise { near } else if up { x.ceil() } else { x.floor() };
    n.to_i64().filter(|n| *n >= 1)
}

/// Lot exponent: 10^exp base units are worth between 0.1 and 1 quote unit.
pub(crate) fn lot_exp(price: f64) -> i32 {
    if !(price.is_finite() && price > 0.0) {
        return 0;
    }
    clampf((-price.log10()).floor(), -10.0, 12.0) as i32
}

/// `qty` in whole lots of 10^exp, as an exact decimal; None below one lot.
pub(crate) fn lots(qty: f64, exp: i32, up: bool) -> Option<Decimal> {
    let units = qty / 10f64.powi(exp);
    // A hair of slack, so float noise at an exact lot boundary rounds to it
    let units = whole_steps(units, up);
    if !(1.0..1e15).contains(&units) {
        return None;
    }
    let n = Decimal::from(units as i64);
    let d = if exp >= 0 {
        n.checked_mul(Decimal::from(10i64.checked_pow(exp.unsigned_abs())?))?
    } else {
        Decimal::try_new(units as i64, exp.unsigned_abs()).ok()?
    };
    Some(d.normalize())
}

/// Base quantity worth at least `max(value, min_value, floor)` at `price`, rounded up;
/// values in quote units.
pub(crate) fn order_qty(value: f64, min_value: f64, price: f64, floor_value: f64, exp: i32) -> Option<Decimal> {
    if !(price.is_finite() && price > 0.0) {
        return None;
    }
    lots(value.max(min_value).max(floor_value) / price, exp, true)
}

/// `n` ticks as an exact price; None below one tick.
pub(crate) fn ticks_price(n: i64, tick: f64) -> Option<Decimal> {
    if n < 1 {
        return None;
    }
    Decimal::from(n).checked_mul(tick_decimal(tick)?)
}

/// `n` ticks as an exact price string.
pub(crate) fn tick_price(n: i64, tick: f64) -> Option<String> {
    Some(ticks_price(n, tick)?.normalize().to_string())
}

/// An order ready to submit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TakeOrder {
    pub side: OrderType,
    pub price: String,
    pub price_f: f64,
    pub quantity: Decimal,
    pub tif: TakeTif,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipWhy {
    NoLevel,
    NotBetterThanLp,
    Slippage,
    TooSmall,
}

/// P4: an IOC at exactly the best agent price, never larger than one order of that level.
/// With `take_lp` the LP's best level counts too when it is better; at the same price the agent's comes first.
pub(crate) fn size_take(
    want: &Want,
    book: &BookTop,
    inv: &Inventory,
    floor_value: f64,
    reference: f64,
    exp: i32,
    take_lp: bool,
) -> Result<TakeOrder, SkipWhy> {
    let dev = want.max_deviation_percent / 100.0;
    let buy = want.side == OrderType::Bid;
    let (agent, lp) = if buy { (&book.agent_ask, &book.lp_ask) } else { (&book.agent_bid, &book.lp_bid) };
    // A lower offer is better to buy from, a higher bid to sell to
    let better = |a: f64, b: f64| if buy { a < b } else { a > b };
    let level = match (agent, lp) {
        (Some(a), Some(l)) if take_lp && better(l.price, a.price) => l,
        (Some(a), lp) => {
            let beats_lp = match lp {
                Some(l) => take_lp || better(a.price, l.price),
                None if buy => a.price < reference * (1.0 + NO_LP_BAND),
                None => a.price > reference * (1.0 - NO_LP_BAND),
            };
            if !beats_lp {
                return Err(SkipWhy::NotBetterThanLp);
            }
            a
        }
        (None, Some(l)) if take_lp => l,
        (None, _) => return Err(SkipWhy::NoLevel),
    };
    let slipped = if buy { level.price > reference * (1.0 + dev) } else { level.price < reference * (1.0 - dev) };
    if slipped {
        return Err(SkipWhy::Slippage);
    }
    let funds = if buy { inv.quote_avail / level.price } else { inv.base_avail };
    // Depth gives a level's total and order count, not each order's size, so an
    // even share can still fill two unequal orders there
    let per_order = level.qty / f64::from(level.count.max(1));
    let qty = want.max_quantity.min(per_order).min(funds);
    let quantity = lots(qty, exp, false).ok_or(SkipWhy::TooSmall)?;
    if quantity.to_f64().unwrap_or(0.0) * level.price < floor_value {
        return Err(SkipWhy::TooSmall);
    }
    Ok(TakeOrder { side: want.side, price: level.price_str.clone(), price_f: level.price, quantity, tif: want.tif })
}

/// A resting take one tick inside its side, at least 1 bp from the reference, never crossing.
pub(crate) fn price_rest(
    want: &Want,
    book: &BookTop,
    inv: &Inventory,
    floor_value: f64,
    reference: f64,
    tick: f64,
    exp: i32,
) -> Result<TakeOrder, SkipWhy> {
    if !(tick.is_finite() && tick > 0.0) {
        return Err(SkipWhy::NoLevel);
    }
    // In whole ticks, each side's prices rounded the way the order rounds
    let up = want.side == OrderType::Offer;
    let ticks = |a: &Option<Level>, b: &Option<Level>| {
        [a, b].into_iter().flatten().filter_map(|l| level_ticks(l, tick, up)).collect::<Vec<i64>>()
    };
    let (bids, asks) = (ticks(&book.agent_bid, &book.lp_bid), ticks(&book.agent_ask, &book.lp_ask));
    let (n, funds) = if want.side == OrderType::Bid {
        let gap = to_ticks(reference * (1.0 - REST_REF_GAP) / tick, false).ok_or(SkipWhy::NoLevel)?;
        let mut n = bids.iter().max().map_or(gap, |b| b.saturating_add(1)).min(gap);
        if let Some(ask) = asks.iter().min() {
            n = n.min(ask.saturating_sub(1));
        }
        (n, inv.quote_avail / (n as f64 * tick))
    } else {
        let gap = to_ticks(reference * (1.0 + REST_REF_GAP) / tick, true).ok_or(SkipWhy::NoLevel)?;
        let mut n = asks.iter().min().map_or(gap, |a| a.saturating_sub(1)).max(gap);
        if let Some(bid) = bids.iter().max() {
            n = n.max(bid.saturating_add(1));
        }
        (n, inv.base_avail)
    };
    let price = tick_price(n, tick).ok_or(SkipWhy::NoLevel)?;
    let price_f = n as f64 * tick;
    let quantity = lots(want.max_quantity.min(funds), exp, false).ok_or(SkipWhy::TooSmall)?;
    if quantity.to_f64().unwrap_or(0.0) * price_f < floor_value {
        return Err(SkipWhy::TooSmall);
    }
    Ok(TakeOrder { side: want.side, price, price_f, quantity, tif: want.tif })
}

#[cfg(test)]
mod tests {
    use super::super::book::Level;
    use super::*;

    fn level(price: f64, qty: f64, count: u32) -> Level {
        Level { price, price_str: format!("{price}"), qty, count }
    }

    fn inv() -> Inventory {
        Inventory { base: 1e6, quote: 2000.0, base_avail: 1e6, quote_avail: 2000.0, usable: true }
    }

    fn want(side: OrderType, max_quantity: f64) -> Want {
        Want { side, max_quantity, max_deviation_percent: 0.15, tif: TakeTif::Ioc }
    }

    #[test]
    fn lots_and_prices() {
        assert_eq!((lot_exp(0.0016), lot_exp(0.0056), lot_exp(0.15), lot_exp(2.5), lot_exp(90_000.0)), (2, 2, 0, -1, -5));
        assert_eq!(lots(12_345.0, 2, false).map(|d| d.to_string()), Some("12300".into()));
        assert_eq!(lots(12_345.0, 2, true).map(|d| d.to_string()), Some("12400".into()));
        assert_eq!(lots(1.23456, -3, false).map(|d| d.to_string()), Some("1.234".into()));
        assert_eq!(lots(50.0, 2, false), None);
        assert_eq!(lots(f64::NAN, 2, true), None);
        // $12 at 0.0016 is 7500 HTEST; a $40 rung is 25000
        assert_eq!(order_qty(5.0, MIN_ORDER_USD, 0.0016, 10.1, 2).map(|d| d.to_string()), Some("7500".into()));
        assert_eq!(order_qty(5.0, MIN_ORDER_USD, 0.0016, 12.5, 2).map(|d| d.to_string()), Some("7900".into()));
        assert_eq!(order_qty(40.0, MIN_ORDER_USD, 0.0016, 0.0, 2).map(|d| d.to_string()), Some("25000".into()));
        assert_eq!(tick_price(16_123, 1e-7).as_deref(), Some("0.0016123"));
        assert_eq!(tick_price(0, 1e-7), None);
    }

    #[test]
    fn size_take_truth_table() {
        let r = 0.0016;
        let book = BookTop {
            agent_bid: Some(level(0.001599, 30_000.0, 3)),
            agent_ask: Some(level(0.001601, 20_000.0, 1)),
            lp_bid: Some(level(0.001598, 1e4, 1)),
            lp_ask: Some(level(0.001602, 1e4, 1)),
        };
        // Buy at exactly the agent offer, capped by its one order
        let t = size_take(&want(OrderType::Bid, 50_000.0), &book, &inv(), 10.0, r, 2, false).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string()), ("0.001601", "20000".to_string()));
        // Sell: three orders share the level, so a third of it at most
        let t = size_take(&want(OrderType::Offer, 50_000.0), &book, &inv(), 10.0, r, 2, false).unwrap();
        assert_eq!((t.price_f, t.quantity.to_string()), (0.001599, "10000".to_string()));
        // Not better than the LP, beyond slippage, too small, or no level
        let lp_better = BookTop { lp_ask: Some(level(0.001601, 1e4, 1)), ..book.clone() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &lp_better, &inv(), 10.0, r, 2, false), Err(SkipWhy::NotBetterThanLp));
        let tight = Want { max_deviation_percent: 0.05, ..want(OrderType::Bid, 1e5) };
        assert_eq!(size_take(&tight, &book, &inv(), 10.0, r, 2, false), Err(SkipWhy::Slippage));
        assert_eq!(size_take(&want(OrderType::Bid, 5000.0), &book, &inv(), 10.0, r, 2, false), Err(SkipWhy::TooSmall));
        let poor = Inventory { quote_avail: 5.0, ..inv() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &book, &poor, 10.0, r, 2, false), Err(SkipWhy::TooSmall));
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &BookTop::default(), &inv(), 10.0, r, 2, false), Err(SkipWhy::NoLevel));
        // No LP quote: only within 4 bps of the reference
        let no_lp = BookTop { lp_ask: None, agent_ask: Some(level(0.0016007, 20_000.0, 1)), ..book.clone() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &no_lp, &inv(), 10.0, r, 2, false), Err(SkipWhy::NotBetterThanLp));
        let no_lp = BookTop { lp_ask: None, agent_ask: Some(level(0.0016005, 20_000.0, 1)), ..book };
        assert!(size_take(&want(OrderType::Bid, 1e5), &no_lp, &inv(), 10.0, r, 2, false).is_ok());
    }

    #[test]
    fn with_take_lp_the_lp_level_is_taken_when_it_is_the_best_price() {
        let r = 0.0016;
        let lp_only = BookTop { lp_bid: Some(level(0.001598, 30_000.0, 3)), lp_ask: Some(level(0.001602, 20_000.0, 2)), ..Default::default() };
        let take = |side, book: &BookTop, take_lp| size_take(&want(side, 1e5), book, &inv(), 10.0, r, 2, take_lp);
        // Off: only agent levels count
        assert_eq!(take(OrderType::Bid, &lp_only, false), Err(SkipWhy::NoLevel));
        // On: the LP's best offer, one of its two orders at most; the bid, a third of it
        let t = take(OrderType::Bid, &lp_only, true).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string(), t.tif), ("0.001602", "10000".to_string(), TakeTif::Ioc));
        let t = take(OrderType::Offer, &lp_only, true).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string()), ("0.001598", "10000".to_string()));
        // The strategy's own slippage limit still holds: 5 bps allows up to 0.0016008
        let tight = Want { max_deviation_percent: 0.05, ..want(OrderType::Bid, 1e5) };
        assert_eq!(size_take(&tight, &lp_only, &inv(), 10.0, r, 2, true), Err(SkipWhy::Slippage));
        // An agent offer at or under the LP's price comes first; a worse one gives way to the LP
        for (agent, price, qty) in [(0.001602, "0.001602", "8000"), (0.001601, "0.001601", "8000"), (0.001603, "0.001602", "10000")] {
            let book = BookTop { agent_ask: Some(level(agent, 8000.0, 1)), ..lp_only.clone() };
            let t = take(OrderType::Bid, &book, true).unwrap();
            assert_eq!((t.price.as_str(), t.quantity.to_string()), (price, qty.to_string()), "{agent}");
            let off = take(OrderType::Bid, &book, false);
            assert_eq!(off.is_ok(), agent < 0.001602, "{agent}: {off:?}");
        }
        // Never under the floor, and never past the funds
        let small = BookTop { lp_ask: Some(level(0.001602, 5000.0, 1)), ..lp_only.clone() };
        assert_eq!(take(OrderType::Bid, &small, true), Err(SkipWhy::TooSmall));
        let poor = Inventory { quote_avail: 5.0, ..inv() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &lp_only, &poor, 10.0, r, 2, true), Err(SkipWhy::TooSmall));
    }

    #[test]
    fn a_resting_take_one_tick_inside_the_lp_is_exact_at_every_scale() {
        use super::super::maker::tests::{lp_level, lp_strings, scales};
        for (p, tick) in scales() {
            let (lp_bid, lp_ask, t) = lp_strings(p, tick);
            let book = BookTop { lp_bid: Some(lp_level(lp_bid)), lp_ask: Some(lp_level(lp_ask)), ..Default::default() };
            let rich = Inventory { base: 1e9, quote: 1e12, base_avail: 1e9, quote_avail: 1e12, usable: true };
            let gtd = |side| Want { tif: TakeTif::Gtd { ttl_secs: 45 }, ..want(side, 100.0 / p) };
            let exp = lot_exp(p);
            let price = |side| {
                let t = price_rest(&gtd(side), &book, &rich, 10.0, p, tick, exp).unwrap();
                Decimal::from_str_exact(&t.price).unwrap()
            };
            assert_eq!((price(OrderType::Bid), price(OrderType::Offer)), (lp_bid + t, lp_ask - t), "{p} {tick}");
        }
    }

    #[test]
    fn whole_steps_absorb_float_noise_only() {
        assert_eq!((whole_steps(2.4, false), whole_steps(2.4, true)), (2.0, 3.0));
        assert_eq!((whole_steps(2.0000000001, true), whole_steps(1.9999999999, false)), (2.0, 2.0));
        assert_eq!((whole_steps(2.00001, true), whole_steps(1.99999, false)), (3.0, 1.0));
        // At 1.66e13 steps a float resolves about 0.002 of one
        let x = 16_580_941_226_000.0 - 0.002;
        assert_eq!((whole_steps(x, false), whole_steps(x + 0.004, true)), (16_580_941_226_000.0, 16_580_941_226_000.0));
        assert_eq!(whole_steps(16_580_941_225_999.5, false), 16_580_941_225_999.0);
        assert!(whole_steps(f64::NAN, false).is_nan());
    }

    #[test]
    fn a_resting_take_sits_one_tick_inside_and_clear_of_the_reference() {
        let r = 0.0016;
        let tick = 1e-7;
        let book = BookTop { agent_bid: None, agent_ask: None, lp_bid: Some(level(0.00159, 1e4, 1)), lp_ask: Some(level(0.00161, 1e4, 1)) };
        let gtd = |side| Want { tif: TakeTif::Gtd { ttl_secs: 45 }, ..want(side, 20_000.0) };
        let t = price_rest(&gtd(OrderType::Bid), &book, &inv(), 10.0, r, tick, 2).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string()), ("0.0015901", "20000".to_string()));
        let t = price_rest(&gtd(OrderType::Offer), &book, &inv(), 10.0, r, tick, 2).unwrap();
        assert_eq!(t.price.as_str(), "0.0016099");
        // A tight book: capped 1 bp from the reference
        let tight = BookTop { lp_bid: Some(level(0.0015999, 1e4, 1)), lp_ask: Some(level(0.0016001, 1e4, 1)), ..book };
        let t = price_rest(&gtd(OrderType::Bid), &tight, &inv(), 10.0, r, tick, 2).unwrap();
        assert!(t.price_f <= r * (1.0 - REST_REF_GAP) + 1e-12, "{}", t.price);
        let t = price_rest(&gtd(OrderType::Offer), &tight, &inv(), 10.0, r, tick, 2).unwrap();
        assert!(t.price_f >= r * (1.0 + REST_REF_GAP) - 1e-12, "{}", t.price);
    }
}
