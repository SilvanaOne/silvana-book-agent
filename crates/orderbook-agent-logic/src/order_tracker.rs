//! Order tracking and settlement verification
//!
//! Maintains an in-memory map of all orders placed by the agent, and verifies
//! settlement proposals against them. User orders (placed via frontend) are
//! imported from the server on demand.

use base64::Engine;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
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
}

/// Order tracker with immutable start_time and Ed25519 key
pub struct OrderTracker {
    start_time_ms: u64,
    private_key_bytes: [u8; 32],
    orders: HashMap<u64, TrackedOrder>,
    /// Maps proposal_id → (order_id, pending_quantity) for active settlements
    settlement_orders: HashMap<String, (u64, Decimal)>,
}

impl OrderTracker {
    /// Create a new order tracker with immutable start time
    pub fn new(start_time_ms: u64, private_key_bytes: [u8; 32]) -> Self {
        Self {
            start_time_ms,
            private_key_bytes,
            orders: HashMap::new(),
            settlement_orders: HashMap::new(),
        }
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
    ) -> (String, Vec<u8>, u64) {
        let nonce = chrono::Utc::now().timestamp_millis() as u64;

        // Canonical JSON with sorted keys (BTreeMap guarantees alphabetical order)
        let mut fields = BTreeMap::new();
        fields.insert("market_id", serde_json::Value::String(market_id.to_string()));
        fields.insert("nonce", serde_json::json!(nonce));
        fields.insert("order_type", serde_json::Value::String(order_type.to_string()));
        fields.insert("placed_by", serde_json::Value::String("agent".to_string()));
        fields.insert("price", serde_json::Value::String(price.to_string()));
        fields.insert("quantity", serde_json::Value::String(quantity.to_string()));
        let signed_data_bytes = serde_json::to_vec(&fields).unwrap();
        let signature = sign_order_data(&self.private_key_bytes, &signed_data_bytes);

        (signature, signed_data_bytes, nonce)
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
    /// Returns Accepted if order is in tracker and passes all checks,
    /// NeedServerLookup if order is not in tracker (user order),
    /// or Rejected if verification fails.
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
            &self.private_key_bytes,
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
        let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
        let remaining = tracked.quantity - tracked.settled_quantity - tracked.pending_quantity;
        if remaining < base_quantity {
            return VerifyResult::Rejected {
                reason: format!(
                    "Order {} insufficient capacity: remaining={}, requested={}",
                    order_id, remaining, base_quantity
                ),
            };
        }

        VerifyResult::Accepted { order_id }
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
            &self.private_key_bytes,
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

        // Import into tracker
        self.import_order_from_server(order);

        // Now verify remaining capacity against the proposal
        if let Some(tracked) = self.orders.get(&order_id) {
            let base_quantity = Decimal::from_str(&proposal.base_quantity).unwrap_or_default();
            let remaining = tracked.quantity - tracked.settled_quantity - tracked.pending_quantity;
            if remaining < base_quantity {
                return VerifyResult::Rejected {
                    reason: format!(
                        "Order {} insufficient capacity: remaining={}, requested={}",
                        order_id, remaining, base_quantity
                    ),
                };
            }
        }

        VerifyResult::Accepted { order_id }
    }

    /// Mark settlement as pending (after preconfirmation)
    pub fn mark_pending(&mut self, proposal_id: &str, order_id: u64, quantity: Decimal) {
        if let Some(order) = self.orders.get_mut(&order_id) {
            order.pending_quantity += quantity;
            info!(
                "Order {} pending += {} (total pending: {}, settled: {})",
                order_id, quantity, order.pending_quantity, order.settled_quantity
            );
        }
        self.settlement_orders
            .insert(proposal_id.to_string(), (order_id, quantity));
    }

    /// Mark settlement as completed — move pending → settled
    pub fn mark_settled(&mut self, proposal_id: &str) {
        if let Some((order_id, quantity)) = self.settlement_orders.remove(proposal_id) {
            if let Some(order) = self.orders.get_mut(&order_id) {
                order.pending_quantity = (order.pending_quantity - quantity).max(Decimal::ZERO);
                order.settled_quantity += quantity;
                info!(
                    "[{}] Order {} settled: {} (pending={}, settled={})",
                    proposal_id, order_id, quantity, order.pending_quantity, order.settled_quantity
                );
            }
        }
    }

    /// Mark settlement as failed — release pending quantity
    pub fn mark_failed(&mut self, proposal_id: &str) {
        if let Some((order_id, quantity)) = self.settlement_orders.remove(proposal_id) {
            if let Some(order) = self.orders.get_mut(&order_id) {
                order.pending_quantity = (order.pending_quantity - quantity).max(Decimal::ZERO);
                warn!(
                    "[{}] Order {} settlement failed: released {} pending (now pending={}, settled={})",
                    proposal_id, order_id, quantity, order.pending_quantity, order.settled_quantity
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
            .map(|(proposal_id, (order_id, quantity))| SavedSettlementOrder {
                proposal_id: proposal_id.clone(),
                order_id: *order_id,
                quantity: quantity.to_string(),
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
            self.settlement_orders
                .insert(saved.proposal_id, (saved.order_id, quantity));
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
        }
    }

    #[test]
    fn test_sign_and_verify_order() {
        let key = test_private_key();
        let tracker = OrderTracker::new(1000, key);

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "1.0");

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
        let mut tracker = OrderTracker::new(1000, key);

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5.0");

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
        let mut tracker = OrderTracker::new(1000, key);

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "5.0");
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "5.0", nonce, &signature, &signed_data);

        // First settlement: 2.0
        let proposal1 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&proposal1, "our-party"), VerifyResult::Accepted { .. }));
        tracker.mark_pending("proposal-1", 42, Decimal::from_str("2.0").unwrap());

        // Second settlement: 2.0 (total pending = 4.0, remaining = 1.0)
        let mut proposal2 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        proposal2.proposal_id = "test-proposal-2".to_string();
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));
        tracker.mark_pending("proposal-2", 42, Decimal::from_str("2.0").unwrap());

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
        let mut tracker = OrderTracker::new(start_time, key);

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
        let tracker = OrderTracker::new(1000, key);

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
        let mut tracker = OrderTracker::new(1000, key);

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
        let mut tracker = OrderTracker::new(1000, key);

        let (signature, signed_data, nonce) = tracker.sign_order("BTC-USD", "bid", "100.50", "3.0");
        tracker.track_order(42, "BTC-USD", OrderType::Bid as i32, "100.50", "3.0", nonce, &signature, &signed_data);

        // Pending 2.0
        tracker.mark_pending("proposal-1", 42, Decimal::from_str("2.0").unwrap());

        // Can't fit another 2.0 (remaining = 1.0)
        let proposal2 = make_proposal("our-party", "counterparty", "2.0", 42, 99);
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Rejected { .. }));

        // Fail the first settlement
        tracker.mark_failed("proposal-1");

        // Now 2.0 fits again (remaining = 3.0)
        assert!(matches!(tracker.verify_settlement(&proposal2, "our-party"), VerifyResult::Accepted { .. }));
    }
}
