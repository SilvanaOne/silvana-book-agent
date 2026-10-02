//! gRPC client for Orderbook RPC service
//!
//! This module provides a client for external parties to record transactions
//! and settlement events via RPC (instead of direct database access).
//!
//! Authentication uses gRPC metadata headers with self-describing JWTs (RFC 8037).
//!
//! The underlying gRPC channel is cached globally for connection reuse across
//! multiple workers and operations.

use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::OnceCell;
use tonic::transport::Channel;
use tonic::Request;
use tracing::{info, warn};

use crate::transport::{self, ChannelOpts};
use crate::{clock, sync};

/// Client deadline for each RPC; also the channel's request timeout.
const RPC_DEADLINE: Duration = Duration::from_secs(120);

const MAX_DECODING_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

const CHANNEL_OPTS: ChannelOpts = ChannelOpts {
    connect: Duration::from_secs(10),
    tls: transport::TLS_HANDSHAKE_TIMEOUT,
    request: Some(RPC_DEADLINE),
    keepalive: true,
};

/// Cached channels, one per URL; a failed connect leaves its cell empty for the next caller.
static CHANNELS: Mutex<BTreeMap<String, Arc<OnceCell<Channel>>>> = Mutex::new(BTreeMap::new());

fn channel_cell(url: &str) -> Arc<OnceCell<Channel>> {
    Arc::clone(sync::lock(&CHANNELS).entry(url.to_string()).or_default())
}

/// The cell's channel, connecting if it is empty; a caller queued behind
/// another connect also gives up after `budget`.
async fn cached_channel<F, Fut>(cell: &OnceCell<Channel>, url: &str, budget: Duration, connect: F) -> Result<Channel>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Channel>>,
{
    let init = cell.get_or_try_init(|| async {
        info!("Connecting to Orderbook RPC at {}", url);
        connect()
            .await
            .map_err(|e| anyhow!("Failed to connect to {}: {}", url, e.root_cause()))
    });
    match tokio::time::timeout(budget, init).await {
        Ok(channel) => channel.cloned(),
        Err(_) => Err(anyhow!(
            "Failed to connect to {url}: (Unavailable) connect to {url} timed out after {budget:?}"
        )),
    }
}

use orderbook_proto::{
    // Settlement service
    settlement::settlement_service_client::SettlementServiceClient,
    RecordSettlementEventRequest,
    RecordTransactionRequest,
    SaveDisclosedContractRequest,
    GetSettlementProposalByIdRequest, SettlementProposalMessage,
    // Preconfirmation and settlement status
    SubmitPreconfirmationRequest, PreconfirmationDecision, PreconfirmationResponse,
    GetSettlementStatusRequest, GetSettlementStatusResponse,
    DisclosedContractMessage,
    settlement::CancelSettlementRequest,
    settlement::{ErrorEvent, ReportErrorsRequest},
};

/// Authentication interceptor for gRPC requests
#[derive(Clone)]
struct AuthInterceptor {
    token: Arc<RwLock<String>>,
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, tonic::Status> {
        let token = sync::read(&self.token).clone();
        if !token.is_empty() {
            request.metadata_mut().insert(
                "authorization",
                format!("Bearer {}", token)
                    .parse()
                    .map_err(|_| tonic::Status::internal("Failed to parse JWT token"))?,
            );
        }
        Ok(request)
    }
}

/// gRPC client for Orderbook RPC service
pub struct OrderbookRpcClient {
    settlement_client: SettlementServiceClient<tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>>,
    token: Arc<RwLock<String>>,
    deadline: Duration,
}

impl OrderbookRpcClient {
    /// Get or create the cached gRPC channel for `url`
    async fn get_or_create_channel(url: &str, opts: ChannelOpts) -> Result<Channel> {
        let connect = || transport::connect_channel(url, opts);
        cached_channel(&channel_cell(url), url, opts.connect_budget(), connect).await
    }

    /// Connect to the Orderbook RPC service
    ///
    /// The underlying gRPC channel is cached and reused across all connections.
    /// Each client instance has its own JWT for authentication.
    ///
    /// # Arguments
    /// * `url` - The gRPC endpoint URL (e.g., "https://orderbook-devnet.silvana.dev:443")
    /// * `jwt` - Optional JWT token for authentication (self-describing with embedded public key)
    pub async fn connect(url: &str, jwt: Option<String>) -> Result<Self> {
        Self::connect_with(url, jwt, CHANNEL_OPTS, RPC_DEADLINE).await
    }

    async fn connect_with(url: &str, jwt: Option<String>, opts: ChannelOpts, deadline: Duration) -> Result<Self> {
        let channel = Self::get_or_create_channel(url, opts).await?;
        Ok(Self::with_channel(channel, jwt, deadline))
    }

    fn with_channel(channel: Channel, jwt: Option<String>, deadline: Duration) -> Self {
        let token = Arc::new(RwLock::new(jwt.unwrap_or_default()));
        let auth_interceptor = AuthInterceptor { token: token.clone() };
        let settlement_client = SettlementServiceClient::with_interceptor(channel, auth_interceptor)
            .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        Self {
            settlement_client,
            token,
            deadline,
        }
    }

    /// Update the JWT token for authentication
    pub fn set_jwt(&self, jwt: String) {
        *sync::write(&self.token) = jwt;
    }

    /// Report structured errors to the server (best-effort ingestion).
    /// Returns (accepted, rejected). Callers are expected to swallow errors
    /// with a warn — reporting must never affect the calling flow, and an
    /// older server answering UNIMPLEMENTED is a normal rollout state.
    pub async fn report_errors(&mut self, errors: Vec<ErrorEvent>) -> Result<(u32, u32)> {
        let inner = transport::with_deadline(
            self.deadline,
            "ReportErrors",
            self.settlement_client.report_errors(ReportErrorsRequest { errors }),
        )
        .await?;
        Ok((inner.accepted, inner.rejected))
    }

    /// Record a transaction in the transaction_history table
    ///
    /// Returns the auto-generated transaction ID
    pub async fn record_transaction(
        &mut self,
        request: RecordTransactionRequest,
    ) -> Result<u64> {
        let inner = transport::with_deadline(
            self.deadline,
            "RecordTransaction",
            self.settlement_client.record_transaction(request),
        )
        .await?;

        if !inner.success {
            return Err(anyhow!("RecordTransaction failed: {}", inner.message));
        }

        Ok(inner.transaction_id)
    }

    /// Record a settlement event in the settlement_proposal_history table
    ///
    /// Returns the auto-generated event ID
    pub async fn record_settlement_event(
        &mut self,
        request: RecordSettlementEventRequest,
    ) -> Result<u64> {
        let inner = transport::with_deadline(
            self.deadline,
            "RecordSettlementEvent",
            self.settlement_client.record_settlement_event(request),
        )
        .await?;

        if !inner.success {
            return Err(anyhow!("RecordSettlementEvent failed: {}", inner.message));
        }

        Ok(inner.event_id)
    }

    /// Save a disclosed contract for settlement visibility
    ///
    /// Used during Canton Coin allocation to share LockedAmulet contract
    /// with the settlement operator.
    pub async fn save_disclosed_contract(
        &mut self,
        request: SaveDisclosedContractRequest,
    ) -> Result<()> {
        let inner = transport::with_deadline(
            self.deadline,
            "SaveDisclosedContract",
            self.settlement_client.save_disclosed_contract(request),
        )
        .await?;

        if !inner.success {
            return Err(anyhow!("SaveDisclosedContract failed: {}", inner.message));
        }

        Ok(())
    }

    /// Get settlement proposal by ID
    ///
    /// Fetches proposal details from the database via RPC.
    /// Used by propose/accept/allocate commands to get buyer/seller party IDs and amounts.
    /// Authentication via gRPC metadata header (set via connect() or set_jwt()).
    pub async fn get_settlement_proposal_by_id(
        &mut self,
        proposal_id: &str,
    ) -> Result<Option<SettlementProposalMessage>> {
        let request = GetSettlementProposalByIdRequest {
            canton_auth: None,  // External parties use metadata auth
            proposal_id: proposal_id.to_string(),
        };

        let inner = transport::with_deadline(
            self.deadline,
            "GetSettlementProposalById",
            self.settlement_client.get_settlement_proposal_by_id(request),
        )
        .await?;

        if inner.found {
            Ok(inner.proposal)
        } else {
            Ok(None)
        }
    }

    /// Submit preconfirmation decision for a settlement proposal
    ///
    /// This is the off-chain step where the party confirms they want to proceed
    /// with the settlement. Must be done before DVP propose/accept.
    pub async fn submit_preconfirmation(
        &mut self,
        proposal_id: &str,
        settlement_id: &str,
        party_id: &str,
        accept: bool,
    ) -> Result<()> {
        let decision = PreconfirmationDecision {
            proposal_id: proposal_id.to_string(),
            settlement_id: settlement_id.to_string(),
            response: if accept {
                PreconfirmationResponse::Accept as i32
            } else {
                PreconfirmationResponse::Reject as i32
            },
            rejection_reason: None,
            conditions: None,
            signed_by: party_id.to_string(),
            decided_at: Some(prost_types::Timestamp {
                seconds: clock::now_secs_i64(),
                nanos: 0,
            }),
        };

        let request = SubmitPreconfirmationRequest {
            auth: None, // External parties use metadata auth
            decision: Some(decision),
        };

        transport::with_deadline(
            self.deadline,
            "SubmitPreconfirmation",
            self.settlement_client.submit_preconfirmation(request),
        )
        .await?;

        Ok(())
    }

    /// Cancel a settlement (buyer or seller can cancel before settlement execution)
    pub async fn cancel_settlement(
        &mut self,
        proposal_id: &str,
        reason: &str,
    ) -> Result<bool> {
        let request = CancelSettlementRequest {
            auth: None, // External parties use metadata auth
            proposal_id: proposal_id.to_string(),
            reason: reason.to_string(),
        };

        let resp = transport::with_deadline(
            self.deadline,
            "CancelSettlement",
            self.settlement_client.cancel_settlement(request),
        )
        .await?;

        if !resp.success {
            warn!("CancelSettlement rejected: {}", resp.message);
        }
        Ok(resp.success)
    }

    /// Get settlement status including next actions for buyer/seller
    ///
    /// Returns the full settlement status with per-step details and
    /// server-computed NextAction for each party.
    pub async fn get_settlement_status(
        &mut self,
        settlement_id: &str,
    ) -> Result<GetSettlementStatusResponse> {
        let request = GetSettlementStatusRequest {
            auth: None, // External parties use metadata auth
            settlement_id: settlement_id.to_string(),
            supports_multicall: true,
        };

        transport::with_deadline(
            self.deadline,
            "GetSettlementStatus",
            self.settlement_client.get_settlement_status(request),
        )
        .await
    }

    /// Save a disclosed contract with specific fields
    ///
    /// Helper for saving LockedAmulet/LockedHolding contracts during allocation.
    pub async fn save_disclosed_contract_details(
        &mut self,
        proposal_id: &str,
        contract_id: &str,
        template_id: &str,
        created_event_blob: &str,
        synchronizer_id: &str,
    ) -> Result<()> {
        let request = SaveDisclosedContractRequest {
            auth: None,
            proposal_id: proposal_id.to_string(),
            contract: Some(DisclosedContractMessage {
                contract_id: contract_id.to_string(),
                template_id: template_id.to_string(),
                created_event_blob: created_event_blob.to_string(),
                synchronizer_id: synchronizer_id.to_string(),
            }),
        };

        self.save_disclosed_contract(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tonic::codegen::http;
    use tonic::service::Interceptor;

    const SHORT: Duration = Duration::from_millis(250);

    /// Counts calls and never answers them.
    #[derive(Clone, Default)]
    struct SilentSettlement(Arc<AtomicUsize>);

    impl tonic::server::NamedService for SilentSettlement {
        const NAME: &'static str = "silvana.settlement.v1.SettlementService";
    }

    impl<B> tonic::codegen::Service<http::Request<B>> for SilentSettlement
    where
        B: tonic::codegen::Body + Send + 'static,
        B::Error: Into<tonic::codegen::StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: http::Request<B>) -> Self::Future {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    fn serve(listener: tokio::net::TcpListener) -> Arc<AtomicUsize> {
        let server = SilentSettlement::default();
        let calls = server.0.clone();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(server)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        calls
    }

    async fn spawn_server() -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (url, serve(listener))
    }

    fn test_opts() -> ChannelOpts {
        ChannelOpts {
            connect: Duration::from_secs(2),
            tls: Duration::from_millis(200),
            request: None,
            keepalive: false,
        }
    }

    async fn test_client(url: &str) -> OrderbookRpcClient {
        match OrderbookRpcClient::connect_with(url, Some("jwt".to_string()), test_opts(), SHORT).await {
            Ok(c) => c,
            Err(e) => panic!("connect to {url}: {e:#}"),
        }
    }

    /// The error of a call that must fail within a few seconds.
    async fn bounded<T>(call: impl Future<Output = Result<T>>) -> String {
        match tokio::time::timeout(Duration::from_secs(5), call).await {
            Ok(Err(e)) => e.to_string(),
            Ok(Ok(_)) => panic!("a silent server cannot answer"),
            Err(_) => panic!("the client deadline did not end the call"),
        }
    }

    async fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // The channel's request timeout does not cover the reconnect wait, so every call carries its own deadline
    #[tokio::test]
    async fn every_rpc_is_bounded_by_the_client_deadline() {
        let (url, calls) = spawn_server().await;
        let mut c = test_client(&url).await;
        let errors = [
            ("ReportErrors", bounded(c.report_errors(Vec::new())).await),
            ("RecordTransaction", bounded(c.record_transaction(RecordTransactionRequest::default())).await),
            (
                "RecordSettlementEvent",
                bounded(c.record_settlement_event(RecordSettlementEventRequest::default())).await,
            ),
            (
                "SaveDisclosedContract",
                bounded(c.save_disclosed_contract_details("p", "c", "t", "b", "s")).await,
            ),
            ("GetSettlementProposalById", bounded(c.get_settlement_proposal_by_id("p")).await),
            ("SubmitPreconfirmation", bounded(c.submit_preconfirmation("p", "s", "party", true)).await),
            ("CancelSettlement", bounded(c.cancel_settlement("p", "test")).await),
            ("GetSettlementStatus", bounded(c.get_settlement_status("s")).await),
        ];
        for (what, err) in errors {
            let expected = format!("{what} RPC failed (Unavailable): client deadline");
            assert!(err.starts_with(&expected), "{err}");
        }
        wait_for("every call to reach the server", || calls.load(Ordering::SeqCst) == 8).await;
    }

    #[tokio::test]
    async fn channels_are_cached_per_url() {
        let (url_a, calls_a) = spawn_server().await;
        let (url_b, calls_b) = spawn_server().await;
        let mut a = test_client(&url_a).await;
        let mut b = test_client(&url_b).await;
        bounded(a.report_errors(Vec::new())).await;
        bounded(b.report_errors(Vec::new())).await;
        wait_for("one call on each server", || {
            calls_a.load(Ordering::SeqCst) == 1 && calls_b.load(Ordering::SeqCst) == 1
        })
        .await;
        assert!(Arc::ptr_eq(&channel_cell(&url_a), &channel_cell(&url_a)));
        assert!(!Arc::ptr_eq(&channel_cell(&url_a), &channel_cell(&url_b)));
    }

    #[tokio::test]
    async fn a_failed_connect_leaves_the_channel_uncached() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{addr}");
        let first = OrderbookRpcClient::connect_with(&url, None, test_opts(), SHORT).await;
        let err = first.err().expect("nothing listens yet").to_string();
        assert!(err.starts_with(&format!("Failed to connect to {url}: ")), "{err}");
        assert!(channel_cell(&url).get().is_none());

        let calls = serve(tokio::net::TcpListener::bind(addr).await.unwrap());
        let mut c = test_client(&url).await;
        bounded(c.report_errors(Vec::new())).await;
        wait_for("the call to reach the new server", || calls.load(Ordering::SeqCst) == 1).await;
    }

    // Without its own bound a caller would wait for every queued connect in turn
    #[tokio::test]
    async fn a_caller_queued_behind_a_stuck_connect_is_bounded() {
        let cell = Arc::new(OnceCell::new());
        let holder = cell.clone();
        let stuck = tokio::spawn(async move {
            let _ = holder.get_or_try_init(std::future::pending::<Result<Channel>>).await;
        });
        tokio::task::yield_now().await;
        let url = "http://queued.invalid";
        let waiter = cached_channel(&cell, url, SHORT, std::future::pending::<Result<Channel>>);
        let err = tokio::time::timeout(Duration::from_secs(4), waiter)
            .await
            .expect("the caller's own budget should end the wait")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with(&format!("Failed to connect to {url}: (Unavailable) connect to")), "{err}");
        assert!(cell.get().is_none());
        stuck.abort();
    }

    // A peer that accepts TCP but never answers the TLS handshake fails the connect
    #[tokio::test]
    async fn a_stalled_tls_handshake_fails_the_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let connect = OrderbookRpcClient::connect_with(&url, None, test_opts(), SHORT);
        let result = tokio::time::timeout(Duration::from_secs(4), connect).await;
        assert!(matches!(result, Ok(Err(_))), "the handshake bound should end the connect");
        assert!(channel_cell(&url).get().is_none());
        assert_eq!(CHANNEL_OPTS.connect_budget(), Duration::from_secs(25));
        assert_eq!(CHANNEL_OPTS.request, Some(RPC_DEADLINE));
        drop(listener);
    }

    #[tokio::test]
    async fn the_token_lock_survives_poison() {
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let client = OrderbookRpcClient::with_channel(channel, Some("t0".to_string()), SHORT);
        let token = client.token.clone();
        let poisoner = std::thread::spawn(move || {
            let _held = token.write().unwrap();
            panic!("poison the token lock");
        });
        assert!(poisoner.join().is_err());
        assert!(client.token.is_poisoned());

        let mut interceptor = AuthInterceptor { token: client.token.clone() };
        let header = |r: Request<()>| r.metadata().get("authorization").unwrap().to_str().unwrap().to_string();
        assert_eq!(header(interceptor.call(Request::new(())).unwrap()), "Bearer t0");
        client.set_jwt("t1".to_string());
        assert_eq!(header(interceptor.call(Request::new(())).unwrap()), "Bearer t1");
    }
}
