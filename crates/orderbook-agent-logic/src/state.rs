//! State persistence for graceful shutdown and restart
//!
//! On Ctrl-C, the agent saves verification/dedup state to a JSON file.
//! On restart, it restores this state so settlements can resume without
//! re-verification or dedup failures.
//!
//! The server tracks full settlement status — we only persist the
//! agent-side state needed for verification and deduplication.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{error, info, warn};

use crate::settlement::{PendingFee, PendingTrafficFee};

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
    /// Pending background fee payments (queued but not yet completed)
    #[serde(default)]
    pub pending_fees: Vec<PendingFee>,
    /// Pending traffic fee payments (queued but not yet completed)
    #[serde(default)]
    pub pending_traffic_fees: Vec<PendingTrafficFee>,
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
}

impl SavedState {
    /// Create a new SavedState with the current timestamp
    pub fn new(party_id: String, start_time_ms: u64) -> Self {
        Self {
            version: STATE_VERSION,
            saved_at: chrono::Utc::now().to_rfc3339(),
            party_id,
            start_time_ms,
            completed_proposals: Vec::new(),
            rejected_proposals: Vec::new(),
            orders: Vec::new(),
            settlement_orders: Vec::new(),
            accepted_rfq_trades: Vec::new(),
            quoted_rfq_trades: Vec::new(),
            fill_state: None,
            pending_fees: Vec::new(),
            pending_traffic_fees: Vec::new(),
        }
    }
}

/// Save state to a JSON file atomically (write to .tmp, then rename)
pub fn save_state(path: &Path, state: &SavedState) -> Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, path)?;
    info!("State saved to {}", path.display());
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
    let timestamp = chrono::Utc::now()
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
    }

    #[test]
    fn test_save_and_load_state() {
        let dir = std::env::temp_dir().join("silvana-test-state");
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
}
