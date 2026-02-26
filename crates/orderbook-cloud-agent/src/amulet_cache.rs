//! Three-pool amulet cache for parallel payment processing
//!
//! Tracks amulet (CC) contracts across three pools:
//! - **Available**: amulets known from ACS, refreshed every 30s (TTL 3 min)
//! - **Consumed**: amulets used in committed transactions (permanent until ACS refresh confirms gone)
//! - **Reserved**: amulets tentatively assigned to in-flight workers (TTL 60s)
//!
//! The scheduler calls `get_selectable_amulets()` to get available amulets that are
//! neither consumed nor reserved, then `reserve()` to atomically claim them for a worker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// TTL for available amulets (refreshed by ACS worker every 30s)
const AVAILABLE_TTL_SECS: u64 = 180; // 3 minutes

/// TTL for reservations (workers should complete within this time)
const RESERVATION_TTL_SECS: u64 = 60;

/// A cached amulet with amount and discovery time
#[derive(Debug, Clone)]
pub struct CachedAmulet {
    pub contract_id: String,
    pub amount: Decimal,
    pub discovered_at: Instant,
}

/// Entry in the consumed pool
#[derive(Debug, Clone)]
struct ConsumedEntry {
    /// Why it was consumed (update_id or "inactive")
    #[allow(dead_code)]
    reason: String,
    #[allow(dead_code)]
    consumed_at: Instant,
}

/// Entry in the reserved pool
#[derive(Debug, Clone)]
struct ReservedEntry {
    /// Which payment reserved this amulet
    #[allow(dead_code)]
    payment_id: String,
    reserved_at: Instant,
}

/// Three-pool amulet cache
pub struct AmuletCache {
    /// Amulets known from ACS (refreshed periodically)
    available: RwLock<HashMap<String, CachedAmulet>>,
    /// Amulets consumed by committed transactions
    consumed: RwLock<HashMap<String, ConsumedEntry>>,
    /// Amulets reserved by in-flight workers
    reserved: RwLock<HashMap<String, ReservedEntry>>,
}

impl AmuletCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            available: RwLock::new(HashMap::new()),
            consumed: RwLock::new(HashMap::new()),
            reserved: RwLock::new(HashMap::new()),
        })
    }

    /// Refresh available amulets from ACS query results.
    /// Replaces the available pool entirely. Clears consumed entries for CIDs
    /// no longer in ACS (they've been fully processed by the ledger).
    pub async fn refresh_from_acs(&self, amulets: Vec<CachedAmulet>) {
        let now = Instant::now();

        // Build new available map
        let new_available: HashMap<String, CachedAmulet> = amulets
            .into_iter()
            .map(|a| (a.contract_id.clone(), CachedAmulet {
                contract_id: a.contract_id,
                amount: a.amount,
                discovered_at: now,
            }))
            .collect();

        let new_count = new_available.len();

        // Clean consumed entries: remove CIDs that no longer appear in ACS
        // (meaning the ledger has fully processed the consumption)
        {
            let mut consumed = self.consumed.write().await;
            let before = consumed.len();
            consumed.retain(|cid, _| new_available.contains_key(cid));
            let removed = before - consumed.len();
            if removed > 0 {
                debug!("Cleaned {} consumed entries no longer in ACS", removed);
            }
        }

        // Replace available pool
        {
            let mut available = self.available.write().await;
            *available = new_available;
        }

        debug!("ACS refresh: {} amulets available", new_count);
    }

    /// Get selectable amulets: available minus consumed minus reserved, sorted ascending by amount.
    pub async fn get_selectable_amulets(&self) -> Vec<CachedAmulet> {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;

        let mut selectable: Vec<CachedAmulet> = available
            .values()
            .filter(|a| {
                // Skip expired available entries
                if now.duration_since(a.discovered_at).as_secs() > AVAILABLE_TTL_SECS {
                    return false;
                }
                // Skip consumed
                if consumed.contains_key(&a.contract_id) {
                    return false;
                }
                // Skip reserved (that haven't expired)
                if let Some(entry) = reserved.get(&a.contract_id) {
                    if now.duration_since(entry.reserved_at).as_secs() <= RESERVATION_TTL_SECS {
                        return false;
                    }
                    // Expired reservation — treat as available
                }
                true
            })
            .cloned()
            .collect();

        // Sort ascending by amount (smallest first for optimal packing)
        selectable.sort_by(|a, b| a.amount.cmp(&b.amount));
        selectable
    }

    /// Reserve amulets for a payment worker. Atomic: all-or-nothing.
    /// Returns false if any CID is already reserved/consumed.
    pub async fn reserve(&self, contract_ids: &[String], payment_id: &str) -> bool {
        let now = Instant::now();
        let consumed = self.consumed.read().await;
        let mut reserved = self.reserved.write().await;

        // Check all CIDs are available
        for cid in contract_ids {
            if consumed.contains_key(cid) {
                debug!("Cannot reserve {}: already consumed", cid);
                return false;
            }
            if let Some(entry) = reserved.get(cid) {
                if now.duration_since(entry.reserved_at).as_secs() <= RESERVATION_TTL_SECS {
                    debug!("Cannot reserve {}: already reserved by {}", cid, entry.payment_id);
                    return false;
                }
            }
        }

        // All clear — reserve them
        for cid in contract_ids {
            reserved.insert(cid.clone(), ReservedEntry {
                payment_id: payment_id.to_string(),
                reserved_at: now,
            });
        }

        debug!(
            "Reserved {} amulets for payment {}",
            contract_ids.len(),
            payment_id
        );
        true
    }

    /// Mark amulets as consumed (after successful transaction).
    /// Moves them from reserved to consumed.
    pub async fn mark_consumed(&self, contract_ids: &[String], reason: &str) {
        let now = Instant::now();
        let mut consumed = self.consumed.write().await;
        let mut reserved = self.reserved.write().await;

        for cid in contract_ids {
            reserved.remove(cid);
            consumed.insert(cid.clone(), ConsumedEntry {
                reason: reason.to_string(),
                consumed_at: now,
            });
        }

        debug!(
            "Marked {} amulets as consumed ({})",
            contract_ids.len(),
            reason
        );
    }

    /// Release reservations on failure (make amulets available again).
    pub async fn release_reservations(&self, contract_ids: &[String]) {
        let mut reserved = self.reserved.write().await;
        for cid in contract_ids {
            reserved.remove(cid);
        }
        debug!("Released {} amulet reservations", contract_ids.len());
    }

    /// Add newly created amulets from transaction change/split.
    /// These go directly into the available pool.
    pub async fn add_created_amulets(&self, amulets: Vec<CachedAmulet>) {
        if amulets.is_empty() {
            return;
        }
        let mut available = self.available.write().await;
        let count = amulets.len();
        for amulet in amulets {
            available.insert(amulet.contract_id.clone(), amulet);
        }
        debug!("Added {} newly created amulets to cache", count);
    }

    /// Clean up expired reservations (called periodically by ACS worker).
    pub async fn cleanup_expired_reservations(&self) {
        let now = Instant::now();
        let mut reserved = self.reserved.write().await;
        let before = reserved.len();
        reserved.retain(|_, entry| {
            now.duration_since(entry.reserved_at).as_secs() <= RESERVATION_TTL_SECS
        });
        let removed = before - reserved.len();
        if removed > 0 {
            info!("Cleaned up {} expired amulet reservations", removed);
        }
    }

    /// Get cache statistics for heartbeat logging: (available, consumed, reserved, selectable)
    pub async fn stats(&self) -> (usize, usize, usize, usize) {
        let now = Instant::now();
        let available = self.available.read().await;
        let consumed = self.consumed.read().await;
        let reserved = self.reserved.read().await;

        let selectable = available
            .values()
            .filter(|a| {
                if now.duration_since(a.discovered_at).as_secs() > AVAILABLE_TTL_SECS {
                    return false;
                }
                if consumed.contains_key(&a.contract_id) {
                    return false;
                }
                if let Some(entry) = reserved.get(&a.contract_id) {
                    if now.duration_since(entry.reserved_at).as_secs() <= RESERVATION_TTL_SECS {
                        return false;
                    }
                }
                true
            })
            .count();

        (available.len(), consumed.len(), reserved.len(), selectable)
    }

}
