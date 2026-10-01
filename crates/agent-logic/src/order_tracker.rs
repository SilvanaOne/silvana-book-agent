//! Order tracking and settlement verification
//!
//! Maintains an in-memory map of all orders placed by the agent, and verifies
//! settlement proposals against them. User orders (placed via frontend) are
//! imported from the server on demand.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use anyhow::{Context, Result};
use base64::Engine;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use orderbook_proto::orderbook::{Order, SettlementProposal};

use crate::auth::{sign_order_data, verify_order_signature};
use crate::state::{SavedSettlementOrder, SavedTrackedOrder};

/// Tracked order with quantity accounting
pub struct TrackedOrder {
    pub order_id: u64,
    pub market_id: String,
    pub order_type: i32, // OrderType as i32
    pub price: Decimal,
    pub quantity: Decimal,
    pub settled_quantity: Decimal,
    pub pending_quantity: Decimal,
    pub nonce: u64,
    pub signature: String,
    pub signed_data: Vec<u8>,
    pub placed_by: String, // "agent" or "user"
    pub is_active: bool,
}

/// Result of settlement verification
pub enum VerifyResult {
    /// Order verified, proceed with settlement
    Accepted { order_id: u64 },
    /// Order failed verification
    Rejected { reason: String },
    /// Order not in tracker — caller should fetch from server by order_id
    NeedServerLookup { order_id: u64 },
    /// Order not in tracker while a placement in its market is unconfirmed;
    /// hold the proposal and retry instead of looking it up
    PlacementInFlight { order_id: u64 },
}

/// Per-market count of order placements submitted but not yet tracked.
type PlacingCounts = Arc<Mutex<HashMap<String, usize>>>;

/// How long a submit that failed at the transport keeps its market's untracked matches held.
const SUBMIT_FAILED_HOLD: Duration = Duration::from_secs(60);

/// How long a failed submit stays adoptable, should the server turn out to have booked it.
const FAILED_SUBMIT_ADOPT: Duration = Duration::from_secs(3600);

/// Most failed submits kept for adoption; the oldest are dropped first.
const FAILED_SUBMIT_MAX: usize = 256;

/// Signed payload of a submit that returned an error; the server may still have booked it.
#[derive(Clone)]
pub struct FailedSubmit {
    pub market_id: String,
    pub order_type: i32,
    pub price: String,
    pub quantity: String,
    pub nonce: u64,
    pub signature: String,
    pub signed_data: Vec<u8>,
}

/// Marks one order placement in flight for a market until dropped. Take it under
/// the tracker lock before submitting; drop it under the lock that tracks the order.
#[must_use = "the placement is only guarded while this value is alive"]
pub struct PlacementGuard {
    placing: PlacingCounts,
    market_id: String,
}

impl Drop for PlacementGuard {
    fn drop(&mut self) {
        let mut placing = self.placing.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = placing.get_mut(&self.market_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                placing.remove(&self.market_id);
            }
        }
    }
}

/// Local record of an adopted settlement: which order it maps to, how much
/// it will consume, and whether that capacity has actually been reserved.
///
/// `reserved == false` between adoption (agent verified + preconfirmed its own
/// side) and the counterparty's preconfirmation; the capacity effect on
/// `pending_quantity` is applied only by `try_reserve_pending` once the
/// counterparty has committed. This keeps one-sided proposals from tying up
/// inventory for the full server timeout window.
pub struct SettlementOrderEntry {
    pub order_id: u64,
    pub quantity: Decimal,
    pub reserved: bool,
}

/// Order tracker with immutable start_time and Ed25519 key
pub struct OrderTracker {
    start_time_ms: u64,
    private_key: crate::secret::Secret<32>,
    orders: HashMap<u64, TrackedOrder>,
    /// Maps proposal_id → adoption record for active settlements
    settlement_orders: HashMap<String, SettlementOrderEntry>,
    /// Placements submitted but not yet tracked, per market
    placing: PlacingCounts,
    /// Last submit per market that failed at the transport; the server may still have booked it
    submit_failed_at: HashMap<String, Instant>,
    /// Failed submits by the time they failed, oldest first
    failed_submits: VecDeque<(Instant, FailedSubmit)>,
    #[cfg(test)]
    fail_signing: bool,
}

impl OrderTracker {
    /// Create a new order tracker with immutable start time
    pub fn new(start_time_ms: u64, private_key: crate::secret::Secret<32>) -> Self {
        Self {
            start_time_ms,
            private_key,
            orders: HashMap::new(),
            settlement_orders: HashMap::new(),
            placing: Arc::new(Mutex::new(HashMap::new())),
            submit_failed_at: HashMap::new(),
            failed_submits: VecDeque::new(),
            #[cfg(test)]
            fail_signing: false,
        }
    }

    /// Mark an order placement in flight for `market_id` until the guard drops.
    pub fn begin_placement(&self, market_id: &str) -> PlacementGuard {
        {
            let mut placing = self.placing.lock().unwrap_or_else(PoisonError::into_inner);
            let count = placing.entry(market_id.to_string()).or_insert(0);
            *count = count.saturating_add(1);
        }
        PlacementGuard {
            placing: Arc::clone(&self.placing),
            market_id: market_id.to_string(),
        }
    }

    /// True while any placement guard for `market_id` is alive.
    pub fn placement_in_flight(&self, market_id: &str) -> bool {
        let placing = self.placing.lock().unwrap_or_else(PoisonError::into_inner);
        placing.get(market_id).is_some_and(|count| *count > 0)
    }

    /// Record a submit that returned an error at `now`; its payload is kept so the
    /// order can be adopted if the server booked it (see [`Self::adopt_failed_submit`]).
    pub fn note_submit_failed(&mut self, submit: FailedSubmit, now: Instant) {
        self.submit_failed_at
            .retain(|_, at| now.saturating_duration_since(*at) < SUBMIT_FAILED_HOLD);
        self.submit_failed_at.insert(submit.market_id.clone(), now);
        self.prune_failed_submits(now);
        while self.failed_submits.len() >= FAILED_SUBMIT_MAX {
            self.failed_submits.pop_front();
        }
        self.failed_submits.push_back((now, submit));
    }

    fn prune_failed_submits(&mut self, now: Instant) {
        self.failed_submits
            .retain(|(at, _)| now.saturating_duration_since(*at) < FAILED_SUBMIT_ADOPT);
    }

    /// Track a server order booked by one of our failed submits, like any agent order.
    /// True if `order` matched a kept submit by nonce, signature and signed data.
    pub fn adopt_failed_submit(&mut self, order: &Order, now: Instant) -> bool {
        self.prune_failed_submits(now);
        if self.orders.contains_key(&order.order_id) {
            return false;
        }
        let Some(pos) = self.failed_submits.iter().position(|(_, s)| {
            s.nonce == order.nonce
                && s.market_id == order.market_id
                && order.signature.as_deref() == Some(s.signature.as_str())
                && s.signed_data == order.signed_data
        }) else {
            return false;
        };
        let Some((_, s)) = self.failed_submits.remove(pos) else {
            return false;
        };
        info!(
            "Adopted order {} booked by a failed submit: market={}, price={}, qty={}",
            order.order_id, s.market_id, s.price, s.quantity
        );
        self.track_order(
            order.order_id, &s.market_id, s.order_type,
            &s.price, &s.quantity, s.nonce, &s.signature, &s.signed_data,
        );
        true
    }

    /// [`Self::adopt_failed_submit`] over listed orders; returns how many were adopted.
    pub fn adopt_listed(&mut self, orders: &[Order], now: Instant) -> usize {
        if self.failed_submits.is_empty() {
            return 0;
        }
        orders.iter().filter(|o| self.adopt_failed_submit(o, now)).count()
    }

    /// Failed submits still kept for adoption, oldest first.
    #[cfg(test)]
    pub(crate) fn failed_submits(&self) -> Vec<FailedSubmit> {
        self.failed_submits.iter().map(|(_, s)| s.clone()).collect()
    }

    /// End every failed-submit hold now, keeping the payloads.
    #[cfg(test)]
    pub(crate) fn expire_submit_holds(&mut self) {
        self.submit_failed_at.clear();
    }

    /// True within [`SUBMIT_FAILED_HOLD`] of a failed submit for `market_id`.
    fn submit_failed_recently(&self, market_id: &str, now: Instant) -> bool {
        self.submit_failed_at
            .get(market_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < SUBMIT_FAILED_HOLD)
    }

    /// Make the next `sign_order` calls fail (test hook for error paths).
    #[cfg(test)]
    pub(crate) fn set_fail_signing(&mut self, fail: bool) {
        self.fail_signing = fail;
    }

    /// Sign order data and return (signature, signed_data_bytes, nonce)
    ///
    /// Creates canonical JSON with sorted keys, signs with Ed25519.
    pub fn sign_order(
        &self,
        market_id: &str,
        order_type: &str,
        price: &str,
        quantity: &str,
    ) -> Result<(String, Vec<u8>, u64)> {
        #[cfg(test)]
        if self.fail_signing {
            anyhow::bail!("order signing disabled for test");
        }

        let nonce = chrono::Utc::now().timestamp_millis() as u64;

        // Canonical JSON with sorted keys (BTreeMap guarantees alphabetical order)
        let mut fields = BTreeMap::new();
        fields.insert("market_id", serde_json::Value::String(market_id.to_string()));
        fields.insert("nonce", serde_json::json!(nonce));
        fields.insert("order_type", serde_json::Value::String(order_type.to_string()));
        fields.insert("placed_by", serde_json::Value::String("agent".to_string()));
        fields.insert("price", serde_json::Value::String(price.to_string()));
        fields.insert("quantity", serde_json::Value::String(quantity.to_string()));
        let signed_data_bytes =
            serde_json::to_vec(&fields).context("failed to serialize order for signing")?;
        let signature = sign_order_data(&self.private_key.expose(), &signed_data_bytes);

        Ok((signature, signed_data_bytes, nonce))
    }

    /// Track an order placed by the agent
    pub fn track_order(
        &mut self,
        order_id: u64,
        market_id: &str,
        order_type: i32,
        price: &str,
        quantity: &str,
        nonce: u64,
        signature: &str,
        signed_data: &[u8],
    ) {
        let order = TrackedOrder {
            order_id,
            market_id: market_id.to_string(),
            order_type,
            price: Decimal::from_str(price).unwrap_or_default(),
            quantity: Decimal::from_str(quantity).unwrap_or_default(),
            settled_quantity: Decimal::ZERO,
            pending_quantity: Decimal::ZERO,
            nonce,
            signature: signature.to_string(),
            signed_data: signed_data.to_vec(),
            placed_by: "agent".to_string(),
            is_active: true,
        };
        debug!("Tracking agent order: id={}, market={}, type={}, price={}, qty={}",
            order_id, market_id, order_type, price, quantity);
        self.orders.insert(order_id, order);
    }

    /// Import a server-fetched order (user order) into the tracker after verification
    fn import_order_from_server(&mut self, order: &Order) {
        let order_id = order.order_id;
        if self.orders.contains_key(&order_id) {
            return; // Already tracked
        }

        let tracked = TrackedOrder {
            order_id,
            market_id: order.market_id.clone(),
            order_type: order.order_type,
            price: Decimal::from_str(&order.price).unwrap_or_default(),
            quantity: Decimal::from_str(&order.quantity).unwrap_or_default(),
            settled_quantity: Decimal::from_str(&order.filled_quantity).unwrap_or_default(),
            pending_quantity: Decimal::from_str(&order.pending_quantity).unwrap_or_default(),
            nonce: order.nonce,
            signature: order.signature.clone().unwrap_or_default(),
            signed_data: order.signed_data.clone(),
            placed_by: "user".to_string(),
            is_active: true,
        };
        info!("Imported user order from server: id={}, market={}, price={}, qty={}",
            order_id, &order.market_id, &order.price, &order.quantity);
        self.orders.insert(order_id, tracked);
    }

    /// Verify a settlement proposal against tracked orders
    ///
    /// Returns Accepted or Rejected for tracked orders; for untracked ones
    /// PlacementInFlight (market placement pending) or NeedServerLookup.
    pub fn verify_settlement(
        &self,
        proposal: &SettlementProposal,
        our_party: &str,
    ) -> VerifyResult {
        // Determine our side and extract order_id from order_match
        let order_match = match &proposal.order_match {
            Some(om) => om,
            None => {
                return VerifyResult::Rejected {
                    reason: "No order_match data in proposal".to_string(),
                };
            }
        };

        let is_buyer = proposal.buyer == our_party;
        let order_id = if is_buyer {
            order_match.bid_order_id
        } else {
            order_match.offer_order_id
        };

        // Path A: Check internal tracker
        if let Some(tracked) = self.orders.get(&order_id) {
            return self.verify_tracked_order(tracked, proposal, order_id);
        }

        // A fresh agent order can match before it is tracked, or after a failed submit; hold, don't look up
        if self.placement_in_flight(&proposal.market_id)
            || self.submit_failed_recently(&proposal.market_id, Instant::now())
        {
            return VerifyResult::PlacementInFlight { order_id };
        }

        // Path B: Order not in tracker — need server lookup
        VerifyResult::NeedServerLookup { order_id }
    }

    /// Verify a tracked order against a settlement proposal
    fn verify_tracked_order(
        &self,
        tracked: &TrackedOrder,
        proposal: &SettlementProposal,
        order_id: u64,
    ) -> VerifyResult {
        // Allow settlements for cancelled orders — the match happened on the server
        // before cancellation. Signature + nonce + capacity checks are sufficient.
        if !tracked.is_active {
            info!("Order {} is cancelled but accepting settlement (match happened before cancellation)", order_id);
        }

        // Verify signature
        if !verify_order_signature(
            &self.private_key.expose(),
            &tracked.signed_data,
            &tracked.signature,
        ) {
            return VerifyResult::Rejected {
                reason: format!("Order {} has invalid signature", order_id),
            };
        }

        // Verify nonce > start_time
        if tracked.nonce <= self.start_time_ms {
            return VerifyResult::Rejected {
                reason: format!(
                    "Order {} nonce {} <= start_time {}",
                    order_id, tracked.nonce, self.start_time_ms
                ),
            };
        }

        // Verify remaining capacity
        check_capacity(tracked, order_id, &proposal.base_quantity)
    }

    /// Verify a server-fetched order and import it into tracker if valid
    ///
    /// Called for user orders that weren't in the internal tracker.
    pub fn verify_and_import_order(
        &mut self,
        order: &Order,
        proposal: &SettlementProposal,
    ) -> VerifyResult {
        let order_id = order.order_id;

        // Must have signature
        let signature = match &order.signature {
            Some(sig) if !sig.is_empty() => sig.clone(),
            _ => {
                return VerifyResult::Rejected {
                    reason: format!("Order {} has no signature", order_id),
                };
            }
        };

        // Verify signature with our key
        if !verify_order_signature(
            &self.private_key.expose(),
            &order.signed_data,
            &signature,
        ) {
            return VerifyResult::Rejected {
                reason: format!("Order {} signature verification failed (not signed by our key)", order_id),
            };
        }

        // Verify nonce > start_time
        if order.nonce <= self.start_time_ms {
            return VerifyResult::Rejected {
                reason: format!(
                    "Order {} nonce {} <= start_time {} (stale order)",
                    order_id, order.nonce, self.start_time_ms
                ),
            };
        }

        // Server-supplied amounts are never negative; such an order is not imported
        let negative = [&order.quantity, &order.filled_quantity, &order.pending_quantity]
            .into_iter()
            .any(|raw| Decimal::from_str(raw).is_ok_and(|v| v < Decimal::ZERO));
        if negative {
            return VerifyResult::Rejected {
                reason: format!("Order {order_id} has a negative quantity"),
            };
        }

        // Import into tracker
        self.import_order_from_server(order);

        // Now verify remaining capacity against the proposal
        if let Some(tracked) = self.orders.get(&order_id) {
            return check_capacity(tracked, order_id, &proposal.base_quantity);
        }

        VerifyResult::Accepted { order_id }
    }

    /// Record the local adoption decision for a settlement (at adoption time,
    /// after verification). Does NOT touch `pending_quantity` — the capacity
    /// reservation is deferred to `try_reserve_pending` once the counterparty
    /// has preconfirmed. Idempotent: an existing entry (e.g. restored from
    /// saved state) is left untouched, preserving its `reserved` flag.
    pub fn record_settlement_order(&mut self, proposal_id: &str, order_id: u64, quantity: Decimal) {
        if self.settlement_orders.contains_key(proposal_id) {
            return;
        }
        debug!(
            "[{}] Recorded settlement order mapping: order={}, qty={} (unreserved)",
            proposal_id, order_id, quantity
        );
        self.settlement_orders.insert(
            proposal_id.to_string(),
            SettlementOrderEntry { order_id, quantity, reserved: false },
        );
    }

    /// Reserve the order capacity for an adopted settlement — called on the
    /// first post-preconfirm action (i.e. once the counterparty has committed).
    ///
    /// Returns `Ok(true)` if newly reserved, `Ok(false)` if already reserved
    /// (idempotent re-entry, incl. entries restored as reserved from saved
    /// state), `Err` if the proposal was never adopted or the order no longer
    /// has capacity. The capacity re-check is the atomic backstop for
    /// adoption-time checks that overlapped while nothing was reserved.
    pub fn try_reserve_pending(&mut self, proposal_id: &str) -> Result<bool, String> {
        let entry = self
            .settlement_orders
            .get_mut(proposal_id)
            .ok_or_else(|| "settlement not adopted (no local settlement order record)".to_string())?;
        if entry.reserved {
            return Ok(false);
        }
        if entry.order_id != 0 {
            if let Some(order) = self.orders.get(&entry.order_id) {
                let remaining = remaining_capacity(order).ok_or_else(|| {
                    format!("order {} capacity arithmetic overflow at reservation", entry.order_id)
                })?;
                if remaining < entry.quantity {
                    return Err(format!(
                        "order {} insufficient capacity at reservation: remaining={}, requested={}",
                        entry.order_id, remaining, entry.quantity
                    ));
                }
            }
        }
        entry.reserved = true;
        let (order_id, quantity) = (entry.order_id, entry.quantity);
        if let Some(order) = self.orders.get_mut(&order_id) {
            order.pending_quantity = order.pending_quantity.saturating_add(quantity);
            info!(
                "Order {} pending += {} (total pending: {}, settled: {})",
                order_id, quantity, order.pending_quantity, order.settled_quantity
            );
        }
        Ok(true)
    }

    /// Mark settlement as completed — move pending → settled
    pub fn mark_settled(&mut self, proposal_id: &str) {
        if let Some(entry) = self.settlement_orders.remove(proposal_id) {
            // A settlement only reaches Settled after reservation; the guard
            // protects the accounting if a terminal ever arrives earlier.
            if !entry.reserved {
                return;
            }
            if let Some(order) = self.orders.get_mut(&entry.order_id) {
                order.pending_quantity =
                    order.pending_quantity.saturating_sub(entry.quantity).max(Decimal::ZERO);
                order.settled_quantity = order.settled_quantity.saturating_add(entry.quantity);
                info!(
                    "[{}] Order {} settled: {} (pending={}, settled={})",
                    proposal_id, entry.order_id, entry.quantity, order.pending_quantity, order.settled_quantity
                );
            }
        }
    }

    /// Mark settlement as failed — release pending quantity (if it was reserved)
    pub fn mark_failed(&mut self, proposal_id: &str) {
        if let Some(entry) = self.settlement_orders.remove(proposal_id) {
            if !entry.reserved {
                debug!("[{}] Settlement failed before reservation — nothing to release", proposal_id);
                return;
            }
            if let Some(order) = self.orders.get_mut(&entry.order_id) {
                order.pending_quantity =
                    order.pending_quantity.saturating_sub(entry.quantity).max(Decimal::ZERO);
                warn!(
                    "[{}] Order {} settlement failed: released {} pending (now pending={}, settled={})",
                    proposal_id, entry.order_id, entry.quantity, order.pending_quantity, order.settled_quantity
                );
            }
        }
    }

    /// Export tracker state for persistence
    ///
    /// Returns (start_time_ms, orders, settlement_orders) as serializable types.
    pub fn export_state(&self) -> (u64, Vec<SavedTrackedOrder>, Vec<SavedSettlementOrder>) {
        let orders: Vec<SavedTrackedOrder> = self
            .orders
            .values()
            .map(|o| SavedTrackedOrder {
                order_id: o.order_id,
                market_id: o.market_id.clone(),
                order_type: o.order_type,
                price: o.price.to_string(),
                quantity: o.quantity.to_string(),
                settled_quantity: o.settled_quantity.to_string(),
                pending_quantity: o.pending_quantity.to_string(),
                nonce: o.nonce,
                signature: o.signature.clone(),
                signed_data: base64::engine::general_purpose::STANDARD.encode(&o.signed_data),
                placed_by: o.placed_by.clone(),
                is_active: o.is_active,
            })
            .collect();

        let settlement_orders: Vec<SavedSettlementOrder> = self
            .settlement_orders
            .iter()
            .map(|(proposal_id, entry)| SavedSettlementOrder {
                proposal_id: proposal_id.clone(),
                order_id: entry.order_id,
                quantity: entry.quantity.to_string(),
                reserved: entry.reserved,
            })
            .collect();

        (self.start_time_ms, orders, settlement_orders)
    }

    /// Import previously saved state into this tracker
    ///
    /// Used on restart to restore order verification and quantity accounting.
    pub fn import_state(
        &mut self,
        orders: Vec<SavedTrackedOrder>,
        settlement_orders: Vec<SavedSettlementOrder>,
    ) {
        for saved in orders {
            let signed_data = base64::engine::general_purpose::STANDARD
                .decode(&saved.signed_data)
                .unwrap_or_default();
            let order = TrackedOrder {
                order_id: saved.order_id,
                market_id: saved.market_id,
                order_type: saved.order_type,
                price: Decimal::from_str(&saved.price).unwrap_or_default(),
                quantity: Decimal::from_str(&saved.quantity).unwrap_or_default(),
                settled_quantity: Decimal::from_str(&saved.settled_quantity).unwrap_or_default(),
                pending_quantity: Decimal::from_str(&saved.pending_quantity).unwrap_or_default(),
                nonce: saved.nonce,
                signature: saved.signature,
                signed_data,
                placed_by: saved.placed_by,
                is_active: saved.is_active,
            };
            self.orders.insert(order.order_id, order);
        }

        for saved in settlement_orders {
            let quantity = Decimal::from_str(&saved.quantity).unwrap_or_default();
            self.settlement_orders.insert(
                saved.proposal_id,
                SettlementOrderEntry {
                    order_id: saved.order_id,
                    quantity,
                    // Legacy state files predate the reserved flag; their
                    // entries all had pending_quantity applied (serde default
                    // = true preserves that accounting through the upgrade).
                    reserved: saved.reserved,
                },
            );
        }

        info!(
            "Restored {} order(s) and {} settlement mapping(s) from saved state",
            self.orders.len(),
            self.settlement_orders.len()
        );
    }

    /// Check if a proposal is already tracked in settlement_orders.
    /// Used on restart to skip re-verification of restored proposals.
    pub fn has_settlement_order(&self, proposal_id: &str) -> bool {
        self.settlement_orders.contains_key(proposal_id)
    }

    /// Cancel a specific order
    pub fn cancel_order(&mut self, order_id: u64) {
        if let Some(order) = self.orders.get_mut(&order_id) {
            order.is_active = false;
        }
    }

    /// Cancel all tracked orders
    pub fn cancel_all(&mut self) {
        for order in self.orders.values_mut() {
            order.is_active = false;
        }
    }
}

/// Quantity still free on an order; None if the amounts fall outside the Decimal range.
fn remaining_capacity(order: &TrackedOrder) -> Option<Decimal> {
    order
        .quantity
        .checked_sub(order.settled_quantity)?
        .checked_sub(order.pending_quantity)
}

/// Accepted when `requested` is a positive quantity within the order's remaining capacity.
fn check_capacity(order: &TrackedOrder, order_id: u64, requested: &str) -> VerifyResult {
    let base_quantity = Decimal::from_str(requested).unwrap_or_default();
    if base_quantity <= Decimal::ZERO {
        return VerifyResult::Rejected {
            reason: format!("Order {order_id} invalid requested quantity {requested:?}"),
        };
    }
    match remaining_capacity(order) {
        None => VerifyResult::Rejected {
            reason: format!("Order {order_id} capacity arithmetic overflow"),
        },
        Some(remaining) if remaining < base_quantity => VerifyResult::Rejected {
            reason: format!(
                "Order {order_id} insufficient capacity: remaining={remaining}, requested={base_quantity}"
            ),
        },
        Some(_) => VerifyResult::Accepted { order_id },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::sign_order_data;
    use orderbook_proto::orderbook::OrderType;

    fn test_private_key() -> [u8; 32] {
        [
            0x0f, 0xe6, 0x65, 0xf7, 0xed, 0xb1, 0x93, 0xdb,
            0x35, 0xcc, 0x37, 0xd7, 0xd7, 0x03, 0xe1, 0x2a,
            0xe9, 0x4e, 0x9e, 0x1c, 0x5f, 0x5b, 0x88, 0x57,
            0xae, 0x1b, 0x6a, 0xca, 0x00, 0x5d, 0xf1, 0x5b,
        ]
    }

    fn make_proposal(buyer: &str, seller: &str, base_qty: &str, bid_order_id: u64, offer_order_id: u64) -> SettlementProposal {
        use orderbook_proto::orderbook::OrderMatch;
        SettlementProposal {
            proposal_id: "test-proposal-1".to_string(),
            market_id: "BTC-USD".to_string(),
            buyer: buyer.to_string(),
            seller: seller.to_string(),
            base_instrument: "BTC".to_string(),
            quote_instrument: "USD".to_string(),
            base_quantity: base_qty.to_string(),
            quote_quantity: "1000.0".to_string(),
            settlement_price: "100.50".to_string(),
            dvp_processing_fee_buyer: "0".to_string(),
            dvp_processing_fee_seller: "0".to_string(),
            allocation_processing_fee_buyer: "0".to_string(),
            allocation_processing_fee_seller: "0".to_string(),
            status: 0,
            error_message: None,
            created_at: None,
            settled_at: None,
            cancelled_at: None,
            failed_at: None,
            order_match: Some(OrderMatch {
                settlement_proposal_id: "test-proposal-1".to_string(),
                bid_order_id,
                offer_order_id,
                matched_quantity: base_qty.to_string(),
                matched_price: "100.50".to_string(),
                created_at: None,
            }),
            dvp_proposer_party: None,
            origin: String::new(),
        }
    }

    #[test]
    fn test_sign_and_verify_order() {
        let key = test_private_key();
        let tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "1.0").unwrap();

        assert!(!signature.is_empty());
        assert!(!signed_data.is_empty());
        assert!(nonce > 1000);

        // Verify the signature
        assert!(verify_order_signature(&key, &signed_data, &signature));

        // Tampered data should fail
        let mut tampered = signed_data.clone();
        tampered[0] ^= 0xFF;
        assert!(!verify_order_signature(&key, &tampered, &signature));
    }

    #[test]
    fn test_settlement_matching_agent_order() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5.0").unwrap();

        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "5.0", nonce, &signature, &signed_data);

        let proposal = make_proposal("our-party", "counterparty", "2.0", 42, 99);

        match tracker.verify_settlement(&proposal, "our-party") {
            VerifyResult::Accepted { order_id } => assert_eq!(order_id, 42),
            other => panic!("Expected Accepted, got {:?}", match other {
                VerifyResult::Rejected { reason } => format!("Rejected: {}", reason),
                VerifyResult::NeedServerLookup { order_id } => format!("NeedServerLookup: {}", order_id),
                _ => "unknown".to_string(),
            }),
        }
    }

    #[test]
    fn test_quantity_tracking() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "5.0", nonce, &signature, &signed_data);

        // First settlement: 2.0
        let proposal1 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&proposal1, "our-party"), VerifyResult::Accepted { .. }));
        tracker.record_settlement_order("proposal-1", 42, Decimal::from_str("2.0").unwrap());
        assert!(tracker.try_reserve_pending("proposal-1").unwrap());

        // Second settlement: 2.0 (total pending = 4.0, remaining = 1.0)
        let mut proposal2 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        proposal2.proposal_id = "test-proposal-2".to_string();
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));
        tracker.record_settlement_order("proposal-2", 42, Decimal::from_str("2.0").unwrap());
        assert!(tracker.try_reserve_pending("proposal-2").unwrap());

        // Third settlement: 2.0 should fail (remaining = 1.0)
        let mut proposal3 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        proposal3.proposal_id = "test-proposal-3".to_string();
        assert!(matches!(tracker.verify_settlement(&proposal3, "our-party"), VerifyResult::Rejected { .. }));

        // Fail first settlement (releases pending capacity), then third should work
        tracker.mark_failed("proposal-1");
        // Now: settled=0, pending=2.0, remaining=3.0 — enough for 2.0
        assert!(matches!(tracker.verify_settlement(&proposal3, "our-party"), VerifyResult::Accepted { .. }));
    }

    #[test]
    fn test_stale_nonce_rejected() {
        let key = test_private_key();
        let start_time = chrono::Utc::now().timestamp_millis() as u64;
        let mut tracker = OrderTracker::new(start_time, crate::secret::Secret::seal(&mut { key }));

        // Create signed data with old nonce
        let old_nonce = start_time - 1000; // Before start_time
        let signed_data = serde_json::to_vec(&serde_json::json!({
            "market_id": "BTC-USD",
            "nonce": old_nonce,
            "order_type": "bid",
            "placed_by": "agent",
            "price": "100.50",
            "quantity": "1.0",
        })).unwrap();
        let signature = sign_order_data(&key, &signed_data);

        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "1.0", old_nonce, &signature, &signed_data);

        let proposal = make_proposal("our-party", "counterparty", "1.0", 42, 99);

        match tracker.verify_settlement(&proposal, "our-party") {
            VerifyResult::Rejected { reason } => assert!(reason.contains("nonce"), "Expected nonce rejection, got: {}", reason),
            _ => panic!("Expected Rejected for stale nonce"),
        }
    }

    #[test]
    fn test_unknown_order_needs_server_lookup() {
        let key = test_private_key();
        let tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        // Order 42 is not in the tracker
        let proposal = make_proposal("our-party", "counterparty", "1.0", 42, 99);

        match tracker.verify_settlement(&proposal, "our-party") {
            VerifyResult::NeedServerLookup { order_id } => assert_eq!(order_id, 42),
            _ => panic!("Expected NeedServerLookup"),
        }
    }

    #[test]
    fn test_invalid_signature_rejected() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let nonce = chrono::Utc::now().timestamp_millis() as u64;
        let signed_data = serde_json::to_vec(&serde_json::json!({
            "market_id": "BTC-USD",
            "nonce": nonce,
            "order_type": "bid",
            "placed_by": "agent",
            "price": "100.50",
            "quantity": "1.0",
        })).unwrap();

        // Use a different key to sign
        let wrong_key: [u8; 32] = [0xAA; 32];
        let bad_signature = sign_order_data(&wrong_key, &signed_data);

        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "1.0", nonce, &bad_signature, &signed_data);

        let proposal = make_proposal("our-party", "counterparty", "1.0", 42, 99);

        match tracker.verify_settlement(&proposal, "our-party") {
            VerifyResult::Rejected { reason } => assert!(reason.contains("signature"), "Expected signature rejection, got: {}", reason),
            _ => panic!("Expected Rejected for invalid signature"),
        }
    }

    #[test]
    fn test_failed_settlement_releases_pending() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "3.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "3.0", nonce, &signature, &signed_data);

        // Pending 2.0
        tracker.record_settlement_order("proposal-1", 42, Decimal::from_str("2.0").unwrap());
        assert!(tracker.try_reserve_pending("proposal-1").unwrap());

        // Can't fit another 2.0 (remaining = 1.0)
        let proposal2 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Rejected { .. }));

        // Fail the first settlement
        tracker.mark_failed("proposal-1");

        // Now 2.0 fits again (remaining = 3.0)
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));
    }

    // Deferred reservation: recording the adoption decision must not consume
    // order capacity; only try_reserve_pending (counterparty committed) does.
    #[test]
    fn test_record_does_not_consume_capacity() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "3.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "3.0", nonce, &signature, &signed_data);

        tracker.record_settlement_order("proposal-1", 42, Decimal::from_str("2.0").unwrap());
        assert!(tracker.has_settlement_order("proposal-1"));

        // Capacity untouched: another 2.0 still verifies (remaining = 3.0)
        let proposal2 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));

        // Unreserved failure releases nothing and removes the record
        tracker.mark_failed("proposal-1");
        assert!(!tracker.has_settlement_order("proposal-1"));
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));
    }

    #[test]
    fn test_try_reserve_pending_idempotent_and_missing() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "5.0", nonce, &signature, &signed_data);

        // Missing entry → invariant error
        assert!(tracker.try_reserve_pending("nope").is_err());

        tracker.record_settlement_order("proposal-1", 42, Decimal::from_str("2.0").unwrap());
        assert!(tracker.try_reserve_pending("proposal-1").unwrap()); // newly reserved
        assert!(!tracker.try_reserve_pending("proposal-1").unwrap()); // idempotent re-entry

        // pending applied exactly once: a 3.0 proposal fits (remaining = 3.0),
        // a 4.0 proposal would not.
        let fits = make_proposal("our-party", "counterparty", "3.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&fits, "our-party"), VerifyResult::Accepted { .. }));
    }

    // The reservation-time capacity re-check is the atomic backstop for
    // adoption-time advisory checks that overlapped while nothing was reserved.
    #[test]
    fn test_reserve_capacity_backstop() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "3.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "3.0", nonce, &signature, &signed_data);

        // Both adopted while capacity looked fine (nothing reserved yet)
        tracker.record_settlement_order("proposal-1", 42, Decimal::from_str("2.0").unwrap());
        tracker.record_settlement_order("proposal-2", 42, Decimal::from_str("2.0").unwrap());

        assert!(tracker.try_reserve_pending("proposal-1").unwrap());
        // Second reservation exceeds remaining (1.0 < 2.0) → rejected
        assert!(tracker.try_reserve_pending("proposal-2").is_err());

        // RFQ-style entries (order_id = 0) skip the capacity check
        tracker.record_settlement_order("rfq-1", 0, Decimal::from_str("9.9").unwrap());
        assert!(tracker.try_reserve_pending("rfq-1").unwrap());
    }

    fn market_proposal(market_id: &str, offer_order_id: u64) -> SettlementProposal {
        let mut p = make_proposal("counterparty", "our-party", "1.0", 7, offer_order_id);
        p.market_id = market_id.to_string();
        p
    }

    #[test]
    fn test_placement_guard_holds_untracked_orders_in_its_market_only() {
        let key = test_private_key();
        let tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let guard = tracker.begin_placement("BTC-USD");
        assert!(tracker.placement_in_flight("BTC-USD"));
        match tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party") {
            VerifyResult::PlacementInFlight { order_id } => assert_eq!(order_id, 42),
            _ => panic!("expected PlacementInFlight for the guarded market"),
        }
        // Other markets still go to the server lookup
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("ETH-USD", 42), "our-party"),
            VerifyResult::NeedServerLookup { order_id: 42 }
        ));

        drop(guard);
        assert!(!tracker.placement_in_flight("BTC-USD"));
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party"),
            VerifyResult::NeedServerLookup { order_id: 42 }
        ));
    }

    #[test]
    fn test_placement_guard_counts_nested_placements() {
        let key = test_private_key();
        let tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let first = tracker.begin_placement("BTC-USD");
        let second = tracker.begin_placement("BTC-USD");
        drop(first);
        assert!(tracker.placement_in_flight("BTC-USD"));
        drop(second);
        assert!(!tracker.placement_in_flight("BTC-USD"));
    }

    #[test]
    fn test_tracked_order_wins_over_placement_guard() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));

        let guard = tracker.begin_placement("BTC-USD");
        let (signature, signed_data, nonce) =
            tracker.sign_order("BTC-USD", "offer", "100.50", "5.0").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Offer as i32, "100.50", "5.0", nonce, &signature, &signed_data);

        // The guard is still alive (the next rung is placing); the tracked order wins
        assert!(tracker.placement_in_flight("BTC-USD"));
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party"),
            VerifyResult::Accepted { order_id: 42 }
        ));
        drop(guard);
    }

    /// A signed offer on `market_id` whose submit returned an error.
    fn failed_offer(tracker: &OrderTracker, market_id: &str, quantity: &str) -> FailedSubmit {
        let (signature, signed_data, nonce) =
            tracker.sign_order(market_id, "offer", "100.50", quantity).unwrap();
        FailedSubmit {
            market_id: market_id.to_string(),
            order_type: OrderType::Offer as i32,
            price: "100.50".to_string(),
            quantity: quantity.to_string(),
            nonce,
            signature,
            signed_data,
        }
    }

    /// How the server lists a booked order whose whole quantity is matched but unsettled.
    fn booked(order_id: u64, s: &FailedSubmit) -> Order {
        Order {
            order_id,
            market_id: s.market_id.clone(),
            order_type: s.order_type,
            price: s.price.clone(),
            quantity: s.quantity.clone(),
            filled_quantity: "0".to_string(),
            pending_quantity: s.quantity.clone(),
            nonce: s.nonce,
            signature: Some(s.signature.clone()),
            signed_data: s.signed_data.clone(),
            ..Default::default()
        }
    }

    #[test]
    fn test_failed_submit_holds_untracked_orders_for_a_while() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));
        let now = std::time::Instant::now();

        let submit = failed_offer(&tracker, "BTC-USD", "1.0");
        tracker.note_submit_failed(submit, now);
        assert!(!tracker.placement_in_flight("BTC-USD"), "no guard is involved");
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party"),
            VerifyResult::PlacementInFlight { order_id: 42 }
        ));
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("ETH-USD", 42), "our-party"),
            VerifyResult::NeedServerLookup { order_id: 42 }
        ));

        // A tracked order still verifies normally
        let (signature, signed_data, nonce) =
            tracker.sign_order("BTC-USD", "offer", "100.50", "5.0").unwrap();
        tracker.track_order(43, "BTC-USD", OrderType::Offer as i32, "100.50", "5.0", nonce, &signature, &signed_data);
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("BTC-USD", 43), "our-party"),
            VerifyResult::Accepted { order_id: 43 }
        ));

        // Past the window the lookup resumes
        if let Some(old) = now.checked_sub(SUBMIT_FAILED_HOLD + Duration::from_secs(1)) {
            let submit = failed_offer(&tracker, "BTC-USD", "1.0");
            tracker.note_submit_failed(submit, old);
            assert!(matches!(
                tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party"),
                VerifyResult::NeedServerLookup { order_id: 42 }
            ));
        }
    }

    #[test]
    fn test_order_booked_by_a_failed_submit_is_adopted_with_fresh_capacity() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));
        let now = Instant::now();
        let submit = failed_offer(&tracker, "BTC-USD", "1.0");
        tracker.note_submit_failed(submit.clone(), now);

        // Another market, nonce or signature is not ours to adopt
        let mut other = booked(42, &submit);
        other.market_id = "ETH-USD".to_string();
        assert!(!tracker.adopt_failed_submit(&other, now));
        let mut other = booked(42, &submit);
        other.nonce = submit.nonce.wrapping_add(1);
        assert!(!tracker.adopt_failed_submit(&other, now));
        let mut other = booked(42, &submit);
        other.signature = Some("forged".to_string());
        assert!(!tracker.adopt_failed_submit(&other, now));

        // Ours is tracked with nothing settled or pending, so its match fits; adopted once
        assert!(tracker.adopt_failed_submit(&booked(42, &submit), now));
        assert!(tracker.failed_submits().is_empty());
        assert!(matches!(
            tracker.verify_settlement(&market_proposal("BTC-USD", 42), "our-party"),
            VerifyResult::Accepted { order_id: 42 }
        ));
        assert!(!tracker.adopt_failed_submit(&booked(42, &submit), now));
    }

    #[test]
    fn test_failed_submits_are_kept_for_a_window_and_a_bounded_count() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));
        let now = Instant::now();
        let first = failed_offer(&tracker, "BTC-USD", "1.0");
        tracker.note_submit_failed(first.clone(), now);
        for _ in 0..FAILED_SUBMIT_MAX {
            let submit = failed_offer(&tracker, "BTC-USD", "2.0");
            tracker.note_submit_failed(submit, now);
        }
        assert_eq!(tracker.failed_submits().len(), FAILED_SUBMIT_MAX);
        assert!(!tracker.adopt_failed_submit(&booked(1, &first), now), "the oldest was dropped");

        if let Some(later) = now.checked_add(FAILED_SUBMIT_ADOPT) {
            let last = tracker.failed_submits().pop().unwrap();
            assert!(!tracker.adopt_failed_submit(&booked(2, &last), later));
            assert!(tracker.failed_submits().is_empty());
        }
    }

    #[test]
    fn test_server_order_with_out_of_range_amounts_is_rejected_without_panic() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));
        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "1").unwrap();
        let order = Order {
            order_id: 42,
            market_id: "BTC-USD".to_string(),
            order_type: OrderType::Bid as i32,
            price: "100.50".to_string(),
            quantity: "1".to_string(),
            filled_quantity: "0".to_string(),
            pending_quantity: "-79228162514264337593543950335".to_string(),
            nonce,
            signature: Some(signature),
            signed_data,
            ..Default::default()
        };
        let proposal = make_proposal("our-party", "counterparty", "1", 42, 99);
        assert!(matches!(
            tracker.verify_and_import_order(&order, &proposal),
            VerifyResult::Rejected { .. }
        ));
        assert!(!tracker.orders.contains_key(&42));

        // Small negatives would otherwise inflate the capacity to 6
        for (id, filled, pending) in [(43u64, "0", "-5"), (44, "-5", "0")] {
            let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "1").unwrap();
            let order = Order {
                order_id: id,
                market_id: "BTC-USD".to_string(),
                order_type: OrderType::Bid as i32,
                price: "100.50".to_string(),
                quantity: "1".to_string(),
                filled_quantity: filled.to_string(),
                pending_quantity: pending.to_string(),
                nonce,
                signature: Some(signature),
                signed_data,
                ..Default::default()
            };
            let proposal = make_proposal("our-party", "counterparty", "5", id, 99);
            assert!(
                matches!(
                    tracker.verify_and_import_order(&order, &proposal),
                    VerifyResult::Rejected { ref reason } if reason.contains("negative")
                ),
                "order {id}"
            );
            assert!(!tracker.orders.contains_key(&id), "order {id}");
        }
    }

    #[test]
    fn test_out_of_range_requested_or_tracked_amounts_are_rejected_without_panic() {
        let key = test_private_key();
        let mut tracker = OrderTracker::new(1000, crate::secret::Secret::seal(&mut { key }));
        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5").unwrap();
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "5", nonce, &signature, &signed_data);

        for requested in ["-79228162514264337593543950335", "0", "junk"] {
            let proposal = make_proposal("our-party", "counterparty", requested, 42, 99);
            assert!(
                matches!(tracker.verify_settlement(&proposal, "our-party"), VerifyResult::Rejected { .. }),
                "requested {requested}"
            );
        }

        // Tracked amounts at the edge of the Decimal range are refused, not computed
        tracker.orders.get_mut(&42).unwrap().pending_quantity = Decimal::MIN;
        let proposal = make_proposal("our-party", "counterparty", "1", 42, 99);
        match tracker.verify_settlement(&proposal, "our-party") {
            VerifyResult::Rejected { reason } => assert!(reason.contains("overflow"), "{reason}"),
            _ => panic!("expected an overflow reject"),
        }
        tracker.record_settlement_order("p-reserve", 42, Decimal::ONE);
        assert!(tracker.try_reserve_pending("p-reserve").is_err());

        // Releasing or settling a reservation saturates instead of overflowing
        tracker.record_settlement_order("p-fail", 42, Decimal::MAX);
        tracker.settlement_orders.get_mut("p-fail").unwrap().reserved = true;
        tracker.mark_failed("p-fail");
        assert_eq!(tracker.orders[&42].pending_quantity, Decimal::ZERO);
        tracker.orders.get_mut(&42).unwrap().settled_quantity = Decimal::MAX;
        tracker.orders.get_mut(&42).unwrap().pending_quantity = Decimal::MIN;
        tracker.record_settlement_order("p-settle", 42, Decimal::MAX);
        tracker.settlement_orders.get_mut("p-settle").unwrap().reserved = true;
        tracker.mark_settled("p-settle");
        assert_eq!(tracker.orders[&42].settled_quantity, Decimal::MAX);
        assert_eq!(tracker.orders[&42].pending_quantity, Decimal::ZERO);
    }
}
