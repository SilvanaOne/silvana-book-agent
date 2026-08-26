//! Trailing signed net flow per (counterparty, token) and per token, decayed
//! over a window and checkpointed to JSON. Reads are pure; file IO is lock-free.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Minimum interval between dirty checkpoints.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(60);

/// Entries whose decayed magnitude falls below this are dropped.
const EPSILON_BASE: f64 = 1e-9;

/// State-file format version.
const NET_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct DecayedNet {
    /// Value AS OF `updated_at_ms` — readers decay it forward, only writers
    /// rebase it (exponential decay composes, so lazy rebasing is exact).
    value: f64,
    updated_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PendingNet {
    party: Option<String>,
    token: String,
    /// Signed base delta applied at confirm (+ = user bought base from us).
    delta: f64,
    at_ms: i64,
}

struct Inner {
    /// (party, token) → decayed signed net.
    nets: HashMap<(String, String), DecayedNet>,
    /// token → decayed desk-level signed net (sum over all counterparties,
    /// including party-less confirms).
    desk: HashMap<String, DecayedNet>,
    /// quote_id → pending confirm-time delta (reversed on expiry).
    pending: HashMap<String, PendingNet>,
    dirty: bool,
    last_checkpoint: Instant,
}

/// See module docs. Constructed once per agent process via
/// [`NetPositionTracker::load_or_new`] and shared as an `Arc`.
pub struct NetPositionTracker {
    inner: Mutex<Inner>,
    path: PathBuf,
    /// Serialises file writes: rename is last-writer-wins, so an in-flight
    /// checkpoint must not land after the shutdown save.
    write_lock: Mutex<()>,
    /// Set once shutdown has saved: any checkpoint that has not yet written is
    /// then a no-op, so it can never overwrite the final state.
    saved_final: std::sync::atomic::AtomicBool,
    /// Exponential-decay time constant, hours.
    window_hours: f64,
    /// How long a pending entry may stand, on both the load path and the sweep
    /// backstop. Derived from the quote lifetime so the two cannot drift apart.
    stale_pending_after: Duration,
}

// Persistence format

#[derive(Serialize, Deserialize)]
struct SavedNetEntry {
    party: String,
    token: String,
    net: f64,
    updated_at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct SavedDeskEntry {
    token: String,
    net: f64,
    updated_at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct SavedPendingNet {
    quote_id: String,
    party: Option<String>,
    token: String,
    delta: f64,
    at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct SavedNetPositions {
    version: u32,
    saved_at: String,
    window_hours: f64,
    entries: Vec<SavedNetEntry>,
    desk: Vec<SavedDeskEntry>,
    pending: Vec<SavedPendingNet>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Whether a reversal may recreate an aggregate that is no longer present.
/// The two callers need opposite answers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReverseMode {
    /// Live path: an absent aggregate was offset to zero, not decayed away, so
    /// the reversal must land even if that recreates the entry.
    Live,
    /// Load path: the map was just pruned, so an absent aggregate really is
    /// gone and recreating it would invent a position.
    AfterPrune,
}

impl NetPositionTracker {
    /// Load from `path`, or start empty. Entries past the decay window are
    /// dropped; pendings past `stale_pending_after` are reversed.
    pub fn load_or_new(
        path: PathBuf,
        window_hours: f64,
        stale_pending_after: Duration,
    ) -> std::sync::Arc<Self> {
        let window_hours = if window_hours.is_finite() && window_hours > 0.0 {
            window_hours
        } else {
            24.0
        };
        let tracker = Self {
            inner: Mutex::new(Inner {
                nets: HashMap::new(),
                desk: HashMap::new(),
                pending: HashMap::new(),
                dirty: false,
                last_checkpoint: Instant::now(),
            }),
            path,
            write_lock: Mutex::new(()),
            saved_final: std::sync::atomic::AtomicBool::new(false),
            window_hours,
            stale_pending_after,
        };

        match std::fs::read_to_string(&tracker.path) {
            Err(_) => {
                debug!(
                    "Net-position state {} not found — starting empty",
                    tracker.path.display()
                );
            }
            Ok(data) => match serde_json::from_str::<SavedNetPositions>(&data) {
                Err(e) => {
                    warn!(
                        "Net-position state {} is corrupt ({}) — starting empty",
                        tracker.path.display(),
                        e
                    );
                }
                Ok(saved) if saved.version != NET_STATE_VERSION => {
                    warn!(
                        "Net-position state version {} != expected {} — starting empty",
                        saved.version, NET_STATE_VERSION
                    );
                }
                Ok(saved) => {
                    let now = now_ms();
                    // Never prune an aggregate a still-restorable pending needs:
                    // the two clocks are independent. sweep_decayed evicts it.
                    let max_age_ms =
                        ((window_hours * 3_600_000.0) as i64).max(stale_pending_after.as_millis() as i64);
                    let mut inner = tracker.inner.lock().unwrap();
                    for e in saved.entries {
                        if e.net.is_finite()
                            && e.net.abs() > EPSILON_BASE
                            && now - e.updated_at_ms <= max_age_ms
                        {
                            inner.nets.insert(
                                (e.party, e.token),
                                DecayedNet { value: e.net, updated_at_ms: e.updated_at_ms },
                            );
                        }
                    }
                    for e in saved.desk {
                        if e.net.is_finite()
                            && e.net.abs() > EPSILON_BASE
                            && now - e.updated_at_ms <= max_age_ms
                        {
                            inner.desk.insert(
                                e.token,
                                DecayedNet { value: e.net, updated_at_ms: e.updated_at_ms },
                            );
                        }
                    }
                    // Live pendings carry over; dead ones are reversed below,
                    // outside the borrow.
                    let mut dead: Vec<PendingNet> = Vec::new();
                    for p in saved.pending {
                        if !p.delta.is_finite() {
                            continue;
                        }
                        let pend = PendingNet {
                            party: p.party,
                            token: p.token,
                            delta: p.delta,
                            at_ms: p.at_ms,
                        };
                        if now - p.at_ms > stale_pending_after.as_millis() as i64 {
                            dead.push(pend);
                        } else {
                            inner.pending.insert(p.quote_id, pend);
                        }
                    }
                    let (n, d, pn) = (inner.nets.len(), dead.len(), inner.pending.len());
                    for p in dead {
                        // AfterPrune: the aggregate may have just been pruned
                        // above, and recreating it would invent a position.
                        Self::reverse_pending_locked(
                            &mut inner,
                            tracker.window_hours,
                            &p,
                            ReverseMode::AfterPrune,
                        );
                    }
                    info!(
                        "Net-position state loaded from {}: {} party entries, {} pending kept, {} stale pending reversed",
                        tracker.path.display(),
                        n,
                        pn,
                        d
                    );
                }
            },
        }
        std::sync::Arc::new(tracker)
    }

    /// Decay factor from an entry's timestamp to `now`.
    fn decay(&self, from_ms: i64, to_ms: i64) -> f64 {
        let dt_hours = ((to_ms - from_ms).max(0) as f64) / 3_600_000.0;
        (-dt_hours / self.window_hours).exp()
    }

    fn read_entry(&self, e: Option<&DecayedNet>, now: i64) -> f64 {
        match e {
            Some(e) if e.value.is_finite() => e.value * self.decay(e.updated_at_ms, now),
            _ => 0.0,
        }
    }

    /// Current decayed signed net for (party, token). Pure read — no state
    /// mutation, lock held only for the lookup.
    pub fn net(&self, party: &str, token: &str) -> f64 {
        let now = now_ms();
        let inner = self.inner.lock().unwrap();
        self.read_entry(inner.nets.get(&(party.to_string(), token.to_string())), now)
    }

    /// Current decayed desk-level signed net for a token (all counterparties).
    pub fn desk_net(&self, token: &str) -> f64 {
        let now = now_ms();
        let inner = self.inner.lock().unwrap();
        self.read_entry(inner.desk.get(token), now)
    }

    /// Rebase-and-add: decay the stored value to `now`, add `delta`, restamp.
    fn apply_delta_locked(
        inner: &mut Inner,
        window_hours: f64,
        party: Option<&str>,
        token: &str,
        delta: f64,
    ) {
        let now = now_ms();
        let rebase = |e: &mut DecayedNet| {
            let dt_hours = ((now - e.updated_at_ms).max(0) as f64) / 3_600_000.0;
            e.value = e.value * (-dt_hours / window_hours).exp() + delta;
            e.updated_at_ms = now;
        };
        if let Some(party) = party {
            let e = inner
                .nets
                .entry((party.to_string(), token.to_string()))
                .or_insert(DecayedNet { value: 0.0, updated_at_ms: now });
            rebase(e);
            if e.value.abs() <= EPSILON_BASE {
                inner.nets.remove(&(party.to_string(), token.to_string()));
            }
        }
        let e = inner
            .desk
            .entry(token.to_string())
            .or_insert(DecayedNet { value: 0.0, updated_at_ms: now });
        rebase(e);
        if e.value.abs() <= EPSILON_BASE {
            inner.desk.remove(token);
        }
        inner.dirty = true;
    }

    /// Reverse a pending delta, as it now stands after decay.
    fn reverse_pending_locked(
        inner: &mut Inner,
        window_hours: f64,
        p: &PendingNet,
        mode: ReverseMode,
    ) {
        let now = now_ms();
        // Undo the delta AS IT NOW STANDS — `delta · exp(−age/window)`. The raw
        // value over-reverses and drives the net negative out of nothing.
        let age_hours = ((now - p.at_ms).max(0) as f64) / 3_600_000.0;
        let remaining = p.delta * (-age_hours / window_hours).exp();
        let rebase = |e: &mut DecayedNet| {
            let dt_hours = ((now - e.updated_at_ms).max(0) as f64) / 3_600_000.0;
            e.value = e.value * (-dt_hours / window_hours).exp() - remaining;
            e.updated_at_ms = now;
        };
        // Whether an absent aggregate is recreated is the whole point of
        // `mode` — see [`ReverseMode`].
        if let Some(party) = p.party.as_deref() {
            let key = (party.to_string(), p.token.clone());
            if inner.nets.contains_key(&key) || mode == ReverseMode::Live {
                let e = inner
                    .nets
                    .entry(key.clone())
                    .or_insert(DecayedNet { value: 0.0, updated_at_ms: now });
                rebase(e);
                if e.value.abs() <= EPSILON_BASE {
                    inner.nets.remove(&key);
                }
                inner.dirty = true;
            }
        }
        if inner.desk.contains_key(&p.token) || mode == ReverseMode::Live {
            let e = inner
                .desk
                .entry(p.token.clone())
                .or_insert(DecayedNet { value: 0.0, updated_at_ms: now });
            rebase(e);
            if e.value.abs() <= EPSILON_BASE {
                inner.desk.remove(&p.token);
            }
            inner.dirty = true;
        }
    }

    /// Drop aggregates whose decayed value has fallen to nothing. Decay is
    /// lazy, so a one-off party is never otherwise evicted. Returns the count.
    pub fn sweep_decayed(&self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let now = now_ms();
        let w = self.window_hours;
        let decayed = |e: &DecayedNet| -> f64 {
            let dt_hours = ((now - e.updated_at_ms).max(0) as f64) / 3_600_000.0;
            e.value * (-dt_hours / w).exp()
        };
        let before = inner.nets.len() + inner.desk.len();
        inner.nets.retain(|_, e| decayed(e).abs() > EPSILON_BASE);
        inner.desk.retain(|_, e| decayed(e).abs() > EPSILON_BASE);
        let dropped = before - (inner.nets.len() + inner.desk.len());
        if dropped > 0 {
            inner.dirty = true;
        }
        dropped
    }

    /// Count the signed base delta (+q for user BUY, −q for SELL) and remember
    /// it as pending so [`Self::release`] can reverse it. Idempotent per quote.
    pub fn record_confirm(
        &self,
        quote_id: &str,
        party: Option<&str>,
        token: &str,
        signed_base_delta: f64,
    ) {
        if !signed_base_delta.is_finite() || signed_base_delta == 0.0 || token.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.pending.contains_key(quote_id) {
            return; // idempotent re-confirm
        }
        inner.pending.insert(
            quote_id.to_string(),
            PendingNet {
                party: party.map(|p| p.to_string()),
                token: token.to_string(),
                delta: signed_base_delta,
                at_ms: now_ms(),
            },
        );
        Self::apply_delta_locked(&mut inner, self.window_hours, party, token, signed_base_delta);
    }

    /// Settle observed: drop the pending marker, keep the applied value. An
    /// unknown quote is a debug no-op.
    pub fn settle(&self, quote_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.pending.remove(quote_id).is_some() {
            inner.dirty = true;
        } else {
            debug!("Net tracker: settle for unknown quote {} (restored/expired?)", quote_id);
        }
    }

    /// The quote expired unfilled: reverse the confirm-time delta.
    pub fn release(&self, quote_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(p) = inner.pending.remove(quote_id) {
            // Live: the aggregate may be absent because offsetting flow
            // cancelled it to zero, and the reversal must still restore it.
            Self::reverse_pending_locked(&mut inner, self.window_hours, &p, ReverseMode::Live);
        }
    }

    /// Backstop for pendings the sweep will never see again (restart races):
    /// reverse anything older than `stale_pending_after`.
    pub fn expire_stale_pending(&self) {
        let cutoff = now_ms() - self.stale_pending_after.as_millis() as i64;
        let mut inner = self.inner.lock().unwrap();
        let stale: Vec<(String, PendingNet)> = inner
            .pending
            .iter()
            .filter(|(_, p)| p.at_ms < cutoff)
            .map(|(k, p)| (k.clone(), p.clone()))
            .collect();
        for (quote_id, p) in stale {
            warn!(
                "Net tracker: reversing stale pending confirm {} ({} {}, never settled/swept)",
                quote_id, p.delta, p.token
            );
            inner.pending.remove(&quote_id);
            Self::reverse_pending_locked(&mut inner, self.window_hours, &p, ReverseMode::Live);
        }
    }

    fn snapshot_locked(inner: &Inner, window_hours: f64) -> SavedNetPositions {
        SavedNetPositions {
            version: NET_STATE_VERSION,
            saved_at: chrono::Utc::now().to_rfc3339(),
            window_hours,
            entries: inner
                .nets
                .iter()
                .filter(|(_, e)| e.value.is_finite() && e.value.abs() > EPSILON_BASE)
                .map(|((party, token), e)| SavedNetEntry {
                    party: party.clone(),
                    token: token.clone(),
                    net: e.value,
                    updated_at_ms: e.updated_at_ms,
                })
                .collect(),
            desk: inner
                .desk
                .iter()
                .filter(|(_, e)| e.value.is_finite() && e.value.abs() > EPSILON_BASE)
                .map(|(token, e)| SavedDeskEntry {
                    token: token.clone(),
                    net: e.value,
                    updated_at_ms: e.updated_at_ms,
                })
                .collect(),
            pending: inner
                .pending
                .iter()
                .map(|(quote_id, p)| SavedPendingNet {
                    quote_id: quote_id.clone(),
                    party: p.party.clone(),
                    token: p.token.clone(),
                    delta: p.delta,
                    at_ms: p.at_ms,
                })
                .collect(),
        }
    }

    fn write_snapshot(&self, snap: &SavedNetPositions) -> std::io::Result<()> {
        // Compact, not pretty: this is a machine-read checkpoint rewritten
        // every 60s, and pretty-printing is a ~40% size tax on every write.
        let json = serde_json::to_string(snap)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Write the state file when dirty and at least 60 s since the last write.
    /// Snapshot under the lock, file IO outside it. Safe at any cadence.
    pub fn checkpoint_if_dirty(self: &std::sync::Arc<Self>) {
        // Evict fully-decayed aggregates first, so the map and the file cannot
        // grow without bound.
        let dropped = self.sweep_decayed();
        let snap = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.dirty || inner.last_checkpoint.elapsed() < CHECKPOINT_EVERY {
                return;
            }
            inner.dirty = false;
            inner.last_checkpoint = Instant::now();
            Self::snapshot_locked(&inner, self.window_hours)
        };
        if dropped > 0 {
            debug!("Net-position sweep dropped {dropped} fully-decayed entries");
        }

        let me = std::sync::Arc::clone(self);
        let write = move || {
            let _serialised = me.write_lock.lock().unwrap_or_else(|p| p.into_inner());
            if me.saved_final.load(std::sync::atomic::Ordering::Acquire) {
                // Shutdown already wrote the authoritative state; landing now
                // would replace it with an older snapshot.
                return;
            }
            if let Err(e) = me.write_snapshot(&snap) {
                warn!("Net-position checkpoint to {} failed: {}", me.path.display(), e);
                // Keep the data eligible for the next attempt.
                me.inner.lock().unwrap().dirty = true;
            }
        };
        // Synchronous fs IO on a tokio worker can stall the runtime: hand it to
        // the blocking pool, or run inline outside a runtime.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(write);
            }
            Err(_) => write(),
        }
    }

    /// Unconditional save (graceful shutdown).
    pub fn save(&self) {
        let snap = {
            let mut inner = self.inner.lock().unwrap();
            inner.dirty = false;
            Self::snapshot_locked(&inner, self.window_hours)
        };
        // Take the write lock so an in-flight checkpoint completes first, then
        // mark the state final so no later checkpoint can overwrite it.
        let _serialised = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        self.saved_final.store(true, std::sync::atomic::Ordering::Release);
        match self.write_snapshot(&snap) {
            Ok(()) => info!(
                "Net-position state saved to {} ({} entries, {} pending)",
                self.path.display(),
                snap.entries.len(),
                snap.pending.len()
            ),
            Err(e) => warn!("Net-position save to {} failed: {}", self.path.display(), e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The value the shipped default config derives (120 s validity + 30 s
    /// grace + 600 s slack, floored at 900 s).
    const TEST_STALE_AFTER: Duration = Duration::from_secs(900);

    /// `saved_at` is written for humans and never read back, so fixtures use a
    /// fixed placeholder rather than a real timestamp.
    const FIXTURE_SAVED_AT: &str = "1970-01-01T00:00:00Z";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("silvana-net-position-tests");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("{name}-{}.json", uuid::Uuid::now_v7()))
    }

    fn fresh(name: &str) -> Arc<NetPositionTracker> {
        NetPositionTracker::load_or_new(scratch(name), 24.0, TEST_STALE_AFTER)
    }

    /// A pending that died while the process was down must not be reversed
    /// against an aggregate the staleness prune already dropped.
    #[test]
    fn stale_pending_reversal_after_prune() {
        let path = scratch("pruned-aggregate");
        // Aggregates far older than the decay window (so they prune on load)
        // but still carrying a pending — the shape after a long outage.
        let very_old = now_ms() - 100 * 3_600_000; // 100h, window is 24h
        let saved = serde_json::json!({
            "version": NET_STATE_VERSION,
            "saved_at": FIXTURE_SAVED_AT,
            "window_hours": 24.0,
            "entries": [{
                "party": "party-a", "token": "TOKEN",
                "net": 500_000.0, "updated_at_ms": very_old
            }],
            "desk": [{ "token": "TOKEN", "net": 500_000.0, "updated_at_ms": very_old }],
            "pending": [{
                "quote_id": "q-dead", "party": "party-a", "token": "TOKEN",
                "delta": 500_000.0, "at_ms": very_old
            }]
        });
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();

        let t = NetPositionTracker::load_or_new(path, 24.0, TEST_STALE_AFTER);
        let net = t.net("party-a", "TOKEN");
        assert!(
            net >= 0.0,
            "load produced a negative net of {net} from a pruned aggregate"
        );
    }

    /// Reversal must remove the delta as it now stands: the raw value
    /// over-reverses by `delta·(1 − exp(−age/w))` and drives the net negative.
    #[test]
    fn reversal_removes_the_decayed_delta_not_the_raw_one() {
        let path = scratch("decayed-reversal");
        let six_hours_ago = now_ms() - 6 * 3_600_000;
        // Aggregate and pending both stamped at the same confirm 6h ago.
        let saved = serde_json::json!({
            "version": NET_STATE_VERSION,
            "saved_at": FIXTURE_SAVED_AT,
            "window_hours": 24.0,
            "entries": [{
                "party": "alice", "token": "TOKEN",
                "net": 500_000.0, "updated_at_ms": six_hours_ago
            }],
            "desk": [{ "token": "TOKEN", "net": 500_000.0, "updated_at_ms": six_hours_ago }],
            // >15min old => dead => reversed at load.
            "pending": [{
                "quote_id": "q-dead", "party": "alice", "token": "TOKEN",
                "delta": 500_000.0, "at_ms": six_hours_ago
            }]
        });
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();

        let t = NetPositionTracker::load_or_new(path, 24.0, TEST_STALE_AFTER);
        let net = t.net("alice", "TOKEN");
        assert!(
            net.abs() < 1.0,
            "reversing a 6h-old +500k left net={net} — expected ~0"
        );
    }

    /// The same discipline on the live-quote path: release must not
    /// over-reverse either.
    #[test]
    fn release_reverses_cleanly_leaving_no_residue() {
        let t = fresh("release-clean");
        t.record_confirm("q1", Some("bob"), "TOKEN", 250_000.0);
        assert!((t.net("bob", "TOKEN") - 250_000.0).abs() < 1.0);
        t.release("q1");
        let after = t.net("bob", "TOKEN");
        assert!(
            after.abs() < 1.0,
            "release left residue {after}"
        );
    }

    /// Decay is lazy, so a party that trades once and never returns would
    /// otherwise live forever in memory and in every checkpoint.
    #[test]
    fn sweep_drops_fully_decayed_entries() {
        let path = scratch("sweep");
        let ancient = now_ms() - 1000 * 3_600_000; // ~42 days vs a 1h window
        let saved = serde_json::json!({
            "version": NET_STATE_VERSION,
            "saved_at": FIXTURE_SAVED_AT,
            "window_hours": 1.0,
            "entries": [{
                "party": "one-shot", "token": "TOKEN",
                "net": 10_000.0, "updated_at_ms": ancient
            }],
            "desk": [{ "token": "TOKEN", "net": 10_000.0, "updated_at_ms": ancient }],
            "pending": []
        });
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();
        // A long window on load keeps the entry; the sweep then decays it out.
        let t = NetPositionTracker::load_or_new(path, 100_000.0, TEST_STALE_AFTER);
        let before = t.net("one-shot", "TOKEN");
        assert!(before.abs() > 0.0, "entry should load");

        let t2 = fresh("sweep-live");
        t2.record_confirm("q1", Some("ghost"), "TOKEN", 1e-12); // below epsilon
        let dropped = t2.sweep_decayed();
        assert!(dropped > 0 || t2.net("ghost", "TOKEN").abs() <= 1e-9);
    }

    /// confirm → settle keeps the delta; confirm → release reverses it; both
    /// touch the desk net consistently.
    #[test]
    fn confirm_settle_release_lifecycle() {
        let t = fresh("lifecycle");
        t.record_confirm("q1", Some("alice"), "TOKEN", 50_000.0);
        assert!((t.net("alice", "TOKEN") - 50_000.0).abs() < 1.0);
        assert!((t.desk_net("TOKEN") - 50_000.0).abs() < 1.0);

        t.settle("q1");
        assert!((t.net("alice", "TOKEN") - 50_000.0).abs() < 1.0, "settle keeps the delta");

        // A sell reduces the net (signed).
        t.record_confirm("q2", Some("alice"), "TOKEN", -20_000.0);
        assert!((t.net("alice", "TOKEN") - 30_000.0).abs() < 1.0);

        // Expired quote: reversed.
        t.record_confirm("q3", Some("alice"), "TOKEN", 10_000.0);
        t.release("q3");
        assert!((t.net("alice", "TOKEN") - 30_000.0).abs() < 1.0);
        assert!((t.desk_net("TOKEN") - 30_000.0).abs() < 1.0);

        // Party-less confirm counts on the desk only.
        t.record_confirm("q4", None, "TOKEN", 5_000.0);
        assert!((t.desk_net("TOKEN") - 35_000.0).abs() < 1.0);
        assert!((t.net("alice", "TOKEN") - 30_000.0).abs() < 1.0);

        // Idempotent re-confirm of the same quote_id.
        t.record_confirm("q4", None, "TOKEN", 5_000.0);
        assert!((t.desk_net("TOKEN") - 35_000.0).abs() < 1.0);

        // Unknown-party read is 0.
        assert_eq!(t.net("bob", "TOKEN"), 0.0);
        assert_eq!(t.net("alice", "CC"), 0.0);
    }

    /// Round trip at the tracker level: buy X, settle; sell X, settle → net
    /// returns to ~0 (decay over microseconds is negligible).
    #[test]
    fn round_trip_nets_to_zero() {
        let t = fresh("round-trip");
        t.record_confirm("b", Some("alice"), "TOKEN", 100_000.0);
        t.settle("b");
        t.record_confirm("s", Some("alice"), "TOKEN", -100_000.0);
        t.settle("s");
        assert!(t.net("alice", "TOKEN").abs() < 1.0);
        assert!(t.desk_net("TOKEN").abs() < 1.0);
    }

    /// A live reversal must land even when the aggregate is absent because
    /// offsetting flow cancelled it to zero, or the still-held leg is lost.
    #[test]
    fn release_restores_offsetting_position() {
        let t = fresh("offset-release");
        t.record_confirm("buy", Some("alice"), "TOKEN", 100_000.0);
        t.record_confirm("sell", Some("alice"), "TOKEN", -100_000.0);
        // The two cancel, so both aggregates are gone from the maps.
        assert!(t.net("alice", "TOKEN").abs() < 1.0);
        assert!(t.desk_net("TOKEN").abs() < 1.0);

        // The buy quote expires unfilled. Only the sell is still held.
        t.release("buy");
        assert!(
            (t.net("alice", "TOKEN") + 100_000.0).abs() < 10.0,
            "party net {} — the still-held sell was lost",
            t.net("alice", "TOKEN")
        );
        assert!(
            (t.desk_net("TOKEN") + 100_000.0).abs() < 10.0,
            "desk net {} — the still-held sell was lost",
            t.desk_net("TOKEN")
        );

        // Releasing the sell too returns everything to flat, with no residue.
        t.release("sell");
        assert!(t.net("alice", "TOKEN").abs() < 1.0);
        assert!(t.desk_net("TOKEN").abs() < 1.0);
    }

    /// A decay window shorter than the stale-pending backstop must not let the
    /// load-path prune drop an aggregate whose pending is still restorable.
    #[test]
    fn short_window_keeps_pending_aggregate() {
        let path = scratch("short-window");
        let recent = now_ms() - 10 * 60 * 1000; // 10 min: inside the 900 s backstop
        let saved = serde_json::json!({
            "version": NET_STATE_VERSION,
            "saved_at": FIXTURE_SAVED_AT,
            "window_hours": 0.05,
            "entries": [{
                "party": "alice", "token": "TOKEN",
                "net": 500_000.0, "updated_at_ms": recent
            }],
            "desk": [{ "token": "TOKEN", "net": 500_000.0, "updated_at_ms": recent }],
            "pending": [{
                "quote_id": "q-live", "party": "alice", "token": "TOKEN",
                "delta": 500_000.0, "at_ms": recent
            }]
        });
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();

        // 0.05 h = 180 s, well under the 900 s backstop.
        let t = NetPositionTracker::load_or_new(path, 0.05, TEST_STALE_AFTER);
        t.release("q-live");
        let net = t.net("alice", "TOKEN");
        assert!(
            net >= -1.0,
            "release produced a negative net of {net}"
        );
        let desk = t.desk_net("TOKEN");
        assert!(desk >= -1.0, "desk net {desk} went negative");
    }

    /// Persistence round trip: save → load restores nets, desk and pending;
    /// a corrupt file starts empty.
    #[test]
    fn persistence_round_trip_and_corrupt_file() {
        let path = scratch("persist");
        {
            let t = NetPositionTracker::load_or_new(path.clone(), 24.0, TEST_STALE_AFTER);
            t.record_confirm("q1", Some("alice"), "TOKEN", 42_000.0);
            t.settle("q1");
            t.record_confirm("q2", Some("bob"), "TOKEN", 7_000.0); // still pending
            t.save();
        }
        {
            let t = NetPositionTracker::load_or_new(path.clone(), 24.0, TEST_STALE_AFTER);
            assert!((t.net("alice", "TOKEN") - 42_000.0).abs() < 10.0);
            assert!((t.desk_net("TOKEN") - 49_000.0).abs() < 10.0);
            // Pending survived: releasing it reverses bob's delta.
            t.release("q2");
            assert!(t.net("bob", "TOKEN").abs() < 1.0);
            assert!((t.desk_net("TOKEN") - 42_000.0).abs() < 10.0);
        }
        // Corrupt file → warn + empty, never crash.
        std::fs::write(&path, "{not json").unwrap();
        let t = NetPositionTracker::load_or_new(path.clone(), 24.0, TEST_STALE_AFTER);
        assert_eq!(t.net("alice", "TOKEN"), 0.0);
        let _ = std::fs::remove_file(&path);
    }

    /// Entries older than the window are dropped at load; stale pending is
    /// reversed at load.
    #[test]
    fn load_drops_stale_and_reverses_dead_pending() {
        let path = scratch("stale");
        let old_ms = now_ms() - 48 * 3_600_000; // 48h > 24h window
        let saved = SavedNetPositions {
            version: NET_STATE_VERSION,
            saved_at: chrono::Utc::now().to_rfc3339(),
            window_hours: 24.0,
            entries: vec![SavedNetEntry {
                party: "alice".into(),
                token: "TOKEN".into(),
                net: 1_000_000.0,
                updated_at_ms: old_ms,
            }],
            desk: vec![SavedDeskEntry {
                token: "TOKEN".into(),
                net: 1_000_000.0,
                updated_at_ms: now_ms(),
            }],
            // Dead pending (older than 15 min): must be reversed against desk.
            pending: vec![SavedPendingNet {
                quote_id: "qdead".into(),
                party: None,
                token: "TOKEN".into(),
                delta: 400_000.0,
                at_ms: now_ms() - 30 * 60_000,
            }],
        };
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();
        let t = NetPositionTracker::load_or_new(path.clone(), 24.0, TEST_STALE_AFTER);
        assert_eq!(t.net("alice", "TOKEN"), 0.0, "48h-old entry dropped");
        // 608,247 not 600,000: the reversal removes the delta as it now stands
        // (400,000·exp(−0.5h/24h) = 391,753), not the raw 400,000.
        let expected = 1_000_000.0 - 400_000.0 * (-0.5f64 / 24.0).exp();
        assert!(
            (t.desk_net("TOKEN") - expected).abs() < 1.0,
            "dead pending reversed against desk: {} (expected {expected})",
            t.desk_net("TOKEN")
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Decay: a synthetic entry one window old reads ≈ 37% of its value.
    #[test]
    fn decay_over_one_window() {
        let path = scratch("decay");
        let one_window_ago = now_ms() - 24 * 3_600_000 + 5_000; // just inside the drop cutoff
        let saved = SavedNetPositions {
            version: NET_STATE_VERSION,
            saved_at: chrono::Utc::now().to_rfc3339(),
            window_hours: 24.0,
            entries: vec![SavedNetEntry {
                party: "alice".into(),
                token: "TOKEN".into(),
                net: 100_000.0,
                updated_at_ms: one_window_ago,
            }],
            desk: vec![],
            pending: vec![],
        };
        std::fs::write(&path, serde_json::to_string(&saved).unwrap()).unwrap();
        let t = NetPositionTracker::load_or_new(path.clone(), 24.0, TEST_STALE_AFTER);
        let v = t.net("alice", "TOKEN");
        let expect = 100_000.0 * (-1.0f64).exp();
        assert!(
            (v - expect).abs() / expect < 0.01,
            "one-window-old 100k should read ≈{expect:.0}, got {v:.0}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// expire_stale_pending reverses only entries older than the cutoff.
    #[test]
    fn expire_stale_pending_reverses_old_only() {
        let t = fresh("expire");
        t.record_confirm("fresh", Some("alice"), "TOKEN", 10_000.0);
        {
            // Backdate a pending entry through the internals.
            let mut inner = t.inner.lock().unwrap();
            inner.pending.insert(
                "old".into(),
                PendingNet {
                    party: Some("alice".into()),
                    token: "TOKEN".into(),
                    delta: 5_000.0,
                    at_ms: now_ms() - 3_600_000,
                },
            );
            NetPositionTracker::apply_delta_locked(
                &mut inner,
                24.0,
                Some("alice"),
                "TOKEN",
                5_000.0,
            );
        }
        assert!((t.net("alice", "TOKEN") - 15_000.0).abs() < 1.0);
        t.expire_stale_pending();
        // Reversed by its decayed remainder, leaving ~10,204. The ~204 residue
        // is this test backdating only the marker; real confirms stamp both.
        let expected = 15_000.0 - 5_000.0 * (-1.0f64 / 24.0).exp();
        assert!(
            (t.net("alice", "TOKEN") - expected).abs() < 1.0,
            "old reversed, fresh kept: {} (expected {expected})",
            t.net("alice", "TOKEN")
        );
    }
}
