//! Price rounding helpers.
//!
//! Silvana orderbook rejects orders whose price is not a multiple of the
//! market's `tick_size`. Every write-side agent must round its computed
//! order price before submitting.
//!
//! Historical note: individual agents originally called `.round_dp(8)`
//! inline. That works for markets whose tick_size is 1e-8 (CC-USDC on
//! devnet), and is a superset for coarser ticks (1e-7, 1e-6, …). It is
//! NOT safe for irregular ticks like 5e-8 or 2.5e-9. This module offers
//! both the historical helper and a tick-aware round for future use.

use rust_decimal::Decimal;
use std::str::FromStr;

/// Historical rounding — rounds to 8 decimal places (nearest, banker's).
/// Safe for markets with `tick_size` = 1e-8 or any coarser power-of-10.
///
/// Use `round_to_tick` when the market's exact tick_size is known.
pub fn round_to_8_decimals(price: Decimal) -> Decimal {
    price.round_dp(8)
}

/// Round `price` to the nearest multiple of `tick_size`.
///
/// Returns the original price unchanged when `tick_size <= 0` (fallback for
/// missing market info).
pub fn round_to_tick(price: Decimal, tick_size: Decimal) -> Decimal {
    if tick_size <= Decimal::ZERO {
        return price;
    }
    let multiplier = price / tick_size;
    multiplier.round() * tick_size
}

/// Default tick_size when the market can't be resolved from RPC.
/// Matches CC-USDC on devnet.
pub fn default_tick_size() -> Decimal {
    Decimal::from_str("0.00000001").expect("literal decimal parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_to_tick_1e8() {
        let tick = Decimal::from_str("0.00000001").unwrap();
        let price = Decimal::from_str("0.14724765123456789").unwrap();
        let rounded = round_to_tick(price, tick);
        assert_eq!(rounded, Decimal::from_str("0.14724765").unwrap());
    }

    #[test]
    fn round_to_tick_5e8() {
        // Odd tick: 5e-8. Prices must be a multiple of 0.00000005.
        let tick = Decimal::from_str("0.00000005").unwrap();
        let price = Decimal::from_str("0.14724766").unwrap(); // 8-decimal
        let rounded = round_to_tick(price, tick);
        // 0.14724766 / 5e-8 = 2944953.2 → round(2944953) → 2944953 × 5e-8 = 0.14724765
        assert_eq!(rounded, Decimal::from_str("0.14724765").unwrap());
    }

    #[test]
    fn round_to_tick_zero_tick_passthrough() {
        let price = Decimal::from_str("1.23").unwrap();
        assert_eq!(round_to_tick(price, Decimal::ZERO), price);
    }

    #[test]
    fn round_to_8_decimals_basic() {
        let price = Decimal::from_str("0.14724765500000002").unwrap();
        assert_eq!(round_to_8_decimals(price), Decimal::from_str("0.14724766").unwrap());
    }
}
