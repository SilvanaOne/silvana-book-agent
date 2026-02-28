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
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::debug;

// ---------------------------------------------------------------------------
// Configuration constants
// ---------------------------------------------------------------------------

/// CC token key used in the tokens map (distinct from instrument_id).
pub const CC_TOKEN: &str = "CC";

// ---------------------------------------------------------------------------
// Per-token state
// ---------------------------------------------------------------------------

/// Balance and commitment state for a single token.
struct TokenState {
    /// Current selectable (CC) or unlocked (non-CC) balance.
    balance: Decimal,
    /// CC committed to this token's allocations, keyed by proposal_id.
    allocation_commitments: HashMap<String, Decimal>,
}

impl TokenState {
    fn new() -> Self {
        Self {
            balance: Decimal::ZERO,
            allocation_commitments: HashMap::new(),
        }
    }

    fn committed(&self) -> Decimal {
        self.allocation_commitments.values().copied().sum()
    }

    fn available(&self) -> Decimal {
        (self.balance - self.committed()).max(Decimal::ZERO)
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
        self.entries.values().copied().sum()
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
    /// CC committed for fees (separate from CC allocation commitments).
    fee_commitments: FeeCommitments,
    /// Cached CC/USD exchange rate (for fee estimation).
    cc_usd_rate: Decimal,
    /// Alias map: short name → canonical key (e.g. "USDCx" → instrument_id).
    aliases: HashMap<String, String>,
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
            }),
            fee_reserve_cc: Decimal::from_f64_retain(fee_reserve_cc).unwrap_or(Decimal::ZERO),
            margin,
            ema_alpha: alpha,
            depletion_max_hours,
            depletion_min_hours,
        })
    }

    // -----------------------------------------------------------------------
    // Balance updates (called by ACS worker / runner balance refresh)
    // -----------------------------------------------------------------------

    /// Update CC balance from AmuletCache selectable total.
    pub async fn update_cc_balance(&self, selectable_cc: Decimal) {
        let mut s = self.state.write().await;
        s.tokens.entry(CC_TOKEN.to_string()).or_insert_with(TokenState::new).balance = selectable_cc;
    }

    /// Update a non-CC token balance from GetBalances RPC.
    pub async fn update_token_balance(&self, instrument_id: &str, unlocked: Decimal) {
        let mut s = self.state.write().await;
        s.tokens.entry(instrument_id.to_string()).or_insert_with(TokenState::new).balance = unlocked;
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

    // -----------------------------------------------------------------------
    // Commitment management
    // -----------------------------------------------------------------------

    /// Estimate CC needed for a settlement's fees (one side only).
    ///
    /// `my_fees_usd` is the sum of dvp + allocation fee for this agent's side.
    /// Returns `my_fees_usd / cc_usd_rate * margin`, or `Decimal::ZERO` if rate unknown.
    pub async fn estimate_fee_cc(&self, my_fees_usd: Decimal) -> Decimal {
        let s = self.state.read().await;
        if s.cc_usd_rate <= Decimal::ZERO || my_fees_usd <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        let raw = my_fees_usd / s.cc_usd_rate;
        let margin = Decimal::from_f64_retain(self.margin).unwrap_or(Decimal::ONE);
        raw * margin
    }

    /// Try to commit resources for a settlement.
    ///
    /// Checks **both** the allocation token balance **and** CC for fees.
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
        let mut s = self.state.write().await;

        // --- Check allocation token availability ---
        let token_state = s.tokens.entry(allocation_token.to_string()).or_insert_with(TokenState::new);
        let token_available = token_state.available();

        if token_available < allocation_amount {
            return Err(format!(
                "insufficient {} ({:.4} available, {:.4} needed for allocation)",
                allocation_token, token_available, allocation_amount
            ));
        }

        // --- Check CC for fees (may overlap if allocation_token == CC) ---
        let cc_state = s.tokens.entry(CC_TOKEN.to_string()).or_insert_with(TokenState::new);
        let cc_available_for_fees = cc_state.available() - self.fee_reserve_cc - s.fee_commitments.total();
        // If allocation_token is CC, the allocation also consumes CC
        let cc_after_alloc = if allocation_token == CC_TOKEN {
            cc_available_for_fees - allocation_amount
        } else {
            cc_available_for_fees
        };

        if cc_after_alloc < fee_cc {
            return Err(format!(
                "insufficient CC for fees ({:.4} available after allocation + reserve, {:.4} needed)",
                cc_after_alloc.max(Decimal::ZERO),
                fee_cc
            ));
        }

        // --- Commit ---
        // Re-borrow token_state (may be same as cc_state if CC)
        s.tokens
            .entry(allocation_token.to_string())
            .or_insert_with(TokenState::new)
            .allocation_commitments
            .insert(proposal_id.to_string(), allocation_amount);

        s.fee_commitments
            .entries
            .insert(proposal_id.to_string(), fee_cc);

        debug!(
            "[{}] Committed: {} {} + {:.4} CC fees",
            proposal_id, allocation_amount, allocation_token, fee_cc
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

        // Remove fee commitment
        s.fee_commitments.entries.remove(proposal_id);

        debug!("[{}] Released liquidity commitments", proposal_id);
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
    /// For CC, also subtracts fee_reserve and fee commitments.
    /// Resolves aliases (e.g. "USDCx" → instrument_id).
    pub async fn available(&self, token: &str) -> Decimal {
        let s = self.state.read().await;
        let resolved = Self::resolve_alias(&s.aliases, token);
        let base = s.tokens.get(resolved).map_or(Decimal::ZERO, |t| t.available());
        if resolved == CC_TOKEN {
            (base - self.fee_reserve_cc - s.fee_commitments.total()).max(Decimal::ZERO)
        } else {
            base
        }
    }

    /// Available CC after all commitments and fee reserve.
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
        // For CC, subtract fee reserve and fee commitments
        let effective_available = if resolved == CC_TOKEN {
            (available - self.fee_reserve_cc - s.fee_commitments.total()).max(Decimal::ZERO)
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
        let fee_total = s.fee_commitments.total();
        let mut result = Vec::new();

        for (token, state) in &s.tokens {
            let committed = state.committed();
            let fee_committed = if token == CC_TOKEN { fee_total } else { Decimal::ZERO };
            let available = if token == CC_TOKEN {
                (state.available() - self.fee_reserve_cc - fee_total).max(Decimal::ZERO)
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
                fee_committed,
                available,
                num_commitments: state.allocation_commitments.len(),
                net_outflow_per_hour: net_outflow,
                hours_to_depletion,
                depletion_coefficient: coeff,
            });
        }

        // Sort: CC first, then alphabetical
        result.sort_by(|a, b| {
            if a.token == CC_TOKEN { std::cmp::Ordering::Less }
            else if b.token == CC_TOKEN { std::cmp::Ordering::Greater }
            else { a.token.cmp(&b.token) }
        });

        result
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

/// Stats for a single token (returned by `LiquidityManager::stats()`).
pub struct TokenStats {
    pub token: String,
    pub balance: Decimal,
    pub committed: Decimal,
    pub fee_committed: Decimal,
    pub available: Decimal,
    pub num_commitments: usize,
    pub net_outflow_per_hour: f64,
    pub hours_to_depletion: f64,
    pub depletion_coefficient: f64,
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
            self.committed + self.fee_committed,
            self.available,
            self.num_commitments,
            self.net_outflow_per_hour,
            self.depletion_coefficient,
            depl_str,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_commit_and_release() {
        let lm = LiquidityManager::new(5.0, 1.1, 4.0, 12.0, 1.0);

        // Set balances
        lm.update_cc_balance(Decimal::from(100)).await;
        lm.update_token_balance("USDCx", Decimal::from(5000)).await;
        lm.update_cc_usd_rate(Decimal::from_str("0.10").unwrap()).await;

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

        // Allocate USDCx (plenty available), but need 20 CC for fees (only 15 available after reserve)
        let result = lm.try_commit("p1", "USDCx", Decimal::from(1000), Decimal::from(20)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("insufficient CC for fees"));
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
}
