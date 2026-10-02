//! gRPC channel construction with bounded connects, and bounded calls and streams.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio_stream::{Stream, StreamExt};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::clock;

/// Bound on a TLS handshake, including on reconnect.
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default per-RPC deadline.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Default TCP connect bound.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the next message of a server stream.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

const CONNECT_SLACK: Duration = Duration::from_secs(5);
const H2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const H2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_KEEPALIVE: Duration = Duration::from_secs(60);

/// Bounds for one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelOpts {
    /// TCP connect bound.
    pub connect: Duration,
    /// TLS handshake bound.
    pub tls: Duration,
    /// Client-local bound on response headers of each call.
    pub request: Option<Duration>,
    /// HTTP/2 and TCP keepalive.
    pub keepalive: bool,
}

impl Default for ChannelOpts {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            tls: TLS_HANDSHAKE_TIMEOUT,
            request: Some(RPC_TIMEOUT),
            keepalive: true,
        }
    }
}

impl ChannelOpts {
    /// Outer bound on `connect_channel`; it also covers DNS resolution.
    pub const fn connect_budget(&self) -> Duration {
        self.connect.saturating_add(self.tls).saturating_add(CONNECT_SLACK)
    }
}

/// Endpoint for `url` with the bounds in `opts`; TLS for `https` with SNI from the URI host.
pub fn endpoint(url: &str, opts: ChannelOpts) -> Result<Endpoint> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut ep = Channel::from_shared(url.to_string())
        .context("Invalid gRPC URL")?
        .connect_timeout(opts.connect);
    if let Some(request) = opts.request {
        ep = ep.timeout(request);
    }
    if opts.keepalive {
        ep = ep
            .http2_keep_alive_interval(H2_KEEPALIVE_INTERVAL)
            .keep_alive_timeout(H2_KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .tcp_keepalive(Some(TCP_KEEPALIVE));
    }
    if ep.uri().scheme_str() == Some("https") {
        let host = ep
            .uri()
            .host()
            .ok_or_else(|| anyhow!("gRPC URL has no host: {url}"))?;
        let sni = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let tls = ClientTlsConfig::new()
            .with_webpki_roots()
            .domain_name(sni)
            .timeout(opts.tls);
        ep = ep.tls_config(tls).context("Failed to configure TLS")?;
    }
    Ok(ep)
}

/// Connect to `url` within `opts.connect_budget()`.
pub async fn connect_channel(url: &str, opts: ChannelOpts) -> Result<Channel> {
    connect_within(url, opts, opts.connect_budget()).await
}

async fn connect_within(url: &str, opts: ChannelOpts, budget: Duration) -> Result<Channel> {
    let ep = endpoint(url, opts)?;
    match tokio::time::timeout(budget, ep.connect()).await {
        Ok(connected) => connected.with_context(|| format!("Failed to connect to gRPC service at {url}")),
        Err(_) => Err(anyhow!("(Unavailable) connect to {url} timed out after {budget:?}")),
    }
}

/// Run one call under a client deadline that also covers the channel's reconnect wait.
pub async fn with_deadline<T, F>(d: Duration, what: &str, call: F) -> Result<T>
where
    F: Future<Output = Result<tonic::Response<T>, tonic::Status>>,
{
    match tokio::time::timeout(d, call).await {
        Ok(Ok(response)) => Ok(response.into_inner()),
        Ok(Err(s)) => Err(anyhow!("{what} RPC failed ({}): {}", s.code(), s.message())),
        Err(_) => Err(anyhow!("{what} RPC failed (Unavailable): client deadline {d:?} exceeded")),
    }
}

/// Why `collect_stream` stopped.
#[derive(Debug)]
pub enum StreamEnd {
    /// The server ended the stream.
    Complete,
    /// The server sent an error.
    Error(tonic::Status),
    /// No message arrived within the idle bound.
    Idle(Duration),
    /// The total bound elapsed.
    Deadline(Duration),
}

impl StreamEnd {
    /// Whether every message the server meant to send was received.
    pub fn is_complete(&self) -> bool {
        matches!(self, StreamEnd::Complete)
    }
}

impl fmt::Display for StreamEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamEnd::Complete => write!(f, "complete"),
            StreamEnd::Error(s) => write!(f, "stream error ({}): {}", s.code(), s.message()),
            StreamEnd::Idle(d) => write!(f, "no message for {d:?}"),
            StreamEnd::Deadline(d) => write!(f, "stream deadline {d:?} exceeded"),
        }
    }
}

/// Collect a server stream under an idle and a total bound, keeping every item received.
pub async fn collect_stream<S, T>(stream: S, idle: Duration, total: Duration) -> (Vec<T>, StreamEnd)
where
    S: Stream<Item = Result<T, tonic::Status>>,
{
    tokio::pin!(stream);
    let deadline = clock::deadline_after(total);
    let mut items = Vec::new();
    let end = loop {
        let next = tokio::time::timeout(idle, stream.next());
        match tokio::time::timeout_at(deadline, next).await {
            Err(_) => break StreamEnd::Deadline(total),
            Ok(Err(_)) => break StreamEnd::Idle(idle),
            Ok(Ok(None)) => break StreamEnd::Complete,
            Ok(Ok(Some(Ok(item)))) => items.push(item),
            Ok(Ok(Some(Err(status)))) => break StreamEnd::Error(status),
        }
    };
    (items, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
    use tonic::codegen::http;
    use tonic::codegen::http::uri::PathAndQuery;
    use tonic::{Request, Response, Status};

    const HANG: &str = "/test.Stall/Hang";
    const TWO_THEN_STALL: &str = "/test.Stall/TwoThenStall";

    /// Codec for empty messages.
    #[derive(Clone, Copy, Default)]
    struct UnitCodec;
    struct UnitEncoder;
    struct UnitDecoder;

    impl Encoder for UnitEncoder {
        type Item = ();
        type Error = Status;
        fn encode(&mut self, _: (), _: &mut EncodeBuf<'_>) -> Result<(), Status> {
            Ok(())
        }
    }

    impl Decoder for UnitDecoder {
        type Item = ();
        type Error = Status;
        fn decode(&mut self, _: &mut DecodeBuf<'_>) -> Result<Option<()>, Status> {
            Ok(Some(()))
        }
    }

    impl Codec for UnitCodec {
        type Encode = ();
        type Decode = ();
        type Encoder = UnitEncoder;
        type Decoder = UnitDecoder;
        fn encoder(&mut self) -> UnitEncoder {
            UnitEncoder
        }
        fn decoder(&mut self) -> UnitDecoder {
            UnitDecoder
        }
    }

    type UnitStream = Pin<Box<dyn Stream<Item = Result<(), Status>> + Send>>;

    /// Sends response headers and two messages, then nothing.
    struct TwoThenStall;

    impl tonic::server::ServerStreamingService<()> for TwoThenStall {
        type Response = ();
        type ResponseStream = UnitStream;
        type Future = tonic::codegen::BoxFuture<Response<UnitStream>, Status>;
        fn call(&mut self, _: Request<()>) -> Self::Future {
            Box::pin(async {
                let s = tokio_stream::iter([Ok(()), Ok(())]).chain(tokio_stream::pending());
                Ok(Response::new(Box::pin(s) as UnitStream))
            })
        }
    }

    /// `Hang` never answers; `TwoThenStall` stalls after two messages.
    #[derive(Clone)]
    struct StallServer;

    impl tonic::server::NamedService for StallServer {
        const NAME: &'static str = "test.Stall";
    }

    impl<B> tonic::codegen::Service<http::Request<B>> for StallServer
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
            if req.uri().path() == HANG {
                return Box::pin(std::future::pending());
            }
            Box::pin(async move {
                let mut grpc = tonic::server::Grpc::new(UnitCodec);
                Ok(grpc.server_streaming(TwoThenStall, req).await)
            })
        }
    }

    async fn spawn_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(StallServer)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        url
    }

    fn no_request_bound() -> ChannelOpts {
        ChannelOpts { request: None, keepalive: false, ..ChannelOpts::default() }
    }

    async fn ready(channel: Channel) -> tonic::client::Grpc<Channel> {
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.unwrap();
        grpc
    }

    async fn unary(channel: Channel, path: &'static str) -> Result<Response<()>, Status> {
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.map_err(|e| Status::unavailable(e.to_string()))?;
        grpc.unary(Request::new(()), PathAndQuery::from_static(path), UnitCodec).await
    }

    // A peer that accepts TCP but never answers the TLS handshake fails the connect
    #[tokio::test]
    async fn a_stalled_tls_handshake_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let opts = ChannelOpts { connect: Duration::from_secs(2), tls: Duration::from_millis(200), ..no_request_bound() };
        let result = tokio::time::timeout(Duration::from_secs(4), connect_channel(&url, opts)).await;
        assert!(matches!(result, Ok(Err(_))), "the handshake bound should end the connect");
        drop(listener);
    }

    // The outer budget ends a connect that the inner bounds would let run on
    #[tokio::test]
    async fn the_connect_budget_bounds_the_whole_connect() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let opts = ChannelOpts { connect: Duration::from_secs(60), tls: Duration::from_secs(60), ..no_request_bound() };
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            connect_within(&url, opts, Duration::from_millis(300)),
        )
        .await;
        let err = result.expect("the budget should end the connect").unwrap_err().to_string();
        assert!(err.contains("(Unavailable)") && err.contains("timed out"), "{err}");
        assert_eq!(opts.connect_budget(), Duration::from_secs(125));
        drop(listener);
    }

    // SNI comes from the URI host, so paths and IPv6 literals are handled
    #[test]
    fn tls_endpoints_take_sni_from_the_uri_host() {
        let opts = ChannelOpts::default();
        assert!(endpoint("https://host.example.com/prefix", opts).is_ok());
        assert!(endpoint("https://host.example.com:443", opts).is_ok());
        assert!(endpoint("https://[::1]:8443", opts).is_ok());
        assert!(endpoint("http://127.0.0.1:1", opts).is_ok());
        assert!(endpoint("not a url", opts).is_err());
    }

    #[tokio::test]
    async fn with_deadline_bounds_a_hung_server() {
        let url = spawn_server().await;
        let channel = connect_channel(&url, no_request_bound()).await.unwrap();
        let call = with_deadline(Duration::from_millis(300), "Hang", unary(channel, HANG));
        let err = tokio::time::timeout(Duration::from_secs(4), call)
            .await
            .expect("the deadline should end the call")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("Hang RPC failed (Unavailable): client deadline"), "{err}");
    }

    // A lazy channel waits inside the reconnect path, which Endpoint::timeout does not cover
    #[tokio::test]
    async fn with_deadline_bounds_the_reconnect_wait() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let channel = endpoint(&url, no_request_bound()).unwrap().connect_lazy();
        let call = with_deadline(Duration::from_millis(300), "Silent", unary(channel, HANG));
        let result = tokio::time::timeout(Duration::from_secs(4), call).await;
        assert!(matches!(result, Ok(Err(_))), "the deadline should end the call");
        drop(listener);
    }

    #[tokio::test]
    async fn with_deadline_maps_results() {
        let ok = with_deadline(Duration::from_secs(1), "Ok", async { Ok(Response::new(7u8)) }).await;
        assert_eq!(ok.unwrap(), 7);
        let err = with_deadline(Duration::from_secs(1), "Get", async {
            Err::<Response<u8>, _>(Status::not_found("no such thing"))
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("Get RPC failed (") && err.ends_with("): no such thing"), "{err}");
    }

    #[tokio::test]
    async fn collect_stream_bounds_a_stream_idle_after_headers() {
        let url = spawn_server().await;
        let channel = connect_channel(&url, no_request_bound()).await.unwrap();
        let mut grpc = ready(channel).await;
        let open = grpc.server_streaming(Request::new(()), PathAndQuery::from_static(TWO_THEN_STALL), UnitCodec);
        let stream = with_deadline(Duration::from_secs(4), "TwoThenStall", open).await.unwrap();
        let collected = tokio::time::timeout(
            Duration::from_secs(4),
            collect_stream(stream, Duration::from_millis(300), Duration::from_secs(30)),
        )
        .await
        .expect("the idle bound should end the stream");
        let (items, end) = collected;
        assert_eq!(items.len(), 2, "messages received before the stall are kept");
        assert!(matches!(end, StreamEnd::Idle(_)), "{end}");
        assert!(!end.is_complete());
    }

    #[tokio::test]
    async fn collect_stream_reports_complete_and_error_ends() {
        let (items, end) = collect_stream(tokio_stream::iter([Ok(1), Ok(2), Ok(3)]), STREAM_IDLE_TIMEOUT, RPC_TIMEOUT).await;
        assert_eq!(items, vec![1, 2, 3]);
        assert!(end.is_complete());

        let mixed = [Ok(1), Ok(2), Err(Status::internal("boom")), Ok(4)];
        let (items, end) = collect_stream(tokio_stream::iter(mixed), STREAM_IDLE_TIMEOUT, RPC_TIMEOUT).await;
        assert_eq!(items, vec![1, 2], "items before the error are kept");
        assert!(matches!(&end, StreamEnd::Error(s) if s.message() == "boom"));
        assert!(end.to_string().ends_with(": boom"), "{end}");
    }

    #[tokio::test]
    async fn collect_stream_total_deadline_keeps_partial_items() {
        let ticking = tokio_stream::StreamExt::map(
            tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_millis(20))),
            |_| Ok::<u8, Status>(1),
        );
        let (items, end) = tokio::time::timeout(
            Duration::from_secs(4),
            collect_stream(ticking, Duration::from_secs(1), Duration::from_millis(200)),
        )
        .await
        .expect("the total bound should end the stream");
        assert!(!items.is_empty(), "items before the deadline are kept");
        assert!(matches!(end, StreamEnd::Deadline(_)), "{end}");

        let (items, end) = collect_stream(
            tokio_stream::pending::<Result<u8, Status>>(),
            Duration::MAX,
            Duration::from_millis(50),
        )
        .await;
        assert!(items.is_empty());
        assert!(matches!(end, StreamEnd::Deadline(_)), "huge idle bound must not panic");
    }
}
