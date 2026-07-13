//! gRPC client wrapper for orderbook and pricing services
//!
//! Provides a unified interface for interacting with the orderbook gRPC service,
//! including authentication, price fetching, order submission, cancellation,
//! and settlement streaming.

use anyhow::{Context, Result};
use orderbook_proto::{
    pricing::{pricing_service_client::PricingServiceClient, GetPriceRequest, GetPriceResponse},
    orderbook::{
        orderbook_service_client::OrderbookServiceClient, CancelOrderRequest, CancelOrderResponse,
        GetInstrumentsRequest, GetMarketsRequest, GetOrdersRequest, Instrument, Market, Order, OrderStatus, OrderType,
        SubmitOrderRequest, SubmitOrderResponse, TimeInForce,
        SubscribeSettlementsRequest, SettlementUpdate,
        GetSettlementProposalsRequest, SettlementProposal, SettlementStatus,
        RequestQuotesRequest, RequestQuotesResponse,
        AcceptQuoteRequest, AcceptQuoteResponse,
        GetRoundsDataRequest, GetRoundsDataResponse,
    },
    rfqv2::{
        rfq_v2_service_client::RfqV2ServiceClient,
        AcceptQuoteAtomicRequest, AcceptQuoteAtomicResponse, RfqConfirmRejectReason,
        RequestQuotesV2Request, RequestQuotesV2Response,
    },
};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::Request;
use tokio_stream::Stream;
use std::pin::Pin;
use tracing::debug;

use crate::auth::generate_jwt;
use crate::config::BaseConfig;

/// External account authentication data
#[derive(Clone)]
struct ExternalAuthData {
    party_id: String,
    public_key_hex: String,
    private_key_bytes: [u8; 32],
    role: String,
    ttl_secs: u64,
    node_name: String,
}

/// Unified client for orderbook and pricing services
pub struct OrderbookClient {
    pricing_client: PricingServiceClient<tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>>,
    orderbook_client: OrderbookServiceClient<tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>>,
    /// RFQ V2 (AtomicDVP) user-facing service — same channel/auth as v1
    rfqv2_client: RfqV2ServiceClient<tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>>,
    // Raw client for streaming (interceptors don't work well with streaming)
    raw_orderbook_client: OrderbookServiceClient<Channel>,
    auth_data: ExternalAuthData,
}

impl OrderbookClient {
    /// Create a new orderbook client with external account authentication
    pub async fn new(config: &BaseConfig) -> Result<Self> {
        let channel = Self::create_channel(&config.orderbook_grpc_url).await?;

        // Generate initial JWT for interceptor
        let jwt = generate_jwt(
            &config.party_id,
            &config.role,
            &config.private_key_bytes,
            config.token_ttl_secs,
            Some(config.node_name.as_str()),
        )?;

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let token_arc = Arc::new(RwLock::new(jwt));
        let expires_at = Arc::new(RwLock::new(now + config.token_ttl_secs));

        let auth_data = ExternalAuthData {
            party_id: config.party_id.clone(),
            public_key_hex: config.public_key_hex.clone(),
            private_key_bytes: config.private_key_bytes,
            role: config.role.clone(),
            ttl_secs: config.token_ttl_secs,
            node_name: config.node_name.clone(),
        };

        let auth_interceptor = AuthInterceptor {
            token: token_arc.clone(),
            expires_at: expires_at.clone(),
            auth_data: auth_data.clone(),
        };

        let pricing_client = PricingServiceClient::with_interceptor(
            channel.clone(),
            auth_interceptor.clone(),
        )
        .max_decoding_message_size(16 * 1024 * 1024);

        let orderbook_client = OrderbookServiceClient::with_interceptor(
            channel.clone(),
            auth_interceptor.clone(),
        )
        .max_decoding_message_size(16 * 1024 * 1024);

        let rfqv2_client = RfqV2ServiceClient::with_interceptor(
            channel.clone(),
            auth_interceptor,
        )
        .max_decoding_message_size(16 * 1024 * 1024);

        let raw_orderbook_client = OrderbookServiceClient::new(channel)
            .max_decoding_message_size(16 * 1024 * 1024);

        Ok(Self {
            pricing_client,
            orderbook_client,
            rfqv2_client,
            raw_orderbook_client,
            auth_data,
        })
    }

    /// Create gRPC channel with TLS
    async fn create_channel(grpc_url: &str) -> Result<Channel> {
        // Initialize Rustls crypto provider
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        if grpc_url.starts_with("https://") {
            // Use embedded webpki-roots (Mozilla root certificates compiled into binary)
            let tls_config = ClientTlsConfig::new()
                .with_webpki_roots()
                .domain_name(
                    grpc_url
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
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
                .context("Failed to connect to gRPC service")
        } else {
            // Plain HTTP connection for local testing
            Channel::from_shared(grpc_url.to_string())
                .context("Invalid gRPC URL")?
                .connect()
                .await
                .context("Failed to connect to gRPC service")
        }
    }

    /// Get current price for a market
    pub async fn get_price(&mut self, market_id: &str) -> Result<GetPriceResponse> {
        let request = Request::new(GetPriceRequest {
            market_id: market_id.to_string(),
            source: None,
        });

        let response = self
            .pricing_client
            .get_price(request)
            .await
            .map_err(|e| anyhow::anyhow!("get_price failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Get all active markets (includes tick_size)
    pub async fn get_markets(&mut self) -> Result<Vec<Market>> {
        let request = Request::new(GetMarketsRequest {
            market_type: None,
            base_instrument: None,
            quote_instrument: None,
            active_only: Some(true),
            limit: None,
            offset: None,
        });

        let response = self
            .orderbook_client
            .get_markets(request)
            .await
            .map_err(|e| anyhow::anyhow!("get_markets failed: {}", e.message()))?;

        Ok(response.into_inner().markets)
    }

    /// Get all instruments (includes registry for DVP term verification).
    ///
    /// Canton Coin is identified on the client by `instrument_type == "token"`;
    /// its `instrument_id` drives the CC → Amulet translation in
    /// `BaseConfig::resolve_instrument`, its `registry` is the DSO party.
    pub async fn get_instruments(&mut self) -> Result<Vec<Instrument>> {
        let request = Request::new(GetInstrumentsRequest {
            instrument_type: None,
            limit: None,
            offset: None,
        });

        let response = self
            .orderbook_client
            .get_instruments(request)
            .await
            .map_err(|e| anyhow::anyhow!("get_instruments failed: {}", e.message()))?;

        Ok(response.into_inner().instruments)
    }

    /// Submit a new order with pre-computed signature fields
    pub async fn submit_order(
        &mut self,
        market_id: &str,
        order_type: OrderType,
        price: String,
        quantity: String,
        trader_order_ref: Option<String>,
        signature: Option<String>,
        signed_data: Vec<u8>,
        nonce: u64,
    ) -> Result<SubmitOrderResponse> {
        let request = Request::new(SubmitOrderRequest {
            market_id: market_id.to_string(),
            order_type: order_type as i32,
            price,
            quantity,
            time_in_force: TimeInForce::Gtc as i32, // Good Till Cancel
            expires_at: None,
            trader_order_ref,
            credentials: None,
            requirements: None,
            metadata: None,
            signature,
            signed_data,
            nonce,
            // LP resting orders must fill retail flow: opt out of the LP-only
            // counterparty filter (retail orders default it to true).
            only_liquidity_providers: Some(false),
            liquidity_provider_name: None,
        });

        let response = self
            .orderbook_client
            .submit_order(request)
            .await
            .map_err(|e| anyhow::anyhow!("submit_order failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Cancel an existing order
    pub async fn cancel_order(&mut self, order_id: u64) -> Result<CancelOrderResponse> {
        let request = Request::new(CancelOrderRequest {
            order_id,
        });

        let response = self
            .orderbook_client
            .cancel_order(request)
            .await
            .map_err(|e| anyhow::anyhow!("Cancel order failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Get live orders for a market (Active + Partial)
    pub async fn get_active_orders(&mut self, market_id: &str) -> Result<Vec<Order>> {
        let mut orders = Vec::new();

        for status in [OrderStatus::Active, OrderStatus::Partial] {
            let request = Request::new(GetOrdersRequest {
                market_id: Some(market_id.to_string()),
                status: Some(status as i32),
                order_type: None,
                limit: None,
                offset: None,
                liquidity_provider_active_seconds: None,
                liquidity_provider_names: vec![],
            });

            let response = self
                .orderbook_client
                .get_orders(request)
                .await
                .map_err(|e| anyhow::anyhow!("get_orders failed: {}", e.message()))?;

            orders.extend(response.into_inner().orders);
        }

        Ok(orders)
    }

    /// Get ALL live orders for this party (across all markets)
    pub async fn get_all_active_orders(&mut self) -> Result<Vec<Order>> {
        let mut orders = Vec::new();

        for status in [OrderStatus::Active, OrderStatus::Partial] {
            let request = Request::new(GetOrdersRequest {
                market_id: None,
                status: Some(status as i32),
                order_type: None,
                limit: None,
                offset: None,
                liquidity_provider_active_seconds: None,
                liquidity_provider_names: vec![],
            });

            let response = self
                .orderbook_client
                .get_orders(request)
                .await
                .map_err(|e| anyhow::anyhow!("get_all_active_orders failed: {}", e.message()))?;

            orders.extend(response.into_inner().orders);
        }

        Ok(orders)
    }

    /// Get pending settlement proposals for this party (paginated, fetches all)
    pub async fn get_pending_proposals(&mut self) -> Result<Vec<SettlementProposal>> {
        let page_size = 50u32;
        let mut all_proposals = Vec::new();
        let mut offset = 0u32;

        loop {
            let request = Request::new(GetSettlementProposalsRequest {
                market_id: None,
                status: Some(SettlementStatus::Pending as i32),
                limit: Some(page_size),
                offset: Some(offset),
            });

            let response = self
                .orderbook_client
                .get_settlement_proposals(request)
                .await
                .map_err(|e| anyhow::anyhow!("get_settlement_proposals failed: {}", e.message()))?;

            let inner = response.into_inner();
            let page_count = inner.proposals.len() as u32;
            all_proposals.extend(inner.proposals);

            if page_count < page_size || all_proposals.len() as u32 >= inner.total {
                break;
            }
            offset += page_size;
        }

        Ok(all_proposals)
    }

    /// Subscribe to settlement updates
    ///
    /// Returns a stream of SettlementUpdate events for settlements involving this party.
    pub async fn subscribe_settlements(
        &mut self,
        market_id: Option<String>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SettlementUpdate, tonic::Status>> + Send>>> {
        let mut request = Request::new(SubscribeSettlementsRequest {
            market_id,
        });

        // Add authorization header
        let jwt = generate_jwt(
            &self.auth_data.party_id,
            &self.auth_data.role,
            &self.auth_data.private_key_bytes,
            self.auth_data.ttl_secs,
            Some(self.auth_data.node_name.as_str()),
        )?;
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", jwt).parse().context("Failed to parse JWT")?,
        );

        let response = self
            .raw_orderbook_client
            .subscribe_settlements(request)
            .await
            .context("Failed to subscribe to settlements")?;

        Ok(Box::pin(response.into_inner()))
    }

    /// Request quotes from connected liquidity providers
    pub async fn request_quotes(
        &mut self,
        market_id: &str,
        direction: &str,
        quantity: &str,
        lp_names: Vec<String>,
        timeout_secs: Option<u32>,
    ) -> Result<RequestQuotesResponse> {
        let request = Request::new(RequestQuotesRequest {
            market_id: market_id.to_string(),
            direction: direction.to_string(),
            quantity: quantity.to_string(),
            lp_names,
            timeout_secs,
        });

        let response = self
            .orderbook_client
            .request_quotes(request)
            .await
            .map_err(|e| anyhow::anyhow!("request_quotes failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Accept a specific RFQ quote
    pub async fn accept_quote(
        &mut self,
        rfq_id: &str,
        quote_id: &str,
    ) -> Result<AcceptQuoteResponse> {
        let request = Request::new(AcceptQuoteRequest {
            rfq_id: rfq_id.to_string(),
            quote_id: quote_id.to_string(),
        });

        let response = self
            .orderbook_client
            .accept_quote(request)
            .await
            .map_err(|e| anyhow::anyhow!("accept_quote failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Request RFQ V2 (AtomicDVP) quotes from V2-connected liquidity providers.
    /// `fee_tokens` is the priority-ordered settlement-fee token preference
    /// (instruments-table symbols, e.g. ["USDC", "CC"]); empty = CC.
    pub async fn request_quotes_atomic(
        &mut self,
        market_id: &str,
        direction: &str,
        quantity: &str,
        lp_names: Vec<String>,
        timeout_secs: Option<u32>,
        fee_tokens: Vec<String>,
    ) -> Result<RequestQuotesV2Response> {
        let request = Request::new(RequestQuotesV2Request {
            market_id: market_id.to_string(),
            direction: direction.to_string(),
            quantity: quantity.to_string(),
            lp_names,
            timeout_secs,
            fee_tokens,
            // No swap-venue delegation: the agent always acts as its own party.
            user: None,
        });

        let response = self
            .rfqv2_client
            .request_quotes(request)
            .await
            .map_err(|e| anyhow::anyhow!("request_quotes_atomic failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Accept an RFQ V2 quote — blocks up to the confirm round trip
    /// (`timeout_secs`, server default 10 s, clamp 1..30).
    ///
    /// An LP reject travels in the response BODY: gRPC OK + `success=false`
    /// with `reject_reason`/`reject_detail` set — NOT a transport error. Only
    /// transport-level failures (timeout, LP disconnect, V2-unaware server)
    /// surface as `Err`. Use [`atomic_reject_reason_name`] to render the enum.
    pub async fn accept_quote_atomic(
        &mut self,
        rfq_id: &str,
        quote_id: &str,
        timeout_secs: Option<u32>,
    ) -> Result<AcceptQuoteAtomicResponse> {
        let request = Request::new(AcceptQuoteAtomicRequest {
            rfq_id: rfq_id.to_string(),
            quote_id: quote_id.to_string(),
            timeout_secs,
            // No swap-venue delegation: the agent accepts as its own party.
            user: None,
        });

        let response = self
            .rfqv2_client
            .accept_quote_atomic(request)
            .await
            .map_err(|e| anyhow::anyhow!("accept_quote_atomic failed ({}): {}", e.code(), e.message()))?;

        Ok(response.into_inner())
    }

    /// Get the party ID for this client
    pub fn party_id(&self) -> &str {
        &self.auth_data.party_id
    }

    /// Get the private key bytes
    pub fn private_key_bytes(&self) -> &[u8; 32] {
        &self.auth_data.private_key_bytes
    }

    /// Get the public key hex
    pub fn public_key_hex(&self) -> &str {
        &self.auth_data.public_key_hex
    }

    /// Get rounds data including issuance forecast
    pub async fn get_rounds_data(&mut self, limit: Option<u32>) -> Result<GetRoundsDataResponse> {
        let request = Request::new(GetRoundsDataRequest {
            limit,
        });
        let response = self
            .orderbook_client
            .get_rounds_data(request)
            .await
            .map_err(|e| anyhow::anyhow!("get_rounds_data failed: {}", e.message()))?;
        Ok(response.into_inner())
    }
}

/// Human-readable name for an RFQ V2 LP confirm-reject reason code
/// (`AcceptQuoteAtomicResponse.reject_reason`).
pub fn atomic_reject_reason_name(code: i32) -> &'static str {
    RfqConfirmRejectReason::try_from(code)
        .map(|r| r.as_str_name())
        .unwrap_or("RFQ_CONFIRM_REJECT_REASON_UNKNOWN")
}

/// Authentication interceptor for gRPC requests with automatic JWT refresh
#[derive(Clone)]
struct AuthInterceptor {
    token: Arc<RwLock<String>>,
    expires_at: Arc<RwLock<u64>>,
    auth_data: ExternalAuthData,
}

/// Refresh JWT 5 minutes before expiry
const REFRESH_BEFORE_EXPIRY_SECS: u64 = 300;

impl tonic::service::Interceptor for AuthInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, tonic::Status> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expires_at = *self.expires_at.read().unwrap();

        if now + REFRESH_BEFORE_EXPIRY_SECS >= expires_at {
            match generate_jwt(
                &self.auth_data.party_id,
                &self.auth_data.role,
                &self.auth_data.private_key_bytes,
                self.auth_data.ttl_secs,
                Some(self.auth_data.node_name.as_str()),
            ) {
                Ok(new_jwt) => {
                    debug!("JWT token refreshed (was expiring in {}s)", expires_at.saturating_sub(now));
                    *self.token.write().unwrap() = new_jwt;
                    *self.expires_at.write().unwrap() = now + self.auth_data.ttl_secs;
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
