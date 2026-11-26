//! Config types built the way code outside this crate builds them.

use agent_logic::config::{MarketConfig, PriceLevel};

#[test]
fn a_grid_market_builds_from_struct_literals() {
    let level = |delta_percent: f64| PriceLevel { delta_percent, quantity: "10".to_string() };
    let market = MarketConfig {
        market_id: "AAA-USD".to_string(),
        enabled: true,
        base_order_size: None,
        bid_levels: vec![level(-0.5)],
        offer_levels: vec![level(0.5)],
        price_change_threshold_percent: 1.0,
        rfq: None,
    };
    assert_eq!(market.bid_levels, [PriceLevel { delta_percent: -0.5, quantity: "10".to_string() }]);
    assert_eq!(market.offer_levels[0].delta_percent, 0.5);
}
