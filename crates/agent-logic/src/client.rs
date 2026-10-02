//! gRPC client wrapper for orderbook and pricing services
//!
//! Provides a unified interface for interacting with the orderbook gRPC service,
//! including authentication, price fetching, order submission, cancellation,
//! and settlement streaming.

use anyhow::{Context, Result, anyhow};
use indexmap::IndexMap;
use orderbook_proto::{
    orderbook::{
        AcceptQuoteRequest, AcceptQuoteResponse, CancelOrderRequest, CancelOrderResponse,
        GetInstrumentsRequest, GetMarketsRequest, GetOrdersRequest, GetRoundsDataRequest,
        GetRoundsDataResponse, GetSettlementProposalsRequest, Instrument, Market, Order,
        OrderStatus, OrderType, RequestQuotesRequest, RequestQuotesResponse, SettlementProposal,
        SettlementStatus, SettlementUpdate, SubmitOrderRequest, SubmitOrderResponse,
        SubscribeSettlementsRequest, TimeInForce, orderbook_service_client::OrderbookServiceClient,
    },
    pricing::{GetPriceRequest, GetPriceResponse, pricing_service_client::PricingServiceClient},
    rfqv2::{
        AcceptQuoteAtomicRequest, AcceptQuoteAtomicResponse, RequestQuotesV2Request,
        RequestQuotesV2Response, RfqConfirmRejectReason, rfq_v2_service_client::RfqV2ServiceClient,
    },
};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_stream::Stream;
use tonic::Request;
use tonic::transport::Channel;
use tracing::debug;

use crate::auth::{generate_jwt, generate_jwt_with_branch};
use crate::config::BaseConfig;
use crate::secret::Secret;
use crate::transport::{self, ChannelOpts};
use crate::{clock, sync};

/// Per-RPC deadline on the orderbook and pricing channels.
pub(crate) const RPC_TIMEOUT: Duration = transport::RPC_TIMEOUT;

const MAX_DECODING_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Most pages one `get_instruments` listing fetches.
const MAX_INSTRUMENT_PAGES: usize = 100;
/// Most pages one `get_pending_proposals` listing fetches.
const MAX_PROPOSAL_PAGES: usize = 200;

/// External account authentication data
#[derive(Clone)]
struct ExternalAuthData {
    party_id: String,
    public_key_hex: String,
    private_key: Secret<32>,
    role: String,
    ttl_secs: u64,
    node_name: String,
    /// RFQ V2 analytics branch stamped into every minted token (VA13);
    /// None = the server default ("main").
    venue_branch: Option<String>,
}

/// Unified client for orderbook and pricing services
pub struct OrderbookClient {
    pricing_client: PricingServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    orderbook_client: OrderbookServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    /// RFQ V2 (AtomicDVP) user-facing service — same channel/auth as v1
    rfqv2_client: RfqV2ServiceClient<
        tonic::service::interceptor::InterceptedService<Channel, AuthInterceptor>,
    >,
    // Raw client for streaming (interceptors don't work well with streaming)
    raw_orderbook_client: OrderbookServiceClient<Channel>,
    auth_data: ExternalAuthData,
    /// Bound on each call, including the channel's reconnect wait.
    deadline: Duration,
}

impl OrderbookClient {
    /// Create a new orderbook client with external account authentication
    pub async fn new(config: &BaseConfig) -> Result<Self> {
        let channel = Self::create_channel(&config.orderbook_grpc_url, ChannelOpts::default()).await?;
        Self::with_channel(channel, config)
    }

    /// Client whose channel connects on first use, so tests need no server.
    #[cfg(test)]
    pub(crate) fn lazy_for_tests(config: &BaseConfig) -> Result<Self> {
        let endpoint = Channel::from_shared(config.orderbook_grpc_url.clone())
            .context("Invalid gRPC URL")?
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2));
        Self::with_channel(endpoint.connect_lazy(), config)
    }

    /// Build the service clients over a channel.
    fn with_channel(channel: Channel, config: &BaseConfig) -> Result<Self> {
        // Generate initial JWT for interceptor
        let jwt = generate_jwt_with_branch(
            &config.party_id,
            &config.role,
            &*config.private_key.expose()?,
            config.token_ttl_secs,
            Some(config.node_name.as_str()),
            config.venue_branch.as_deref(),
        )?;

        let now = clock::now_secs();
        let token_arc = Arc::new(RwLock::new(jwt));
        let expires_at = Arc::new(RwLock::new(now.saturating_add(config.token_ttl_secs)));

        let auth_data = ExternalAuthData {
            party_id: config.party_id.clone(),
            public_key_hex: config.public_key_hex.clone(),
            private_key: config.private_key.clone(),
            role: config.role.clone(),
            ttl_secs: config.token_ttl_secs,
            node_name: config.node_name.clone(),
            venue_branch: config.venue_branch.clone(),
        };

        let auth_interceptor = AuthInterceptor {
            token: token_arc.clone(),
            expires_at: expires_at.clone(),
            auth_data: auth_data.clone(),
        };

        let pricing_client =
            PricingServiceClient::with_interceptor(channel.clone(), auth_interceptor.clone())
                .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        let orderbook_client =
            OrderbookServiceClient::with_interceptor(channel.clone(), auth_interceptor.clone())
                .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        let rfqv2_client = RfqV2ServiceClient::with_interceptor(channel.clone(), auth_interceptor)
            .max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        let raw_orderbook_client =
            OrderbookServiceClient::new(channel).max_decoding_message_size(MAX_DECODING_MESSAGE_SIZE);

        Ok(Self {
            pricing_client,
            orderbook_client,
            rfqv2_client,
            raw_orderbook_client,
            auth_data,
            deadline: RPC_TIMEOUT,
        })
    }

    /// Create the gRPC channel (TLS for `https`), bounded as a whole by `opts`.
    async fn create_channel(grpc_url: &str, opts: ChannelOpts) -> Result<Channel> {
        transport::connect_channel(grpc_url, opts).await
    }

    /// Get current price for a market
    pub async fn get_price(&mut self, market_id: &str) -> Result<GetPriceResponse> {
        let request = Request::new(GetPriceRequest {
            market_id: market_id.to_string(),
            source: None,
        });

        let response = within(self.deadline, self.pricing_client.get_price(request))
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

        let response = within(self.deadline, self.orderbook_client.get_markets(request))
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
        // The server defaults to 50 per page and caps a request at 1000, so
        // page until `total` is covered rather than silently truncating.
        let client = self.orderbook_client.clone();
        let deadline = self.deadline;
        collect_pages("get_instruments", MAX_INSTRUMENT_PAGES, move |offset| {
            let mut client = client.clone();
            async move {
                let request = Request::new(GetInstrumentsRequest {
                    instrument_type: None,
                    limit: Some(1000),
                    offset: Some(offset),
                });
                let response = within(deadline, client.get_instruments(request))
                    .await
                    .map_err(|e| anyhow::anyhow!("get_instruments failed: {}", e.message()))?
                    .into_inner();
                Ok(Page { total: response.total, ends: false, items: response.instruments })
            }
        })
        .await
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

        let response = within(self.deadline, self.orderbook_client.submit_order(request))
            .await
            .map_err(submit_order_error)?;

        Ok(response.into_inner())
    }

    /// Cancel an existing order
    pub async fn cancel_order(&mut self, order_id: u64) -> Result<CancelOrderResponse> {
        let request = Request::new(CancelOrderRequest { order_id });

        let response = within(self.deadline, self.orderbook_client.cancel_order(request))
            .await
            .map_err(|e| anyhow::anyhow!("Cancel order failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Get live orders for a market (Active + Partial)
    pub async fn get_active_orders(&mut self, market_id: &str) -> Result<Vec<Order>> {
        let mut pages = Vec::new();

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

            let response = within(self.deadline, self.orderbook_client.get_orders(request))
                .await
                .map_err(|e| anyhow::anyhow!("get_orders failed: {}", e.message()))?;

            pages.push(response.into_inner().orders);
        }

        Ok(merge_status_pages(pages))
    }

    /// Get ALL live orders for this party (across all markets)
    pub async fn get_all_active_orders(&mut self) -> Result<Vec<Order>> {
        let mut pages = Vec::new();

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

            let response = within(self.deadline, self.orderbook_client.get_orders(request))
                .await
                .map_err(|e| anyhow::anyhow!("get_all_active_orders failed: {}", e.message()))?;

            pages.push(response.into_inner().orders);
        }

        Ok(merge_status_pages(pages))
    }

    /// Get pending settlement proposals for this party (paginated, fetches all)
    pub async fn get_pending_proposals(&mut self) -> Result<Vec<SettlementProposal>> {
        const PAGE_SIZE: u32 = 50;
        let client = self.orderbook_client.clone();
        let deadline = self.deadline;
        collect_pages("get_settlement_proposals", MAX_PROPOSAL_PAGES, move |offset| {
            let mut client = client.clone();
            async move {
                let request = Request::new(GetSettlementProposalsRequest {
                    market_id: None,
                    status: Some(SettlementStatus::Pending as i32),
                    limit: Some(PAGE_SIZE),
                    offset: Some(offset),
                });
                let inner = within(deadline, client.get_settlement_proposals(request))
                    .await
                    .map_err(|e| anyhow::anyhow!("get_settlement_proposals failed: {}", e.message()))?
                    .into_inner();
                let short = u32::try_from(inner.proposals.len()).is_ok_and(|n| n < PAGE_SIZE);
                Ok(Page { total: inner.total, ends: short, items: inner.proposals })
            }
        })
        .await
    }

    /// Subscribe to settlement updates
    ///
    /// Returns a stream of SettlementUpdate events for settlements involving this party.
    pub async fn subscribe_settlements(
        &mut self,
        market_id: Option<String>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SettlementUpdate, tonic::Status>> + Send>>> {
        let mut request = Request::new(SubscribeSettlementsRequest { market_id });

        // Add authorization header
        let jwt = generate_jwt(
            &self.auth_data.party_id,
            &self.auth_data.role,
            &*self.auth_data.private_key.expose()?,
            self.auth_data.ttl_secs,
            Some(self.auth_data.node_name.as_str()),
        )?;
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", jwt)
                .parse()
                .context("Failed to parse JWT")?,
        );

        let response = within(self.deadline, self.raw_orderbook_client.subscribe_settlements(request))
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

        let response = within(self.deadline, self.orderbook_client.request_quotes(request))
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

        let response = within(self.deadline, self.orderbook_client.accept_quote(request))
            .await
            .map_err(|e| anyhow::anyhow!("accept_quote failed: {}", e.message()))?;

        Ok(response.into_inner())
    }

    /// Request RFQ V2 (AtomicDVP) quotes from V2-connected liquidity providers.
    /// `fee_tokens` is the priority-ordered settlement-fee token preference
    /// (instruments-table symbols, e.g. ["USDC", "CC"]); empty = CC.
    ///
    /// The RFQ is sized in EXACTLY ONE leg (the server rejects both-set and
    /// neither-set): base `quantity`, or quote `quote_quantity` (pass
    /// `Some(..)` with an empty `quantity`). Quote sizing pins the taker's
    /// PAY leg on a buy — the LP's spread then moves the base received, so a
    /// pool-capped buyer can never be pushed over its pool by the spread.
    pub async fn request_quotes_atomic(
        &mut self,
        market_id: &str,
        direction: &str,
        quantity: &str,
        quote_quantity: Option<&str>,
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
            quote_quantity: quote_quantity.map(str::to_string),
        });

        let response = within(self.deadline, self.rfqv2_client.request_quotes(request))
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

        let response = within(self.deadline, self.rfqv2_client.accept_quote_atomic(request))
            .await
            .map_err(|e| {
                anyhow::anyhow!("accept_quote_atomic failed ({}): {}", e.code(), e.message())
            })?;

        Ok(response.into_inner())
    }

    /// Get the party ID for this client
    pub fn party_id(&self) -> &str {
        &self.auth_data.party_id
    }

    /// Get the public key hex
    pub fn public_key_hex(&self) -> &str {
        &self.auth_data.public_key_hex
    }

    /// Get rounds data including issuance forecast
    pub async fn get_rounds_data(&mut self, limit: Option<u32>) -> Result<GetRoundsDataResponse> {
        let request = Request::new(GetRoundsDataRequest { limit });
        let response = within(self.deadline, self.orderbook_client.get_rounds_data(request))
            .await
            .map_err(|e| anyhow::anyhow!("get_rounds_data failed: {}", e.message()))?;
        Ok(response.into_inner())
    }
}

/// One call bounded by `d`, including the channel's reconnect wait.
async fn within<T>(
    d: Duration,
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> Result<tonic::Response<T>, tonic::Status> {
    tokio::time::timeout(d, call)
        .await
        .unwrap_or_else(|_| Err(tonic::Status::deadline_exceeded(format!("client deadline {d:?} exceeded"))))
}

/// One page of a paginated listing.
struct Page<T> {
    /// Server-reported size of the whole listing.
    total: u32,
    /// The server signalled the last page.
    ends: bool,
    items: Vec<T>,
}

/// Pages a listing from offset 0 until a page is empty or marked last, or the
/// reported total is covered; still incomplete after `max_pages` is an error.
async fn collect_pages<T, F, Fut>(what: &str, max_pages: usize, mut fetch: F) -> Result<Vec<T>>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<Page<T>>>,
{
    let mut collected: Vec<T> = Vec::new();
    for _ in 0..max_pages {
        let offset = u32::try_from(collected.len()).map_err(|_| anyhow!("{what}: offset out of range"))?;
        let page = fetch(offset).await?;
        let empty = page.items.is_empty();
        collected.extend(page.items);
        let covered = u64::try_from(collected.len()).map_or(true, |n| n >= u64::from(page.total));
        if empty || page.ends || covered {
            return Ok(collected);
        }
    }
    Err(anyhow!("{what}: listing still incomplete after {max_pages} pages"))
}

/// Displays as "submit_order failed: `<message>`" and keeps the status for its code.
pub(crate) fn submit_order_error(status: tonic::Status) -> anyhow::Error {
    let msg = format!("submit_order failed: {}", status.message());
    anyhow::Error::new(status).context(msg)
}

/// One list from the per-status pages, each order id once.
fn merge_status_pages(pages: Vec<Vec<Order>>) -> Vec<Order> {
    dedup_orders(pages.into_iter().flatten().collect())
}

/// Drop repeated order ids; the later copy wins (Partial is fetched after Active).
fn dedup_orders(orders: Vec<Order>) -> Vec<Order> {
    let mut by_id: IndexMap<u64, Order> = IndexMap::with_capacity(orders.len());
    for order in orders {
        by_id.insert(order.order_id, order);
    }
    by_id.into_values().collect()
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
        let now = clock::now_secs();
        let expires_at = *sync::read(&self.expires_at);

        if now.saturating_add(REFRESH_BEFORE_EXPIRY_SECS) >= expires_at {
            // On failure the previous token is kept
            let refreshed = self.auth_data.private_key.expose().map_err(anyhow::Error::from).and_then(|key| {
                generate_jwt_with_branch(
                    &self.auth_data.party_id,
                    &self.auth_data.role,
                    &key,
                    self.auth_data.ttl_secs,
                    Some(self.auth_data.node_name.as_str()),
                    self.auth_data.venue_branch.as_deref(),
                )
            });
            match refreshed {
                Ok(new_jwt) => {
                    debug!(
                        "JWT token refreshed (was expiring in {}s)",
                        expires_at.saturating_sub(now)
                    );
                    *sync::write(&self.token) = new_jwt;
                    *sync::write(&self.expires_at) = now.saturating_add(self.auth_data.ttl_secs);
                }
                Err(e) => {
                    tracing::error!("Failed to refresh JWT: {}", e);
                }
            }
        }

        let token = sync::read(&self.token).clone();
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", token)
                .parse()
                .map_err(|_| tonic::Status::internal("Failed to parse JWT token"))?,
        );
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(order_id: u64, filled: &str) -> Order {
        Order { order_id, filled_quantity: filled.to_string(), ..Default::default() }
    }

    // A server that accepts TCP but never answers the TLS handshake fails the connect
    #[tokio::test]
    async fn a_stalled_tls_handshake_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let opts = ChannelOpts { tls: Duration::from_millis(200), ..ChannelOpts::default() };
        let connect = OrderbookClient::create_channel(&url, opts);
        let result = tokio::time::timeout(Duration::from_secs(10), connect).await;
        assert!(matches!(result, Ok(Err(_))), "the handshake bound should end the connect");
        drop(listener);
    }

    // A server that accepts TCP but never answers HTTP/2: each call ends at the client deadline
    #[tokio::test]
    async fn calls_on_a_silent_server_end_at_the_client_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let channel = Channel::from_shared(config.orderbook_grpc_url.clone()).unwrap().connect_lazy();
        let mut client = OrderbookClient::with_channel(channel, &config).unwrap();
        client.deadline = Duration::from_millis(200);
        let bound = Duration::from_secs(5);

        let err = tokio::time::timeout(bound, client.get_price("CC-USDCx")).await.unwrap().unwrap_err();
        assert!(err.to_string().contains("client deadline"), "{err}");
        let err = tokio::time::timeout(bound, client.get_instruments()).await.unwrap().unwrap_err();
        assert!(err.to_string().contains("client deadline"), "{err}");
        let submit = client.submit_order("CC-USDCx", OrderType::Bid, "1".into(), "1".into(), None, None, Vec::new(), 1);
        let err = tokio::time::timeout(bound, submit).await.unwrap().unwrap_err();
        assert!(crate::order_manager::submit_outcome_unknown(&err), "a timed-out submit may have been booked");
        assert!(tokio::time::timeout(bound, client.subscribe_settlements(None)).await.unwrap().is_err());
        drop(listener);
    }

    fn page(total: u32, ends: bool, items: std::ops::Range<u32>) -> Result<Page<u32>> {
        Ok(Page { total, ends, items: items.collect() })
    }

    #[tokio::test]
    async fn pages_are_collected_until_the_listing_ends() {
        // Covered total, from cumulative offsets
        let mut offsets = Vec::new();
        let all = collect_pages("t", 10, |offset| {
            offsets.push(offset);
            let end = (offset + 3).min(7);
            std::future::ready(page(7, false, offset..end))
        })
        .await
        .unwrap();
        assert_eq!(all, (0..7).collect::<Vec<_>>());
        assert_eq!(offsets, [0, 3, 6]);

        // A page marked last, and an empty page, end the listing early
        let ends = collect_pages("t", 10, |o| std::future::ready(page(100, true, o..o + 2))).await.unwrap();
        assert_eq!(ends, [0, 1]);
        let empty = collect_pages("t", 10, |_| std::future::ready(page(100, false, 0..0))).await.unwrap();
        assert!(empty.is_empty());

        let failed = collect_pages("t", 10, |_| std::future::ready(Err::<Page<u32>, _>(anyhow!("down")))).await;
        assert_eq!(failed.unwrap_err().to_string(), "down");
    }

    #[tokio::test]
    async fn a_listing_that_never_ends_is_an_error_after_the_page_cap() {
        let mut fetches = 0usize;
        let err = collect_pages("get_instruments", 5, |o| {
            fetches += 1;
            std::future::ready(page(u32::MAX, false, o..o + 1))
        })
        .await
        .unwrap_err();
        assert_eq!(fetches, 5);
        assert!(err.to_string().contains("get_instruments: listing still incomplete after 5 pages"), "{err}");
    }

    #[test]
    fn dedup_orders_prefers_later_copy() {
        let out = dedup_orders(vec![order(1, "0"), order(2, "0"), order(1, "0.5"), order(3, "0")]);
        let ids: Vec<u64> = out.iter().map(|o| o.order_id).collect();
        assert_eq!(ids, vec![1, 2, 3], "first-seen order kept, no duplicates");
        assert_eq!(out[0].filled_quantity, "0.5", "the later (Partial) copy wins");
    }

    #[test]
    fn dedup_orders_keeps_distinct_orders() {
        let out = dedup_orders(vec![order(5, "0"), order(4, "1")]);
        assert_eq!(out.len(), 2);
        assert!(dedup_orders(Vec::new()).is_empty());
    }

    #[test]
    fn merge_status_pages_drops_id_in_both_pages() {
        let active = vec![order(7, "0")];
        let partial = vec![order(7, "0.5"), order(8, "0")];
        let out = merge_status_pages(vec![active, partial]);
        let ids: Vec<u64> = out.iter().map(|o| o.order_id).collect();
        assert_eq!(ids, vec![7, 8]);
        assert_eq!(out[0].filled_quantity, "0.5", "the Partial copy wins");
    }

    fn interceptor(private_key: Secret<32>, expires_at: u64) -> AuthInterceptor {
        AuthInterceptor {
            token: Arc::new(RwLock::new("previous".to_string())),
            expires_at: Arc::new(RwLock::new(expires_at)),
            auth_data: ExternalAuthData {
                party_id: "party".to_string(),
                public_key_hex: String::new(),
                private_key,
                role: "agent".to_string(),
                ttl_secs: 3600,
                node_name: "node".to_string(),
                venue_branch: None,
            },
        }
    }

    fn sent_token(i: &mut AuthInterceptor) -> String {
        use tonic::service::Interceptor;
        let request = i.call(Request::new(())).unwrap();
        request.metadata().get("authorization").unwrap().to_str().unwrap().to_string()
    }

    // A key that cannot be opened keeps the previous token instead of panicking
    #[test]
    fn interceptor_keeps_the_previous_token_when_the_key_cannot_be_opened() {
        let mut i = interceptor(Secret::corrupt_for_tests(), 0);
        assert_eq!(sent_token(&mut i), "Bearer previous");
        assert_eq!(*sync::read(&i.expires_at), 0, "the expiry is not advanced");
    }

    #[test]
    fn interceptor_refreshes_a_token_near_expiry() {
        let mut i = interceptor(Secret::seal(&mut [3u8; 32]).unwrap(), 0);
        let token = sent_token(&mut i);
        assert!(token.starts_with("Bearer ") && token != "Bearer previous");
        assert!(*sync::read(&i.expires_at) > 0);
    }

    #[tokio::test]
    async fn a_client_whose_key_cannot_be_opened_is_an_error() {
        let mut config = BaseConfig::test_minimal().unwrap();
        config.private_key = Secret::corrupt_for_tests();
        let channel = Channel::from_static("http://127.0.0.1:1").connect_lazy();
        let err = OrderbookClient::with_channel(channel, &config).err().unwrap();
        assert_eq!(err.to_string(), "sealed secret is corrupt");
    }
}
