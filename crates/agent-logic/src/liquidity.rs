//! Liquidity management for the orderbook agent.
//!
//! Tracks per-token balances, commitments to in-flight settlements,
//! and buy/sell flow to detect inventory depletion.  Provides:
//!
//! - **TokenBudget**: available vs committed balance per token
//! - **FlowTracker**: EMA-based depletion rate detection
//! - **Spread Adjuster**: depletion coefficient for dynamic spread widening

use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::debug;

// ---------------------------------------------------------------------------
// Configuration constants
// ---------------------------------------------------------------------------

/// CC token key used in the tokens map (distinct from instrument_id).
pub const CC_TOKEN: &str = "CC";

/// A balance not refreshed within this window is reported as stale.
pub const DEFAULT_BALANCE_STALE_AFTER: Duration = Duration::from_secs(120);

/// A prepaid figure without a successful poll within this window is unknown to the fee checks.
pub const PREPAID_STALE_AFTER: Duration = Duration::from_secs(120);

/// Local fee debits kept between polls; past this the oldest two are merged.
const MAX_LOCAL_DEBITS: usize = 1024;

/// Prepaid CC held per settlement for its own transactions: traffic and service fees.
pub const DEFAULT_SETTLEMENT_TRAFFIC_CC: Decimal = Decimal::from_parts(12, 0, 0, false, 1);

/// The per-side processing fee of one fill, in USD, for a market with none seen yet.
const MIN_FILL_FEES_USD: Decimal = Decimal::ONE;

/// Markets whose last per-side fee is kept.
const MAX_FILL_FEE_MARKETS: usize = 256;

/// Marks a commit refused only because fee capacity is not known yet; it is retried.
pub const FEE_CAPACITY_UNKNOWN: &str = "fee capacity unknown";

const BALANCES_NOT_LOADED: &str = "Balances not loaded yet";
const PREPAID_NOT_LOADED: &str = "Prepaid balance not loaded yet";
const PREPAID_STALE: &str = "Prepaid balance stale";

/// A configured per-settlement traffic hold in CC; the default when unusable.
pub fn settlement_traffic_cc(configured: f64) -> Decimal {
    Decimal::from_f64(configured)
        .filter(|cc| *cc >= Decimal::ZERO)
        .unwrap_or(DEFAULT_SETTLEMENT_TRAFFIC_CC)
}

/// Display bounds: `±u64::MAX`.
const SHOWN_MAX: Decimal = Decimal::from_parts(u32::MAX, u32::MAX, 0, false, 0);
const SHOWN_MIN: Decimal = Decimal::from_parts(u32::MAX, u32::MAX, 0, true, 0);

/// `amount` capped to what `{:.4}` can print; Decimal formatting panics near its maximum.
pub(crate) fn shown(amount: Decimal) -> Decimal {
    amount.max(SHOWN_MIN).min(SHOWN_MAX)
}

// ---------------------------------------------------------------------------
// Per-token state
// ---------------------------------------------------------------------------

/// Balance and commitment state for a single token.
struct TokenState {
    /// Current selectable (CC) or unlocked (non-CC) balance.
    balance: Decimal,
    /// CC committed to this token's allocations, keyed by proposal_id.
    allocation_commitments: HashMap<String, Decimal>,
    /// When `balance` was last set from a fresh read; `None` until the first.
    last_refreshed: Option<Instant>,
}

impl TokenState {
    fn new() -> Self {
        Self {
            balance: Decimal::ZERO,
            allocation_commitments: HashMap::new(),
            last_refreshed: None,
        }
    }

    /// A negative reported balance is held as zero.
    fn set_balance(&mut self, balance: Decimal, now: Instant) {
        self.balance = balance.max(Decimal::ZERO);
        self.last_refreshed = Some(now);
    }

    fn committed(&self) -> Decimal {
        self.allocation_commitments.values().fold(Decimal::ZERO, |acc, v| acc.saturating_add(*v))
    }

    fn available(&self) -> Decimal {
        self.balance.saturating_sub(self.committed()).max(Decimal::ZERO)
    }
}

// ---------------------------------------------------------------------------
// Flow tracking (EMA-based depletion detection)
// ---------------------------------------------------------------------------

/// Per-token flow tracker for depletion rate detection.
struct TokenFlow {
    /// EMA of net outflow per hour (positive = depleting).
    net_outflow_per_hour: f64,
    /// Timestamp of the last EMA update.
    last_update: Instant,
    /// Accumulated outflow since last EMA tick.
    pending_outflow: f64,
    /// Accumulated inflow since last EMA tick.
    pending_inflow: f64,
}

impl TokenFlow {
    fn new() -> Self {
        Self {
            net_outflow_per_hour: 0.0,
            last_update: Instant::now(),
            pending_outflow: 0.0,
            pending_inflow: 0.0,
        }
    }

    /// Flush pending flow into the EMA.  Called before reads and periodically.
    fn flush(&mut self, alpha: f64) {
        let elapsed_secs = self.last_update.elapsed().as_secs_f64();
        if elapsed_secs < 1.0 {
            return; // too soon, accumulate more
        }

        let net_outflow = self.pending_outflow - self.pending_inflow;
        let rate_this_period = net_outflow / (elapsed_secs / 3600.0); // per hour

        // EMA update: new = alpha * sample + (1 - alpha) * old
        self.net_outflow_per_hour =
            alpha * rate_this_period + (1.0 - alpha) * self.net_outflow_per_hour;

        self.pending_outflow = 0.0;
        self.pending_inflow = 0.0;
        self.last_update = Instant::now();
    }
}

/// Serializable snapshot of flow state for persistence across restarts.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedTokenFlow {
    pub token: String,
    pub net_outflow_per_hour: f64,
}

// ---------------------------------------------------------------------------
// Fee commitment tracking
// ---------------------------------------------------------------------------

/// Tracks CC committed for fee payments, keyed by proposal_id.
/// Separate from token allocation commitments because fees are always in CC
/// regardless of which token is being allocated.
struct FeeCommitments {
    entries: HashMap<String, Decimal>,
}

impl FeeCommitments {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn total(&self) -> Decimal {
        self.entries.values().fold(Decimal::ZERO, |acc, v| acc.saturating_add(*v))
    }
}

// ---------------------------------------------------------------------------
// Liquidity state (all behind a single RwLock)
// ---------------------------------------------------------------------------

struct LiquidityState {
    /// Per-token balances and allocation commitments.
    tokens: HashMap<String, TokenState>,
    /// Per-token flow tracking.
    flows: HashMap<String, TokenFlow>,
    /// CC committed for fees, paid from prepaid credit (separate from CC allocations).
    fee_commitments: FeeCommitments,
    /// Cached CC/USD exchange rate (for fee estimation).
    cc_usd_rate: Decimal,
    /// Alias map: short name → canonical key (e.g. "USDCx" → instrument_id).
    aliases: HashMap<String, String>,
    /// Off-chain prepaid credit that settlement fees are debited from.
    prepaid: PrepaidState,
    /// Prepaid held per settlement for its own transactions, apart from its fees.
    traffic_holds: HashMap<String, Decimal>,
    /// This side's fees (USD) on the last settlement seen per market.
    fill_fees_usd: HashMap<String, Decimal>,
}

/// Prepaid credit as last polled, with the fees paid since.
#[derive(Default)]
struct PrepaidState {
    /// Available credit (balance plus credit limit) and when that poll was sent.
    polled: Option<(Decimal, Instant)>,
    /// Fees debited locally, by time; a poll sent after one already reflects it.
    local_debits: VecDeque<(Instant, Decimal)>,
    /// Amount of one auto top-up, while auto top-up runs.
    topup_cc: Option<Decimal>,
    /// Prepaid held for each settlement's own transactions.
    traffic_cc: Decimal,
}

/// Why fee capacity does not cover a fee.
enum FeeShortfall {
    /// A figure the check needs is unknown or stale.
    Unknown(&'static str),
    /// Capacity is known and too small.
    Short(String),
}

/// The adoption gate's verdict on one settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adoption {
    /// Funds and fee capacity suffice.
    Accept,
    /// Not decidable yet (balance, rate or prepaid credit unknown): hold it.
    Defer(String),
    /// Short of funds or fee capacity.
    Reject(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Manages liquidity across all traded tokens.
///
/// Thread-safe via internal `RwLock`.  All methods are async.
pub struct LiquidityManager {
    state: RwLock<LiquidityState>,
    /// Minimum CC to keep available after all commitments.
    fee_reserve_cc: Decimal,
    /// Safety margin multiplier for estimates (e.g. 1.1 = 10%).
    margin: f64,
    /// EMA smoothing factor (derived from window hours).
    ema_alpha: f64,
    /// Hours-to-depletion above which coefficient is 0.
    depletion_max_hours: f64,
    /// Hours-to-depletion below which coefficient is 10.
    depletion_min_hours: f64,
    /// Staleness window for balances, in seconds.
    stale_after_secs: AtomicU64,
}

impl LiquidityManager {
    /// Create a new `LiquidityManager`.
    ///
    /// - `fee_reserve_cc`: minimum CC kept free (from `AGENT_FEE_RESERVE_CC`)
    /// - `margin`: safety multiplier for fee estimates (e.g. 1.1)
    /// - `ema_window_hours`: EMA window for flow tracking (e.g. 4.0)
    /// - `depletion_max_hours`: hours above which depletion coefficient = 0
    /// - `depletion_min_hours`: hours below which depletion coefficient = 10
    pub fn new(
        fee_reserve_cc: f64,
        margin: f64,
        ema_window_hours: f64,
        depletion_max_hours: f64,
        depletion_min_hours: f64,
    ) -> Arc<Self> {
        // EMA alpha: higher alpha = more responsive to recent data
        // Using minute-granularity: alpha = 2 / (N + 1) where N = window in minutes
        let alpha = 2.0 / (ema_window_hours * 60.0 + 1.0);

        Arc::new(Self {
            state: RwLock::new(LiquidityState {
                tokens: HashMap::new(),
                flows: HashMap::new(),
                fee_commitments: FeeCommitments::new(),
                cc_usd_rate: Decimal::ZERO,
                aliases: HashMap::new(),
                prepaid: PrepaidState {
                    traffic_cc: DEFAULT_SETTLEMENT_TRAFFIC_CC,
                    ..PrepaidState::default()
                },
                traffic_holds: HashMap::new(),
                fill_fees_usd: HashMap::new(),
            }),
            fee_reserve_cc: Decimal::from_f64_retain(fee_reserve_cc).unwrap_or(Decimal::ZERO),
            margin,
            ema_alpha: alpha,
            depletion_max_hours,
            depletion_min_hours,
            stale_after_secs: AtomicU64::new(DEFAULT_BALANCE_STALE_AFTER.as_secs()),
        })
    }

    /// Override the staleness window (see [`DEFAULT_BALANCE_STALE_AFTER`]).
    pub fn set_stale_after(&self, stale_after: Duration) {
        self.stale_after_secs
            .store(stale_after.as_secs(), Ordering::Relaxed);
    }

    fn stale_after(&self) -> Duration {
        Duration::from_secs(self.stale_after_secs.load(Ordering::Relaxed))
    }

    /// True once a non-CC balance and either the CC balance or the prepaid credit are loaded;
    /// until then the gates report "Balances loading", not a false "Insufficient liquidity".
    pub async fn is_ready(&self) -> bool {
        let s = self.state.read().await;
        let has_cc = s.tokens.contains_key(CC_TOKEN);
        let has_non_cc = s.tokens.keys().any(|k| k != CC_TOKEN);
        has_non_cc && (has_cc || s.prepaid.polled.is_some())
    }

    // -----------------------------------------------------------------------
    // Balance updates (called by ACS worker / runner balance refresh)
    // -----------------------------------------------------------------------

    /// Update CC balance from AmuletCache selectable total.
    pub async fn update_cc_balance(&self, selectable_cc: Decimal) {
        let now = Instant::now();
        let mut s = self.state.write().await;
        s.tokens
            .entry(CC_TOKEN.to_string())
            .or_insert_with(TokenState::new)
            .set_balance(selectable_cc, now);
    }

    /// Update a non-CC token balance from GetBalances RPC.
    pub async fn update_token_balance(&self, instrument_id: &str, unlocked: Decimal) {
        let now = Instant::now();
        let mut s = self.state.write().await;
        s.tokens
            .entry(instrument_id.to_string())
            .or_insert_with(TokenState::new)
            .set_balance(unlocked, now);
    }

    /// Update several token balances under one lock.
    pub async fn update_token_balances(&self, updates: &[(String, Decimal)]) {
        if updates.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut s = self.state.write().await;
        for (instrument_id, unlocked) in updates {
            s.tokens
                .entry(instrument_id.clone())
                .or_insert_with(TokenState::new)
                .set_balance(*unlocked, now);
        }
    }

    /// Clear the refresh stamp of every non-CC token absent from a full
    /// balance snapshot, so its last value is no longer treated as current.
    /// Balances are kept — a filtered or partial view must not blank them.
    pub async fn mark_missing_unrefreshed(&self, present: &HashSet<String>) {
        let mut s = self.state.write().await;
        for (token, state) in s.tokens.iter_mut() {
            if token != CC_TOKEN && !present.contains(token) {
                state.last_refreshed = None;
            }
        }
    }

    /// Age of a token's balance beyond the staleness window, if it is stale.
    ///
    /// `None` for fresh balances and for tokens with no refresh recorded.
    pub async fn is_stale(&self, token: &str) -> Option<Duration> {
        let s = self.state.read().await;
        Self::stale_age(&s, token, Instant::now(), self.stale_after())
    }

    /// Every token whose balance is stale, with its age.
    pub async fn stale_tokens(&self) -> Vec<(String, Duration)> {
        let s = self.state.read().await;
        let now = Instant::now();
        let stale_after = self.stale_after();
        let mut out: Vec<(String, Duration)> = s
            .tokens
            .keys()
            .filter_map(|k| Self::stale_age(&s, k, now, stale_after).map(|age| (k.clone(), age)))
            .collect();
        out.sort();
        out
    }

    fn stale_age(
        s: &LiquidityState,
        token: &str,
        now: Instant,
        stale_after: Duration,
    ) -> Option<Duration> {
        let resolved = Self::resolve_alias(&s.aliases, token);
        let refreshed = s.tokens.get(resolved)?.last_refreshed?;
        let age = now.saturating_duration_since(refreshed);
        (age >= stale_after).then_some(age)
    }

    /// Register a short alias for a token (e.g. "USDCx" → instrument_id).
    /// Used by the RFQ handler to look up tokens by market symbol.
    pub async fn register_alias(&self, alias: &str, canonical_key: &str) {
        let mut s = self.state.write().await;
        s.aliases.insert(alias.to_string(), canonical_key.to_string());
    }

    /// Resolve a token name through the alias map.
    /// Returns the canonical key if an alias exists, otherwise the input.
    fn resolve_alias<'a>(aliases: &'a HashMap<String, String>, token: &'a str) -> &'a str {
        aliases.get(token).map(|s| s.as_str()).unwrap_or(token)
    }

    /// Update the cached CC/USD exchange rate.
    pub async fn update_cc_usd_rate(&self, rate: Decimal) {
        let mut s = self.state.write().await;
        s.cc_usd_rate = rate;
    }

    /// Record a polled prepaid figure (balance plus credit limit, in CC) sent at `polled_at`;
    /// debits after that time are kept, and an older poll is ignored.
    pub async fn update_prepaid(&self, available_cc: Decimal, polled_at: Instant) {
        let mut s = self.state.write().await;
        if s.prepaid.polled.is_some_and(|(_, at)| polled_at < at) {
            return;
        }
        s.prepaid.local_debits.retain(|(at, _)| *at >= polled_at);
        s.prepaid.polled = Some((available_cc, polled_at));
    }

    /// Whether auto top-up runs, and the CC it moves per top-up.
    pub async fn set_auto_topup(&self, topup_cc: Option<Decimal>) {
        let mut s = self.state.write().await;
        s.prepaid.topup_cc = topup_cc.filter(|t| *t > Decimal::ZERO);
    }

    /// Prepaid CC held for each settlement's own transactions (negative reads as zero).
    pub async fn set_settlement_traffic(&self, traffic_cc: Decimal) {
        let mut s = self.state.write().await;
        s.prepaid.traffic_cc = traffic_cc.max(Decimal::ZERO);
    }

    /// A fee was debited: lowers the prepaid figure and the proposal's fee commitment by
    /// `paid_cc` as stated, else by the estimate for `fee_usd`, else by the remaining commitment.
    pub async fn record_fee_paid(&self, proposal_id: &str, paid_cc: Option<Decimal>, fee_usd: Option<Decimal>) {
        let mut s = self.state.write().await;
        let committed = s.fee_commitments.entries.get(proposal_id).copied();
        let debit = match paid_cc.filter(|cc| *cc >= Decimal::ZERO) {
            Some(stated) => stated,
            None => fee_usd
                .and_then(|usd| self.fee_cc_at(s.cc_usd_rate, usd))
                .filter(|cc| *cc > Decimal::ZERO)
                .or(committed)
                .unwrap_or(Decimal::ZERO),
        };
        if debit <= Decimal::ZERO {
            return;
        }
        if s.prepaid.local_debits.len() >= MAX_LOCAL_DEBITS {
            // Merged under the later time, so it is dropped no sooner
            let oldest = s.prepaid.local_debits.pop_front().map_or(Decimal::ZERO, |(_, d)| d);
            if let Some(next) = s.prepaid.local_debits.front_mut() {
                next.1 = next.1.saturating_add(oldest);
            }
        }
        s.prepaid.local_debits.push_back((Instant::now(), debit));
        if let Some(c) = s.fee_commitments.entries.get_mut(proposal_id) {
            *c = c.saturating_sub(debit).max(Decimal::ZERO);
        }
        debug!("[{}] Prepaid credit debited locally: {:.4} CC", proposal_id, shown(debit));
    }

    /// This side's last fee is recorded: what its fee commitment still holds is margin,
    /// not a future debit. The traffic hold stays until release.
    pub async fn clear_fee_commitment(&self, proposal_id: &str) {
        let mut s = self.state.write().await;
        if let Some(c) = s.fee_commitments.entries.get_mut(proposal_id) {
            *c = Decimal::ZERO;
        }
    }

    /// Note this side's fees (USD) on a settlement in `market_id`, for that market's per-fill need.
    pub async fn note_fill_fee(&self, market_id: &str, fees_usd: Decimal) {
        if market_id.is_empty() || fees_usd < Decimal::ZERO {
            return;
        }
        let mut s = self.state.write().await;
        if s.fill_fees_usd.len() < MAX_FILL_FEE_MARKETS || s.fill_fees_usd.contains_key(market_id) {
            s.fill_fees_usd.insert(market_id.to_string(), fees_usd);
        }
    }

    /// Hold the state lock until the guard drops.
    #[cfg(test)]
    pub(crate) async fn hold_lock(&self) -> impl Drop + '_ {
        self.state.write().await
    }

    // -----------------------------------------------------------------------
    // Commitment management
    // -----------------------------------------------------------------------

    /// Estimate CC needed for a settlement's fees (one side only).
    ///
    /// `my_fees_usd` is the sum of dvp + allocation fee for this agent's side.
    /// Returns `my_fees_usd / cc_usd_rate * margin`, or `Decimal::ZERO` if rate unknown.
    pub async fn estimate_fee_cc(&self, my_fees_usd: Decimal) -> Decimal {
        let s = self.state.read().await;
        self.fee_cc_at(s.cc_usd_rate, my_fees_usd).unwrap_or(Decimal::ZERO)
    }

    /// `fees_usd / rate * margin`; `None` without a usable rate.
    fn fee_cc_at(&self, rate: Decimal, fees_usd: Decimal) -> Option<Decimal> {
        if rate <= Decimal::ZERO {
            return None;
        }
        if fees_usd <= Decimal::ZERO {
            return Some(Decimal::ZERO);
        }
        let margin = Decimal::from_f64_retain(self.margin).unwrap_or(Decimal::ONE);
        // Out of Decimal range reads as unaffordable
        Some(
            fees_usd
                .checked_div(rate)
                .and_then(|raw| raw.checked_mul(margin))
                .unwrap_or(Decimal::MAX),
        )
    }

    /// Polled prepaid credit less local debits, while the poll is fresh.
    fn prepaid_now(s: &LiquidityState, now: Instant) -> Option<Decimal> {
        let (polled, at) = s.prepaid.polled?;
        if now.saturating_duration_since(at) >= PREPAID_STALE_AFTER {
            return None;
        }
        Some(polled.saturating_sub(Self::local_debit_total(s)))
    }

    fn local_debit_total(s: &LiquidityState) -> Decimal {
        s.prepaid
            .local_debits
            .iter()
            .fold(Decimal::ZERO, |acc, (_, d)| acc.saturating_add(*d))
    }

    /// Prepaid held for in-flight settlements: unpaid fees and traffic holds.
    fn held_total(s: &LiquidityState) -> Decimal {
        s.traffic_holds
            .values()
            .fold(s.fee_commitments.total(), |acc, v| acc.saturating_add(*v))
            .max(Decimal::ZERO)
    }

    /// Wallet CC that held fees need moved into prepaid, in whole top-ups (unknown
    /// prepaid reads as zero); zero without auto top-up.
    fn wallet_hold(s: &LiquidityState, now: Instant) -> Decimal {
        let Some(topup) = s.prepaid.topup_cc.filter(|t| *t > Decimal::ZERO) else {
            return Decimal::ZERO;
        };
        let short = Self::held_total(s).saturating_sub(Self::prepaid_now(s, now).unwrap_or(Decimal::ZERO));
        if short <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        short
            .checked_div(topup)
            .map(|n| n.ceil())
            .and_then(|n| n.checked_mul(topup))
            .unwrap_or(Decimal::MAX)
    }

    /// CC free to trade: after commitments, the fee reserve and the wallet hold.
    fn cc_free(&self, s: &LiquidityState, cc_available: Decimal, now: Instant) -> Decimal {
        cc_available
            .saturating_sub(self.fee_reserve_cc)
            .saturating_sub(Self::wallet_hold(s, now))
            .max(Decimal::ZERO)
    }

    /// The CC part of an allocation (zero for any other token).
    fn cc_allocation(s: &LiquidityState, allocation_token: &str, allocation_amount: Decimal) -> Decimal {
        if Self::resolve_alias(&s.aliases, allocation_token) == CC_TOKEN {
            allocation_amount
        } else {
            Decimal::ZERO
        }
    }

    /// Wallet CC that auto top-up can move into prepaid credit: whole top-ups of
    /// the CC above the reserve and `cc_allocation`; zero without auto top-up.
    fn wallet_term(&self, s: &LiquidityState, cc_allocation: Decimal) -> Decimal {
        let Some(topup) = s.prepaid.topup_cc.filter(|t| *t > Decimal::ZERO) else {
            return Decimal::ZERO;
        };
        let spare = s
            .tokens
            .get(CC_TOKEN)
            .map_or(Decimal::ZERO, |t| t.available())
            .saturating_sub(self.fee_reserve_cc)
            .saturating_sub(cc_allocation);
        if spare <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        spare
            .checked_div(topup)
            .map(|n| n.floor())
            .and_then(|n| n.checked_mul(topup))
            .unwrap_or(Decimal::ZERO)
            .max(Decimal::ZERO)
    }

    /// Fee capacity in CC: prepaid credit plus the wallet term, less what in-flight settlements
    /// hold, never below zero; `None` while the prepaid figure is unknown or stale.
    fn capacity(&self, s: &LiquidityState, now: Instant, cc_allocation: Decimal) -> Option<Decimal> {
        let prepaid = Self::prepaid_now(s, now)?;
        Some(
            prepaid
                .saturating_add(self.wallet_term(s, cc_allocation))
                .saturating_sub(Self::held_total(s))
                .max(Decimal::ZERO),
        )
    }

    /// Why the prepaid figure is unusable.
    fn prepaid_unknown(s: &LiquidityState) -> &'static str {
        if s.prepaid.polled.is_none() {
            PREPAID_NOT_LOADED
        } else {
            PREPAID_STALE
        }
    }

    /// Whether the allocation token covers the allocation (absent entries count as zero);
    /// a CC allocation also leaves the fee reserve in place.
    fn check_allocation(
        &self,
        s: &LiquidityState,
        allocation_token: &str,
        allocation_amount: Decimal,
    ) -> Result<(), String> {
        // Resolve aliases so commitment checks agree with `available()`.
        let allocation_token = Self::resolve_alias(&s.aliases, allocation_token);
        let token_available = s
            .tokens
            .get(allocation_token)
            .map_or(Decimal::ZERO, |t| t.available());

        if token_available < allocation_amount {
            return Err(format!(
                "insufficient {} ({:.4} available, {:.4} needed for allocation)",
                allocation_token, shown(token_available), shown(allocation_amount)
            ));
        }
        if allocation_token == CC_TOKEN {
            let free = token_available.saturating_sub(self.fee_reserve_cc);
            if free < allocation_amount {
                return Err(format!(
                    "insufficient CC ({:.4} available after reserve, {:.4} needed for allocation)",
                    shown(free.max(Decimal::ZERO)),
                    shown(allocation_amount)
                ));
            }
        }
        Ok(())
    }

    /// Whether fee capacity covers this settlement's `fee_cc` and `traffic_cc` for this allocation.
    fn check_fee(
        &self,
        s: &LiquidityState,
        now: Instant,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
        traffic_cc: Decimal,
    ) -> Result<(), FeeShortfall> {
        let cc_allocation = Self::cc_allocation(s, allocation_token, allocation_amount);
        let need = fee_cc.max(Decimal::ZERO).saturating_add(traffic_cc.max(Decimal::ZERO));
        if need <= Decimal::ZERO {
            return self.check_wallet_cover(s, now, cc_allocation);
        }
        match self.capacity(s, now, cc_allocation) {
            Some(capacity) if capacity >= need => Ok(()),
            // The wallet term read an unloaded CC balance as zero
            Some(_) if s.prepaid.topup_cc.is_some() && !s.tokens.contains_key(CC_TOKEN) => {
                Err(FeeShortfall::Unknown(BALANCES_NOT_LOADED))
            }
            Some(capacity) => Err(FeeShortfall::Short(format!(
                "insufficient fee capacity ({:.4} CC available, {:.4} needed)",
                shown(capacity),
                shown(need)
            ))),
            // Out of Decimal range: unaffordable whatever the credit
            None if need == Decimal::MAX => Err(FeeShortfall::Short(format!(
                "fee estimate out of range ({:.4} needed)",
                shown(need)
            ))),
            None => Err(FeeShortfall::Unknown(Self::prepaid_unknown(s))),
        }
    }

    /// With nothing of its own to fund, a CC allocation must still leave the wallet CC
    /// that held fees count on for their top-up (unknown prepaid reads as zero).
    fn check_wallet_cover(
        &self,
        s: &LiquidityState,
        now: Instant,
        cc_allocation: Decimal,
    ) -> Result<(), FeeShortfall> {
        let held = Self::held_total(s);
        if cc_allocation <= Decimal::ZERO || s.prepaid.topup_cc.is_none() || held <= Decimal::ZERO {
            return Ok(());
        }
        let prepaid = Self::prepaid_now(s, now);
        let cover = prepaid
            .unwrap_or(Decimal::ZERO)
            .saturating_add(self.wallet_term(s, cc_allocation));
        if cover >= held {
            return Ok(());
        }
        match prepaid {
            Some(_) => Err(FeeShortfall::Short(format!(
                "insufficient CC ({:.4} CC of held fees need wallet CC for top-up, {:.4} covered)",
                shown(held),
                shown(cover.max(Decimal::ZERO))
            ))),
            None => Err(FeeShortfall::Unknown(Self::prepaid_unknown(s))),
        }
    }

    /// Shared availability check for the commits and `can_commit`; read-only, so all see
    /// the same verdict. Unknown fee capacity fails with [`FEE_CAPACITY_UNKNOWN`].
    fn check_commit(
        &self,
        s: &LiquidityState,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
        traffic_cc: Decimal,
    ) -> Result<(), String> {
        self.check_allocation(s, allocation_token, allocation_amount)?;
        self.check_fee(s, Instant::now(), allocation_token, allocation_amount, fee_cc, traffic_cc)
            .map_err(|shortfall| match shortfall {
                FeeShortfall::Unknown(why) => format!("{FEE_CAPACITY_UNKNOWN} ({why})"),
                FeeShortfall::Short(reason) => reason,
            })
    }

    /// The adoption gate's check for a settlement this agent runs, under one lock: a
    /// shortfall rejects, while an unknown CC balance, rate or prepaid figure defers.
    pub async fn adoption_check(
        &self,
        allocation_token: &str,
        allocation_amount: Decimal,
        my_fees_usd: Decimal,
    ) -> Adoption {
        self.gate(allocation_token, allocation_amount, my_fees_usd, true).await
    }

    /// [`Self::adoption_check`] for a quote whose settlement runs none of this agent's
    /// transactions or fees: the allocation, and the CC that held fees count on.
    pub async fn quote_check(&self, allocation_token: &str, allocation_amount: Decimal) -> Adoption {
        self.gate(allocation_token, allocation_amount, Decimal::ZERO, false).await
    }

    async fn gate(
        &self,
        allocation_token: &str,
        allocation_amount: Decimal,
        my_fees_usd: Decimal,
        own_traffic: bool,
    ) -> Adoption {
        let s = self.state.read().await;
        if Self::resolve_alias(&s.aliases, allocation_token) == CC_TOKEN
            && !s.tokens.contains_key(CC_TOKEN)
        {
            return Adoption::Defer(BALANCES_NOT_LOADED.to_string());
        }
        if let Err(reason) = self.check_allocation(&s, allocation_token, allocation_amount) {
            return Adoption::Reject(reason);
        }
        let fee_cc = match self.fee_cc_at(s.cc_usd_rate, my_fees_usd) {
            Some(fee_cc) => fee_cc,
            None if my_fees_usd > Decimal::ZERO => {
                return Adoption::Defer("CC rate not known yet".to_string())
            }
            None => Decimal::ZERO,
        };
        let traffic_cc = if own_traffic { s.prepaid.traffic_cc } else { Decimal::ZERO };
        match self.check_fee(&s, Instant::now(), allocation_token, allocation_amount, fee_cc, traffic_cc) {
            Ok(()) => Adoption::Accept,
            Err(FeeShortfall::Short(reason)) => Adoption::Reject(reason),
            Err(FeeShortfall::Unknown(why)) => Adoption::Defer(why.to_string()),
        }
    }

    /// Fee capacity in CC with no allocation; `None` while prepaid credit is unknown or stale.
    pub async fn fee_capacity(&self) -> Option<Decimal> {
        let s = self.state.read().await;
        self.capacity(&s, Instant::now(), Decimal::ZERO)
    }

    /// Fee capacity in CC for a settlement allocating `allocation_amount` of `allocation_token`.
    #[cfg(test)]
    pub async fn fee_capacity_for(&self, allocation_token: &str, allocation_amount: Decimal) -> Option<Decimal> {
        let s = self.state.read().await;
        let cc_allocation = Self::cc_allocation(&s, allocation_token, allocation_amount);
        self.capacity(&s, Instant::now(), cc_allocation)
    }

    /// Fee capacity one fill needs, plus its own traffic: per market from the last fee seen
    /// there, and the default for a market with none (zero fee without a rate).
    pub async fn fill_needs(&self) -> (Decimal, Vec<(String, Decimal)>) {
        let s = self.state.read().await;
        let need = |fees_usd: Decimal| {
            self.fee_cc_at(s.cc_usd_rate, fees_usd)
                .unwrap_or(Decimal::ZERO)
                .saturating_add(s.prepaid.traffic_cc)
        };
        let by_market = s.fill_fees_usd.iter().map(|(m, usd)| (m.clone(), need(*usd))).collect();
        (need(MIN_FILL_FEES_USD), by_market)
    }

    /// Advisory availability check — same rules as `try_commit`, no mutation.
    ///
    /// Used by the adoption gate: the agent verifies it *could* fund the
    /// settlement before preconfirming, but the actual reservation is deferred
    /// until the counterparty has preconfirmed (see `ensure_reserved` in
    /// settlement.rs), mirroring the RFQ V2 indicative/confirm split.
    pub async fn can_commit(
        &self,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
    ) -> Result<(), String> {
        let s = self.state.read().await;
        self.check_commit(&s, allocation_token, allocation_amount, fee_cc, Decimal::ZERO)
    }

    /// True if a commitment (allocation, fee or traffic) exists for this proposal.
    pub async fn has_commitment(&self, proposal_id: &str) -> bool {
        let s = self.state.read().await;
        s.fee_commitments.entries.contains_key(proposal_id)
            || s.traffic_holds.contains_key(proposal_id)
            || s.tokens
                .values()
                .any(|t| t.allocation_commitments.contains_key(proposal_id))
    }

    /// Prepaid that in-flight settlements still hold: unpaid fees and traffic; never below zero.
    pub async fn prepaid_demand(&self) -> Decimal {
        let s = self.state.read().await;
        Self::held_total(&s)
    }

    /// Try to commit resources for a settlement.
    ///
    /// Checks **both** the allocation token balance **and** fee capacity.
    /// On success, records the commitment.  On failure, returns an error
    /// description suitable for logging.
    ///
    /// - `allocation_token`: `"CC"` or an instrument_id
    /// - `allocation_amount`: token amount the agent must allocate
    /// - `fee_cc`: estimated CC for this side's fees
    pub async fn try_commit(
        &self,
        proposal_id: &str,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
    ) -> Result<(), String> {
        self.commit(proposal_id, allocation_token, allocation_amount, fee_cc, false).await
    }

    /// [`Self::try_commit`] for a settlement this agent runs: prices `fees_usd` under the lock and
    /// holds its traffic too; returns the fee held, or [`FEE_CAPACITY_UNKNOWN`] without a rate.
    pub async fn try_commit_settlement(
        &self,
        proposal_id: &str,
        allocation_token: &str,
        allocation_amount: Decimal,
        fees_usd: Decimal,
    ) -> Result<Decimal, String> {
        let mut s = self.state.write().await;
        let fee_cc = match self.fee_cc_at(s.cc_usd_rate, fees_usd) {
            Some(fee_cc) => fee_cc,
            None if fees_usd > Decimal::ZERO => {
                return Err(format!("{FEE_CAPACITY_UNKNOWN} (CC rate not known yet)"));
            }
            None => Decimal::ZERO,
        };
        self.commit_in(&mut s, proposal_id, allocation_token, allocation_amount, fee_cc, true)?;
        Ok(fee_cc)
    }

    async fn commit(
        &self,
        proposal_id: &str,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
        own_traffic: bool,
    ) -> Result<(), String> {
        let mut s = self.state.write().await;
        self.commit_in(&mut s, proposal_id, allocation_token, allocation_amount, fee_cc, own_traffic)
    }

    fn commit_in(
        &self,
        s: &mut LiquidityState,
        proposal_id: &str,
        allocation_token: &str,
        allocation_amount: Decimal,
        fee_cc: Decimal,
        own_traffic: bool,
    ) -> Result<(), String> {
        let traffic_cc = if own_traffic { s.prepaid.traffic_cc } else { Decimal::ZERO };

        // Atomic check-and-reserve: same check as `can_commit`, but under the
        // write lock so no other committer can slip in between check and insert.
        self.check_commit(s, allocation_token, allocation_amount, fee_cc, traffic_cc)?;

        // Commit under the same resolved key the checks read.
        let allocation_token = Self::resolve_alias(&s.aliases, allocation_token).to_string();

        s.tokens
            .entry(allocation_token.clone())
            .or_insert_with(TokenState::new)
            .allocation_commitments
            .insert(proposal_id.to_string(), allocation_amount);

        s.fee_commitments
            .entries
            .insert(proposal_id.to_string(), fee_cc);
        if traffic_cc > Decimal::ZERO {
            s.traffic_holds.insert(proposal_id.to_string(), traffic_cc);
        }

        debug!(
            "[{}] Committed: {} {} + {:.4} CC fees + {:.4} CC traffic",
            proposal_id, allocation_amount, allocation_token, shown(fee_cc), shown(traffic_cc)
        );
        Ok(())
    }

    /// Release all commitments for a proposal (on any terminal state).
    pub async fn release(&self, proposal_id: &str) {
        let mut s = self.state.write().await;

        // Remove allocation commitment from whichever token it was in
        for token_state in s.tokens.values_mut() {
            token_state.allocation_commitments.remove(proposal_id);
        }

        // Remove fee commitment and traffic hold
        s.fee_commitments.entries.remove(proposal_id);
        s.traffic_holds.remove(proposal_id);

        debug!("[{}] Released liquidity commitments", proposal_id);
    }

    /// Reconciliation safety net: drop any commitments whose proposal_id is no
    /// longer in the live set.
    ///
    /// The authoritative set of in-flight settlements lives in the runner's
    /// `active_settlements`. Reservations are normally released on each
    /// settlement's terminal event, but if such an event is ever missed (e.g. a
    /// stream gap on reconnect, or a future code path that forgets to release)
    /// the per-proposal CC reservation would leak forever, decaying
    /// `available_cc` to 0 and starving the RFQ handler. Calling this from the
    /// runner heartbeat with the current live proposal_ids makes the reservation
    /// set self-heal every cycle. Idempotent.
    ///
    /// Returns the number of orphaned reservations dropped (for logging).
    pub async fn retain_commitments(&self, live_ids: &HashSet<String>) -> usize {
        let mut s = self.state.write().await;
        // RFQ V2 confirm-time commitments ("rfqv2:<quote_id>", held from
        // confirm until settle-observed/expiry) are NOT settlement proposals
        // and never appear in the runner's live set — they are released by the
        // V2 settle/sweep path, so the reconciler must not reap them.
        let keep = |pid: &String| live_ids.contains(pid) || pid.starts_with("rfqv2:");
        // Count the UNION of orphaned proposal_ids across both maps, so the
        // returned count (and the runner's warn!) can't silently undercount if
        // an allocation commitment ever lacks a matching fee entry or vice versa.
        let mut orphaned: HashSet<&String> = HashSet::new();
        for token_state in s.tokens.values() {
            orphaned.extend(
                token_state
                    .allocation_commitments
                    .keys()
                    .filter(|pid| !keep(*pid)),
            );
        }
        orphaned.extend(
            s.fee_commitments
                .entries
                .keys()
                .filter(|pid| !keep(*pid)),
        );
        orphaned.extend(s.traffic_holds.keys().filter(|pid| !keep(*pid)));
        let dropped = orphaned.len();
        for token_state in s.tokens.values_mut() {
            token_state
                .allocation_commitments
                .retain(|pid, _| keep(pid));
        }
        s.fee_commitments
            .entries
            .retain(|pid, _| keep(pid));
        s.traffic_holds.retain(|pid, _| keep(pid));
        if dropped > 0 {
            debug!("Reconciled liquidity commitments: dropped {} orphaned reservation(s)", dropped);
        }
        dropped
    }

    // -----------------------------------------------------------------------
    // Flow tracking
    // -----------------------------------------------------------------------

    /// Record an outflow (agent allocates/sells token in a settlement).
    pub async fn record_outflow(&self, token: &str, amount: f64) {
        let mut s = self.state.write().await;
        s.flows.entry(token.to_string()).or_insert_with(TokenFlow::new).pending_outflow += amount;
    }

    /// Record an inflow (agent receives token from a completed settlement).
    pub async fn record_inflow(&self, token: &str, amount: f64) {
        let mut s = self.state.write().await;
        s.flows.entry(token.to_string()).or_insert_with(TokenFlow::new).pending_inflow += amount;
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Available balance for a token after commitments.
    /// For CC, also subtracts the fee reserve and the wallet CC held fees need for top-up.
    /// Resolves aliases (e.g. "USDCx" → instrument_id).
    pub async fn available(&self, token: &str) -> Decimal {
        let s = self.state.read().await;
        let resolved = Self::resolve_alias(&s.aliases, token);
        let base = s.tokens.get(resolved).map_or(Decimal::ZERO, |t| t.available());
        if resolved == CC_TOKEN {
            self.cc_free(&s, base, Instant::now())
        } else {
            base
        }
    }

    /// Available CC after allocation commitments and the fee reserve.
    pub async fn available_cc(&self) -> Decimal {
        self.available(CC_TOKEN).await
    }

    /// Get the depletion coefficient for a token (0.0 to 10.0).
    ///
    /// Based on estimated hours to depletion:
    /// - > `depletion_max_hours` (default 12): 0.0
    /// - < `depletion_min_hours` (default 1):  10.0
    /// - between: linear interpolation
    pub async fn depletion_coefficient(&self, token: &str) -> f64 {
        let mut s = self.state.write().await;
        let resolved = Self::resolve_alias(&s.aliases, token).to_string();

        // Flush pending flow into EMA
        if let Some(flow) = s.flows.get_mut(resolved.as_str()) {
            flow.flush(self.ema_alpha);
        }

        let net_outflow = s.flows.get(resolved.as_str()).map_or(0.0, |f| f.net_outflow_per_hour);
        if net_outflow <= 0.0 {
            return 0.0; // not depleting
        }

        let available = s.tokens.get(resolved.as_str()).map_or(Decimal::ZERO, |t| t.available());
        // For CC, subtract the fee reserve
        let effective_available = if resolved == CC_TOKEN {
            self.cc_free(&s, available, Instant::now())
        } else {
            available
        };

        let avail_f64 = effective_available.to_f64().unwrap_or(0.0);
        if avail_f64 <= 0.0 {
            return 10.0; // already depleted
        }

        let hours = avail_f64 / net_outflow;
        self.hours_to_coefficient(hours)
    }

    /// Convert hours-to-depletion to a coefficient (0.0–10.0).
    fn hours_to_coefficient(&self, hours: f64) -> f64 {
        if hours >= self.depletion_max_hours {
            0.0
        } else if hours <= self.depletion_min_hours {
            10.0
        } else {
            10.0 * (self.depletion_max_hours - hours)
                / (self.depletion_max_hours - self.depletion_min_hours)
        }
    }

    // -----------------------------------------------------------------------
    // Observability
    // -----------------------------------------------------------------------

    /// Get stats for heartbeat logging.
    pub async fn stats(&self) -> Vec<TokenStats> {
        let mut s = self.state.write().await;

        // Flush all flows first (needs mutable borrow of flows only)
        for flow in s.flows.values_mut() {
            flow.flush(self.ema_alpha);
        }

        // Now collect stats (immutable borrows only)
        let now = Instant::now();
        let mut result = Vec::new();

        for (token, state) in &s.tokens {
            let committed = state.committed();
            let available = if token == CC_TOKEN {
                self.cc_free(&s, state.available(), now)
            } else {
                state.available()
            };

            let (net_outflow, hours_to_depletion, coeff) = if let Some(flow) = s.flows.get(token.as_str()) {
                let outflow = flow.net_outflow_per_hour;
                let hours = if outflow > 0.0 {
                    let avail = available.to_f64().unwrap_or(0.0);
                    if avail > 0.0 { avail / outflow } else { 0.0 }
                } else {
                    f64::INFINITY
                };
                (outflow, hours, self.hours_to_coefficient(hours))
            } else {
                (0.0, f64::INFINITY, 0.0)
            };

            result.push(TokenStats {
                token: token.clone(),
                balance: state.balance,
                committed,
                fee_reserve: if token == CC_TOKEN { self.fee_reserve_cc } else { Decimal::ZERO },
                available,
                num_commitments: state.allocation_commitments.len(),
                net_outflow_per_hour: net_outflow,
                hours_to_depletion,
                depletion_coefficient: coeff,
                refreshed_secs_ago: state
                    .last_refreshed
                    .map(|t| now.saturating_duration_since(t).as_secs()),
            });
        }

        result.sort_by(|a, b| stats_order(&a.token, &b.token));

        result
    }

    /// Prepaid credit and fee capacity for heartbeat logging.
    pub async fn prepaid_stats(&self) -> PrepaidStats {
        let s = self.state.read().await;
        let now = Instant::now();
        let capacity = self.capacity(&s, now, Decimal::ZERO);
        PrepaidStats {
            available: s
                .prepaid
                .polled
                .map(|(polled, _)| polled.saturating_sub(Self::local_debit_total(&s))),
            refreshed_secs_ago: s
                .prepaid
                .polled
                .map(|(_, at)| now.saturating_duration_since(at).as_secs()),
            stale: s.prepaid.polled.is_some() && capacity.is_none(),
            fee_committed: s.fee_commitments.total(),
            traffic_held: s.traffic_holds.values().fold(Decimal::ZERO, |acc, v| acc.saturating_add(*v)),
            capacity,
            auto_topup_cc: s.prepaid.topup_cc,
        }
    }

    // -----------------------------------------------------------------------
    // State persistence
    // -----------------------------------------------------------------------

    /// Export flow tracker state for persistence.
    pub async fn save_flows(&self) -> Vec<SavedTokenFlow> {
        let s = self.state.read().await;
        s.flows
            .iter()
            .map(|(token, flow)| SavedTokenFlow {
                token: token.clone(),
                net_outflow_per_hour: flow.net_outflow_per_hour,
            })
            .collect()
    }

    /// Restore flow tracker state from a previous session.
    pub async fn restore_flows(&self, flows: Vec<SavedTokenFlow>) {
        let mut s = self.state.write().await;
        for saved in flows {
            let flow = s.flows.entry(saved.token).or_insert_with(TokenFlow::new);
            flow.net_outflow_per_hour = saved.net_outflow_per_hour;
        }
    }
}

/// CC first, then alphabetical; a total order, as `sort_by` requires.
fn stats_order(a: &str, b: &str) -> std::cmp::Ordering {
    (a != CC_TOKEN).cmp(&(b != CC_TOKEN)).then_with(|| a.cmp(b))
}

/// Stats for a single token (returned by `LiquidityManager::stats()`).
pub struct TokenStats {
    pub token: String,
    pub balance: Decimal,
    pub committed: Decimal,
    pub fee_reserve: Decimal,
    pub available: Decimal,
    pub num_commitments: usize,
    pub net_outflow_per_hour: f64,
    pub hours_to_depletion: f64,
    pub depletion_coefficient: f64,
    /// Seconds since the balance was last refreshed; `None` if never.
    pub refreshed_secs_ago: Option<u64>,
}

impl std::fmt::Display for TokenStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let depl_str = if self.hours_to_depletion.is_infinite() {
            ">12h".to_string()
        } else {
            format!("{:.1}h", self.hours_to_depletion)
        };
        write!(
            f,
            "{} {:.2} bal / {:.2} committed / {:.2} avail ({} stlmts), flow {:.1}/hr, depl={:.1} ({})",
            self.token,
            self.balance,
            self.committed,
            self.available,
            self.num_commitments,
            self.net_outflow_per_hour,
            self.depletion_coefficient,
            depl_str,
        )
    }
}

/// Prepaid fee credit (returned by `LiquidityManager::prepaid_stats()`).
pub struct PrepaidStats {
    /// Polled credit less local debits; `None` until the first poll.
    pub available: Option<Decimal>,
    /// Seconds since the last successful poll was sent; `None` if never.
    pub refreshed_secs_ago: Option<u64>,
    /// The figure is too old for the fee checks.
    pub stale: bool,
    /// Fees committed to in-flight settlements.
    pub fee_committed: Decimal,
    /// Prepaid held for in-flight settlements' own transactions.
    pub traffic_held: Decimal,
    /// Fee capacity; `None` while the figure is unknown or stale.
    pub capacity: Option<Decimal>,
    /// CC moved per auto top-up, when enabled.
    pub auto_topup_cc: Option<Decimal>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_caps_at_the_u64_bounds() {
        assert_eq!(shown(Decimal::MAX), Decimal::from(u64::MAX));
        assert_eq!(shown(Decimal::MIN), -Decimal::from(u64::MAX));
        assert_eq!(shown(Decimal::new(-15, 1)), Decimal::new(-15, 1));
    }

    #[test]
    fn stats_order_is_total_with_cc_first() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(stats_order(CC_TOKEN, CC_TOKEN), Equal);
        assert_eq!(stats_order(CC_TOKEN, "AAA"), Less);
        assert_eq!(stats_order("AAA", CC_TOKEN), Greater);
        assert_eq!(stats_order("USDC", "CBTC"), Greater);
        let mut tokens = vec!["USDC", CC_TOKEN, "CBTC", "CC1"];
        tokens.sort_by(|a, b| stats_order(a, b));
        assert_eq!(tokens, [CC_TOKEN, "CBTC", "CC1", "USDC"]);
    }

    #[test]
    fn stats_display_survives_extreme_commitments() {
        let stats = TokenStats {
            token: CC_TOKEN.to_string(),
            balance: Decimal::MAX,
            committed: Decimal::MAX,
            fee_reserve: Decimal::ZERO,
            available: Decimal::MIN,
            num_commitments: 1,
            net_outflow_per_hour: 0.0,
            hours_to_depletion: f64::INFINITY,
            depletion_coefficient: 0.0,
            refreshed_secs_ago: None,
        };
        let shown = stats.to_string();
        assert!(shown.starts_with("CC 79228162514264337593543950335.00 bal"), "{shown}");
    }

    #[tokio::test]
    async fn stale_age_uses_the_configured_window() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        let s = lm.state.read().await;
        let later = Instant::now() + Duration::from_secs(200);
        assert!(
            LiquidityManager::stale_age(&s, "USDCx", Instant::now(), Duration::from_secs(120))
                .is_none()
        );
        assert!(
            LiquidityManager::stale_age(&s, "USDCx", later, Duration::from_secs(120)).is_some()
        );
        // Unknown token: nothing to report.
        assert!(LiquidityManager::stale_age(&s, "NOPE", later, Duration::from_secs(120)).is_none());
    }

    #[tokio::test]
    async fn batch_update_stamps_refresh() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_token_balances(&[
            ("USDCx".to_string(), Decimal::from(5000)),
            ("EDELx".to_string(), Decimal::from(100)),
        ])
        .await;
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
        assert_eq!(lm.available("EDELx").await, Decimal::from(100));
        assert!(lm.is_stale("USDCx").await.is_none());
        assert!(lm.stale_tokens().await.is_empty());
        let stats = lm.stats().await;
        assert!(stats.iter().all(|s| s.refreshed_secs_ago == Some(0)));

        lm.set_stale_after(Duration::ZERO);
        assert!(lm.is_stale("USDCx").await.is_some());
        assert_eq!(lm.stale_tokens().await.len(), 2);
    }

    #[tokio::test]
    async fn commit_under_alias_matches_canonical_key() {
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("internal-id", Decimal::from(5000)).await;
        lm.register_alias("SYM", "internal-id").await;
        lm.try_commit("p1", "SYM", Decimal::from(2000), Decimal::ZERO)
            .await
            .expect("alias-keyed commit");
        // The commitment must land on the canonical entry, visible both ways.
        assert_eq!(lm.available("internal-id").await, Decimal::from(3000));
        assert_eq!(lm.available("SYM").await, Decimal::from(3000));
        lm.release("p1").await;
        assert_eq!(lm.available("internal-id").await, Decimal::from(5000));
    }

    #[tokio::test]
    async fn missing_tokens_lose_refresh_stamp() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balances(&[
            ("A".to_string(), Decimal::from(10)),
            ("B".to_string(), Decimal::from(20)),
        ])
        .await;
        let present: HashSet<String> = ["A".to_string()].into_iter().collect();
        lm.mark_missing_unrefreshed(&present).await;
        lm.set_stale_after(Duration::ZERO);
        assert!(lm.is_stale("A").await.is_some(), "present token keeps its stamp");
        assert!(lm.is_stale("B").await.is_none(), "absent token has no stamp");
        assert_eq!(lm.available("B").await, Decimal::from(20), "balance is kept");
        assert!(lm.is_stale(CC_TOKEN).await.is_some(), "CC stamp untouched");
    }

    #[tokio::test]
    async fn unrefreshed_token_is_never_reported_stale() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        // A commit attempt on a token with no recorded balance must not
        // create a refresh time or a staleness verdict for it.
        let _ = lm
            .try_commit("p1", "USDCx", Decimal::from(1), Decimal::ZERO)
            .await;
        lm.set_stale_after(Duration::ZERO);
        assert!(lm.is_stale("USDCx").await.is_none());
        assert!(
            lm.stats()
                .await
                .iter()
                .all(|s| s.token != "USDCx" || s.refreshed_secs_ago.is_none())
        );
        // The stamped CC entry, by contrast, is reported once the window is zero.
        assert!(lm.is_stale(CC_TOKEN).await.is_some());
    }

    #[tokio::test]
    async fn test_commit_and_release() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);

        // Set balances
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        lm.update_prepaid(Decimal::from(100), Instant::now()).await;

        // Check initial availability
        // CC: 100 - 5 (reserve) = 95
        assert_eq!(lm.available_cc().await, Decimal::from(95));
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));

        // Commit: sell 50 CC + fees (~1 USD = 10 CC * 1.1 margin = 11 CC)
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert!(fee_cc > Decimal::ZERO);

        let result = lm.try_commit("p1", CC_TOKEN, Decimal::from(50), fee_cc).await;
        assert!(result.is_ok());

        // CC available should decrease
        let avail = lm.available_cc().await;
        assert!(avail < Decimal::from(95));

        // Commit: buy CC, allocate 2000 USDCx + fees
        let fee_cc2 = lm.estimate_fee_cc(Decimal::ONE).await;
        let result = lm.try_commit("p2", "USDCx", Decimal::from(2000), fee_cc2).await;
        assert!(result.is_ok());
        assert_eq!(lm.available("USDCx").await, Decimal::from(3000));

        // Release p1
        lm.release("p1").await;
        let avail_after = lm.available_cc().await;
        assert!(avail_after > avail);

        // Release p2
        lm.release("p2").await;
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
    }

    // can_commit must give the same verdict as try_commit on identical inputs,
    // without creating any commitment or mutating balances.
    #[tokio::test]
    async fn test_can_commit_parity_and_readonly() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;
        lm.update_prepaid(Decimal::from(100), Instant::now()).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;

        // Pass case: advisory says yes, commit then succeeds
        assert!(lm.can_commit("USDCx", Decimal::from(2000), fee_cc).await.is_ok());
        // Advisory made no reservation
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
        assert!(lm.try_commit("p1", "USDCx", Decimal::from(2000), fee_cc).await.is_ok());
        assert!(lm.has_commitment("p1").await);

        // Fail cases mirror try_commit: token shortfall...
        assert!(lm.can_commit("USDCx", Decimal::from(3001), fee_cc).await.is_err());
        assert!(lm.try_commit("px", "USDCx", Decimal::from(3001), fee_cc).await.is_err());
        // ...a CC allocation into the reserve...
        assert!(lm.can_commit(CC_TOKEN, Decimal::from(96), Decimal::ONE).await.is_err());
        assert!(lm.try_commit("py", CC_TOKEN, Decimal::from(96), Decimal::ONE).await.is_err());
        // ...and a fee beyond the remaining capacity
        assert!(lm.can_commit("USDCx", Decimal::ONE, Decimal::from(90)).await.is_err());
        assert!(lm.try_commit("pf", "USDCx", Decimal::ONE, Decimal::from(90)).await.is_err());
        assert!(!lm.has_commitment("pf").await);
        // Unknown token = zero balance on both paths
        assert!(lm.can_commit("NOPE", Decimal::ONE, Decimal::ZERO).await.is_err());
        assert!(lm.try_commit("pz", "NOPE", Decimal::ONE, Decimal::ZERO).await.is_err());

        // No stray commitments from any of the advisory/failed calls
        assert!(!lm.has_commitment("px").await);
        assert!(!lm.has_commitment("py").await);
        assert!(!lm.has_commitment("pz").await);
        assert_eq!(lm.available("USDCx").await, Decimal::from(3000)); // only p1 held
    }

    #[tokio::test]
    async fn test_reject_insufficient_balance() {
        let lm = LiquidityManager::new(5.0, 1.0, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(20)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        // Try to commit 20 CC (but reserve is 5, so only 15 available)
        let result = lm.try_commit("p1", CC_TOKEN, Decimal::from(20), Decimal::ZERO).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("insufficient CC"));
    }

    #[tokio::test]
    async fn test_reject_insufficient_fee_cc() {
        let lm = LiquidityManager::new(5.0, 1.0, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(20)).await;
        lm.update_token_balance("USDCx", Decimal::from(10000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        // Allocate USDCx (plenty available), but need 20 CC for fees (15 of prepaid credit)
        lm.update_prepaid(Decimal::from(15), Instant::now()).await;
        let result = lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::from(20)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("insufficient fee capacity"));
    }

    #[tokio::test]
    async fn test_depletion_coefficient() {
        let lm = LiquidityManager::new(0.0, 1.0, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;

        // No flow → coefficient = 0
        let coeff = lm.depletion_coefficient(CC_TOKEN).await;
        assert_eq!(coeff, 0.0);

        // Simulate outflow: we need to set net_outflow_per_hour directly
        // since EMA needs time to accumulate. Use record_outflow + manual flush.
        {
            let mut s = lm.state.write().await;
            let flow = s.flows.entry(CC_TOKEN.to_string()).or_insert_with(TokenFlow::new);
            // Directly set the EMA for testing
            flow.net_outflow_per_hour = 50.0; // depleting at 50 CC/hr
        }

        // 100 CC / 50 per hour = 2 hours → coefficient = 10 * (12 - 2) / 11 ≈ 9.09
        let coeff = lm.depletion_coefficient(CC_TOKEN).await;
        assert!((coeff - 9.09).abs() < 0.1);
    }

    #[tokio::test]
    async fn test_depletion_coefficient_boundaries() {
        let lm = LiquidityManager::new(0.0, 1.0, 4.0, 12.0, 1.0);
        assert_eq!(lm.hours_to_coefficient(15.0), 0.0);   // > 12h
        assert_eq!(lm.hours_to_coefficient(12.0), 0.0);   // exactly 12h
        assert_eq!(lm.hours_to_coefficient(1.0), 10.0);   // exactly 1h
        assert_eq!(lm.hours_to_coefficient(0.5), 10.0);   // < 1h
        assert!((lm.hours_to_coefficient(6.5) - 5.0).abs() < 0.01); // midpoint
    }

    #[tokio::test]
    async fn test_save_and_restore_flows() {
        let lm = LiquidityManager::new(0.0, 1.0, 4.0, 12.0, 1.0);

        // Set some flow state
        {
            let mut s = lm.state.write().await;
            let flow = s.flows.entry(CC_TOKEN.to_string()).or_insert_with(TokenFlow::new);
            flow.net_outflow_per_hour = 42.5;
        }

        // Save
        let saved = lm.save_flows().await;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].token, CC_TOKEN);
        assert_eq!(saved[0].net_outflow_per_hour, 42.5);

        // Restore into a new manager
        let lm2 = LiquidityManager::new(0.0, 1.0, 4.0, 12.0, 1.0);
        lm2.restore_flows(saved).await;

        let s = lm2.state.read().await;
        assert_eq!(s.flows.get(CC_TOKEN).unwrap().net_outflow_per_hour, 42.5);
    }

    #[tokio::test]
    async fn test_stats_output() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        lm.update_prepaid(Decimal::from(100), Instant::now()).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        lm.try_commit("p1", CC_TOKEN, Decimal::from(30), fee_cc).await.unwrap();

        let stats = lm.stats().await;
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].token, CC_TOKEN); // CC first
        assert!(stats[0].committed > Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_zero_rate_returns_zero_fee() {
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        // Rate is 0 (never fetched)
        let fee = lm.estimate_fee_cc(Decimal::ONE).await;
        assert_eq!(fee, Decimal::ZERO);
    }

    // Amounts at the edge of the Decimal range read as unaffordable instead of overflowing
    #[tokio::test]
    async fn test_out_of_range_fee_and_allocation_are_unaffordable() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.15").unwrap()).await;

        let fee_cc = lm.estimate_fee_cc(Decimal::MAX).await;
        assert_eq!(fee_cc, Decimal::MAX);
        let reason = lm.can_commit("USDCx", Decimal::ONE, fee_cc).await.unwrap_err();
        assert!(reason.contains("18446744073709551615.0000 needed"), "{reason}");
        assert!(lm.can_commit(CC_TOKEN, Decimal::MAX, Decimal::ZERO).await.is_err());
        // Intake refuses negative amounts; the CC arithmetic must not overflow on one either
        let _ = lm.can_commit(CC_TOKEN, Decimal::MIN, Decimal::ONE).await;
    }

    #[tokio::test]
    async fn test_retain_commitments_drops_orphaned() {
        // Reproduces the reservation leak: a proposal whose terminal event was
        // missed (so `release` was never called) keeps its CC reservation
        // forever, decaying available CC. `retain_commitments` reconciles the
        // reservation set against the live set and reclaims the orphan.
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert!(fee_cc > Decimal::ZERO);

        lm.update_prepaid(Decimal::from(100), Instant::now()).await;
        // Commit two proposals: p_live (still in flight) and p_orphan (its
        // terminal event will be "missed").
        lm.try_commit("p_live", "USDCx", Decimal::from(1000), fee_cc).await.unwrap();
        lm.try_commit("p_orphan", "USDCx", Decimal::from(2000), fee_cc).await.unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::from(2000)); // 5000 - 1000 - 2000
        let cc_both = lm.fee_capacity().await.unwrap();

        // Reconcile against the live set (only p_live remains active).
        let mut live = HashSet::new();
        live.insert("p_live".to_string());
        assert_eq!(lm.retain_commitments(&live).await, 1); // p_orphan reclaimed

        // p_orphan's allocation + fee reservation freed; p_live's retained.
        assert_eq!(lm.available("USDCx").await, Decimal::from(4000)); // 5000 - 1000
        assert!(lm.fee_capacity().await.unwrap() > cc_both);

        // Idempotent: re-running with the same live set drops nothing.
        assert_eq!(lm.retain_commitments(&live).await, 0);
        assert_eq!(lm.available("USDCx").await, Decimal::from(4000));

        // Empty live set → p_live is also reclaimed (full reset, as on reboot).
        assert_eq!(lm.retain_commitments(&HashSet::new()).await, 1);
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
    }

    #[tokio::test]
    async fn test_retain_commitments_keeps_rfqv2_keys() {
        // RFQ V2 confirm-time commitments ("rfqv2:<quote_id>") never appear in
        // the runner's live set (they are not settlement proposals) — the
        // reconciler must not reap them; the V2 settle/sweep path releases
        // them. Reaping here would silently un-reserve a signed quote's funds.
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;

        lm.try_commit("rfqv2:q1", "USDCx", Decimal::from(1500), Decimal::ZERO)
            .await
            .unwrap();
        assert_eq!(lm.available("USDCx").await, Decimal::from(3500));

        // Empty live set: an ordinary proposal would be reaped; the rfqv2 key
        // survives.
        assert_eq!(lm.retain_commitments(&HashSet::new()).await, 0);
        assert_eq!(lm.available("USDCx").await, Decimal::from(3500));

        // The V2 path's own release is what frees it.
        lm.release("rfqv2:q1").await;
        assert_eq!(lm.available("USDCx").await, Decimal::from(5000));
    }

    #[tokio::test]
    async fn a_negative_reported_balance_cannot_panic_the_manager() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_prepaid(Decimal::from(10), Instant::now()).await;
        lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::ZERO).await.unwrap();
        lm.try_commit("p2", CC_TOKEN, Decimal::from(10), Decimal::ONE).await.unwrap();
        lm.update_token_balance("USDCx", Decimal::MIN).await;
        lm.update_cc_balance(Decimal::MIN).await;

        let stats = lm.stats().await;
        assert_eq!(stats.len(), 2);
        assert!(stats.iter().all(|s| s.balance.is_zero() && s.available.is_zero()));
        assert!(lm.can_commit("USDCx", Decimal::ONE, Decimal::ZERO).await.is_err());
        assert_eq!(lm.available("USDCx").await, Decimal::ZERO);
        assert_eq!(lm.available_cc().await, Decimal::ZERO);
    }

    #[tokio::test]
    async fn out_of_range_totals_saturate_instead_of_panicking() {
        let mut t = TokenState::new();
        t.allocation_commitments.insert("a".into(), Decimal::MAX);
        t.allocation_commitments.insert("b".into(), Decimal::MAX);
        assert_eq!(t.committed(), Decimal::MAX);
        let mut t = TokenState::new();
        t.allocation_commitments.insert("a".into(), Decimal::ONE);
        t.balance = Decimal::MIN;
        assert_eq!(t.available(), Decimal::ZERO);
        let mut fees = FeeCommitments::new();
        fees.entries.insert("a".into(), Decimal::MIN);
        fees.entries.insert("b".into(), Decimal::MIN);
        assert_eq!(fees.total(), Decimal::MIN);

        // A negative fee total leaves CC inventory alone and adds no capacity
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.restore_flows(vec![SavedTokenFlow { token: CC_TOKEN.to_string(), net_outflow_per_hour: 1.0 }])
            .await;
        lm.state.write().await.fee_commitments.entries.insert("f".into(), Decimal::MIN);
        assert_eq!(lm.available_cc().await, Decimal::from(95));
        assert_eq!(lm.depletion_coefficient(CC_TOKEN).await, 0.0);
        let stats = lm.stats().await;
        assert_eq!(stats.len(), 1);
        assert!(stats.iter().all(|s| s.available == Decimal::from(95)));
        lm.update_prepaid(Decimal::from(10), Instant::now()).await;
        lm.set_auto_topup(Some(Decimal::from(25))).await;
        assert_eq!(lm.fee_capacity().await, Some(Decimal::from(85)));
        assert_eq!(lm.fee_capacity_for(CC_TOKEN, Decimal::MIN).await, Some(Decimal::MAX));
        lm.state.write().await.prepaid.polled = Some((Decimal::MIN, Instant::now()));
        lm.state.write().await.fee_commitments.entries.insert("f".into(), Decimal::MAX);
        assert_eq!(lm.fee_capacity().await, Some(Decimal::ZERO));
        lm.record_fee_paid("f", None, Some(Decimal::MAX)).await;
        assert_eq!(lm.prepaid_stats().await.capacity, Some(Decimal::ZERO));
    }

    // The top-up's demand: every pending fee and traffic hold together, never below zero
    #[tokio::test]
    async fn the_prepaid_demand_is_the_sum_of_every_hold() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        assert_eq!(lm.prepaid_demand().await, Decimal::ZERO);
        for (pid, fee) in [("a", 2), ("b", 7), ("c", 3)] {
            lm.state.write().await.fee_commitments.entries.insert(pid.into(), Decimal::from(fee));
        }
        lm.state.write().await.traffic_holds.insert("a".into(), Decimal::ONE);
        assert_eq!(lm.prepaid_demand().await, Decimal::from(13));
        let negative = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        negative.state.write().await.fee_commitments.entries.insert("a".into(), Decimal::from(-5));
        assert_eq!(negative.prepaid_demand().await, Decimal::ZERO);
    }

    // A fee too large for fixed-precision formatting is logged capped
    #[tokio::test]
    async fn committing_a_huge_fee_logs_without_panicking() {
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::DEBUG);
        let lm = LiquidityManager::new(0.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::MAX).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.0000000001").unwrap()).await;
        lm.update_token_balance("USDCx", Decimal::from(10)).await;
        lm.update_prepaid(Decimal::MAX, Instant::now()).await;
        let fee_cc = Decimal::from_scientific("1e27").unwrap();
        assert!(lm.try_commit("p", "USDCx", Decimal::ONE, fee_cc).await.is_ok());
        assert_eq!(logs.count("18446744073709551615.0000 CC fees"), 1);
    }

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn ago(secs: u64) -> Instant {
        Instant::now().checked_sub(Duration::from_secs(secs)).unwrap()
    }

    /// An agent holding no CC: 174 TUSDT, rate 0.075 (so $1.00 of fees is ~14.67 CC).
    async fn zero_cc_agent(prepaid: Option<Decimal>) -> Arc<LiquidityManager> {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::ZERO).await;
        lm.update_token_balance("TUSDT", Decimal::from(174)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        if let Some(p) = prepaid {
            lm.update_prepaid(p, Instant::now()).await;
        }
        lm
    }

    #[tokio::test]
    async fn a_zero_cc_agent_adopts_on_prepaid_credit() {
        let lm = zero_cc_agent(Some(dec("94"))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert!(fee_cc > dec("14.6") && fee_cc < dec("14.7"), "{fee_cc}");
        assert_eq!(lm.adoption_check("TUSDT", dec("25.38"), Decimal::ONE).await, Adoption::Accept);
        assert!(lm.can_commit("TUSDT", dec("25.38"), fee_cc).await.is_ok());
        lm.try_commit("p1", "TUSDT", dec("25.38"), fee_cc).await.unwrap();
        assert_eq!(lm.fee_capacity().await, Some(dec("94").saturating_sub(fee_cc)));
    }

    #[tokio::test]
    async fn too_little_prepaid_credit_rejects_for_fee_capacity() {
        let lm = zero_cc_agent(Some(dec("10"))).await;
        match lm.adoption_check("TUSDT", dec("25.38"), Decimal::ONE).await {
            Adoption::Reject(reason) => assert!(reason.contains("insufficient fee capacity"), "{reason}"),
            other => panic!("expected a reject, got {other:?}"),
        }
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert!(lm.try_commit("p1", "TUSDT", dec("25.38"), fee_cc).await.is_err());
        assert!(!lm.has_commitment("p1").await);
    }

    #[tokio::test]
    async fn unknown_or_stale_prepaid_or_rate_defers() {
        let lm = zero_cc_agent(None).await;
        assert!(matches!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Defer(_)));
        assert_eq!(lm.fee_capacity().await, None);
        // The reservation path refuses too, and records nothing.
        assert!(lm.try_commit("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await.is_err());
        assert!(!lm.has_commitment("p1").await);

        lm.update_prepaid(dec("94"), ago(PREPAID_STALE_AFTER.as_secs() + 1)).await;
        assert!(matches!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Defer(_)));
        lm.update_prepaid(dec("94"), Instant::now()).await;
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);

        let no_rate = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        no_rate.update_token_balance("TUSDT", Decimal::from(174)).await;
        no_rate.update_prepaid(dec("94"), Instant::now()).await;
        assert!(matches!(no_rate.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Defer(_)));
        // No fee: nothing to fund, so nothing to wait for.
        assert_eq!(no_rate.adoption_check("TUSDT", Decimal::ONE, Decimal::ZERO).await, Adoption::Accept);
        // A known shortfall of the allocated token still rejects.
        let short = zero_cc_agent(None).await;
        assert!(matches!(short.adoption_check("TUSDT", Decimal::from(175), Decimal::ONE).await, Adoption::Reject(_)));
    }

    #[tokio::test]
    async fn a_cc_seller_never_counts_its_allocated_cc_as_fee_credit() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(300)).await;
        lm.update_token_balance("TUSDT", Decimal::from(10)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        lm.update_prepaid(dec("2"), Instant::now()).await;
        lm.set_auto_topup(Some(Decimal::from(125))).await;
        // Selling 200 of 300 CC: 95 left above the reserve, less than one top-up.
        match lm.adoption_check(CC_TOKEN, Decimal::from(200), Decimal::ONE).await {
            Adoption::Reject(reason) => assert!(reason.contains("insufficient fee capacity"), "{reason}"),
            other => panic!("expected a reject, got {other:?}"),
        }
        assert_eq!(lm.fee_capacity_for(CC_TOKEN, Decimal::from(200)).await, Some(dec("2")));
        // Allocating another token, two top-ups of the wallet CC count.
        assert_eq!(lm.fee_capacity_for("TUSDT", Decimal::ONE).await, Some(dec("252")));
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);
        // CC already allocated to another settlement is not fee credit either.
        lm.try_commit("p0", CC_TOKEN, Decimal::from(100), Decimal::ZERO).await.unwrap();
        assert_eq!(lm.fee_capacity().await, Some(dec("127")));
    }

    #[tokio::test]
    async fn wallet_cc_counts_only_with_auto_topup() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(1000)).await;
        lm.update_token_balance("TUSDT", Decimal::from(100)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        lm.update_prepaid(dec("2"), Instant::now()).await;
        assert_eq!(lm.fee_capacity().await, Some(dec("2")));
        assert!(matches!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Reject(_)));
        lm.set_auto_topup(Some(Decimal::from(125))).await;
        assert_eq!(lm.fee_capacity().await, Some(dec("877")));
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);
        lm.set_auto_topup(None).await;
        assert_eq!(lm.fee_capacity().await, Some(dec("2")));
    }

    #[tokio::test]
    async fn a_paid_fee_is_debited_locally_until_the_next_poll() {
        let lm = zero_cc_agent(Some(dec("94"))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        lm.try_commit("p1", "TUSDT", Decimal::ONE, fee_cc).await.unwrap();
        let held = lm.fee_capacity().await.unwrap();
        // A poll sent before the debit does not undo it.
        let before = Instant::now();
        lm.record_fee_paid("p1", None, Some(dec("0.3"))).await;
        lm.record_fee_paid("p1", None, Some(dec("0.7"))).await;
        // Paying moves the fee from the commitment to the prepaid figure: no double count.
        assert!(held.saturating_sub(lm.fee_capacity().await.unwrap()).abs() < dec("0.0001"));
        lm.release("p1").await;
        let after_release = lm.fee_capacity().await.unwrap();
        assert!(after_release < dec("94"), "{after_release}");
        assert!(dec("94").saturating_sub(fee_cc).saturating_sub(after_release).abs() < dec("0.0001"));
        lm.update_prepaid(dec("94"), before).await;
        assert_eq!(lm.fee_capacity().await, Some(after_release));
        // A poll sent after the debit carries it.
        lm.update_prepaid(dec("79.33"), Instant::now()).await;
        assert_eq!(lm.fee_capacity().await, Some(dec("79.33")));
        // Without a rate, the proposal's own commitment is the debit.
        lm.try_commit("p2", "TUSDT", Decimal::ONE, Decimal::from(10)).await.unwrap();
        lm.update_cc_usd_rate(Decimal::ZERO).await;
        lm.record_fee_paid("p2", None, Some(dec("0.3"))).await;
        lm.release("p2").await;
        assert_eq!(lm.fee_capacity().await, Some(dec("69.33")));
    }

    #[tokio::test]
    async fn fee_commitments_no_longer_reduce_cc_inventory() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(dec("0.10")).await;
        lm.update_prepaid(Decimal::from(100), Instant::now()).await;
        lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::from(11)).await.unwrap();
        assert_eq!(lm.available_cc().await, Decimal::from(95));
        assert_eq!(lm.available(CC_TOKEN).await, Decimal::from(95));
        let stats = lm.stats().await;
        let cc = stats.iter().find(|s| s.token == CC_TOKEN).unwrap();
        assert_eq!(cc.available, Decimal::from(95));
        assert_eq!(lm.prepaid_stats().await.fee_committed, Decimal::from(11));
        assert_eq!(lm.fee_capacity().await, Some(Decimal::from(89)));
        lm.state.write().await.flows.entry(CC_TOKEN.to_string()).or_insert_with(TokenFlow::new).net_outflow_per_hour = 19.0;
        // 95 CC at 19 CC/h is 5 h: 10 * (12 - 5) / 11
        assert!((lm.depletion_coefficient(CC_TOKEN).await - 6.36).abs() < 0.01);
    }

    #[tokio::test]
    async fn readiness_needs_no_cc_entry_once_prepaid_is_known() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_token_balance("TUSDT", Decimal::from(174)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        assert!(!lm.is_ready().await);
        lm.update_prepaid(dec("94"), Instant::now()).await;
        assert!(lm.is_ready().await);
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);
        // A CC allocation still waits for the CC balance.
        assert!(matches!(lm.adoption_check(CC_TOKEN, Decimal::ONE, Decimal::ONE).await, Adoption::Defer(_)));
    }

    /// 300 CC, 100 TUSDT, prepaid 10, auto top-up 125: a $1.00 fee (~14.67 CC) fits only with the wallet term.
    async fn fee_on_wallet_term() -> Arc<LiquidityManager> {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_cc_balance(Decimal::from(300)).await;
        lm.update_token_balance("TUSDT", Decimal::from(100)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        lm.update_prepaid(dec("10"), Instant::now()).await;
        lm.set_auto_topup(Some(Decimal::from(125))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        lm.try_commit("p1", "TUSDT", Decimal::ONE, fee_cc).await.unwrap();
        lm
    }

    #[tokio::test]
    async fn a_zero_fee_cc_payout_cannot_take_the_cc_a_pending_fee_needs() {
        let lm = fee_on_wallet_term().await;
        // 200 CC out leaves 95 above the reserve: no whole top-up, so p1's fee is unfunded
        assert!(lm.can_commit(CC_TOKEN, Decimal::from(200), Decimal::ZERO).await.is_err());
        assert!(lm.try_commit("rfqv2:q1", CC_TOKEN, Decimal::from(200), Decimal::ZERO).await.is_err());
        assert!(!lm.has_commitment("rfqv2:q1").await);
        // Advisory sizing keeps one top-up back: 300 - 5 - 125
        assert_eq!(lm.available(CC_TOKEN).await, Decimal::from(170));
        // 160 CC out leaves 135, one top-up: 10 + 125 still covers the fee
        lm.try_commit("rfqv2:q2", CC_TOKEN, Decimal::from(160), Decimal::ZERO).await.unwrap();
        // Prepaid alone covering the fee needs no wallet CC
        lm.update_prepaid(dec("50"), Instant::now()).await;
        assert_eq!(lm.available(CC_TOKEN).await, Decimal::from(135));
        // Without fee commitments a zero-fee CC commit is bounded by the reserve only
        let free = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        free.update_cc_balance(Decimal::from(300)).await;
        free.set_auto_topup(Some(Decimal::from(125))).await;
        free.try_commit("rfqv2:q3", CC_TOKEN, Decimal::from(295), Decimal::ZERO).await.unwrap();
        assert!(free.try_commit("rfqv2:q4", CC_TOKEN, Decimal::ONE, Decimal::ZERO).await.is_err());
    }

    #[tokio::test]
    async fn auto_topup_with_the_cc_balance_not_loaded_defers() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);
        lm.update_token_balance("TUSDT", Decimal::from(174)).await;
        lm.update_cc_usd_rate(dec("0.075")).await;
        lm.update_prepaid(dec("40"), Instant::now()).await;
        lm.set_auto_topup(Some(Decimal::from(125))).await;
        // $3.00 is 44 CC: prepaid alone is short and the wallet CC is not known yet
        let held = Adoption::Defer("Balances not loaded yet".to_string());
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::from(3)).await, held);
        let fee_cc = lm.estimate_fee_cc(Decimal::from(3)).await;
        let err = lm.try_commit("p1", "TUSDT", Decimal::ONE, fee_cc).await.unwrap_err();
        assert!(err.contains("fee capacity unknown"), "{err}");
        // Prepaid alone covering the fee needs no wallet figure
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, dec("0.5")).await, Adoption::Accept);
        lm.update_cc_balance(Decimal::from(2000)).await;
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::from(3)).await, Adoption::Accept);
    }

    #[tokio::test]
    async fn each_settlement_holds_prepaid_for_its_own_transactions() {
        let traffic = DEFAULT_SETTLEMENT_TRAFFIC_CC;
        let lm = zero_cc_agent(Some(dec("16"))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        // 14.67 CC of fees and 1.2 CC of traffic fit in 16
        assert_eq!(lm.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);
        assert_eq!(lm.try_commit_settlement("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await, Ok(fee_cc));
        assert_eq!(lm.prepaid_demand().await, fee_cc + traffic);
        assert_eq!(lm.fee_capacity().await, Some(dec("16") - fee_cc - traffic));
        // A debit without a rate takes the fee commitment, never the traffic hold
        lm.update_cc_usd_rate(Decimal::ZERO).await;
        lm.record_fee_paid("p1", None, None).await;
        assert_eq!(lm.prepaid_demand().await, traffic);
        lm.release("p1").await;
        assert_eq!(lm.prepaid_demand().await, Decimal::ZERO);
        assert_eq!(lm.fee_capacity().await, Some(dec("16") - fee_cc));

        // 15.5 covers the fee but not its traffic
        let short = zero_cc_agent(Some(dec("15.5"))).await;
        assert!(matches!(short.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Reject(_)));
        assert!(short.try_commit_settlement("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await.is_err());
        // A quote this agent runs no transactions for, and a plain commit, hold no traffic
        assert_eq!(short.quote_check("TUSDT", Decimal::ONE).await, Adoption::Accept);
        short.try_commit("rfqv2:q", "TUSDT", Decimal::ONE, fee_cc).await.unwrap();
        assert_eq!(short.prepaid_demand().await, fee_cc);

        // The hold is configurable, and a zero-fee settlement still holds it
        let tuned = zero_cc_agent(Some(dec("15.5"))).await;
        tuned.set_settlement_traffic(dec("0.5")).await;
        assert_eq!(tuned.adoption_check("TUSDT", Decimal::ONE, Decimal::ONE).await, Adoption::Accept);
        tuned.try_commit_settlement("p2", "TUSDT", Decimal::ONE, Decimal::ZERO).await.unwrap();
        assert_eq!(tuned.prepaid_demand().await, dec("0.5"));
        assert_eq!(tuned.retain_commitments(&HashSet::new()).await, 1);
        assert_eq!(tuned.prepaid_demand().await, Decimal::ZERO);
        let unknown = zero_cc_agent(None).await;
        assert!(matches!(unknown.adoption_check("TUSDT", Decimal::ONE, Decimal::ZERO).await, Adoption::Defer(_)));
    }

    #[tokio::test]
    async fn the_stated_debit_replaces_the_estimate() {
        let lm = zero_cc_agent(Some(dec("94"))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        lm.try_commit("p1", "TUSDT", Decimal::ONE, fee_cc).await.unwrap();
        // $0.30 at 0.075 was debited as 4 CC; the estimate with its margin is 4.4
        lm.record_fee_paid("p1", Some(dec("4")), Some(dec("0.3"))).await;
        assert_eq!(lm.prepaid_stats().await.available, Some(dec("90")));
        assert_eq!(lm.prepaid_demand().await, fee_cc - dec("4"));
        // Without a stated amount the estimate stands in; a stated zero debits nothing
        lm.record_fee_paid("p1", None, Some(dec("0.3"))).await;
        let avail = lm.prepaid_stats().await.available.unwrap();
        assert!((avail - dec("85.6")).abs() < dec("0.0001"), "{avail}");
        lm.record_fee_paid("p1", Some(Decimal::ZERO), Some(dec("0.3"))).await;
        assert_eq!(lm.prepaid_stats().await.available, Some(avail));
    }

    #[tokio::test]
    async fn one_fill_needs_the_last_fee_seen_in_its_market_and_its_traffic() {
        let traffic = DEFAULT_SETTLEMENT_TRAFFIC_CC;
        let lm = zero_cc_agent(Some(dec("94"))).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert_eq!(lm.fill_needs().await, (fee_cc + traffic, Vec::new()));
        // $2.00 per side seen on CC-USDCx, $0.10 on a discounted market
        lm.note_fill_fee("CC-USDCx", Decimal::from(2)).await;
        lm.note_fill_fee("CC-USDCx", Decimal::from(2)).await;
        lm.note_fill_fee("cETH-TUSDT", dec("0.1")).await;
        lm.note_fill_fee("", Decimal::from(5)).await;
        lm.note_fill_fee("X-Y", Decimal::from(-1)).await;
        let (default, mut by_market) = lm.fill_needs().await;
        by_market.sort();
        let two = lm.estimate_fee_cc(Decimal::from(2)).await;
        let tenth = lm.estimate_fee_cc(dec("0.1")).await;
        assert_eq!(default, fee_cc + traffic);
        assert_eq!(by_market, vec![("CC-USDCx".to_string(), two + traffic), ("cETH-TUSDT".to_string(), tenth + traffic)]);
        lm.update_cc_usd_rate(Decimal::ZERO).await;
        let (default, by_market) = lm.fill_needs().await;
        assert_eq!(default, traffic);
        assert!(by_market.iter().all(|(_, need)| *need == traffic));
        // Bounded: a new market past the cap is not kept, a known one still updates
        for i in 0..MAX_FILL_FEE_MARKETS {
            lm.note_fill_fee(&format!("M{i}-USDCx"), Decimal::ONE).await;
        }
        lm.note_fill_fee("CC-USDCx", Decimal::from(3)).await;
        lm.note_fill_fee("NEW-USDCx", Decimal::ONE).await;
        let s = lm.state.read().await;
        assert_eq!(s.fill_fees_usd.len(), MAX_FILL_FEE_MARKETS);
        assert_eq!(s.fill_fees_usd.get("CC-USDCx"), Some(&Decimal::from(3)));
        assert!(!s.fill_fees_usd.contains_key("NEW-USDCx"));
    }

    // The rate is read under the commit's lock: unknown with fees owed, nothing is held
    #[tokio::test]
    async fn a_settlement_commit_without_a_rate_waits_instead_of_holding_no_fee() {
        let traffic = DEFAULT_SETTLEMENT_TRAFFIC_CC;
        let lm = zero_cc_agent(Some(dec("94"))).await;
        lm.update_cc_usd_rate(Decimal::ZERO).await;
        let err = lm.try_commit_settlement("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await.unwrap_err();
        assert!(err.contains(FEE_CAPACITY_UNKNOWN), "{err}");
        assert!(!lm.has_commitment("p1").await);
        assert_eq!(lm.prepaid_demand().await, Decimal::ZERO);
        // Nothing owed: only the traffic is held
        assert_eq!(lm.try_commit_settlement("p2", "TUSDT", Decimal::ONE, Decimal::ZERO).await, Ok(Decimal::ZERO));
        assert_eq!(lm.prepaid_demand().await, traffic);
        lm.update_cc_usd_rate(dec("0.075")).await;
        let fee_cc = lm.estimate_fee_cc(Decimal::ONE).await;
        assert_eq!(lm.try_commit_settlement("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await, Ok(fee_cc));
        assert_eq!(lm.prepaid_demand().await, fee_cc + traffic + traffic);
    }

    // Stated debits carry no margin: after the last fee leg the rest is not demand; the traffic hold stays
    #[tokio::test]
    async fn clearing_the_fee_commitment_keeps_the_traffic_hold() {
        let traffic = DEFAULT_SETTLEMENT_TRAFFIC_CC;
        let lm = zero_cc_agent(Some(dec("94"))).await;
        lm.try_commit_settlement("p1", "TUSDT", Decimal::ONE, Decimal::ONE).await.unwrap();
        // $1.00 at 0.075 is 13.33 CC debited; the commitment held it with the 1.1 margin
        lm.record_fee_paid("p1", Some(dec("4")), None).await;
        lm.record_fee_paid("p1", Some(dec("9.3333")), None).await;
        assert!(lm.prepaid_demand().await > traffic + Decimal::ONE);
        lm.clear_fee_commitment("p1").await;
        assert_eq!(lm.prepaid_demand().await, traffic);
        assert!(lm.has_commitment("p1").await);
        assert_eq!(lm.fee_capacity().await, Some(dec("94") - dec("13.3333") - traffic));
        // Unknown proposals are left alone
        lm.clear_fee_commitment("nope").await;
        assert!(!lm.has_commitment("nope").await);
        lm.release("p1").await;
        assert_eq!(lm.prepaid_demand().await, Decimal::ZERO);
    }
}
