//! RFQ V2 SettlementTicket pool (design §5.4, D1).
//!
//! Pure state: Free / Assigned / Spent transitions over the LP's on-ledger
//! SettlementTickets. Only constructed when `ticket_threshold_usd` is set.
//! Batch-refill LOGIC (issue when free < low_water) lives in the split/refill
//! worker; reconciliation against the ACS happens there too.
//!
//! Assigned tickets return to Free only at `valid_until + settle_grace_secs`
//! (`expire_assignments`) — the on-ledger window check guarantees an in-flight
//! settle of the expired quote aborts, so reuse cannot violate the
//! "<= 1 live quote per ticket" invariant. Tickets are only *archived* (Spent)
//! by an actual settle.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use agent_logic::state::SavedTicket;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketStatus {
    Free,
    Assigned { quote_id: String, expires_at: Instant },
    Spent,
}

#[derive(Debug, Clone)]
pub struct TicketEntry {
    pub ticket_id: String,
    pub contract_id: String,
    /// Full template id as the ledger returned it (disclosure needs it)
    pub template_id: Option<String>,
    /// None = blob-pending (not yet disclosable; refill/reconcile backfills)
    pub created_event_blob: Option<String>,
    /// create-arguments JSON, verbatim text (for the envelope's AtomicAcsContract)
    pub payload_json: Option<String>,
    pub status: TicketStatus,
}

impl TicketEntry {
    /// Disclosable = every envelope-required field present.
    fn is_disclosable(&self) -> bool {
        self.template_id.is_some()
            && self.created_event_blob.is_some()
            && self.payload_json.is_some()
    }
}

/// On-ledger ticket facts fed into [`TicketPool::reconcile_from_acs`].
#[derive(Debug, Clone)]
pub struct TicketAcsInfo {
    pub ticket_id: String,
    pub contract_id: String,
    pub template_id: Option<String>,
    pub created_event_blob: Option<String>,
    pub payload_json: Option<String>,
}

/// Ticket pool keyed by ticket_id. Sync mutex: all critical sections are
/// short and never held across an await (snapshot() must be callable from the
/// runner's synchronous state-save closure).
pub struct TicketPool {
    entries: Mutex<HashMap<String, TicketEntry>>,
}

impl TicketPool {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Pop a Free, disclosable ticket and assign it to a quote.
    /// Empty pool ⇒ None (the caller rejects NO_TICKET_AVAILABLE — never issue
    /// synchronously; the batch worker refills).
    pub fn assign(&self, quote_id: &str, expires_at: Instant) -> Option<(String, TicketEntry)> {
        let mut entries = self.entries.lock().unwrap();
        let ticket_id = entries
            .values()
            .find(|e| e.status == TicketStatus::Free && e.is_disclosable())
            .map(|e| e.ticket_id.clone())?;
        let entry = entries.get_mut(&ticket_id).expect("just found");
        entry.status = TicketStatus::Assigned {
            quote_id: quote_id.to_string(),
            expires_at,
        };
        debug!("Assigned ticket {} to quote {}", ticket_id, quote_id);
        Some((ticket_id.clone(), entry.clone()))
    }

    /// Return the ticket assigned to `quote_id` (if any) to the Free pool
    /// (reject paths / expiry sweep).
    pub fn unassign(&self, quote_id: &str) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.values_mut() {
            if matches!(&e.status, TicketStatus::Assigned { quote_id: q, .. } if q == quote_id) {
                debug!("Unassigned ticket {} from quote {}", e.ticket_id, quote_id);
                e.status = TicketStatus::Free;
            }
        }
    }

    /// A SettlementTicket contract was archived on-ledger (consumed by a
    /// settle or cancelled) — mark Spent.
    pub fn on_archived(&self, contract_id: &str) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.values_mut() {
            if e.contract_id == contract_id {
                debug!("Ticket {} archived on ledger — Spent", e.ticket_id);
                e.status = TicketStatus::Spent;
            }
        }
    }

    /// Mark the ticket assigned to `quote_id` as Spent (observed settle).
    pub fn mark_spent(&self, quote_id: &str) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.values_mut() {
            if matches!(&e.status, TicketStatus::Assigned { quote_id: q, .. } if q == quote_id) {
                e.status = TicketStatus::Spent;
            }
        }
    }

    pub fn free_count(&self) -> usize {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.status == TicketStatus::Free && e.is_disclosable())
            .count()
    }

    /// Reconcile against the on-ledger SettlementTicket set:
    /// - in pool but not on ledger → drop (spent/cancelled and fully processed)
    /// - on ledger but unknown → adopt as Free
    /// - known → keep status, backfill blob/payload/template/cid
    pub fn reconcile_from_acs(&self, on_ledger: Vec<TicketAcsInfo>) {
        let mut entries = self.entries.lock().unwrap();
        let ledger_ids: std::collections::HashSet<&str> =
            on_ledger.iter().map(|t| t.ticket_id.as_str()).collect();

        let before = entries.len();
        entries.retain(|tid, _| ledger_ids.contains(tid.as_str()));
        let dropped = before - entries.len();

        let mut adopted = 0usize;
        for t in on_ledger {
            match entries.get_mut(&t.ticket_id) {
                Some(e) => {
                    e.contract_id = t.contract_id;
                    if t.template_id.is_some() {
                        e.template_id = t.template_id;
                    }
                    if t.created_event_blob.is_some() {
                        e.created_event_blob = t.created_event_blob;
                    }
                    if t.payload_json.is_some() {
                        e.payload_json = t.payload_json;
                    }
                }
                None => {
                    entries.insert(
                        t.ticket_id.clone(),
                        TicketEntry {
                            ticket_id: t.ticket_id,
                            contract_id: t.contract_id,
                            template_id: t.template_id,
                            created_event_blob: t.created_event_blob,
                            payload_json: t.payload_json,
                            status: TicketStatus::Free,
                        },
                    );
                    adopted += 1;
                }
            }
        }
        if dropped > 0 || adopted > 0 {
            info!(
                "Ticket pool reconciled: {} dropped (off-ledger), {} adopted (Free), {} total",
                dropped,
                adopted,
                entries.len()
            );
        }
    }

    /// Return expired assignments to Free (backstop; the reject paths and the
    /// settle watcher are the primary mechanisms).
    pub fn expire_assignments(&self, now: Instant) {
        let mut entries = self.entries.lock().unwrap();
        for e in entries.values_mut() {
            if matches!(&e.status, TicketStatus::Assigned { expires_at, .. } if now >= *expires_at)
            {
                warn!("Ticket {} assignment expired — returning to Free", e.ticket_id);
                e.status = TicketStatus::Free;
            }
        }
    }

    /// Snapshot for SavedState (blobs NOT persisted — re-fetched at startup).
    pub fn snapshot(&self) -> Vec<SavedTicket> {
        let now_instant = Instant::now();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| {
                let (status, quote_id, expires_at_ms) = match &e.status {
                    TicketStatus::Free => ("free".to_string(), None, None),
                    TicketStatus::Assigned { quote_id, expires_at } => {
                        let remaining_ms =
                            expires_at.saturating_duration_since(now_instant).as_millis() as u64;
                        (
                            "assigned".to_string(),
                            Some(quote_id.clone()),
                            Some(now_ms + remaining_ms),
                        )
                    }
                    TicketStatus::Spent => ("spent".to_string(), None, None),
                };
                SavedTicket {
                    ticket_id: e.ticket_id.clone(),
                    contract_id: e.contract_id.clone(),
                    status,
                    quote_id,
                    expires_at_ms,
                }
            })
            .collect()
    }

    /// Restore from SavedState (blob-pending; reconcile backfills). Assigned
    /// entries whose wall-clock expiry passed while the agent was down come
    /// back Free.
    pub fn restore(&self, saved: Vec<SavedTicket>) {
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let now_instant = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        let count = saved.len();
        for t in saved {
            let status = match t.status.as_str() {
                "assigned" => match (t.quote_id.clone(), t.expires_at_ms) {
                    (Some(quote_id), Some(expires_at_ms)) if expires_at_ms > now_ms => {
                        TicketStatus::Assigned {
                            quote_id,
                            expires_at: now_instant
                                + std::time::Duration::from_millis(expires_at_ms - now_ms),
                        }
                    }
                    _ => TicketStatus::Free,
                },
                "spent" => TicketStatus::Spent,
                _ => TicketStatus::Free,
            };
            entries.insert(
                t.ticket_id.clone(),
                TicketEntry {
                    ticket_id: t.ticket_id,
                    contract_id: t.contract_id,
                    template_id: None,
                    created_event_blob: None,
                    payload_json: None,
                    status,
                },
            );
        }
        if count > 0 {
            info!("Restored {} ticket(s) from saved state (blobs pending reconcile)", count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn on_ledger_entry(tid: &str) -> TicketAcsInfo {
        TicketAcsInfo {
            ticket_id: tid.to_string(),
            contract_id: format!("00{tid}"),
            template_id: Some("#atomic-dvp-v1:AtomicDVP:SettlementTicket".to_string()),
            created_event_blob: Some(format!("blob-{tid}")),
            payload_json: Some(format!("{{\"ticketId\":\"{tid}\"}}")),
        }
    }

    #[test]
    fn assign_unassign_spend_cycle() {
        let pool = TicketPool::new();
        pool.reconcile_from_acs(vec![on_ledger_entry("t1"), on_ledger_entry("t2")]);
        assert_eq!(pool.free_count(), 2);

        let expires = Instant::now() + Duration::from_secs(60);
        let (tid, entry) = pool.assign("q1", expires).unwrap();
        assert_eq!(entry.ticket_id, tid);
        assert_eq!(pool.free_count(), 1);

        // reject path: unassign returns it to Free
        pool.unassign("q1");
        assert_eq!(pool.free_count(), 2);

        // settle path: assigned then archived → Spent, never Free again
        let (tid2, _) = pool.assign("q2", expires).unwrap();
        let cid = format!("00{tid2}");
        pool.on_archived(&cid);
        assert_eq!(pool.free_count(), 1);
        pool.unassign("q2"); // no-op: already Spent
        assert_eq!(pool.free_count(), 1);
    }

    #[test]
    fn empty_pool_and_blob_pending_not_assignable() {
        let pool = TicketPool::new();
        assert!(pool.assign("q", Instant::now()).is_none());
        // blob-pending ticket is not disclosable → not assignable
        pool.reconcile_from_acs(vec![TicketAcsInfo {
            ticket_id: "t1".to_string(),
            contract_id: "00t1".to_string(),
            template_id: None,
            created_event_blob: None,
            payload_json: None,
        }]);
        assert_eq!(pool.free_count(), 0);
        assert!(pool.assign("q", Instant::now()).is_none());
        // blob backfill via reconcile makes it assignable
        pool.reconcile_from_acs(vec![on_ledger_entry("t1")]);
        assert_eq!(pool.free_count(), 1);
    }

    #[test]
    fn expiry_reassign() {
        let pool = TicketPool::new();
        pool.reconcile_from_acs(vec![on_ledger_entry("t1")]);
        let past = Instant::now() - Duration::from_secs(1);
        let (tid, _) = pool.assign("q1", past).unwrap();
        assert_eq!(pool.free_count(), 0);
        pool.expire_assignments(Instant::now());
        assert_eq!(pool.free_count(), 1);
        // reassignable to a new quote after expiry
        let (tid2, _) = pool
            .assign("q2", Instant::now() + Duration::from_secs(60))
            .unwrap();
        assert_eq!(tid, tid2);
    }

    #[test]
    fn reconcile_drops_offledger_and_keeps_assigned() {
        let pool = TicketPool::new();
        pool.reconcile_from_acs(vec![on_ledger_entry("t1"), on_ledger_entry("t2")]);
        let expires = Instant::now() + Duration::from_secs(60);
        let (assigned_tid, _) = pool.assign("q1", expires).unwrap();
        // t3 appears; the non-assigned one of {t1,t2} disappears
        let keep = assigned_tid.clone();
        pool.reconcile_from_acs(vec![on_ledger_entry(&keep), on_ledger_entry("t3")]);
        assert_eq!(pool.free_count(), 1); // t3 only
        // the assigned ticket kept its assignment
        let snap = pool.snapshot();
        let kept = snap.iter().find(|t| t.ticket_id == keep).unwrap();
        assert_eq!(kept.status, "assigned");
        assert_eq!(kept.quote_id.as_deref(), Some("q1"));
    }

    #[test]
    fn snapshot_restore_roundtrip() {
        let pool = TicketPool::new();
        pool.reconcile_from_acs(vec![on_ledger_entry("t1"), on_ledger_entry("t2")]);
        pool.assign("q1", Instant::now() + Duration::from_secs(120)).unwrap();
        let snap = pool.snapshot();

        let restored = TicketPool::new();
        restored.restore(snap);
        // blobs not persisted → nothing assignable until reconcile
        assert_eq!(restored.free_count(), 0);
        restored.reconcile_from_acs(vec![on_ledger_entry("t1"), on_ledger_entry("t2")]);
        // one Free (blob backfilled), one still Assigned
        assert_eq!(restored.free_count(), 1);
        let snap2 = restored.snapshot();
        assert!(snap2.iter().any(|t| t.status == "assigned" && t.quote_id.as_deref() == Some("q1")));
    }
}
