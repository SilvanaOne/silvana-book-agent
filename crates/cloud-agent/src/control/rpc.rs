//! The agent's client of the rpc's control service: bounded calls, fresh short
//! control tokens, and the rpc's clock as its responses report it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_control::{
    DEFINITE_UPLOAD_REFUSALS, Hash32, HeadRef, MAX_LOG_ENTRIES, PolicyDocuments, SignedEntry, UtcTime,
};
use agent_logic::config::BaseConfig;
use agent_logic::control_store::{ControlLock, ControlStore};
use anyhow::{Result, anyhow, bail};
use orderbook_proto::orderbook::agent_control_agent_service_client::AgentControlAgentServiceClient;
use orderbook_proto::orderbook::{
    AckPolicyRequest, AckPolicyResponse, AppendControlEntriesRequest, AppendControlEntriesResponse,
    ControlHead, ControlHeartbeat, ControlLinkPurpose, ControlLinkStatus, ControlLogEntry,
    ControlPolicyDocument, ControlPolicyView, ControlVerification, GetAgentControlLogRequest,
    GetAgentControlLogResponse, GetControlDescriptorRequest, GetControlDescriptorResponse,
    GetPolicyRequest, GetPolicyResponse, ListPendingControlRequest, ListPendingControlResponse,
    PostReceiptRequest, PostReceiptResponse,
};
use tonic::metadata::MetadataMap;
use tonic::transport::Channel;

/// Bound on one call, connection included.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on reading a whole log, every page included.
pub const LOG_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest gap between this clock and the rpc's before a control entry is signed.
pub const MAX_CLOCK_SKEW_SECS: i64 = 60;
/// A clock reading older than this no longer counts (it covers a typed confirmation).
const CLOCK_SAMPLE_MAX_AGE_SECS: i64 = 900;
/// Entries asked for per GetControlLog page.
const PAGE_LIMIT: u32 = 256;
/// Largest response accepted (a log page with its policy documents).
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Statuses the rpc answers with when the call itself failed, whatever code they carry.
const TRANSIENT_STATUSES: [&str; 4] = ["Unknown", "Internal", "ResourceExhausted", "Aborted"];
/// Waits before each retry of a transient append failure.
#[cfg(not(test))]
const RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(4)];
#[cfg(test)]
const RETRY_DELAYS: [Duration; 3] = [Duration::from_millis(10); 3];

/// Mints the bearer token of each call but an append.
pub type TokenSource = Arc<dyn Fn() -> Result<String> + Send + Sync>;

/// Mints an append's token, claiming the head after it; the store must hold that head.
pub type AppendTokenSource =
    Arc<dyn Fn(&ControlStore, &ControlLock, &HeadRef) -> Result<String> + Send + Sync>;

/// The rpc's clock, as a response reports it.
pub trait ServerTime {
    fn server_time(&self) -> Option<&prost_types::Timestamp>;
}

impl ServerTime for GetControlDescriptorResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        self.server_time.as_ref()
    }
}

impl ServerTime for GetAgentControlLogResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        self.server_time.as_ref()
    }
}

impl ServerTime for AppendControlEntriesResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        self.server_time.as_ref()
    }
}

impl ServerTime for ListPendingControlResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        self.server_time.as_ref()
    }
}

/// No time field: the `date` header is read instead.
impl ServerTime for GetPolicyResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        None
    }
}

/// No time field: the `date` header is read instead.
impl ServerTime for AckPolicyResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        None
    }
}

/// No time field: the `date` header is read instead.
impl ServerTime for PostReceiptResponse {
    fn server_time(&self) -> Option<&prost_types::Timestamp> {
        None
    }
}

/// The `date` header, in seconds since the epoch.
fn date_header_secs(metadata: &MetadataMap) -> Option<i64> {
    let text = metadata.get("date").and_then(|v| v.to_str().ok())?;
    chrono::DateTime::parse_from_rfc2822(text).ok().map(|at| at.timestamp())
}

/// The rpc's time from a response: its `server_time`, else its `date` header.
fn response_secs(server_time: Option<&prost_types::Timestamp>, metadata: &MetadataMap) -> Option<i64> {
    server_time
        .map(|at| at.seconds)
        .filter(|secs| *secs > 0)
        .or_else(|| date_header_secs(metadata))
}

/// Why a call did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcError {
    /// No answer: transport error, timeout, Unavailable or DeadlineExceeded.
    Unreachable(String),
    /// The rpc has no control service.
    Unimplemented(String),
    /// Any other answer; `code` is N of `request refused (code N)`.
    Refused { code: Option<u16>, status: String },
}

impl RpcError {
    pub fn from_status(status: &tonic::Status) -> Self {
        use tonic::Code;
        let text = format!("{:?}: {}", status.code(), status.message());
        match status.code() {
            Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => Self::Unreachable(text),
            Code::Unimplemented => Self::Unimplemented(text),
            _ => Self::Refused { code: refusal_code(status.message()), status: text },
        }
    }

    /// A refusal the rpc repeats for the same upload bytes ([`DEFINITE_UPLOAD_REFUSALS`]).
    pub fn definite_code(&self) -> Option<u16> {
        match self {
            Self::Refused { code: Some(code), status }
                if DEFINITE_UPLOAD_REFUSALS.contains(code)
                    && !TRANSIENT_STATUSES.iter().any(|transient| status.starts_with(transient)) =>
            {
                Some(*code)
            }
            _ => None,
        }
    }

    /// Worth retrying: no answer, a failed call, or a refusal the rpc may not repeat (such
    /// as 4083, its rate limit). Local failures and a missing service are not.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Unreachable(_) => true,
            Self::Unimplemented(_) => false,
            Self::Refused { code: Some(_), .. } => self.definite_code().is_none(),
            Self::Refused { code: None, status } => {
                TRANSIENT_STATUSES.iter().any(|transient| status.starts_with(transient))
            }
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(text) => write!(f, "the rpc is unreachable ({text})"),
            Self::Unimplemented(text) => write!(f, "the rpc has no control service ({text})"),
            Self::Refused { status, .. } => write!(f, "the rpc refused the call ({status})"),
        }
    }
}

impl std::error::Error for RpcError {}

/// The SHA-256 a policy document is stored under.
pub(crate) fn document_hash(bytes: &[u8]) -> Hash32 {
    use sha2::Digest;
    Hash32(sha2::Sha256::digest(bytes).into())
}

/// N of a `request refused (code N)` message.
pub fn refusal_code(message: &str) -> Option<u16> {
    let rest = message.split("request refused (code ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !rest.get(digits.len()..).is_some_and(|r| r.starts_with(')')) {
        return None;
    }
    digits.parse().ok()
}

/// A verified-shape copy of the rpc's log: rows, documents and what the rpc says of it.
#[derive(Clone, Debug, Default)]
pub struct RemoteLog {
    /// Rows from seq 1 up to the first seq the rpc skipped, in order.
    pub entries: Vec<(Vec<u8>, [u8; 64])>,
    /// Rows after the first skipped seq, with their seqs, ascending.
    pub beyond_gap: Vec<(u64, Vec<u8>, [u8; 64])>,
    /// The first seq the rpc skipped.
    pub gap_at: Option<u64>,
    /// The highest seq the rpc reports storing: its last row or its stored head.
    pub stored_tip: u64,
    /// Policy documents, each under the hash of its bytes.
    pub documents: Vec<Vec<u8>>,
    /// The rpc's `upload_from`: it lacks entries from this seq on. A hint only.
    pub upload_from: Option<u64>,
    /// The rpc reads its copy as Broken at `problem_seq`. A hint only.
    pub broken: bool,
    pub problem_seq: Option<u64>,
}

impl RemoteLog {
    /// Every row with its seq, ascending.
    pub fn rows(&self) -> impl Iterator<Item = (u64, &[u8], &[u8; 64])> {
        let contiguous = self
            .entries
            .iter()
            .zip(1u64..)
            .map(|((bytes, signature), seq)| (seq, bytes.as_slice(), signature));
        let rest = self.beyond_gap.iter().map(|(seq, bytes, signature)| (*seq, bytes.as_slice(), signature));
        contiguous.chain(rest)
    }

    /// The highest seq the rpc stores or reports.
    pub fn tip(&self) -> u64 {
        let rows = self.rows().map(|(seq, ..)| seq).max().unwrap_or(0);
        rows.max(self.stored_tip)
    }

    /// Whether the rpc holds no row and reports no head.
    pub fn is_empty(&self) -> bool {
        self.tip() == 0
    }

    /// The documents as a set keyed by their hashes.
    pub fn policy_documents(&self) -> PolicyDocuments {
        let mut docs = PolicyDocuments::new();
        for document in &self.documents {
            docs.insert(document.clone());
        }
        docs
    }
}

/// What GetControlDescriptor returned; only the descriptor bytes are trusted, after re-hashing.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub bytes: Vec<u8>,
    pub purpose: ControlLinkPurpose,
    pub status: ControlLinkStatus,
    pub invite_code: Option<String>,
}

/// The offset of the rpc's clock from this one, and when it was read.
#[derive(Clone, Copy, Debug)]
struct ClockSample {
    offset_secs: i64,
    read_at: i64,
}

/// Where a reading of the rpc's clock came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockSource {
    Response,
    Heartbeat,
}

/// The last reading from a response, and from a heartbeat, used when no recent response carried one.
#[derive(Clone, Copy, Debug, Default)]
struct Clock {
    response: Option<ClockSample>,
    heartbeat: Option<ClockSample>,
}

/// Calls AgentControlAgentService as this agent.
#[derive(Clone)]
pub struct ControlRpc {
    channel: Channel,
    token: TokenSource,
    append_token: AppendTokenSource,
    timeout: Duration,
    clock: Arc<Mutex<Clock>>,
}

impl std::fmt::Debug for ControlRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlRpc").field("timeout", &self.timeout).finish_non_exhaustive()
    }
}

/// The local clock in seconds; an unreadable clock refuses.
fn local_now() -> Result<i64> {
    agent_logic::clock::unix_now()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| anyhow!("the local clock is unreadable"))
}

impl ControlRpc {
    /// A client over `channel`; appends take their tokens from `append_token`, other calls from `token`.
    pub fn new(channel: Channel, token: TokenSource, append_token: AppendTokenSource) -> Self {
        Self { channel, token, append_token, timeout: CALL_TIMEOUT, clock: Arc::new(Mutex::new(Clock::default())) }
    }

    /// Connects to the config's rpc. Tokens (at most 900 s) claim the accepted head,
    /// except an append's, which claims the head after it.
    pub async fn connect(config: &BaseConfig) -> Result<Self, RpcError> {
        let opts = agent_logic::transport::ChannelOpts::default();
        let connect = agent_logic::transport::connect_channel(&config.orderbook_grpc_url, opts);
        let channel = match tokio::time::timeout(CALL_TIMEOUT, connect).await {
            Ok(Ok(channel)) => channel,
            Ok(Err(e)) => return Err(RpcError::Unreachable(format!("{e:#}"))),
            Err(_) => return Err(RpcError::Unreachable(format!("no connection within {CALL_TIMEOUT:?}"))),
        };
        let config = config.clone();
        let append_config = config.clone();
        Ok(Self::new(
            channel,
            Arc::new(move || config.control_jwt().map(|(token, _)| token)),
            Arc::new(move |store: &ControlStore, lock: &ControlLock, head: &HeadRef| {
                append_config.control_jwt_for_head(store, lock, head)
            }),
        ))
    }

    /// Every later call is bounded by `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn client(&self) -> AgentControlAgentServiceClient<Channel> {
        AgentControlAgentServiceClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_RESPONSE_BYTES)
    }

    fn request<T>(&self, message: T) -> Result<tonic::Request<T>, RpcError> {
        let token = (self.token)()
            .map_err(|e| RpcError::Refused { code: None, status: format!("no control token: {e:#}") })?;
        self.request_with(&token, message)
    }

    fn request_with<T>(&self, token: &str, message: T) -> Result<tonic::Request<T>, RpcError> {
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| RpcError::Refused { code: None, status: "the control token is not a header value".into() })?;
        let mut request = tonic::Request::new(message);
        request.metadata_mut().insert("authorization", bearer);
        request.set_timeout(self.timeout);
        Ok(request)
    }

    /// Runs one call within the bound and reads the rpc's clock from its answer.
    async fn call<T, F>(&self, call: F) -> Result<T, RpcError>
    where
        T: ServerTime,
        F: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        match tokio::time::timeout(self.timeout, call).await {
            Err(_) => Err(RpcError::Unreachable(format!("no answer within {:?}", self.timeout))),
            Ok(Err(status)) => {
                if let Some(secs) = date_header_secs(status.metadata()) {
                    self.observe(ClockSource::Response, secs);
                }
                Err(RpcError::from_status(&status))
            }
            Ok(Ok(response)) => {
                if let Some(secs) = response_secs(response.get_ref().server_time(), response.metadata()) {
                    self.observe(ClockSource::Response, secs);
                }
                Ok(response.into_inner())
            }
        }
    }

    fn observe(&self, source: ClockSource, server_secs: i64) {
        if server_secs <= 0 {
            return;
        }
        if let Ok(local) = local_now() {
            let sample = Some(ClockSample { offset_secs: server_secs.saturating_sub(local), read_at: local });
            let mut clock = agent_logic::sync::lock(&self.clock);
            match source {
                ClockSource::Response => clock.response = sample,
                ClockSource::Heartbeat => clock.heartbeat = sample,
            }
        }
    }

    /// Records the rpc's time from a ControlEvents heartbeat.
    pub fn observe_heartbeat(&self, heartbeat: &ControlHeartbeat) {
        if let Some(at) = &heartbeat.at {
            self.observe(ClockSource::Heartbeat, at.seconds);
        }
    }

    /// The time to sign a control entry at: now, when this clock is within
    /// [`MAX_CLOCK_SKEW_SECS`] of the rpc's as last read.
    pub fn checked_now(&self) -> Result<UtcTime> {
        let local = local_now()?;
        let clock = *agent_logic::sync::lock(&self.clock);
        let fresh = |sample: &ClockSample| {
            local.saturating_sub(sample.read_at).unsigned_abs() <= CLOCK_SAMPLE_MAX_AGE_SECS.unsigned_abs()
        };
        let sample = match (clock.response, clock.heartbeat) {
            (Some(sample), _) if fresh(&sample) => sample,
            (_, Some(sample)) if fresh(&sample) => sample,
            (None, None) => bail!("refusing to sign: the rpc's time is unknown (no response carried it); retry"),
            _ => bail!("refusing to sign: the rpc's time was read too long ago; retry"),
        };
        let skew = sample.offset_secs.unsigned_abs();
        if skew > MAX_CLOCK_SKEW_SECS.unsigned_abs() {
            bail!(
                "refusing to sign: this machine's clock is {skew}s {} the rpc's (at most {MAX_CLOCK_SKEW_SECS}s \
                 allowed): fix the clock and retry",
                if sample.offset_secs > 0 { "behind" } else { "ahead of" }
            );
        }
        UtcTime::from_unix_secs(local).ok_or_else(|| anyhow!("the local clock is out of range"))
    }

    /// The whole remote log with its documents, page by page.
    pub async fn fetch_log(&self) -> Result<RemoteLog, RpcError> {
        match tokio::time::timeout(LOG_TIMEOUT, self.fetch_pages()).await {
            Ok(result) => result,
            Err(_) => Err(RpcError::Unreachable(format!("the log was not read within {LOG_TIMEOUT:?}"))),
        }
    }

    /// A seq the rpc skips is recorded, not refused: the rows after it are kept with their
    /// seqs, so a repair can tell missing rows from changed ones.
    async fn fetch_pages(&self) -> Result<RemoteLog, RpcError> {
        let refused = |status: String| RpcError::Refused { code: None, status };
        let mut log = RemoteLog::default();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut from_seq: u64 = 1;
        let mut next_contiguous: u64 = 1;
        loop {
            let request = self.request(GetAgentControlLogRequest { from_seq, limit: PAGE_LIMIT })?;
            let mut client = self.client();
            let page = self.call(client.get_control_log(request)).await?;
            let got = page.entries.len();
            for entry in page.entries {
                if entry.seq < from_seq || entry.seq == 0 {
                    return Err(refused(format!("the rpc sent entry {} out of order (from seq {from_seq})", entry.seq)));
                }
                let signature: [u8; 64] = entry
                    .signature
                    .as_slice()
                    .try_into()
                    .map_err(|_| refused(format!("entry {} has no 64-byte signature", entry.seq)))?;
                if log.gap_at.is_none() && entry.seq == next_contiguous {
                    log.entries.push((entry.canonical, signature));
                    next_contiguous = next_contiguous.saturating_add(1);
                } else {
                    log.gap_at.get_or_insert(next_contiguous);
                    log.beyond_gap.push((entry.seq, entry.canonical, signature));
                }
                log.stored_tip = log.stored_tip.max(entry.seq);
                from_seq = entry.seq.saturating_add(1);
            }
            for document in page.documents {
                if document.policy_hash != document_hash(&document.document).to_string() {
                    return Err(refused(format!("policy document {} does not hash to its name", document.policy_hash)));
                }
                if seen.insert(document.policy_hash) {
                    log.documents.push(document.document);
                }
            }
            if log.entries.len().saturating_add(log.beyond_gap.len()) > MAX_LOG_ENTRIES
                || log.documents.len() > MAX_LOG_ENTRIES
            {
                return Err(refused("the rpc sent more than a log may hold".into()));
            }
            log.upload_from = page.upload_from.or(log.upload_from);
            log.problem_seq = page.problem_seq.or(log.problem_seq);
            log.broken |= page.verification == ControlVerification::Broken as i32;
            if let Some(head) = &page.stored_head {
                log.stored_tip = log.stored_tip.max(head.seq);
            }
            if !page.has_more || got == 0 {
                return Ok(log);
            }
        }
    }

    /// Uploads `entries` (in order) with the documents they reference; returns the rpc's head.
    /// Its token claims the last entry's head, which `store` must hold under `lock`.
    pub async fn append(
        &self,
        store: &ControlStore,
        lock: &ControlLock,
        entries: &[SignedEntry],
        documents: &[Vec<u8>],
    ) -> Result<HeadRef, RpcError> {
        let claim = last_head(entries)?;
        self.append_claiming(store, lock, entries, documents, &claim).await
    }

    /// [`Self::append`] whose token claims `claim`, at or above the last entry; `store`
    /// must hold it under `lock`.
    pub async fn append_claiming(
        &self,
        store: &ControlStore,
        lock: &ControlLock,
        entries: &[SignedEntry],
        documents: &[Vec<u8>],
        claim: &HeadRef,
    ) -> Result<HeadRef, RpcError> {
        let last = last_head(entries)?;
        if claim.seq < last.seq {
            return Err(RpcError::Refused {
                code: None,
                status: format!("the claim (seq {}) is below the upload's last entry (seq {})", claim.seq, last.seq),
            });
        }
        let token = (self.append_token)(store, lock, claim)
            .map_err(|e| RpcError::Refused { code: None, status: format!("no append token: {e:#}") })?;
        let request = self.request_with(&token, AppendControlEntriesRequest {
            entries: entries
                .iter()
                .map(|e| ControlLogEntry {
                    seq: e.seq(),
                    canonical: e.bytes().to_vec(),
                    signature: e.signature().to_vec(),
                    entry_hash: e.hash().to_string(),
                })
                .collect(),
            documents: documents
                .iter()
                .map(|d| ControlPolicyDocument { policy_hash: document_hash(d).to_string(), document: d.clone() })
                .collect(),
        })?;
        let mut client = self.client();
        let response = self.call(client.append_control_entries(request)).await?;
        head_of(response.head.as_ref())?
            .ok_or_else(|| RpcError::Refused { code: None, status: "the rpc reported no head".into() })
    }

    /// [`Self::append`], retried with backoff while the answer is transient
    /// ([`RpcError::is_transient`]); an append the rpc already holds is accepted again.
    pub async fn append_retrying(
        &self,
        store: &ControlStore,
        lock: &ControlLock,
        entries: &[SignedEntry],
        documents: &[Vec<u8>],
    ) -> Result<HeadRef, RpcError> {
        let claim = last_head(entries)?;
        self.append_retrying_claiming(store, lock, entries, documents, &claim).await
    }

    /// [`Self::append_claiming`], retried as [`Self::append_retrying`] is.
    pub async fn append_retrying_claiming(
        &self,
        store: &ControlStore,
        lock: &ControlLock,
        entries: &[SignedEntry],
        documents: &[Vec<u8>],
        claim: &HeadRef,
    ) -> Result<HeadRef, RpcError> {
        let mut delays = RETRY_DELAYS.iter();
        loop {
            match self.append_claiming(store, lock, entries, documents, claim).await {
                Err(e) if e.is_transient() => match delays.next() {
                    Some(delay) => {
                        tracing::warn!("agent control: upload not accepted yet ({e}); retrying in {delay:?}");
                        tokio::time::sleep(*delay).await;
                    }
                    None => return Err(e),
                },
                other => return other,
            }
        }
    }

    /// The link descriptor stored under `commitment` (32 lowercase hex).
    pub async fn descriptor(&self, commitment: &agent_control::Commitment) -> Result<Descriptor, RpcError> {
        let request = self.request(GetControlDescriptorRequest { commitment: commitment.to_string() })?;
        let mut client = self.client();
        let response = self.call(client.get_control_descriptor(request)).await?;
        Ok(Descriptor {
            purpose: ControlLinkPurpose::try_from(response.purpose).unwrap_or(ControlLinkPurpose::Unspecified),
            status: ControlLinkStatus::try_from(response.status).unwrap_or(ControlLinkStatus::Unspecified),
            invite_code: response.invite_code,
            bytes: response.descriptor,
        })
    }

    /// Pending policies and commands, as the rpc lists them.
    pub async fn pending(&self) -> Result<ListPendingControlResponse, RpcError> {
        let request = self.request(ListPendingControlRequest {})?;
        let mut client = self.client();
        self.call(client.list_pending_control(request)).await
    }

    /// The policy stored under `policy_hash`; only its document and proof are trusted,
    /// after re-hashing.
    pub async fn policy(&self, policy_hash: &Hash32) -> Result<ControlPolicyView, RpcError> {
        let request = self.request(GetPolicyRequest { policy_hash: policy_hash.to_string() })?;
        let mut client = self.client();
        let response = self.call(client.get_policy(request)).await?;
        let view = response
            .policy
            .ok_or_else(|| RpcError::Refused { code: None, status: "the rpc returned no policy".into() })?;
        if document_hash(&view.document) != *policy_hash {
            return Err(RpcError::Refused {
                code: None,
                status: format!("the policy the rpc returned does not hash to {policy_hash}"),
            });
        }
        Ok(view)
    }

    /// Answers a pending policy with the agent's signed refusal receipt.
    pub async fn ack_policy(
        &self,
        policy_hash: &Hash32,
        receipt: Vec<u8>,
        signature: [u8; 64],
    ) -> Result<AckPolicyResponse, RpcError> {
        let request = self.request(AckPolicyRequest {
            policy_hash: policy_hash.to_string(),
            receipt,
            signature: signature.to_vec(),
        })?;
        let mut client = self.client();
        self.call(client.ack_policy(request)).await
    }

    /// Delivers the agent's signed receipt for a command.
    pub async fn post_receipt(&self, receipt: Vec<u8>, signature: [u8; 64]) -> Result<PostReceiptResponse, RpcError> {
        let request = self.request(PostReceiptRequest { receipt, signature: signature.to_vec() })?;
        let mut client = self.client();
        self.call(client.post_receipt(request)).await
    }
}

/// The head of the last of `entries`; an empty upload is refused locally.
fn last_head(entries: &[SignedEntry]) -> Result<HeadRef, RpcError> {
    entries
        .last()
        .map(|last| HeadRef { seq: last.seq(), hash: *last.hash() })
        .ok_or_else(|| RpcError::Refused { code: None, status: "nothing to upload".into() })
}

/// A head from the rpc; an empty one is none.
pub fn head_of(head: Option<&ControlHead>) -> Result<Option<HeadRef>, RpcError> {
    match head {
        None => Ok(None),
        Some(head) if head.seq == 0 => Ok(None),
        Some(head) => Hash32::parse(&head.hash)
            .map(|hash| Some(HeadRef { seq: head.seq, hash }))
            .map_err(|_| RpcError::Refused { code: None, status: format!("the rpc sent an invalid head hash at seq {}", head.seq) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_codes_are_read_from_the_opaque_message() {
        assert_eq!(refusal_code("request refused (code 4090)"), Some(4090));
        assert_eq!(refusal_code("x: request refused (code 4085) y"), Some(4085));
        assert_eq!(refusal_code("request refused (code 40a)"), None);
        assert_eq!(refusal_code("refused"), None);
        let definite = RpcError::from_status(&tonic::Status::failed_precondition("request refused (code 4085)"));
        assert_eq!(definite.definite_code(), Some(4085));
        for status in [
            tonic::Status::internal("request refused (code 4085)"),
            tonic::Status::unavailable("request refused (code 4085)"),
            tonic::Status::failed_precondition("request refused (code 4101)"),
            tonic::Status::failed_precondition("no code"),
        ] {
            assert_eq!(RpcError::from_status(&status).definite_code(), None, "{status:?}");
        }
        assert!(matches!(RpcError::from_status(&tonic::Status::unimplemented("")), RpcError::Unimplemented(_)));
    }

    #[test]
    fn only_the_shared_definite_codes_are_definite_and_the_rest_retry() {
        let refusal = |code: u16| RpcError::from_status(&tonic::Status::permission_denied(format!("request refused (code {code})")));
        for code in 4081..=4095u16 {
            let e = refusal(code);
            if DEFINITE_UPLOAD_REFUSALS.contains(&code) {
                assert_eq!(e.definite_code(), Some(code));
                assert!(!e.is_transient(), "{code}");
            } else {
                assert_eq!(e.definite_code(), None, "{code}");
                assert!(e.is_transient(), "{code}");
            }
        }
        assert!(refusal(4083).is_transient(), "the rate limit passes");
        assert!(RpcError::Unreachable("down".into()).is_transient());
        assert!(!RpcError::Unimplemented("none".into()).is_transient());
        assert!(RpcError::from_status(&tonic::Status::internal("boom")).is_transient());
        let local = RpcError::Refused { code: None, status: "no append token: does not hold".into() };
        assert!(!local.is_transient() && local.definite_code().is_none());
    }

    #[test]
    fn rows_tip_and_emptiness_cover_rows_past_a_gap() {
        let row = |n: u8| (vec![n], [n; 64]);
        let mut log = RemoteLog { entries: vec![row(1), row(2)], ..Default::default() };
        assert_eq!(log.tip(), 2);
        log.gap_at = Some(3);
        log.beyond_gap.push((5, vec![5], [5; 64]));
        assert_eq!(log.rows().map(|(seq, ..)| seq).collect::<Vec<_>>(), vec![1, 2, 5]);
        assert_eq!(log.tip(), 5);
        log.stored_tip = 7;
        assert_eq!(log.tip(), 7);
        assert!(RemoteLog::default().is_empty() && !log.is_empty());
    }

    fn offline_rpc() -> ControlRpc {
        let channel = Channel::from_static("http://127.0.0.1:1").connect_lazy();
        ControlRpc::new(channel, Arc::new(|| Ok("t".into())), Arc::new(|_: &ControlStore, _: &ControlLock, _: &HeadRef| Ok("t".into())))
    }

    #[tokio::test]
    async fn the_clock_check_needs_a_recent_close_reading() {
        let rpc = offline_rpc();
        assert!(rpc.checked_now().unwrap_err().to_string().contains("unknown"));
        let now = local_now().unwrap();
        rpc.observe(ClockSource::Response, now + 30);
        assert!(rpc.checked_now().is_ok());
        rpc.observe(ClockSource::Response, now + 61);
        let err = rpc.checked_now().unwrap_err().to_string();
        assert!(err.contains("behind") && err.contains("fix the clock"), "{err}");
        rpc.observe(ClockSource::Response, now - 120);
        assert!(rpc.checked_now().unwrap_err().to_string().contains("ahead of"));
        // A reading that is no time at all is ignored.
        rpc.observe(ClockSource::Response, i64::MIN);
        rpc.observe(ClockSource::Response, 0);
        assert!(rpc.checked_now().unwrap_err().to_string().contains("ahead of"));
    }

    #[test]
    fn server_time_comes_first_then_the_date_header() {
        let mut metadata = MetadataMap::new();
        assert_eq!(response_secs(None, &metadata), None);
        metadata.insert("date", "Sun, 22 Nov 2026 10:00:00 GMT".parse().unwrap());
        let header = chrono::DateTime::parse_from_rfc2822("Sun, 22 Nov 2026 10:00:00 GMT").unwrap().timestamp();
        assert_eq!(response_secs(None, &metadata), Some(header));
        let field = prost_types::Timestamp { seconds: header + 500, nanos: 0 };
        assert_eq!(response_secs(Some(&field), &metadata), Some(header + 500));
        let unset = prost_types::Timestamp { seconds: 0, nanos: 0 };
        assert_eq!(response_secs(Some(&unset), &metadata), Some(header));
        assert_eq!(response_secs(Some(&field), &MetadataMap::new()), Some(header + 500));
    }

    #[tokio::test]
    async fn a_heartbeat_counts_only_without_a_recent_response_reading() {
        let rpc = offline_rpc();
        let now = local_now().unwrap();
        let beat = |secs: i64| ControlHeartbeat { at: Some(prost_types::Timestamp { seconds: secs, nanos: 0 }), stored_head: None };
        rpc.observe_heartbeat(&beat(now + 10));
        assert!(rpc.checked_now().is_ok(), "the heartbeat is the fallback");
        rpc.observe(ClockSource::Response, now + 300);
        rpc.observe_heartbeat(&beat(now));
        assert!(rpc.checked_now().unwrap_err().to_string().contains("behind"), "a response reading comes first");
        // A stale response reading falls back to the heartbeat.
        agent_logic::sync::lock(&rpc.clock).response =
            Some(ClockSample { offset_secs: 300, read_at: now - CLOCK_SAMPLE_MAX_AGE_SECS - 1 });
        assert!(rpc.checked_now().is_ok());
        agent_logic::sync::lock(&rpc.clock).heartbeat = None;
        assert!(rpc.checked_now().unwrap_err().to_string().contains("too long ago"));
    }
}
