//! Helpers shared by unit tests.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio_stream::{Stream, StreamExt};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http;
use tonic::{Response, Status};

/// A second live dispatcher keeps callsite interest from being cached as "never".
fn keep_callsite_interest() {
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| {
        tracing::Dispatch::new(tracing_subscriber::fmt().with_writer(std::io::sink).finish())
    });
}

/// Format WARN events on this thread until the guard drops; tracing formats
/// arguments only for enabled levels, so tests of log formatting need this.
pub(crate) fn warn_logging() -> tracing::subscriber::DefaultGuard {
    keep_callsite_interest();
    tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_max_level(tracing::Level::WARN)
            .finish(),
    )
}

/// Collects formatted log output for assertions.
#[derive(Clone, Default)]
pub(crate) struct LogBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        agent_logic::sync::lock(&self.0).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

impl LogBuf {
    /// Capture this thread's events at `level` and above until the guard drops.
    pub(crate) fn capture(&self, level: tracing::Level) -> tracing::subscriber::DefaultGuard {
        keep_callsite_interest();
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .with_max_level(level)
                .finish(),
        )
    }

    pub(crate) fn count(&self, needle: &str) -> usize {
        String::from_utf8_lossy(&agent_logic::sync::lock(&self.0)).matches(needle).count()
    }
}

/// A port that refuses connections.
pub(crate) fn refused_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// How the fake ledger answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fake {
    /// Unary calls get one empty message; streams send two and end.
    Answer,
    /// Unary calls never answer; streams send two messages, then stall.
    Stall,
    /// Unary calls answer like `Answer` after this delay; streams as `Answer`.
    Slow(std::time::Duration),
    /// Unary calls answer; streams send two messages, then stall.
    StreamStall,
    /// Unary calls answer; streams send two messages, then fail.
    StreamFail,
    /// Unary calls answer; streams are refused before any message.
    StreamRefuse,
    /// GetLedgerEnd answers offset 7 and GetUpdates one checkpoint at offset 50; the rest as `Answer`.
    Progress,
}

/// GetLedgerEndResponse { offset: 7 }.
const LEDGER_END_7: [u8; 2] = [0x08, 0x07];
/// GetUpdatesResponse { offset_checkpoint: { offset: 50 } }.
const CHECKPOINT_50: [u8; 4] = [0x12, 0x02, 0x08, 0x32];

type CallLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A DAppProviderService fake. Empty messages decode as default responses,
/// so callers see empty lists and default fields.
pub(crate) struct FakeLedger {
    pub(crate) url: String,
    server: tokio::task::JoinHandle<()>,
    calls: CallLog,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeLedger {
    pub(crate) async fn start(mode: Fake) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = CallLog::default();
        let service = FakeService { mode, calls: calls.clone() };
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepted = connections.clone();
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener).map(move |conn| {
            if conn.is_ok() {
                accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            conn
        });
        let server = tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming(incoming)
                .await;
        });
        Self { url, server, calls, connections }
    }

    /// TCP connections accepted so far.
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many requests the fake has received for `method`, e.g. "GetLedgerEnd".
    pub(crate) fn calls(&self, method: &str) -> usize {
        let suffix = format!("/{method}");
        agent_logic::sync::lock(&self.calls).iter().filter(|p| p.ends_with(&suffix)).count()
    }

    /// Stop listening, so new connections are refused.
    pub(crate) async fn stop(self) {
        self.server.abort();
        let _ = self.server.await;
    }
}

/// Writes pre-encoded message bytes and reads any message as `()`.
#[derive(Clone, Copy, Default)]
pub(crate) struct RawCodec;
pub(crate) struct RawEncoder;
/// Reads any message as `()`.
pub(crate) struct UnitDecoder;

impl Encoder for RawEncoder {
    type Item = Vec<u8>;
    type Error = Status;
    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        use tokio::io::AsyncReadExt;
        let mut rest = item.as_slice();
        let mut cx = Context::from_waker(std::task::Waker::noop());
        while !rest.is_empty() {
            let mut copy = std::pin::pin!(rest.read_buf(dst));
            if !matches!(copy.as_mut().poll(&mut cx), Poll::Ready(Ok(n)) if n > 0) {
                return Err(Status::internal("short write"));
            }
        }
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

impl Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = ();
    type Encoder = RawEncoder;
    type Decoder = UnitDecoder;
    fn encoder(&mut self) -> RawEncoder {
        RawEncoder
    }
    fn decoder(&mut self) -> UnitDecoder {
        UnitDecoder
    }
}

type RawStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, Status>> + Send>>;

struct Streams {
    mode: Fake,
    head: Vec<Vec<u8>>,
}

impl tonic::server::ServerStreamingService<()> for Streams {
    type Response = Vec<u8>;
    type ResponseStream = RawStream;
    type Future = tonic::codegen::BoxFuture<Response<RawStream>, Status>;
    fn call(&mut self, _: tonic::Request<()>) -> Self::Future {
        let mode = self.mode;
        let head = tokio_stream::iter(self.head.clone().into_iter().map(Ok::<Vec<u8>, Status>));
        Box::pin(async move {
            let stream: RawStream = match mode {
                Fake::Answer | Fake::Slow(_) | Fake::Progress => Box::pin(head),
                Fake::Stall | Fake::StreamStall => Box::pin(head.chain(tokio_stream::pending())),
                Fake::StreamFail => Box::pin(head.chain(tokio_stream::iter([Err(Status::internal("boom"))]))),
                Fake::StreamRefuse => return Err(Status::internal("refused")),
            };
            Ok(Response::new(stream))
        })
    }
}

struct Unary {
    mode: Fake,
    reply: Vec<u8>,
}

impl tonic::server::UnaryService<()> for Unary {
    type Response = Vec<u8>;
    type Future = tonic::codegen::BoxFuture<Response<Vec<u8>>, Status>;
    fn call(&mut self, _: tonic::Request<()>) -> Self::Future {
        let (mode, reply) = (self.mode, self.reply.clone());
        Box::pin(async move {
            match mode {
                Fake::Stall => std::future::pending::<()>().await,
                Fake::Slow(delay) => tokio::time::sleep(delay).await,
                Fake::Answer | Fake::StreamStall | Fake::StreamFail | Fake::StreamRefuse | Fake::Progress => {}
            }
            Ok(Response::new(reply))
        })
    }
}

#[derive(Clone)]
struct FakeService {
    mode: Fake,
    calls: CallLog,
}

impl tonic::server::NamedService for FakeService {
    const NAME: &'static str = "silvana.ledger.v1.DAppProviderService";
}

impl<B> tonic::codegen::Service<http::Request<B>> for FakeService
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

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let mode = self.mode;
        agent_logic::sync::lock(&self.calls).push(req.uri().path().to_string());
        // Every request is read as one empty message, so the unit codec can decode it
        let (parts, _) = req.into_parts();
        let req = http::Request::from_parts(parts, tonic::body::Body::new(String::from("\0\0\0\0\0")));
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(RawCodec);
            let path = req.uri().path();
            let progress = mode == Fake::Progress;
            let (updates, ledger_end) = (path.ends_with("/GetUpdates"), path.ends_with("/GetLedgerEnd"));
            Ok(if updates || path.ends_with("/GetActiveContracts") {
                let head = if progress && updates { vec![CHECKPOINT_50.to_vec()] } else { vec![Vec::new(), Vec::new()] };
                grpc.server_streaming(Streams { mode, head }, req).await
            } else {
                let reply = if progress && ledger_end { LEDGER_END_7.to_vec() } else { Vec::new() };
                grpc.unary(Unary { mode, reply }, req).await
            })
        })
    }
}
