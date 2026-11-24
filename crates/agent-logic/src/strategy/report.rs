//! The per-market report line.

use super::{Start, Stats};
use crate::num::Px;

pub(crate) struct ReportLine<'a> {
    pub market_id: &'a str,
    pub kind: &'a str,
    pub base: f64,
    pub quote: f64,
    pub price: f64,
    pub start: Option<Start>,
    pub stats: Stats,
    /// Whether the market takes; the grid only rests
    pub takes: bool,
    pub status: &'a str,
}

/// One line: inventory, value at the reference, change since the start and against holding.
pub(crate) fn format_report(r: &ReportLine<'_>) -> String {
    let value = r.base * r.price + r.quote;
    let mut out = format!("Strategy {} [{}]: base {:.2}", r.market_id, r.kind, r.base);
    if let Some(s) = r.start {
        out.push_str(&format!(" ({:+.2})", r.base - s.base));
    }
    out.push_str(&format!(" quote {:.2}", r.quote));
    if let Some(s) = r.start {
        out.push_str(&format!(" ({:+.2})", r.quote - s.quote));
    }
    out.push_str(&format!(" value {:.2} @ {}", value, Px(r.price, 6)));
    if let Some(s) = r.start {
        let start_value = s.base * s.price + s.quote;
        let hold_value = s.base * r.price + s.quote;
        let change = value - start_value;
        let pct = if start_value > 0.0 { change / start_value * 100.0 } else { 0.0 };
        out.push_str(&format!(
            " (start {:.2}, {:+.2} {:+.2}%, vs hold {:+.2})",
            start_value,
            change,
            pct,
            value - hold_value
        ));
    }
    if r.takes {
        out.push_str(&format!(
            " takes {} ({} filled, {} empty)",
            r.stats.takes, r.stats.takes_filled, r.stats.takes_empty
        ));
    }
    out.push_str(&format!(" fills {} (buy {:.2} / sell {:.2})", r.stats.fills, r.stats.bought, r.stats.sold));
    if !r.status.is_empty() {
        out.push_str(" | ");
        out.push_str(r.status);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_line_reads_as_documented() {
        let line = format_report(&ReportLine {
            market_id: "HTEST-USDC",
            kind: "rebalancer",
            base: 1520.0,
            quote: 1480.2,
            price: 0.99,
            start: Some(Start { base: 1500.0, quote: 1500.0, price: 1.0 }),
            stats: Stats { takes: 3, takes_filled: 2, takes_empty: 1, fills: 5, bought: 30.0, sold: 10.0 },
            takes: true,
            status: "in band",
        });
        assert_eq!(
            line,
            "Strategy HTEST-USDC [rebalancer]: base 1520.00 (+20.00) quote 1480.20 (-19.80) value 2985.00 @ 0.990000 \
             (start 3000.00, -15.00 -0.50%, vs hold +0.00) takes 3 (2 filled, 1 empty) fills 5 (buy 30.00 / sell 10.00) | in band"
        );
        let bare = format_report(&ReportLine {
            market_id: "A-B",
            kind: "noise_taker",
            base: 1.0,
            quote: 2.0,
            price: 3.0,
            start: None,
            stats: Stats::default(),
            takes: true,
            status: "",
        });
        assert_eq!(
            bare,
            "Strategy A-B [noise_taker]: base 1.00 quote 2.00 value 5.00 @ 3.000000 takes 0 (0 filled, 0 empty) fills 0 (buy 0.00 / sell 0.00)"
        );
        // A reference like 0.001595 used to print as 0.0016
        let small = format_report(&ReportLine { base: 1_000_000.0, price: 0.001595, ..line_args("HTEST-USDC") });
        assert!(small.contains(" value 1597.00 @ 0.001595 takes"), "{small}");
    }

    fn line_args(market_id: &str) -> ReportLine<'_> {
        ReportLine {
            market_id,
            kind: "arbitrage",
            base: 0.0,
            quote: 2.0,
            price: 1.0,
            start: None,
            stats: Stats::default(),
            takes: true,
            status: "",
        }
    }

    #[test]
    fn the_grid_line_has_no_takes() {
        let line = format_report(&ReportLine {
            market_id: "CC-USDC",
            kind: "grid",
            base: 100.0,
            quote: 51.0,
            price: 0.15,
            start: Some(Start { base: 110.0, quote: 48.5, price: 0.16 }),
            stats: Stats { fills: 2, bought: 0.0, sold: 10.0, ..Stats::default() },
            takes: false,
            status: "rungs 3 bid / 3 offer",
        });
        // Start 66.10, now 66.00; holding would be worth 65.00
        assert_eq!(
            line,
            "Strategy CC-USDC [grid]: base 100.00 (-10.00) quote 51.00 (+2.50) value 66.00 @ 0.150000 \
             (start 66.10, -0.10 -0.15%, vs hold +1.00) fills 2 (buy 0.00 / sell 10.00) | rungs 3 bid / 3 offer"
        );
    }
}
