//! gRPC client for the DAppProviderService (CIP-0103)
//!
//! Provides query methods (balances, contracts, preapprovals, rates) and
//! two-phase transaction submission (prepare → sign → execute).
//!
//! Phase A: Signs the server-provided hash directly.
//! Phase B (future): tx-verifier will inspect + recompute hash before signing.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use agent_logic::clock;
use agent_logic::secret::Secret;
use agent_logic::transport::{self, ChannelOpts, StreamEnd};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use tonic::transport::Channel;
use tonic::Request;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default for `MAX_RETRIES`.
pub const DEFAULT_MAX_RETRIES: u32 = 5;
const MAX_RETRIES_ENV: &str = "MAX_RETRIES";

/// Attempts per transaction; `crate::env::validate` rejects bad values at startup.
static MAX_RETRIES: Lazy<u32> = Lazy::new(|| match max_retries_from_env() {
    Ok(n) => n.max(1),
    Err(e) => {
        warn!("{e:#}; using {DEFAULT_MAX_RETRIES}");
        DEFAULT_MAX_RETRIES
    }
});

/// `MAX_RETRIES` from the environment: unset or blank gives the default,
/// anything else must be a whole number of at least 1.
pub fn max_retries_from_env() -> Result<u32> {
    match std::env::var(MAX_RETRIES_ENV) {
        Ok(raw) => parse_max_retries(Some(&raw)),
        Err(std::env::VarError::NotPresent) => parse_max_retries(None),
        Err(e) => bail!("{MAX_RETRIES_ENV}: {e}"),
    }
}

pub(crate) fn parse_max_retries(raw: Option<&str>) -> Result<u32> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_MAX_RETRIES);
    };
    let n: u32 = raw
        .parse()
        .with_context(|| format!("{MAX_RETRIES_ENV}={raw:?} is not a whole number"))?;
    if n == 0 {
        bail!("{MAX_RETRIES_ENV}=0: at least one attempt is required");
    }
    Ok(n)
}

const BASE_DELAY_MS: u64 = 1000;
/// Longest retry delay before jitter.
const MAX_BACKOFF_MS: u64 = 30_000;
/// Wait before re-preparing after INACTIVE_CONTRACTS.
const INACTIVE_RETRY_DELAY: Duration = Duration::from_millis(2000);
/// Wait before scanning ledger updates for a command whose execute failed.
const RECOVERY_SCAN_DELAY: Duration = Duration::from_secs(5);
/// A submit starts no new attempt once this much time has passed.
pub const SUBMIT_BUDGET: Duration = Duration::from_secs(120);
/// Bound on local transaction verification.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
/// Added to each call's client deadline so the channel's request timeout fires first.
const CALL_DEADLINE_SLACK: Duration = Duration::from_secs(5);
/// Added to the request timeout to bound collecting a whole stream.
const STREAM_TOTAL_SLACK: Duration = Duration::from_secs(30);
const MAX_DECODING_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Retry delay for 0-based `attempt`: exponential, capped at 30s, plus up to 10% jitter.
fn backoff_ms(attempt: u32) -> u64 {
    let delay = BASE_DELAY_MS
        .saturating_mul(2u64.saturating_pow(attempt))
        .min(MAX_BACKOFF_MS);
    delay.saturating_add(clock::jitter_ms((delay / 10).saturating_add(1)))
}

/// Time limit for starting new attempts of one submit; a running attempt is never cut short.
#[derive(Clone, Copy, Debug)]
struct SubmitBudget {
    total: Duration,
    deadline: tokio::time::Instant,
}

impl SubmitBudget {
    fn new(total: Duration) -> Self {
        Self { total, deadline: clock::deadline_after(total) }
    }

    /// Whether attempt `attempt_no + 1` may start after `wait`, given the last one took `last`.
    fn allows_retry(
        &self,
        attempt_no: u32,
        max_retries: u32,
        wait: Duration,
        last: Duration,
        command_id: &str,
    ) -> bool {
        if attempt_no >= max_retries {
            return false;
        }
        if self.fits_at(tokio::time::Instant::now(), wait, last) {
            return true;
        }
        warn!(
            "Submit budget of {}s spent after {} attempt(s); not retrying [{}]",
            self.total.as_secs(),
            attempt_no,
            command_id
        );
        false
    }

    fn fits_at(&self, now: tokio::time::Instant, wait: Duration, last: Duration) -> bool {
        now < self.deadline && self.deadline.saturating_duration_since(now) >= wait.saturating_add(last)
    }
}

/// Duration in seconds to pause regular fee dispatch after SEQUENCER_BACKPRESSURE.
static FEE_PAUSE_SECS: Lazy<u64> = Lazy::new(|| {
    std::env::var("FEE_PAUSE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
});

/// Duration in seconds to pause background housekeeping after
/// SEQUENCER_BACKPRESSURE.
///
/// Deliberately much longer than `FEE_PAUSE_SECS`: background work (today, the
/// DvpProposal GC) has no deadline whatsoever, so it should be the last thing to
/// resume competing for sequencer slots. This is the same deadline the on-chain
/// CC traffic fees used before traffic billing moved off-chain — a low-priority
/// class of transaction that yields to settlements under congestion.
static BACKGROUND_PAUSE_SECS: Lazy<u64> = Lazy::new(|| {
    std::env::var("BACKGROUND_PAUSE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
});

/// Epoch millis after which regular fee dispatch may resume.
static FEE_PAUSE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Epoch millis after which background housekeeping may resume.
static BACKGROUND_PAUSE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Record a backpressure event — pauses regular fees for FEE_PAUSE_SECS
/// and background housekeeping for BACKGROUND_PAUSE_SECS.
pub fn signal_sequencer_backpressure() {
    let now_ms = clock::now_millis();

    let fee_resume_at = resume_at_ms(now_ms, *FEE_PAUSE_SECS);
    FEE_PAUSE_UNTIL_MS.fetch_max(fee_resume_at, AtomicOrdering::Relaxed);

    let background_resume_at = resume_at_ms(now_ms, *BACKGROUND_PAUSE_SECS);
    BACKGROUND_PAUSE_UNTIL_MS.fetch_max(background_resume_at, AtomicOrdering::Relaxed);
}

fn resume_at_ms(now_ms: u64, pause_secs: u64) -> u64 {
    now_ms.saturating_add(pause_secs.saturating_mul(1000))
}

/// Whole seconds left until `resume_at`, rounded up; `None` once it has passed.
fn remaining_secs(resume_at: u64, now_ms: u64) -> Option<u64> {
    (now_ms < resume_at).then(|| resume_at.saturating_sub(now_ms).div_ceil(1000))
}

fn pause_remaining(until_ms: &AtomicU64) -> Option<u64> {
    let resume_at = until_ms.load(AtomicOrdering::Relaxed);
    if resume_at == 0 {
        return None;
    }
    remaining_secs(resume_at, clock::now_millis())
}

/// Check whether regular fees are currently paused due to sequencer backpressure.
/// Returns Some(remaining_secs) if paused, None if not.
pub fn fee_pause_remaining() -> Option<u64> {
    pause_remaining(&FEE_PAUSE_UNTIL_MS)
}

/// Check whether background housekeeping is currently paused due to sequencer
/// backpressure. Returns Some(remaining_secs) if paused, None if not.
pub fn background_pause_remaining() -> Option<u64> {
    pause_remaining(&BACKGROUND_PAUSE_UNTIL_MS)
}

use agent_logic::auth::generate_jwt;
use message_signing::{
    sign_canonical, verify_canonical, parse_public_key,
    canonical_prepare_request, canonical_prepare_response,
    canonical_execute_request, canonical_execute_response,
    canonical_params_pay_fee, canonical_params_propose_dvp, canonical_params_accept_dvp,
    canonical_params_allocate, canonical_params_cancel_dvp_proposal,
    canonical_params_reject_dvp_proposal,
    canonical_params_transfer_cc, canonical_params_request_preapproval,
    canonical_params_request_recurring_prepaid, canonical_params_request_recurring_payasyougo,
    canonical_params_request_user_service, canonical_params_transfer_cip56,
    canonical_params_accept_cip56, canonical_params_split_cc,
    canonical_params_execute_multicall,
    canonical_params_lock_holdings, canonical_params_process_lock_unlock_requests,
    canonical_params_resize_lock, canonical_params_terminate_lock,
    canonical_params_faucet, canonical_params_prepay_traffic,
};
use orderbook_proto::ledger::prepare_transaction_request::Params;
use tx_verifier::OperationExpectation;
use orderbook_proto::ledger::{
    d_app_provider_service_client::DAppProviderServiceClient,
    ActiveContractInfo, DiscoveredContract, ExecuteTransactionRequest,
    ExecuteTransactionResponse, GetActiveContractsRequest, GetBalancesRequest,
    GetPrepaidTrafficBalanceRequest,
    GetDsoRatesRequest, GetDsoRatesResponse, GetLedgerEndRequest,
    GetPreapprovalsRequest, GetSettlementContractsRequest, GetUpdatesRequest,
    GetUpdatesResponse, MessageSignature, PreapprovalInfo, PrepareTransactionRequest,
    get_updates_response, ledger_event,
    PrepareTransactionResponse, TokenBalance,
    FaucetRequest, FaucetResponse,
    FaucetInstrument, ListFaucetInstrumentsRequest,
    PreparePayFeeRequest, PreparePayFeeResponse,
    ExecutePayFeeRequest, ExecutePayFeeResponse,
    TransactionOperation,
};

/// Whether an operation is background housekeeping — work with no deadline that
/// exists only to keep the ACS tidy.
///
/// Such a transaction must not be retried into a congested sequencer: the retries
/// are exactly the load we are trying to shed, and nothing is lost by giving up.
/// The contract stays active and the next scan re-queues it, by which time the
/// caller's pause gate has waited the congestion out.
///
/// Both variants here are emitted only by [`crate::dvp_gc_worker`]. Anything
/// added to this list must be similarly deadline-free — a transaction a user or
/// a settlement is waiting on does not belong here.
fn is_background_op(operation: i32) -> bool {
    operation == TransactionOperation::CancelDvpProposal as i32
        || operation == TransactionOperation::RejectDvpProposal as i32
}

/// Build canonical payload from a PrepareTransactionRequest for signing
fn build_canonical_from_prepare_request(req: &PrepareTransactionRequest) -> Result<Vec<u8>> {
    let params = req.params.as_ref().context("PrepareTransactionRequest missing params")?;
    let params_canonical = match params {
        Params::PayFee(p) => canonical_params_pay_fee(&p.proposal_id, &p.fee_type),
        Params::ProposeDvp(p) => canonical_params_propose_dvp(&p.proposal_id),
        Params::AcceptDvp(p) => canonical_params_accept_dvp(&p.proposal_id, &p.dvp_proposal_cid),
        Params::Allocate(p) => canonical_params_allocate(&p.proposal_id, &p.dvp_cid),
        Params::CancelDvpProposal(p) => canonical_params_cancel_dvp_proposal(&p.dvp_proposal_cid),
        Params::RejectDvpProposal(p) => canonical_params_reject_dvp_proposal(&p.dvp_proposal_cid, &p.reason),
        Params::TransferCc(p) => canonical_params_transfer_cc(
            &p.receiver_party, &p.amount, p.description.as_deref(),
            &p.command_id, p.settlement_proposal_id.as_deref(),
        ),
        Params::RequestPreapproval(p) => canonical_params_request_preapproval(&p.instrument_admin),
        Params::RequestRecurringPrepaid(p) => canonical_params_request_recurring_prepaid(
            &p.app_party, &p.amount, &p.locked_amount, p.lock_days,
            p.description.as_deref(), p.reference.as_deref(),
        ),
        Params::RequestRecurringPayasyougo(p) => canonical_params_request_recurring_payasyougo(
            &p.app_party, &p.amount, p.description.as_deref(), p.reference.as_deref(),
        ),
        Params::RequestUserService(p) => canonical_params_request_user_service(
            p.reference_id.as_deref(), p.party_name.as_deref(),
        ),
        Params::TransferCip56(p) => canonical_params_transfer_cip56(
            &p.instrument_id, &p.instrument_admin, &p.receiver_party,
            &p.amount, p.reference.as_deref(), &p.input_holding_cids,
            p.max_input_holdings,
        ),
        Params::AcceptCip56(p) => canonical_params_accept_cip56(&p.contract_id),
        Params::SplitCc(p) => canonical_params_split_cc(&p.output_amounts),
        Params::ExecuteMulticall(p) => canonical_params_execute_multicall(p.operations.len()),
        Params::LockHoldings(p) => canonical_params_lock_holdings(&p.lock_service_cid, &p.amount, &p.context),
        Params::ProcessLockUnlockRequests(p) => canonical_params_process_lock_unlock_requests(&p.lock_controller_cid, p.requests.len()),
        Params::ResizeLock(p) => canonical_params_resize_lock(&p.lock_controller_cid, &p.new_amount),
        Params::TerminateLock(p) => canonical_params_terminate_lock(&p.lock_controller_cid),
        Params::PrepayTraffic(p) => canonical_params_prepay_traffic(
            &p.amount, p.description.as_deref(), &p.command_id,
        ),
    };
    Ok(canonical_prepare_request(req.operation, &params_canonical))
}

/// Signing identity for the interceptor. Absent for the few queries the RPC
/// relays without authentication, which need no party and no key.
#[derive(Clone)]
struct AuthIdentity {
    token: Arc<RwLock<String>>,
    expires_at: Arc<RwLock<u64>>,
    party_id: String,
    role: String,
    private_key: Secret<32>,
    ttl_secs: u64,
    node_name: Option<String>,
}

/// Authentication interceptor for gRPC requests with automatic JWT refresh
#[derive(Clone)]
struct AuthInterceptor {
    identity: Option<AuthIdentity>,
}

impl AuthInterceptor {
    /// Interceptor for `party_id`, starting with a freshly minted token.
    fn for_party(
        party_id: &str,
        role: &str,
        private_key: &Secret<32>,
        ttl_secs: u64,
        node_name: Option<&str>,
    ) -> Result<Self> {
        let jwt = generate_jwt(party_id, role, &*private_key.expose()?, ttl_secs, node_name)?;
        let now = agent_logic::clock::now_secs();
        Ok(Self {
            identity: Some(AuthIdentity {
                token: Arc::new(RwLock::new(jwt)),
                expires_at: Arc::new(RwLock::new(now.saturating_add(ttl_secs))),
                party_id: party_id.to_string(),
                role: role.to_string(),
                private_key: private_key.clone(),
                ttl_secs,
                node_name: node_name.map(|s| s.to_string()),
            }),
        })
    }
}

/// Refresh JWT 5 minutes before expiry
const REFRESH_BEFORE_EXPIRY_SECS: u64 = 300;

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, tonic::Status> {
        let Some(identity) = self.identity.as_ref() else {
            return Ok(request);
        };
        let now = agent_logic::clock::now_secs();
        let expires_at = *agent_logic::sync::read(&identity.expires_at);

        if now.saturating_add(REFRESH_BEFORE_EXPIRY_SECS) >= expires_at {
            // On failure the previous token is kept
            let refreshed = identity.private_key.expose().map_err(anyhow::Error::from).and_then(|key| {
                generate_jwt(
                    &identity.party_id,
                    &identity.role,
                    &key,
                    identity.ttl_secs,
                    identity.node_name.as_deref(),
                )
            });
            match refreshed {
                Ok(new_jwt) => {
                    debug!("JWT token refreshed (was expiring in {}s)", expires_at.saturating_sub(now));
                    *agent_logic::sync::write(&identity.token) = new_jwt;
                    *agent_logic::sync::write(&identity.expires_at) = now.saturating_add(identity.ttl_secs);
                }
                Err(e) => {
                    tracing::error!("Failed to refresh JWT: {}", e);
                }
            }
        }

        let token = agent_logic::sync::read(&identity.token).clone();
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", token)
                .parse()
                .map_err(|_| tonic::Status::internal("Failed to parse JWT token"))?,
        );
        Ok(request)
    }
}

/// Client-side time bounds of one ledger client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CallBounds {
    /// Deadline on one call or stream open, including the channel's reconnect wait.
    call: Duration,
    /// Longest wait for the next stream message.
    stream_idle: Duration,
    /// Bound on collecting one whole stream.
    stream_total: Duration,
}

impl CallBounds {
    fn for_request_timeout(request: Duration) -> Self {
        Self {
            call: request.saturating_add(CALL_DEADLINE_SLACK),
            stream_idle: transport::STREAM_IDLE_TIMEOUT,
            stream_total: request.saturating_add(STREAM_TOTAL_SLACK),
        }
    }

    fn for_request_secs(request_timeout_secs: Option<u64>) -> Self {
        let secs = request_timeout_secs.unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
        Self::for_request_timeout(Duration::from_secs(secs))
    }
}

fn channel_opts(connect_timeout_secs: Option<u64>, request_timeout_secs: Option<u64>) -> ChannelOpts {
    ChannelOpts {
        connect: Duration::from_secs(connect_timeout_secs.unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS)),
        tls: transport::TLS_HANDSHAKE_TIMEOUT,
        request: Some(Duration::from_secs(request_timeout_secs.unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS))),
        keepalive: true,
    }
}

/// Gap between a caller's outer wait and the client's own bound.
const OUTER_WAIT_SLACK: Duration = Duration::from_secs(5);

/// Outer wait for creating a client with these timeouts; above its own connect bound.
pub(crate) fn connect_wait(connect_timeout_secs: u64, request_timeout_secs: u64) -> Duration {
    channel_opts(Some(connect_timeout_secs), Some(request_timeout_secs))
        .connect_budget()
        .saturating_add(OUTER_WAIT_SLACK)
}

/// Outer wait for one call; above the client's own call deadline.
pub(crate) fn call_wait(request_timeout_secs: u64) -> Duration {
    CallBounds::for_request_secs(Some(request_timeout_secs)).call.saturating_add(OUTER_WAIT_SLACK)
}

/// Outer wait for a collected stream; above the open deadline plus the stream bound.
pub(crate) fn stream_wait(request_timeout_secs: u64) -> Duration {
    let bounds = CallBounds::for_request_secs(Some(request_timeout_secs));
    bounds.call.saturating_add(bounds.stream_total).saturating_add(OUTER_WAIT_SLACK)
}

/// `work` bounded by `budget`; running out is an error naming `what`.
/// Only for work that commits nothing, as it is dropped on elapse.
pub(crate) async fn within<T>(
    what: &str,
    budget: Duration,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(budget, work).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("{what} did not finish within {budget:?}")),
    }
}

/// Client for the DAppProviderService gRPC API (CIP-0103). Clones share the
/// channel and the token.
#[derive(Clone)]
pub struct DAppProviderClient {
    client: DAppProviderServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    bounds: CallBounds,
    /// The party id whose JWT this client carries. Used to bind fees
    /// authorization signatures to a specific party.
    party_id: String,
    private_key: Secret<32>,
    /// Pre-configured ledger service public key for response signature verification
    ledger_service_public_key: [u8; 32],
    /// Optional auto-topup trigger. When set, every successful
    /// `submit_transaction` non-blockingly nudges the background topup
    /// runner (which owns its own DAppProviderClient and runs in a
    /// dedicated task — see [`crate::topup::TopupRunner::spawn`]).
    topup_trigger: Option<crate::topup::TopupTrigger>,
}

impl DAppProviderClient {
    /// Create a new DAppProviderClient (CIP-0103)
    /// Client that sends no authorization header, for the queries the RPC
    /// relays without authentication. Party-scoped calls will be rejected.
    pub async fn new_anonymous(
        grpc_url: &str,
        connection_timeout_secs: Option<u64>,
        request_timeout_secs: Option<u64>,
    ) -> Result<Self> {
        let channel =
            Self::create_channel(grpc_url, connection_timeout_secs, request_timeout_secs).await?;
        let client =
            DAppProviderServiceClient::with_interceptor(channel, AuthInterceptor { identity: None })
                .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);
        Ok(Self {
            client,
            bounds: CallBounds::for_request_secs(request_timeout_secs),
            party_id: String::new(),
            private_key: Secret::seal(&mut [0u8; 32])?,
            ledger_service_public_key: [0u8; 32],
            topup_trigger: None,
        })
    }

    pub async fn new(
        grpc_url: &str,
        party_id: &str,
        role: &str,
        private_key: &Secret<32>,
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
        connection_timeout_secs: Option<u64>,
        request_timeout_secs: Option<u64>,
    ) -> Result<Self> {
        let channel = Self::create_channel(grpc_url, connection_timeout_secs, request_timeout_secs).await?;
        let interceptor = AuthInterceptor::for_party(party_id, role, private_key, ttl_secs, node_name)?;
        let client = DAppProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);
        Ok(Self {
            client,
            bounds: CallBounds::for_request_secs(request_timeout_secs),
            party_id: party_id.to_string(),
            private_key: private_key.clone(),
            ledger_service_public_key: *ledger_service_public_key,
            topup_trigger: None,
        })
    }

    /// Set call deadlines to match a channel built with this request timeout.
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.bounds = CallBounds::for_request_timeout(request_timeout);
        self
    }

    #[cfg(test)]
    fn with_bounds(mut self, bounds: CallBounds) -> Self {
        self.bounds = bounds;
        self
    }

    #[cfg(test)]
    pub(crate) fn call_deadline(&self) -> Duration {
        self.bounds.call
    }

    /// Attach an auto-topup trigger. After every successful submit, the
    /// trigger nudges the background topup runner (non-blocking). Call
    /// this only on the client used by the agent's main loop — never on
    /// the runner's own internal client.
    pub fn with_topup_trigger(mut self, trigger: crate::topup::TopupTrigger) -> Self {
        self.topup_trigger = Some(trigger);
        self
    }

    /// Create a DAppProviderClient from a pre-existing shared channel.
    ///
    /// Skips channel creation (no TCP+TLS handshake). Each client gets its own
    /// `AuthInterceptor` for JWT refresh, but shares the underlying HTTP/2 connection.
    /// Call deadlines assume the default request timeout; see [`Self::with_request_timeout`].
    pub fn from_channel(
        channel: Channel,
        party_id: &str,
        role: &str,
        private_key: &Secret<32>,
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
    ) -> Result<Self> {
        let interceptor = AuthInterceptor::for_party(party_id, role, private_key, ttl_secs, node_name)?;
        let client = DAppProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);
        Ok(Self {
            client,
            bounds: CallBounds::for_request_secs(None),
            party_id: party_id.to_string(),
            private_key: private_key.clone(),
            ledger_service_public_key: *ledger_service_public_key,
            topup_trigger: None,
        })
    }

    /// Create gRPC channel with optional TLS and timeouts
    ///
    /// The returned `Channel` is `Clone` and supports HTTP/2 multiplexing —
    /// it can be shared across multiple workers to avoid per-request TCP+TLS overhead.
    pub async fn create_channel(
        grpc_url: &str,
        connect_timeout_secs: Option<u64>,
        request_timeout_secs: Option<u64>,
    ) -> Result<Channel> {
        transport::connect_channel(grpc_url, channel_opts(connect_timeout_secs, request_timeout_secs))
            .await
            .context("Failed to connect to DAppProvider service")
    }

    // ========================================================================
    // Query RPCs
    // ========================================================================

    /// Get active contracts for the authenticated party (streaming RPC).
    ///
    /// A mid-stream error is a hard `Err`: a truncated snapshot must never
    /// masquerade as the full ACS — a partial list returned as `Ok` once drove
    /// the holdings cache to evict every live rung (the mainnet split storm).
    /// Callers that can use partial data ADDITIVELY should call
    /// [`Self::get_active_contracts_partial`] instead.
    pub async fn get_active_contracts(
        &mut self,
        template_filters: &[String],
    ) -> Result<Vec<ActiveContractInfo>> {
        let (contracts, complete) = self.get_active_contracts_partial(template_filters).await?;
        if !complete {
            return Err(anyhow!(
                "GetActiveContracts stream aborted mid-stream after {} contract(s) — snapshot incomplete",
                contracts.len()
            ));
        }
        Ok(contracts)
    }

    /// Streaming ACS fetch that reports completeness instead of failing on a
    /// mid-stream error. `complete == false` means the stream broke partway:
    /// the returned contracts exist on the ledger and may be merged
    /// ADDITIVELY, but the list MUST NOT drive eviction/reconciliation.
    pub async fn get_active_contracts_partial(
        &mut self,
        template_filters: &[String],
    ) -> Result<(Vec<ActiveContractInfo>, bool)> {
        let open = self.client.get_active_contracts(GetActiveContractsRequest {
            template_filters: template_filters.to_vec(),
        });
        let stream = transport::with_deadline(self.bounds.call, "GetActiveContracts", open).await?;
        let (responses, end) =
            transport::collect_stream(stream, self.bounds.stream_idle, self.bounds.stream_total).await;
        let contracts: Vec<ActiveContractInfo> =
            responses.into_iter().filter_map(|response| response.contract).collect();
        match &end {
            StreamEnd::Complete => {}
            StreamEnd::Error(e) => warn!(
                "GetActiveContracts stream error after {} contract(s): {}",
                contracts.len(),
                e
            ),
            incomplete => warn!(
                "GetActiveContracts stream incomplete after {} contract(s): {}",
                contracts.len(),
                incomplete
            ),
        }
        Ok((contracts, end.is_complete()))
    }

    /// Get current ledger end offset
    pub async fn get_ledger_end(&mut self) -> Result<i64> {
        let call = self.client.get_ledger_end(GetLedgerEndRequest {});
        Ok(transport::with_deadline(self.bounds.call, "GetLedgerEnd", call).await?.offset)
    }

    /// Ledger updates in an offset range (streaming RPC, collected into a Vec).
    /// `complete == false` means the stream ended early: the updates are only a prefix of the range.
    pub async fn get_updates(
        &mut self,
        begin_exclusive: i64,
        end_inclusive: Option<i64>,
        template_filters: &[String],
    ) -> Result<(Vec<GetUpdatesResponse>, bool)> {
        let open = self.client.get_updates(GetUpdatesRequest {
            begin_exclusive,
            end_inclusive,
            template_filters: template_filters.to_vec(),
        });
        let stream = transport::with_deadline(self.bounds.call, "GetUpdates", open).await?;
        let (updates, end) =
            transport::collect_stream(stream, self.bounds.stream_idle, self.bounds.stream_total).await;
        match &end {
            StreamEnd::Complete => {}
            StreamEnd::Error(e) => warn!("GetUpdates stream error: {}", e),
            incomplete => warn!(
                "GetUpdates stream incomplete after {} update(s): {}",
                updates.len(),
                incomplete
            ),
        }
        Ok((updates, end.is_complete()))
    }

    /// Get token balances
    pub async fn get_balances(&mut self) -> Result<Vec<TokenBalance>> {
        let call = self.client.get_balances(GetBalancesRequest {});
        Ok(transport::with_deadline(self.bounds.call, "GetBalances", call).await?.balances)
    }

    /// Get off-chain prepaid traffic balance + credit limit for the
    /// authenticated party. Returns rust_decimal::Decimal values parsed at
    /// the boundary so callers don't have to handle decimal strings.
    pub async fn get_prepaid_traffic_balance(
        &mut self,
    ) -> Result<crate::PrepaidTrafficBalance> {
        let call = self.client.get_prepaid_traffic_balance(GetPrepaidTrafficBalanceRequest {});
        let r = transport::with_deadline(self.bounds.call, "GetPrepaidTrafficBalance", call).await?;
        Ok(crate::PrepaidTrafficBalance {
            balance_cc: r.balance_cc.parse()
                .map_err(|e| anyhow!("invalid balance_cc '{}': {}", r.balance_cc, e))?,
            credit_limit_cc: r.credit_limit_cc.parse()
                .map_err(|e| anyhow!("invalid credit_limit_cc '{}': {}", r.credit_limit_cc, e))?,
            available_cc: r.available_cc.parse()
                .map_err(|e| anyhow!("invalid available_cc '{}': {}", r.available_cc, e))?,
            total_credited_cc: r.total_credited_cc.parse()
                .map_err(|e| anyhow!("invalid total_credited_cc '{}': {}", r.total_credited_cc, e))?,
            total_debited_cc: r.total_debited_cc.parse()
                .map_err(|e| anyhow!("invalid total_debited_cc '{}': {}", r.total_debited_cc, e))?,
        })
    }

    /// Fetch TransferPreapproval contracts
    pub async fn get_preapprovals(&mut self) -> Result<Vec<PreapprovalInfo>> {
        let call = self.client.get_preapprovals(GetPreapprovalsRequest {});
        Ok(transport::with_deadline(self.bounds.call, "GetPreapprovals", call).await?.preapprovals)
    }

    /// Get DSO rates (CC/USD rate, current round)
    pub async fn get_dso_rates(&mut self) -> Result<GetDsoRatesResponse> {
        let call = self.client.get_dso_rates(GetDsoRatesRequest {});
        transport::with_deadline(self.bounds.call, "GetDsoRates", call).await
    }

    /// Discover on-chain DvpProposal/Dvp contracts for active settlements
    pub async fn get_settlement_contracts(
        &mut self,
        settlement_ids: &[String],
    ) -> Result<Vec<DiscoveredContract>> {
        let call = self.client.get_settlement_contracts(GetSettlementContractsRequest {
            settlement_ids: settlement_ids.to_vec(),
        });
        Ok(transport::with_deadline(self.bounds.call, "GetSettlementContracts", call).await?.contracts)
    }

    /// Get unlocked amulets via the dedicated GetAmulets RPC
    pub async fn get_amulets(&mut self) -> Result<Vec<crate::acs_worker::AmuletInfo>> {
        use orderbook_proto::ledger::GetAmuletsRequest;

        let call = self.client.get_amulets(GetAmuletsRequest {});
        let resp = transport::with_deadline(self.bounds.call, "GetAmulets", call).await?;

        Ok(resp
            .amulets
            .into_iter()
            .map(|a| crate::acs_worker::AmuletInfo {
                contract_id: a.contract_id,
                amount: a.amount.parse::<rust_decimal::Decimal>().unwrap_or(rust_decimal::Decimal::ZERO),
            })
            .collect())
    }

    /// Get amulets via GetActiveContracts with Amulet template filter (fallback)
    pub async fn get_amulets_via_acs(&mut self) -> Result<Vec<crate::acs_worker::AmuletInfo>> {
        let template = "#splice-amulet:Splice.Amulet:Amulet".to_string();
        let contracts = self.get_active_contracts(&[template]).await?;

        Ok(contracts
            .into_iter()
            .filter(|c| !c.template_id.contains("Locked"))
            .filter_map(|c| {
                let args = c.create_arguments?;
                let amount_str = args.fields.get("amount")
                    .and_then(|v| match &v.kind {
                        Some(prost_types::value::Kind::StructValue(s)) =>
                            s.fields.get("initialAmount"),
                        _ => None,
                    })
                    .and_then(|v| match &v.kind {
                        Some(prost_types::value::Kind::StringValue(s)) => Some(s.clone()),
                        _ => None,
                    })?;
                let amount = amount_str.parse::<rust_decimal::Decimal>().ok()?;
                Some(crate::acs_worker::AmuletInfo {
                    contract_id: c.contract_id,
                    amount,
                })
            })
            .collect())
    }

    // ========================================================================
    // Two-Phase Transaction RPCs
    // ========================================================================

    /// Prepare a transaction (Phase 1)
    ///
    /// Signs the request with our Ed25519 key and verifies the response signature
    /// from the ledger service.
    pub async fn prepare_transaction(
        &mut self,
        mut req: PrepareTransactionRequest,
    ) -> Result<PrepareTransactionResponse> {
        // Sign request
        let canonical = build_canonical_from_prepare_request(&req)?;
        let sig_data = sign_canonical(&*self.private_key.expose()?, &canonical);
        req.request_signature = Some(MessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        });

        let call = self.client.prepare_transaction(req);
        let response = transport::with_deadline(self.bounds.call, "PrepareTransaction", call).await?;

        // Verify response signature
        let resp_sig = response.response_signature.as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;

        self.verify_server_key(&resp_sig.public_key)?;

        let canonical_resp = canonical_prepare_response(
            &response.transaction_id,
            &response.prepared_transaction_hash,
            &response.command_id,
            &response.prepared_transaction,
            &response.hashing_scheme_version,
        );

        verify_canonical(&self.ledger_service_public_key, &canonical_resp, &resp_sig.signature, &resp_sig.signing_scheme)
            .context("PrepareTransaction response signature verification failed")?;

        // Independently verify the fees_signature, bound to (party,
        // transaction_id, fees_json). Out-of-band from the Canton-payload
        // signature so the multihash signature stays exactly what Canton
        // expects. The transaction_id binding makes the signature a
        // single-use token: the server's SessionStore consumes it on
        // execute, so any replay finds no session.
        let fees_sig = response.fees_signature.as_ref()
            .ok_or_else(|| anyhow!("Missing fees_signature from ledger service"))?;
        self.verify_server_key(&fees_sig.public_key)?;
        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            &response.transaction_id,
            &response.fees_json,
        );
        verify_canonical(&self.ledger_service_public_key, &fees_canonical, &fees_sig.signature, &fees_sig.signing_scheme)
            .context("PrepareTransaction fees_signature verification failed")?;

        Ok(response)
    }

    /// Execute a signed transaction (Phase 2)
    ///
    /// Signs the request with our Ed25519 key and verifies the response signature.
    /// `fees_json` MUST be the exact string echoed from the prepare response —
    /// the server tampering check will reject mismatches.
    pub async fn execute_transaction(
        &mut self,
        transaction_id: &str,
        signature: &str,
        fees_json: &str,
    ) -> Result<ExecuteTransactionResponse> {
        // Sign the (Canton-required) request canonical with the existing
        // request_signature — UNCHANGED so the Canton multihash signature
        // stays untouched.
        let canonical = canonical_execute_request(transaction_id, signature);
        let sig_data = sign_canonical(&*self.private_key.expose()?, &canonical);

        // Independently sign the context-bound fees authorization. Bound to
        // (party, transaction_id, fees_json) — single-use because the
        // server's SessionStore consumes transaction_id atomically.
        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            transaction_id,
            fees_json,
        );
        let fees_auth_data = sign_canonical(&*self.private_key.expose()?, &fees_canonical);

        let call = self.client.execute_transaction(ExecuteTransactionRequest {
            transaction_id: transaction_id.to_string(),
            signature: signature.to_string(),
            fees_json: fees_json.to_string(),
            request_signature: Some(MessageSignature {
                signature: sig_data.signature_b64,
                public_key: sig_data.public_key_b64url,
                signing_scheme: sig_data.signing_scheme,
            }),
            fees_authorization: Some(MessageSignature {
                signature: fees_auth_data.signature_b64,
                public_key: fees_auth_data.public_key_b64url,
                signing_scheme: fees_auth_data.signing_scheme,
            }),
        });
        let response = transport::with_deadline(self.bounds.call, "ExecuteTransaction", call).await?;

        // Verify response signature
        let resp_sig = response.response_signature.as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;

        self.verify_server_key(&resp_sig.public_key)?;

        let canonical_resp = canonical_execute_response(
            response.success,
            &response.update_id,
            response.contract_id.as_deref(),
            response.error_message.as_deref(),
            response.rewards_amount.as_deref(),
            response.rewards_round,
        );

        verify_canonical(&self.ledger_service_public_key, &canonical_resp, &resp_sig.signature, &resp_sig.signing_scheme)
            .context("ExecuteTransaction response signature verification failed")?;

        Ok(response)
    }

    /// Verify the ledger service's response public key matches the configured key
    fn verify_server_key(&self, public_key_b64url: &str) -> Result<()> {
        let key = parse_public_key(public_key_b64url)
            .context("Failed to parse ledger service public key from response")?;

        if key != self.ledger_service_public_key {
            return Err(anyhow!(
                "Ledger service public key mismatch — response key does not match configured LEDGER_SERVICE_PUBLIC_KEY"
            ));
        }
        Ok(())
    }

    /// High-level: prepare, verify, sign, and execute a transaction
    ///
    /// Uses tx-verifier to inspect the PreparedTransaction and compute the hash.
    /// Phase A: Inspector and hasher are stubs — accepts all, signs server hash.
    /// Phase B (future): Full inspection + independent hash recomputation.
    pub async fn submit_transaction(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        verbose: bool,
        dry_run: bool,
        force: bool,
    ) -> Result<ExecuteTransactionResponse> {
        self.submit_with_retries(req, expectation, verbose, dry_run, force, *MAX_RETRIES)
            .await
    }

    async fn submit_with_retries(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        verbose: bool,
        dry_run: bool,
        force: bool,
        max_retries: u32,
    ) -> Result<ExecuteTransactionResponse> {
        let mut uncertain = None;
        self.submit_attempts(req, expectation, verbose, dry_run, force, max_retries, &mut uncertain)
            .await
            .map_err(|e| mark_uncertain(e, uncertain.as_deref()))
    }

    /// The attempt loop; `uncertain` names an execute that failed but may still commit.
    #[allow(clippy::too_many_arguments)]
    async fn submit_attempts(
        &mut self,
        req: PrepareTransactionRequest,
        expectation: &OperationExpectation,
        verbose: bool,
        dry_run: bool,
        force: bool,
        max_retries: u32,
        uncertain: &mut Option<String>,
    ) -> Result<ExecuteTransactionResponse> {
        let budget = SubmitBudget::new(SUBMIT_BUDGET);

        // Record ledger offset before attempting transaction (for update-based recovery)
        let start_offset = match self.get_ledger_end().await {
            Ok(offset) => {
                debug!("Recorded ledger offset before tx: {}", offset);
                offset
            }
            Err(e) => {
                warn!("Failed to get ledger offset, update-based recovery disabled: {}", e);
                0
            }
        };

        for attempt in 0..max_retries {
            let attempt_no = attempt.saturating_add(1);
            let attempt_started = tokio::time::Instant::now();
            // 1. Prepare (fresh contracts each attempt — contracts may become stale).
            //    A prepare failure propagates out of submit_transaction, so signal
            //    the ledger-health breaker here too — otherwise an outage that
            //    manifests at prepare (participant / ledger-service unreachable)
            //    would never trip it and the agent would keep quoting into it.
            let mut prepared = match self.prepare_transaction(req.clone()).await {
                Ok(p) => p,
                Err(e) => {
                    let msg = format!("{:#}", e);
                    let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(&msg);
                    if unreachable {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    // Background archival ops routinely race the counterparty's
                    // archive; an archived-contract refusal there is expected.
                    let label = classify_submit_error(&msg);
                    let background_gone = is_background_op(req.operation)
                        && matches!(
                            label,
                            "CONTRACT_NOT_FOUND" | "INACTIVE_CONTRACTS" | "LOCKED_CONTRACTS"
                        );
                    report_submit_error(
                        if unreachable {
                            "CONNECTION_ERROR"
                        } else if background_gone {
                            label
                        } else {
                            "SERVER_ERROR"
                        },
                        if background_gone { "warning" } else { "error" },
                        "ledger_client.prepare",
                        &self.party_id,
                        None,
                        expectation,
                        req.operation,
                        attempt_no,
                        &msg,
                    );
                    return Err(e);
                }
            };

            info!(
                "Transaction prepared: id={}, command_id={}, traffic={}",
                prepared.transaction_id,
                prepared.command_id,
                prepared.traffic_estimate.as_ref().map(|t| t.total_bytes).unwrap_or(0),
            );

            // 2. Verify transaction and compute hash (the bytes are not needed afterwards)
            let verification = verify_bounded(
                std::mem::take(&mut prepared.prepared_transaction),
                prepared.prepared_transaction_hash.clone(),
                prepared.hashing_scheme_version.clone(),
                expectation.clone(),
                verbose,
            )
            .await?;

            for w in &verification.warnings {
                warn!("TX verification: {}", w);
            }

            if dry_run {
                agent_logic::out!("--- DRY RUN ---");
                agent_logic::out!("Inspection: {}", if verification.accepted { "ACCEPTED" } else { "REJECTED" });
                agent_logic::out!("Summary: {}", verification.summary);
                if let Some(reason) = &verification.rejection_reason {
                    agent_logic::out!("Rejection: {}", reason);
                }
                for w in &verification.warnings {
                    agent_logic::out!("Warning: {}", w);
                }
                let hash_status = if verification.computed_hash == [0u8; 32] {
                    "STUB (using server hash)".to_string()
                } else {
                    let server_hash_bytes = BASE64.decode(&prepared.prepared_transaction_hash).unwrap_or_default();
                    if verification.computed_hash.as_slice() == server_hash_bytes.as_slice() {
                        format!("MATCH ({})", hex::encode(verification.computed_hash))
                    } else {
                        format!(
                            "MISMATCH (server={}, computed={})",
                            hex::encode(&server_hash_bytes),
                            hex::encode(verification.computed_hash)
                        )
                    }
                };
                agent_logic::out!("Hash: {}", hash_status);
                agent_logic::out!("--- NOT SIGNED, NOT EXECUTED ---");
                return Ok(ExecuteTransactionResponse {
                    success: false,
                    update_id: String::new(),
                    contract_id: None,
                    error_message: Some("dry run — not executed".to_string()),
                    traffic: prepared.traffic_estimate,
                    rewards_amount: None,
                    rewards_round: None,
                    response_signature: None,
                    created_contracts: vec![],
                    transaction_status: 0,
                    provider_error: None,
                });
            }

            if !verification.accepted {
                if force {
                    warn!(
                        "Transaction verification REJECTED but --force is set, proceeding: {}",
                        verification.rejection_reason.as_deref().unwrap_or("unknown")
                    );
                } else {
                    let reason = verification.rejection_reason.unwrap_or_default();
                    // A local verifier rejection on the DVP (fund-moving) path
                    // may indicate tampering — critical, mirroring the atomic
                    // path.
                    report_submit_error(
                        "VALIDATION_ERROR",
                        "critical",
                        "ledger_client.verify",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        req.operation,
                        attempt_no,
                        &format!("Transaction verification REJECTED: {reason}"),
                    );
                    anyhow::bail!("Transaction verification REJECTED: {}", reason);
                }
            }

            // 3. Determine which hash to sign
            let hash_to_sign = if verification.computed_hash == [0u8; 32] {
                // Phase A: hasher returned stub — use server hash (temporarily trusted)
                warn!("Phase A: signing server-provided hash (verification stub active)");
                BASE64
                    .decode(&prepared.prepared_transaction_hash)
                    .context("Failed to decode prepared_transaction_hash")?
            } else {
                // Phase B: sign our independently computed hash
                verification.computed_hash.to_vec()
            };

            let signature = sign_hash_bytes(&*self.private_key.expose()?, &hash_to_sign)?;

            // 4. Execute (catch gRPC errors for update-based recovery).
            //    Echo back the server's fees_json verbatim — the server
            //    tampering check rejects substitutions.
            let result = match self
                .execute_transaction(&prepared.transaction_id, &signature, &prepared.fees_json)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    // gRPC/network error — transaction may have succeeded
                    let recovery = self
                        .recover_after_execute_error(
                            e,
                            &prepared.command_id,
                            start_offset,
                            RECOVERY_SCAN_DELAY,
                            expectation,
                            req.operation,
                            attempt_no,
                        )
                        .await;
                    let e = match recovery {
                        ExecuteRecovery::Recovered(recovered) => return Ok(*recovered),
                        ExecuteRecovery::Stop(err) => return Err(err),
                        ExecuteRecovery::Retry(e) => e,
                    };
                    // Not seen on the ledger yet is no proof it never commits
                    if uncertain.is_none() {
                        *uncertain = Some(prepared.command_id.clone());
                    }
                    let delay_ms = backoff_ms(attempt);
                    let wait = Duration::from_millis(delay_ms);
                    if budget.allows_retry(attempt_no, max_retries, wait, attempt_started.elapsed(), &prepared.command_id) {
                        warn!(
                            "Retrying after execute error (attempt {}/{}): {:#} — in {}ms [{}]",
                            attempt_no, max_retries, e, delay_ms, prepared.command_id
                        );
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    // Terminal failure for this submission (retries exhausted). Signal
                    // the ledger-health breaker once per submit_transaction call, only
                    // for true sequencer-unreachable errors (not business rejections).
                    let msg = format!("{:#}", e);
                    let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(&msg);
                    if unreachable {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    report_submit_error(
                        if unreachable { "CONNECTION_ERROR" } else { "SERVER_ERROR" },
                        "error",
                        "ledger_client.execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        req.operation,
                        attempt_no,
                        &msg,
                    );
                    return Err(e);
                }
            };

            if !result.success {
                let error_msg = result.error_message.as_deref().unwrap_or("unknown error");

                // SEQUENCER_BACKPRESSURE: signal fee pause regardless of retry outcome
                if error_msg.contains("SEQUENCER_BACKPRESSURE") {
                    warn!(
                        "SEQUENCER_BACKPRESSURE detected (attempt {}/{}), pausing fees {}s / background {}s [{}]",
                        attempt_no, max_retries, *FEE_PAUSE_SECS, *BACKGROUND_PAUSE_SECS, prepared.command_id
                    );
                    signal_sequencer_backpressure();

                    // Background housekeeping gives up on the first push-back
                    // instead of spending MAX_RETRIES more prepare/execute
                    // round-trips on a sequencer that just said it is full. The
                    // error text (SEQUENCER_BACKPRESSURE) reaches the caller
                    // intact so it can classify this as "try later", not a fault.
                    if is_background_op(req.operation) {
                        report_submit_error(
                            "SEQUENCER_BACKPRESSURE",
                            "warning",
                            "ledger_client.execute",
                            &self.party_id,
                            Some(&prepared.command_id),
                            expectation,
                            req.operation,
                            attempt_no,
                            error_msg,
                        );
                        anyhow::bail!("Transaction failed: {}", error_msg);
                    }
                }

                // DUPLICATE_COMMAND: check ledger updates before giving up
                if error_msg.contains("DUPLICATE_COMMAND") {
                    if start_offset > 0 {
                        tokio::time::sleep(RECOVERY_SCAN_DELAY).await;
                        match self.find_transaction_in_updates(&prepared.command_id, start_offset).await {
                            UpdateScan::Found(recovered) => {
                                info!("DUPLICATE_COMMAND recovered via ledger updates: command_id={}, update_id={}",
                                    prepared.command_id, recovered.update_id);
                                agent_logic::ledger_health::record_submit_success();
                                return Ok(*recovered);
                            }
                            UpdateScan::NotFound => {}
                            UpdateScan::Unknown(why) => {
                                warn!("DUPLICATE_COMMAND: ledger scan incomplete ({why}) [{}]", prepared.command_id);
                            }
                        }
                    }
                    // The command was accepted by the ledger (duplicate) — ledger is up.
                    agent_logic::ledger_health::record_submit_success();
                    report_submit_error(
                        "DUPLICATE_COMMAND",
                        "warning",
                        "ledger_client.execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        req.operation,
                        attempt_no,
                        error_msg,
                    );
                    anyhow::bail!("Command already submitted (DUPLICATE_COMMAND): {}", error_msg);
                }

                // INACTIVE_CONTRACTS: contract was consumed/archived between
                // prepare and execute. Re-prepare to get fresh CIDs from ACS.
                if error_msg.contains("INACTIVE_CONTRACTS") {
                    // Background archival ops pin their contract id, so a
                    // re-prepare cannot help — give up on the first failure.
                    if is_background_op(req.operation) {
                        report_submit_error(
                            "INACTIVE_CONTRACTS",
                            "warning",
                            "ledger_client.execute",
                            &self.party_id,
                            Some(&prepared.command_id),
                            expectation,
                            req.operation,
                            attempt_no,
                            error_msg,
                        );
                        anyhow::bail!("Transaction failed: {}", error_msg);
                    }
                    if budget.allows_retry(
                        attempt_no,
                        max_retries,
                        INACTIVE_RETRY_DELAY,
                        attempt_started.elapsed(),
                        &prepared.command_id,
                    ) {
                        warn!(
                            "INACTIVE_CONTRACTS (attempt {}/{}), re-preparing with fresh CIDs in 2s [{}]",
                            attempt_no, max_retries, prepared.command_id
                        );
                        tokio::time::sleep(INACTIVE_RETRY_DELAY).await;
                        continue;
                    }
                    report_submit_error(
                        "INACTIVE_CONTRACTS",
                        "error",
                        "ledger_client.execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        req.operation,
                        attempt_no,
                        error_msg,
                    );
                    anyhow::bail!("INACTIVE_CONTRACTS after {} attempts: {}", attempt_no, error_msg);
                }

                // A locked contract is being consumed by a competing transaction;
                // background archival ops give up rather than re-contend.
                if error_msg.contains("LOCKED_CONTRACTS") && is_background_op(req.operation) {
                    report_submit_error(
                        "LOCKED_CONTRACTS",
                        "warning",
                        "ledger_client.execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        req.operation,
                        attempt_no,
                        error_msg,
                    );
                    anyhow::bail!("Transaction failed: {}", error_msg);
                }

                // The participant may have taken it: look on the ledger before any retry
                if !server_failure_is_definite(error_msg) {
                    let recovery = self
                        .recover_after_execute_error(
                            anyhow!("{error_msg}"),
                            &prepared.command_id,
                            start_offset,
                            RECOVERY_SCAN_DELAY,
                            expectation,
                            req.operation,
                            attempt_no,
                        )
                        .await;
                    match recovery {
                        ExecuteRecovery::Recovered(recovered) => return Ok(*recovered),
                        ExecuteRecovery::Stop(err) => return Err(err),
                        ExecuteRecovery::Retry(_) if uncertain.is_none() => {
                            *uncertain = Some(prepared.command_id.clone());
                        }
                        ExecuteRecovery::Retry(_) => {}
                    }
                }

                let delay_ms = backoff_ms(attempt);
                let wait = Duration::from_millis(delay_ms);
                if budget.allows_retry(attempt_no, max_retries, wait, attempt_started.elapsed(), &prepared.command_id) {
                    warn!(
                        "Transaction error (attempt {}/{}): {} — retrying in {}ms [{}]",
                        attempt_no, max_retries, error_msg, delay_ms, prepared.command_id
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                // Terminal failure for this submission (retries exhausted). Signal the
                // ledger-health breaker once per call, only for sequencer-unreachable
                // errors (SEQUENCER_BACKPRESSURE already handled above via its own pause).
                let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(error_msg);
                if unreachable {
                    agent_logic::ledger_health::record_submit_failure();
                }
                report_submit_error(
                    classify_submit_error(error_msg),
                    "error",
                    "ledger_client.execute",
                    &self.party_id,
                    Some(&prepared.command_id),
                    expectation,
                    req.operation,
                    attempt_no,
                    error_msg,
                );
                anyhow::bail!("Transaction failed: {}", error_msg);
            }

            // Successful submission — the ledger is reachable; clear the
            // ledger-health breaker so RFQ quoting resumes.
            agent_logic::ledger_health::record_submit_success();

            // Post-success: nudge the auto-topup runner. Non-blocking;
            // the runner has its own task and its own client, so this
            // never recurses or blocks the caller.
            if let Some(trigger) = self.topup_trigger.as_ref() {
                trigger.nudge();
            }

            return Ok(result);
        }

        Err(anyhow!("Transaction submit: retry loop exhausted after {max_retries} attempt(s)"))
    }

    /// After an execute RPC error, check the ledger (when recovery is on) for the command.
    #[allow(clippy::too_many_arguments)]
    async fn recover_after_execute_error(
        &mut self,
        e: anyhow::Error,
        command_id: &str,
        start_offset: i64,
        scan_delay: Duration,
        expectation: &OperationExpectation,
        operation: i32,
        attempt_no: u32,
    ) -> ExecuteRecovery {
        if start_offset <= 0 {
            return ExecuteRecovery::Retry(e);
        }
        warn!("Execute error: {:#} — checking ledger updates", e);
        tokio::time::sleep(scan_delay).await;
        match self.find_transaction_in_updates(command_id, start_offset).await {
            UpdateScan::Found(recovered) => {
                info!("Transaction recovered via ledger updates: command_id={}, update_id={}",
                    command_id, recovered.update_id);
                // The tx landed despite the gRPC error — ledger is reachable.
                agent_logic::ledger_health::record_submit_success();
                ExecuteRecovery::Recovered(recovered)
            }
            UpdateScan::NotFound => ExecuteRecovery::Retry(e),
            UpdateScan::Unknown(why) => {
                let msg = format!("{e:#}");
                let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(&msg);
                if unreachable {
                    agent_logic::ledger_health::record_submit_failure();
                }
                let detail =
                    format!("{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete ({why}); not retried: {msg}");
                report_submit_error(
                    if unreachable { "CONNECTION_ERROR" } else { "SERVER_ERROR" },
                    "error",
                    "ledger_client.execute",
                    &self.party_id,
                    Some(command_id),
                    expectation,
                    operation,
                    attempt_no,
                    &detail,
                );
                ExecuteRecovery::Stop(anyhow!("{detail}"))
            }
        }
    }

    /// Search ledger updates for a transaction with the given command_id.
    /// Follows the pattern from dvp_settle.rs find_dvp_settle_update().
    async fn find_transaction_in_updates(
        &mut self,
        command_id: &str,
        start_offset: i64,
    ) -> UpdateScan {
        let current_end = match self.get_ledger_end().await {
            Ok(end) => end,
            Err(e) => return UpdateScan::Unknown(format!("ledger end unavailable: {e:#}")),
        };
        if current_end <= start_offset {
            return UpdateScan::NotFound;
        }

        match self.get_updates(start_offset, Some(current_end), &[]).await {
            Ok((updates, complete)) => scan_updates(&updates, complete, command_id),
            Err(e) => UpdateScan::Unknown(format!("updates scan failed: {e:#}")),
        }
    }

    /// Request tokens from the faucet
    pub async fn request_faucet(&mut self, mut req: FaucetRequest) -> Result<FaucetResponse> {
        // Sign the request
        let canonical = canonical_params_faucet(
            &req.token_name, &req.token_admin, &req.ticket, req.dry_run,
        );
        let canonical_bytes = canonical_prepare_request(0, &canonical);
        let sig_data = sign_canonical(&*self.private_key.expose()?, &canonical_bytes);
        req.request_signature = Some(MessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        });

        let call = self.client.request_faucet(req);
        transport::with_deadline(self.bounds.call, "RequestFaucet", call).await
    }

    /// List instruments supported by the faucet (drives both faucet calls and
    /// CIP-56 preapproval creation on the agent side).
    pub async fn list_faucet_instruments(&mut self) -> Result<Vec<FaucetInstrument>> {
        let call = self.client.list_faucet_instruments(ListFaucetInstrumentsRequest {});
        Ok(transport::with_deadline(self.bounds.call, "ListFaucetInstruments", call).await?.instruments)
    }

    // ========================================================================
    // Off-chain processing-fee payment (DVP and allocation processing fees).
    //
    // Two-phase flow mirrors PrepareTransaction / ExecuteTransaction. Each
    // call pays exactly ONE fee (DVP or allocation). The two are independent —
    // settlement.rs invokes `pay_processing_fee("dvp", ...)` and
    // `pay_processing_fee("allocate", ...)` at separate steps, and the
    // allocate call only happens if the cloud-agent reaches the Allocate
    // step (i.e. the counterparty has allocated).
    // ========================================================================

    /// Phase 1: ask the server to compute the processing fee in CC and
    /// return a signed schedule + a single-use `session_id`. Verifies BOTH
    /// `response_signature` (full canonical including session_id) and
    /// `fees_signature` (context-bound canonical_pay_fee_authorization
    /// covering party + session_id + proposal_id + fee_type + fees_json).
    pub async fn prepare_pay_fee(
        &mut self,
        proposal_id: &str,
        fee_type: &str,
    ) -> Result<PreparePayFeeResponse> {
        // Sign the request canonical.
        let canonical = message_signing::canonical_prepare_pay_fee_request(proposal_id, fee_type);
        let sig_data = message_signing::sign_canonical(&*self.private_key.expose()?, &canonical);

        let call = self.client.prepare_pay_fee(PreparePayFeeRequest {
            proposal_id: proposal_id.to_string(),
            fee_type: fee_type.to_string(),
            request_signature: Some(MessageSignature {
                signature: sig_data.signature_b64,
                public_key: sig_data.public_key_b64url,
                signing_scheme: sig_data.signing_scheme,
            }),
        });
        let response = transport::with_deadline(self.bounds.call, "PreparePayFee", call).await?;

        // Verify response_signature over the full canonical (includes session_id).
        let resp_sig = response
            .response_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;
        self.verify_server_key(&resp_sig.public_key)?;
        let canonical_resp = message_signing::canonical_prepare_pay_fee_response(
            &response.proposal_id,
            &response.fee_type,
            &response.fees_json,
            &response.session_id,
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &canonical_resp,
            &resp_sig.signature,
            &resp_sig.signing_scheme,
        )
        .context("PreparePayFee response signature verification failed")?;

        // Independently verify fees_signature over the context-bound payload.
        // Bound to: party + session_id + proposal_id + fee_type + fees_json.
        let fees_sig = response
            .fees_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing fees_signature from ledger service"))?;
        self.verify_server_key(&fees_sig.public_key)?;
        let fees_canonical = message_signing::canonical_pay_fee_authorization(
            &self.party_id,
            &response.session_id,
            &response.proposal_id,
            &response.fee_type,
            &response.fees_json,
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &fees_canonical,
            &fees_sig.signature,
            &fees_sig.signing_scheme,
        )
        .context("PreparePayFee fees_signature verification failed")?;

        Ok(response)
    }

    /// Phase 2: echo the schedule back. Both signatures cover the full
    /// context (party + session_id + proposal_id + fee_type + fees_json),
    /// pinning the authorization to the single-use server-issued session.
    pub async fn execute_pay_fee(
        &mut self,
        proposal_id: &str,
        fee_type: &str,
        fees_json: &str,
        session_id: &str,
    ) -> Result<ExecutePayFeeResponse> {
        let canonical = message_signing::canonical_execute_pay_fee_request(
            proposal_id, fee_type, fees_json, session_id,
        );
        let sig_data = message_signing::sign_canonical(&*self.private_key.expose()?, &canonical);

        let fees_canonical = message_signing::canonical_pay_fee_authorization(
            &self.party_id,
            session_id,
            proposal_id,
            fee_type,
            fees_json,
        );
        let fees_auth_data =
            message_signing::sign_canonical(&*self.private_key.expose()?, &fees_canonical);

        let call = self.client.execute_pay_fee(ExecutePayFeeRequest {
            proposal_id: proposal_id.to_string(),
            fee_type: fee_type.to_string(),
            fees_json: fees_json.to_string(),
            session_id: session_id.to_string(),
            request_signature: Some(MessageSignature {
                signature: sig_data.signature_b64,
                public_key: sig_data.public_key_b64url,
                signing_scheme: sig_data.signing_scheme,
            }),
            fees_authorization: Some(MessageSignature {
                signature: fees_auth_data.signature_b64,
                public_key: fees_auth_data.public_key_b64url,
                signing_scheme: fees_auth_data.signing_scheme,
            }),
        });
        let response = transport::with_deadline(self.bounds.call, "ExecutePayFee", call).await?;

        // Verify response_signature.
        let resp_sig = response
            .response_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;
        self.verify_server_key(&resp_sig.public_key)?;
        let canonical_resp = message_signing::canonical_execute_pay_fee_response(
            response.success,
            response.error_message.as_deref(),
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &canonical_resp,
            &resp_sig.signature,
            &resp_sig.signing_scheme,
        )
        .context("ExecutePayFee response signature verification failed")?;

        Ok(response)
    }

    /// Convenience wrapper: prepare → verify → authorize → execute.
    /// `fee_type` is "dvp" or "allocate".
    pub async fn pay_processing_fee(
        &mut self,
        proposal_id: &str,
        fee_type: &str,
    ) -> Result<()> {
        let prep = self.prepare_pay_fee(proposal_id, fee_type).await?;
        let exec = self
            .execute_pay_fee(proposal_id, fee_type, &prep.fees_json, &prep.session_id)
            .await?;
        if !exec.success {
            return Err(anyhow!(
                "ExecutePayFee returned failure for proposal {} fee_type {}: {}",
                proposal_id, fee_type,
                exec.error_message.unwrap_or_else(|| "<no error message>".to_string())
            ));
        }
        Ok(())
    }
}

/// Sign a hash with Ed25519 and return base64-encoded signature
fn sign_hash_bytes(private_key_bytes: &[u8; 32], hash_bytes: &[u8]) -> Result<String> {
    let signing_key = SigningKey::from_bytes(private_key_bytes);
    let signature = signing_key.sign(hash_bytes);
    Ok(BASE64.encode(signature.to_bytes()))
}

/// Result of scanning ledger updates for one command.
#[derive(Debug)]
enum UpdateScan {
    Found(Box<ExecuteTransactionResponse>),
    NotFound,
    /// The scan did not cover the whole range, so absence proves nothing.
    Unknown(String),
}

/// What to do after an execute RPC error.
#[derive(Debug)]
enum ExecuteRecovery {
    Recovered(Box<ExecuteTransactionResponse>),
    /// The command is not on the ledger, or recovery is off; a retry is allowed.
    Retry(anyhow::Error),
    /// The outcome is unknown; a retry could execute the command twice.
    Stop(anyhow::Error),
}

/// Look for `command_id` in `updates`; a miss in an incomplete scan is `Unknown`.
fn scan_updates(updates: &[GetUpdatesResponse], complete: bool, command_id: &str) -> UpdateScan {
    for update_resp in updates {
        if let Some(get_updates_response::Update::Transaction(tx)) = &update_resp.update {
            if tx.command_id == command_id {
                // Found the transaction — extract contract_id from created events
                let contract_id = tx.events.iter().find_map(|event| {
                    if let Some(ledger_event::Event::Created(created)) = &event.event {
                        Some(created.contract_id.clone())
                    } else {
                        None
                    }
                });

                if contract_id.is_none() {
                    warn!(
                        "Found tx by command_id={} but no created events: update_id={}, events={:?}",
                        command_id, tx.update_id, tx.events
                    );
                }

                return UpdateScan::Found(Box::new(ExecuteTransactionResponse {
                    success: true,
                    update_id: tx.update_id.clone(),
                    contract_id,
                    error_message: None,
                    traffic: None,
                    rewards_amount: None,
                    rewards_round: None,
                    response_signature: None,
                    created_contracts: vec![],
                    transaction_status: 0,
                    provider_error: None,
                }));
            }
        }
    }

    if complete {
        UpdateScan::NotFound
    } else {
        UpdateScan::Unknown(format!("updates scan incomplete after {} update(s)", updates.len()))
    }
}

/// `tx_verifier::verify_and_hash` on the blocking pool, bounded by `VERIFY_TIMEOUT`.
async fn verify_bounded(
    prepared_transaction: Vec<u8>,
    server_hash_base64: String,
    hashing_scheme_version: String,
    expectation: OperationExpectation,
    verbose: bool,
) -> Result<tx_verifier::VerificationResult> {
    run_blocking_bounded(VERIFY_TIMEOUT, "Transaction verification", move || {
        tx_verifier::verify_and_hash(
            &prepared_transaction,
            &server_hash_base64,
            &hashing_scheme_version,
            &expectation,
            verbose,
        )
    })
    .await
}

/// Run CPU-bound `work` off the async workers; a panic or overrun becomes an error.
async fn run_blocking_bounded<T, F>(limit: Duration, what: &str, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|e| anyhow!("{what} needs a tokio runtime: {e}"))?;
    match tokio::time::timeout(limit, runtime.spawn_blocking(work)).await {
        Ok(Ok(result)) => result,
        Ok(Err(join)) => Err(anyhow!("{what} failed: {join}")),
        Err(_) => Err(anyhow!("{what} timed out after {limit:?}")),
    }
}

/// Wall-clock micros for deadline checks; an unreadable clock reads as the far future.
fn now_micros_fail_closed() -> i64 {
    micros_or_max(clock::unix_now())
}

fn micros_or_max(since_epoch: Option<Duration>) -> i64 {
    since_epoch
        .and_then(|d| i64::try_from(d.as_micros()).ok())
        .unwrap_or(i64::MAX)
}

// ============================================================================
// RFQ V2 (AtomicDVP) — AtomicDvpProviderService client
// ============================================================================

use orderbook_proto::rfqv2::{
    atomic_dvp_provider_service_client::AtomicDvpProviderServiceClient,
    prepare_atomic_transaction_request::Params as AtomicParams,
    AtomicMessageSignature, ExecuteAtomicTransactionResponse as AtomicExecuteResponse,
    ExecuteAtomicTransactionRequest, GetAtomicContractsRequest, GetAtomicContractsResponse,
    PrepareAtomicTransactionRequest, PrepareAtomicTransactionResponse,
};

/// Marker prefix for ambiguous execute failures (the settle MAY have landed on
/// ledger despite the error). Callers MUST reconcile via ledger state before
/// re-quoting — see [`is_ambiguous_execute_error`].
pub const ATOMIC_EXECUTE_AMBIGUOUS: &str = "ATOMIC_EXECUTE_AMBIGUOUS";

/// Marker for a [`DAppProviderClient::submit_transaction`] execute whose outcome
/// the ledger scan could not settle; such a submit is not retried.
pub const EXECUTE_OUTCOME_UNKNOWN: &str = "EXECUTE_OUTCOME_UNKNOWN";

/// True when an error from [`AtomicProviderClient::submit_atomic_transaction`]
/// means the transaction may have committed (recovery scan unavailable) — the
/// caller must reconcile before retrying with different inputs.
/// Also true for [`EXECUTE_OUTCOME_UNKNOWN`] errors.
pub fn is_ambiguous_execute_error(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains(ATOMIC_EXECUTE_AMBIGUOUS) || text.contains(EXECUTE_OUTCOME_UNKNOWN)
}

/// Whether an execute failure the ledger service reported proves nothing was
/// submitted or committed; unknown text may have reached the participant.
fn server_failure_is_definite(msg: &str) -> bool {
    // An in-flight duplicate may still commit
    if msg.contains("SUBMISSION_ALREADY_IN_FLIGHT") {
        return false;
    }
    const PRE_SUBMIT: [&str; 4] = [
        "Transaction session ",
        "fees_json mismatch",
        "Invalid party ID format",
        "Missing offset in ledger-end response",
    ];
    if msg.starts_with("Transaction failed:")
        || PRE_SUBMIT.iter().any(|p| msg.starts_with(p))
        || msg.contains("/state/ledger-end")
    {
        return true;
    }
    match msg.strip_prefix("Execute submission failed (HTTP ") {
        // A timeout status says nothing about the submission
        Some(rest) if rest.starts_with('4') => !rest.starts_with("408"),
        Some(rest) if rest.starts_with('5') => {
            ["BACKPRESSURE", "INACTIVE_CONTRACTS", "LOCKED_CONTRACTS"].iter().any(|code| rest.contains(code))
        }
        _ => false,
    }
}

/// A submit failure after an earlier execute `prior` that may still commit is
/// itself of unknown outcome.
pub(crate) fn mark_uncertain(e: anyhow::Error, prior: Option<&str>) -> anyhow::Error {
    match prior {
        Some(cmd) if !is_ambiguous_execute_error(&e) => {
            anyhow!("{EXECUTE_OUTCOME_UNKNOWN}: earlier execute {cmd} may still commit: {e:#}")
        }
        _ => e,
    }
}

/// Marker for "the signed quote window closed before we could retry". NOT
/// ambiguous: the execute was rejected, so nothing committed and the caller can
/// re-quote immediately. Distinct from a bare deadline-exceeded DAML_FAILURE,
/// which is what we get if we re-submit into a dead window instead of stopping.
pub const QUOTE_WINDOW_CLOSED: &str = "QUOTE_WINDOW_CLOSED";

/// The settlement proposal an operation belongs to, if any — context for
/// error reports.
fn expectation_proposal_id(e: &OperationExpectation) -> Option<&str> {
    match e {
        OperationExpectation::PayFee { proposal_id, .. }
        | OperationExpectation::ProposeDvp { proposal_id, .. }
        | OperationExpectation::AcceptDvp { proposal_id, .. }
        | OperationExpectation::Allocate { proposal_id, .. } => Some(proposal_id),
        _ => None,
    }
}

/// The RFQ quote id for atomic settles — the only business correlator an
/// AtomicDVP carries (no settlement_proposal_id). Used as `order_id`.
fn expectation_quote_id(e: &OperationExpectation) -> Option<&str> {
    match e {
        OperationExpectation::AtomicDvpSettle { quote_id, .. } => Some(quote_id),
        _ => None,
    }
}

/// Classify a terminal error message against the shared vocabulary (the same
/// labels the rpc side whitelists in ERROR_TYPE_VOCAB). Substring match, most
/// specific first; falls back to UNKNOWN. Replaces the binary
/// unreachable/UNKNOWN choice so congestion/timeout/precondition failures are
/// not all logged as UNKNOWN.
fn classify_submit_error(msg: &str) -> &'static str {
    const PATTERNS: &[(&str, &str)] = &[
        ("SEQUENCER_NOT_ENOUGH_TRAFFIC_CREDIT", "TRAFFIC_CREDIT_EXHAUSTED"),
        ("SEQUENCER_BACKPRESSURE", "SEQUENCER_BACKPRESSURE"),
        ("LOCAL_VERDICT_INACTIVE_CONTRACTS", "INACTIVE_CONTRACTS"),
        ("INACTIVE_CONTRACTS", "INACTIVE_CONTRACTS"),
        ("LOCAL_VERDICT_LOCKED_CONTRACTS", "LOCKED_CONTRACTS"),
        ("LOCKED_CONTRACTS", "LOCKED_CONTRACTS"),
        ("DUPLICATE_COMMAND", "DUPLICATE_COMMAND"),
        ("MEDIATOR_SAYS_TX_TIMED_OUT", "MEDIATOR_TX_TIMED_OUT"),
        ("SUBMISSION_ALREADY_IN_FLIGHT", "ALREADY_IN_FLIGHT"),
        ("CONTRACT_NOT_FOUND", "CONTRACT_NOT_FOUND"),
        ("UNKNOWN_CONTRACT_SYNCHRONIZERS", "CONTRACT_NOT_FOUND"),
        ("LOCAL_VERDICT_MALFORMED", "MALFORMED_TRANSACTION"),
        ("SEQUENCER_REQUEST_REFUSED", "SEQUENCER_REQUEST_REFUSED"),
        ("SEQUENCER_REQUEST_FAILED", "SEQUENCER_REQUEST_FAILED"),
        ("PreconditionFailed", "PRECONDITION_FAILED"),
        ("COMPLETION_TIMEOUT", "COMPLETION_TIMEOUT"),
    ];
    for (needle, label) in PATTERNS {
        if msg.contains(needle) {
            return label;
        }
    }
    if agent_logic::ledger_health::is_sequencer_unreachable(msg) {
        "SEQUENCER_REQUEST_FAILED"
    } else {
        "UNKNOWN"
    }
}

/// Best-effort structured error report for a terminal submission failure
/// (ReportErrors -> orderbook-rpc via the global `error_reporter`). Sync
/// try_send; never affects the calling flow; no-op if the reporter was never
/// initialized. `attempts` is the number of submission attempts made (1-based
/// at a terminal arm); the operation enum + attempts land in metadata for the
/// §11 retry-exhaustion queries.
#[allow(clippy::too_many_arguments)]
fn report_submit_error(
    error_type: &str,
    severity: &str,
    module: &str,
    party_id: &str,
    command_id: Option<&str>,
    expectation: &OperationExpectation,
    operation: i32,
    attempts: u32,
    message: &str,
) {
    let mut b = agent_logic::error_reporter::ErrorEventBuilder::new(error_type, message)
        .severity(severity)
        .module(module)
        .party(party_id)
        .metadata(serde_json::json!({
            "operation": operation,
            "attempts": attempts,
        }));
    if let Some(cid) = command_id {
        b = b.command_id(cid);
    }
    if let Some(pid) = expectation_proposal_id(expectation) {
        b = b.settlement_proposal_id(pid);
    }
    if let Some(qid) = expectation_quote_id(expectation) {
        b = b.order_id(qid);
    }
    b.send();
}

/// The signed deadline an operation must settle within, if it has one. Only an
/// AtomicDVP settle carries one — every other operation is deadline-free, so
/// `None` leaves the retry loop's behavior unchanged.
fn quote_deadline_of(expectation: &OperationExpectation) -> Option<i64> {
    match expectation {
        OperationExpectation::AtomicDvpSettle {
            valid_until_micros, ..
        } => Some(*valid_until_micros),
        _ => None,
    }
}

/// Whether re-preparing is pointless because the signed window no longer has
/// room for a full prepare→sign→execute round. Reuses the taker-side pre-check
/// margin so the two agree on what "enough time left" means.
fn quote_window_closed(deadline_micros: Option<i64>, now_micros: i64) -> bool {
    match deadline_micros {
        Some(valid_until) => {
            now_micros.saturating_add(crate::atomic_swap::PRECHECK_VALIDITY_MARGIN_MICROS) >= valid_until
        }
        None => false,
    }
}

/// Build the canonical payload for a PrepareAtomicTransactionRequest.
/// V2 operation identity = the params oneof arm (no operation int); field
/// order per arm matches the message-signing canonical builders exactly —
/// the ledger-service verify arms must use the same builders.
pub fn build_canonical_from_prepare_atomic_request(
    req: &PrepareAtomicTransactionRequest,
) -> Result<Vec<u8>> {
    let params = req
        .params
        .as_ref()
        .context("PrepareAtomicTransactionRequest missing params")?;
    let params_canonical = match params {
        AtomicParams::AtomicDvpSettle(p) => {
            let quote = p
                .quote
                .as_ref()
                .context("AtomicDvpSettleParams missing quote")?;
            let lp_fees: Vec<message_signing::AtomicLpFee<'_>> = p
                .lp_fees
                .iter()
                .map(|f| message_signing::AtomicLpFee {
                    receiver: &f.receiver,
                    instrument_admin: &f.instrument_admin,
                    instrument_id: &f.instrument_id,
                    amount: &f.amount,
                })
                .collect();
            message_signing::canonical_params_atomic_dvp_settle(
                &p.venue_cid,
                &quote.quote_id,
                &quote.ticket_id,
                &quote.user,
                &quote.side,
                &quote.base_amount,
                &quote.quote_amount,
                quote.created_at_micros,
                quote.valid_until_micros,
                &p.quote_signature_der_hex,
                p.ticket_cid.as_deref(),
                &p.lp_input_holding_cids,
                &p.user_input_holding_cids,
                &lp_fees,
            )
        }
        AtomicParams::IssueTickets(p) => {
            message_signing::canonical_params_issue_tickets(&p.ticket_ids)
        }
        AtomicParams::SplitHoldings(p) => {
            let splits: Vec<(String, u32)> =
                p.splits.iter().map(|s| (s.amount.clone(), s.count)).collect();
            message_signing::canonical_params_split_holdings(
                &p.instrument_id,
                &p.instrument_admin,
                &splits,
                &p.input_holding_cids,
            )
        }
        AtomicParams::CreateAtomicDvpVenue(p) => {
            message_signing::canonical_params_create_atomic_dvp_venue(
                &p.pair_name,
                &p.base_instrument_id,
                &p.base_instrument_admin,
                &p.quote_instrument_id,
                &p.quote_instrument_admin,
                &p.quote_public_key_spki_hex,
            )
        }
        AtomicParams::UpdateVenueKey(p) => message_signing::canonical_params_update_venue_key(
            &p.venue_cid,
            &p.new_quote_public_key_spki_hex,
        ),
        AtomicParams::RetireVenue(p) => {
            message_signing::canonical_params_retire_venue(&p.venue_cid)
        }
        AtomicParams::CancelTickets(p) => {
            message_signing::canonical_params_cancel_tickets(&p.ticket_cids)
        }
    };
    Ok(message_signing::canonical_prepare_atomic_request(&params_canonical))
}

/// Client for the AtomicDvpProviderService gRPC API (RFQ V2 / AtomicDVP).
/// Twin of [`DAppProviderClient`] — same auth, signing, and verification
/// machinery over the V2-isolated service.
pub struct AtomicProviderClient {
    client: AtomicDvpProviderServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    bounds: CallBounds,
    party_id: String,
    private_key: Secret<32>,
    ledger_service_public_key: [u8; 32],
}

impl AtomicProviderClient {
    /// Create a new AtomicProviderClient (same 9-arg shape as DAppProviderClient::new)
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        grpc_url: &str,
        party_id: &str,
        role: &str,
        private_key: &Secret<32>,
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
        connection_timeout_secs: Option<u64>,
        request_timeout_secs: Option<u64>,
    ) -> Result<Self> {
        let channel = DAppProviderClient::create_channel(
            grpc_url,
            connection_timeout_secs,
            request_timeout_secs,
        )
        .await?;
        Self::with_channel(
            channel,
            party_id,
            role,
            private_key,
            ttl_secs,
            node_name,
            ledger_service_public_key,
            CallBounds::for_request_secs(request_timeout_secs),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_channel(
        channel: Channel,
        party_id: &str,
        role: &str,
        private_key: &Secret<32>,
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
        bounds: CallBounds,
    ) -> Result<Self> {
        let interceptor = AuthInterceptor::for_party(party_id, role, private_key, ttl_secs, node_name)?;
        let client = AtomicDvpProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);
        Ok(Self {
            client,
            bounds,
            party_id: party_id.to_string(),
            private_key: private_key.clone(),
            ledger_service_public_key: *ledger_service_public_key,
        })
    }

    fn verify_server_key(&self, public_key_b64url: &str) -> Result<()> {
        let key = parse_public_key(public_key_b64url)
            .context("Failed to parse ledger service public key from response")?;
        if key != self.ledger_service_public_key {
            return Err(anyhow!(
                "Ledger service public key mismatch — response key does not match configured LEDGER_SERVICE_PUBLIC_KEY"
            ));
        }
        Ok(())
    }

    fn sign_as_atomic(&self, canonical: &[u8]) -> Result<AtomicMessageSignature> {
        let sig_data = sign_canonical(&*self.private_key.expose()?, canonical);
        Ok(AtomicMessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        })
    }

    /// Prepare an atomic transaction (Phase 1): sign the request, verify the
    /// response + fees signatures.
    pub async fn prepare_atomic(
        &mut self,
        mut req: PrepareAtomicTransactionRequest,
    ) -> Result<PrepareAtomicTransactionResponse> {
        let canonical = build_canonical_from_prepare_atomic_request(&req)?;
        req.request_signature = Some(self.sign_as_atomic(&canonical)?);

        let call = self.client.prepare_atomic_transaction(req);
        let response =
            transport::with_deadline(self.bounds.call, "PrepareAtomicTransaction", call).await?;

        let resp_sig = response
            .response_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;
        self.verify_server_key(&resp_sig.public_key)?;
        let canonical_resp = canonical_prepare_response(
            &response.transaction_id,
            &response.prepared_transaction_hash,
            &response.command_id,
            &response.prepared_transaction,
            &response.hashing_scheme_version,
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &canonical_resp,
            &resp_sig.signature,
            &resp_sig.signing_scheme,
        )
        .context("PrepareAtomicTransaction response signature verification failed")?;

        let fees_sig = response
            .fees_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing fees_signature from ledger service"))?;
        self.verify_server_key(&fees_sig.public_key)?;
        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            &response.transaction_id,
            &response.fees_json,
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &fees_canonical,
            &fees_sig.signature,
            &fees_sig.signing_scheme,
        )
        .context("PrepareAtomicTransaction fees_signature verification failed")?;

        Ok(response)
    }

    /// Execute a signed atomic transaction (Phase 2). `fees_json` MUST be the
    /// exact string echoed from the prepare response.
    pub async fn execute_atomic(
        &mut self,
        transaction_id: &str,
        signature: &str,
        fees_json: &str,
    ) -> Result<AtomicExecuteResponse> {
        let canonical = canonical_execute_request(transaction_id, signature);
        let request_signature = Some(self.sign_as_atomic(&canonical)?);

        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            transaction_id,
            fees_json,
        );
        let fees_authorization = Some(self.sign_as_atomic(&fees_canonical)?);

        let call = self.client.execute_atomic_transaction(ExecuteAtomicTransactionRequest {
            transaction_id: transaction_id.to_string(),
            signature: signature.to_string(),
            fees_json: fees_json.to_string(),
            request_signature,
            fees_authorization,
        });
        let response =
            transport::with_deadline(self.bounds.call, "ExecuteAtomicTransaction", call).await?;

        // Response canonical: the ExecuteAtomicTransactionResponse's `message`
        // field maps to the v1 canonical's error_message slot (empty = None);
        // contract_id / rewards do not exist on the atomic response.
        let resp_sig = response
            .response_signature
            .as_ref()
            .ok_or_else(|| anyhow!("Missing response_signature from ledger service"))?;
        self.verify_server_key(&resp_sig.public_key)?;
        let msg_opt = (!response.message.is_empty()).then_some(response.message.as_str());
        let canonical_resp = canonical_execute_response(
            response.success,
            &response.update_id,
            None,
            msg_opt,
            None,
            None,
        );
        verify_canonical(
            &self.ledger_service_public_key,
            &canonical_resp,
            &resp_sig.signature,
            &resp_sig.signing_scheme,
        )
        .context("ExecuteAtomicTransaction response signature verification failed")?;

        Ok(response)
    }

    /// Discovery + targeted blob backfill for the atomic-dvp templates.
    pub async fn get_atomic_contracts(
        &mut self,
        template_ids: &[String],
        contract_ids: &[String],
    ) -> Result<GetAtomicContractsResponse> {
        let call = self.client.get_atomic_contracts(GetAtomicContractsRequest {
            template_ids: template_ids.to_vec(),
            contract_ids: contract_ids.to_vec(),
        });
        transport::with_deadline(self.bounds.call, "GetAtomicContracts", call).await
    }

    /// High-level: prepare → tx-verify → sign → execute, mirroring
    /// [`DAppProviderClient::submit_transaction`]. No update-scan recovery here
    /// (the atomic service exposes no GetUpdates): an execute failure whose
    /// commit status is unknowable is returned as a distinct
    /// [`ATOMIC_EXECUTE_AMBIGUOUS`] error so callers can reconcile via ledger
    /// state before re-quoting (double-fill guard).
    pub async fn submit_atomic_transaction(
        &mut self,
        req: PrepareAtomicTransactionRequest,
        expectation: &OperationExpectation,
        verbose: bool,
        dry_run: bool,
        force: bool,
    ) -> Result<AtomicExecuteResponse> {
        self.submit_atomic_with_retries(req, expectation, verbose, dry_run, force, *MAX_RETRIES)
            .await
    }

    async fn submit_atomic_with_retries(
        &mut self,
        req: PrepareAtomicTransactionRequest,
        expectation: &OperationExpectation,
        verbose: bool,
        dry_run: bool,
        force: bool,
        max_retries: u32,
    ) -> Result<AtomicExecuteResponse> {
        let budget = SubmitBudget::new(SUBMIT_BUDGET);

        // An AtomicDVP settle carries a SIGNED deadline: the on-ledger choice
        // aborts with `deadline-exceeded` once ledger time passes it. Every
        // `continue` below re-prepares from scratch, so without this a slow
        // failure (notably MEDIATOR_SAYS_TX_TIMED_OUT, which can surface later
        // than the whole validity window) burns its retries re-submitting into
        // a window that has already closed — turning a clean re-quote into a
        // DAML_FAILURE. `None` for every other operation: they have no
        // deadline and keep the previous behavior exactly.
        let quote_deadline_micros = quote_deadline_of(expectation);
        // The clock is read only for an operation that has a deadline
        let window_closed = || match quote_deadline_micros {
            Some(valid_until) => quote_window_closed(Some(valid_until), now_micros_fail_closed()),
            None => false,
        };

        for attempt in 0..max_retries {
            let attempt_no = attempt.saturating_add(1);
            let attempt_started = tokio::time::Instant::now();
            // Re-check after the backoff sleep, not just before it: the delay
            // grows as 1000 * 2^attempt ms, so a late retry can outlive the
            // window it was cleared against.
            if attempt > 0 && window_closed() {
                report_submit_error(
                    "quote_window_closed",
                    "warning", // clean re-quote, not a fault
                    "ledger_client.atomic_execute",
                    &self.party_id,
                    None,
                    expectation,
                    0,
                    attempt_no,
                    "window closed during retry backoff",
                );
                anyhow::bail!("{QUOTE_WINDOW_CLOSED}: window closed during retry backoff");
            }

            // 1. Prepare (fresh contracts each attempt — contracts may become stale)
            let mut prepared = match self.prepare_atomic(req.clone()).await {
                Ok(p) => p,
                Err(e) => {
                    let msg = format!("{:#}", e);
                    let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(&msg);
                    if unreachable {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    report_submit_error(
                        if unreachable { "CONNECTION_ERROR" } else { "SERVER_ERROR" },
                        "error",
                        "ledger_client.atomic_prepare",
                        &self.party_id,
                        None,
                        expectation,
                        0,
                        attempt_no,
                        &msg,
                    );
                    return Err(e);
                }
            };

            info!(
                "Atomic transaction prepared: id={}, command_id={}",
                prepared.transaction_id, prepared.command_id,
            );

            // 2. Verify transaction and compute hash (the bytes are not needed afterwards)
            let verification = verify_bounded(
                std::mem::take(&mut prepared.prepared_transaction),
                prepared.prepared_transaction_hash.clone(),
                prepared.hashing_scheme_version.clone(),
                expectation.clone(),
                verbose,
            )
            .await?;

            for w in &verification.warnings {
                warn!("TX verification: {}", w);
            }

            if dry_run {
                agent_logic::out!("--- DRY RUN (atomic) ---");
                agent_logic::out!("Inspection: {}", if verification.accepted { "ACCEPTED" } else { "REJECTED" });
                agent_logic::out!("Summary: {}", verification.summary);
                if let Some(reason) = &verification.rejection_reason {
                    agent_logic::out!("Rejection: {}", reason);
                }
                agent_logic::out!("--- NOT SIGNED, NOT EXECUTED ---");
                return Ok(AtomicExecuteResponse {
                    success: false,
                    update_id: String::new(),
                    message: "dry run — not executed".to_string(),
                    created_contracts_json: String::new(),
                    response_signature: None,
                });
            }

            if !verification.accepted {
                if force {
                    warn!(
                        "Atomic transaction verification REJECTED but --force is set, proceeding: {}",
                        verification.rejection_reason.as_deref().unwrap_or("unknown")
                    );
                } else {
                    let reason = verification.rejection_reason.unwrap_or_default();
                    report_submit_error(
                        "VALIDATION_ERROR",
                        "critical", // a local verifier rejection may indicate tampering
                        "ledger_client.atomic_verify",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        &format!("Atomic transaction verification REJECTED: {reason}"),
                    );
                    anyhow::bail!("Atomic transaction verification REJECTED: {}", reason);
                }
            }

            // 3. Hash selection (Phase A sentinel = sign server hash)
            let hash_to_sign = if verification.computed_hash == [0u8; 32] {
                warn!("Phase A: signing server-provided hash (verification stub active)");
                BASE64
                    .decode(&prepared.prepared_transaction_hash)
                    .context("Failed to decode prepared_transaction_hash")?
            } else {
                verification.computed_hash.to_vec()
            };
            let signature = sign_hash_bytes(&*self.private_key.expose()?, &hash_to_sign)?;

            // 4. Execute — echo fees_json verbatim
            let result = match self
                .execute_atomic(&prepared.transaction_id, &signature, &prepared.fees_json)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    // Transport error after the execute was sent: the tx may
                    // have landed. Do NOT blind-retry with a fresh prepare —
                    // surface a distinct error kind for caller reconciliation.
                    let msg = format!("{:#}", e);
                    if agent_logic::ledger_health::is_sequencer_unreachable(&msg) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    report_submit_error(
                        "atomic_execute_ambiguous",
                        "warning", // needs reconciliation, not a failure
                        "ledger_client.atomic_execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        &msg,
                    );
                    return Err(anyhow!(
                        "{}: execute failed with transport error (tx may have committed): {:#}",
                        ATOMIC_EXECUTE_AMBIGUOUS,
                        e
                    ));
                }
            };

            if !result.success {
                let error_msg = result.message.as_str();

                if error_msg.contains("SEQUENCER_BACKPRESSURE") {
                    warn!(
                        "SEQUENCER_BACKPRESSURE on atomic tx (attempt {}/{}) [{}]",
                        attempt_no, max_retries, prepared.command_id
                    );
                    signal_sequencer_backpressure();
                }

                // DUPLICATE_COMMAND: the command was accepted earlier — commit
                // status unknown without an update scan.
                if error_msg.contains("DUPLICATE_COMMAND") {
                    agent_logic::ledger_health::record_submit_success();
                    report_submit_error(
                        "atomic_execute_ambiguous",
                        "warning",
                        "ledger_client.atomic_execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        error_msg,
                    );
                    return Err(anyhow!(
                        "{}: DUPLICATE_COMMAND (an earlier submission likely committed): {}",
                        ATOMIC_EXECUTE_AMBIGUOUS,
                        error_msg
                    ));
                }

                // Checked before any retry or window bail: a fresh prepare could settle twice
                if !server_failure_is_definite(error_msg) {
                    if agent_logic::ledger_health::is_sequencer_unreachable(error_msg) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    report_submit_error(
                        "atomic_execute_ambiguous",
                        "warning",
                        "ledger_client.atomic_execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        error_msg,
                    );
                    return Err(anyhow!(
                        "{ATOMIC_EXECUTE_AMBIGUOUS}: execute reported a failure after submission (tx may have committed): {error_msg}"
                    ));
                }

                // INACTIVE_CONTRACTS: safe to re-prepare (nothing committed)
                if error_msg.contains("INACTIVE_CONTRACTS") {
                    if !window_closed()
                        && budget.allows_retry(
                            attempt_no,
                            max_retries,
                            INACTIVE_RETRY_DELAY,
                            attempt_started.elapsed(),
                            &prepared.command_id,
                        )
                    {
                        warn!(
                            "INACTIVE_CONTRACTS on atomic tx (attempt {}/{}), re-preparing in 2s [{}]",
                            attempt_no, max_retries, prepared.command_id
                        );
                        tokio::time::sleep(INACTIVE_RETRY_DELAY).await;
                        continue;
                    }
                    if window_closed() {
                        report_submit_error(
                            "quote_window_closed",
                            "warning",
                            "ledger_client.atomic_execute",
                            &self.party_id,
                            Some(&prepared.command_id),
                            expectation,
                            0,
                            attempt_no,
                            error_msg,
                        );
                        anyhow::bail!("{QUOTE_WINDOW_CLOSED}: {error_msg}");
                    }
                    report_submit_error(
                        "INACTIVE_CONTRACTS",
                        "error",
                        "ledger_client.atomic_execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        error_msg,
                    );
                    anyhow::bail!("INACTIVE_CONTRACTS after {} attempts: {}", attempt_no, error_msg);
                }

                if window_closed() {
                    // Nothing committed (the execute was rejected), so this is a
                    // clean re-quote — not an ambiguous outcome.
                    report_submit_error(
                        "quote_window_closed",
                        "warning",
                        "ledger_client.atomic_execute",
                        &self.party_id,
                        Some(&prepared.command_id),
                        expectation,
                        0,
                        attempt_no,
                        error_msg,
                    );
                    anyhow::bail!("{QUOTE_WINDOW_CLOSED}: {error_msg}");
                }

                let delay_ms = backoff_ms(attempt);
                let wait = Duration::from_millis(delay_ms);
                if budget.allows_retry(attempt_no, max_retries, wait, attempt_started.elapsed(), &prepared.command_id) {
                    warn!(
                        "Atomic transaction error (attempt {}/{}): {} — retrying in {}ms [{}]",
                        attempt_no, max_retries, error_msg, delay_ms, prepared.command_id
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }
                let unreachable = agent_logic::ledger_health::is_sequencer_unreachable(error_msg);
                if unreachable {
                    agent_logic::ledger_health::record_submit_failure();
                }
                report_submit_error(
                    classify_submit_error(error_msg),
                    "error",
                    "ledger_client.atomic_execute",
                    &self.party_id,
                    Some(&prepared.command_id),
                    expectation,
                    0,
                    attempt_no,
                    error_msg,
                );
                anyhow::bail!("Atomic transaction failed: {}", error_msg);
            }

            agent_logic::ledger_health::record_submit_success();
            return Ok(result);
        }

        Err(anyhow!("Atomic transaction submit: retry loop exhausted after {max_retries} attempt(s)"))
    }
}

#[cfg(test)]
mod quote_window_tests {
    use super::*;
    use crate::atomic_swap::PRECHECK_VALIDITY_MARGIN_MICROS as MARGIN;

    pub(super) fn atomic_settle(valid_until_micros: i64) -> OperationExpectation {
        OperationExpectation::AtomicDvpSettle {
            user_party: "user::1220aa".into(),
            venue_cid: "00venue".into(),
            template_id: "#atomic-dvp-v2:AtomicDVP:AtomicDVP".into(),
            quote_id: "q-1".into(),
            ticket_id: String::new(),
            ticket_cid: None,
            side: "Buy".into(),
            base_amount: "5.0".into(),
            quote_amount: "25.0".into(),
            lp_party: "lp::1220bb".into(),
            base_instrument_id: "EDELx".into(),
            base_instrument_admin: "admin::1220cc".into(),
            quote_instrument_id: "cETH".into(),
            quote_instrument_admin: "admin::1220dd".into(),
            valid_until_micros,
            lp_input_holding_cids: vec!["00lp1".into()],
            user_input_holding_cids: vec!["00u1".into()],
        }
    }

    #[test]
    fn only_atomic_settle_carries_a_deadline() {
        assert_eq!(quote_deadline_of(&atomic_settle(1_234)), Some(1_234));
        // Every other operation stays deadline-free, so the retry loop is
        // unchanged for it.
        assert_eq!(
            quote_deadline_of(&OperationExpectation::IssueTickets {
                lp_party: "lp::1220bb".into(),
                ticket_count: 4,
            }),
            None
        );
    }

    #[test]
    fn deadline_free_operations_never_stop_retrying() {
        assert!(!quote_window_closed(None, i64::MAX));
    }

    #[test]
    fn window_closes_once_less_than_the_margin_remains() {
        let now = 1_000_000_000;
        // Comfortably open: a full margin plus a second to spare.
        assert!(!quote_window_closed(Some(now + MARGIN + 1_000_000), now));
        // Exactly the margin left is NOT enough — prepare+sign+execute needs it.
        assert!(quote_window_closed(Some(now + MARGIN), now));
        // Already expired (the W10 case: the mediator timeout surfaced ~5 s
        // after the quote died, and the old code re-prepared 1 s later).
        assert!(quote_window_closed(Some(now - 5_000_000), now));
    }

    #[test]
    fn guard_agrees_with_the_taker_side_precheck_margin() {
        // Both sides must call the same window "closed" — otherwise the retry
        // loop re-prepares something settle_envelope would have rejected.
        let now = 1_000_000_000;
        let valid_until = now + MARGIN;
        assert!(quote_window_closed(Some(valid_until), now));
        assert!(now + MARGIN >= valid_until, "pre_submit_check bails here too");
    }
}

#[cfg(test)]
mod backpressure_pause_tests {
    use super::*;

    #[test]
    fn only_deadline_free_housekeeping_counts_as_background() {
        // These two are emitted solely by the DvpProposal GC, and skipping one
        // costs nothing — the contract stays active and the next scan re-queues
        // it.
        assert!(is_background_op(TransactionOperation::CancelDvpProposal as i32));
        assert!(is_background_op(TransactionOperation::RejectDvpProposal as i32));

        // Everything a user, a counterparty or a settlement is waiting on must
        // keep its retries: giving up on these turns congestion into a failed
        // trade rather than a deferred cleanup.
        assert!(!is_background_op(TransactionOperation::Allocate as i32));
        assert!(!is_background_op(TransactionOperation::PayDvpFee as i32));
        assert!(!is_background_op(TransactionOperation::PayAllocFee as i32));
        assert!(!is_background_op(TransactionOperation::ProposeDvp as i32));
        assert!(!is_background_op(TransactionOperation::AcceptDvp as i32));
        assert!(!is_background_op(TransactionOperation::Unspecified as i32));
    }

    /// One test, not several: the two deadlines are process-global, so separate
    /// tests would race each other through `signal_sequencer_backpressure`.
    #[test]
    fn backpressure_arms_both_pauses_and_background_outlasts_fees() {
        assert_eq!(
            background_pause_remaining(),
            None,
            "nothing should be paused before the first backpressure event",
        );

        signal_sequencer_backpressure();

        let fee = fee_pause_remaining().expect("fee pause armed");
        let background = background_pause_remaining().expect("background pause armed");
        assert!(
            background >= fee,
            "deadline-free work must be the last to resume competing for \
             sequencer slots: background {}s < fees {}s",
            background,
            fee,
        );
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    use tonic::service::Interceptor;

    fn sent_token(i: &mut AuthInterceptor) -> String {
        let request = i.call(Request::new(())).unwrap();
        request.metadata().get("authorization").unwrap().to_str().unwrap().to_string()
    }

    // A key that cannot be opened keeps the previous token instead of panicking
    #[test]
    fn interceptor_keeps_the_previous_token_when_the_key_cannot_be_opened() {
        let mut good = AuthInterceptor::for_party("party", "agent", &Secret::seal(&mut [3u8; 32]).unwrap(), 3600, None)
            .unwrap();
        let first = sent_token(&mut good);
        let identity = good.identity.as_mut().unwrap();
        identity.private_key = Secret::corrupt_for_tests();
        *agent_logic::sync::write(&identity.expires_at) = 0;
        assert_eq!(sent_token(&mut good), first);
        assert_eq!(*agent_logic::sync::read(&good.identity.as_ref().unwrap().expires_at), 0);
    }

    #[test]
    fn a_client_identity_needs_a_key_that_opens() {
        let err = AuthInterceptor::for_party("party", "agent", &Secret::corrupt_for_tests(), 3600, None).err().unwrap();
        assert_eq!(err.to_string(), "sealed secret is corrupt");
        let mut anonymous = AuthInterceptor { identity: None };
        assert!(anonymous.call(Request::new(())).unwrap().metadata().get("authorization").is_none());
    }
}


#[cfg(test)]
mod bounded_io_tests {
    use super::*;
    use crate::test_util::{refused_url, Fake, FakeLedger, RawCodec};
    use orderbook_proto::ledger::{
        prepare_transaction_request, FaucetRequest, GetUpdatesResponse, LedgerCreatedEvent,
        LedgerEvent, LedgerTransaction, RequestPreapprovalParams,
    };
    use orderbook_proto::rfqv2::IssueTicketsParams;
    use std::convert::Infallible;
    use std::future::Future;
    use std::sync::atomic::AtomicU32;
    use std::task::{Context as TaskContext, Poll};
    use tonic::codegen::http;
    use tonic::{Response, Status};

    fn key() -> Secret<32> {
        Secret::seal(&mut [7u8; 32]).unwrap()
    }

    fn short_bounds() -> CallBounds {
        CallBounds {
            call: Duration::from_millis(250),
            stream_idle: Duration::from_millis(250),
            stream_total: Duration::from_secs(3),
        }
    }

    /// A channel with no request timeout of its own, so only the client deadline bounds a call.
    fn lazy_channel(url: &str) -> Channel {
        let opts = ChannelOpts { request: None, keepalive: false, ..ChannelOpts::default() };
        transport::endpoint(url, opts).unwrap().connect_lazy()
    }

    fn dapp_client(channel: Channel, bounds: CallBounds) -> DAppProviderClient {
        DAppProviderClient::from_channel(channel, "party", "agent", &key(), 3600, None, &[0u8; 32])
            .unwrap()
            .with_bounds(bounds)
    }

    fn atomic_client(channel: Channel, bounds: CallBounds) -> AtomicProviderClient {
        AtomicProviderClient::with_channel(channel, "party", "agent", &key(), 3600, None, &[0u8; 32], bounds)
            .unwrap()
    }

    async fn expect_deadline<T>(what: &str, call: impl Future<Output = Result<T>>) {
        let result = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .unwrap_or_else(|_| panic!("{what}: the client deadline should end the call"));
        let Err(err) = result else { panic!("{what}: a silent peer cannot answer") };
        let text = format!("{err:#}");
        assert!(
            text.contains(&format!("{what} RPC failed (Unavailable): client deadline")),
            "{what}: {text}"
        );
    }

    // A peer that accepts TCP but never answers would hang these calls without a client deadline
    #[tokio::test]
    async fn every_ledger_call_is_bounded_by_the_client_deadline() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let mut c = dapp_client(lazy_channel(&url), short_bounds());
        let prepare = PrepareTransactionRequest {
            params: Some(prepare_transaction_request::Params::RequestPreapproval(
                RequestPreapprovalParams { instrument_admin: "admin".into(), ..Default::default() },
            )),
            ..Default::default()
        };
        expect_deadline("GetActiveContracts", c.get_active_contracts_partial(&[])).await;
        expect_deadline("GetLedgerEnd", c.get_ledger_end()).await;
        expect_deadline("GetUpdates", c.get_updates(0, None, &[])).await;
        expect_deadline("GetBalances", c.get_balances()).await;
        expect_deadline("GetPrepaidTrafficBalance", c.get_prepaid_traffic_balance()).await;
        expect_deadline("GetPreapprovals", c.get_preapprovals()).await;
        expect_deadline("GetDsoRates", c.get_dso_rates()).await;
        expect_deadline("GetSettlementContracts", c.get_settlement_contracts(&[])).await;
        expect_deadline("GetAmulets", c.get_amulets()).await;
        expect_deadline("PrepareTransaction", c.prepare_transaction(prepare)).await;
        expect_deadline("ExecuteTransaction", c.execute_transaction("tx", "sig", "{}")).await;
        expect_deadline("RequestFaucet", c.request_faucet(FaucetRequest::default())).await;
        expect_deadline("ListFaucetInstruments", c.list_faucet_instruments()).await;
        expect_deadline("PreparePayFee", c.prepare_pay_fee("p1", "dvp")).await;
        expect_deadline("ExecutePayFee", c.execute_pay_fee("p1", "dvp", "{}", "s1")).await;
        drop(silent);
    }

    #[tokio::test]
    async fn every_atomic_call_is_bounded_by_the_client_deadline() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", silent.local_addr().unwrap());
        let mut c = atomic_client(lazy_channel(&url), short_bounds());
        let prepare = PrepareAtomicTransactionRequest {
            params: Some(AtomicParams::IssueTickets(IssueTicketsParams { ticket_ids: vec!["t1".into()] })),
            ..Default::default()
        };
        expect_deadline("PrepareAtomicTransaction", c.prepare_atomic(prepare)).await;
        expect_deadline("ExecuteAtomicTransaction", c.execute_atomic("tx", "sig", "{}")).await;
        expect_deadline("GetAtomicContracts", c.get_atomic_contracts(&[], &[])).await;
        drop(silent);
    }

    // ---- a fake ledger whose streams send two empty messages, then stall, end or fail ----

    /// A client of a fake ledger whose unary calls answer and whose streams follow `mode`.
    async fn fake_ledger(mode: Fake) -> DAppProviderClient {
        let fake = FakeLedger::start(mode).await;
        dapp_client(lazy_channel(&fake.url), short_bounds())
    }

    // A stream that stalls after headers used to hang the caller forever
    #[tokio::test]
    async fn a_stalled_updates_stream_returns_its_prefix_as_incomplete() {
        let mut c = fake_ledger(Fake::StreamStall).await;
        let (updates, complete) = tokio::time::timeout(Duration::from_secs(5), c.get_updates(0, None, &[]))
            .await
            .expect("the idle bound should end the stream")
            .unwrap();
        assert_eq!(updates.len(), 2, "messages before the stall are kept");
        assert!(!complete);
    }

    #[tokio::test]
    async fn updates_streams_report_whether_they_completed() {
        let (updates, complete) = fake_ledger(Fake::Answer).await.get_updates(0, None, &[]).await.unwrap();
        assert_eq!((updates.len(), complete), (2, true));
        let (updates, complete) = fake_ledger(Fake::StreamFail).await.get_updates(0, None, &[]).await.unwrap();
        assert_eq!((updates.len(), complete), (2, false));
    }

    #[tokio::test]
    async fn a_stalled_acs_stream_is_bounded_and_never_passes_as_complete() {
        let mut c = fake_ledger(Fake::StreamStall).await;
        let (_, complete) = tokio::time::timeout(Duration::from_secs(5), c.get_active_contracts_partial(&[]))
            .await
            .expect("the idle bound should end the stream")
            .unwrap();
        assert!(!complete);
        let strict = tokio::time::timeout(Duration::from_secs(5), c.get_active_contracts(&[])).await.unwrap();
        assert!(strict.unwrap_err().to_string().contains("snapshot incomplete"));
        let (_, complete) = fake_ledger(Fake::Answer).await.get_active_contracts_partial(&[]).await.unwrap();
        assert!(complete);
    }

    #[tokio::test]
    async fn the_total_bound_ends_a_stream_that_keeps_trickling() {
        let mut c = fake_ledger(Fake::StreamStall).await;
        c.bounds = CallBounds { stream_idle: Duration::from_secs(30), stream_total: Duration::from_millis(300), ..short_bounds() };
        let (updates, complete) = tokio::time::timeout(Duration::from_secs(5), c.get_updates(0, None, &[]))
            .await
            .expect("the total bound should end the stream")
            .unwrap();
        assert_eq!((updates.len(), complete), (2, false));
    }

    // ---- update scans ----

    fn tx_update(command_id: &str, update_id: &str) -> GetUpdatesResponse {
        GetUpdatesResponse {
            update: Some(get_updates_response::Update::Transaction(LedgerTransaction {
                command_id: command_id.into(),
                update_id: update_id.into(),
                events: vec![LedgerEvent {
                    event: Some(ledger_event::Event::Created(LedgerCreatedEvent {
                        contract_id: "00cid".into(),
                        ..Default::default()
                    })),
                }],
                ..Default::default()
            })),
        }
    }

    // A miss in a truncated scan must not read as "never executed"
    #[test]
    fn a_miss_in_an_incomplete_scan_is_unknown() {
        let updates = vec![tx_update("other", "u0")];
        assert!(matches!(scan_updates(&updates, true, "cmd"), UpdateScan::NotFound));
        let UpdateScan::Unknown(why) = scan_updates(&updates, false, "cmd") else {
            panic!("an incomplete scan without the command is unknown");
        };
        assert!(why.contains("incomplete after 1 update(s)"), "{why}");
        assert!(matches!(scan_updates(&[], false, "cmd"), UpdateScan::Unknown(_)));
    }

    #[test]
    fn a_hit_counts_even_in_an_incomplete_scan() {
        let updates = vec![tx_update("other", "u0"), tx_update("cmd", "u1")];
        for complete in [true, false] {
            let UpdateScan::Found(found) = scan_updates(&updates, complete, "cmd") else {
                panic!("the command is in the scanned prefix");
            };
            assert!(found.success);
            assert_eq!(found.update_id, "u1");
            assert_eq!(found.contract_id.as_deref(), Some("00cid"));
        }
    }

    // The fake ledger ends at offset 0, so a start of -1 makes it scan its update stream
    #[tokio::test]
    async fn a_scan_over_a_stalled_stream_is_unknown() {
        let mut c = fake_ledger(Fake::StreamStall).await;
        let scan = tokio::time::timeout(Duration::from_secs(5), c.find_transaction_in_updates("cmd", -1))
            .await
            .expect("the idle bound should end the scan");
        let UpdateScan::Unknown(why) = scan else { panic!("expected Unknown, got {scan:?}") };
        assert!(why.contains("incomplete after 2 update(s)"), "{why}");
        let scan = fake_ledger(Fake::StreamFail).await.find_transaction_in_updates("cmd", -1).await;
        assert!(matches!(scan, UpdateScan::Unknown(_)), "{scan:?}");
        let scan = fake_ledger(Fake::StreamRefuse).await.find_transaction_in_updates("cmd", -1).await;
        let UpdateScan::Unknown(why) = scan else { panic!("expected Unknown, got {scan:?}") };
        assert!(why.starts_with("updates scan failed"), "{why}");
        let scan = fake_ledger(Fake::Answer).await.find_transaction_in_updates("cmd", -1).await;
        assert!(matches!(scan, UpdateScan::NotFound), "{scan:?}");
        let scan = fake_ledger(Fake::StreamStall).await.find_transaction_in_updates("cmd", 0).await;
        assert!(matches!(scan, UpdateScan::NotFound), "nothing new since the start: {scan:?}");
    }

    // An inconclusive scan used to count as "not found", so the command was executed again
    #[tokio::test]
    async fn an_execute_error_with_an_inconclusive_scan_is_not_retried() {
        let expectation = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let mut c = dapp_client(lazy_channel(&refused_url()), short_bounds());
        let failed = || anyhow!("ExecuteTransaction RPC failed (Internal): boom");
        let recovery = c
            .recover_after_execute_error(failed(), "cmd", 7, Duration::ZERO, &expectation, 0, 1)
            .await;
        let ExecuteRecovery::Stop(err) = recovery else { panic!("expected Stop, got {recovery:?}") };
        assert!(is_ambiguous_execute_error(&err));
        let text = format!("{err:#}");
        assert!(text.contains("ledger end unavailable") && text.ends_with("boom"), "{text}");

        // Recovery off (no start offset): retried as before
        let recovery = c.recover_after_execute_error(failed(), "cmd", 0, Duration::ZERO, &expectation, 0, 1).await;
        assert!(matches!(recovery, ExecuteRecovery::Retry(_)), "{recovery:?}");
        // Not on the ledger yet: a re-prepare is allowed, but a later failure is still marked unknown
        let mut quiet = fake_ledger(Fake::Answer).await;
        let recovery = quiet.recover_after_execute_error(failed(), "cmd", 7, Duration::ZERO, &expectation, 0, 1).await;
        assert!(matches!(recovery, ExecuteRecovery::Retry(_)), "{recovery:?}");
    }

    // ---- a fake ledger that signs a prepare, then fails every execute ----

    const SERVER_KEY: [u8; 32] = [9u8; 32];

    fn put_varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn put_bytes(field: u64, data: &[u8], out: &mut Vec<u8>) {
        put_varint((field << 3) | 2, out);
        put_varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    fn signature_field(field: u64, canonical: &[u8], out: &mut Vec<u8>) {
        let sig = sign_canonical(&SERVER_KEY, canonical);
        let mut m = Vec::new();
        put_bytes(1, sig.signature_b64.as_bytes(), &mut m);
        put_bytes(2, sig.public_key_b64url.as_bytes(), &mut m);
        put_bytes(3, sig.signing_scheme.as_bytes(), &mut m);
        put_bytes(field, &m, out);
    }

    /// A PrepareTransactionResponse for command "cmd-1", signed by `SERVER_KEY`.
    fn signed_prepare(party: &str) -> Vec<u8> {
        let (txid, cmd, version, fees) = ("tx-1", "cmd-1", "V2", "[]");
        let hash = BASE64.encode([1u8; 32]);
        let mut m = Vec::new();
        put_bytes(1, txid.as_bytes(), &mut m);
        put_bytes(2, hash.as_bytes(), &mut m);
        put_bytes(3, cmd.as_bytes(), &mut m);
        put_bytes(6, version.as_bytes(), &mut m);
        put_bytes(8, fees.as_bytes(), &mut m);
        signature_field(30, &message_signing::canonical_prepare_response(txid, &hash, cmd, &[], version), &mut m);
        signature_field(31, &message_signing::canonical_tx_fees_authorization(party, txid, fees), &mut m);
        m
    }

    struct Fixed(Result<Vec<u8>, Status>);

    impl tonic::server::UnaryService<()> for Fixed {
        type Response = Vec<u8>;
        type Future = tonic::codegen::BoxFuture<Response<Vec<u8>>, Status>;
        fn call(&mut self, _: tonic::Request<()>) -> Self::Future {
            let reply = self.0.clone();
            Box::pin(async move { reply.map(Response::new) })
        }
    }

    /// An execute response reporting `error` in field `error_field`, signed by `SERVER_KEY`.
    fn signed_failure(error: &str, error_field: u64) -> Vec<u8> {
        let mut m = Vec::new();
        put_bytes(error_field, error.as_bytes(), &mut m);
        signature_field(30, &canonical_execute_response(false, "", None, Some(error), None, None), &mut m);
        m
    }

    /// Ledger end 7, a signed prepare, and `execute` as the answer to every other call.
    #[derive(Clone)]
    struct SigningLedger {
        prepare: Vec<u8>,
        execute: Result<Vec<u8>, Status>,
    }

    impl SigningLedger {
        /// Its execute times out.
        fn timing_out() -> Self {
            Self { prepare: signed_prepare("party"), execute: Err(Status::deadline_exceeded("Timeout expired")) }
        }

        /// Its execute reports `error`, in the response field `error_field`.
        fn failing(error: &str, error_field: u64) -> Self {
            Self { prepare: signed_prepare("party"), execute: Ok(signed_failure(error, error_field)) }
        }
    }

    impl tonic::server::NamedService for SigningLedger {
        const NAME: &'static str = "silvana.ledger.v1.DAppProviderService";
    }

    impl<B> tonic::codegen::Service<http::Request<B>> for SigningLedger
    where
        B: tonic::codegen::Body + Send + 'static,
        B::Error: Into<tonic::codegen::StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let path = req.uri().path().to_string();
            let reply = if path.ends_with("/GetLedgerEnd") {
                Ok(vec![0x08, 7])
            } else if path.ends_with("/PrepareTransaction") || path.ends_with("/PrepareAtomicTransaction") {
                Ok(self.prepare.clone())
            } else {
                self.execute.clone()
            };
            let (parts, _) = req.into_parts();
            let req = http::Request::from_parts(parts, tonic::body::Body::new(String::from("\0\0\0\0\0")));
            Box::pin(async move { Ok(tonic::server::Grpc::new(RawCodec).unary(Fixed(reply), req).await) })
        }
    }

    /// [`SigningLedger`] served as the atomic service.
    #[derive(Clone)]
    struct AtomicSigningLedger(SigningLedger);

    impl tonic::server::NamedService for AtomicSigningLedger {
        const NAME: &'static str = "silvana.rfqv2.v1.AtomicDvpProviderService";
    }

    impl<B> tonic::codegen::Service<http::Request<B>> for AtomicSigningLedger
    where
        B: tonic::codegen::Body + Send + 'static,
        B::Error: Into<tonic::codegen::StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Infallible>;

        fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            tonic::codegen::Service::call(&mut self.0, req)
        }
    }

    /// Serve `routes` on a free port; returns its URL and the server task.
    async fn serve(routes: tonic::service::Routes) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let server = tokio::spawn(async move {
            let _ = tonic::transport::Server::builder().add_routes(routes).serve_with_incoming(incoming).await;
        });
        (url, server)
    }

    fn server_key() -> [u8; 32] {
        SigningKey::from_bytes(&SERVER_KEY).verifying_key().to_bytes()
    }

    fn preapproval_request() -> PrepareTransactionRequest {
        PrepareTransactionRequest {
            params: Some(prepare_transaction_request::Params::RequestPreapproval(
                RequestPreapprovalParams { instrument_admin: "admin".into(), ..Default::default() },
            )),
            ..Default::default()
        }
    }

    /// One V1 submit attempt against `ledger`; returns its error.
    async fn submit_once(ledger: SigningLedger) -> anyhow::Error {
        let (url, server) = serve(tonic::service::Routes::new(ledger)).await;
        let mut c = DAppProviderClient::from_channel(lazy_channel(&url), "party", "agent", &key(), 3600, None, &server_key())
            .unwrap()
            .with_bounds(short_bounds());
        // An expectation the inspector does not check, so the empty fake transaction passes
        let expectation = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let submit = c.submit_with_retries(preapproval_request(), &expectation, false, false, false, 1);
        let err = tokio::time::timeout(Duration::from_secs(30), submit).await.expect("bounded").unwrap_err();
        server.abort();
        err
    }

    // An execute that timed out and was not yet on the ledger used to end as a plain failure
    #[tokio::test]
    async fn a_failure_after_an_unconfirmed_execute_is_outcome_unknown() {
        let err = submit_once(SigningLedger::timing_out()).await;
        let text = format!("{err:#}");
        assert!(is_ambiguous_execute_error(&err), "{text}");
        assert!(text.contains("earlier execute cmd-1 may still commit") && text.contains("Timeout expired"), "{text}");
    }

    const COMPLETIONS_ERROR: &str = "error sending request for url (http://participant/v2/commands/completions)";

    // A failure the server reports after the participant took the submission may still commit
    #[tokio::test]
    async fn a_server_reported_failure_after_submission_is_outcome_unknown() {
        let err = submit_once(SigningLedger::failing(COMPLETIONS_ERROR, 4)).await;
        let text = format!("{err:#}");
        assert!(is_ambiguous_execute_error(&err), "{text}");
        assert!(text.contains("earlier execute cmd-1 may still commit") && text.ends_with(COMPLETIONS_ERROR), "{text}");

        let err = submit_once(SigningLedger::failing("Transaction failed: rejected", 4)).await;
        assert!(!is_ambiguous_execute_error(&err), "a completion rejection is definite: {err:#}");
    }

    /// One atomic submit attempt for `expectation` against `ledger`; returns its error.
    async fn submit_atomic_once(ledger: SigningLedger, expectation: OperationExpectation) -> anyhow::Error {
        let (url, server) = serve(tonic::service::Routes::new(AtomicSigningLedger(ledger))).await;
        let mut c = AtomicProviderClient::with_channel(
            lazy_channel(&url), "party", "agent", &key(), 3600, None, &server_key(), short_bounds(),
        )
        .unwrap();
        let req = PrepareAtomicTransactionRequest {
            params: Some(AtomicParams::IssueTickets(IssueTicketsParams { ticket_ids: vec!["t1".into()] })),
            ..Default::default()
        };
        // Forced, so an expectation the empty fake transaction fails still reaches the execute
        let submit = c.submit_atomic_with_retries(req, &expectation, false, false, true, 1);
        let err = tokio::time::timeout(Duration::from_secs(30), submit).await.expect("bounded").unwrap_err();
        server.abort();
        err
    }

    // A re-prepare or a closed-window requote after such a failure could settle a second time
    #[tokio::test]
    async fn an_atomic_failure_reported_after_submission_is_ambiguous() {
        let tickets = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let err = submit_atomic_once(SigningLedger::failing(COMPLETIONS_ERROR, 3), tickets.clone()).await;
        assert!(format!("{err:#}").contains(ATOMIC_EXECUTE_AMBIGUOUS), "{err:#}");

        let closed = super::quote_window_tests::atomic_settle(0);
        let err = submit_atomic_once(SigningLedger::failing(COMPLETIONS_ERROR, 3), closed.clone()).await;
        assert!(format!("{err:#}").contains(ATOMIC_EXECUTE_AMBIGUOUS), "checked before the window: {err:#}");

        let err = submit_atomic_once(SigningLedger::failing("Transaction failed: rejected", 3), closed).await;
        assert!(format!("{err:#}").starts_with(QUOTE_WINDOW_CLOSED), "{err:#}");
        let err = submit_atomic_once(SigningLedger::failing("Transaction failed: rejected", 3), tickets).await;
        assert!(!is_ambiguous_execute_error(&err), "{err:#}");
    }

    #[test]
    fn server_failures_are_definite_only_with_proof() {
        for definite in [
            "Transaction failed: INACTIVE_CONTRACTS",
            "Transaction session not found: tx-1 (expired or invalid)",
            "fees_json mismatch: cloud-agent echoed a different schedule",
            "Invalid party ID format: expected 'namespace::fingerprint', got 'x'",
            "error sending request for url (http://participant/v2/state/ledger-end)",
            "Missing offset in ledger-end response",
            "Execute submission failed (HTTP 400 Bad Request): {}",
            "Execute submission failed (HTTP 503 Service Unavailable): SEQUENCER_BACKPRESSURE",
        ] {
            assert!(server_failure_is_definite(definite), "{definite}");
        }
        for ambiguous in [
            COMPLETIONS_ERROR,
            "error sending request for url (http://participant/v2/interactive-submission/execute)",
            "Timeout waiting for transaction completion (submission_id: s)",
            "Execute submission failed (HTTP 502 Bad Gateway): upstream",
            "Execute submission failed (HTTP 408 Request Timeout): {}",
            "Transaction failed: SUBMISSION_ALREADY_IN_FLIGHT",
            "error decoding response body",
            "",
        ] {
            assert!(!server_failure_is_definite(ambiguous), "{ambiguous}");
        }
    }

    #[tokio::test]
    async fn an_unreachable_ledger_end_makes_the_scan_unknown() {
        let mut c = dapp_client(lazy_channel(&refused_url()), short_bounds());
        let scan = tokio::time::timeout(Duration::from_secs(5), c.find_transaction_in_updates("cmd", 7))
            .await
            .unwrap();
        let UpdateScan::Unknown(why) = scan else { panic!("expected Unknown, got {scan:?}") };
        assert!(why.starts_with("ledger end unavailable"), "{why}");
    }

    #[test]
    fn a_failure_after_an_uncertain_execute_is_marked_unknown() {
        let plain = mark_uncertain(anyhow!("Transaction failed: INACTIVE_CONTRACTS"), None);
        assert!(!is_ambiguous_execute_error(&plain));
        let marked = mark_uncertain(anyhow!("Transaction failed: INACTIVE_CONTRACTS"), Some("cmd-1"));
        assert!(is_ambiguous_execute_error(&marked));
        let text = marked.to_string();
        assert!(text.contains("earlier execute cmd-1 may still commit") && text.ends_with("INACTIVE_CONTRACTS"), "{text}");
        let already = anyhow!("{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete");
        assert_eq!(mark_uncertain(already, Some("cmd-1")).to_string(), format!("{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete"));
    }

    #[test]
    fn outcome_unknown_errors_count_as_ambiguous() {
        let err = anyhow!("{EXECUTE_OUTCOME_UNKNOWN}: ledger scan incomplete (x); not retried: y");
        assert!(is_ambiguous_execute_error(&err));
        assert!(is_ambiguous_execute_error(&anyhow!("{ATOMIC_EXECUTE_AMBIGUOUS}: z")));
        assert!(!is_ambiguous_execute_error(&anyhow!("Transaction failed: INACTIVE_CONTRACTS")));
    }

    // ---- retry policy ----

    #[test]
    fn max_retries_parsing() {
        assert_eq!(parse_max_retries(None).unwrap(), DEFAULT_MAX_RETRIES);
        assert_eq!(parse_max_retries(Some("")).unwrap(), DEFAULT_MAX_RETRIES);
        assert_eq!(parse_max_retries(Some(" 3 ")).unwrap(), 3);
        assert_eq!(parse_max_retries(Some("4294967295")).unwrap(), u32::MAX);
        let zero = parse_max_retries(Some("0")).unwrap_err().to_string();
        assert!(zero.contains("MAX_RETRIES=0"), "{zero}");
        for bad in ["abc", "-1", "1.5", "4294967296"] {
            let err = parse_max_retries(Some(bad)).unwrap_err().to_string();
            assert!(err.contains("MAX_RETRIES"), "{bad}: {err}");
        }
    }

    // 2^attempt used to overflow and the delay grew without limit
    #[test]
    fn backoff_is_exponential_capped_and_never_panics() {
        for _ in 0..50 {
            assert!((1000..=1100).contains(&backoff_ms(0)));
            assert!((8000..=8800).contains(&backoff_ms(3)));
            for attempt in [5, 6, 31, 63, 64, 65, u32::MAX] {
                assert!((30_000..=33_000).contains(&backoff_ms(attempt)), "{attempt}");
            }
        }
    }

    #[test]
    fn the_budget_admits_a_retry_only_while_one_more_attempt_fits() {
        let now = tokio::time::Instant::now();
        let budget = SubmitBudget { total: Duration::from_secs(10), deadline: now + Duration::from_secs(10) };
        let s = Duration::from_secs;
        assert!(budget.fits_at(now, s(1), s(9)));
        assert!(!budget.fits_at(now, s(1), s(10)));
        assert!(!budget.fits_at(now + s(9), s(2), Duration::ZERO));
        assert!(!budget.fits_at(now + s(60), Duration::ZERO, Duration::ZERO), "past the deadline");
        assert!(!budget.fits_at(now, Duration::MAX, Duration::MAX), "huge waits must not panic");
        assert!(budget.fits_at(now, Duration::ZERO, Duration::ZERO));
    }

    #[test]
    fn the_retry_gate_checks_attempts_before_the_budget() {
        let fresh = SubmitBudget::new(Duration::from_secs(60));
        assert!(fresh.allows_retry(1, 5, Duration::from_secs(1), Duration::from_secs(1), "c"));
        assert!(!fresh.allows_retry(5, 5, Duration::ZERO, Duration::ZERO, "c"), "attempts exhausted");
        let spent = SubmitBudget::new(Duration::ZERO);
        assert!(!spent.allows_retry(1, 5, Duration::from_millis(1), Duration::ZERO, "c"), "budget spent");
    }

    // MAX_RETRIES=0 used to reach unreachable!() and panic
    #[tokio::test]
    async fn an_empty_retry_loop_is_an_error() {
        let expectation = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let mut v1 = dapp_client(lazy_channel(&refused_url()), short_bounds());
        let err = v1
            .submit_with_retries(PrepareTransactionRequest::default(), &expectation, false, false, false, 0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("retry loop exhausted after 0 attempt(s)"), "{err}");
        let mut atomic = atomic_client(lazy_channel(&refused_url()), short_bounds());
        let err = atomic
            .submit_atomic_with_retries(PrepareAtomicTransactionRequest::default(), &expectation, false, false, false, 0)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("retry loop exhausted after 0 attempt(s)"), "{err}");
    }

    // ---- clocks and pauses ----

    // now + margin used to overflow for a far-future clock reading
    #[test]
    fn an_unreadable_clock_closes_the_quote_window() {
        assert!(quote_window_closed(Some(i64::MAX), i64::MAX));
        assert!(quote_window_closed(Some(0), i64::MAX));
        assert!(!quote_window_closed(None, i64::MAX));
        assert!(now_micros_fail_closed() > 1_577_836_800_000_000);
        assert_eq!(micros_or_max(None), i64::MAX, "a clock before 1970 counts as the far future");
        assert_eq!(micros_or_max(Some(Duration::MAX)), i64::MAX);
        assert_eq!(micros_or_max(Some(Duration::from_secs(2))), 2_000_000);
    }

    #[test]
    fn pause_arithmetic_saturates_and_rounds_up() {
        assert_eq!(resume_at_ms(1_000, 10), 11_000);
        assert_eq!(resume_at_ms(u64::MAX - 5, 10), u64::MAX);
        assert_eq!(resume_at_ms(5, u64::MAX), u64::MAX);
        assert_eq!(remaining_secs(11_000, 1_000), Some(10));
        assert_eq!(remaining_secs(11_000, 10_999), Some(1));
        assert_eq!(remaining_secs(11_001, 1_000), Some(11));
        assert_eq!(remaining_secs(11_000, 11_000), None);
        assert_eq!(remaining_secs(u64::MAX, 0), Some(u64::MAX.div_ceil(1000)));
    }

    // ---- channel and bounds ----

    #[tokio::test]
    async fn bounds_follow_the_request_timeout() {
        let b = CallBounds::for_request_secs(None);
        assert_eq!(b.call, Duration::from_secs(125));
        assert_eq!(b.stream_total, Duration::from_secs(150));
        assert_eq!(b.stream_idle, transport::STREAM_IDLE_TIMEOUT);
        let b = CallBounds::for_request_secs(Some(u64::MAX));
        assert_eq!(b.call, Duration::MAX, "saturates instead of panicking");
        let c = dapp_client(lazy_channel(&refused_url()), short_bounds())
            .with_request_timeout(Duration::from_secs(300));
        assert_eq!(c.bounds, CallBounds::for_request_timeout(Duration::from_secs(300)));
        let opts = channel_opts(None, Some(300));
        assert_eq!(opts.connect, Duration::from_secs(30));
        assert_eq!(opts.tls, transport::TLS_HANDSHAKE_TIMEOUT);
        assert_eq!(opts.request, Some(Duration::from_secs(300)));
        assert!(opts.keepalive);
    }

    // An outer wait at or below the client's own bound would cut calls that are still within it
    #[test]
    fn outer_waits_sit_above_the_client_bounds() {
        for (connect, request) in [(1, 1), (30, 120), (300, 3600)] {
            let b = CallBounds::for_request_secs(Some(request));
            assert!(connect_wait(connect, request) > channel_opts(Some(connect), Some(request)).connect_budget());
            assert!(call_wait(request) > b.call);
            assert!(stream_wait(request) > b.call.saturating_add(b.stream_total));
        }
        assert_eq!(stream_wait(u64::MAX), Duration::MAX, "saturates instead of panicking");
    }

    #[tokio::test]
    async fn within_turns_an_elapsed_budget_into_an_error() {
        let probe = within("probe", Duration::from_millis(20), std::future::pending::<Result<()>>());
        let err = tokio::time::timeout(Duration::from_secs(5), probe).await.expect("bounded").unwrap_err();
        assert_eq!(err.to_string(), "probe did not finish within 20ms");
        assert_eq!(within("probe", Duration::from_secs(1), async { Ok(7) }).await.unwrap(), 7);
    }

    // A peer that accepts TCP but never answers TLS used to hang channel creation
    #[tokio::test]
    async fn a_stalled_tls_handshake_fails_channel_creation() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let limit = transport::TLS_HANDSHAKE_TIMEOUT.saturating_add(Duration::from_secs(5));
        let result = tokio::time::timeout(limit, DAppProviderClient::create_channel(&url, Some(1), Some(1))).await;
        let err = result.expect("the handshake bound should end the connect").unwrap_err();
        assert!(format!("{err:#}").starts_with("Failed to connect to DAppProvider service"), "{err:#}");
        drop(listener);
    }

    // ---- blocking verification ----

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_leaves_the_runtime_thread_free() {
        let ticks = Arc::new(AtomicU32::new(0));
        let counter = ticks.clone();
        let ticker = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                counter.fetch_add(1, AtomicOrdering::Relaxed);
            }
        });
        let out = run_blocking_bounded(Duration::from_secs(5), "work", || {
            std::thread::sleep(Duration::from_millis(400));
            Ok(7u8)
        })
        .await
        .unwrap();
        ticker.abort();
        assert_eq!(out, 7);
        assert!(ticks.load(AtomicOrdering::Relaxed) >= 5, "the runtime kept running during the work");
    }

    #[tokio::test]
    async fn slow_or_panicking_work_becomes_an_error() {
        let started = std::time::Instant::now();
        let slow = run_blocking_bounded(Duration::from_millis(100), "work", || {
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(slow.starts_with("work timed out after"), "{slow}");
        assert!(started.elapsed() < Duration::from_secs(1));
        let panicked = run_blocking_bounded(Duration::from_secs(5), "work", || -> Result<()> { panic!("boom") })
            .await
            .unwrap_err()
            .to_string();
        assert!(panicked.starts_with("work failed:"), "{panicked}");
    }

    #[tokio::test]
    async fn bounded_verification_matches_a_direct_call() {
        let expectation = OperationExpectation::IssueTickets { lp_party: "lp".into(), ticket_count: 1 };
        let direct = tx_verifier::verify_and_hash(&[], "", "", &expectation, false);
        let bounded = verify_bounded(Vec::new(), String::new(), String::new(), expectation, false).await;
        match (direct, bounded) {
            (Ok(d), Ok(b)) => {
                assert_eq!((d.accepted, d.computed_hash, d.summary), (b.accepted, b.computed_hash, b.summary));
            }
            (Err(d), Err(b)) => assert_eq!(d.to_string(), b.to_string()),
            (d, b) => panic!("results differ: {d:?} vs {b:?}"),
        }
    }
}
