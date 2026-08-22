//! Best-effort structured error reporting to the orderbook server
//! (`SettlementService.ReportErrors`).
//!
//! The agent has no database; every error it wants recorded travels over
//! gRPC. Design rules, in order:
//!
//! 1. `report()` is SYNC (`try_send`, never awaits, never blocks) so deep
//!    call sites — including the sync `ledger_health` module — can report
//!    without threading a handle: the module-level `OnceLock` global makes
//!    hooks additive across the 26+ submission call sites.
//! 2. **Drop, never re-queue.** A failed flush is warned (rate-limited) and
//!    discarded — retrying error reports amplifies outages, and the server
//!    keeps its own durability. An older server answering UNIMPLEMENTED is a
//!    normal rollout state and is handled identically.
//! 3. Reporting can never affect the calling flow: before `init()` (or if
//!    init never runs) `report()` is a silent no-op.
//!
//! Flush cadence: 50 events or 5 s, whichever first. JWTs are short-TTL, so
//! `init` takes a `JwtSource` closure and the flusher mints a fresh token
//! per flush (an Ed25519 sign — cheap at this cadence).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use orderbook_proto::settlement::ErrorEvent;
use tokio::sync::mpsc;
use tracing::warn;

use crate::rpc_client::OrderbookRpcClient;

/// Buffered events before `try_send` drops (diagnostic trail, not ledger).
const BUFFER_EVENTS: usize = 1024;
/// Flush when this many events are buffered, or on the interval. Matches the
/// server's max batch size (50).
const FLUSH_MAX_EVENTS: usize = 50;
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Rate limit for drop/flush-failure warnings.
const WARN_EVERY: Duration = Duration::from_secs(60);

/// Fresh-JWT provider for the flusher (agent JWTs are short-TTL).
pub type JwtSource = Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>;

struct Reporter {
    tx: mpsc::Sender<ErrorEvent>,
    dropped: AtomicU64,
    last_warn: Mutex<Option<Instant>>,
}

static REPORTER: OnceLock<Reporter> = OnceLock::new();

/// Install the process-global reporter and spawn its flusher. Idempotent —
/// the first call wins (both init sites may race; either configuration is
/// equivalent). Requires a tokio runtime; if none is active the reporter is
/// NOT installed (a live sender with no flusher would fill and drop forever).
pub fn init(orderbook_url: String, jwt_source: JwtSource) {
    // Spawn the flusher FIRST, so REPORTER is only ever set once a receiver
    // exists. If there is no runtime, `spawn` would panic — guard on it and
    // stay uninstalled rather than leave a senders-only Reporter behind.
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("error reporter init called outside a tokio runtime — not installed");
        return;
    }
    let (tx, rx) = mpsc::channel(BUFFER_EVENTS);
    let handle = tokio::spawn(run_flusher(orderbook_url, jwt_source, rx));
    if REPORTER
        .set(Reporter {
            tx,
            dropped: AtomicU64::new(0),
            last_warn: Mutex::new(None),
        })
        .is_ok()
    {
        tracing::info!("error reporter installed (ReportErrors -> orderbook-rpc)");
    } else {
        // Lost the init race: abort our flusher and drop our channel.
        handle.abort();
    }
}

pub fn is_installed() -> bool {
    REPORTER.get().is_some()
}

/// Idempotent init from a `BaseConfig` — the JWT closure mints a fresh
/// short-TTL token per flush from the config's signing key.
pub fn init_from_config(config: &crate::config::BaseConfig) {
    if is_installed() {
        return;
    }
    let url = config.orderbook_grpc_url.clone();
    let party_id = config.party_id.clone();
    let role = config.role.clone();
    let private_key = config.private_key.clone();
    let token_ttl_secs = config.token_ttl_secs;
    let node_name = config.node_name.clone();
    init(
        url,
        Arc::new(move || {
            crate::auth::generate_jwt(
                &party_id,
                &role,
                &private_key.expose(),
                token_ttl_secs,
                Some(node_name.as_str()),
            )
        }),
    );
}

/// Queue one error event. Sync, never blocks, never awaits, never fails the
/// caller; silent no-op before `init()`.
pub fn report(event: ErrorEvent) {
    let Some(r) = REPORTER.get() else { return };
    if r.tx.try_send(event).is_err() {
        let n = r.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        // Poison-safe: this is the one panicking path in a function
        // documented "never fails the caller", and it runs on the sync
        // submission path (ledger_health, ledger_client) — reuse a poisoned
        // guard rather than propagate the panic.
        let mut warn_at = r
            .last_warn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if warn_at.is_none_or(|t| t.elapsed() >= WARN_EVERY) {
            *warn_at = Some(Instant::now());
            warn!(
                "error reporter buffer full — {} events dropped so far (lossy by design)",
                n
            );
        }
    }
}

async fn run_flusher(url: String, jwt_source: JwtSource, mut rx: mpsc::Receiver<ErrorEvent>) {
    let mut client: Option<OrderbookRpcClient> = None;
    let mut last_warn: Option<Instant> = None;
    loop {
        let Some(first) = rx.recv().await else { return };
        let mut batch = vec![first];
        let deadline = tokio::time::Instant::now() + FLUSH_INTERVAL;
        while batch.len() < FLUSH_MAX_EVENTS {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(event)) => batch.push(event),
                Ok(None) => break,
                Err(_) => break, // interval elapsed
            }
        }

        // Connect lazily (the tonic channel is process-cached, so this is
        // cheap after the first success) and refresh the short-TTL JWT.
        if client.is_none() {
            match OrderbookRpcClient::connect(&url, None).await {
                Ok(c) => client = Some(c),
                Err(e) => {
                    warn_rate_limited(&mut last_warn, &format!("connect failed: {e:#}"), batch.len());
                    continue; // batch dropped, never re-queued
                }
            }
        }
        let c = client.as_mut().expect("client just ensured");
        match jwt_source() {
            Ok(jwt) => c.set_jwt(jwt),
            Err(e) => {
                warn_rate_limited(&mut last_warn, &format!("jwt mint failed: {e:#}"), batch.len());
                continue;
            }
        }

        let n = batch.len();
        match c.report_errors(batch).await {
            Ok((_accepted, rejected)) if rejected > 0 => {
                // The server rate-limited some events (party/venue window) —
                // visible ONLY here, since a partial-accept is not an RPC
                // error. Rate-limited so a sustained storm doesn't spam.
                warn_rate_limited(
                    &mut last_warn,
                    &format!("server rejected {rejected} of {n} error reports (rate-limited)"),
                    rejected as usize,
                );
            }
            Ok(_) => {}
            Err(e) => {
                // Includes UNIMPLEMENTED from an older server — same handling.
                warn_rate_limited(&mut last_warn, &format!("{e:#}"), n);
                // Drop the client so the next flush re-resolves it; tonic
                // reconnects the cached channel internally either way.
                client = None;
            }
        }
    }
}

fn warn_rate_limited(last_warn: &mut Option<Instant>, err: &str, dropped: usize) {
    if last_warn.is_none_or(|t| t.elapsed() >= WARN_EVERY) {
        *last_warn = Some(Instant::now());
        warn!(
            dropped,
            error = %err,
            "error report flush failed — batch dropped (never re-queued)"
        );
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Convenience builder with agent defaults: `source="agent"`,
/// `severity="error"`, `occurred_at=now`. Daml template/choice are not known
/// agent-side and are deliberately absent (server/SDK sources fill them).
pub struct ErrorEventBuilder {
    event: ErrorEvent,
}

impl ErrorEventBuilder {
    pub fn new(error_type: &str, error_message: impl Into<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            event: ErrorEvent {
                source: "agent".to_string(),
                severity: "error".to_string(),
                error_type: error_type.to_string(),
                error_message: error_message.into(),
                occurred_at: Some(prost_types::Timestamp {
                    seconds: now.timestamp(),
                    nanos: now.timestamp_subsec_nanos() as i32,
                }),
                ..Default::default()
            },
        }
    }

    pub fn severity(mut self, severity: &str) -> Self {
        self.event.severity = severity.to_string();
        self
    }
    pub fn error_code(mut self, code: impl Into<String>) -> Self {
        self.event.error_code = Some(code.into());
        self
    }
    pub fn party(mut self, party_id: impl Into<String>) -> Self {
        self.event.party_id = Some(party_id.into());
        self
    }
    pub fn update_id(mut self, update_id: impl Into<String>) -> Self {
        self.event.update_id = Some(update_id.into());
        self
    }
    pub fn command_id(mut self, command_id: impl Into<String>) -> Self {
        self.event.command_id = Some(command_id.into());
        self
    }
    pub fn contract_id(mut self, contract_id: impl Into<String>) -> Self {
        self.event.contract_id = Some(contract_id.into());
        self
    }
    pub fn settlement_proposal_id(mut self, id: impl Into<String>) -> Self {
        self.event.settlement_proposal_id = Some(id.into());
        self
    }
    pub fn market_id(mut self, market_id: impl Into<String>) -> Self {
        self.event.market_id = Some(market_id.into());
        self
    }
    pub fn order_id(mut self, order_id: impl Into<String>) -> Self {
        self.event.order_id = Some(order_id.into());
        self
    }
    pub fn venue_name(mut self, venue: impl Into<String>) -> Self {
        self.event.venue_name = Some(venue.into());
        self
    }
    pub fn venue_branch(mut self, branch: impl Into<String>) -> Self {
        self.event.venue_branch = Some(branch.into());
        self
    }
    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.event.module = Some(module.into());
        self
    }
    pub fn metadata(mut self, value: serde_json::Value) -> Self {
        self.event.metadata = json_to_prost_struct(&value);
        self
    }

    pub fn build(self) -> ErrorEvent {
        self.event
    }

    /// Build and queue in one call.
    pub fn send(self) {
        report(self.build());
    }
}

/// serde_json object -> prost Struct (None for non-objects).
fn json_to_prost_struct(value: &serde_json::Value) -> Option<prost_types::Struct> {
    let serde_json::Value::Object(map) = value else {
        return None;
    };
    Some(prost_types::Struct {
        fields: map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_prost_value(v)))
            .collect(),
    })
}

fn json_to_prost_value(value: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match value {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.iter().map(json_to_prost_value).collect(),
        }),
        serde_json::Value::Object(_) => Kind::StructValue(
            json_to_prost_struct(value).expect("object checked above"),
        ),
    };
    prost_types::Value { kind: Some(kind) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_before_init_is_a_silent_noop() {
        // Must not panic or block (REPORTER may or may not be set depending
        // on test ordering; either way this must return immediately).
        report(ErrorEventBuilder::new("UNKNOWN", "no reporter").build());
    }

    #[test]
    fn builder_defaults_and_setters() {
        let e = ErrorEventBuilder::new("ledger_unhealthy", "breaker open")
            .severity("critical")
            .party("alice::1220")
            .settlement_proposal_id("0198")
            .metadata(serde_json::json!({"attempts": 3, "op": "ProposeDvp"}))
            .build();
        assert_eq!(e.source, "agent");
        assert_eq!(e.severity, "critical");
        assert_eq!(e.error_type, "ledger_unhealthy");
        assert!(e.occurred_at.is_some());
        assert_eq!(e.party_id.as_deref(), Some("alice::1220"));
        let meta = e.metadata.expect("metadata");
        assert!(meta.fields.contains_key("attempts"));
        assert!(meta.fields.contains_key("op"));
    }
}
