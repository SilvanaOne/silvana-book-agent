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
        GetMarketsRequest, GetOrdersRequest, Market, Order, OrderStatus, OrderType,
        SubmitOrderRequest, SubmitOrderResponse, TimeInForce,
        SubscribeSettlementsRequest, SettlementUpdate,
        GetSettlementProposalsRequest, SettlementProposal, SettlementStatus,
        RequestQuotesRequest, RequestQuotesResponse,
        AcceptQuoteRequest, AcceptQuoteResponse,
    },
};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::transport::{Channel, ClientTlsConfig};
use tonic::Request;
use tokio_stream::Stream;
use std::pin::Pin;
use tracing::info;

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
        );

        let orderbook_client = OrderbookServiceClient::with_interceptor(
            channel.clone(),
            auth_interceptor,
        );

        let raw_orderbook_client = OrderbookServiceClient::new(channel);

        Ok(Self {
            pricing_client,
            orderbook_client,
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

    /// Get pending settlement proposals for this party
    pub async fn get_pending_proposals(&mut self) -> Result<Vec<SettlementProposal>> {
        let request = Request::new(GetSettlementProposalsRequest {
            market_id: None,
            status: Some(SettlementStatus::Pending as i32),
            limit: Some(50),
            offset: None,
        });

        let response = self
            .orderbook_client
            .get_settlement_proposals(request)
            .await
            .map_err(|e| anyhow::anyhow!("get_settlement_proposals failed: {}", e.message()))?;

        Ok(response.into_inner().proposals)
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
                    info!("JWT token refreshed (was expiring in {}s)", expires_at.saturating_sub(now));
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
