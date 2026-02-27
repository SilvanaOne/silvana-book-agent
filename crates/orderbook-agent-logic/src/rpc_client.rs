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
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::Mutex as TokioMutex;
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::Request;
use tracing::info;

/// Global cached gRPC channel (created once, reused by all clients)
static CACHED_CHANNEL: OnceLock<Channel> = OnceLock::new();
/// Mutex to prevent multiple concurrent channel creation attempts
static CHANNEL_INIT_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();

fn get_init_lock() -> &'static TokioMutex<()> {
    CHANNEL_INIT_LOCK.get_or_init(|| TokioMutex::new(()))
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
};

/// Authentication interceptor for gRPC requests
#[derive(Clone)]
struct AuthInterceptor {
    token: Arc<RwLock<String>>,
}

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, tonic::Status> {
        let token = self.token.read().unwrap().clone();
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
}

impl OrderbookRpcClient {
    /// Get or create the global cached gRPC channel
    async fn get_or_create_channel(url: &str) -> Result<Channel> {
        // Fast path: channel already exists
        if let Some(channel) = CACHED_CHANNEL.get() {
            return Ok(channel.clone());
        }

        // Slow path: create channel (with lock to prevent races)
        let _guard = get_init_lock().lock().await;

        // Double-check after acquiring lock
        if let Some(channel) = CACHED_CHANNEL.get() {
            return Ok(channel.clone());
        }

        info!("Connecting to Orderbook RPC at {}", url);

        // Initialize Rustls crypto provider
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let channel = if url.starts_with("https://") {
            let domain = url
                .strip_prefix("https://")
                .and_then(|s| s.split('/').next())
                .and_then(|s| s.split(':').next())
                .ok_or_else(|| anyhow!("Failed to extract domain from URL"))?;

            let tls_config = ClientTlsConfig::new()
                .with_webpki_roots()
                .domain_name(domain);
            Channel::from_shared(url.to_string())?
                .tls_config(tls_config)?
                .timeout(std::time::Duration::from_secs(120))
                .connect()
                .await
                .map_err(|e| anyhow!("Failed to connect to {}: {}", url, e))?
        } else {
            Channel::from_shared(url.to_string())?
                .timeout(std::time::Duration::from_secs(120))
                .connect()
                .await
                .map_err(|e| anyhow!("Failed to connect to {}: {}", url, e))?
        };

        let _ = CACHED_CHANNEL.set(channel.clone());
        Ok(channel)
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
        let channel = Self::get_or_create_channel(url).await?;

        let token = Arc::new(RwLock::new(jwt.unwrap_or_default()));
        let auth_interceptor = AuthInterceptor { token: token.clone() };
        let settlement_client = SettlementServiceClient::with_interceptor(channel, auth_interceptor);

        Ok(Self {
            settlement_client,
            token,
        })
    }

    /// Update the JWT token for authentication
    pub fn set_jwt(&self, jwt: String) {
        *self.token.write().unwrap() = jwt;
    }

    /// Record a transaction in the transaction_history table
    ///
    /// Returns the auto-generated transaction ID
    pub async fn record_transaction(
        &mut self,
        request: RecordTransactionRequest,
    ) -> Result<u64> {
        let response = self
            .settlement_client
            .record_transaction(request)
            .await
            .map_err(|e| anyhow!("RecordTransaction RPC failed ({}): {}", e.code(), e.message()))?;

        let inner = response.into_inner();
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
        let response = self
            .settlement_client
            .record_settlement_event(request)
            .await
            .map_err(|e| anyhow!("RecordSettlementEvent RPC failed ({}): {}", e.code(), e.message()))?;

        let inner = response.into_inner();
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
        let response = self
            .settlement_client
            .save_disclosed_contract(request)
            .await
            .map_err(|e| anyhow!("SaveDisclosedContract RPC failed ({}): {}", e.code(), e.message()))?;

        let inner = response.into_inner();
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

        let response = self
            .settlement_client
            .get_settlement_proposal_by_id(request)
            .await
            .map_err(|e| anyhow!("GetSettlementProposalById RPC failed ({}): {}", e.code(), e.message()))?;

        let inner = response.into_inner();
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
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
        };

        let request = SubmitPreconfirmationRequest {
            auth: None, // External parties use metadata auth
            decision: Some(decision),
        };

        self.settlement_client
            .submit_preconfirmation(request)
            .await
            .map_err(|e| anyhow!("SubmitPreconfirmation RPC failed ({}): {}", e.code(), e.message()))?;

        Ok(())
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
        };

        let response = self
            .settlement_client
            .get_settlement_status(request)
            .await
            .map_err(|e| anyhow!("GetSettlementStatus RPC failed ({}): {}", e.code(), e.message()))?;

        Ok(response.into_inner())
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
