//! gRPC client for the DAppProviderService (CIP-0103)
//!
//! Provides query methods (balances, contracts, preapprovals, rates) and
//! two-phase transaction submission (prepare → sign → execute).
//!
//! Phase A: Signs the server-provided hash directly.
//! Phase B (future): tx-verifier will inspect + recompute hash before signing.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use once_cell::sync::Lazy;
use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use tokio_stream::StreamExt;
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::Request;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Maximum retries for transactions (from MAX_RETRIES env var, default 5)
static MAX_RETRIES: Lazy<u32> = Lazy::new(|| {
    std::env::var("MAX_RETRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
});

const BASE_DELAY_MS: u64 = 1000;

/// Duration in seconds to pause regular fee dispatch after SEQUENCER_BACKPRESSURE.
static FEE_PAUSE_SECS: Lazy<u64> = Lazy::new(|| {
    std::env::var("FEE_PAUSE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
});

/// Duration in seconds to pause traffic fee dispatch after SEQUENCER_BACKPRESSURE.
static TRAFFIC_FEE_PAUSE_SECS: Lazy<u64> = Lazy::new(|| {
    std::env::var("TRAFFIC_FEE_PAUSE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
});

/// Epoch millis after which regular fee dispatch may resume.
static FEE_PAUSE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Epoch millis after which traffic fee dispatch may resume.
static TRAFFIC_PAUSE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Record a backpressure event — pauses regular fees for FEE_PAUSE_SECS
/// and traffic fees for TRAFFIC_FEE_PAUSE_SECS.
pub fn signal_sequencer_backpressure() {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let fee_resume_at = now_ms + *FEE_PAUSE_SECS * 1000;
    FEE_PAUSE_UNTIL_MS.fetch_max(fee_resume_at, AtomicOrdering::Relaxed);

    let traffic_resume_at = now_ms + *TRAFFIC_FEE_PAUSE_SECS * 1000;
    TRAFFIC_PAUSE_UNTIL_MS.fetch_max(traffic_resume_at, AtomicOrdering::Relaxed);
}

/// Check whether regular fees are currently paused due to sequencer backpressure.
/// Returns Some(remaining_secs) if paused, None if not.
pub fn fee_pause_remaining() -> Option<u64> {
    let resume_at = FEE_PAUSE_UNTIL_MS.load(AtomicOrdering::Relaxed);
    if resume_at == 0 {
        return None;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if now_ms < resume_at {
        Some((resume_at - now_ms + 999) / 1000)
    } else {
        None
    }
}

/// Check whether traffic fees are currently paused due to sequencer backpressure.
/// Returns Some(remaining_secs) if paused, None if not.
pub fn traffic_fee_pause_remaining() -> Option<u64> {
    let resume_at = TRAFFIC_PAUSE_UNTIL_MS.load(AtomicOrdering::Relaxed);
    if resume_at == 0 {
        return None;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if now_ms < resume_at {
        Some((resume_at - now_ms + 999) / 1000)
    } else {
        None
    }
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
};

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
            &p.amount, p.reference.as_deref(),
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

/// Authentication interceptor for gRPC requests with automatic JWT refresh
#[derive(Clone)]
struct AuthInterceptor {
    token: Arc<RwLock<String>>,
    expires_at: Arc<RwLock<u64>>,
    party_id: String,
    role: String,
    private_key_bytes: [u8; 32],
    ttl_secs: u64,
    node_name: Option<String>,
}

/// Refresh JWT 5 minutes before expiry
const REFRESH_BEFORE_EXPIRY_SECS: u64 = 300;

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, tonic::Status> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expires_at = *self.expires_at.read().unwrap();

        if now + REFRESH_BEFORE_EXPIRY_SECS >= expires_at {
            match generate_jwt(
                &self.party_id,
                &self.role,
                &self.private_key_bytes,
                self.ttl_secs,
                self.node_name.as_deref(),
            ) {
                Ok(new_jwt) => {
                    debug!("JWT token refreshed (was expiring in {}s)", expires_at.saturating_sub(now));
                    *self.token.write().unwrap() = new_jwt;
                    *self.expires_at.write().unwrap() = now + self.ttl_secs;
                }
                Err(e) => {
                    tracing::error!("Failed to refresh JWT: {}", e);
                }
            }
        }

        let token = self.token.read().unwrap().clone();
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", token)
                .parse()
                .map_err(|_| tonic::Status::internal("Failed to parse JWT token"))?,
        );
        Ok(request)
    }
}

/// Client for the DAppProviderService gRPC API (CIP-0103)
pub struct DAppProviderClient {
    client: DAppProviderServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    /// The party id whose JWT this client carries. Used to bind fees
    /// authorization signatures to a specific party.
    party_id: String,
    private_key_bytes: [u8; 32],
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
    pub async fn new(
        grpc_url: &str,
        party_id: &str,
        role: &str,
        private_key_bytes: &[u8; 32],
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
        connection_timeout_secs: Option<u64>,
        request_timeout_secs: Option<u64>,
    ) -> Result<Self> {
        let channel = Self::create_channel(grpc_url, connection_timeout_secs, request_timeout_secs).await?;
        let jwt = generate_jwt(party_id, role, private_key_bytes, ttl_secs, node_name)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let interceptor = AuthInterceptor {
            token: Arc::new(RwLock::new(jwt)),
            expires_at: Arc::new(RwLock::new(now + ttl_secs)),
            party_id: party_id.to_string(),
            role: role.to_string(),
            private_key_bytes: *private_key_bytes,
            ttl_secs,
            node_name: node_name.map(|s| s.to_string()),
        };
        let client = DAppProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(16 * 1024 * 1024);
        Ok(Self {
            client,
            party_id: party_id.to_string(),
            private_key_bytes: *private_key_bytes,
            ledger_service_public_key: *ledger_service_public_key,
            topup_trigger: None,
        })
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
    pub fn from_channel(
        channel: Channel,
        party_id: &str,
        role: &str,
        private_key_bytes: &[u8; 32],
        ttl_secs: u64,
        node_name: Option<&str>,
        ledger_service_public_key: &[u8; 32],
    ) -> Result<Self> {
        let jwt = generate_jwt(party_id, role, private_key_bytes, ttl_secs, node_name)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let interceptor = AuthInterceptor {
            token: Arc::new(RwLock::new(jwt)),
            expires_at: Arc::new(RwLock::new(now + ttl_secs)),
            party_id: party_id.to_string(),
            role: role.to_string(),
            private_key_bytes: *private_key_bytes,
            ttl_secs,
            node_name: node_name.map(|s| s.to_string()),
        };
        let client = DAppProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(16 * 1024 * 1024);
        Ok(Self {
            client,
            party_id: party_id.to_string(),
            private_key_bytes: *private_key_bytes,
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
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let connect_timeout = Duration::from_secs(connect_timeout_secs.unwrap_or(30));
        let request_timeout = Duration::from_secs(request_timeout_secs.unwrap_or(120));

        if grpc_url.starts_with("https://") {
            let tls_config = ClientTlsConfig::new().with_webpki_roots().domain_name(
                grpc_url
                    .trim_start_matches("https://")
                    .split(':')
                    .next()
                    .unwrap_or("localhost"),
            );

            Channel::from_shared(grpc_url.to_string())
                .context("Invalid gRPC URL")?
                .tls_config(tls_config)
                .context("Failed to configure TLS")?
                .connect_timeout(connect_timeout)
                .timeout(request_timeout)
                .connect()
                .await
                .context("Failed to connect to DAppProvider service")
        } else {
            Channel::from_shared(grpc_url.to_string())
                .context("Invalid gRPC URL")?
                .connect_timeout(connect_timeout)
                .timeout(request_timeout)
                .connect()
                .await
                .context("Failed to connect to DAppProvider service")
        }
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
        let resp = self
            .client
            .get_active_contracts(GetActiveContractsRequest {
                template_filters: template_filters.to_vec(),
            })
            .await
            .map_err(|s| anyhow!("GetActiveContracts RPC failed ({}): {}", s.code(), s.message()))?;

        let mut stream = resp.into_inner();
        let mut contracts = Vec::new();
        let mut complete = true;
        while let Some(item) = stream.next().await {
            match item {
                Ok(response) => {
                    if let Some(contract) = response.contract {
                        contracts.push(contract);
                    }
                }
                Err(e) => {
                    warn!(
                        "GetActiveContracts stream error after {} contract(s): {}",
                        contracts.len(),
                        e
                    );
                    complete = false;
                    break;
                }
            }
        }
        Ok((contracts, complete))
    }

    /// Get current ledger end offset
    pub async fn get_ledger_end(&mut self) -> Result<i64> {
        let resp = self
            .client
            .get_ledger_end(GetLedgerEndRequest {})
            .await
            .map_err(|s| anyhow!("GetLedgerEnd RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner().offset)
    }

    /// Get ledger updates from a given offset range (streaming RPC, collected into Vec)
    pub async fn get_updates(
        &mut self,
        begin_exclusive: i64,
        end_inclusive: Option<i64>,
        template_filters: &[String],
    ) -> Result<Vec<GetUpdatesResponse>> {
        let resp = self
            .client
            .get_updates(GetUpdatesRequest {
                begin_exclusive,
                end_inclusive,
                template_filters: template_filters.to_vec(),
            })
            .await
            .map_err(|s| anyhow!("GetUpdates RPC failed ({}): {}", s.code(), s.message()))?;

        let mut stream = resp.into_inner();
        let mut updates = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(response) => {
                    updates.push(response);
                }
                Err(e) => {
                    warn!("GetUpdates stream error: {}", e);
                    break;
                }
            }
        }
        Ok(updates)
    }

    /// Get token balances
    pub async fn get_balances(&mut self) -> Result<Vec<TokenBalance>> {
        let resp = self
            .client
            .get_balances(GetBalancesRequest {})
            .await
            .map_err(|s| anyhow!("GetBalances RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner().balances)
    }

    /// Get off-chain prepaid traffic balance + credit limit for the
    /// authenticated party. Returns rust_decimal::Decimal values parsed at
    /// the boundary so callers don't have to handle decimal strings.
    pub async fn get_prepaid_traffic_balance(
        &mut self,
    ) -> Result<crate::PrepaidTrafficBalance> {
        let resp = self
            .client
            .get_prepaid_traffic_balance(GetPrepaidTrafficBalanceRequest {})
            .await
            .map_err(|s| anyhow!("GetPrepaidTrafficBalance RPC failed ({}): {}", s.code(), s.message()))?;
        let r = resp.into_inner();
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
        let resp = self
            .client
            .get_preapprovals(GetPreapprovalsRequest {})
            .await
            .map_err(|s| anyhow!("GetPreapprovals RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner().preapprovals)
    }

    /// Get DSO rates (CC/USD rate, current round)
    pub async fn get_dso_rates(&mut self) -> Result<GetDsoRatesResponse> {
        let resp = self
            .client
            .get_dso_rates(GetDsoRatesRequest {})
            .await
            .map_err(|s| anyhow!("GetDsoRates RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner())
    }

    /// Discover on-chain DvpProposal/Dvp contracts for active settlements
    pub async fn get_settlement_contracts(
        &mut self,
        settlement_ids: &[String],
    ) -> Result<Vec<DiscoveredContract>> {
        let resp = self
            .client
            .get_settlement_contracts(GetSettlementContractsRequest {
                settlement_ids: settlement_ids.to_vec(),
            })
            .await
            .map_err(|s| anyhow!("GetSettlementContracts RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner().contracts)
    }

    /// Get unlocked amulets via the dedicated GetAmulets RPC
    pub async fn get_amulets(&mut self) -> Result<Vec<crate::acs_worker::AmuletInfo>> {
        use orderbook_proto::ledger::GetAmuletsRequest;

        let resp = self
            .client
            .get_amulets(GetAmuletsRequest {})
            .await
            .map_err(|s| anyhow!("GetAmulets RPC failed ({}): {}", s.code(), s.message()))?;

        Ok(resp
            .into_inner()
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
        let sig_data = sign_canonical(&self.private_key_bytes, &canonical);
        req.request_signature = Some(MessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        });

        let resp = self
            .client
            .prepare_transaction(req)
            .await
            .map_err(|s| anyhow!("PrepareTransaction RPC failed ({}): {}", s.code(), s.message()))?;
        let response = resp.into_inner();

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
        let sig_data = sign_canonical(&self.private_key_bytes, &canonical);

        // Independently sign the context-bound fees authorization. Bound to
        // (party, transaction_id, fees_json) — single-use because the
        // server's SessionStore consumes transaction_id atomically.
        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            transaction_id,
            fees_json,
        );
        let fees_auth_data = sign_canonical(&self.private_key_bytes, &fees_canonical);

        let resp = self
            .client
            .execute_transaction(ExecuteTransactionRequest {
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
            })
            .await
            .map_err(|s| anyhow!("ExecuteTransaction RPC failed ({}): {}", s.code(), s.message()))?;
        let response = resp.into_inner();

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
        let max_retries = *MAX_RETRIES;

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
            // 1. Prepare (fresh contracts each attempt — contracts may become stale).
            //    A prepare failure propagates out of submit_transaction, so signal
            //    the ledger-health breaker here too — otherwise an outage that
            //    manifests at prepare (participant / ledger-service unreachable)
            //    would never trip it and the agent would keep quoting into it.
            let prepared = match self.prepare_transaction(req.clone()).await {
                Ok(p) => p,
                Err(e) => {
                    if agent_logic::ledger_health::is_sequencer_unreachable(&format!("{:#}", e)) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    return Err(e);
                }
            };

            info!(
                "Transaction prepared: id={}, command_id={}, traffic={}",
                prepared.transaction_id,
                prepared.command_id,
                prepared.traffic_estimate.as_ref().map(|t| t.total_bytes).unwrap_or(0),
            );

            // 2. Verify transaction and compute hash
            let verification = tx_verifier::verify_and_hash(
                &prepared.prepared_transaction,
                &prepared.prepared_transaction_hash,
                &prepared.hashing_scheme_version,
                expectation,
                verbose,
            )?;

            for w in &verification.warnings {
                warn!("TX verification: {}", w);
            }

            if dry_run {
                println!("--- DRY RUN ---");
                println!("Inspection: {}", if verification.accepted { "ACCEPTED" } else { "REJECTED" });
                println!("Summary: {}", verification.summary);
                if let Some(reason) = &verification.rejection_reason {
                    println!("Rejection: {}", reason);
                }
                for w in &verification.warnings {
                    println!("Warning: {}", w);
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
                println!("Hash: {}", hash_status);
                println!("--- NOT SIGNED, NOT EXECUTED ---");
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
                    anyhow::bail!(
                        "Transaction verification REJECTED: {}",
                        verification.rejection_reason.unwrap_or_default()
                    );
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

            let signature = sign_hash_bytes(&self.private_key_bytes, &hash_to_sign)?;

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
                    if start_offset > 0 {
                        warn!("Execute error: {:#} — checking ledger updates", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        if let Some(recovered) = self.find_transaction_in_updates(
                            &prepared.command_id, start_offset
                        ).await {
                            info!("Transaction recovered via ledger updates: command_id={}, update_id={}",
                                prepared.command_id, recovered.update_id);
                            // The tx landed despite the gRPC error — ledger is reachable.
                            agent_logic::ledger_health::record_submit_success();
                            return Ok(recovered);
                        }
                    }
                    if attempt < max_retries - 1 {
                        let delay = BASE_DELAY_MS * 2_u64.pow(attempt);
                        let jitter = rand::thread_rng().gen_range(0..delay / 10 + 1);
                        warn!(
                            "Retrying after execute error (attempt {}/{}): {:#} — in {}ms [{}]",
                            attempt + 1, max_retries, e, delay + jitter, prepared.command_id
                        );
                        tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                        continue;
                    }
                    // Terminal failure for this submission (retries exhausted). Signal
                    // the ledger-health breaker once per submit_transaction call, only
                    // for true sequencer-unreachable errors (not business rejections).
                    if agent_logic::ledger_health::is_sequencer_unreachable(&format!("{:#}", e)) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    return Err(e);
                }
            };

            if !result.success {
                let error_msg = result.error_message.as_deref().unwrap_or("unknown error");

                // SEQUENCER_BACKPRESSURE: signal fee pause regardless of retry outcome
                if error_msg.contains("SEQUENCER_BACKPRESSURE") {
                    warn!(
                        "SEQUENCER_BACKPRESSURE detected (attempt {}/{}), pausing fees {}s / traffic {}s [{}]",
                        attempt + 1, max_retries, *FEE_PAUSE_SECS, *TRAFFIC_FEE_PAUSE_SECS, prepared.command_id
                    );
                    signal_sequencer_backpressure();
                }

                // DUPLICATE_COMMAND: check ledger updates before giving up
                if error_msg.contains("DUPLICATE_COMMAND") {
                    if start_offset > 0 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        if let Some(recovered) = self.find_transaction_in_updates(
                            &prepared.command_id, start_offset
                        ).await {
                            info!("DUPLICATE_COMMAND recovered via ledger updates: command_id={}, update_id={}",
                                prepared.command_id, recovered.update_id);
                            agent_logic::ledger_health::record_submit_success();
                            return Ok(recovered);
                        }
                    }
                    // The command was accepted by the ledger (duplicate) — ledger is up.
                    agent_logic::ledger_health::record_submit_success();
                    anyhow::bail!("Command already submitted (DUPLICATE_COMMAND): {}", error_msg);
                }

                // INACTIVE_CONTRACTS: contract was consumed/archived between
                // prepare and execute. Re-prepare to get fresh CIDs from ACS.
                if error_msg.contains("INACTIVE_CONTRACTS") {
                    if attempt < max_retries - 1 {
                        warn!(
                            "INACTIVE_CONTRACTS (attempt {}/{}), re-preparing with fresh CIDs in 2s [{}]",
                            attempt + 1, max_retries, prepared.command_id
                        );
                        tokio::time::sleep(Duration::from_millis(2000)).await;
                        continue;
                    }
                    anyhow::bail!("INACTIVE_CONTRACTS after {} attempts: {}", max_retries, error_msg);
                }

                if attempt < max_retries - 1 {
                    let delay = BASE_DELAY_MS * 2_u64.pow(attempt);
                    let jitter = rand::thread_rng().gen_range(0..delay / 10 + 1);
                    warn!(
                        "Transaction error (attempt {}/{}): {} — retrying in {}ms [{}]",
                        attempt + 1, max_retries, error_msg, delay + jitter, prepared.command_id
                    );
                    tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                    continue;
                }
                // Terminal failure for this submission (retries exhausted). Signal the
                // ledger-health breaker once per call, only for sequencer-unreachable
                // errors (SEQUENCER_BACKPRESSURE already handled above via its own pause).
                if agent_logic::ledger_health::is_sequencer_unreachable(error_msg) {
                    agent_logic::ledger_health::record_submit_failure();
                }
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

        unreachable!("Retry loop should have returned or errored")
    }

    /// Search ledger updates for a transaction with the given command_id.
    /// Returns a synthetic ExecuteTransactionResponse if found.
    /// Follows the pattern from dvp_settle.rs find_dvp_settle_update().
    async fn find_transaction_in_updates(
        &mut self,
        command_id: &str,
        start_offset: i64,
    ) -> Option<ExecuteTransactionResponse> {
        let current_end = self.get_ledger_end().await.ok()?;
        if current_end <= start_offset {
            return None;
        }

        let updates = self.get_updates(start_offset, Some(current_end), &[]).await.ok()?;

        for update_resp in &updates {
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

                    return Some(ExecuteTransactionResponse {
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
                    });
                }
            }
        }

        None
    }

    /// Request tokens from the faucet
    pub async fn request_faucet(&mut self, mut req: FaucetRequest) -> Result<FaucetResponse> {
        // Sign the request
        let canonical = canonical_params_faucet(
            &req.token_name, &req.token_admin, &req.ticket, req.dry_run,
        );
        let canonical_bytes = canonical_prepare_request(0, &canonical);
        let sig_data = sign_canonical(&self.private_key_bytes, &canonical_bytes);
        req.request_signature = Some(MessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        });

        let response = self.client.request_faucet(req).await
            .map_err(|s| anyhow!("RequestFaucet RPC failed ({}): {}", s.code(), s.message()))?;

        Ok(response.into_inner())
    }

    /// List instruments supported by the faucet (drives both faucet calls and
    /// CIP-56 preapproval creation on the agent side).
    pub async fn list_faucet_instruments(&mut self) -> Result<Vec<FaucetInstrument>> {
        let response = self
            .client
            .list_faucet_instruments(ListFaucetInstrumentsRequest {})
            .await
            .map_err(|s| anyhow!("ListFaucetInstruments RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(response.into_inner().instruments)
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
        let sig_data = message_signing::sign_canonical(&self.private_key_bytes, &canonical);

        let resp = self
            .client
            .prepare_pay_fee(PreparePayFeeRequest {
                proposal_id: proposal_id.to_string(),
                fee_type: fee_type.to_string(),
                request_signature: Some(MessageSignature {
                    signature: sig_data.signature_b64,
                    public_key: sig_data.public_key_b64url,
                    signing_scheme: sig_data.signing_scheme,
                }),
            })
            .await
            .map_err(|s| anyhow!("PreparePayFee RPC failed ({}): {}", s.code(), s.message()))?;
        let response = resp.into_inner();

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
        let sig_data = message_signing::sign_canonical(&self.private_key_bytes, &canonical);

        let fees_canonical = message_signing::canonical_pay_fee_authorization(
            &self.party_id,
            session_id,
            proposal_id,
            fee_type,
            fees_json,
        );
        let fees_auth_data =
            message_signing::sign_canonical(&self.private_key_bytes, &fees_canonical);

        let resp = self
            .client
            .execute_pay_fee(ExecutePayFeeRequest {
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
            })
            .await
            .map_err(|s| anyhow!("ExecutePayFee RPC failed ({}): {}", s.code(), s.message()))?;
        let response = resp.into_inner();

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

/// True when an error from [`AtomicProviderClient::submit_atomic_transaction`]
/// means the transaction may have committed (recovery scan unavailable) — the
/// caller must reconcile before retrying with different inputs.
pub fn is_ambiguous_execute_error(err: &anyhow::Error) -> bool {
    format!("{:#}", err).contains(ATOMIC_EXECUTE_AMBIGUOUS)
}

/// Marker for "the signed quote window closed before we could retry". NOT
/// ambiguous: the execute was rejected, so nothing committed and the caller can
/// re-quote immediately. Distinct from a bare deadline-exceeded DAML_FAILURE,
/// which is what we get if we re-submit into a dead window instead of stopping.
pub const QUOTE_WINDOW_CLOSED: &str = "QUOTE_WINDOW_CLOSED";

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
            now_micros + crate::atomic_swap::PRECHECK_VALIDITY_MARGIN_MICROS >= valid_until
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
    party_id: String,
    private_key_bytes: [u8; 32],
    ledger_service_public_key: [u8; 32],
}

impl AtomicProviderClient {
    /// Create a new AtomicProviderClient (same 9-arg shape as DAppProviderClient::new)
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        grpc_url: &str,
        party_id: &str,
        role: &str,
        private_key_bytes: &[u8; 32],
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
        let jwt = generate_jwt(party_id, role, private_key_bytes, ttl_secs, node_name)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let interceptor = AuthInterceptor {
            token: Arc::new(RwLock::new(jwt)),
            expires_at: Arc::new(RwLock::new(now + ttl_secs)),
            party_id: party_id.to_string(),
            role: role.to_string(),
            private_key_bytes: *private_key_bytes,
            ttl_secs,
            node_name: node_name.map(|s| s.to_string()),
        };
        let client = AtomicDvpProviderServiceClient::with_interceptor(channel, interceptor)
            .max_decoding_message_size(16 * 1024 * 1024);
        Ok(Self {
            client,
            party_id: party_id.to_string(),
            private_key_bytes: *private_key_bytes,
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

    fn sign_as_atomic(&self, canonical: &[u8]) -> AtomicMessageSignature {
        let sig_data = sign_canonical(&self.private_key_bytes, canonical);
        AtomicMessageSignature {
            signature: sig_data.signature_b64,
            public_key: sig_data.public_key_b64url,
            signing_scheme: sig_data.signing_scheme,
        }
    }

    /// Prepare an atomic transaction (Phase 1): sign the request, verify the
    /// response + fees signatures.
    pub async fn prepare_atomic(
        &mut self,
        mut req: PrepareAtomicTransactionRequest,
    ) -> Result<PrepareAtomicTransactionResponse> {
        let canonical = build_canonical_from_prepare_atomic_request(&req)?;
        req.request_signature = Some(self.sign_as_atomic(&canonical));

        let resp = self
            .client
            .prepare_atomic_transaction(req)
            .await
            .map_err(|s| {
                anyhow!("PrepareAtomicTransaction RPC failed ({}): {}", s.code(), s.message())
            })?;
        let response = resp.into_inner();

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
        let request_signature = Some(self.sign_as_atomic(&canonical));

        let fees_canonical = message_signing::canonical_tx_fees_authorization(
            &self.party_id,
            transaction_id,
            fees_json,
        );
        let fees_authorization = Some(self.sign_as_atomic(&fees_canonical));

        let resp = self
            .client
            .execute_atomic_transaction(ExecuteAtomicTransactionRequest {
                transaction_id: transaction_id.to_string(),
                signature: signature.to_string(),
                fees_json: fees_json.to_string(),
                request_signature,
                fees_authorization,
            })
            .await
            .map_err(|s| {
                anyhow!("ExecuteAtomicTransaction RPC failed ({}): {}", s.code(), s.message())
            })?;
        let response = resp.into_inner();

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
        let resp = self
            .client
            .get_atomic_contracts(GetAtomicContractsRequest {
                template_ids: template_ids.to_vec(),
                contract_ids: contract_ids.to_vec(),
            })
            .await
            .map_err(|s| anyhow!("GetAtomicContracts RPC failed ({}): {}", s.code(), s.message()))?;
        Ok(resp.into_inner())
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
        let max_retries = *MAX_RETRIES;

        // An AtomicDVP settle carries a SIGNED deadline: the on-ledger choice
        // aborts with `deadline-exceeded` once ledger time passes it. Every
        // `continue` below re-prepares from scratch, so without this a slow
        // failure (notably MEDIATOR_SAYS_TX_TIMED_OUT, which can surface later
        // than the whole validity window) burns its retries re-submitting into
        // a window that has already closed — turning a clean re-quote into a
        // DAML_FAILURE. `None` for every other operation: they have no
        // deadline and keep the previous behavior exactly.
        let quote_deadline_micros = quote_deadline_of(expectation);
        let window_closed =
            || quote_window_closed(quote_deadline_micros, chrono::Utc::now().timestamp_micros());

        for attempt in 0..max_retries {
            // Re-check after the backoff sleep, not just before it: the delay
            // grows as 1000 * 2^attempt ms, so a late retry can outlive the
            // window it was cleared against.
            if attempt > 0 && window_closed() {
                anyhow::bail!("{QUOTE_WINDOW_CLOSED}: window closed during retry backoff");
            }

            // 1. Prepare (fresh contracts each attempt — contracts may become stale)
            let prepared = match self.prepare_atomic(req.clone()).await {
                Ok(p) => p,
                Err(e) => {
                    if agent_logic::ledger_health::is_sequencer_unreachable(&format!("{:#}", e)) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
                    return Err(e);
                }
            };

            info!(
                "Atomic transaction prepared: id={}, command_id={}",
                prepared.transaction_id, prepared.command_id,
            );

            // 2. Verify transaction and compute hash
            let verification = tx_verifier::verify_and_hash(
                &prepared.prepared_transaction,
                &prepared.prepared_transaction_hash,
                &prepared.hashing_scheme_version,
                expectation,
                verbose,
            )?;

            for w in &verification.warnings {
                warn!("TX verification: {}", w);
            }

            if dry_run {
                println!("--- DRY RUN (atomic) ---");
                println!("Inspection: {}", if verification.accepted { "ACCEPTED" } else { "REJECTED" });
                println!("Summary: {}", verification.summary);
                if let Some(reason) = &verification.rejection_reason {
                    println!("Rejection: {}", reason);
                }
                println!("--- NOT SIGNED, NOT EXECUTED ---");
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
                    anyhow::bail!(
                        "Atomic transaction verification REJECTED: {}",
                        verification.rejection_reason.unwrap_or_default()
                    );
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
            let signature = sign_hash_bytes(&self.private_key_bytes, &hash_to_sign)?;

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
                    if agent_logic::ledger_health::is_sequencer_unreachable(&format!("{:#}", e)) {
                        agent_logic::ledger_health::record_submit_failure();
                    }
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
                        attempt + 1, max_retries, prepared.command_id
                    );
                    signal_sequencer_backpressure();
                }

                // DUPLICATE_COMMAND: the command was accepted earlier — commit
                // status unknown without an update scan.
                if error_msg.contains("DUPLICATE_COMMAND") {
                    agent_logic::ledger_health::record_submit_success();
                    return Err(anyhow!(
                        "{}: DUPLICATE_COMMAND (an earlier submission likely committed): {}",
                        ATOMIC_EXECUTE_AMBIGUOUS,
                        error_msg
                    ));
                }

                // INACTIVE_CONTRACTS: safe to re-prepare (nothing committed)
                if error_msg.contains("INACTIVE_CONTRACTS") {
                    if attempt < max_retries - 1 && !window_closed() {
                        warn!(
                            "INACTIVE_CONTRACTS on atomic tx (attempt {}/{}), re-preparing in 2s [{}]",
                            attempt + 1, max_retries, prepared.command_id
                        );
                        tokio::time::sleep(Duration::from_millis(2000)).await;
                        continue;
                    }
                    if window_closed() {
                        anyhow::bail!("{QUOTE_WINDOW_CLOSED}: {error_msg}");
                    }
                    anyhow::bail!("INACTIVE_CONTRACTS after {} attempts: {}", max_retries, error_msg);
                }

                if window_closed() {
                    // Nothing committed (the execute was rejected), so this is a
                    // clean re-quote — not an ambiguous outcome.
                    anyhow::bail!("{QUOTE_WINDOW_CLOSED}: {error_msg}");
                }

                if attempt < max_retries - 1 {
                    let delay = BASE_DELAY_MS * 2_u64.pow(attempt);
                    let jitter = rand::thread_rng().gen_range(0..delay / 10 + 1);
                    warn!(
                        "Atomic transaction error (attempt {}/{}): {} — retrying in {}ms [{}]",
                        attempt + 1, max_retries, error_msg, delay + jitter, prepared.command_id
                    );
                    tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                    continue;
                }
                if agent_logic::ledger_health::is_sequencer_unreachable(error_msg) {
                    agent_logic::ledger_health::record_submit_failure();
                }
                anyhow::bail!("Atomic transaction failed: {}", error_msg);
            }

            agent_logic::ledger_health::record_submit_success();
            return Ok(result);
        }

        unreachable!("Retry loop should have returned or errored")
    }
}

#[cfg(test)]
mod quote_window_tests {
    use super::*;
    use crate::atomic_swap::PRECHECK_VALIDITY_MARGIN_MICROS as MARGIN;

    fn atomic_settle(valid_until_micros: i64) -> OperationExpectation {
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
