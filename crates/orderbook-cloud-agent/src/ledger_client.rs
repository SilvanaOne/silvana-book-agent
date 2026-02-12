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

use orderbook_agent_logic::auth::generate_jwt;
use message_signing::{
    sign_canonical, verify_canonical, parse_public_key,
    canonical_prepare_request, canonical_prepare_response,
    canonical_execute_request, canonical_execute_response,
    canonical_params_pay_fee, canonical_params_propose_dvp, canonical_params_accept_dvp,
    canonical_params_allocate, canonical_params_transfer_cc, canonical_params_request_preapproval,
    canonical_params_request_recurring_prepaid, canonical_params_request_recurring_payasyougo,
    canonical_params_request_user_service, canonical_params_transfer_cip56,
    canonical_params_accept_cip56,
};
use orderbook_proto::ledger::prepare_transaction_request::Params;
use tx_verifier::OperationExpectation;
use orderbook_proto::ledger::{
    d_app_provider_service_client::DAppProviderServiceClient,
    ActiveContractInfo, DiscoveredContract, ExecuteTransactionRequest,
    ExecuteTransactionResponse, GetActiveContractsRequest, GetBalancesRequest,
    GetDsoRatesRequest, GetDsoRatesResponse, GetLedgerEndRequest,
    GetPreapprovalsRequest, GetSettlementContractsRequest, GetUpdatesRequest,
    GetUpdatesResponse, MessageSignature, PreapprovalInfo, PrepareTransactionRequest,
    get_updates_response, ledger_event,
    PrepareTransactionResponse, TokenBalance,
};

/// Build canonical payload from a PrepareTransactionRequest for signing
fn build_canonical_from_prepare_request(req: &PrepareTransactionRequest) -> Result<Vec<u8>> {
    let params = req.params.as_ref().context("PrepareTransactionRequest missing params")?;
    let params_canonical = match params {
        Params::PayFee(p) => canonical_params_pay_fee(&p.proposal_id, &p.fee_type),
        Params::ProposeDvp(p) => canonical_params_propose_dvp(&p.proposal_id),
        Params::AcceptDvp(p) => canonical_params_accept_dvp(&p.proposal_id, &p.dvp_proposal_cid),
        Params::Allocate(p) => canonical_params_allocate(&p.proposal_id, &p.dvp_cid),
        Params::TransferCc(p) => canonical_params_transfer_cc(
            &p.receiver_party, &p.amount, p.description.as_deref(),
            &p.command_id, p.settlement_proposal_id.as_deref(),
        ),
        Params::RequestPreapproval(_) => canonical_params_request_preapproval(),
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
                    info!("JWT token refreshed (was expiring in {}s)", expires_at.saturating_sub(now));
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
    private_key_bytes: [u8; 32],
    /// Pre-configured ledger service public key for response signature verification
    ledger_service_public_key: [u8; 32],
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
    ) -> Result<Self> {
        let channel = Self::create_channel(grpc_url).await?;
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
        let client = DAppProviderServiceClient::with_interceptor(channel, interceptor);
        Ok(Self {
            client,
            private_key_bytes: *private_key_bytes,
            ledger_service_public_key: *ledger_service_public_key,
        })
    }

    /// Create gRPC channel with optional TLS
    async fn create_channel(grpc_url: &str) -> Result<Channel> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

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
                .connect()
                .await
                .context("Failed to connect to DAppProvider service")
        } else {
            Channel::from_shared(grpc_url.to_string())
                .context("Invalid gRPC URL")?
                .connect()
                .await
                .context("Failed to connect to DAppProvider service")
        }
    }

    // ========================================================================
    // Query RPCs
    // ========================================================================

    /// Get active contracts for the authenticated party (streaming RPC)
    pub async fn get_active_contracts(
        &mut self,
        template_filters: &[String],
    ) -> Result<Vec<ActiveContractInfo>> {
        let resp = self
            .client
            .get_active_contracts(GetActiveContractsRequest {
                template_filters: template_filters.to_vec(),
            })
            .await
            .context("GetActiveContracts RPC failed")?;

        let mut stream = resp.into_inner();
        let mut contracts = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(response) => {
                    if let Some(contract) = response.contract {
                        contracts.push(contract);
                    }
                }
                Err(e) => {
                    warn!("GetActiveContracts stream error: {}", e);
                    break;
                }
            }
        }
        Ok(contracts)
    }

    /// Get current ledger end offset
    pub async fn get_ledger_end(&mut self) -> Result<i64> {
        let resp = self
            .client
            .get_ledger_end(GetLedgerEndRequest {})
            .await
            .context("GetLedgerEnd RPC failed")?;
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
            .context("GetUpdates RPC failed")?;

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
            .context("GetBalances RPC failed")?;
        Ok(resp.into_inner().balances)
    }

    /// Fetch TransferPreapproval contracts
    pub async fn get_preapprovals(&mut self) -> Result<Vec<PreapprovalInfo>> {
        let resp = self
            .client
            .get_preapprovals(GetPreapprovalsRequest {})
            .await
            .context("GetPreapprovals RPC failed")?;
        Ok(resp.into_inner().preapprovals)
    }

    /// Get DSO rates (CC/USD rate, current round)
    pub async fn get_dso_rates(&mut self) -> Result<GetDsoRatesResponse> {
        let resp = self
            .client
            .get_dso_rates(GetDsoRatesRequest {})
            .await
            .context("GetDsoRates RPC failed")?;
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
            .context("GetSettlementContracts RPC failed")?;
        Ok(resp.into_inner().contracts)
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
            .context("PrepareTransaction RPC failed")?;
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

        Ok(response)
    }

    /// Execute a signed transaction (Phase 2)
    ///
    /// Signs the request with our Ed25519 key and verifies the response signature.
    pub async fn execute_transaction(
        &mut self,
        transaction_id: &str,
        signature: &str,
    ) -> Result<ExecuteTransactionResponse> {
        // Sign request
        let canonical = canonical_execute_request(transaction_id, signature);
        let sig_data = sign_canonical(&self.private_key_bytes, &canonical);

        let resp = self
            .client
            .execute_transaction(ExecuteTransactionRequest {
                transaction_id: transaction_id.to_string(),
                signature: signature.to_string(),
                request_signature: Some(MessageSignature {
                    signature: sig_data.signature_b64,
                    public_key: sig_data.public_key_b64url,
                    signing_scheme: sig_data.signing_scheme,
                }),
            })
            .await
            .context("ExecuteTransaction RPC failed")?;
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
            // 1. Prepare (fresh contracts each attempt — contracts may become stale)
            let prepared = self.prepare_transaction(req.clone()).await?;

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

            // 4. Execute (catch gRPC errors for update-based recovery)
            let result = match self
                .execute_transaction(&prepared.transaction_id, &signature)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    // gRPC/network error — transaction may have succeeded
                    if start_offset > 0 {
                        warn!("Execute error: {} — checking ledger updates", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        if let Some(recovered) = self.find_transaction_in_updates(
                            &prepared.command_id, start_offset
                        ).await {
                            info!("Transaction recovered via ledger updates: command_id={}, update_id={}",
                                prepared.command_id, recovered.update_id);
                            return Ok(recovered);
                        }
                    }
                    if attempt < max_retries - 1 {
                        let delay = BASE_DELAY_MS * 2_u64.pow(attempt);
                        let jitter = rand::thread_rng().gen_range(0..delay / 10 + 1);
                        warn!(
                            "Retrying after execute error (attempt {}/{}): {} — in {}ms [{}]",
                            attempt + 1, max_retries, e, delay + jitter, prepared.command_id
                        );
                        tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                        continue;
                    }
                    return Err(e);
                }
            };

            if !result.success {
                let error_msg = result.error_message.as_deref().unwrap_or("unknown error");

                // DUPLICATE_COMMAND: check ledger updates before giving up
                if error_msg.contains("DUPLICATE_COMMAND") {
                    if start_offset > 0 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        if let Some(recovered) = self.find_transaction_in_updates(
                            &prepared.command_id, start_offset
                        ).await {
                            info!("DUPLICATE_COMMAND recovered via ledger updates: command_id={}, update_id={}",
                                prepared.command_id, recovered.update_id);
                            return Ok(recovered);
                        }
                    }
                    anyhow::bail!("Command already submitted (DUPLICATE_COMMAND): {}", error_msg);
                }

                // INACTIVE_CONTRACTS: contract was consumed/archived — retrying the
                // same command will always fail. Bail immediately so the settlement
                // layer can re-discover contracts via sync_on_chain_contracts().
                if error_msg.contains("INACTIVE_CONTRACTS") {
                    warn!("Contract consumed, not retrying [{}]: {}", prepared.command_id, error_msg);
                    anyhow::bail!("Contract consumed (INACTIVE_CONTRACTS): {}", error_msg);
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
                anyhow::bail!("Transaction failed: {}", error_msg);
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
                        transaction_status: 0,
                        provider_error: None,
                    });
                }
            }
        }

        None
    }
}

/// Sign a hash with Ed25519 and return base64-encoded signature
fn sign_hash_bytes(private_key_bytes: &[u8; 32], hash_bytes: &[u8]) -> Result<String> {
    let signing_key = SigningKey::from_bytes(private_key_bytes);
    let signature = signing_key.sign(hash_bytes);
    Ok(BASE64.encode(signature.to_bytes()))
}

