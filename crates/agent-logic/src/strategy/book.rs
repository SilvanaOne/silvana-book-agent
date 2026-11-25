//! The agents' book: full depth minus the LP's depth minus our own orders.

use orderbook_proto::orderbook::{Order, OrderType, OrderbookDepth, OrderbookLevel};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

/// Best level on one side; `price_str` is the server's exact price.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Level {
    pub price: f64,
    pub price_str: String,
    pub qty: f64,
    pub count: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct BookTop {
    pub agent_bid: Option<Level>,
    pub agent_ask: Option<Level>,
    /// The LP's best level: its whole depth and order count at that price
    pub lp_bid: Option<Level>,
    pub lp_ask: Option<Level>,
}

impl BookTop {
    /// Best bid of anyone but us.
    pub(crate) fn best_bid(&self) -> Option<f64> {
        max_opt(self.agent_bid.as_ref().map(|l| l.price), self.lp_bid.as_ref().map(|l| l.price))
    }

    /// Best offer of anyone but us.
    pub(crate) fn best_ask(&self) -> Option<f64> {
        min_opt(self.agent_ask.as_ref().map(|l| l.price), self.lp_ask.as_ref().map(|l| l.price))
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn dec(s: &str) -> Option<Decimal> {
    Decimal::from_str_exact(s.trim()).ok()
}

/// What is left of an order to match.
fn remaining(o: &Order) -> Decimal {
    if let Some(r) = dec(&o.remaining_quantity) {
        return r;
    }
    let q = dec(&o.quantity).unwrap_or(Decimal::ZERO);
    let used = dec(&o.filled_quantity)
        .unwrap_or(Decimal::ZERO)
        .saturating_add(dec(&o.pending_quantity).unwrap_or(Decimal::ZERO));
    q.saturating_sub(used).max(Decimal::ZERO)
}

/// Prices compare as decimals, so "0.5" matches "0.5000000000".
pub(crate) fn book_top(full: &OrderbookDepth, lp: &OrderbookDepth, own: &[Order]) -> BookTop {
    BookTop {
        agent_bid: best_agent(&full.bids, &lp.bids, own, OrderType::Bid),
        agent_ask: best_agent(&full.offers, &lp.offers, own, OrderType::Offer),
        lp_bid: best_lp(&lp.bids, OrderType::Bid),
        lp_ask: best_lp(&lp.offers, OrderType::Offer),
    }
}

fn better(side: OrderType, a: Decimal, b: Decimal) -> bool {
    if side == OrderType::Bid { a > b } else { a < b }
}

/// The LP's best price; levels listed twice at one price are added up.
fn best_lp(levels: &[OrderbookLevel], side: OrderType) -> Option<Level> {
    let mut best: Option<(Decimal, &str)> = None;
    for l in levels {
        let (Some(p), Some(q)) = (dec(&l.price), dec(&l.total_quantity)) else { continue };
        if q > Decimal::ZERO && best.is_none_or(|(b, _)| better(side, p, b)) {
            best = Some((p, l.price.as_str()));
        }
    }
    let (price, price_str) = best?;
    let (qty, count) = levels
        .iter()
        .filter(|l| dec(&l.price) == Some(price))
        .fold((Decimal::ZERO, 0u32), |(q, c), l| {
            (q.saturating_add(dec(&l.total_quantity).unwrap_or(Decimal::ZERO)), c.saturating_add(l.order_count))
        });
    Some(Level { price: price.to_f64()?, price_str: price_str.to_string(), qty: qty.to_f64()?, count: count.max(1) })
}

fn best_agent(full: &[OrderbookLevel], lp: &[OrderbookLevel], own: &[Order], side: OrderType) -> Option<Level> {
    let mut best: Option<(Decimal, Level)> = None;
    for level in full {
        let (Some(price), Some(total)) = (dec(&level.price), dec(&level.total_quantity)) else { continue };
        if best.as_ref().is_some_and(|(b, _)| !better(side, price, *b)) {
            continue;
        }
        let (lp_qty, lp_count) = lp
            .iter()
            .filter(|l| dec(&l.price) == Some(price))
            .fold((Decimal::ZERO, 0u32), |(q, c), l| {
                (q.saturating_add(dec(&l.total_quantity).unwrap_or(Decimal::ZERO)), c.saturating_add(l.order_count))
            });
        let (own_qty, own_count) = own
            .iter()
            .filter(|o| o.order_type == side as i32 && dec(&o.price) == Some(price))
            .fold((Decimal::ZERO, 0u32), |(q, c), o| (q.saturating_add(remaining(o)), c.saturating_add(1)));
        let qty = total.saturating_sub(lp_qty).saturating_sub(own_qty);
        if qty <= Decimal::ZERO {
            continue;
        }
        let count = level.order_count.saturating_sub(lp_count).saturating_sub(own_count).max(1);
        let (Some(pf), Some(qf)) = (price.to_f64(), qty.to_f64()) else { continue };
        best = Some((price, Level { price: pf, price_str: level.price.clone(), qty: qf, count }));
    }
    best.map(|(_, l)| l)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn lvl(price: &str, qty: &str, count: u32) -> OrderbookLevel {
        OrderbookLevel { price: price.into(), total_quantity: qty.into(), order_count: count, first_order_time: None }
    }

    pub(crate) fn depth(bids: Vec<OrderbookLevel>, offers: Vec<OrderbookLevel>) -> OrderbookDepth {
        OrderbookDepth { bids, offers, ..Default::default() }
    }

    fn own(side: OrderType, price: &str, remaining: &str) -> Order {
        Order {
            order_type: side as i32,
            price: price.into(),
            remaining_quantity: remaining.into(),
            ..Default::default()
        }
    }

    #[test]
    fn book_top_truth_table() {
        let full = depth(
            vec![lvl("0.0016000000", "30000", 3), lvl("0.0015990", "10000", 1), lvl("0.00159", "5000", 1)],
            vec![lvl("0.0016100000", "20000", 2), lvl("0.00162", "10000", 1)],
        );
        let lp = depth(vec![lvl("0.0016", "10000", 1), lvl("0.00159", "5000", 1)], vec![lvl("0.00162", "10000", 1)]);

        // Our own bid at the top level is not the agents' liquidity
        let top = book_top(&full, &lp, &[own(OrderType::Bid, "0.0016", "20000")]);
        assert_eq!(top.agent_bid.as_ref().map(|l| (l.price_str.as_str(), l.qty, l.count)), Some(("0.0015990", 10000.0, 1)));
        assert_eq!(top.agent_ask.as_ref().map(|l| (l.price_str.as_str(), l.qty, l.count)), Some(("0.0016100000", 20000.0, 2)));
        let lp_top = |l: &Option<Level>| l.as_ref().map(|l| (l.price_str.clone(), l.qty, l.count));
        assert_eq!((lp_top(&top.lp_bid), lp_top(&top.lp_ask)), (Some(("0.0016".into(), 10000.0, 1)), Some(("0.00162".into(), 10000.0, 1))));

        // Without our order, another agent rests 20000 at the top beside the LP
        let top = book_top(&full, &lp, &[]);
        assert_eq!(top.agent_bid.map(|l| (l.price, l.qty, l.count)), Some((0.0016, 20000.0, 2)));

        // A level that is all LP, or all ours, is skipped; an empty LP book has no prices
        let top = book_top(&full, &full, &[]);
        assert_eq!((top.agent_bid, top.agent_ask), (None, None));
        let top = book_top(&full, &depth(Vec::new(), Vec::new()), &[own(OrderType::Offer, "0.00161", "20000")]);
        assert_eq!(top.agent_ask.as_ref().map(|l| l.price), Some(0.00162));
        assert_eq!((top.lp_bid.as_ref(), top.lp_ask.as_ref()), (None, None));
        assert_eq!((top.best_bid(), top.best_ask()), (Some(0.0016), Some(0.00162)));

        // The LP's depth at its best price, two orders listed apart
        let lp = depth(vec![lvl("0.0016", "4000", 1), lvl("0.0016000", "6000", 2), lvl("0.0015", "1", 1)], Vec::new());
        let top = book_top(&full, &lp, &[]);
        assert_eq!(top.lp_bid.map(|l| (l.price, l.qty, l.count)), Some((0.0016, 10000.0, 3)));

        // Junk levels are ignored; an unsorted book still gives the best level
        let full = depth(vec![lvl("x", "1", 1), lvl("0.9", "1", 1), lvl("1.1", "1", 1)], Vec::new());
        assert_eq!(book_top(&full, &OrderbookDepth::default(), &[]).agent_bid.map(|l| l.price), Some(1.1));
    }
}
