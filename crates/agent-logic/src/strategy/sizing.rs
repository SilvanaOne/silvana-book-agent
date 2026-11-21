//! Lot rounding, prices on the tick grid, and the size and price of a take.

use orderbook_proto::orderbook::OrderType;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use super::book::BookTop;
use super::{clampf, Inventory, TakeTif, Want};

/// Strategy orders are worth at least this much (quote units).
pub(crate) const MIN_ORDER_USD: f64 = 12.0;
/// The server's floor when it reports none.
pub(crate) const SERVER_FLOOR_USD: f64 = 10.0;
/// With no LP quote, buy only below ref + 4 bps and sell only above ref - 4 bps.
pub(crate) const NO_LP_BAND: f64 = 0.0004;
/// A resting take stays at least 1 bp from the reference.
pub(crate) const REST_REF_GAP: f64 = 0.0001;

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
    let units = if up { (units - 1e-9).ceil() } else { (units + 1e-9).floor() };
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

/// Base quantity worth at least `max(usd, MIN_ORDER_USD, floor)` at `price`, rounded up.
pub(crate) fn order_qty(usd: f64, price: f64, floor_value: f64, exp: i32) -> Option<Decimal> {
    if !(price.is_finite() && price > 0.0) {
        return None;
    }
    lots(usd.max(MIN_ORDER_USD).max(floor_value) / price, exp, true)
}

/// `n` ticks as an exact price string.
pub(crate) fn tick_price(n: f64, tick: f64) -> Option<String> {
    if !(1.0..1e15).contains(&n) {
        return None;
    }
    let tick = Decimal::from_f64_retain(tick)?.round_dp(10);
    if tick <= Decimal::ZERO {
        return None;
    }
    Some(Decimal::from(n as i64).checked_mul(tick)?.round_dp(10).normalize().to_string())
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
pub(crate) fn size_take(
    want: &Want,
    book: &BookTop,
    inv: &Inventory,
    floor_value: f64,
    reference: f64,
    exp: i32,
) -> Result<TakeOrder, SkipWhy> {
    let dev = want.max_deviation_percent / 100.0;
    let (level, funds) = if want.side == OrderType::Bid {
        let level = book.agent_ask.as_ref().ok_or(SkipWhy::NoLevel)?;
        let beats_lp = match book.lp_ask {
            Some(lp) => level.price < lp,
            None => level.price < reference * (1.0 + NO_LP_BAND),
        };
        if !beats_lp {
            return Err(SkipWhy::NotBetterThanLp);
        }
        if level.price > reference * (1.0 + dev) {
            return Err(SkipWhy::Slippage);
        }
        (level, inv.quote_avail / level.price)
    } else {
        let level = book.agent_bid.as_ref().ok_or(SkipWhy::NoLevel)?;
        let beats_lp = match book.lp_bid {
            Some(lp) => level.price > lp,
            None => level.price > reference * (1.0 - NO_LP_BAND),
        };
        if !beats_lp {
            return Err(SkipWhy::NotBetterThanLp);
        }
        if level.price < reference * (1.0 - dev) {
            return Err(SkipWhy::Slippage);
        }
        (level, inv.base_avail)
    };
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
    let (n, funds) = if want.side == OrderType::Bid {
        let mut p = book.best_bid().map_or(reference * (1.0 - REST_REF_GAP), |b| b + tick);
        p = p.min(reference * (1.0 - REST_REF_GAP));
        if let Some(ask) = book.best_ask() {
            p = p.min(ask - tick);
        }
        let n = (p / tick + 1e-9).floor();
        (n, inv.quote_avail / (n * tick))
    } else {
        let mut p = book.best_ask().map_or(reference * (1.0 + REST_REF_GAP), |a| a - tick);
        p = p.max(reference * (1.0 + REST_REF_GAP));
        if let Some(bid) = book.best_bid() {
            p = p.max(bid + tick);
        }
        ((p / tick - 1e-9).ceil(), inv.base_avail)
    };
    let price = tick_price(n, tick).ok_or(SkipWhy::NoLevel)?;
    let price_f = n * tick;
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
        assert_eq!(order_qty(5.0, 0.0016, 10.1, 2).map(|d| d.to_string()), Some("7500".into()));
        assert_eq!(order_qty(5.0, 0.0016, 12.5, 2).map(|d| d.to_string()), Some("7900".into()));
        assert_eq!(order_qty(40.0, 0.0016, 0.0, 2).map(|d| d.to_string()), Some("25000".into()));
        assert_eq!(tick_price(16_123.0, 1e-7).as_deref(), Some("0.0016123"));
        assert_eq!(tick_price(0.0, 1e-7), None);
    }

    #[test]
    fn size_take_truth_table() {
        let r = 0.0016;
        let book = BookTop {
            agent_bid: Some(level(0.001599, 30_000.0, 3)),
            agent_ask: Some(level(0.001601, 20_000.0, 1)),
            lp_bid: Some(0.001598),
            lp_ask: Some(0.001602),
        };
        // Buy at exactly the agent offer, capped by its one order
        let t = size_take(&want(OrderType::Bid, 50_000.0), &book, &inv(), 10.0, r, 2).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string()), ("0.001601", "20000".to_string()));
        // Sell: three orders share the level, so a third of it at most
        let t = size_take(&want(OrderType::Offer, 50_000.0), &book, &inv(), 10.0, r, 2).unwrap();
        assert_eq!((t.price_f, t.quantity.to_string()), (0.001599, "10000".to_string()));
        // Not better than the LP, beyond slippage, too small, or no level
        let lp_better = BookTop { lp_ask: Some(0.001601), ..book.clone() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &lp_better, &inv(), 10.0, r, 2), Err(SkipWhy::NotBetterThanLp));
        let tight = Want { max_deviation_percent: 0.05, ..want(OrderType::Bid, 1e5) };
        assert_eq!(size_take(&tight, &book, &inv(), 10.0, r, 2), Err(SkipWhy::Slippage));
        assert_eq!(size_take(&want(OrderType::Bid, 5000.0), &book, &inv(), 10.0, r, 2), Err(SkipWhy::TooSmall));
        let poor = Inventory { quote_avail: 5.0, ..inv() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &book, &poor, 10.0, r, 2), Err(SkipWhy::TooSmall));
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &BookTop::default(), &inv(), 10.0, r, 2), Err(SkipWhy::NoLevel));
        // No LP quote: only within 4 bps of the reference
        let no_lp = BookTop { lp_ask: None, agent_ask: Some(level(0.0016007, 20_000.0, 1)), ..book.clone() };
        assert_eq!(size_take(&want(OrderType::Bid, 1e5), &no_lp, &inv(), 10.0, r, 2), Err(SkipWhy::NotBetterThanLp));
        let no_lp = BookTop { lp_ask: None, agent_ask: Some(level(0.0016005, 20_000.0, 1)), ..book };
        assert!(size_take(&want(OrderType::Bid, 1e5), &no_lp, &inv(), 10.0, r, 2).is_ok());
    }

    #[test]
    fn a_resting_take_sits_one_tick_inside_and_clear_of_the_reference() {
        let r = 0.0016;
        let tick = 1e-7;
        let book = BookTop { agent_bid: None, agent_ask: None, lp_bid: Some(0.0015900), lp_ask: Some(0.0016100) };
        let gtd = |side| Want { tif: TakeTif::Gtd { ttl_secs: 45 }, ..want(side, 20_000.0) };
        let t = price_rest(&gtd(OrderType::Bid), &book, &inv(), 10.0, r, tick, 2).unwrap();
        assert_eq!((t.price.as_str(), t.quantity.to_string()), ("0.0015901", "20000".to_string()));
        let t = price_rest(&gtd(OrderType::Offer), &book, &inv(), 10.0, r, tick, 2).unwrap();
        assert_eq!(t.price.as_str(), "0.0016099");
        // A tight book: capped 1 bp from the reference
        let tight = BookTop { lp_bid: Some(0.0015999), lp_ask: Some(0.0016001), ..book };
        let t = price_rest(&gtd(OrderType::Bid), &tight, &inv(), 10.0, r, tick, 2).unwrap();
        assert!(t.price_f <= r * (1.0 - REST_REF_GAP) + 1e-12, "{}", t.price);
        let t = price_rest(&gtd(OrderType::Offer), &tight, &inv(), 10.0, r, tick, 2).unwrap();
        assert!(t.price_f >= r * (1.0 + REST_REF_GAP) - 1e-12, "{}", t.price);
    }
}
