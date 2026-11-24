//! State persistence for graceful shutdown and restart
//!
//! On Ctrl-C, the agent saves verification/dedup state to a JSON file.
//! On restart, it restores this state so settlements can resume without
//! re-verification or dedup failures.
//!
//! The server tracks full settlement status — we only persist the
//! agent-side state needed for verification and deduplication.

use anyhow::{anyhow, Result};
use orderbook_proto::orderbook::SettlementProposal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::error::Category;
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;
use tracing::{error, info, warn};

use crate::clock;
use crate::liquidity::SavedTokenFlow;
use crate::strategy::Start;

/// Current state file format version
const STATE_VERSION: u32 = 1;

/// Complete saved state for the agent
#[derive(Serialize, Deserialize)]
pub struct SavedState {
    /// Format version for future migration
    pub version: u32,
    /// ISO 8601 timestamp when state was saved
    pub saved_at: String,
    /// Party ID for validation on restore
    pub party_id: String,
    /// Immutable start time — orders with nonce <= this are rejected
    pub start_time_ms: u64,
    /// Proposal IDs that completed successfully (dedup set)
    pub completed_proposals: Vec<String>,
    /// Proposal IDs that were rejected (dedup set)
    pub rejected_proposals: Vec<String>,
    /// Tracked orders for settlement verification
    pub orders: Vec<SavedTrackedOrder>,
    /// Active settlement-to-order mappings for quantity accounting
    pub settlement_orders: Vec<SavedSettlementOrder>,
    /// Buyer RFQ trades awaiting settlement verification
    pub accepted_rfq_trades: Vec<SavedAcceptedRfqTrade>,
    /// LP quoted trades awaiting settlement verification
    pub quoted_rfq_trades: Vec<SavedQuotedTrade>,
    /// Fill loop state (buyer/seller commands only)
    pub fill_state: Option<SavedFillState>,
    /// Flow tracker state for depletion detection (restored on restart)
    #[serde(default)]
    pub flow_tracker: Vec<SavedTokenFlow>,
    /// RFQ V2: SettlementTicket pool state (blobs NOT persisted — re-fetched
    /// from the ACS after restart)
    #[serde(default)]
    pub atomic_tickets: Vec<SavedTicket>,
    /// RFQ V2: Confirmed quotes still inside their signed validity window —
    /// their holdings/ticket reservations must be restored BEFORE the first
    /// ACS refresh so a restarted LP never double-discloses them
    #[serde(default)]
    pub pending_v2_quotes: Vec<SavedPendingV2>,
    /// Strategy start balances, reused by a restart within `STRATEGY_START_MAX_AGE_MS`
    #[serde(default)]
    pub strategy_starts: Vec<SavedStrategyStart>,
}

/// Saved strategy start balances older than this are taken again at startup.
pub(crate) const STRATEGY_START_MAX_AGE_MS: u64 = 6 * 3600 * 1000;

/// A strategy market's balances and reference price when the strategy started
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedStrategyStart {
    pub market_id: String,
    /// The strategy kind they were taken for
    pub kind: String,
    /// Decimal strings
    pub base: String,
    pub quote: String,
    pub price: String,
    /// Base and quote held at the save, as decimal strings; absent in older files
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_at_save: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_at_save: Option<String>,
    /// When they were saved, in ms since the epoch
    pub saved_at_ms: u64,
}

/// Base and quote a strategy market holds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Held {
    pub base: f64,
    pub quote: f64,
}

/// Why saved start balances are not used.
#[derive(Debug, PartialEq)]
enum Unused {
    Expired,
    Unreadable,
}

/// An amount to 12 places, so the string always reads back; None outside the decimal range.
fn amount_string(v: f64) -> Option<String> {
    Decimal::from_f64(v).map(|d| d.round_dp(12).normalize().to_string())
}

/// A price to 15 significant digits, so a tiny one keeps its digits.
fn price_string(v: f64) -> Option<String> {
    Decimal::from_f64(v).and_then(|d| d.round_sf(15)).map(|d| d.normalize().to_string())
}

/// `now - then`, or 0 within float noise of the larger.
fn moved(now: f64, then: f64) -> f64 {
    let d = now - then;
    if d.abs() > 1e-9 * now.abs().max(then.abs()).max(1.0) {
        d
    } else {
        0.0
    }
}

impl SavedStrategyStart {
    /// None unless every value is finite and within the decimal range.
    pub(crate) fn new(market_id: &str, kind: &str, start: Start, held: Held, saved_at_ms: u64) -> Option<Self> {
        Some(Self {
            market_id: market_id.to_string(),
            kind: kind.to_string(),
            base: amount_string(start.base)?,
            quote: amount_string(start.quote)?,
            price: price_string(start.price)?,
            base_at_save: Some(amount_string(held.base)?),
            quote_at_save: Some(amount_string(held.quote)?),
            saved_at_ms,
        })
    }

    /// The balances, if saved no longer ago than the limit and readable.
    pub(crate) fn start_at(&self, now_ms: u64) -> Option<Start> {
        self.read(now_ms).ok().map(|(start, _)| start)
    }

    /// The start and the balances held at the save (None in an older file).
    fn read(&self, now_ms: u64) -> Result<(Start, Option<Held>), Unused> {
        if now_ms.saturating_sub(self.saved_at_ms) > STRATEGY_START_MAX_AGE_MS {
            return Err(Unused::Expired);
        }
        self.parse().ok_or(Unused::Unreadable)
    }

    fn parse(&self) -> Option<(Start, Option<Held>)> {
        let value = |s: &str| Decimal::from_str_exact(s).ok().and_then(|d| d.to_f64()).filter(|v| v.is_finite());
        let start = Start { base: value(&self.base)?, quote: value(&self.quote)?, price: value(&self.price)? };
        let held = match (self.base_at_save.as_deref(), self.quote_at_save.as_deref()) {
            (None, None) => None,
            (Some(base), Some(quote)) => Some(Held { base: value(base)?, quote: value(quote)? }),
            _ => return None,
        };
        Some((start, held))
    }
}

/// A start in use, and the balances its strategy last held.
#[derive(Debug)]
struct LiveStart {
    kind: String,
    start: Start,
    held: Held,
}

/// Strategy start balances in use by this process, and restored ones not yet taken.
#[derive(Debug, Default)]
pub(crate) struct StrategyStarts {
    live: BTreeMap<String, LiveStart>,
    restored: BTreeMap<String, SavedStrategyStart>,
}

impl StrategyStarts {
    /// Keeps the saved start balances still usable at `now_ms`; an unreadable one is logged.
    pub(crate) fn restore(&mut self, saved: &[SavedStrategyStart], now_ms: u64) {
        self.restored.clear();
        for s in saved {
            let (market, kind) = (&s.market_id, &s.kind);
            match s.read(now_ms) {
                Ok(_) => {
                    self.restored.insert(market.clone(), s.clone());
                }
                Err(Unused::Expired) => info!("Strategy {} [{}]: saved start balances older than 6 h", market, kind),
                Err(Unused::Unreadable) => {
                    warn!("Strategy {} [{}]: saved start balances unreadable; taken again", market, kind)
                }
            }
        }
    }

    /// A market's restored start, once, if taken for the same kind, moved by what came in or
    /// went out while stopped (`held` against the balances at the save); also returns that move.
    pub(crate) fn take(&mut self, market_id: &str, kind: &str, held: Held, now_ms: u64) -> Option<(Start, Held)> {
        let saved = self.restored.remove(market_id).filter(|s| s.kind == kind)?;
        let (start, at_save) = saved.read(now_ms).ok()?;
        let shift = match at_save {
            Some(a) => Held { base: moved(held.base, a.base), quote: moved(held.quote, a.quote) },
            // An older file has no balances at the save: the start stays as saved
            None => Held::default(),
        };
        Some((Start { base: start.base + shift.base, quote: start.quote + shift.quote, price: start.price }, shift))
    }

    /// The start a strategy of this process counts from, and the balances it holds now.
    pub(crate) fn set(&mut self, market_id: &str, kind: &str, start: Start, held: Held) {
        self.restored.remove(market_id);
        match self.live.get_mut(market_id) {
            Some(live) if live.kind == kind => {
                live.start = start;
                live.held = held;
            }
            _ => {
                self.live.insert(market_id.to_string(), LiveStart { kind: kind.to_string(), start, held });
            }
        }
    }

    /// What a save at `now_ms` keeps: those in use stamped now, restored ones as they were.
    pub(crate) fn export(&self, now_ms: u64) -> Vec<SavedStrategyStart> {
        let live = self.live.iter().filter_map(|(m, l)| SavedStrategyStart::new(m, &l.kind, l.start, l.held, now_ms));
        let restored = self.restored.values().filter(|s| s.start_at(now_ms).is_some()).cloned();
        live.chain(restored).collect()
    }
}

/// Serializable RFQ V2 SettlementTicket pool entry
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedTicket {
    pub ticket_id: String,
    pub contract_id: String,
    /// "free" | "assigned" | "spent"
    pub status: String,
    /// quote_id when status == "assigned"
    pub quote_id: Option<String>,
    /// Wall-clock assignment expiry (epoch ms) when status == "assigned"
    pub expires_at_ms: Option<u64>,
}

/// Serializable RFQ V2 Confirmed quote (hard reserve backing a live envelope)
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedPendingV2 {
    pub quote_id: String,
    pub market_id: String,
    pub holding_cids: Vec<String>,
    /// "" = ticketless
    pub ticket_id: String,
    pub valid_until_micros: i64,
}

/// Serializable mirror of `TrackedOrder` from order_tracker.rs
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedTrackedOrder {
    pub order_id: u64,
    pub market_id: String,
    pub order_type: i32,
    pub price: String,
    pub quantity: String,
    pub settled_quantity: String,
    pub pending_quantity: String,
    pub nonce: u64,
    pub signature: String,
    /// Base64-encoded signed data bytes
    pub signed_data: String,
    pub placed_by: String,
    pub is_active: bool,
}

/// Serializable settlement-to-order mapping
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedSettlementOrder {
    pub proposal_id: String,
    pub order_id: u64,
    /// Decimal quantity as string
    pub quantity: String,
    /// Whether the order's pending_quantity reservation was applied.
    /// Defaults to true for legacy state files, which predate deferred
    /// reservation and always had pending_quantity applied at adoption.
    #[serde(default = "default_reserved")]
    pub reserved: bool,
    /// The terms adopted with the proposal; absent in older state files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<AdoptedTerms>,
}

fn default_reserved() -> bool {
    true
}

/// What the agent adopted for a settlement: both parties and both legs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedTerms {
    pub buyer: String,
    pub seller: String,
    pub base_instrument: String,
    pub base_quantity: String,
    pub quote_instrument: String,
    pub quote_quantity: String,
}

impl AdoptedTerms {
    pub fn of(proposal: &SettlementProposal) -> Self {
        Self {
            buyer: proposal.buyer.clone(),
            seller: proposal.seller.clone(),
            base_instrument: proposal.base_instrument.clone(),
            base_quantity: proposal.base_quantity.clone(),
            quote_instrument: proposal.quote_instrument.clone(),
            quote_quantity: proposal.quote_quantity.clone(),
        }
    }

    /// The first field of `proposal` that differs from these terms; quantities compare by value.
    pub fn first_difference(&self, proposal: &SettlementProposal) -> Option<&'static str> {
        let texts = [
            ("buyer", &self.buyer, &proposal.buyer),
            ("seller", &self.seller, &proposal.seller),
            ("base_instrument", &self.base_instrument, &proposal.base_instrument),
            ("quote_instrument", &self.quote_instrument, &proposal.quote_instrument),
        ];
        let amounts = [
            ("base_quantity", &self.base_quantity, &proposal.base_quantity),
            ("quote_quantity", &self.quote_quantity, &proposal.quote_quantity),
        ];
        texts
            .into_iter()
            .find(|(_, adopted, now)| adopted != now)
            .or_else(|| amounts.into_iter().find(|(_, adopted, now)| !same_amount(adopted, now)))
            .map(|(field, _, _)| field)
    }
}

/// Equal text, or two plain decimals of the same exact value.
fn same_amount(a: &str, b: &str) -> bool {
    let exact = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit() || c == b'.' || c == b'-')
            .then(|| Decimal::from_str_exact(s).ok())
            .flatten()
    };
    a == b || matches!((exact(a), exact(b)), (Some(x), Some(y)) if x == y)
}

/// Serializable mirror of `AcceptedRfqTrade` from runner.rs
#[derive(Serialize, Deserialize)]
pub struct SavedAcceptedRfqTrade {
    pub proposal_id: String,
    pub market_id: String,
    pub price: String,
    pub base_quantity: String,
    pub quote_quantity: String,
}

/// Serializable mirror of `QuotedTrade` from runner.rs
#[derive(Serialize, Deserialize)]
pub struct SavedQuotedTrade {
    pub market_id: String,
    pub price: String,
    pub base_quantity: String,
    pub quote_quantity: String,
}

/// Fill loop state for buyer/seller commands
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedFillState {
    pub direction: String,
    pub market_id: String,
    pub total_amount: f64,
    pub filled_total: f64,
    pub remaining: f64,
    pub round: u32,
    /// A round whose settle may have committed when the loop stopped.
    #[serde(default)]
    pub unresolved: Option<UnresolvedRound>,
}

/// A fill round of unknown outcome; a restart must resolve it before quoting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedRound {
    pub kind: UnresolvedKind,
    /// The V1 proposal id or the atomic quote id.
    pub id: String,
    /// The round's base quantity.
    pub qty: f64,
}

/// How an unresolved round was settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnresolvedKind {
    /// A V1 multicall.
    V1,
    /// An atomic settle.
    Atomic,
}

impl SavedState {
    /// Create a new SavedState with the current timestamp
    pub fn new(party_id: String, start_time_ms: u64) -> Self {
        Self {
            version: STATE_VERSION,
            saved_at: clock::now_utc().to_rfc3339(),
            party_id,
            start_time_ms,
            completed_proposals: Vec::new(),
            rejected_proposals: Vec::new(),
            orders: Vec::new(),
            settlement_orders: Vec::new(),
            accepted_rfq_trades: Vec::new(),
            quoted_rfq_trades: Vec::new(),
            fill_state: None,
            flow_tracker: Vec::new(),
            atomic_tickets: Vec::new(),
            pending_v2_quotes: Vec::new(),
            strategy_starts: Vec::new(),
        }
    }
}

/// Save state to a JSON file atomically (write to .tmp, then rename); the file
/// and its directory are synced, so a reported save survives a host crash.
pub fn save_state(path: &Path, state: &SavedState) -> Result<()> {
    save_state_with(path, state, std::fs::File::sync_all)
}

fn save_state_with(
    path: &Path,
    state: &SavedState,
    sync_file: impl Fn(&std::fs::File) -> std::io::Result<()>,
) -> Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(json.as_bytes())?;
    // A save that is not durable is reported as failed
    sync_file(&file)?;
    drop(file);
    std::fs::rename(&tmp_path, path)?;
    sync_dir(parent_dir(path))?;
    info!("State saved to {}", path.display());
    Ok(())
}

/// The directory holding `path`; "." for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        // Some filesystems cannot sync a directory
        Err(e) if matches!(e.kind(), std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported) => Ok(()),
        other => other,
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Save a timestamped backup copy of the state file into a `backups/` subfolder.
/// Never panics — logs errors and returns silently on failure.
pub fn save_backup(path: &Path, state: &SavedState) {
    let backup_dir = match path.parent() {
        Some(parent) => parent.join("backups"),
        None => {
            error!("Cannot determine parent directory for backup: {}", path.display());
            return;
        }
    };

    if let Err(e) = std::fs::create_dir_all(&backup_dir) {
        error!("Failed to create backup directory {}: {}", backup_dir.display(), e);
        return;
    }

    // Use ISO8601 timestamp with colons replaced by dashes for filesystem safety
    let timestamp = clock::now_utc()
        .format("%Y-%m-%dT%H-%M-%SZ")
        .to_string();
    let backup_path = backup_dir.join(format!("agent-state-{}.json", timestamp));

    let json = match serde_json::to_string_pretty(state) {
        Ok(j) => j,
        Err(e) => {
            error!("Failed to serialize state for backup: {}", e);
            return;
        }
    };

    if let Err(e) = std::fs::write(&backup_path, &json) {
        error!("Failed to write backup file {}: {}", backup_path.display(), e);
        return;
    }

    info!("State backup saved to {}", backup_path.display());
}

/// Load state from a JSON file. Returns None if file doesn't exist or is corrupt.
pub fn load_state(path: &Path) -> Option<SavedState> {
    if !path.exists() {
        return None;
    }

    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) => {
            warn!("Failed to read state file {}: {}", path.display(), e);
            return None;
        }
    };

    match serde_json::from_str::<SavedState>(&data) {
        Ok(state) => {
            if state.version != STATE_VERSION {
                warn!(
                    "State file version {} != expected {}, ignoring",
                    state.version, STATE_VERSION
                );
                return None;
            }
            info!(
                "Loaded state from {} (saved at {}, {} completed, {} rejected, {} orders)",
                path.display(),
                state.saved_at,
                state.completed_proposals.len(),
                state.rejected_proposals.len(),
                state.orders.len(),
            );
            Some(state)
        }
        Err(e) => {
            warn!("Failed to parse state file {}: {}", path.display(), e);
            None
        }
    }
}

/// Load the state file; only a missing file is `None`. A file that cannot be read
/// or parsed, or of another version, is an error that names it and shows no content.
pub fn load_state_strict(path: &Path) -> Result<Option<SavedState>> {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow!("cannot read state file {}: {}", path.display(), e)),
    };
    let state: SavedState = serde_json::from_str(&data).map_err(|e| {
        let what = match e.classify() {
            Category::Syntax => "a syntax error",
            Category::Data => "a missing field or a value of the wrong type",
            Category::Eof => "an unexpected end",
            Category::Io => "a read error",
        };
        anyhow!("state file {} does not parse: {} at line {} column {}", path.display(), what, e.line(), e.column())
    })?;
    if state.version != STATE_VERSION {
        anyhow::bail!("state file {} has version {}, expected {}", path.display(), state.version, STATE_VERSION);
    }
    Ok(Some(state))
}

/// Prune stale data from state before saving to disk.
///
/// Removes orders, proposals, and RFQ trades that are no longer needed.
/// Settlement deadlines max out at ~18h (6h allocate + 12h settle),
/// so anything older than 24h is safe to discard.
pub fn prune_state(state: &mut SavedState) {
    // Build reference sets from active settlement_orders
    let referenced_order_ids: HashSet<u64> = state
        .settlement_orders
        .iter()
        .map(|so| so.order_id)
        .collect();
    let active_proposal_ids: HashSet<&str> = state
        .settlement_orders
        .iter()
        .map(|so| so.proposal_id.as_str())
        .collect();
    let cutoff_ms = clock::now_millis().saturating_sub(24 * 3600 * 1000);

    // Fix stale pending_quantity on orders not backed by a settlement_order
    let mut pending_fixed = 0usize;
    for order in &mut state.orders {
        if order.pending_quantity != "0"
            && !referenced_order_ids.contains(&order.order_id)
        {
            order.pending_quantity = "0".to_string();
            pending_fixed = pending_fixed.saturating_add(1);
        }
    }

    // Prune orders: keep if active, referenced by settlement, or recent
    let orders_before = state.orders.len();
    state.orders.retain(|o| {
        o.is_active
            || referenced_order_ids.contains(&o.order_id)
            || o.nonce > cutoff_ms
    });
    let orders_pruned = orders_before.saturating_sub(state.orders.len());

    // Cap dedup sets at 1000 (appended chronologically, drain oldest from front)
    const MAX_DEDUP_SET: usize = 1000;
    let completed_before = state.completed_proposals.len();
    if let Some(drain_count) = completed_before.checked_sub(MAX_DEDUP_SET) {
        state.completed_proposals.drain(..drain_count);
    }
    let rejected_before = state.rejected_proposals.len();
    if let Some(drain_count) = rejected_before.checked_sub(MAX_DEDUP_SET) {
        state.rejected_proposals.drain(..drain_count);
    }

    // Clear quoted RFQ trades (no timestamps; stale after shutdown/restart)
    let quoted_before = state.quoted_rfq_trades.len();
    state.quoted_rfq_trades.clear();

    // Prune accepted RFQ trades to only those with active settlements
    let accepted_before = state.accepted_rfq_trades.len();
    state
        .accepted_rfq_trades
        .retain(|t| active_proposal_ids.contains(t.proposal_id.as_str()));

    // Drop V2 quotes whose signed window (+ a generous grace) has passed —
    // the on-ledger window check makes a late settle impossible.
    let now_micros = clock::now_micros_i64();
    state
        .pending_v2_quotes
        .retain(|q| q.valid_until_micros.saturating_add(120_000_000) > now_micros);

    info!(
        "State pruned: orders {}->{} (removed {}, fixed {} stale pending), \
         completed {}->{}, rejected {}->{}, quoted_rfq cleared {}, accepted_rfq {}->{}",
        orders_before,
        state.orders.len(),
        orders_pruned,
        pending_fixed,
        completed_before,
        state.completed_proposals.len(),
        rejected_before,
        state.rejected_proposals.len(),
        quoted_before,
        accepted_before,
        state.accepted_rfq_trades.len(),
    );
}

/// Delete the state file after successful restore
pub fn delete_state(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!("Failed to delete state file {}: {}", path.display(), e);
        } else {
            info!("Deleted state file {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saved_state_roundtrip() {
        let mut state = SavedState::new("test-party".to_string(), 1234567890);
        state.completed_proposals = vec!["prop-1".to_string(), "prop-2".to_string()];
        state.rejected_proposals = vec!["prop-3".to_string()];
        state.orders.push(SavedTrackedOrder {
            order_id: 42,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100.50".to_string(),
            quantity: "5.0".to_string(),
            settled_quantity: "2.0".to_string(),
            pending_quantity: "1.0".to_string(),
            nonce: 9999999,
            signature: "abc123".to_string(),
            signed_data: "dGVzdA==".to_string(), // "test" in base64
            placed_by: "agent".to_string(),
            is_active: true,
        });
        state.settlement_orders.push(SavedSettlementOrder {
            proposal_id: "prop-4".to_string(),
            order_id: 42,
            quantity: "1.0".to_string(),
            reserved: true,
            terms: None,
        });
        state.accepted_rfq_trades.push(SavedAcceptedRfqTrade {
            proposal_id: "rfq-1".to_string(),
            market_id: "BTC-USD".to_string(),
            price: "100.50".to_string(),
            base_quantity: "1.0".to_string(),
            quote_quantity: "100.50".to_string(),
        });
        state.quoted_rfq_trades.push(SavedQuotedTrade {
            market_id: "BTC-USD".to_string(),
            price: "99.50".to_string(),
            base_quantity: "2.0".to_string(),
            quote_quantity: "199.00".to_string(),
        });
        state.fill_state = Some(SavedFillState {
            direction: "buy".to_string(),
            market_id: "BTC-USD".to_string(),
            total_amount: 100.0,
            filled_total: 50.0,
            remaining: 50.0,
            round: 5,
            unresolved: Some(UnresolvedRound { kind: UnresolvedKind::V1, id: "p-5".to_string(), qty: 10.0 }),
        });

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: SavedState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.version, STATE_VERSION);
        assert_eq!(restored.party_id, "test-party");
        assert_eq!(restored.start_time_ms, 1234567890);
        assert_eq!(restored.completed_proposals.len(), 2);
        assert_eq!(restored.rejected_proposals.len(), 1);
        assert_eq!(restored.orders.len(), 1);
        assert_eq!(restored.orders[0].order_id, 42);
        assert_eq!(restored.settlement_orders.len(), 1);
        assert_eq!(restored.accepted_rfq_trades.len(), 1);
        assert_eq!(restored.quoted_rfq_trades.len(), 1);

        let fill = restored.fill_state.unwrap();
        assert_eq!(fill.direction, "buy");
        assert_eq!(fill.filled_total, 50.0);
        assert_eq!(fill.remaining, 50.0);
        assert_eq!(fill.round, 5);
        let marker = fill.unresolved.unwrap();
        assert_eq!((marker.kind, marker.id.as_str(), marker.qty), (UnresolvedKind::V1, "p-5", 10.0));
        assert!(json.contains("\"kind\": \"v1\""), "{json}");
    }

    // A fill state saved before the unresolved-round marker existed still loads
    #[test]
    fn a_fill_state_without_the_marker_loads() {
        let old = r#"{"direction":"sell","market_id":"CC-USDC","total_amount":10.0,"filled_total":4.0,"remaining":6.0,"round":2}"#;
        let fill: SavedFillState = serde_json::from_str(old).unwrap();
        assert_eq!((fill.round, fill.unresolved), (2, None));
        let atomic = r#"{"direction":"buy","market_id":"M","total_amount":1.0,"filled_total":0.0,"remaining":1.0,"round":1,
            "unresolved":{"kind":"atomic","id":"q-1","qty":0.5}}"#;
        let fill: SavedFillState = serde_json::from_str(atomic).unwrap();
        assert_eq!(fill.unresolved, Some(UnresolvedRound { kind: UnresolvedKind::Atomic, id: "q-1".to_string(), qty: 0.5 }));
    }

    #[test]
    fn test_save_and_load_state() {
        let dir = std::env::temp_dir().join(format!("silvana-test-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test-state.json");

        let state = SavedState::new("test-party".to_string(), 42);
        save_state(&path, &state).unwrap();

        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.party_id, "test-party");
        assert_eq!(loaded.start_time_ms, 42);

        delete_state(&path);
        assert!(!path.exists());

        // load_state returns None for missing file
        assert!(load_state(&path).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh directory for one test, removed when dropped.
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("silvana-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // A restart used to count a strategy's position from the balances it found, buying it twice
    #[test]
    fn strategy_start_balances_round_trip_and_expire_after_six_hours() {
        let dir = TestDir::new("strategy-starts");
        let path = dir.0.join("agent-state.json");
        let now = 1_700_000_000_000u64;
        let start = Start { base: 1_250_000.0, quote: 1480.5, price: 0.0015 };
        let held = Held { base: 1_300_000.0, quote: 1405.5 };
        let mut live = StrategyStarts::default();
        live.set("HTEST-USDC", "momentum", start, held);
        let mut state = SavedState::new("party".to_string(), 1);
        state.strategy_starts = live.export(now);
        save_state(&path, &state).unwrap();
        let loaded = load_state(&path).unwrap().strategy_starts;
        assert_eq!(loaded, state.strategy_starts);
        assert_eq!((loaded.len(), loaded[0].saved_at_ms, loaded[0].base.as_str()), (1, now, "1250000"));
        let at_save = (loaded[0].base_at_save.as_deref(), loaded[0].quote_at_save.as_deref());
        assert_eq!(at_save, (Some("1300000"), Some("1405.5")));

        // An hour later the same kind takes them, once; another kind starts afresh
        let hour = now + 3_600_000;
        let mut restored = StrategyStarts::default();
        restored.restore(&loaded, hour);
        assert_eq!(restored.take("HTEST-USDC", "twap", held, hour), None);
        restored.restore(&loaded, hour);
        assert_eq!(restored.take("HTEST-USDC", "momentum", held, hour), Some((start, Held::default())));
        assert_eq!(restored.take("HTEST-USDC", "momentum", held, hour), None);
        // Six hours after the save they still count; a millisecond later they do not
        let six = now + STRATEGY_START_MAX_AGE_MS;
        assert_eq!(loaded[0].start_at(six), Some(start));
        restored.restore(&loaded, six + 1);
        assert_eq!(restored.take("HTEST-USDC", "momentum", held, six + 1), None);
        // Restored but not taken, they keep their save time, so they still expire
        restored.restore(&loaded, hour);
        assert_eq!(restored.export(hour + 5), loaded);
        assert!(restored.export(six + 1).is_empty());
        // Taken into use, the next save stamps them anew with the balances last held
        let later = Held { base: 1_200_000.0, quote: 1480.5 };
        restored.set("HTEST-USDC", "momentum", start, later);
        let out = restored.export(six + 1);
        assert_eq!((out[0].saved_at_ms, out[0].base_at_save.as_deref()), (six + 1, Some("1200000")));
        // A value that is not finite is never written
        assert!(SavedStrategyStart::new("M", "momentum", Start { base: f64::NAN, ..start }, held, now).is_none());
        let inf = Held { quote: f64::INFINITY, ..held };
        assert!(SavedStrategyStart::new("M", "momentum", start, inf, now).is_none());

        // A state file written before these existed loads with none
        let mut old = serde_json::to_value(SavedState::new("party".to_string(), 1)).unwrap();
        assert!(old.as_object_mut().unwrap().remove("strategy_starts").is_some());
        std::fs::write(&path, old.to_string()).unwrap();
        assert!(load_state_strict(&path).unwrap().unwrap().strategy_starts.is_empty());
        assert!(load_state(&path).unwrap().strategy_starts.is_empty());
    }

    // Funds moved while stopped used to count as a position change, so the strategy traded them back
    #[test]
    fn a_restart_after_a_withdrawal_keeps_the_position_not_the_old_base() {
        let (now, hour) = (1_700_000_000_000u64, 1_700_003_600_000u64);
        let start = Start { base: 1_000_000.0, quote: 2400.0, price: 0.0015 };
        // Long 500k, bought for 750, at the stop
        let at_save = Held { base: 1_500_000.0, quote: 1650.0 };
        let mut live = StrategyStarts::default();
        live.set("HTEST-USDC", "momentum", start, at_save);
        let saved = live.export(now);
        // While stopped, 400k base went out and 100 quote came in
        let held = Held { base: 1_100_000.0, quote: 1750.0 };
        let mut restored = StrategyStarts::default();
        restored.restore(&saved, hour);
        let (moved_start, shift) = restored.take("HTEST-USDC", "momentum", held, hour).unwrap();
        assert_eq!(shift, Held { base: -400_000.0, quote: 100.0 });
        assert_eq!(moved_start, Start { base: 600_000.0, quote: 2500.0, price: 0.0015 });
        assert_eq!(held.base - moved_start.base, 500_000.0, "the position, not 100k against the old base");
        assert_eq!(held.quote - moved_start.quote, at_save.quote - start.quote);

        // A file without the balances at the save keeps the start as saved
        let old: Vec<_> =
            saved.iter().map(|s| SavedStrategyStart { base_at_save: None, quote_at_save: None, ..s.clone() }).collect();
        let json = serde_json::to_string(&old).unwrap();
        assert!(!json.contains("at_save"), "{json}");
        let old: Vec<SavedStrategyStart> = serde_json::from_str(&json).unwrap();
        restored.restore(&old, hour);
        assert_eq!(restored.take("HTEST-USDC", "momentum", held, hour), Some((start, Held::default())));
    }

    // A near-zero residual used to be written as a string with more places than a decimal holds
    #[test]
    fn a_near_zero_residual_and_a_tiny_price_read_back() {
        let residual = 1.4210854715202004e-14;
        assert!(Decimal::from_str_exact(&residual.to_string()).is_err());
        let now = 1_700_000_000_000u64;
        let start = Start { base: 1_250_000.0, quote: residual, price: 1.23456789e-12 };
        let held = Held { base: 1_250_000.0, quote: -residual };
        let saved = SavedStrategyStart::new("M", "momentum", start, held, now).unwrap();
        assert_eq!((saved.quote.as_str(), saved.price.as_str()), ("0", "0.00000000000123456789"));
        assert_eq!(saved.quote_at_save.as_deref(), Some("0"));
        let back: Vec<SavedStrategyStart> = serde_json::from_str(&serde_json::to_string(&[saved]).unwrap()).unwrap();
        let mut restored = StrategyStarts::default();
        restored.restore(&back, now + 1);
        let expected = Start { quote: 0.0, ..start };
        assert_eq!(restored.take("M", "momentum", held, now + 1), Some((expected, Held::default())));
    }

    // An entry that does not read back is dropped with a warning; the others are kept
    #[test]
    fn an_unreadable_saved_start_is_dropped_with_a_warning() {
        let logs = crate::test_logs::LogBuf::default();
        let _guard = logs.capture(tracing::Level::WARN);
        let now = 1_700_000_000_000u64;
        let start = Start { base: 1_250_000.0, quote: 1480.5, price: 0.0015 };
        let held = Held { base: 1_250_000.0, quote: 1480.5 };
        let good = SavedStrategyStart::new("A-USDC", "momentum", start, held, now).unwrap();
        // As the earlier writer stored a residual
        let quote = 1.4210854715202004e-14f64.to_string();
        let bad = SavedStrategyStart { market_id: "B-USDC".to_string(), quote, ..good.clone() };
        let half = SavedStrategyStart { market_id: "C-USDC".to_string(), quote_at_save: None, ..good.clone() };
        let old = now - STRATEGY_START_MAX_AGE_MS - 1;
        let expired = SavedStrategyStart { market_id: "D-USDC".to_string(), saved_at_ms: old, ..good.clone() };
        let mut restored = StrategyStarts::default();
        restored.restore(&[good, bad, half, expired], now);
        assert_eq!(restored.take("A-USDC", "momentum", held, now), Some((start, Held::default())));
        for market in ["B-USDC", "C-USDC", "D-USDC"] {
            assert_eq!(restored.take(market, "momentum", held, now), None, "{market}");
        }
        assert_eq!(logs.count("saved start balances unreadable"), 2);
        assert_eq!(logs.count("Strategy B-USDC [momentum]: saved start balances unreadable; taken again"), 1);
        assert_eq!(logs.count("Strategy C-USDC [momentum]: saved start balances unreadable"), 1);
        assert_eq!(logs.count("D-USDC"), 0, "an expired one is no warning");
    }

    #[test]
    fn a_bare_file_name_is_synced_in_the_current_directory() {
        assert_eq!(parent_dir(Path::new("agent-state.json")), Path::new("."));
        assert_eq!(parent_dir(Path::new("run/agent-state.json")), Path::new("run"));
    }

    // A save used to report success while its data could still be lost to a host crash
    #[test]
    fn a_save_that_cannot_be_synced_fails_and_keeps_the_previous_file() {
        let dir = TestDir::new("state-sync");
        let path = dir.0.join("agent-state.json");
        save_state(&path, &SavedState::new("old-party".to_string(), 1)).unwrap();
        let before = std::fs::read(&path).unwrap();
        let refused = |_: &std::fs::File| Err(std::io::Error::other("sync refused"));
        let err = save_state_with(&path, &SavedState::new("new-party".to_string(), 2), refused).unwrap_err();
        assert!(err.to_string().contains("sync refused"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let synced = std::cell::Cell::new(0u32);
        let counted = |f: &std::fs::File| {
            synced.set(synced.get() + 1);
            f.sync_all()
        };
        save_state_with(&path, &SavedState::new("new-party".to_string(), 2), counted).unwrap();
        assert_eq!(synced.get(), 1);
        assert_eq!(load_state(&path).unwrap().party_id, "new-party");
    }

    // A state file that could not be read used to load as no state at all
    #[test]
    fn a_strict_load_fails_on_anything_but_a_missing_file() {
        let dir = TestDir::new("state-strict");
        let path = dir.0.join("agent-state.json");
        assert!(load_state_strict(&path).unwrap().is_none());
        save_state(&path, &SavedState::new("party".to_string(), 3)).unwrap();
        assert_eq!(load_state_strict(&path).unwrap().unwrap().start_time_ms, 3);

        std::fs::write(&path, r#"{"version": 1, "party_id": "probe-value",}"#).unwrap();
        let err = load_state_strict(&path).err().unwrap().to_string();
        assert!(err.contains("agent-state.json") && err.contains("syntax error at line 1"), "{err}");
        assert!(!err.contains("probe-value"), "{err}");

        let mut wrong = serde_json::to_value(SavedState::new("party".to_string(), 3)).unwrap();
        wrong["start_time_ms"] = serde_json::json!("probe-value");
        std::fs::write(&path, wrong.to_string()).unwrap();
        let err = load_state_strict(&path).err().unwrap().to_string();
        assert!(err.contains("wrong type") && !err.contains("probe-value"), "{err}");

        let mut newer = serde_json::to_value(SavedState::new("party".to_string(), 3)).unwrap();
        newer["version"] = serde_json::json!(99);
        std::fs::write(&path, newer.to_string()).unwrap();
        assert!(load_state_strict(&path).err().unwrap().to_string().contains("version 99"));

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load_state_strict(&path).err().unwrap().to_string().contains("cannot read state file"));
    }

    #[test]
    fn test_prune_state() {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let old_nonce = now_ms - (48 * 3600 * 1000); // 48h ago
        let recent_nonce = now_ms - (1 * 3600 * 1000); // 1h ago

        let mut state = SavedState::new("test-party".to_string(), 1000);

        // Order 1: old, inactive, no settlement — should be PRUNED
        state.orders.push(SavedTrackedOrder {
            order_id: 1,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100".to_string(),
            quantity: "1.0".to_string(),
            settled_quantity: "0".to_string(),
            pending_quantity: "0".to_string(),
            nonce: old_nonce,
            signature: "sig1".to_string(),
            signed_data: "data1".to_string(),
            placed_by: "agent".to_string(),
            is_active: false,
        });

        // Order 2: old, inactive, but referenced by settlement — should be KEPT
        state.orders.push(SavedTrackedOrder {
            order_id: 2,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100".to_string(),
            quantity: "1.0".to_string(),
            settled_quantity: "0".to_string(),
            pending_quantity: "0.5".to_string(),
            nonce: old_nonce,
            signature: "sig2".to_string(),
            signed_data: "data2".to_string(),
            placed_by: "agent".to_string(),
            is_active: false,
        });

        // Order 3: recent, inactive — should be KEPT (within 24h)
        state.orders.push(SavedTrackedOrder {
            order_id: 3,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100".to_string(),
            quantity: "1.0".to_string(),
            settled_quantity: "0".to_string(),
            pending_quantity: "0".to_string(),
            nonce: recent_nonce,
            signature: "sig3".to_string(),
            signed_data: "data3".to_string(),
            placed_by: "agent".to_string(),
            is_active: false,
        });

        // Order 4: old, but active — should be KEPT
        state.orders.push(SavedTrackedOrder {
            order_id: 4,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100".to_string(),
            quantity: "1.0".to_string(),
            settled_quantity: "0".to_string(),
            pending_quantity: "0".to_string(),
            nonce: old_nonce,
            signature: "sig4".to_string(),
            signed_data: "data4".to_string(),
            placed_by: "agent".to_string(),
            is_active: true,
        });

        // Order 5: old, inactive, stale pending (no settlement_order) — should be PRUNED, pending fixed
        state.orders.push(SavedTrackedOrder {
            order_id: 5,
            market_id: "BTC-USD".to_string(),
            order_type: 1,
            price: "100".to_string(),
            quantity: "1.0".to_string(),
            settled_quantity: "0".to_string(),
            pending_quantity: "0.5".to_string(),
            nonce: old_nonce,
            signature: "sig5".to_string(),
            signed_data: "data5".to_string(),
            placed_by: "agent".to_string(),
            is_active: false,
        });

        // Settlement order referencing order 2
        state.settlement_orders.push(SavedSettlementOrder {
            proposal_id: "prop-active".to_string(),
            order_id: 2,
            quantity: "0.5".to_string(),
            reserved: true,
            terms: None,
        });

        // Completed proposals: 1500 items (should be capped to 1000)
        state.completed_proposals = (0..1500).map(|i| format!("completed-{}", i)).collect();

        // Rejected proposals: 50 items (under cap, kept as-is)
        state.rejected_proposals = (0..50).map(|i| format!("rejected-{}", i)).collect();

        // Quoted RFQ trades (should all be cleared)
        state.quoted_rfq_trades.push(SavedQuotedTrade {
            market_id: "BTC-USD".to_string(),
            price: "100".to_string(),
            base_quantity: "1.0".to_string(),
            quote_quantity: "100.0".to_string(),
        });

        // Accepted RFQ trades: one active, one stale
        state.accepted_rfq_trades.push(SavedAcceptedRfqTrade {
            proposal_id: "prop-active".to_string(),
            market_id: "BTC-USD".to_string(),
            price: "100".to_string(),
            base_quantity: "1.0".to_string(),
            quote_quantity: "100.0".to_string(),
        });
        state.accepted_rfq_trades.push(SavedAcceptedRfqTrade {
            proposal_id: "prop-stale".to_string(),
            market_id: "BTC-USD".to_string(),
            price: "100".to_string(),
            base_quantity: "1.0".to_string(),
            quote_quantity: "100.0".to_string(),
        });

        // Run pruning
        prune_state(&mut state);

        // Orders: should keep 2 (referenced), 3 (recent), 4 (active) = 3 orders
        assert_eq!(state.orders.len(), 3);
        let kept_ids: Vec<u64> = state.orders.iter().map(|o| o.order_id).collect();
        assert!(kept_ids.contains(&2), "settlement-referenced order should be kept");
        assert!(kept_ids.contains(&3), "recent order should be kept");
        assert!(kept_ids.contains(&4), "active order should be kept");
        assert!(!kept_ids.contains(&1), "old unreferenced order should be pruned");
        assert!(!kept_ids.contains(&5), "old stale-pending order should be pruned");

        // Order 2's pending_quantity should stay (has settlement_order)
        let order2 = state.orders.iter().find(|o| o.order_id == 2).unwrap();
        assert_eq!(order2.pending_quantity, "0.5");

        // Completed proposals capped at 1000
        assert_eq!(state.completed_proposals.len(), 1000);
        // Should keep the latest 1000 (500..1499)
        assert_eq!(state.completed_proposals[0], "completed-500");

        // Rejected proposals unchanged (under cap)
        assert_eq!(state.rejected_proposals.len(), 50);

        // Quoted RFQ trades cleared
        assert!(state.quoted_rfq_trades.is_empty());

        // Accepted RFQ trades: only active one kept
        assert_eq!(state.accepted_rfq_trades.len(), 1);
        assert_eq!(state.accepted_rfq_trades[0].proposal_id, "prop-active");
    }

    #[test]
    fn prune_keeps_live_v2_quotes_and_drops_expired_ones_at_the_extremes() {
        let quote = |id: &str, valid_until_micros: i64| SavedPendingV2 {
            quote_id: id.to_string(),
            market_id: "CC-USDCx".to_string(),
            holding_cids: Vec::new(),
            ticket_id: String::new(),
            valid_until_micros,
        };
        let mut state = SavedState::new("test-party".to_string(), 0);
        state.pending_v2_quotes = vec![quote("far", i64::MAX), quote("gone", i64::MIN), quote("old", 1)];
        prune_state(&mut state);
        let kept: Vec<&str> = state.pending_v2_quotes.iter().map(|q| q.quote_id.as_str()).collect();
        assert_eq!(kept, ["far"]);
    }

    // Legacy state files predate the `reserved` flag: those entries all had
    // pending_quantity applied at adoption, so the serde default must be true
    // to preserve their accounting through the upgrade. New unreserved entries
    // must round-trip as false.
    #[test]
    fn test_saved_settlement_order_reserved_default_and_roundtrip() {
        // Legacy JSON without the field → reserved = true
        let legacy: SavedSettlementOrder = serde_json::from_str(
            r#"{"proposal_id":"p1","order_id":42,"quantity":"2.0"}"#,
        )
        .unwrap();
        assert!(legacy.reserved);
        assert_eq!(legacy.terms, None);

        // New unreserved entry round-trips as false
        let mut entry = SavedSettlementOrder {
            proposal_id: "p2".to_string(),
            order_id: 7,
            quantity: "1.5".to_string(),
            reserved: false,
            terms: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("terms"), "{json}");
        let back: SavedSettlementOrder = serde_json::from_str(&json).unwrap();
        assert!(!back.reserved);
        assert_eq!(back.order_id, 7);

        let proposal = SettlementProposal {
            buyer: "b".to_string(),
            seller: "s".to_string(),
            base_quantity: "1".to_string(),
            quote_quantity: "2".to_string(),
            ..Default::default()
        };
        entry.terms = Some(AdoptedTerms::of(&proposal));
        let back: SavedSettlementOrder = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(back.terms, entry.terms);
    }

    #[test]
    fn adopted_terms_compare_quantities_by_value() {
        let adopted = SettlementProposal {
            buyer: "b".to_string(),
            seller: "s".to_string(),
            base_instrument: "BASE".to_string(),
            base_quantity: "1".to_string(),
            quote_instrument: "QUOTE".to_string(),
            quote_quantity: "2".to_string(),
            ..Default::default()
        };
        let terms = AdoptedTerms::of(&adopted);
        assert_eq!(terms.first_difference(&adopted), None);
        let resent = |change: fn(&mut SettlementProposal)| {
            let mut p = adopted.clone();
            change(&mut p);
            terms.first_difference(&p)
        };
        assert_eq!(resent(|p| p.quote_quantity = "2.000".to_string()), None);
        assert_eq!(resent(|p| p.quote_quantity = "3".to_string()), Some("quote_quantity"));
        assert_eq!(resent(|p| p.quote_quantity = "2e0".to_string()), Some("quote_quantity"));
        assert_eq!(resent(|p| p.base_quantity = "1.0000000000000000000000000000001".to_string()), Some("base_quantity"));
        assert_eq!(resent(|p| p.buyer = "x".to_string()), Some("buyer"));
        assert_eq!(resent(|p| p.seller = "x".to_string()), Some("seller"));
        assert_eq!(resent(|p| p.base_instrument = "X".to_string()), Some("base_instrument"));
        assert_eq!(resent(|p| p.quote_instrument = "X".to_string()), Some("quote_instrument"));
    }
}
