//! The agent's own traffic purchases (AgentTrafficService): an intent names the
//! recipient, the instrument and a memo; the agent's payment with that memo is credited.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tonic::transport::Channel;

use agent_logic::config::BaseConfig;
use orderbook_proto::orderbook::agent_traffic_service_client::AgentTrafficServiceClient;
use orderbook_proto::orderbook::{
    AgentTrafficPurchase, CancelAgentTrafficPurchaseRequest, CreateAgentTrafficPurchaseRequest,
    CreateTrafficPurchaseResponse, GetAgentTrafficPurchaseRequest,
};

/// Bound on one purchase call, and on opening the channel.
pub const PURCHASE_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// A token whose purchases the rpc cannot serve (UNIMPLEMENTED, 4075, 4141, 4142) is left out this long.
pub const LONG_SKIP: Duration = Duration::from_secs(30 * 60);
/// Any other refusal or failure leaves the token out this long.
pub const SHORT_SKIP: Duration = Duration::from_secs(5 * 60);
/// Most bytes one answer may carry.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Why a purchase call gave no usable answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurchaseError {
    /// The rpc does not serve agent purchases.
    Unimplemented,
    /// `request refused (code N)`.
    Refused(u16),
    /// Any other failure, or no answer in time.
    Failed(String),
}

impl std::fmt::Display for PurchaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unimplemented => write!(f, "the rpc does not serve agent traffic purchases"),
            Self::Refused(code) => write!(f, "request refused (code {code})"),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for PurchaseError {}

impl PurchaseError {
    pub fn from_status(status: &tonic::Status) -> Self {
        if status.code() == tonic::Code::Unimplemented {
            return Self::Unimplemented;
        }
        match crate::control::rpc::refusal_code(status.message()) {
            Some(code) => Self::Refused(code),
            None => Self::Failed(format!("{:?}: {}", status.code(), log_safe(status.message()))),
        }
    }

    /// How long the token is left out of the top-up list after this failure.
    pub fn skip_for(&self) -> Duration {
        match self {
            Self::Unimplemented | Self::Refused(4075 | 4141 | 4142) => LONG_SKIP,
            Self::Refused(_) | Self::Failed(_) => SHORT_SKIP,
        }
    }
}

/// Server text for a log line: at most 200 characters, unsafe ones replaced.
pub(crate) fn log_safe(text: &str) -> String {
    text.chars().take(200).map(|c| if crate::log_text::is_log_unsafe(c) { '?' } else { c }).collect()
}

/// Where a purchase stands, as the agent acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurchaseState {
    /// Credited, on time or late.
    Credited,
    /// The payment was seen and refused.
    Rejected,
    /// Still waiting, expired or cancelled: a payment can still be credited.
    Open,
}

pub fn purchase_state(status: &str) -> PurchaseState {
    match status {
        "credited" | "late" => PurchaseState::Credited,
        "rejected" => PurchaseState::Rejected,
        _ => PurchaseState::Open,
    }
}

/// The agent's purchase calls; the rpc answers only for the caller's own purchases.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait PurchaseApi: Send + Sync {
    async fn create(&self, token: &str, token_amount: &str) -> Result<CreateTrafficPurchaseResponse, PurchaseError>;
    async fn get(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError>;
    async fn cancel(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError>;
}

type TokenSource = Arc<dyn Fn() -> Result<String> + Send + Sync>;

/// AgentTrafficService over the agent's rpc; every call carries a fresh token of the agent key.
#[derive(Clone)]
pub struct PurchaseClient {
    channel: Channel,
    token: TokenSource,
    timeout: Duration,
}

impl std::fmt::Debug for PurchaseClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PurchaseClient").field("timeout", &self.timeout).finish_non_exhaustive()
    }
}

impl PurchaseClient {
    pub fn new(channel: Channel, token: TokenSource) -> Self {
        Self { channel, token, timeout: PURCHASE_CALL_TIMEOUT }
    }

    /// Connects to the config's rpc within [`PURCHASE_CALL_TIMEOUT`].
    pub async fn connect(config: &BaseConfig) -> Result<Self> {
        let open = crate::DAppProviderClient::create_channel(
            &config.orderbook_grpc_url,
            Some(config.connection_timeout_secs),
            Some(config.request_timeout_secs),
        );
        let channel = tokio::time::timeout(PURCHASE_CALL_TIMEOUT, open)
            .await
            .map_err(|_| anyhow!("no connection to the rpc within {PURCHASE_CALL_TIMEOUT:?}"))??;
        let config = config.clone();
        Ok(Self::new(channel, Arc::new(move || config.jwt())))
    }

    fn request<T>(&self, message: T) -> Result<tonic::Request<T>, PurchaseError> {
        let token = (self.token)().map_err(|e| PurchaseError::Failed(format!("no token: {e:#}")))?;
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| PurchaseError::Failed("the token is not a header value".into()))?;
        let mut request = tonic::Request::new(message);
        request.metadata_mut().insert("authorization", bearer);
        request.set_timeout(self.timeout);
        Ok(request)
    }

    async fn call<T, F>(&self, what: &str, call: F) -> Result<T, PurchaseError>
    where
        F: Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        match tokio::time::timeout(self.timeout, call).await {
            Err(_) => Err(PurchaseError::Failed(format!("{what}: no answer within {:?}", self.timeout))),
            Ok(Err(status)) => Err(PurchaseError::from_status(&status)),
            Ok(Ok(response)) => Ok(response.into_inner()),
        }
    }

    fn client(&self) -> AgentTrafficServiceClient<Channel> {
        AgentTrafficServiceClient::new(self.channel.clone()).max_decoding_message_size(MAX_RESPONSE_BYTES)
    }
}

/// The purchase an answer carries.
fn purchase_of(purchase: Option<AgentTrafficPurchase>) -> Result<AgentTrafficPurchase, PurchaseError> {
    purchase.ok_or_else(|| PurchaseError::Failed("the answer carries no purchase".into()))
}

#[async_trait::async_trait]
impl PurchaseApi for PurchaseClient {
    async fn create(&self, token: &str, token_amount: &str) -> Result<CreateTrafficPurchaseResponse, PurchaseError> {
        let request = self.request(CreateAgentTrafficPurchaseRequest {
            token: token.to_string(),
            token_amount: token_amount.to_string(),
        })?;
        let mut client = self.client();
        self.call("CreateAgentTrafficPurchase", client.create_agent_traffic_purchase(request)).await
    }

    async fn get(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
        let request = self.request(GetAgentTrafficPurchaseRequest { purchase_id: purchase_id.to_string() })?;
        let mut client = self.client();
        purchase_of(self.call("GetAgentTrafficPurchase", client.get_agent_traffic_purchase(request)).await?.purchase)
    }

    async fn cancel(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
        let request = self.request(CancelAgentTrafficPurchaseRequest { purchase_id: purchase_id.to_string() })?;
        let mut client = self.client();
        purchase_of(self.call("CancelAgentTrafficPurchase", client.cancel_agent_traffic_purchase(request)).await?.purchase)
    }
}

/// A [`PurchaseClient`] that connects on its first call; a failed connect is tried again on the next.
pub struct LazyPurchaseClient {
    config: BaseConfig,
    client: tokio::sync::Mutex<Option<PurchaseClient>>,
}

impl LazyPurchaseClient {
    pub fn new(config: BaseConfig) -> Self {
        Self { config, client: tokio::sync::Mutex::new(None) }
    }

    async fn client(&self) -> Result<PurchaseClient, PurchaseError> {
        let mut slot = tokio::time::timeout(PURCHASE_CALL_TIMEOUT, self.client.lock())
            .await
            .map_err(|_| PurchaseError::Failed("the purchase client is busy connecting".into()))?;
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let client = PurchaseClient::connect(&self.config)
            .await
            .map_err(|e| PurchaseError::Failed(format!("no purchase client: {e:#}")))?;
        *slot = Some(client.clone());
        Ok(client)
    }
}

#[async_trait::async_trait]
impl PurchaseApi for LazyPurchaseClient {
    async fn create(&self, token: &str, token_amount: &str) -> Result<CreateTrafficPurchaseResponse, PurchaseError> {
        self.client().await?.create(token, token_amount).await
    }

    async fn get(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
        self.client().await?.get(purchase_id).await
    }

    async fn cancel(&self, purchase_id: &str) -> Result<AgentTrafficPurchase, PurchaseError> {
        self.client().await?.cancel(purchase_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_decide_how_long_a_token_is_left_out() {
        let status = |code: tonic::Code, message: &str| PurchaseError::from_status(&tonic::Status::new(code, message));
        assert_eq!(status(tonic::Code::Unimplemented, "unknown service"), PurchaseError::Unimplemented);
        for (code, skip) in [(4075, LONG_SKIP), (4141, LONG_SKIP), (4142, LONG_SKIP), (4077, SHORT_SKIP), (4143, SHORT_SKIP), (4144, SHORT_SKIP), (4076, SHORT_SKIP)] {
            let refused = status(tonic::Code::FailedPrecondition, &format!("request refused (code {code})"));
            assert_eq!(refused, PurchaseError::Refused(code));
            assert_eq!(refused.skip_for(), skip, "{code}");
        }
        assert_eq!(PurchaseError::Unimplemented.skip_for(), LONG_SKIP);
        let failed = status(tonic::Code::Unavailable, "busy");
        assert!(matches!(failed, PurchaseError::Failed(_)), "{failed:?}");
        assert_eq!(failed.skip_for(), SHORT_SKIP);
    }

    #[test]
    fn late_credits_count_as_credited_and_open_ones_can_still_land() {
        assert_eq!(purchase_state("credited"), PurchaseState::Credited);
        assert_eq!(purchase_state("late"), PurchaseState::Credited);
        assert_eq!(purchase_state("rejected"), PurchaseState::Rejected);
        for open in ["pending", "expired", "cancelled", ""] {
            assert_eq!(purchase_state(open), PurchaseState::Open, "{open:?}");
        }
    }

    // Each call carries the agent's own bearer token and a deadline
    #[tokio::test]
    async fn a_call_without_a_token_is_not_sent() {
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let client = PurchaseClient::new(channel, Arc::new(|| Err(anyhow!("no key"))));
        let err = client.get("p-1").await.unwrap_err();
        assert!(matches!(&err, PurchaseError::Failed(why) if why.contains("no token")), "{err:?}");
        let ok = PurchaseClient::new(
            tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
            Arc::new(|| Ok("header.claims.sig".to_string())),
        );
        let request = ok.request(()).unwrap();
        assert_eq!(request.metadata().get("authorization").unwrap(), "Bearer header.claims.sig");
    }
}
