//! Per-market `[markets.strategy]` tables, read in a second pass over agent.toml.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::config::{BaseConfig, MarketConfig};

/// Strategy for one market, tagged by `kind`; a market without a table runs the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StrategyConfig {
    Grid(GridParams),
    MarketMaker(MarketMakerConfig),
    Rebalancer(RebalancerConfig),
    Momentum(MomentumConfig),
    MeanReversion(MeanReversionConfig),
    NoiseTaker(NoiseTakerConfig),
    Twap(TwapConfig),
    Arbitrage(ArbitrageConfig),
}

/// The grid takes its rungs from `bid_levels` / `offer_levels`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridParams {}

/// Taker strategies send IOC orders; makers rest one order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyMode {
    #[default]
    Taker,
    Maker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSide {
    Buy,
    Sell,
}

/// Limits every non-grid kind shares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommonParams {
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    /// Seconds a reference reading stays usable; unset means 90
    pub ref_max_age_secs: Option<u64>,
    /// Takers may take the liquidity provider's best level
    pub take_lp: bool,
}

const DEFAULT_MAX_SETTLEMENTS_PER_HOUR: u32 = 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarketMakerConfig {
    pub levels: u32,
    pub half_spread_bps: f64,
    pub level_step_bps: f64,
    pub level_usd: f64,
    pub size_growth: f64,
    pub max_inventory_usd: f64,
    pub skew_bps: f64,
    pub lp_inside: bool,
    pub lp_improve_ticks: u32,
    pub min_edge_bps: f64,
    pub pull_jump_bps: f64,
    pub max_inflight: u32,
    pub max_fills_per_hour: u32,
    pub throttle_spread_bps: f64,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self {
            levels: 2,
            half_spread_bps: 10.0,
            level_step_bps: 10.0,
            level_usd: 40.0,
            size_growth: 1.5,
            max_inventory_usd: 300.0,
            skew_bps: 15.0,
            lp_inside: true,
            lp_improve_ticks: 1,
            min_edge_bps: 3.0,
            pull_jump_bps: 30.0,
            max_inflight: 8,
            max_fills_per_hour: 60,
            throttle_spread_bps: 40.0,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RebalancerConfig {
    /// Required: base share of portfolio value to hold.
    pub target_share: Option<f64>,
    pub band_enter: f64,
    pub band_exit: f64,
    pub mode: StrategyMode,
    pub clip_usd: f64,
    pub cooldown_secs: u64,
    pub maker_offset_bps: f64,
    pub max_slip_bps: f64,
    pub max_usd_per_hour: f64,
    pub flip_cooldown_secs: u64,
    /// Default 1 for a taker, 2 for a maker.
    pub max_inflight: Option<u32>,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for RebalancerConfig {
    fn default() -> Self {
        Self {
            target_share: None,
            band_enter: 0.02,
            band_exit: 0.005,
            mode: StrategyMode::Taker,
            clip_usd: 20.0,
            cooldown_secs: 150,
            maker_offset_bps: 3.0,
            max_slip_bps: 15.0,
            max_usd_per_hour: 600.0,
            flip_cooldown_secs: 900,
            max_inflight: None,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

impl RebalancerConfig {
    pub fn inflight_cap(&self) -> u32 {
        self.max_inflight.unwrap_or(match self.mode {
            StrategyMode::Taker => 1,
            StrategyMode::Maker => 2,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MomentumConfig {
    pub fast_secs: u64,
    pub slow_secs: u64,
    pub enter_bps: f64,
    pub exit_bps: f64,
    pub max_position_usd: f64,
    pub clip_usd: f64,
    pub clip_interval_secs: u64,
    pub min_hold_secs: u64,
    pub max_flips_per_hour: u32,
    pub warmup_secs: u64,
    pub max_slip_bps: f64,
    pub max_inflight: u32,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            fast_secs: 180,
            slow_secs: 900,
            enter_bps: 12.0,
            exit_bps: 4.0,
            max_position_usd: 200.0,
            clip_usd: 30.0,
            clip_interval_secs: 20,
            min_hold_secs: 600,
            max_flips_per_hour: 3,
            warmup_secs: 600,
            max_slip_bps: 15.0,
            max_inflight: 2,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MeanReversionConfig {
    pub ema_secs: u64,
    pub entry_bps: f64,
    pub full_bps: f64,
    pub stop_bps: f64,
    pub max_position_usd: f64,
    pub mode: StrategyMode,
    pub maker_offset_bps: f64,
    pub clip_usd: f64,
    pub warmup_secs: u64,
    pub pause_after_stop_secs: u64,
    pub max_slip_bps: f64,
    pub max_inflight: u32,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for MeanReversionConfig {
    fn default() -> Self {
        Self {
            ema_secs: 1200,
            entry_bps: 15.0,
            full_bps: 60.0,
            stop_bps: 150.0,
            max_position_usd: 200.0,
            mode: StrategyMode::Maker,
            maker_offset_bps: 4.0,
            clip_usd: 30.0,
            warmup_secs: 600,
            pause_after_stop_secs: 900,
            max_slip_bps: 15.0,
            max_inflight: 2,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoiseTakerConfig {
    pub mean_interval_secs: f64,
    pub clip_usd_min: f64,
    pub clip_usd_max: f64,
    pub inventory_bias: f64,
    pub max_position_usd: f64,
    pub max_slip_bps: f64,
    pub rest_probability: f64,
    pub rest_ttl_secs: u64,
    /// Fixed seed for a replayable run; unset seeds from the start time.
    pub seed: Option<u64>,
    pub max_inflight: u32,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for NoiseTakerConfig {
    fn default() -> Self {
        Self {
            mean_interval_secs: 60.0,
            clip_usd_min: 15.0,
            clip_usd_max: 40.0,
            inventory_bias: 0.4,
            max_position_usd: 250.0,
            max_slip_bps: 20.0,
            rest_probability: 0.2,
            rest_ttl_secs: 45,
            seed: None,
            max_inflight: 2,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TwapConfig {
    /// Required.
    pub side: Option<TradeSide>,
    /// Required: base quantity, as a decimal string.
    pub total_quantity: Option<String>,
    /// RFC 3339; set it so a restart resumes the same schedule.
    pub start_at: Option<String>,
    pub start_delay_secs: u64,
    pub duration_secs: u64,
    pub slice_secs: u64,
    pub passive_secs: u64,
    pub max_slip_bps: f64,
    pub max_catchup: f64,
    pub time_jitter: f64,
    pub size_jitter: f64,
    pub max_inflight: u32,
    pub seed: Option<u64>,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
    pub ref_max_age_secs: Option<u64>,
}

impl Default for TwapConfig {
    fn default() -> Self {
        Self {
            side: None,
            total_quantity: None,
            start_at: None,
            start_delay_secs: 300,
            duration_secs: 4800,
            slice_secs: 240,
            passive_secs: 60,
            max_slip_bps: 15.0,
            max_catchup: 2.0,
            time_jitter: 0.2,
            size_jitter: 0.15,
            max_inflight: 3,
            seed: None,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
            ref_max_age_secs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArbitrageConfig {
    pub min_edge_bps: f64,
    pub clip_usd_max: f64,
    pub max_position_usd: f64,
    pub max_inflight: u32,
    pub cooldown_secs: u64,
    pub ref_max_age_secs: u64,
    pub ref_stable_bps: f64,
    /// Also take the liquidity provider's best level when it is the best price
    pub take_lp: bool,
    pub max_settlements_per_hour: u32,
    pub ref_floor: Option<f64>,
    pub ref_cap: Option<f64>,
}

impl Default for ArbitrageConfig {
    fn default() -> Self {
        Self {
            min_edge_bps: 6.0,
            clip_usd_max: 100.0,
            max_position_usd: 150.0,
            max_inflight: 1,
            cooldown_secs: 10,
            // The served stamp is often up to a minute old; stability is the tighter guard
            ref_max_age_secs: 90,
            ref_stable_bps: 30.0,
            take_lp: false,
            max_settlements_per_hour: DEFAULT_MAX_SETTLEMENTS_PER_HOUR,
            ref_floor: None,
            ref_cap: None,
        }
    }
}

/// What a market without a strategy table runs.
pub(crate) static GRID: StrategyConfig = StrategyConfig::Grid(GridParams {});

macro_rules! common_of {
    ($c:expr) => {
        common_of!($c, $c.ref_max_age_secs, $c.take_lp)
    };
    ($c:expr, $age:expr, $take_lp:expr) => {
        CommonParams {
            max_settlements_per_hour: $c.max_settlements_per_hour,
            ref_floor: $c.ref_floor,
            ref_cap: $c.ref_cap,
            ref_max_age_secs: $age,
            take_lp: $take_lp,
        }
    };
}

impl StrategyConfig {
    /// The `kind` name as written in agent.toml.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Grid(_) => "grid",
            Self::MarketMaker(_) => "market_maker",
            Self::Rebalancer(_) => "rebalancer",
            Self::Momentum(_) => "momentum",
            Self::MeanReversion(_) => "mean_reversion",
            Self::NoiseTaker(_) => "noise_taker",
            Self::Twap(_) => "twap",
            Self::Arbitrage(_) => "arbitrage",
        }
    }

    pub fn is_grid(&self) -> bool {
        matches!(self, Self::Grid(_))
    }

    /// Shared limits; None for the grid.
    pub fn common(&self) -> Option<CommonParams> {
        match self {
            Self::Grid(_) => None,
            // A maker never takes
            Self::MarketMaker(c) => Some(common_of!(c, c.ref_max_age_secs, false)),
            Self::Rebalancer(c) => Some(common_of!(c)),
            Self::Momentum(c) => Some(common_of!(c)),
            Self::MeanReversion(c) => Some(common_of!(c)),
            Self::NoiseTaker(c) => Some(common_of!(c)),
            Self::Twap(c) => Some(common_of!(c)),
            Self::Arbitrage(c) => Some(common_of!(c, Some(c.ref_max_age_secs), c.take_lp)),
        }
    }
}

// ---- second TOML pass ----

#[derive(Deserialize)]
struct StrategyView {
    #[serde(default)]
    markets: Vec<StrategyEntry>,
}

#[derive(Deserialize)]
struct StrategyEntry {
    market_id: String,
    #[serde(default)]
    strategy: Option<StrategyConfig>,
}

/// Strategy tables by market id; other agent.toml keys are ignored here.
pub(crate) fn parse_strategies(toml_str: &str) -> Result<BTreeMap<String, StrategyConfig>> {
    let view: StrategyView = toml::from_str(toml_str).context("invalid [markets.strategy] table")?;
    let mut listed: HashMap<&str, usize> = HashMap::new();
    for entry in &view.markets {
        let n = listed.entry(entry.market_id.as_str()).or_insert(0);
        *n = n.saturating_add(1);
    }
    let mut out = BTreeMap::new();
    for entry in &view.markets {
        let Some(strategy) = &entry.strategy else { continue };
        if listed.get(entry.market_id.as_str()).is_some_and(|n| *n > 1) {
            bail!(
                "Market {}: listed more than once with a [markets.strategy] table",
                entry.market_id
            );
        }
        out.insert(entry.market_id.clone(), strategy.clone());
    }
    Ok(out)
}

// ---- validation ----

/// Error helpers in the `Market {id}: ...` style.
struct Check<'a> {
    market: &'a str,
    kind: &'static str,
}

impl Check<'_> {
    fn fail(&self, msg: impl Display) -> anyhow::Error {
        anyhow::anyhow!("Market {}: [markets.strategy] {} {}", self.market, self.kind, msg)
    }

    fn rule(&self, ok: bool, msg: impl Display) -> Result<()> {
        if ok { Ok(()) } else { Err(self.fail(msg)) }
    }

    fn range(&self, name: &str, v: f64, lo: f64, hi: f64) -> Result<()> {
        self.rule(v.is_finite() && v >= lo && v <= hi, format_args!("{name}={v} must be in {lo}..={hi}"))
    }

    fn int(&self, name: &str, v: u64, lo: u64, hi: u64) -> Result<()> {
        self.rule(v >= lo && v <= hi, format_args!("{name}={v} must be in {lo}..={hi}"))
    }

    fn common(&self, c: CommonParams) -> Result<()> {
        self.int("max_settlements_per_hour", c.max_settlements_per_hour.into(), 1, 10_000)?;
        for (name, v) in [("ref_floor", c.ref_floor), ("ref_cap", c.ref_cap)] {
            if let Some(v) = v {
                self.rule(v.is_finite() && v > 0.0, format_args!("{name}={v} must be a positive number"))?;
            }
        }
        if let (Some(floor), Some(cap)) = (c.ref_floor, c.ref_cap) {
            self.rule(floor < cap, format_args!("ref_floor={floor} must be below ref_cap={cap}"))?;
        }
        if let Some(age) = c.ref_max_age_secs {
            self.int("ref_max_age_secs", age, 1, 86_400)?;
        }
        Ok(())
    }
}

/// Positive base quantity, parsed exactly (no exponent forms).
pub(crate) fn parse_quantity(raw: &str) -> Option<Decimal> {
    Decimal::from_str_exact(raw.trim()).ok().filter(|d| *d > Decimal::ZERO)
}

/// Every strategy table against its market and the agent settings.
pub(crate) fn validate_strategies(config: &BaseConfig) -> Result<()> {
    for (market_id, strategy) in &config.strategies {
        let check = Check { market: market_id, kind: strategy.kind() };
        let Some(market) = config.markets.iter().find(|m| &m.market_id == market_id) else {
            return Err(check.fail("is set for a market that is not in [[markets]]"));
        };
        validate_one(&check, market, strategy, config)?;
    }
    Ok(())
}

fn validate_one(check: &Check<'_>, market: &MarketConfig, strategy: &StrategyConfig, config: &BaseConfig) -> Result<()> {
    if strategy.is_grid() {
        return Ok(());
    }
    // An older binary ignores the table and quotes nothing; with levels it would run a grid
    check.rule(
        market.bid_levels.is_empty() && market.offer_levels.is_empty(),
        "cannot be combined with bid_levels/offer_levels (use kind = \"grid\" for those)",
    )?;
    if config.rfq_v2_only {
        tracing::warn!(
            "Market {}: [markets.strategy] {} is ignored with rfq_v2_only (no orders are placed)",
            market.market_id,
            check.kind
        );
    } else {
        check.rule(
            config.liquidity_provider.is_none(),
            "cannot run on an agent with [liquidity_provider]",
        )?;
        // Amounts named `_usd` are converted to the quote at its USD rate
        let quote = market.market_id.split_once('-').map_or("", |(_, q)| q);
        check.rule(
            super::quote_usd(&market.market_id, config.cc_token_id.as_deref(), None) != super::QuoteUsd::Unpriced,
            format_args!("needs a quote with a USD rate (a USD stable or CC); '{quote}' has no USD rate"),
        )?;
    }
    if let Some(common) = strategy.common() {
        check.common(common)?;
    }
    match strategy {
        StrategyConfig::Grid(_) => Ok(()),
        StrategyConfig::MarketMaker(c) => validate_market_maker(check, c),
        StrategyConfig::Rebalancer(c) => validate_rebalancer(check, c),
        StrategyConfig::Momentum(c) => validate_momentum(check, c),
        StrategyConfig::MeanReversion(c) => validate_mean_reversion(check, c),
        StrategyConfig::NoiseTaker(c) => validate_noise(check, c),
        StrategyConfig::Twap(c) => {
            check.rule(
                !market.rfq.as_ref().is_some_and(|r| r.enabled),
                "cannot run on a market with [markets.rfq] enabled",
            )?;
            validate_twap(check, c)
        }
        StrategyConfig::Arbitrage(c) => validate_arbitrage(check, c),
    }
}

fn validate_market_maker(k: &Check<'_>, c: &MarketMakerConfig) -> Result<()> {
    k.int("levels", c.levels.into(), 1, 5)?;
    k.range("half_spread_bps", c.half_spread_bps, 3.0, 100.0)?;
    k.range("level_step_bps", c.level_step_bps, 2.0, 100.0)?;
    k.range("level_usd", c.level_usd, 12.0, 500.0)?;
    k.range("size_growth", c.size_growth, 1.0, 3.0)?;
    k.range("max_inventory_usd", c.max_inventory_usd, 50.0, 5000.0)?;
    k.range("skew_bps", c.skew_bps, 0.0, 50.0)?;
    // The full spread: the defaults (15 vs 10) need it
    k.rule(c.skew_bps < 2.0 * c.half_spread_bps, format_args!(
        "skew_bps={} must be below the spread, 2 x half_spread_bps={}", c.skew_bps, c.half_spread_bps
    ))?;
    k.int("lp_improve_ticks", c.lp_improve_ticks.into(), 0, 100)?;
    k.range("min_edge_bps", c.min_edge_bps, 0.0, 100.0)?;
    k.range("pull_jump_bps", c.pull_jump_bps, 1.0, 1000.0)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 100)?;
    k.int("max_fills_per_hour", c.max_fills_per_hour.into(), 1, 10_000)?;
    k.range("throttle_spread_bps", c.throttle_spread_bps, c.half_spread_bps, 1000.0)
}

fn validate_rebalancer(k: &Check<'_>, c: &RebalancerConfig) -> Result<()> {
    let Some(target) = c.target_share else {
        return Err(k.fail("target_share is required"));
    };
    k.range("target_share", target, 0.05, 0.95)?;
    k.range("band_enter", c.band_enter, 0.001, 0.5)?;
    k.range("band_exit", c.band_exit, 0.0, 0.5)?;
    k.rule(c.band_exit < c.band_enter, format_args!(
        "band_exit={} must be below band_enter={}", c.band_exit, c.band_enter
    ))?;
    k.range("clip_usd", c.clip_usd, 12.0, 200.0)?;
    k.int("cooldown_secs", c.cooldown_secs, 30, 900)?;
    k.range("maker_offset_bps", c.maker_offset_bps, 0.0, 30.0)?;
    k.range("max_slip_bps", c.max_slip_bps, 0.1, 500.0)?;
    k.range("max_usd_per_hour", c.max_usd_per_hour, 12.0, 1_000_000.0)?;
    k.int("flip_cooldown_secs", c.flip_cooldown_secs, 0, 86_400)?;
    k.rule(!(c.take_lp && c.mode == StrategyMode::Maker), TAKE_LP_NEEDS_TAKER)?;
    k.int("max_inflight", c.inflight_cap().into(), 1, 10)
}

/// A maker mode only rests orders, so it never takes.
const TAKE_LP_NEEDS_TAKER: &str = "take_lp applies only with mode = \"taker\"";

fn validate_momentum(k: &Check<'_>, c: &MomentumConfig) -> Result<()> {
    k.int("fast_secs", c.fast_secs, 30, 1800)?;
    k.int("slow_secs", c.slow_secs, 90, 86_400)?;
    k.rule(c.slow_secs >= c.fast_secs.saturating_mul(3), format_args!(
        "slow_secs={} must be at least 3 x fast_secs={}", c.slow_secs, c.fast_secs
    ))?;
    k.range("enter_bps", c.enter_bps, 3.0, 100.0)?;
    k.range("exit_bps", c.exit_bps, 0.0, 100.0)?;
    k.rule(c.exit_bps < c.enter_bps, format_args!(
        "exit_bps={} must be below enter_bps={}", c.exit_bps, c.enter_bps
    ))?;
    k.range("max_position_usd", c.max_position_usd, 12.0, 1_000_000.0)?;
    k.range("clip_usd", c.clip_usd, 12.0, 10_000.0)?;
    k.int("clip_interval_secs", c.clip_interval_secs, 10, 300)?;
    k.int("min_hold_secs", c.min_hold_secs, 0, 86_400)?;
    k.int("max_flips_per_hour", c.max_flips_per_hour.into(), 1, 100)?;
    k.int("warmup_secs", c.warmup_secs, 0, 86_400)?;
    k.range("max_slip_bps", c.max_slip_bps, 0.1, 500.0)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 10)
}

fn validate_mean_reversion(k: &Check<'_>, c: &MeanReversionConfig) -> Result<()> {
    k.int("ema_secs", c.ema_secs, 30, 86_400)?;
    k.range("entry_bps", c.entry_bps, 0.1, 1000.0)?;
    k.range("full_bps", c.full_bps, 0.1, 1000.0)?;
    k.range("stop_bps", c.stop_bps, 0.1, 5000.0)?;
    k.rule(c.entry_bps < c.full_bps && c.full_bps < c.stop_bps, format_args!(
        "needs entry_bps={} < full_bps={} < stop_bps={}", c.entry_bps, c.full_bps, c.stop_bps
    ))?;
    k.range("max_position_usd", c.max_position_usd, 12.0, 1_000_000.0)?;
    k.range("maker_offset_bps", c.maker_offset_bps, 0.0, 30.0)?;
    k.range("clip_usd", c.clip_usd, 12.0, 10_000.0)?;
    k.int("warmup_secs", c.warmup_secs, 0, 86_400)?;
    k.int("pause_after_stop_secs", c.pause_after_stop_secs, 0, 86_400)?;
    k.range("max_slip_bps", c.max_slip_bps, 0.1, 500.0)?;
    k.rule(!(c.take_lp && c.mode == StrategyMode::Maker), TAKE_LP_NEEDS_TAKER)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 10)
}

fn validate_noise(k: &Check<'_>, c: &NoiseTakerConfig) -> Result<()> {
    k.range("mean_interval_secs", c.mean_interval_secs, 5.0, 86_400.0)?;
    k.range("clip_usd_min", c.clip_usd_min, 12.0, 10_000.0)?;
    k.range("clip_usd_max", c.clip_usd_max, 12.0, 10_000.0)?;
    k.rule(c.clip_usd_min <= c.clip_usd_max, format_args!(
        "clip_usd_min={} must not exceed clip_usd_max={}", c.clip_usd_min, c.clip_usd_max
    ))?;
    k.range("inventory_bias", c.inventory_bias, 0.0, 1.0)?;
    k.range("max_position_usd", c.max_position_usd, 12.0, 1_000_000.0)?;
    k.range("max_slip_bps", c.max_slip_bps, 0.1, 500.0)?;
    k.range("rest_probability", c.rest_probability, 0.0, 1.0)?;
    k.int("rest_ttl_secs", c.rest_ttl_secs, 5, 3600)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 10)
}

fn validate_twap(k: &Check<'_>, c: &TwapConfig) -> Result<()> {
    k.rule(c.side.is_some(), "side (buy or sell) is required")?;
    let Some(total) = c.total_quantity.as_deref() else {
        return Err(k.fail("total_quantity is required"));
    };
    k.rule(parse_quantity(total).is_some(), format_args!(
        "total_quantity '{total}' must be a positive decimal number"
    ))?;
    if let Some(start) = c.start_at.as_deref() {
        k.rule(
            chrono::DateTime::parse_from_rfc3339(start).is_ok(),
            format_args!("start_at '{start}' is not an RFC 3339 time"),
        )?;
    }
    k.int("start_delay_secs", c.start_delay_secs, 0, 86_400)?;
    k.int("duration_secs", c.duration_secs, 60, 604_800)?;
    k.int("slice_secs", c.slice_secs, 30, 1800)?;
    k.rule(c.slice_secs <= c.duration_secs, format_args!(
        "slice_secs={} must not exceed duration_secs={}", c.slice_secs, c.duration_secs
    ))?;
    k.rule(c.passive_secs < c.slice_secs, format_args!(
        "passive_secs={} must be below slice_secs={}", c.passive_secs, c.slice_secs
    ))?;
    k.range("max_slip_bps", c.max_slip_bps, 0.1, 500.0)?;
    k.range("max_catchup", c.max_catchup, 1.0, 10.0)?;
    k.range("time_jitter", c.time_jitter, 0.0, 0.9)?;
    k.range("size_jitter", c.size_jitter, 0.0, 0.9)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 10)
}

fn validate_arbitrage(k: &Check<'_>, c: &ArbitrageConfig) -> Result<()> {
    k.range("min_edge_bps", c.min_edge_bps, 0.1, 1000.0)?;
    k.range("clip_usd_max", c.clip_usd_max, 12.0, 100_000.0)?;
    k.range("max_position_usd", c.max_position_usd, 12.0, 1_000_000.0)?;
    k.int("max_inflight", c.max_inflight.into(), 1, 10)?;
    k.int("cooldown_secs", c.cooldown_secs, 0, 3600)?;
    k.int("ref_max_age_secs", c.ref_max_age_secs, 1, 300)?;
    k.range("ref_stable_bps", c.ref_stable_bps, 0.1, 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PriceLevel, RfqMarketConfig};

    fn parse(toml: &str) -> Result<BTreeMap<String, StrategyConfig>> {
        parse_strategies(toml)
    }


    fn market(id: &str) -> MarketConfig {
        MarketConfig {
            market_id: id.to_string(),
            enabled: true,
            base_order_size: None,
            bid_levels: Vec::new(),
            offer_levels: Vec::new(),
            price_change_threshold_percent: 1.0,
            rfq: None,
        }
    }

    fn validate(strategy: StrategyConfig) -> Result<()> {
        validate_with(market("HTEST-USDC"), strategy)
    }

    fn validate_with(m: MarketConfig, strategy: StrategyConfig) -> Result<()> {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.strategies.insert(m.market_id.clone(), strategy);
        config.markets = vec![m];
        config.validate_ranges()
    }

    fn err(r: Result<()>) -> String {
        format!("{:#}", r.unwrap_err())
    }

    const ALL_KINDS: &str = r#"
        [[markets]]
        market_id = "A-USD"
        [markets.strategy]
        kind = "grid"
        [[markets]]
        market_id = "B-USD"
        [markets.strategy]
        kind = "market_maker"
        levels = 3
        half_spread_bps = 15
        ref_max_age_secs = 120
        [[markets]]
        market_id = "C-USD"
        [markets.strategy]
        kind = "rebalancer"
        target_share = 0.25
        mode = "maker"
        [[markets]]
        market_id = "D-USD"
        [markets.strategy]
        kind = "momentum"
        [[markets]]
        market_id = "E-USD"
        [markets.strategy]
        kind = "mean_reversion"
        mode = "taker"
        [[markets]]
        market_id = "F-USD"
        [markets.strategy]
        kind = "noise_taker"
        seed = 8
        [[markets]]
        market_id = "G-USD"
        [markets.strategy]
        kind = "twap"
        side = "buy"
        total_quantity = "250000"
        start_at = "2026-11-22T10:00:00Z"
        [[markets]]
        market_id = "H-USD"
        [markets.strategy]
        kind = "arbitrage"
        [[markets]]
        market_id = "I-USD"
    "#;

    #[test]
    fn every_kind_parses_with_defaults_and_overrides() {
        let map = parse(ALL_KINDS).unwrap();
        let kinds: Vec<&str> = map.values().map(StrategyConfig::kind).collect();
        assert_eq!(
            kinds,
            ["grid", "market_maker", "rebalancer", "momentum", "mean_reversion", "noise_taker", "twap", "arbitrage"]
        );
        assert!(!map.contains_key("I-USD"), "no table, no entry");
        let StrategyConfig::MarketMaker(mm) = &map["B-USD"] else { panic!() };
        assert_eq!((mm.levels, mm.half_spread_bps, mm.level_usd), (3, 15.0, 40.0));
        assert_eq!(map["B-USD"].common().and_then(|c| c.ref_max_age_secs), Some(120));
        let StrategyConfig::Rebalancer(rb) = &map["C-USD"] else { panic!() };
        assert_eq!((rb.target_share, rb.mode, rb.inflight_cap()), (Some(0.25), StrategyMode::Maker, 2));
        assert_eq!(map["C-USD"].common().map(|c| c.ref_max_age_secs), Some(None));
        assert_eq!(map["H-USD"].common().and_then(|c| c.ref_max_age_secs), Some(90));
        let StrategyConfig::NoiseTaker(nt) = &map["F-USD"] else { panic!() };
        assert_eq!((nt.seed, nt.rest_probability), (Some(8), 0.2));
        let StrategyConfig::Twap(tw) = &map["G-USD"] else { panic!() };
        assert_eq!((tw.side, tw.total_quantity.as_deref()), (Some(TradeSide::Buy), Some("250000")));
        assert_eq!(map["H-USD"], StrategyConfig::Arbitrage(ArbitrageConfig::default()));
    }

    #[test]
    fn a_file_without_strategy_tables_gives_an_empty_map() {
        let toml = r#"
            poll_interval_secs = 5
            [[markets]]
            market_id = "CC-USDC"
            bid_levels = [{ delta_percent = -0.5, quantity = "10" }]
        "#;
        assert!(parse(toml).unwrap().is_empty());
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn an_unknown_kind_names_the_valid_ones() {
        let e = format!("{:#}", parse("[[markets]]\nmarket_id = \"A-B\"\n[markets.strategy]\nkind = \"scalper\"\n").unwrap_err());
        assert!(e.contains("scalper") && e.contains("market_maker") && e.contains("noise_taker"), "{e}");
    }

    #[test]
    fn an_unknown_parameter_is_rejected_under_the_tag() {
        for (kind, extra) in [("market_maker", "half_spread = 10"), ("grid", "levels = 2"), ("rebalancer", "target = 0.5")] {
            let toml = format!("[[markets]]\nmarket_id = \"A-B\"\n[markets.strategy]\nkind = \"{kind}\"\n{extra}\n");
            let e = format!("{:#}", parse(&toml).unwrap_err());
            assert!(e.contains("unknown field"), "{kind}: {e}");
        }
    }

    #[test]
    fn a_market_listed_twice_with_a_strategy_is_rejected() {
        let toml = r#"
            [[markets]]
            market_id = "A-B"
            [[markets]]
            market_id = "A-B"
            [markets.strategy]
            kind = "market_maker"
        "#;
        assert!(format!("{:#}", parse(toml).unwrap_err()).contains("more than once"));
        // Twice without a table stays the grid's business
        assert!(parse("[[markets]]\nmarket_id = \"A-B\"\n[[markets]]\nmarket_id = \"A-B\"\n").unwrap().is_empty());
    }

    #[test]
    fn defaults_validate_for_every_kind() {
        for (id, strategy) in parse(ALL_KINDS).unwrap() {
            validate_with(market(&id), strategy).unwrap();
        }
    }

    #[test]
    fn a_strategy_with_levels_is_rejected_but_grid_with_levels_is_fine() {
        let mut m = market("HTEST-USDC");
        m.bid_levels = vec![PriceLevel { delta_percent: -0.5, quantity: "10".into() }];
        let e = err(validate_with(m.clone(), StrategyConfig::MarketMaker(MarketMakerConfig::default())));
        assert!(e.contains("bid_levels/offer_levels"), "{e}");
        validate_with(m, StrategyConfig::Grid(GridParams {})).unwrap();
    }

    #[test]
    fn a_strategy_on_a_liquidity_provider_is_rejected() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.markets = vec![market("HTEST-USDC")];
        config.strategies.insert("HTEST-USDC".into(), StrategyConfig::MarketMaker(MarketMakerConfig::default()));
        config.liquidity_provider = Some(toml::from_str("name = \"lp\"").unwrap());
        assert!(err(config.validate_ranges()).contains("[liquidity_provider]"));
        // Ignored, with a warning, when no orders are placed at all
        config.rfq_v2_only = true;
        config.validate_ranges().unwrap();
    }

    #[test]
    fn a_strategy_for_an_unlisted_market_is_rejected() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.strategies.insert("X-Y".into(), StrategyConfig::NoiseTaker(NoiseTakerConfig::default()));
        assert!(err(config.validate_ranges()).contains("not in [[markets]]"));
    }

    #[test]
    fn range_and_ordering_rules() {
        let mm = MarketMakerConfig::default;
        let cases: Vec<(StrategyConfig, &str)> = vec![
            (StrategyConfig::MarketMaker(MarketMakerConfig { levels: 0, ..mm() }), "levels=0"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { levels: 6, ..mm() }), "levels=6"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { skew_bps: 25.0, ..mm() }), "skew_bps=25 must be below"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { level_usd: f64::NAN, ..mm() }), "level_usd=NaN"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { throttle_spread_bps: 5.0, ..mm() }), "throttle_spread_bps"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { ref_floor: Some(0.006), ref_cap: Some(0.005), ..mm() }), "ref_floor"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { max_settlements_per_hour: 0, ..mm() }), "max_settlements_per_hour"),
            (StrategyConfig::MarketMaker(MarketMakerConfig { ref_max_age_secs: Some(0), ..mm() }), "ref_max_age_secs=0"),
            (StrategyConfig::Rebalancer(RebalancerConfig::default()), "target_share is required"),
            (StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(1.2), ..Default::default() }), "target_share=1.2"),
            (
                StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.5), band_exit: 0.03, ..Default::default() }),
                "band_exit=0.03 must be below",
            ),
            (StrategyConfig::Momentum(MomentumConfig { slow_secs: 300, ..Default::default() }), "3 x fast_secs"),
            (StrategyConfig::Momentum(MomentumConfig { exit_bps: 12.0, ..Default::default() }), "exit_bps=12 must be below"),
            (StrategyConfig::MeanReversion(MeanReversionConfig { full_bps: 200.0, ..Default::default() }), "entry_bps"),
            (StrategyConfig::NoiseTaker(NoiseTakerConfig { clip_usd_min: 50.0, ..Default::default() }), "clip_usd_min=50"),
            (StrategyConfig::NoiseTaker(NoiseTakerConfig { rest_probability: 1.5, ..Default::default() }), "rest_probability"),
            (StrategyConfig::Twap(TwapConfig::default()), "side"),
            (
                StrategyConfig::Twap(TwapConfig { side: Some(TradeSide::Buy), total_quantity: Some("1e5".into()), ..Default::default() }),
                "total_quantity '1e5'",
            ),
            (
                StrategyConfig::Twap(TwapConfig {
                    side: Some(TradeSide::Sell),
                    total_quantity: Some("100".into()),
                    start_at: Some("tomorrow".into()),
                    ..Default::default()
                }),
                "start_at 'tomorrow'",
            ),
            (
                StrategyConfig::Twap(TwapConfig {
                    side: Some(TradeSide::Sell),
                    total_quantity: Some("100".into()),
                    duration_secs: 120,
                    slice_secs: 240,
                    ..Default::default()
                }),
                "slice_secs=240 must not exceed",
            ),
            (StrategyConfig::Arbitrage(ArbitrageConfig { min_edge_bps: 0.0, ..Default::default() }), "min_edge_bps=0"),
        ];
        for (strategy, needle) in cases {
            let kind = strategy.kind();
            let e = err(validate(strategy));
            assert!(e.contains(needle), "{kind}: {e}");
            assert!(e.starts_with("Market HTEST-USDC: [markets.strategy]"), "{e}");
        }
    }

    #[test]
    fn take_lp_is_a_strict_boolean_for_takers_only() {
        let table = |kind: &str, extra: &str| {
            parse(&format!("[[markets]]\nmarket_id = \"A-USD\"\n[markets.strategy]\nkind = \"{kind}\"\n{extra}\n"))
        };
        for kind in ["rebalancer", "momentum", "mean_reversion", "noise_taker", "twap", "arbitrage"] {
            let map = table(kind, "take_lp = true").unwrap();
            assert_eq!(map["A-USD"].common().map(|c| c.take_lp), Some(true), "{kind}");
            let map = table(kind, "").unwrap();
            assert_eq!(map["A-USD"].common().map(|c| c.take_lp), Some(false), "{kind}");
            assert!(table(kind, "take_lp = \"yes\"").is_err(), "{kind}");
        }
        // A maker never takes, so it has no such parameter
        let e = format!("{:#}", table("market_maker", "take_lp = true").unwrap_err());
        assert!(e.contains("unknown field"), "{e}");
    }

    // A rebalancer or mean_reversion in maker mode only rests orders, so take_lp would do nothing
    #[test]
    fn take_lp_needs_the_taker_mode() {
        let rb = |mode| {
            StrategyConfig::Rebalancer(RebalancerConfig { target_share: Some(0.25), mode, take_lp: true, ..Default::default() })
        };
        let mr = |mode| StrategyConfig::MeanReversion(MeanReversionConfig { mode, take_lp: true, ..Default::default() });
        for strategy in [rb(StrategyMode::Maker), mr(StrategyMode::Maker)] {
            let e = err(validate(strategy));
            assert!(e.contains("take_lp applies only with mode = \"taker\""), "{e}");
        }
        validate(rb(StrategyMode::Taker)).unwrap();
        validate(mr(StrategyMode::Taker)).unwrap();
        // mean_reversion is a maker unless told otherwise
        let table = |extra: &str| {
            let head = "[[markets]]\nmarket_id = \"HTEST-USDC\"\n[markets.strategy]\nkind = \"mean_reversion\"\n";
            parse(&format!("{head}take_lp = true\n{extra}")).unwrap()["HTEST-USDC"].clone()
        };
        assert!(err(validate(table(""))).contains("take_lp applies only"));
        validate(table("mode = \"taker\"\n")).unwrap();
    }

    #[test]
    fn a_strategy_needs_a_quote_with_a_usd_rate() {
        let noise = || StrategyConfig::NoiseTaker(NoiseTakerConfig::default());
        for id in ["HECTO-cETH", "TUSDT-HECTO"] {
            let e = err(validate_with(market(id), noise()));
            assert!(e.contains("no USD rate"), "{e}");
        }
        for id in ["TUSDT-CC", "cETH-TUSDT", "CC-USDCx", "X-usd8", "HTEST-USDC", "A-tf-usdt"] {
            validate_with(market(id), noise()).unwrap();
        }
        // The grid has no USD amounts
        validate_with(market("HECTO-cETH"), StrategyConfig::Grid(GridParams {})).unwrap();
    }

    #[test]
    fn twap_is_rejected_on_an_rfq_market() {
        let mut m = market("HTEST-USDC");
        m.rfq = Some(toml::from_str::<RfqMarketConfig>("min_quantity = \"1\"\nmax_quantity = \"10\"").unwrap());
        let twap = TwapConfig { side: Some(TradeSide::Buy), total_quantity: Some("10".into()), ..Default::default() };
        assert!(err(validate_with(m, StrategyConfig::Twap(twap))).contains("[markets.rfq]"));
    }

    #[test]
    fn quantities_parse_exactly() {
        assert_eq!(parse_quantity("250000"), Decimal::from_str_exact("250000").ok());
        assert_eq!(parse_quantity(" 0.5 "), Decimal::from_str_exact("0.5").ok());
        for bad in ["1e5", "0", "-1", "abc", ""] {
            assert_eq!(parse_quantity(bad), None, "{bad}");
        }
    }
}
