//! LP settlement (V1) and atomic RFQ (V2) streams; every open, send and silence is bounded.
//! Confirms, sweeps and settle observations run in their own tasks.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Mutex as TokioMutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tracing::{debug, error, info, warn};

use agent_logic::clock;
use agent_logic::config::BaseConfig;
use agent_logic::shutdown::Shutdown;
use agent_logic::supervise::{self, Policy};
use agent_logic::transport::RPC_TIMEOUT;
use orderbook_proto::rfqv2::{
    AtomicHandshake, AtomicHeartbeat, AtomicLpToServer, AtomicRfqQuote, AtomicRfqReject,
    AtomicRfqRequest, AtomicServerToLp, RfqConfirmReject, RfqConfirmRejectReason,
    RfqConfirmRequest, atomic_lp_to_server::Message as LpMessage,
    atomic_rfq_service_client::AtomicRfqServiceClient,
    atomic_server_to_lp::Message as AtomicServerMessage,
};
use orderbook_proto::settlement::{
    CantonNodeAuth, CantonToServerMessage, Heartbeat, ServerToCantonMessage, SettlementHandshake,
    canton_to_server_message::Message as CantonMessage,
    server_to_canton_message::Message as ServerMessage,
    settlement_service_client::SettlementServiceClient,
};

use crate::rfq_handler::{RfqHandler, RfqResponse};
use crate::rfq_v2::{RfqV2State, SettleObserved};

/// Client heartbeat period on both LP streams.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const OUTBOUND_CAPACITY: usize = 64;
const MAX_DECODING_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
/// Queued settle observations above this are logged as a backlog.
const SETTLE_BACKLOG_WARN: usize = 1000;
const SETTLE_BACKLOG_WARN_EVERY: Duration = Duration::from_secs(60);

/// Time bounds of the LP streams.
#[derive(Clone, Copy, Debug)]
struct Timing {
    /// Stream open; a slower open reconnects.
    open: Duration,
    /// One outbound send; a stalled send reconnects.
    send: Duration,
    heartbeat: Duration,
    /// Inbound silence that ends a session once the server has sent a heartbeat.
    idle: Duration,
    /// Pricing one RFQ.
    handler: Duration,
    /// Pause before reconnecting.
    reconnect: Duration,
    /// Expiry sweep period of the atomic stream.
    sweep: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            open: RPC_TIMEOUT,
            send: Duration::from_secs(10),
            heartbeat: HEARTBEAT_INTERVAL,
            idle: HEARTBEAT_INTERVAL.saturating_mul(3),
            handler: Duration::from_secs(5),
            reconnect: Duration::from_secs(5),
            sweep: Duration::from_secs(10),
        }
    }
}

/// Why a connected session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionEnd {
    Shutdown,
    Ended,
    Failed,
    SendFailed,
    Idle,
}

/// Why an outbound message was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendFailure {
    Closed,
    Stalled(Duration),
    Full,
}

impl fmt::Display for SendFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendFailure::Closed => f.write_str("outbound stream closed"),
            SendFailure::Stalled(d) => write!(f, "send stalled for {d:?}"),
            SendFailure::Full => f.write_str("outbound queue full"),
        }
    }
}

/// Queue `msg` within `within`.
async fn send_bounded<T>(tx: &mpsc::Sender<T>, msg: T, within: Duration) -> Result<(), SendFailure> {
    match tokio::time::timeout(within, tx.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(SendFailure::Closed),
        Err(_) => Err(SendFailure::Stalled(within)),
    }
}

/// Queue a heartbeat without waiting; a full queue means the stream is not draining.
fn send_heartbeat<T>(tx: &mpsc::Sender<T>, msg: T) -> Result<(), SendFailure> {
    tx.try_send(msg).map_err(|e| match e {
        TrySendError::Full(_) => SendFailure::Full,
        TrySendError::Closed(_) => SendFailure::Closed,
    })
}

/// `None` when shutdown wins the race.
async fn until_shutdown<F: Future>(shutdown: &Shutdown, fut: F) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = shutdown.wait() => None,
        out = fut => Some(out),
    }
}

/// Open within `within`, raced with shutdown.
async fn open_bounded<F: Future>(
    shutdown: &Shutdown,
    within: Duration,
    open: F,
) -> Option<Result<F::Output, tokio::time::error::Elapsed>> {
    until_shutdown(shutdown, tokio::time::timeout(within, open)).await
}

/// Next outbound sequence number; wraps at `u64::MAX`.
fn next_seq(seq: &AtomicU64) -> u64 {
    seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

fn now_ts() -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: clock::now_secs_i64(),
        nanos: 0,
    }
}

/// Ticks every `period`, first at `first`; late ticks are delayed, never bunched.
fn ticker(first: tokio::time::Instant, period: Duration) -> tokio::time::Interval {
    let mut t = tokio::time::interval_at(first, period.max(Duration::from_millis(1)));
    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    t
}

/// Ends a session after inbound silence; armed by the first server heartbeat,
/// so a server that never sends heartbeats is never cut off.
struct IdleWatchdog {
    idle: Duration,
    armed: bool,
    deadline: Pin<Box<tokio::time::Sleep>>,
}

impl IdleWatchdog {
    fn new(idle: Duration) -> Self {
        Self {
            idle,
            armed: false,
            deadline: Box::pin(tokio::time::sleep_until(clock::deadline_after(idle))),
        }
    }

    /// Any inbound message restarts the window; a server heartbeat also arms it.
    fn on_message(&mut self, server_heartbeat: bool) {
        self.armed = self.armed || server_heartbeat;
        self.deadline.as_mut().reset(clock::deadline_after(self.idle));
    }

    /// Completes once armed and silent for the whole window.
    async fn expired(&mut self) {
        if self.armed {
            self.deadline.as_mut().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
}

/// Holds an in-flight flag; dropping it clears the flag, also on unwind.
struct InFlight(Arc<AtomicBool>);

impl InFlight {
    fn claim(flag: &Arc<AtomicBool>) -> Option<Self> {
        (!flag.swap(true, Ordering::AcqRel)).then(|| Self(Arc::clone(flag)))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Fresh Bearer JWT for a stream open.
fn bearer(config: &BaseConfig) -> Result<tonic::metadata::MetadataValue<tonic::metadata::Ascii>> {
    let key = config.private_key.expose()?;
    let jwt = agent_logic::auth::generate_jwt(
        &config.party_id,
        &config.role,
        &key,
        config.token_ttl_secs,
        Some(&config.node_name),
    )
    .map_err(|e| anyhow!("{e}"))?;
    format!("Bearer {jwt}").parse().map_err(|e| anyhow!("{e}"))
}

// ---------------------------------------------------------------------------
// V1 settlement stream
// ---------------------------------------------------------------------------

/// Run the LP settlement stream (bidirectional gRPC for RFQ handling).
/// Never spawned when `rfq_v2_only = true` — without the handshake's
/// `liquidity_provider_name` registration the server routes no V1 RFQs to
/// this party and it drops out of `GetConnectedLiquidityProviders`.
pub async fn run_lp_settlement_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    shutdown: Shutdown,
) -> Result<()> {
    settlement_stream(config, rfq_handler, shutdown, Timing::default()).await
}

/// Run the V1 settlement stream as a task that restarts if it fails.
pub(crate) fn spawn_settlement_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    shutdown: Shutdown,
) -> Result<JoinHandle<()>> {
    let s = shutdown.clone();
    supervise::spawn_supervised("LP settlement stream", shutdown, Policy::Restart, move || {
        let (config, handler, shutdown) = (config.clone(), Arc::clone(&rfq_handler), s.clone());
        async move {
            if let Err(e) = run_lp_settlement_stream(config, handler, shutdown).await {
                error!("LP settlement stream failed: {}", e);
            }
        }
    })
}

async fn settlement_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    shutdown: Shutdown,
    timing: Timing,
) -> Result<()> {
    let lp_config = config
        .liquidity_provider
        .as_ref()
        .ok_or_else(|| anyhow!("No LP config"))?;
    let retry_secs = timing.reconnect.as_secs();

    loop {
        if shutdown.is_shutting_down() {
            info!("LP stream shutting down, not reconnecting");
            return Ok(());
        }

        info!("Connecting LP settlement stream to {}", config.orderbook_grpc_url);

        let channel = match until_shutdown(&shutdown, crate::create_raw_channel(&config.orderbook_grpc_url)).await {
            None => return Ok(()),
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                error!("Failed to connect: {}, retrying in {}s", e, retry_secs);
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut client =
            SettlementServiceClient::new(channel).max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        let (outbound_tx, outbound_rx) = mpsc::channel::<CantonToServerMessage>(OUTBOUND_CAPACITY);
        let auth_header = match bearer(&config) {
            Ok(h) => h,
            Err(e) => {
                error!(
                    "LP settlement stream: failed to build auth token: {}, retrying in {}s",
                    e, retry_secs
                );
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };
        let mut open_request = tonic::Request::new(ReceiverStream::new(outbound_rx));
        open_request.metadata_mut().insert("authorization", auth_header);

        let response = match open_bounded(&shutdown, timing.open, client.settlement_stream(open_request)).await {
            None => return Ok(()),
            Some(Ok(Ok(r))) => r,
            Some(Ok(Err(e))) => {
                error!("Failed to open settlement stream: {}, retrying in {}s", e, retry_secs);
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
            Some(Err(_)) => {
                error!(
                    "Failed to open settlement stream: no response within {:?}, retrying in {}s",
                    timing.open, retry_secs
                );
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };
        let inbound = response.into_inner();

        // Send handshake with LP name
        let handshake = CantonToServerMessage {
            session_id: String::new(),
            sequence_number: 0,
            message: Some(CantonMessage::Handshake(SettlementHandshake {
                auth: Some(CantonNodeAuth {
                    party_id: config.party_id.clone(),
                    jwt_token: String::new(),
                    node_instance: config.node_name.clone(),
                    connected_at: Some(now_ts()),
                }),
                party_ids: vec![config.party_id.clone()],
                user_services: vec![],
                operator_party: config.settlement_operator.clone(),
                capabilities: None,
                liquidity_provider_name: Some(lp_config.name.clone()),
            })),
            sent_at: Some(now_ts()),
        };
        if let Err(e) = send_bounded(&outbound_tx, handshake, timing.send).await {
            error!("Failed to send handshake, retrying ({e})");
            if shutdown.sleep(timing.reconnect).await {
                return Ok(());
            }
            continue;
        }

        info!("LP settlement stream connected, listening for RFQ requests");
        settlement_session(&rfq_handler, inbound, &outbound_tx, &shutdown, timing).await;

        if shutdown.is_shutting_down() {
            info!("LP stream shutting down after disconnect");
            return Ok(());
        }
        warn!("LP settlement stream disconnected, reconnecting in {}s", retry_secs);
        if shutdown.sleep(timing.reconnect).await {
            return Ok(());
        }
    }
}

/// Serve one connected settlement stream until it ends.
async fn settlement_session<S>(
    rfq_handler: &RfqHandler,
    mut inbound: S,
    outbound: &mpsc::Sender<CantonToServerMessage>,
    shutdown: &Shutdown,
    timing: Timing,
) -> SessionEnd
where
    S: Stream<Item = Result<ServerToCantonMessage, tonic::Status>> + Unpin,
{
    let mut heartbeat = ticker(clock::deadline_after(timing.heartbeat), timing.heartbeat);
    let mut watchdog = IdleWatchdog::new(timing.idle);
    let mut client_seq: u64 = 0;

    loop {
        tokio::select! {
            biased;
            // Observed only while idle: an RFQ being handled runs to completion.
            _ = shutdown.wait() => {
                info!("LP settlement stream observed shutdown, breaking inner loop");
                return SessionEnd::Shutdown;
            }
            _ = heartbeat.tick() => {
                client_seq = client_seq.wrapping_add(1);
                let now = now_ts();
                let hb = CantonToServerMessage {
                    session_id: String::new(),
                    sequence_number: client_seq,
                    message: Some(CantonMessage::Heartbeat(Heartbeat {
                        session_id: String::new(),
                        sequence_number: client_seq,
                        timestamp: Some(now),
                    })),
                    sent_at: Some(now),
                };
                if let Err(e) = send_heartbeat(outbound, hb) {
                    warn!("LP settlement stream send-failed on heartbeat, reconnecting ({e})");
                    return SessionEnd::SendFailed;
                }
            }
            msg_result = inbound.next() => {
                let msg = match msg_result {
                    None => {
                        warn!("LP settlement stream ended (None), reconnecting");
                        return SessionEnd::Ended;
                    }
                    Some(Err(e)) => {
                        error!("LP settlement stream error: {}", e);
                        return SessionEnd::Failed;
                    }
                    Some(Ok(m)) => m,
                };
                watchdog.on_message(matches!(msg.message, Some(ServerMessage::Heartbeat(_))));

                match msg.message {
                    Some(ServerMessage::HandshakeAck(ack)) => {
                        info!("LP handshake acknowledged: accepted={}", ack.accepted);
                    }
                    Some(ServerMessage::RfqRequest(request)) => {
                        if shutdown.is_shutting_down() {
                            info!("Ignoring RFQ {} - shutting down", request.rfq_id);
                            return SessionEnd::Shutdown;
                        }
                        info!(
                            "Received RFQ request: rfq_id={}, market={}, direction={}, qty={}",
                            request.rfq_id, request.market_id, request.direction, request.quantity
                        );
                        let rfq_id = request.rfq_id.clone();
                        // Pricing commits nothing before its last await, so it may be cut short.
                        let response = match tokio::time::timeout(
                            timing.handler,
                            rfq_handler.handle_rfq_request(request),
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                warn!("RFQ {}: not priced within {:?}, rejecting", rfq_id, timing.handler);
                                rfq_handler.unavailable(rfq_id)
                            }
                        };
                        let message = match response {
                            RfqResponse::Quote(quote) => CantonMessage::RfqQuote(quote),
                            RfqResponse::Reject(reject) => CantonMessage::RfqReject(reject),
                        };
                        let response_msg = CantonToServerMessage {
                            session_id: String::new(),
                            sequence_number: 0,
                            message: Some(message),
                            sent_at: Some(now_ts()),
                        };
                        if let Err(e) = send_bounded(outbound, response_msg, timing.send).await {
                            error!("Failed to send RFQ response, stream may be closed ({e})");
                            return SessionEnd::SendFailed;
                        }
                    }
                    Some(ServerMessage::Heartbeat(_)) => {
                        // Server-side keepalive; it arms the idle watchdog.
                    }
                    other => {
                        debug!("LP stream received unhandled message: {:?}", other.map(|_| "..."));
                    }
                }
            }
            _ = watchdog.expired() => {
                warn!(
                    "LP settlement stream silent for {:?} after a server heartbeat, reconnecting",
                    timing.idle
                );
                return SessionEnd::Idle;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// V2 atomic RFQ stream
// ---------------------------------------------------------------------------

/// Shared by every session of one atomic stream.
struct AtomicCtx {
    party_id: String,
    lp_name: String,
    rfq_handler: Arc<RfqHandler>,
    state: Arc<RfqV2State>,
    sweep_running: Arc<AtomicBool>,
}

fn wrap(seq: u64, message: LpMessage) -> AtomicLpToServer {
    AtomicLpToServer {
        session_id: String::new(),
        sequence_number: seq,
        message: Some(message),
        sent_at: Some(now_ts()),
    }
}

/// Run the RFQ V2 atomic stream (design §5.5): a second bidi stream parallel
/// to the v1 settlement stream. Phase 1 (AtomicRfqRequest) prices through the
/// SHARED v1 pipeline with an advisory availability check (no reserve); phase
/// 2 (RfqConfirmRequest) commits the LiquidityManager funds, hard-reserves
/// holdings, signs, and returns the disclosure envelope.
pub async fn run_lp_atomic_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    state: Arc<RfqV2State>,
    mut settle_rx: mpsc::UnboundedReceiver<SettleObserved>,
    agent_version: String,
    shutdown: Shutdown,
) -> Result<()> {
    // Settle-observation consumer: lives across stream reconnects so watcher
    // events are handled even while the stream is down.
    let consumer = {
        let (state, shutdown) = (Arc::clone(&state), shutdown.clone());
        async move { consume_settles(state, &mut settle_rx, shutdown).await }
    };
    supervise::try_spawn("settle observation consumer", consumer);
    atomic_stream(config, rfq_handler, state, agent_version, shutdown, Timing::default()).await
}

/// Run the V2 atomic stream and its settle consumer as two tasks that each
/// restart if they fail; the consumer keeps the receiver across restarts.
pub(crate) fn spawn_atomic_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    state: Arc<RfqV2State>,
    settle_rx: mpsc::UnboundedReceiver<SettleObserved>,
    agent_version: String,
    shutdown: Shutdown,
) -> Result<(JoinHandle<()>, JoinHandle<()>)> {
    let consumer = supervise::spawn_supervised(
        "settle observation consumer",
        shutdown.clone(),
        Policy::Restart,
        settle_consumer(Arc::clone(&state), settle_rx, shutdown.clone()),
    )?;
    let s = shutdown.clone();
    let stream = supervise::spawn_supervised("LP atomic stream", shutdown, Policy::Restart, move || {
        let run = atomic_stream(
            config.clone(),
            Arc::clone(&rfq_handler),
            Arc::clone(&state),
            agent_version.clone(),
            s.clone(),
            Timing::default(),
        );
        async move {
            if let Err(e) = run.await {
                error!("LP atomic stream failed: {}", e);
            }
        }
    });
    match stream {
        Ok(stream) => Ok((consumer, stream)),
        Err(e) => {
            consumer.abort();
            Err(e)
        }
    }
}

/// Starts of the settle consumer; each start takes the shared receiver.
fn settle_consumer(
    state: Arc<RfqV2State>,
    settle_rx: mpsc::UnboundedReceiver<SettleObserved>,
    shutdown: Shutdown,
) -> impl FnMut() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + 'static {
    let settle_rx = Arc::new(TokioMutex::new(settle_rx));
    move || {
        let (state, rx, shutdown) = (Arc::clone(&state), Arc::clone(&settle_rx), shutdown.clone());
        Box::pin(async move {
            let mut rx = rx.lock().await;
            consume_settles(state, &mut rx, shutdown).await;
        })
    }
}

async fn atomic_stream(
    config: BaseConfig,
    rfq_handler: Arc<RfqHandler>,
    state: Arc<RfqV2State>,
    agent_version: String,
    shutdown: Shutdown,
    timing: Timing,
) -> Result<()> {
    let ctx = AtomicCtx {
        party_id: config.party_id.clone(),
        lp_name: state.lp_name().to_string(),
        rfq_handler,
        state: Arc::clone(&state),
        sweep_running: Arc::new(AtomicBool::new(false)),
    };
    let retry_secs = timing.reconnect.as_secs();

    loop {
        if shutdown.is_shutting_down() {
            info!("LP atomic stream shutting down, not reconnecting");
            return Ok(());
        }

        info!("Connecting LP atomic stream to {}", config.orderbook_grpc_url);

        let channel = match until_shutdown(&shutdown, crate::create_raw_channel(&config.orderbook_grpc_url)).await {
            None => return Ok(()),
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                error!("Atomic stream: failed to connect: {}, retrying in {}s", e, retry_secs);
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };

        let mut client =
            AtomicRfqServiceClient::new(channel).max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        let (outbound_tx, outbound_rx) = mpsc::channel::<AtomicLpToServer>(OUTBOUND_CAPACITY);

        // The V2 stream requires a Bearer JWT at open (no CantonNodeAuth
        // fallback, unlike the v1 settlement stream). Fresh per reconnect.
        let auth_header = match bearer(&config) {
            Ok(h) => h,
            Err(e) => {
                error!("Atomic stream: failed to build auth token: {}, retrying in {}s", e, retry_secs);
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };
        let mut open_request = tonic::Request::new(ReceiverStream::new(outbound_rx));
        open_request.metadata_mut().insert("authorization", auth_header);

        let response = match open_bounded(&shutdown, timing.open, client.atomic_rfq_stream(open_request)).await {
            None => return Ok(()),
            Some(Ok(Ok(r))) => r,
            Some(Ok(Err(e))) => {
                error!("Failed to open atomic RFQ stream: {}, retrying in {}s", e, retry_secs);
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
            Some(Err(_)) => {
                error!(
                    "Failed to open atomic RFQ stream: no response within {:?}, retrying in {}s",
                    timing.open, retry_secs
                );
                if shutdown.sleep(timing.reconnect).await {
                    return Ok(());
                }
                continue;
            }
        };
        let inbound = response.into_inner();

        let handshake = wrap(
            0,
            LpMessage::Handshake(AtomicHandshake {
                party_ids: vec![config.party_id.clone()],
                lp_name: ctx.lp_name.clone(),
                agent_version: agent_version.clone(),
                market_ids: ctx.state.validated_market_ids(),
            }),
        );
        if let Err(e) = send_bounded(&outbound_tx, handshake, timing.send).await {
            error!("Atomic stream: failed to send handshake, retrying ({e})");
            if shutdown.sleep(timing.reconnect).await {
                return Ok(());
            }
            continue;
        }

        info!("LP atomic stream connected, listening for atomic RFQ requests");
        atomic_session(&ctx, inbound, &outbound_tx, &shutdown, timing).await;

        if shutdown.is_shutting_down() {
            info!("LP atomic stream shutting down after disconnect");
            return Ok(());
        }
        warn!("LP atomic stream disconnected, reconnecting in {}s", retry_secs);
        if shutdown.sleep(timing.reconnect).await {
            return Ok(());
        }
    }
}

/// Serve one connected atomic stream until it ends.
async fn atomic_session<S>(
    ctx: &AtomicCtx,
    mut inbound: S,
    outbound: &mpsc::Sender<AtomicLpToServer>,
    shutdown: &Shutdown,
    timing: Timing,
) -> SessionEnd
where
    S: Stream<Item = Result<AtomicServerToLp, tonic::Status>> + Unpin,
{
    // Shared with the confirm tasks of this session.
    let seq = Arc::new(AtomicU64::new(0));
    let send_failed = Arc::new(Notify::new());
    let mut heartbeat = ticker(clock::deadline_after(timing.heartbeat), timing.heartbeat);
    let mut sweep = ticker(tokio::time::Instant::now(), timing.sweep);
    let mut watchdog = IdleWatchdog::new(timing.idle);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => {
                info!("LP atomic stream observed shutdown, breaking inner loop");
                return SessionEnd::Shutdown;
            }
            _ = send_failed.notified() => {
                warn!("LP atomic stream send-failed on a confirm response, reconnecting");
                return SessionEnd::SendFailed;
            }
            _ = heartbeat.tick() => {
                let hb = wrap(next_seq(&seq), LpMessage::Heartbeat(AtomicHeartbeat { at: Some(now_ts()) }));
                if let Err(e) = send_heartbeat(outbound, hb) {
                    warn!("LP atomic stream send-failed on heartbeat, reconnecting ({e})");
                    return SessionEnd::SendFailed;
                }
            }
            _ = sweep.tick() => {
                // Releases expired confirm-time LM commitments / hard
                // reserves / ticket assignments (traceability row 9)
                spawn_sweep(&ctx.state, &ctx.sweep_running);
            }
            msg_result = inbound.next() => {
                let msg = match msg_result {
                    None => {
                        warn!("LP atomic stream ended (None), reconnecting");
                        return SessionEnd::Ended;
                    }
                    Some(Err(e)) => {
                        error!("LP atomic stream error: {}", e);
                        return SessionEnd::Failed;
                    }
                    Some(Ok(m)) => m,
                };
                watchdog.on_message(matches!(msg.message, Some(AtomicServerMessage::Heartbeat(_))));

                match msg.message {
                    Some(AtomicServerMessage::HandshakeAck(ack)) => {
                        info!("LP atomic handshake acknowledged: success={} session={}", ack.success, ack.session_id);
                    }
                    Some(AtomicServerMessage::Heartbeat(_)) => {
                        // Server-side keepalive; it arms the idle watchdog.
                    }
                    Some(AtomicServerMessage::RfqRequest(request)) => {
                        if shutdown.is_shutting_down() {
                            info!("Ignoring atomic RFQ {} - shutting down", request.rfq_id);
                            return SessionEnd::Shutdown;
                        }
                        let message = answer_atomic_rfq(ctx, &request, timing.handler).await;
                        if let Err(e) = send_bounded(outbound, wrap(next_seq(&seq), message), timing.send).await {
                            error!("Failed to send atomic RFQ response, stream may be closed ({e})");
                            return SessionEnd::SendFailed;
                        }
                    }
                    Some(AtomicServerMessage::ConfirmRequest(req)) => {
                        info!(
                            "Received atomic confirm: rfq_id={}, quote_id={}, user={}",
                            req.rfq_id, req.quote_id, req.user_party
                        );
                        spawn_confirm(ctx, req, outbound.clone(), Arc::clone(&seq), Arc::clone(&send_failed), timing.send);
                    }
                    None => {
                        debug!("LP atomic stream received empty message");
                    }
                }
            }
            _ = watchdog.expired() => {
                warn!(
                    "LP atomic stream silent for {:?} after a server heartbeat, reconnecting",
                    timing.idle
                );
                return SessionEnd::Idle;
            }
        }
    }
}

fn atomic_reject(ctx: &AtomicCtx, request: &AtomicRfqRequest, reason: String, min: String, max: String) -> LpMessage {
    LpMessage::Reject(AtomicRfqReject {
        rfq_id: request.rfq_id.clone(),
        market_id: request.market_id.clone(),
        reason,
        lp_party_id: ctx.party_id.clone(),
        min_quantity: min,
        max_quantity: max,
    })
}

/// Indicative quote or reject for one atomic RFQ.
async fn answer_atomic_rfq(ctx: &AtomicCtx, request: &AtomicRfqRequest, within: Duration) -> LpMessage {
    // Venue identity for [[venue_overrides]] pricing: explicit venue_name,
    // falling back to the VA2 attribution prefix for servers predating it.
    let venue = request
        .venue_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(request.quote_id_prefix.as_deref().filter(|s| !s.is_empty()));
    let venue_branch = request.venue_branch.as_deref().filter(|s| !s.is_empty());
    // Requesting party id; keys the per-counterparty accumulator. Absent ⇒ no per-party term.
    let user_party = request.user_party.as_deref().filter(|s| !s.is_empty());
    info!(
        "Received atomic RFQ: rfq_id={}, market={}, direction={}, qty={}, venue={}{}",
        request.rfq_id,
        request.market_id,
        request.direction,
        request.quantity,
        venue.unwrap_or("-"),
        venue_branch.map(|b| format!("/{b}")).unwrap_or_default()
    );

    // V2 direction is a string; the shared pricing fn takes the v1 i32 enum
    // (1=BUY user buys, 2=SELL)
    let direction = match request.direction.to_ascii_lowercase().as_str() {
        "buy" => 1,
        "sell" => 2,
        _ => {
            let reason = format!("invalid direction '{}'", request.direction);
            return atomic_reject(ctx, request, reason, String::new(), String::new());
        }
    };
    if !ctx.state.quotable(&request.market_id) {
        let reason = "market not available for atomic RFQ".to_string();
        return atomic_reject(ctx, request, reason, String::new(), String::new());
    }

    // Pricing and registration commit nothing before their last await, so they may be cut short.
    let priced = price_atomic_rfq(ctx, request, direction, venue, venue_branch, user_party);
    match tokio::time::timeout(within, priced).await {
        Ok(message) => message,
        Err(_) => {
            warn!("Atomic RFQ {}: not priced within {:?}, rejecting", request.rfq_id, within);
            atomic_reject(ctx, request, "temporarily unavailable".to_string(), String::new(), String::new())
        }
    }
}

async fn price_atomic_rfq(
    ctx: &AtomicCtx,
    request: &AtomicRfqRequest,
    direction: i32,
    venue: Option<&str>,
    venue_branch: Option<&str>,
    user_party: Option<&str>,
) -> LpMessage {
    let priced = match ctx
        .rfq_handler
        .price_rfq(
            &request.rfq_id,
            &request.market_id,
            direction,
            &request.quantity,
            request.quote_quantity.as_deref().unwrap_or(""),
            // V2: no min-notional floor — the user pays every fee (3x dust
            // surcharge server-side); the LP pays none.
            false,
            venue,
            venue_branch,
            user_party,
        )
        .await
    {
        Ok(p) => p,
        Err(r) => {
            let reason = r.reason_detail.unwrap_or_else(|| format!("{:?}", r.reason));
            return atomic_reject(
                ctx,
                request,
                reason,
                r.min_quantity.unwrap_or_default(),
                r.max_quantity.unwrap_or_default(),
            );
        }
    };

    // Venue-attribution (VA2): when the server supplies a prefix, the quote id
    // becomes "<venue>-<uuidv7>" so the venue slug rides the signed canonical
    // message (quote_nonce) onto the chain. The server DROPS quotes that fail
    // to echo the expected prefix.
    let id = match clock::uuid_v7() {
        Ok(id) => id,
        Err(e) => {
            warn!("Atomic RFQ {}: no quote id available: {:#}", request.rfq_id, e);
            let reason = "temporarily unavailable".to_string();
            return atomic_reject(ctx, request, reason, String::new(), String::new());
        }
    };
    let quote_id = match request.quote_id_prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}-{id}"),
        _ => id.to_string(),
    };
    let side = if direction == 1 {
        atomic_quote::QuoteSide::Buy
    } else {
        atomic_quote::QuoteSide::Sell
    };
    if let Err(e) = ctx
        .state
        .register_indicative(
            &quote_id,
            &request.market_id,
            side,
            &priced,
            request.settlement_fee.clone(),
            user_party,
        )
        .await
    {
        return atomic_reject(ctx, request, e, String::new(), String::new());
    }

    let quoted_at = clock::now_secs_i64();
    LpMessage::Quote(AtomicRfqQuote {
        rfq_id: request.rfq_id.clone(),
        quote_id,
        market_id: request.market_id.clone(),
        direction: request.direction.clone(),
        price: priced.price_str.clone(),
        quantity: priced.quantity_str.clone(),
        quote_quantity: priced.quote_quantity_str.clone(),
        valid_for_secs: priced.valid_for_secs,
        valid_until: Some(prost_types::Timestamp {
            seconds: quoted_at.saturating_add(i64::from(priced.valid_for_secs)),
            nanos: 0,
        }),
        lp_party_id: ctx.party_id.clone(),
        lp_name: ctx.lp_name.clone(),
        quoted_at: Some(prost_types::Timestamp {
            seconds: quoted_at,
            nanos: 0,
        }),
        // echo of the authoritative fee — the relay drops the quote on any
        // mismatch (design §14 D19)
        settlement_fee: request.settlement_fee.clone(),
    })
}

/// Answer a confirm in its own task, so a slow confirm never blocks the stream.
/// A confirm is never cut short: it commits funds before it awaits.
fn spawn_confirm(
    ctx: &AtomicCtx,
    req: RfqConfirmRequest,
    outbound: mpsc::Sender<AtomicLpToServer>,
    seq: Arc<AtomicU64>,
    send_failed: Arc<Notify>,
    send_within: Duration,
) {
    let state = Arc::clone(&ctx.state);
    let party_id = ctx.party_id.clone();
    let rfq_id = req.rfq_id.clone();
    let quote_id = req.quote_id.clone();
    supervise::try_spawn("atomic confirm", async move {
        let handler = supervise::try_spawn("atomic confirm handler", async move {
            state.handle_confirm(req).await
        });
        let outcome = match handler {
            Some(task) => task.await.map_err(|e| e.to_string()),
            None => Err("not started".to_string()),
        };
        let message = match outcome {
            Ok(Ok(envelope)) => LpMessage::Envelope(envelope),
            Ok(Err(reject)) => LpMessage::ConfirmReject(reject),
            Err(e) => {
                error!("Atomic confirm {} failed: {}", quote_id, e);
                LpMessage::ConfirmReject(RfqConfirmReject {
                    rfq_id,
                    quote_id,
                    lp_party_id: party_id,
                    reason: RfqConfirmRejectReason::InternalError as i32,
                    reason_detail: Some("confirm failed".to_string()),
                    rejected_at: Some(now_ts()),
                })
            }
        };
        if let Err(e) = send_bounded(&outbound, wrap(next_seq(&seq), message), send_within).await {
            error!("Failed to send atomic confirm response, stream may be closed ({e})");
            send_failed.notify_one();
        }
    });
}

/// Start an expiry sweep unless the previous one is still running.
/// A sweep releases commitments, so it is never cut short.
fn spawn_sweep(state: &Arc<RfqV2State>, running: &Arc<AtomicBool>) {
    let Some(flag) = InFlight::claim(running) else {
        debug!("V2 sweep still running; skipping this tick");
        return;
    };
    let state = Arc::clone(state);
    supervise::try_spawn("atomic sweep", async move {
        let _flag = flag;
        state.sweep(std::time::Instant::now()).await;
    });
}

/// Whether a settle backlog of `queued` should be logged now.
fn backlog_warning_due(
    queued: usize,
    last: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    queued > SETTLE_BACKLOG_WARN
        && last.is_none_or(|t| now.saturating_duration_since(t) >= SETTLE_BACKLOG_WARN_EVERY)
}

/// Apply watcher settle observations in order, each in its own task, so a
/// failing one is logged and the next one still runs.
async fn consume_settles(
    state: Arc<RfqV2State>,
    settle_rx: &mut mpsc::UnboundedReceiver<SettleObserved>,
    shutdown: Shutdown,
) {
    let mut backlog_warned_at: Option<std::time::Instant> = None;
    loop {
        let obs = tokio::select! {
            biased;
            _ = shutdown.wait() => return,
            obs = settle_rx.recv() => obs,
        };
        let Some(obs) = obs else {
            return; // watcher gone
        };
        let queued = settle_rx.len();
        let now = std::time::Instant::now();
        if backlog_warning_due(queued, backlog_warned_at, now) {
            backlog_warned_at = Some(now);
            warn!("V2 settle observations backlog: {} queued", queued);
        }
        let quote_id = obs.quote_id.clone();
        let state = Arc::clone(&state);
        let task = supervise::try_spawn("settle observation", async move {
            state.handle_settle_observed(&obs.quote_id, &obs.update_id).await;
        });
        if let Some(task) = task {
            if let Err(e) = task.await {
                error!("Settle observation for quote {} failed: {}", quote_id, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rfq_v2::tests as v2;
    use rust_decimal::Decimal;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context as TaskContext, Poll};
    use tonic::codegen::http;

    fn fast() -> Timing {
        Timing {
            open: Duration::from_millis(300),
            send: Duration::from_millis(100),
            heartbeat: Duration::from_secs(30),
            idle: Duration::from_secs(90),
            handler: Duration::from_millis(100),
            reconnect: Duration::from_millis(50),
            sweep: Duration::from_secs(10),
        }
    }

    fn lp_config() -> BaseConfig {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.liquidity_provider = Some(serde_json::from_str(r#"{"name":"LP test"}"#).unwrap());
        config.markets = vec![serde_json::from_str(
            r#"{"market_id":"EDELx-USDCx","rfq":{"min_quantity":"1","max_quantity":"100000"}}"#,
        )
        .unwrap()];
        config
    }

    fn handler() -> Arc<RfqHandler> {
        Arc::new(RfqHandler::new(&lp_config()).unwrap())
    }

    type Inbound<T> = ReceiverStream<Result<T, tonic::Status>>;

    fn inbound<T>() -> (mpsc::Sender<Result<T, tonic::Status>>, Inbound<T>) {
        let (tx, rx) = mpsc::channel(16);
        (tx, ReceiverStream::new(rx))
    }

    // ---- helpers ----

    #[test]
    fn sequence_numbers_wrap_instead_of_overflowing() {
        let seq = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_seq(&seq), u64::MAX);
        assert_eq!(next_seq(&seq), 0);
        assert_eq!(next_seq(&seq), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sequence_numbers_stay_unique_across_tasks() {
        let seq = Arc::new(AtomicU64::new(0));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let seq = Arc::clone(&seq);
                tokio::spawn(async move { (0..1000).map(|_| next_seq(&seq)).collect::<Vec<_>>() })
            })
            .collect();
        let mut all = Vec::new();
        for t in tasks {
            all.extend(t.await.unwrap());
        }
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 8000);
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_into_a_queue_nobody_drains_gives_up() {
        let (tx, _rx) = mpsc::channel::<u8>(1);
        tx.send(0).await.unwrap();
        let within = Duration::from_secs(10);
        assert_eq!(send_bounded(&tx, 1, within).await, Err(SendFailure::Stalled(within)));
        drop(_rx);
        assert_eq!(send_bounded(&tx, 1, within).await, Err(SendFailure::Closed));
    }

    #[tokio::test]
    async fn heartbeats_never_wait_on_a_full_queue() {
        let (tx, rx) = mpsc::channel::<u8>(1);
        assert_eq!(send_heartbeat(&tx, 0), Ok(()));
        assert_eq!(send_heartbeat(&tx, 1), Err(SendFailure::Full));
        drop(rx);
        assert_eq!(send_heartbeat(&tx, 2), Err(SendFailure::Closed));
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_is_bounded_and_yields_to_shutdown() {
        let shutdown = Shutdown::new();
        let open = std::future::pending::<()>();
        assert!(matches!(open_bounded(&shutdown, RPC_TIMEOUT, open).await, Some(Err(_))));
        shutdown.signal();
        let open = std::future::pending::<()>();
        assert!(open_bounded(&shutdown, Duration::MAX, open).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_watchdog_stays_disarmed_until_a_server_heartbeat() {
        let idle = Duration::from_secs(90);
        let mut wd = IdleWatchdog::new(idle);
        assert!(tokio::time::timeout(idle * 10, wd.expired()).await.is_err());
        wd.on_message(false);
        assert!(tokio::time::timeout(idle * 10, wd.expired()).await.is_err());
        wd.on_message(true);
        let armed_at = tokio::time::Instant::now();
        tokio::time::timeout(idle * 2, wd.expired()).await.unwrap();
        assert!(armed_at.elapsed() >= idle);
    }

    #[tokio::test(start_paused = true)]
    async fn any_message_restarts_the_idle_window() {
        let idle = Duration::from_secs(90);
        let mut wd = IdleWatchdog::new(idle);
        wd.on_message(true);
        tokio::time::sleep(idle / 2).await;
        wd.on_message(false);
        let reset_at = tokio::time::Instant::now();
        wd.expired().await;
        assert!(reset_at.elapsed() >= idle);
    }

    #[test]
    fn the_in_flight_flag_admits_one_holder_and_clears_on_drop() {
        let flag = Arc::new(AtomicBool::new(false));
        let first = InFlight::claim(&flag).unwrap();
        assert!(InFlight::claim(&flag).is_none());
        drop(first);
        assert!(InFlight::claim(&flag).is_some());
        let unwound = std::panic::catch_unwind(|| {
            let _held = InFlight::claim(&flag).unwrap();
            panic!("sweep failed");
        });
        assert!(unwound.is_err());
        assert!(!flag.load(Ordering::Acquire), "an unwind clears the flag");
    }

    #[test]
    fn a_settle_backlog_is_logged_at_most_once_a_minute() {
        let now = std::time::Instant::now();
        assert!(!backlog_warning_due(SETTLE_BACKLOG_WARN, None, now));
        assert!(backlog_warning_due(SETTLE_BACKLOG_WARN + 1, None, now));
        assert!(!backlog_warning_due(5000, Some(now), now + Duration::from_secs(59)));
        assert!(backlog_warning_due(5000, Some(now), now + SETTLE_BACKLOG_WARN_EVERY));
    }

    // ---- V1 session ----

    fn server_heartbeat_v1() -> Result<ServerToCantonMessage, tonic::Status> {
        Ok(ServerToCantonMessage {
            message: Some(ServerMessage::Heartbeat(Heartbeat::default())),
            ..Default::default()
        })
    }

    fn rfq_v1(market: &str) -> Result<ServerToCantonMessage, tonic::Status> {
        Ok(ServerToCantonMessage {
            message: Some(ServerMessage::RfqRequest(orderbook_proto::settlement::RfqRequest {
                rfq_id: "rfq-1".into(),
                market_id: market.into(),
                direction: 1,
                quantity: "100".into(),
                ..Default::default()
            })),
            ..Default::default()
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_v1_stream_is_kept_until_the_server_has_sent_a_heartbeat() {
        let t = fast();
        let shutdown = Shutdown::new();
        let rfq = handler();
        let (out_tx, _out_rx) = mpsc::channel(64);

        // No heartbeat yet: a server without heartbeats is never cut off
        let (_in_tx, stream) = inbound();
        let session = settlement_session(&rfq, stream, &out_tx, &shutdown, t);
        assert!(tokio::time::timeout(t.idle * 4, session).await.is_err());

        let (in_tx, stream) = inbound();
        in_tx.send(server_heartbeat_v1()).await.unwrap();
        let started = tokio::time::Instant::now();
        let end = tokio::time::timeout(t.idle * 2, settlement_session(&rfq, stream, &out_tx, &shutdown, t))
            .await
            .expect("silence after a server heartbeat ends the session");
        assert_eq!(end, SessionEnd::Idle);
        assert!(started.elapsed() >= t.idle);
    }

    // Any inbound message, not only a server heartbeat, restarts the idle window
    #[tokio::test(start_paused = true)]
    async fn any_message_keeps_an_armed_v1_session_alive() {
        let t = fast();
        let shutdown = Shutdown::new();
        let rfq = handler();
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let (in_tx, stream) = inbound();
        in_tx.send(server_heartbeat_v1()).await.unwrap();
        let session = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { settlement_session(&rfq, stream, &out_tx, &shutdown, t).await }
        });
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let ack = ServerToCantonMessage {
                message: Some(ServerMessage::HandshakeAck(Default::default())),
                ..Default::default()
            };
            in_tx.send(Ok(ack)).await.unwrap();
            while out_rx.try_recv().is_ok() {}
        }
        assert!(!session.is_finished(), "four minutes of messages, each within the idle window");
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    #[tokio::test]
    async fn a_stalled_v1_response_send_ends_the_session() {
        let t = fast();
        let shutdown = Shutdown::new();
        let rfq = handler();
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx.send(CantonToServerMessage::default()).await.unwrap();
        let (in_tx, stream) = inbound();
        in_tx.send(rfq_v1("NOPE-USDCx")).await.unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), settlement_session(&rfq, stream, &out_tx, &shutdown, t))
            .await
            .expect("a stalled send ends the session");
        assert_eq!(end, SessionEnd::SendFailed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_on_heartbeat_ends_the_v1_session() {
        let t = fast();
        let shutdown = Shutdown::new();
        let rfq = handler();
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx.send(CantonToServerMessage::default()).await.unwrap();
        let (_in_tx, stream) = inbound();
        let end = tokio::time::timeout(t.heartbeat * 2, settlement_session(&rfq, stream, &out_tx, &shutdown, t))
            .await
            .unwrap();
        assert_eq!(end, SessionEnd::SendFailed);
    }

    #[tokio::test]
    async fn v1_pricing_that_cannot_finish_is_rejected_in_time() {
        let t = fast();
        let shutdown = Shutdown::new();
        let rfq = handler();
        let mids = rfq.mid_prices();
        let _held = mids.write().await;
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let (in_tx, stream) = inbound();
        in_tx.send(rfq_v1("EDELx-USDCx")).await.unwrap();
        let session = tokio::spawn({
            let rfq = Arc::clone(&rfq);
            let shutdown = shutdown.clone();
            async move { settlement_session(&rfq, stream, &out_tx, &shutdown, t).await }
        });
        let sent = tokio::time::timeout(Duration::from_secs(5), out_rx.recv())
            .await
            .expect("the RFQ is answered")
            .unwrap();
        let Some(CantonMessage::RfqReject(reject)) = sent.message else { panic!("expected a reject: {sent:?}") };
        assert_eq!(reject.reason, orderbook_proto::settlement::RfqRejectionReason::TemporarilyUnavailable as i32);
        assert_eq!(reject.reason_detail.as_deref(), Some("Temporarily unavailable"));
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    // ---- V2 session ----

    fn ctx(state: RfqV2State) -> AtomicCtx {
        AtomicCtx {
            party_id: "lp-party::1220test".into(),
            lp_name: "LP Test".into(),
            rfq_handler: handler(),
            state: Arc::new(state),
            sweep_running: Arc::new(AtomicBool::new(false)),
        }
    }

    fn confirm(quote_id: &str) -> Result<AtomicServerToLp, tonic::Status> {
        let mut req = v2::confirm_req(quote_id);
        req.respond_by = Some(prost_types::Timestamp { seconds: clock::now_secs_i64() + 6, nanos: 0 });
        Ok(AtomicServerToLp {
            message: Some(AtomicServerMessage::ConfirmRequest(req)),
            ..Default::default()
        })
    }

    fn rfq_v2(market: &str) -> Result<AtomicServerToLp, tonic::Status> {
        Ok(AtomicServerToLp {
            message: Some(AtomicServerMessage::RfqRequest(AtomicRfqRequest {
                rfq_id: "rfq-1".into(),
                market_id: market.into(),
                direction: "buy".into(),
                quantity: "100".into(),
                ..Default::default()
            })),
            ..Default::default()
        })
    }

    async fn next_message(out_rx: &mut mpsc::Receiver<AtomicLpToServer>) -> LpMessage {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), out_rx.recv())
                .await
                .expect("an answer within the bound")
                .expect("the session keeps the queue open");
            match msg.message {
                Some(LpMessage::Heartbeat(_)) | None => continue,
                Some(m) => return m,
            }
        }
    }

    #[tokio::test]
    async fn a_duplicate_confirm_is_answered_in_progress_while_the_first_runs() {
        let lm = v2::lm_with_usdcx(10_000).await;
        let state = v2::venue_state(lm.clone(), Default::default(), &[], crate::holdings_cache::HoldingsCache::new(false));
        state
            .register_indicative("q1", v2::MARKET, atomic_quote::QuoteSide::Sell, &v2::priced_sell(500, 90), None, None)
            .await
            .unwrap();
        // No holdings yet and a split in flight: the first confirm waits for rungs
        v2::mark_split_in_flight(&state, v2::USDCX_KEY);
        let ctx = Arc::new(ctx(state));
        let shutdown = Shutdown::new();
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let (in_tx, stream) = inbound();
        let session = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let shutdown = shutdown.clone();
            async move { atomic_session(&ctx, stream, &out_tx, &shutdown, fast()).await }
        });

        in_tx.send(confirm("q1")).await.unwrap();
        in_tx.send(confirm("q1")).await.unwrap();
        let LpMessage::ConfirmReject(dup) = next_message(&mut out_rx).await else { panic!("the duplicate is answered first") };
        assert_eq!(dup.quote_id, "q1");
        assert_eq!(dup.reason_detail.as_deref(), Some("confirm already in progress"));

        // The first confirm is still running and issues its envelope once rungs land
        v2::add_usdcx_holding(&ctx.state).await;
        let LpMessage::Envelope(envelope) = next_message(&mut out_rx).await else { panic!("the first confirm issues") };
        assert_eq!(envelope.quote_id, "q1");
        assert_eq!(ctx.state.pending_kind("q1"), Some("Confirmed"));
        assert_eq!(lm.available("USDCx").await, Decimal::from(9_500));

        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    #[tokio::test]
    async fn a_confirm_response_that_cannot_be_sent_ends_the_session() {
        let state = v2::state_with(v2::lm_with_usdcx(1000).await);
        let ctx = ctx(state);
        let shutdown = Shutdown::new();
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx.send(AtomicLpToServer::default()).await.unwrap();
        let (in_tx, stream) = inbound();
        in_tx.send(confirm("unknown")).await.unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), atomic_session(&ctx, stream, &out_tx, &shutdown, fast()))
            .await
            .expect("the failed confirm send ends the session");
        assert_eq!(end, SessionEnd::SendFailed);
    }

    // A confirm that panics is still answered, and the session keeps serving
    #[tokio::test]
    async fn a_panicking_confirm_is_rejected_and_the_session_keeps_serving() {
        let ctx = Arc::new(ctx(v2::state_with(v2::lm_with_usdcx(1000).await)));
        let shutdown = Shutdown::new();
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let (in_tx, stream) = inbound();
        let session = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let shutdown = shutdown.clone();
            async move { atomic_session(&ctx, stream, &out_tx, &shutdown, fast()).await }
        });

        in_tx.send(confirm(v2::PANIC_QUOTE_ID)).await.unwrap();
        let LpMessage::ConfirmReject(reject) = next_message(&mut out_rx).await else {
            panic!("a panicked confirm is rejected")
        };
        assert_eq!(reject.quote_id, v2::PANIC_QUOTE_ID);
        assert_eq!(reject.reason, RfqConfirmRejectReason::InternalError as i32);
        assert_eq!(reject.reason_detail.as_deref(), Some("confirm failed"));

        in_tx.send(confirm("unknown")).await.unwrap();
        let LpMessage::ConfirmReject(next) = next_message(&mut out_rx).await else {
            panic!("the next confirm is answered")
        };
        assert_eq!(next.quote_id, "unknown");
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    #[tokio::test]
    async fn a_stalled_v2_response_send_ends_the_session() {
        let state = v2::state_with(v2::lm_with_usdcx(1000).await);
        let ctx = ctx(state);
        let shutdown = Shutdown::new();
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx.send(AtomicLpToServer::default()).await.unwrap();
        let (in_tx, stream) = inbound();
        in_tx.send(rfq_v2("NOPE-USDCx")).await.unwrap();
        let end = tokio::time::timeout(Duration::from_secs(5), atomic_session(&ctx, stream, &out_tx, &shutdown, fast()))
            .await
            .expect("a stalled send ends the session");
        assert_eq!(end, SessionEnd::SendFailed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_v2_stream_is_kept_until_the_server_has_sent_a_heartbeat() {
        let t = fast();
        let ctx = ctx(v2::state_with(v2::lm_with_usdcx(1000).await));
        let shutdown = Shutdown::new();
        let (out_tx, _out_rx) = mpsc::channel(64);

        let (_in_tx, stream) = inbound();
        assert!(tokio::time::timeout(t.idle * 4, atomic_session(&ctx, stream, &out_tx, &shutdown, t)).await.is_err());

        let (in_tx, stream) = inbound();
        in_tx
            .send(Ok(AtomicServerToLp {
                message: Some(AtomicServerMessage::Heartbeat(AtomicHeartbeat::default())),
                ..Default::default()
            }))
            .await
            .unwrap();
        let started = tokio::time::Instant::now();
        let end = tokio::time::timeout(t.idle * 2, atomic_session(&ctx, stream, &out_tx, &shutdown, t))
            .await
            .expect("silence after a server heartbeat ends the session");
        assert_eq!(end, SessionEnd::Idle);
        assert!(started.elapsed() >= t.idle);
    }

    // Any inbound message, not only a server heartbeat, restarts the idle window
    #[tokio::test(start_paused = true)]
    async fn any_message_keeps_an_armed_v2_session_alive() {
        let t = fast();
        let ctx = Arc::new(ctx(v2::state_with(v2::lm_with_usdcx(1000).await)));
        let shutdown = Shutdown::new();
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let (in_tx, stream) = inbound();
        let heartbeat = AtomicServerToLp {
            message: Some(AtomicServerMessage::Heartbeat(AtomicHeartbeat::default())),
            ..Default::default()
        };
        in_tx.send(Ok(heartbeat)).await.unwrap();
        let session = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let shutdown = shutdown.clone();
            async move { atomic_session(&ctx, stream, &out_tx, &shutdown, t).await }
        });
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let ack = AtomicServerToLp {
                message: Some(AtomicServerMessage::HandshakeAck(Default::default())),
                ..Default::default()
            };
            in_tx.send(Ok(ack)).await.unwrap();
            while out_rx.try_recv().is_ok() {}
        }
        assert!(!session.is_finished(), "four minutes of messages, each within the idle window");
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_queue_on_heartbeat_ends_the_v2_session() {
        let t = fast();
        let ctx = ctx(v2::state_with(v2::lm_with_usdcx(1000).await));
        let shutdown = Shutdown::new();
        let (out_tx, _out_rx) = mpsc::channel(1);
        out_tx.send(AtomicLpToServer::default()).await.unwrap();
        let (_in_tx, stream) = inbound();
        let end = tokio::time::timeout(t.heartbeat * 2, atomic_session(&ctx, stream, &out_tx, &shutdown, t))
            .await
            .unwrap();
        assert_eq!(end, SessionEnd::SendFailed);
    }

    #[tokio::test]
    async fn v2_pricing_that_cannot_finish_is_rejected_in_time() {
        let state = v2::quotable(v2::signable_state(v2::lm_with_usdcx(1000).await, Default::default(), &[]).await);
        let ctx = ctx(state);
        let mids = ctx.rfq_handler.mid_prices();
        let _held = mids.write().await;
        let request = AtomicRfqRequest {
            rfq_id: "rfq-1".into(),
            market_id: v2::MARKET.into(),
            direction: "buy".into(),
            quantity: "100".into(),
            ..Default::default()
        };
        let answer = tokio::time::timeout(Duration::from_secs(5), answer_atomic_rfq(&ctx, &request, fast().handler))
            .await
            .expect("pricing is bounded");
        let LpMessage::Reject(reject) = answer else { panic!("expected a reject") };
        assert_eq!(reject.reason, "temporarily unavailable");
    }

    // The session's own sweep tick is skipped while an earlier sweep still runs
    #[tokio::test(start_paused = true)]
    async fn the_session_sweep_waits_for_a_running_sweep() {
        let state = v2::state_with(v2::lm_with_usdcx(1000).await);
        // Expired on arrival, so any sweep that runs marks it Expired
        state
            .register_indicative("q1", v2::MARKET, atomic_quote::QuoteSide::Sell, &v2::priced_sell(500, 0), None, None)
            .await
            .unwrap();
        let ctx = Arc::new(ctx(state));
        let held = InFlight::claim(&ctx.sweep_running).unwrap();
        let shutdown = Shutdown::new();
        let (out_tx, _out_rx) = mpsc::channel(64);
        let (_in_tx, stream) = inbound();
        let session = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let shutdown = shutdown.clone();
            async move { atomic_session(&ctx, stream, &out_tx, &shutdown, fast()).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(ctx.state.pending_kind("q1"), Some("Indicative"), "the first tick skipped the sweep");

        drop(held);
        tokio::time::sleep(fast().sweep).await;
        assert_eq!(ctx.state.pending_kind("q1"), Some("Expired"), "the next tick ran it");
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    /// A confirmed quote with one holding, restored with `valid_for` left on its window.
    fn confirmed(quote_id: &str, valid_for: Duration) -> agent_logic::state::SavedPendingV2 {
        agent_logic::state::SavedPendingV2 {
            quote_id: quote_id.to_string(),
            market_id: v2::MARKET.to_string(),
            holding_cids: vec![format!("00holding-{quote_id}")],
            ticket_id: String::new(),
            valid_until_micros: clock::now_micros_i64() + valid_for.as_micros() as i64,
        }
    }

    // A sweep stalled on a release neither holds up the session nor is cut short
    #[tokio::test(start_paused = true)]
    async fn a_stalled_sweep_runs_beside_the_session_until_it_ends() {
        let lm = v2::lm_with_usdcx(1000).await;
        let cache = crate::holdings_cache::HoldingsCache::new(false);
        let no_grace = agent_logic::config::RfqV2Config { settle_grace_secs: 0, ..Default::default() };
        let state = v2::venue_state(lm.clone(), no_grace, &[], Arc::clone(&cache));
        let window = Duration::from_millis(300);
        state.restore_pending(vec![confirmed("q1", window), confirmed("q2", window)]).await;
        for quote in ["q1", "q2"] {
            lm.try_commit(&format!("rfqv2:{quote}"), "USDCx", Decimal::from(300), Decimal::ZERO).await.unwrap();
        }
        assert_eq!(lm.available("USDCx").await, Decimal::from(400));
        // Expiry is read from the wall clock, which the paused clock does not move
        std::thread::sleep(window + Duration::from_millis(100));

        let blocked = cache.block_releases_for_tests().await;
        let ctx = Arc::new(ctx(state));
        let shutdown = Shutdown::new();
        let (out_tx, mut out_rx) = mpsc::channel(256);
        let (_in_tx, stream) = inbound();
        let t = fast();
        let session = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let shutdown = shutdown.clone();
            async move { atomic_session(&ctx, stream, &out_tx, &shutdown, t).await }
        });

        let beat = tokio::time::timeout(t.heartbeat * 2, out_rx.recv()).await.expect("the session keeps beating");
        assert!(matches!(beat.and_then(|m| m.message), Some(LpMessage::Heartbeat(_))));
        assert!(ctx.sweep_running.load(Ordering::Acquire), "the first sweep is still running");
        assert_eq!(lm.available("USDCx").await, Decimal::from(700), "it stalled between the two releases");

        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(blocked);
        let full = Decimal::from(1000);
        let released = tokio::time::timeout(Duration::from_secs(5), async {
            while lm.available("USDCx").await != full {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        released.await.expect("the second commitment is released once the holdings are");
        assert_eq!(ctx.state.pending_kind("q2"), Some("Expired"));
        shutdown.signal();
        assert_eq!(session.await.unwrap(), SessionEnd::Shutdown);
    }

    #[tokio::test]
    async fn a_running_sweep_is_not_started_twice() {
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        // Expired on arrival, so any sweep that runs marks it Expired
        state
            .register_indicative("q1", v2::MARKET, atomic_quote::QuoteSide::Sell, &v2::priced_sell(500, 0), None, None)
            .await
            .unwrap();
        let running = Arc::new(AtomicBool::new(false));
        let held = InFlight::claim(&running).unwrap();
        spawn_sweep(&state, &running);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(state.pending_kind("q1"), Some("Indicative"), "skipped while one runs");

        drop(held);
        spawn_sweep(&state, &running);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(state.pending_kind("q1"), Some("Expired"));
        assert!(!running.load(Ordering::Acquire), "the sweep task clears its flag");
    }

    /// A restored confirmed quote that stays valid for a minute.
    fn saved_pending(quote_id: &str) -> agent_logic::state::SavedPendingV2 {
        agent_logic::state::SavedPendingV2 {
            quote_id: quote_id.to_string(),
            market_id: v2::MARKET.to_string(),
            holding_cids: Vec::new(),
            ticket_id: String::new(),
            valid_until_micros: clock::now_micros_i64() + 60_000_000,
        }
    }

    #[tokio::test]
    async fn settle_observations_are_applied_until_the_watcher_goes_away() {
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        state.restore_pending(vec![saved_pending("q0"), saved_pending("q1")]).await;
        let (tx, rx) = mpsc::unbounded_channel();
        for q in ["q0", "unknown", "q1"] {
            tx.send(SettleObserved { quote_id: q.into(), update_id: "u".into() }).unwrap();
        }
        drop(tx);
        let mut rx = rx;
        tokio::time::timeout(Duration::from_secs(5), consume_settles(Arc::clone(&state), &mut rx, Shutdown::new()))
            .await
            .expect("the consumer ends once the watcher is gone");
        assert_eq!(state.pending_kind("q0"), Some("Settled"));
        assert_eq!(state.pending_kind("q1"), Some("Settled"));
    }

    // An observation that panics is logged, and the next one still runs
    #[tokio::test]
    async fn a_panicking_settle_observation_does_not_stop_the_consumer() {
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        state.restore_pending(vec![saved_pending("q1")]).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        for q in [v2::PANIC_QUOTE_ID, "q1"] {
            tx.send(SettleObserved { quote_id: q.into(), update_id: "u".into() }).unwrap();
        }
        drop(tx);
        let consumer = tokio::spawn({
            let state = Arc::clone(&state);
            async move { consume_settles(state, &mut rx, Shutdown::new()).await }
        });
        tokio::time::timeout(Duration::from_secs(5), consumer)
            .await
            .expect("the consumer ends once the watcher is gone")
            .expect("a panicking observation does not take the consumer down");
        assert_eq!(state.pending_kind("q1"), Some("Settled"));
    }

    async fn wait_until(what: &str, cond: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    // The receiver outlives a failed run, so a restarted consumer keeps draining it
    #[tokio::test]
    async fn a_restarted_settle_consumer_keeps_draining_the_same_receiver() {
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        state.restore_pending(vec![saved_pending("q0"), saved_pending("q1")]).await;
        let (tx, rx) = mpsc::unbounded_channel();
        let shutdown = Shutdown::new();
        let mut start = settle_consumer(Arc::clone(&state), rx, shutdown.clone());

        let first = tokio::spawn(start());
        tx.send(SettleObserved { quote_id: "q0".into(), update_id: "u".into() }).unwrap();
        wait_until("q0 settled", || state.pending_kind("q0") == Some("Settled")).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        tx.send(SettleObserved { quote_id: "q1".into(), update_id: "u".into() }).unwrap();
        let second = tokio::spawn(start());
        wait_until("q1 settled", || state.pending_kind("q1") == Some("Settled")).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), second).await.unwrap().unwrap();
    }

    // A stream task that fails is restarted by its supervisor instead of ending
    #[tokio::test]
    async fn a_failing_v1_stream_is_kept_alive_by_its_supervisor() {
        let mut config = lp_config();
        config.liquidity_provider = None;
        let shutdown = Shutdown::new();
        let handle = spawn_settlement_stream(config, handler(), shutdown.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!handle.is_finished(), "the supervisor waits to restart it");
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), handle).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn both_v2_tasks_run_until_shutdown() {
        let (_listener, url) = stalled_tls_url();
        let mut config = lp_config();
        config.orderbook_grpc_url = url;
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        let (_settle_tx, settle_rx) = mpsc::unbounded_channel();
        let shutdown = Shutdown::new();
        let (consumer, stream) =
            spawn_atomic_stream(config, handler(), state, settle_rx, "test".into(), shutdown.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!consumer.is_finished() && !stream.is_finished());
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), consumer).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), stream).await.unwrap().unwrap();
    }

    // ---- full stream loops against a server that never answers the open ----

    struct V1;
    struct V2;

    /// Accepts every stream call and never answers it.
    struct Mute<N> {
        calls: Arc<AtomicUsize>,
        _name: std::marker::PhantomData<N>,
    }

    impl<N> Clone for Mute<N> {
        fn clone(&self) -> Self {
            Self { calls: Arc::clone(&self.calls), _name: std::marker::PhantomData }
        }
    }

    impl tonic::server::NamedService for Mute<V1> {
        const NAME: &'static str = "silvana.settlement.v1.SettlementService";
    }

    impl tonic::server::NamedService for Mute<V2> {
        const NAME: &'static str = "silvana.rfqv2.v1.AtomicRfqService";
    }

    impl<N, B> tonic::codegen::Service<http::Request<B>> for Mute<N> {
        type Response = http::Response<tonic::body::Body>;
        type Error = std::convert::Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<B>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    async fn mute_server<N: Send + Sync + 'static>() -> (String, Arc<AtomicUsize>)
    where
        Mute<N>: tonic::server::NamedService,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let svc = Mute::<N> { calls: Arc::clone(&calls), _name: std::marker::PhantomData };
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        (url, calls)
    }

    async fn wait_for_calls(calls: &AtomicUsize, n: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while calls.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the stream reopens after an unanswered open");
    }

    /// A TLS URL whose peer never answers the handshake.
    fn stalled_tls_url() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        (listener, url)
    }

    #[tokio::test]
    async fn shutdown_ends_a_v1_stream_that_is_still_connecting() {
        let (_listener, url) = stalled_tls_url();
        let mut config = lp_config();
        config.orderbook_grpc_url = url;
        let shutdown = Shutdown::new();
        let task = tokio::spawn(settlement_stream(config, handler(), shutdown.clone(), fast()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(2), task).await.expect("shutdown wins over a connect").unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_ends_a_v2_stream_that_is_still_connecting() {
        let (_listener, url) = stalled_tls_url();
        let mut config = lp_config();
        config.orderbook_grpc_url = url;
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        let shutdown = Shutdown::new();
        let task = tokio::spawn(atomic_stream(config, handler(), state, "test".into(), shutdown.clone(), fast()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(2), task).await.expect("shutdown wins over a connect").unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_unanswered_v1_open_is_retried_and_shutdown_ends_it() {
        let (url, calls) = mute_server::<V1>().await;
        let mut config = lp_config();
        config.orderbook_grpc_url = url;
        let shutdown = Shutdown::new();
        let task = tokio::spawn(settlement_stream(config, handler(), shutdown.clone(), fast()));
        wait_for_calls(&calls, 3).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_unanswered_v2_open_is_retried_and_shutdown_ends_it() {
        let (url, calls) = mute_server::<V2>().await;
        let mut config = lp_config();
        config.orderbook_grpc_url = url;
        let state = Arc::new(v2::state_with(v2::lm_with_usdcx(1000).await));
        let shutdown = Shutdown::new();
        let task = tokio::spawn(atomic_stream(config, handler(), state, "test".into(), shutdown.clone(), fast()));
        wait_for_calls(&calls, 3).await;
        shutdown.signal();
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
    }
}
