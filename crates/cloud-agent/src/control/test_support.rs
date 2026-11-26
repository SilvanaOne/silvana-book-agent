//! An in-process AgentControlAgentService and the control command tests run against it.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use agent_control::{
    Capability, CapabilitySet, ControllerDescriptor, ControllerKey, EntryBody, ForkEvidence, HeadRef,
    Label, LinkDescriptor, LinkPurpose, MAX_APPEND_DOCUMENTS, MAX_APPEND_ENTRIES, MAX_LOG_ENTRIES,
    Network, Nonce16, OwnerAccount, PartyId, SignedEntry, UtcTime, VerifiedLog,
};
use agent_logic::config::BaseConfig;
use agent_logic::control_store::{ControlEnv, ControlMarker, ControlStore, MARKER_ENV};
use agent_logic::control_store::testing::{TestChain, party_for};
use agent_logic::secret::Secret;
use orderbook_proto::orderbook::{
    AckPolicyRequest, AckPolicyResponse, AppendControlEntriesRequest, AppendControlEntriesResponse,
    ControlHead, ControlLinkPurpose, ControlLinkStatus, ControlLogEntry, ControlPolicyStatus,
    ControlVerification, GetAgentControlLogRequest, GetAgentControlLogResponse,
    GetControlDescriptorRequest, GetControlDescriptorResponse, ListPendingControlRequest,
    ListPendingControlResponse, PostReceiptRequest, PostReceiptResponse,
};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http;
use tonic::{Response, Status};

use super::prompt::Scripted;
use super::{ControlCommands, ControlContext};

// ---- a prost codec for hand-written services ----

pub(crate) struct Prost<E, D>(PhantomData<fn() -> (E, D)>);

impl<E, D> Default for Prost<E, D> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

pub(crate) struct ProstEncoder<E>(PhantomData<fn() -> E>);
pub(crate) struct ProstDecoder<D>(PhantomData<fn() -> D>);

impl<E: prost::Message> Encoder for ProstEncoder<E> {
    type Item = E;
    type Error = Status;
    fn encode(&mut self, item: E, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst).map_err(|e| Status::internal(e.to_string()))
    }
}

impl<D: prost::Message + Default> Decoder for ProstDecoder<D> {
    type Item = D;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<D>, Status> {
        D::decode(src).map(Some).map_err(|e| Status::internal(e.to_string()))
    }
}

impl<E, D> Codec for Prost<E, D>
where
    E: prost::Message + Send + 'static,
    D: prost::Message + Default + Send + 'static,
{
    type Encode = E;
    type Decode = D;
    type Encoder = ProstEncoder<E>;
    type Decoder = ProstDecoder<D>;
    fn encoder(&mut self) -> ProstEncoder<E> {
        ProstEncoder(PhantomData)
    }
    fn decoder(&mut self) -> ProstDecoder<D> {
        ProstDecoder(PhantomData)
    }
}

/// What the fake reads from a call's bearer token.
#[derive(Clone, Debug)]
pub(crate) struct Caller {
    pub claim: Option<HeadRef>,
    pub party: PartyId,
}

/// The audience every control-service token names on the fake's network.
pub(crate) const FAKE_AUDIENCE: &str = "silvana.agent.control.devnet";

/// The token's claim, as the rpc's interceptor checks it: a key-possession control JWT of
/// the party's key, living at most 900 s, with a well-formed head claim or none.
fn caller_of(metadata: &tonic::metadata::MetadataMap) -> Result<Caller, Status> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let unauthenticated = || Status::unauthenticated("unauthenticated");
    let token = metadata
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(unauthenticated)?;
    let (input, signature) = token.rsplit_once('.').ok_or_else(unauthenticated)?;
    let (header, claims) = input.split_once('.').ok_or_else(unauthenticated)?;
    let json = |segment: &str| -> Result<serde_json::Value, Status> {
        let raw = URL_SAFE_NO_PAD.decode(segment).map_err(|_| unauthenticated())?;
        serde_json::from_slice(&raw).map_err(|_| unauthenticated())
    };
    let (header, claims) = (json(header)?, json(claims)?);
    let key: [u8; 32] = header
        .pointer("/jwk/x")
        .and_then(|x| x.as_str())
        .and_then(|x| URL_SAFE_NO_PAD.decode(x).ok())
        .and_then(|x| x.try_into().ok())
        .ok_or_else(unauthenticated)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or_else(unauthenticated)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| unauthenticated())?;
    key.verify_strict(input.as_bytes(), &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| unauthenticated())?;
    let party = claims
        .get("sub")
        .and_then(|s| s.as_str())
        .and_then(|s| PartyId::parse(s).ok())
        .ok_or_else(unauthenticated)?;
    if !agent_control::verify_party_key(&party, key.as_bytes()) {
        return Err(unauthenticated());
    }
    let (iat, exp) = (claims.get("iat").and_then(|v| v.as_u64()), claims.get("exp").and_then(|v| v.as_u64()));
    match (iat, exp) {
        (Some(iat), Some(exp)) if exp.saturating_sub(iat) <= 900 => {}
        _ => return Err(unauthenticated()),
    }
    if claims.get("aud").and_then(|v| v.as_str()) != Some(FAKE_AUDIENCE) {
        return Err(unauthenticated());
    }
    let claim = HeadRef::from_jwt_claims(
        claims.get("ctl_seq").and_then(|v| v.as_u64()),
        claims.get("ctl_head").and_then(|v| v.as_str()),
    )
    .map_err(|_| unauthenticated())?;
    Ok(Caller { claim, party })
}

type Handler<Q, R> = Arc<dyn Fn(Caller, Q) -> Result<R, Status> + Send + Sync>;

struct Unary<Q, R> {
    handler: Handler<Q, R>,
    date: Option<String>,
}

impl<Q: Send + 'static, R: Send + 'static> tonic::server::UnaryService<Q> for Unary<Q, R> {
    type Response = R;
    type Future = tonic::codegen::BoxFuture<Response<R>, Status>;
    fn call(&mut self, request: tonic::Request<Q>) -> Self::Future {
        let result = caller_of(request.metadata()).and_then(|caller| (self.handler)(caller, request.into_inner()));
        let date: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>> =
            self.date.as_ref().map(|d| d.parse().unwrap());
        Box::pin(async move {
            match result {
                Ok(reply) => {
                    let mut response = Response::new(reply);
                    if let Some(date) = date {
                        response.metadata_mut().insert("date", date);
                    }
                    Ok(response)
                }
                Err(mut status) => {
                    if let Some(date) = date {
                        status.metadata_mut().insert("date", date);
                    }
                    Err(status)
                }
            }
        })
    }
}

/// A unary call that needs no token, such as GetAgentConfig.
struct Open<R> {
    reply: Result<R, Status>,
}

impl<Q: Send + 'static, R: Send + 'static> tonic::server::UnaryService<Q> for Open<R> {
    type Response = R;
    type Future = tonic::codegen::BoxFuture<Response<R>, Status>;
    fn call(&mut self, _request: tonic::Request<Q>) -> Self::Future {
        let reply = std::mem::replace(&mut self.reply, Err(Status::internal("answered")));
        Box::pin(async move { reply.map(Response::new) })
    }
}

// ---- the fake rpc ----

/// What the fake holds and how it answers.
#[derive(Default)]
pub(crate) struct FakeState {
    /// Stored rows, ascending by seq; a test may delete or edit any of them.
    pub entries: Vec<ControlLogEntry>,
    pub documents: Vec<orderbook_proto::orderbook::ControlPolicyDocument>,
    pub descriptors: BTreeMap<String, GetControlDescriptorResponse>,
    /// The next AppendControlEntries answers this instead.
    pub append_failure: Option<Status>,
    /// Every append also fails with this, keeping nothing.
    pub always_fail_append: Option<Status>,
    /// GetControlLog answers this instead.
    pub log_failure: Option<Status>,
    /// The fake's clock runs this far ahead of the test's.
    pub clock_offset_secs: i64,
    /// Responses carry no `server_time`.
    pub omit_server_time: bool,
    /// Answers carry no `date` header.
    pub omit_date: bool,
    /// The heads row's head: raised by every stored append, never lowered; a test sets or
    /// clears it (a database restore, an operator re-seed).
    pub recorded_head: Option<HeadRef>,
    /// The gate's accepted head: raised by every stored append; cleared by a restart.
    pub accepted_head: Option<HeadRef>,
    /// The highest head a token claimed. It is only observed: a claim never vouches.
    pub highest_claim: Option<HeadRef>,
    /// Fork evidence stored by refused appends.
    pub forks: Vec<ForkEvidence>,
    pub appends: Vec<Vec<u64>>,
    /// The number of documents each append carried.
    pub append_documents: Vec<usize>,
    pub calls: Vec<String>,
    /// Each handled call's method and the head its token claimed.
    pub claims: Vec<(String, Option<HeadRef>)>,
    /// Pending policies by hash, with the party each belongs to.
    pub policies: BTreeMap<String, PartyId>,
    /// Commands by id: the party and the command bytes.
    pub commands: BTreeMap<String, (PartyId, Vec<u8>)>,
    /// The refusal receipts AckPolicy took, by policy hash.
    pub acks: Vec<(String, Vec<u8>)>,
    /// The receipts PostReceipt took, by command id.
    pub receipts: Vec<(String, Vec<u8>)>,
    /// The env GetAgentConfig answers with; none answers Unimplemented.
    pub agent_env: Option<String>,
    /// What ListNodePackages answers, `as_of` set by the fake; none answers Unavailable.
    pub node_packages: Option<orderbook_proto::orderbook::ListNodePackagesResponse>,
    /// The `include_removed` of each ListNodePackages call.
    pub node_package_calls: Vec<bool>,
}

pub(crate) type Shared = Arc<Mutex<FakeState>>;

#[derive(Clone)]
struct FakeService(Shared);

impl tonic::server::NamedService for FakeService {
    const NAME: &'static str = "silvana.orderbook.v1.AgentControlAgentService";
}

/// The same fake under the ledger-service's name, for GetAgentConfig.
#[derive(Clone)]
struct FakeDapp(FakeService);

impl tonic::server::NamedService for FakeDapp {
    const NAME: &'static str = "silvana.ledger.v1.DAppProviderService";
}

impl<B> tonic::codegen::Service<http::Request<B>> for FakeDapp
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
        self.0.call(req)
    }
}

fn http_date(offset: i64) -> String {
    let now = agent_logic::clock::now_utc() + chrono::Duration::seconds(offset);
    now.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// The fake's clock, in seconds since the epoch.
fn server_now(state: &FakeState) -> i64 {
    agent_logic::clock::now_secs_i64() + state.clock_offset_secs
}

fn server_time(state: &FakeState) -> Option<prost_types::Timestamp> {
    (!state.omit_server_time).then(|| prost_types::Timestamp { seconds: server_now(state), nanos: 0 })
}

/// The rpc's opaque refusal.
fn refused(code: u16) -> Status {
    Status::permission_denied(format!("request refused (code {code})"))
}

/// Raises a head; never lowers it.
fn raise(slot: &mut Option<HeadRef>, head: HeadRef) {
    if slot.is_none_or(|seen| seen.seq < head.seq) {
        *slot = Some(head);
    }
}

/// Records the claim of a call other than an append.
fn note_claim(state: &mut FakeState, method: &str, claim: Option<HeadRef>) {
    state.claims.push((method.to_string(), claim));
    if let Some(claim) = claim {
        raise(&mut state.highest_claim, claim);
    }
}

fn head_proto(head: Option<HeadRef>) -> ControlHead {
    head.map_or_else(ControlHead::default, |head| ControlHead { seq: head.seq, hash: head.hash.to_string() })
}

fn decode_row(row: &ControlLogEntry) -> Option<SignedEntry> {
    let signature: [u8; 64] = row.signature.as_slice().try_into().ok()?;
    SignedEntry::new(row.canonical.clone(), signature).ok()
}

/// The longest stored prefix that decodes, counts up from 1 and verifies (plan_append's).
fn verified_prefix(state: &FakeState, party: &PartyId) -> Option<VerifiedLog> {
    let mut decoded = Vec::new();
    for (row, position) in state.entries.iter().zip(1u64..) {
        if row.seq != position {
            break;
        }
        match decode_row(row) {
            Some(entry) => decoded.push(entry),
            None => break,
        }
    }
    super::longest_valid(Network::Devnet, party, decoded)
}

fn stored_tip(state: &FakeState, prefix: Option<&VerifiedLog>) -> u64 {
    let rows = state.entries.last().map_or(0, |row| row.seq);
    rows.max(prefix.map_or(0, |log| log.head().seq))
}

/// plan_append's partial repair: `log` extended with the stored rows right above its head,
/// up to the first one missing, undecodable or not chaining on.
fn extend_with_stored(log: VerifiedLog, stored: &[ControlLogEntry]) -> VerifiedLog {
    let head = log.head().seq;
    let mut rows: Vec<SignedEntry> = Vec::new();
    for row in stored.iter().filter(|row| row.seq > head) {
        if row.seq != head + 1 + rows.len() as u64 {
            break;
        }
        match decode_row(row) {
            Some(entry) => rows.push(entry),
            None => break,
        }
    }
    if rows.is_empty() {
        return log;
    }
    match agent_control::verify_extension(&log, &rows) {
        Ok(extended) => extended,
        Err(e) => {
            rows.truncate(usize::try_from(e.seq.saturating_sub(head).saturating_sub(1)).unwrap().min(rows.len()));
            if rows.is_empty() {
                return log;
            }
            agent_control::verify_extension(&log, &rows).unwrap_or(log)
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    super::rpc::document_hash(bytes).to_string()
}

fn accepted_hash(entry: &SignedEntry) -> Option<String> {
    match entry.entry().body() {
        EntryBody::AcceptPolicy(body) => Some(body.policy_hash.to_string()),
        _ => None,
    }
}

/// Stores the offered documents `entries` accept that are not stored yet.
fn store_documents<'a>(
    state: &mut FakeState,
    entries: impl Iterator<Item = &'a SignedEntry>,
    offered: &BTreeMap<String, Vec<u8>>,
) {
    for hash in entries.filter_map(accepted_hash) {
        if state.documents.iter().any(|d| d.policy_hash == hash) {
            continue;
        }
        if let Some(document) = offered.get(&hash) {
            state.documents.push(orderbook_proto::orderbook::ControlPolicyDocument {
                policy_hash: hash,
                document: document.clone(),
            });
        }
    }
}

/// GetControlLog as the rpc serves it: the raw rows, and its verdict on them.
fn log_page(state: &mut FakeState, caller: Caller, request: GetAgentControlLogRequest) -> Result<GetAgentControlLogResponse, Status> {
    note_claim(state, "GetControlLog", caller.claim);
    if let Some(status) = &state.log_failure {
        return Err(status.clone());
    }
    let from = request.from_seq.max(1);
    let limit = usize::try_from(request.limit.max(1)).unwrap();
    let rows: Vec<&ControlLogEntry> = state.entries.iter().filter(|row| row.seq >= from).collect();
    let entries: Vec<ControlLogEntry> = rows.iter().take(limit).map(|row| (*row).clone()).collect();
    let has_more = rows.len() > entries.len();
    let prefix = verified_prefix(state, &caller.party);
    let verified = prefix.as_ref().map_or(0, |log| log.head().seq);
    let tip = stored_tip(state, prefix.as_ref());
    let recorded_ok = state.recorded_head.is_none_or(|head| {
        prefix.as_ref().is_some_and(|log| log.entry(head.seq).is_some_and(|e| *e.hash() == head.hash))
    });
    let (verification, stored, broken_at) = match (&prefix, tip > verified || !recorded_ok) {
        (_, true) => (ControlVerification::Broken, head_proto(state.recorded_head), Some(verified + 1)),
        (Some(log), false) => (ControlVerification::Verified, head_proto(Some(log.head())), None),
        (None, false) => (ControlVerification::Unlinked, head_proto(None), None),
    };
    let upload_from = match (broken_at, caller.claim) {
        (Some(at), _) => Some(at),
        (None, Some(claim)) if claim.seq > stored.seq => Some(stored.seq + 1),
        _ => None,
    };
    Ok(GetAgentControlLogResponse {
        entries,
        documents: state.documents.clone(),
        stored_head: Some(stored),
        has_more,
        upload_from,
        verification: verification as i32,
        problem_seq: broken_at,
        server_time: server_time(state),
    })
}

/// The rpc's append rules (plan_append), partial repairs included; the skew floor is the
/// stored tip and the heads the rpc recorded, never a token claim.
fn append(state: &mut FakeState, caller: Caller, request: AppendControlEntriesRequest) -> Result<AppendControlEntriesResponse, Status> {
    state.appends.push(request.entries.iter().map(|e| e.seq).collect());
    state.append_documents.push(request.documents.len());
    state.claims.push(("AppendControlEntries".into(), caller.claim));
    if let Some(status) = &state.always_fail_append {
        return Err(status.clone());
    }
    if let Some(status) = state.append_failure.take() {
        return Err(status);
    }
    if request.entries.len() > MAX_APPEND_ENTRIES || request.documents.len() > MAX_APPEND_DOCUMENTS {
        return Err(refused(4084));
    }
    if request.entries.is_empty() && request.documents.is_empty() {
        return Err(refused(4084));
    }
    let mut uploaded: Vec<SignedEntry> = Vec::new();
    for row in &request.entries {
        let entry = decode_row(row).ok_or_else(|| refused(4084))?;
        if entry.seq() != row.seq || entry.seq() == 0 || uploaded.last().is_some_and(|p| p.seq() + 1 != entry.seq()) {
            return Err(refused(4084));
        }
        uploaded.push(entry);
    }
    let offered: BTreeMap<String, Vec<u8>> =
        request.documents.iter().map(|d| (sha256_hex(&d.document), d.document.clone())).collect();
    let prefix = verified_prefix(state, &caller.party);
    if let Some(log) = &prefix {
        store_documents(state, log.entries().iter(), &offered);
    }
    let verified = prefix.as_ref().map_or(0, |log| log.head().seq);
    let tip = stored_tip(state, prefix.as_ref());
    let failing = tip > verified;
    let upload_tip = uploaded.last().map(SignedEntry::seq);
    if uploaded.first().is_some_and(|first| first.seq() > verified + 1) {
        return Err(refused(4091));
    }
    let (mut new, mut already) = (Vec::new(), 0u32);
    for entry in uploaded {
        if let Some(stored) = prefix.as_ref().and_then(|log| log.entry(entry.seq())) {
            if stored.hash() == entry.hash() {
                already += 1;
                continue;
            }
            let evidence = ForkEvidence::new(stored.clone(), entry);
            return match evidence.verify(Network::Devnet, &caller.party) {
                Ok(()) => {
                    state.forks.push(evidence);
                    Err(refused(4089))
                }
                Err(_) => Err(refused(4085)),
            };
        }
        let over = state.entries.iter().find(|row| row.seq == entry.seq()).and_then(decode_row);
        if let Some(stored) = over.filter(|stored| stored.hash() != entry.hash()) {
            let evidence = ForkEvidence::new(stored, entry.clone());
            if evidence.verify(Network::Devnet, &caller.party).is_ok() {
                state.forks.push(evidence);
                return Err(refused(4089));
            }
        }
        new.push(entry);
    }
    match (new.last(), upload_tip) {
        (None, Some(tip)) if tip < verified => return Err(refused(4091)),
        (None, _) if failing => return Err(refused(4091)),
        (None, _) if verified == 0 => return Err(refused(4084)),
        _ => {}
    }
    if usize::try_from(verified).unwrap() + new.len() > MAX_LOG_ENTRIES {
        return Err(refused(4092));
    }
    let log = match (&prefix, new.is_empty()) {
        (Some(base), true) => Ok(base.clone()),
        (Some(base), false) => agent_control::verify_extension(base, &new),
        (None, _) => agent_control::verify_log(Network::Devnet, &caller.party, &new),
    }
    .map_err(|e| refused(if e.code_name().contains("signature") { 4085 } else { 4084 }))?;
    // A partial repair counts the stored rows above it that chain on: the claim is that tip.
    let log = match new.last() {
        Some(last) if last.seq() < tip => extend_with_stored(log, &state.entries),
        _ => log,
    };
    let result_head = log.head();
    if caller.claim != Some(result_head) {
        return Err(refused(4091));
    }
    let vouched = [state.recorded_head, state.accepted_head]
        .into_iter()
        .flatten()
        .filter(|head| log.entry(head.seq).is_none_or(|entry| *entry.hash() == head.hash))
        .map(|head| head.seq)
        .fold(tip, u64::max);
    let now = server_now(state);
    if new.iter().any(|entry| entry.seq() > vouched && entry.created_at().unix_secs().abs_diff(now) > 120) {
        return Err(refused(4090));
    }
    let missing = log.entries().iter().filter_map(accepted_hash).any(|hash| {
        !offered.contains_key(&hash) && !state.documents.iter().any(|d| d.policy_hash == hash)
    });
    if missing {
        return Err(refused(4084));
    }
    let (mut appended, mut repaired) = (0u32, 0u32);
    for entry in &new {
        let row = ControlLogEntry {
            seq: entry.seq(),
            canonical: entry.bytes().to_vec(),
            signature: entry.signature().to_vec(),
            entry_hash: entry.hash().to_string(),
        };
        match state.entries.binary_search_by_key(&entry.seq(), |row| row.seq) {
            Ok(at) if state.entries[at] == row => already += 1,
            Ok(at) => {
                state.entries[at] = row;
                repaired += 1;
            }
            Err(at) => {
                state.entries.insert(at, row);
                if entry.seq() > tip { appended += 1 } else { repaired += 1 }
            }
        }
    }
    store_documents(state, log.entries().iter(), &offered);
    let contradicted = state.recorded_head.filter(|head| log.entry(head.seq).is_some_and(|e| *e.hash() != head.hash));
    if contradicted.is_none() {
        raise(&mut state.recorded_head, result_head);
    }
    raise(&mut state.accepted_head, result_head);
    raise(&mut state.highest_claim, result_head);
    Ok(AppendControlEntriesResponse {
        head: Some(head_proto(Some(result_head))),
        appended,
        already_stored: already,
        repaired,
        server_time: server_time(state),
    })
}

/// AckPolicy as the rpc checks it: a policy of the caller, and a v1 refusal receipt with the
/// policy as its subject, signed by the caller's key.
fn ack_policy(state: &mut FakeState, caller: Caller, request: AckPolicyRequest) -> Result<AckPolicyResponse, Status> {
    use agent_control::{Hash32, Receipt, ReceiptSubject};
    note_claim(state, "AckPolicy", caller.claim);
    let hash = Hash32::parse(&request.policy_hash).map_err(|_| refused(4084))?;
    match state.policies.get(&request.policy_hash) {
        Some(owner) if *owner == caller.party => {}
        Some(_) => return Err(refused(4082)),
        None => return Err(refused(4094)),
    }
    if request.receipt.is_empty() && request.signature.is_empty() {
        return Ok(AckPolicyResponse { status: ControlPolicyStatus::Pending as i32 });
    }
    let signature: [u8; 64] = request.signature.as_slice().try_into().map_err(|_| refused(4084))?;
    let decoded = Receipt::decode(&request.receipt).map_err(|_| refused(4084))?;
    if decoded.network != Network::Devnet || decoded.subject != ReceiptSubject::Policy(hash) || decoded.subject_hash != hash {
        return Err(refused(4084));
    }
    Receipt::verify(&request.receipt, &signature, &caller.party).map_err(|_| refused(4085))?;
    state.acks.push((request.policy_hash, request.receipt));
    Ok(AckPolicyResponse { status: ControlPolicyStatus::Refused as i32 })
}

/// PostReceipt as the rpc checks it: the caller's signed receipt for a command of its own,
/// whose subject hash is the command's, v2 only for a swap.
fn post_receipt(state: &mut FakeState, caller: Caller, request: PostReceiptRequest) -> Result<PostReceiptResponse, Status> {
    use agent_control::{AnyReceipt, Command, CommandOp, ReceiptSubject};
    note_claim(state, "PostReceipt", caller.claim);
    let signature: [u8; 64] = request.signature.as_slice().try_into().map_err(|_| refused(4084))?;
    let verified = AnyReceipt::verify(&request.receipt, &signature, &caller.party).map_err(|_| refused(4085))?;
    let receipt = verified.receipt();
    let ReceiptSubject::Command(id) = &receipt.subject else { return Err(refused(4084)) };
    let id = id.as_str().to_string();
    let (owner, bytes) = state.commands.get(&id).ok_or_else(|| refused(4094))?;
    if *owner != caller.party {
        return Err(refused(4082));
    }
    if receipt.subject_hash != super::rpc::document_hash(bytes) {
        return Err(refused(4084));
    }
    let swap = Command::decode(bytes).is_ok_and(|c| matches!(c.op, CommandOp::Swap { .. }));
    if matches!(verified, AnyReceipt::V2(_)) != swap {
        return Err(refused(4084));
    }
    state.receipts.push((id.clone(), request.receipt));
    Ok(PostReceiptResponse { command_id: id, ..Default::default() })
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
        let shared = self.0.clone();
        let path = req.uri().path().to_string();
        let date = {
            let mut state = shared.lock().unwrap();
            state.calls.push(path.clone());
            (!state.omit_date).then(|| http_date(state.clock_offset_secs))
        };
        Box::pin(async move {
            let method = path.rsplit('/').next().unwrap_or_default().to_string();
            Ok(match method.as_str() {
                "GetControlLog" => {
                    let shared = shared.clone();
                    let handler: Handler<GetAgentControlLogRequest, GetAgentControlLogResponse> =
                        Arc::new(move |caller, q| log_page(&mut shared.lock().unwrap(), caller, q));
                    tonic::server::Grpc::new(Prost::<GetAgentControlLogResponse, GetAgentControlLogRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "AppendControlEntries" => {
                    let shared = shared.clone();
                    let handler: Handler<AppendControlEntriesRequest, AppendControlEntriesResponse> =
                        Arc::new(move |caller, q| append(&mut shared.lock().unwrap(), caller, q));
                    tonic::server::Grpc::new(Prost::<AppendControlEntriesResponse, AppendControlEntriesRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "GetControlDescriptor" => {
                    let shared = shared.clone();
                    let handler: Handler<GetControlDescriptorRequest, GetControlDescriptorResponse> = Arc::new(move |caller, q| {
                        let mut state = shared.lock().unwrap();
                        note_claim(&mut state, "GetControlDescriptor", caller.claim);
                        let mut response = state.descriptors.get(&q.commitment).cloned().ok_or_else(|| refused(4094))?;
                        response.server_time = server_time(&state);
                        Ok(response)
                    });
                    tonic::server::Grpc::new(Prost::<GetControlDescriptorResponse, GetControlDescriptorRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "ListPendingControl" => {
                    let shared = shared.clone();
                    let handler: Handler<ListPendingControlRequest, ListPendingControlResponse> = Arc::new(move |caller, _| {
                        let mut state = shared.lock().unwrap();
                        note_claim(&mut state, "ListPendingControl", caller.claim);
                        let stored = state.entries.last().map(|e| ControlHead { seq: e.seq, hash: e.entry_hash.clone() });
                        Ok(ListPendingControlResponse {
                            stored_head: stored,
                            server_time: server_time(&state),
                            ..Default::default()
                        })
                    });
                    tonic::server::Grpc::new(Prost::<ListPendingControlResponse, ListPendingControlRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "AckPolicy" => {
                    let shared = shared.clone();
                    let handler: Handler<AckPolicyRequest, AckPolicyResponse> =
                        Arc::new(move |caller, q| ack_policy(&mut shared.lock().unwrap(), caller, q));
                    tonic::server::Grpc::new(Prost::<AckPolicyResponse, AckPolicyRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "PostReceipt" => {
                    let shared = shared.clone();
                    let handler: Handler<PostReceiptRequest, PostReceiptResponse> =
                        Arc::new(move |caller, q| post_receipt(&mut shared.lock().unwrap(), caller, q));
                    tonic::server::Grpc::new(Prost::<PostReceiptResponse, PostReceiptRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "ListNodePackages" => {
                    use orderbook_proto::orderbook::{ListNodePackagesRequest, ListNodePackagesResponse};
                    let shared = shared.clone();
                    let handler: Handler<ListNodePackagesRequest, ListNodePackagesResponse> = Arc::new(move |_, q| {
                        let mut state = shared.lock().unwrap();
                        state.node_package_calls.push(q.include_removed);
                        let mut response =
                            state.node_packages.clone().ok_or_else(|| Status::unavailable("agent control unavailable"))?;
                        response.as_of = server_time(&state);
                        Ok(response)
                    });
                    tonic::server::Grpc::new(Prost::<ListNodePackagesResponse, ListNodePackagesRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
                "GetAgentConfig" => {
                    use orderbook_proto::ledger::{GetAgentConfigRequest, GetAgentConfigResponse};
                    let env = shared.lock().unwrap().agent_env.clone();
                    let reply = env
                        .map(|env| GetAgentConfigResponse { env, ..Default::default() })
                        .ok_or_else(|| Status::unimplemented("not in the fake"));
                    tonic::server::Grpc::new(Prost::<GetAgentConfigResponse, GetAgentConfigRequest>::default())
                        .unary(Open { reply }, req)
                        .await
                }
                _ => {
                    let handler: Handler<ListPendingControlRequest, ListPendingControlResponse> =
                        Arc::new(|_, _| Err(Status::unimplemented("not in the fake")));
                    tonic::server::Grpc::new(Prost::<ListPendingControlResponse, ListPendingControlRequest>::default())
                        .unary(Unary { handler, date }, req)
                        .await
                }
            })
        })
    }
}

pub(crate) struct FakeRpc {
    pub url: String,
    pub state: Shared,
    server: tokio::task::JoinHandle<()>,
}

impl FakeRpc {
    pub(crate) async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Shared::default();
        let service = FakeService(state.clone());
        let dapp = FakeDapp(service.clone());
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let server = tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(service)
                .add_service(dapp)
                .serve_with_incoming(incoming)
                .await;
        });
        Self { url, state, server }
    }

    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    /// Stores signed entries as if uploaded earlier; the heads row records the last one.
    pub(crate) fn hold(&self, entries: &[SignedEntry]) {
        let mut state = self.state();
        for e in entries {
            let row = ControlLogEntry {
                seq: e.seq(),
                canonical: e.bytes().to_vec(),
                signature: e.signature().to_vec(),
                entry_hash: e.hash().to_string(),
            };
            match state.entries.binary_search_by_key(&e.seq(), |row| row.seq) {
                Ok(at) => state.entries[at] = row,
                Err(at) => state.entries.insert(at, row),
            }
            raise(&mut state.recorded_head, HeadRef { seq: e.seq(), hash: *e.hash() });
        }
    }

    pub(crate) fn seqs(&self) -> Vec<u64> {
        self.state().entries.iter().map(|e| e.seq).collect()
    }
}

impl Drop for FakeRpc {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// ---- an agent with a temporary control directory and env file ----

pub(crate) struct TestAgent {
    pub seed: [u8; 32],
    pub config: BaseConfig,
    pub party: PartyId,
    pub base: PathBuf,
    pub env_file: PathBuf,
    pub chain: TestChain,
}

fn temp_dir(tag: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let dir = std::env::temp_dir().join(format!("control-cli-{tag}-{}", hex::encode(bytes)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl TestAgent {
    pub(crate) fn new(seed: [u8; 32], rpc: &FakeRpc, tag: &str) -> Self {
        let party = party_for(seed, "agent-1").unwrap();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.private_key = Secret::seal(&mut { seed }).unwrap();
        config.party_id = party.to_string();
        config.canton_chain = Some("devnet".into());
        config.orderbook_grpc_url = rpc.url.clone();
        let base = temp_dir(tag);
        let env_file = base.join(".env");
        std::fs::write(&env_file, format!("PARTY_AGENT={party}\n")).unwrap();
        let chain = TestChain::new(seed, &party, Network::Devnet).unwrap();
        Self { seed, config, party, base, env_file, chain }
    }

    pub(crate) fn store(&self) -> ControlStore {
        ControlStore::open(&self.base, Network::Devnet, &self.party)
    }

    pub(crate) fn marker(&self) -> Option<String> {
        crate::read_env_value(&self.env_file, MARKER_ENV)
    }

    pub(crate) fn env(&self) -> ControlEnv {
        ControlEnv {
            control_dir: Some(self.base.as_os_str().to_owned()),
            home: None,
            chain: Some("devnet".into()),
            marker: self.marker(),
            party: Some(self.config.party_id.clone()),
            agent_dir: Some(self.base.clone()),
        }
    }

    pub(crate) async fn run(&self, command: ControlCommands, answers: &[&str]) -> (anyhow::Result<()>, String) {
        let prompter = Scripted::new(answers.iter().copied());
        let mut out = Vec::new();
        let result = super::run_control_with(
            self.config.clone(),
            &self.env(),
            Some(self.env_file.clone()),
            command,
            &prompter,
            &mut out,
        )
        .await;
        (result, String::from_utf8_lossy(&out).into_owned())
    }

    pub(crate) async fn context(&self) -> ControlContext {
        ControlContext::open(self.config.clone(), &self.env(), Some(self.env_file.clone())).await.unwrap()
    }

    /// Stores the first `n` entries of a fresh chain locally, on the fake, and in the marker.
    pub(crate) async fn linked(&mut self, rpc: &FakeRpc, n: usize) {
        self.chain = TestChain::build(self.seed, &self.party, Network::Devnet, n).unwrap();
        let store = self.store();
        let lock = store.lock().await.unwrap();
        let head = store.append(&lock, &self.chain.entries, &[]).unwrap();
        drop(lock);
        rpc.hold(&self.chain.entries);
        let marker = ControlMarker { network: Network::Devnet, head };
        crate::upsert_env_value(&self.env_file, MARKER_ENV, &marker.to_string()).unwrap();
    }

    pub(crate) fn head(&self) -> Option<HeadRef> {
        self.store().snapshot().unwrap().map(|s| s.log.head())
    }

    /// Stores `chain` (with `documents`) locally, on the fake without the documents, and in the marker.
    pub(crate) async fn linked_chain(&mut self, rpc: &FakeRpc, chain: TestChain, documents: &[Vec<u8>]) {
        self.chain = chain;
        let store = self.store();
        let lock = store.lock().await.unwrap();
        let head = store.append(&lock, &self.chain.entries, documents).unwrap();
        drop(lock);
        rpc.hold(&self.chain.entries);
        let marker = ControlMarker { network: Network::Devnet, head };
        crate::upsert_env_value(&self.env_file, MARKER_ENV, &marker.to_string()).unwrap();
    }
}

/// A descriptor for `party` (None: an invite), stored on the fake under its commitment; returns the code.
pub(crate) fn offer(rpc: &FakeRpc, purpose: LinkPurpose, party: Option<&PartyId>, controller_seeds: &[u8]) -> (String, Vec<u8>) {
    let controllers = controller_seeds
        .iter()
        .map(|seed| {
            let key = ed25519_dalek::SigningKey::from_bytes(&[*seed; 32]).verifying_key().to_bytes();
            ControllerDescriptor::new(
                ControllerKey::Ed25519(key),
                Label::parse(&format!("Key {seed}")).unwrap(),
                CapabilitySet::of(&[Capability::Policy, Capability::Direct, Capability::Read]),
            )
            .unwrap()
        })
        .collect();
    offer_of(rpc, purpose, party, controllers, controller_seeds.first().copied().unwrap_or(1))
}

/// [`offer`] for any controllers; `nonce` fills the descriptor's nonce.
pub(crate) fn offer_of(
    rpc: &FakeRpc,
    purpose: LinkPurpose,
    party: Option<&PartyId>,
    controllers: Vec<ControllerDescriptor>,
    nonce: u8,
) -> (String, Vec<u8>) {
    let expires = UtcTime::from_unix_secs(agent_logic::clock::now_secs_i64() + 3600).unwrap();
    let descriptor = LinkDescriptor::new(
        Network::Devnet,
        purpose,
        OwnerAccount::new(42).unwrap(),
        party.cloned(),
        Nonce16([nonce; 16]),
        expires,
        controllers,
    )
    .unwrap();
    let bytes = descriptor.encode().unwrap();
    let commitment = descriptor.commitment().unwrap();
    rpc.state().descriptors.insert(
        commitment.to_string(),
        GetControlDescriptorResponse {
            purpose: match (purpose, party) {
                (_, None) => ControlLinkPurpose::Invite as i32,
                (LinkPurpose::Link, _) => ControlLinkPurpose::Link as i32,
                (LinkPurpose::AddController, _) => ControlLinkPurpose::Add as i32,
            },
            descriptor: bytes.clone(),
            invite_code: party.is_none().then(|| "AGT-A4DQOBYHA4DQOBYHA4DQOBYHA4".to_string()),
            agent_party: party.map(ToString::to_string),
            owner_account: "42".into(),
            status: ControlLinkStatus::Open as i32,
            expires_at: None,
            server_time: None,
        },
    );
    (agent_control::format_link_code(&commitment), bytes)
}

pub(crate) fn link_cmd(code: &str) -> ControlCommands {
    ControlCommands::Link { code: code.to_string(), yes: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_control::{EntryKind, Hash32, PolicyDocument};
    use agent_logic::control_store::PendingStatus;

    fn add_ed25519(seed: u8, yes: bool) -> ControlCommands {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key().to_bytes();
        ControlCommands::Add {
            code: None,
            ed25519: Some(hex::encode(key)),
            label: Some(format!("Backup {seed}")),
            capabilities: Some("read,policy".into()),
            yes,
        }
    }

    #[tokio::test]
    async fn link_refuses_a_commitment_mismatch_then_signs_and_uploads_seq_1() {
        let rpc = FakeRpc::start().await;
        let agent = TestAgent::new([0xa1; 32], &rpc, "link");
        let (code, bytes) = offer(&rpc, LinkPurpose::Link, Some(&agent.party), &[0x31, 0x32]);
        let (_, other) = offer(&rpc, LinkPurpose::Link, Some(&agent.party), &[0x33]);
        let commitment = agent_control::parse_link_code(&code).unwrap().to_string();
        rpc.state().descriptors.get_mut(&commitment).unwrap().descriptor = other;
        let (result, _) = agent.run(link_cmd(&code), &["link"]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("commitment_mismatch"), "{err}");
        assert!(agent.head().is_none() && rpc.state().appends.is_empty() && agent.marker().is_none());
        assert!(!agent.store().exists(), "nothing signed");

        rpc.state().descriptors.get_mut(&commitment).unwrap().descriptor = bytes;
        let (result, out) = agent.run(link_cmd(&code), &["link"]).await;
        result.unwrap();
        let head = agent.head().unwrap();
        assert_eq!(head.seq, 1);
        assert_eq!(rpc.seqs(), vec![1]);
        let marker = ControlMarker::parse(&agent.marker().unwrap()).unwrap();
        assert_eq!(marker, ControlMarker { network: Network::Devnet, head });
        for seed in [0x31u8, 0x32] {
            let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key().to_bytes();
            let id = agent_control::controller_id_for(agent_control::ControllerType::Ed25519, &key);
            assert!(out.contains(&id.fingerprint()), "{out}");
        }
        assert!(out.contains("Owner account: 42") && out.contains("take effect at about"), "{out}");
        assert!(out.contains("(1 h after the signature)"), "{out}");
        assert!(!out.contains("under Create agent"), "only an invite has a row there: {out}");
        // After the rpc accepts, the times printed are the signed entry's own.
        let stored = agent.store().snapshot().unwrap().unwrap();
        let signed = stored.log.entries()[0].entry();
        assert_eq!(signed.effective_at(), signed.created_at().checked_add_secs(3600).unwrap());
        let times = format!("Signed at {}; it takes effect at {}.", signed.created_at(), signed.effective_at());
        let accepted = out.find("Accepted by the rpc").unwrap();
        assert!(out.find(&times).is_some_and(|at| at > accepted), "{out}");
        let fingerprint = super::super::link::party_fingerprint(&agent.party);
        assert!(out.contains(&format!("Linked at seq 1. Party fingerprint (the Agent page shows the same): {fingerprint}")), "{out}");
        // The code was used: linking again is refused, now that controllers are pending.
        let (result, _) = agent.run(link_cmd(&code), &["link"]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("in effect or pending"));
    }

    #[tokio::test]
    async fn a_link_shows_and_pins_the_platform_parties_the_env_lacks_and_never_replaces_one() {
        let party = |hint: &str, digit: &str| format!("{hint}::1220{}", digit.repeat(64));
        let (fee, traffic, other) = (party("fee", "a"), party("traffic", "b"), party("other", "c"));
        let rpc = FakeRpc::start().await;
        rpc.state().agent_env = Some(format!("NODE_NAME=n1\nPARTY_FEE={other}\nPARTY_PREPAID_TRAFFIC={traffic}\n"));
        let served = super::super::pins::fetch(&rpc.url).await.unwrap();
        assert_eq!(served, vec![("PARTY_FEE", other.clone()), ("PARTY_PREPAID_TRAFFIC", traffic.clone())]);
        // Not confirmed: nothing is pinned.
        let refused = TestAgent::new([0x16; 32], &rpc, "link-pins-no");
        let (code, _) = offer(&rpc, LinkPurpose::Link, Some(&refused.party), &[0x3a]);
        let (result, out) = refused.run(link_cmd(&code), &["no"]).await;
        assert!(result.is_err());
        assert!(out.contains(&format!("PARTY_PREPAID_TRAFFIC={traffic}")), "{out}");
        assert_eq!(crate::read_env_value(&refused.env_file, "PARTY_PREPAID_TRAFFIC"), None);
        // PARTY_FEE is pinned locally already: only the traffic party is shown and written.
        let agent = TestAgent::new([0x17; 32], &rpc, "link-pins");
        crate::upsert_env_value(&agent.env_file, "PARTY_FEE", &fee).unwrap();
        let (code, _) = offer(&rpc, LinkPurpose::Link, Some(&agent.party), &[0x3b]);
        let (result, out) = agent.run(link_cmd(&code), &["link"]).await;
        result.unwrap();
        let shown = out.find(&format!("  PARTY_PREPAID_TRAFFIC={traffic}\n")).unwrap();
        assert!(out.find("Type 'link' to continue").is_some_and(|at| at > shown), "{out}");
        assert!(!out.contains(&other), "{out}");
        let pinned = out.find(&format!("  PARTY_PREPAID_TRAFFIC={traffic} (pinned now)\n")).unwrap();
        let picked = out.find("A running agent picks up the pins written now on its next poller round.");
        assert!(picked.is_some_and(|at| at > pinned), "{out}");
        assert_eq!(crate::read_env_value(&agent.env_file, "PARTY_FEE"), Some(fee));
        assert_eq!(crate::read_env_value(&agent.env_file, "PARTY_PREPAID_TRAFFIC"), Some(traffic));
        assert_eq!(agent.head().map(|h| h.seq), Some(1));
    }

    #[tokio::test]
    async fn link_is_refused_while_anything_is_pending() {
        let rpc = FakeRpc::start().await;
        // A refused pending entry blocks every new signature, link included.
        let mut agent = TestAgent::new([0xa2; 32], &rpc, "pending-link");
        let ctx = agent.context().await;
        let pending = agent.chain.link_pending(0x21).unwrap();
        {
            let lock = ctx.store.lock().await.unwrap();
            ctx.store.sign_entry(&lock, &ctx.signer, &pending, 1).unwrap();
            ctx.store.refuse_pending(&lock, Some(4090), 2).unwrap();
        }
        let (code, _) = offer(&rpc, LinkPurpose::Link, Some(&agent.party), &[0x34]);
        let (result, _) = agent.run(link_cmd(&code), &["link"]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("discard-pending") && err.contains("4090"), "{err}");
        assert!(rpc.state().appends.is_empty());

        // A controller whose add is still pending blocks a link too.
        let rpc = FakeRpc::start().await;
        let mut linked = TestAgent::new([0xa3; 32], &rpc, "pending-add");
        linked.linked(&rpc, 1).await;
        let (code, _) = offer(&rpc, LinkPurpose::Link, Some(&linked.party), &[0x35]);
        let (result, _) = linked.run(link_cmd(&code), &["link"]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("in effect or pending") && err.contains("control add"), "{err}");
        assert_eq!(rpc.seqs(), vec![1]);
    }

    #[tokio::test]
    async fn removing_the_last_holder_needs_force_and_the_typed_freeze() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa4; 32], &rpc, "remove");
        agent.linked(&rpc, 1).await;
        let remove = |force: bool, yes: bool| ControlCommands::Remove { controller: "Test key".into(), force, yes };
        let (result, _) = agent.run(remove(false, false), &["remove"]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("no controller would hold policy,direct,read") && err.contains("remaining controllers: none"), "{err}");
        let (result, _) = agent.run(remove(true, true), &[]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("--yes is not accepted with --force"));
        let (result, out) = agent.run(remove(true, false), &["freeze"]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("not confirmed"), "{out}");
        assert!(out.contains("WARNING") && out.contains("nobody can change or pause the policy"), "{out}");
        assert_eq!(rpc.seqs(), vec![1], "nothing signed");
        let phrase = agent_control::force_confirmation_phrase(&agent.chain.state);
        assert_eq!(phrase, "freeze agent-1 default policy");
        let (result, _) = agent.run(remove(true, false), &[phrase.as_str()]).await;
        result.unwrap();
        assert_eq!(rpc.seqs(), vec![1, 2]);
        let stored = agent.store().snapshot().unwrap().unwrap();
        assert_eq!(stored.log.entries()[1].kind(), EntryKind::RemoveController);
    }

    #[tokio::test]
    async fn adding_a_direct_key_no_policy_scopes_warns_before_the_confirmation() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0x18; 32], &rpc, "add-unscoped");
        agent.linked(&rpc, 1).await;
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x75; 32]).verifying_key().to_bytes();
        let add = ControlCommands::Add {
            code: None,
            ed25519: Some(hex::encode(key)),
            label: Some("Bot".into()),
            capabilities: Some("direct,read".into()),
            yes: false,
        };
        let (result, out) = agent.run(add, &["add"]).await;
        result.unwrap();
        let warned = out.find("Warning: no policy scopes").unwrap();
        assert!(out.find("Type 'add' to continue").is_some_and(|at| at > warned), "{out}");
        assert!(out.contains("swap, transfer_cc, transfer_token"), "{out}");
        // Without direct there is nothing to scope.
        let (result, out) = agent.run(add_ed25519(0x76, true), &[]).await;
        result.unwrap();
        assert!(!out.contains("Warning: no policy scopes"), "{out}");
    }

    #[tokio::test]
    async fn add_by_key_and_capability_changes_follow_the_minimum_rule() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa5; 32], &rpc, "add");
        agent.linked(&rpc, 1).await;
        let (result, out) = agent.run(add_ed25519(0x51, false), &["add"]).await;
        result.unwrap();
        assert!(out.contains("Fingerprint") || out.contains("fingerprint"), "{out}");
        assert_eq!(rpc.seqs(), vec![1, 2]);
        // With a second policy holder, narrowing the first needs no force.
        let change = ControlCommands::Capabilities { controller: "Test key".into(), capabilities: "direct,read".into(), force: false, yes: true };
        let (result, out) = agent.run(change, &[]).await;
        result.unwrap();
        assert!(out.contains("takes effect at about") && out.contains("(1 h after the signature)"), "{out}");
        assert_eq!(rpc.seqs(), vec![1, 2, 3]);
        let stored = agent.store().snapshot().unwrap().unwrap();
        let signed = stored.log.entries()[2].entry();
        assert_eq!(signed.effective_at(), signed.created_at().checked_add_secs(3600).unwrap());
        assert!(out.contains(&format!("Signed at {}; it takes effect at {}.", signed.created_at(), signed.effective_at())), "{out}");
    }

    #[tokio::test]
    async fn show_offline_reads_only_the_local_copy() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa6; 32], &rpc, "show");
        agent.linked(&rpc, 2).await;
        rpc.state().calls.clear();
        let (result, out) = agent.run(ControlCommands::Show { json: false, offline: true }, &[]).await;
        result.unwrap();
        assert!(rpc.state().calls.is_empty(), "no rpc call");
        let fingerprint = super::super::link::party_fingerprint(&agent.party);
        assert_eq!(fingerprint.len(), 24);
        for needle in ["linked, head seq 2", "Test key", "Extra key", "Policy in force: the default rules", "PARTY_FEE=unset", "silvana.link", "--offline", "devnet:2:", fingerprint.as_str()] {
            assert!(out.contains(needle), "{needle}: {out}");
        }
        assert!(out.contains("swap_max_deviation_bps=100 swap_reference_max_age_secs=300;"), "{out}");
        let store_line = format!("Store:        {}", agent.base.join("control").display());
        assert!(out.contains(&store_line), "the store path is shown: {out}");
        let (result, out) = agent.run(ControlCommands::Show { json: true, offline: true }, &[]).await;
        result.unwrap();
        let view: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(view["storeDir"], agent.base.join("control").display().to_string());
        assert_eq!(view["settings"]["swapReferenceMaxAgeSecs"], 300);
        assert_eq!(view["partyFingerprint"], fingerprint.as_str());
        assert_eq!(view["head"]["seq"], 2);
        assert_eq!(view["controllers"].as_array().unwrap().len(), 2);
        assert_eq!(view["rpIds"][0], "silvana.link");
        // Online, the rpc's copy is compared.
        let (result, out) = agent.run(ControlCommands::Show { json: false, offline: false }, &[]).await;
        result.unwrap();
        assert!(out.contains("equal (seq 2)"), "{out}");
    }

    #[tokio::test]
    async fn show_names_the_traffic_fee_pin_in_text_and_json() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa9; 32], &rpc, "show-traffic-fee");
        let traffic_fee = format!("traffic-fee::1220{}", "d".repeat(64));
        agent.config.party_traffic_fee = Some(traffic_fee.clone());
        let (result, out) = agent.run(ControlCommands::Show { json: false, offline: true }, &[]).await;
        result.unwrap();
        assert!(out.contains(&format!("PARTY_PREPAID_TRAFFIC=unset PARTY_TRAFFIC_FEE={traffic_fee}")), "{out}");
        let (result, out) = agent.run(ControlCommands::Show { json: true, offline: true }, &[]).await;
        result.unwrap();
        let view: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(view["platformParties"]["PARTY_TRAFFIC_FEE"], traffic_fee.as_str());
        assert_eq!(view["platformParties"]["PARTY_FEE"], serde_json::Value::Null);
        assert_eq!(view["settings"]["prepaidTrafficTopupTokens"], serde_json::json!(["CC"]));
        // Unset while a token top-up is listed, the text says what is refused.
        agent.config.party_traffic_fee = None;
        agent.config.prepaid_traffic_topup_tokens = vec!["TUSDT".into(), "CC".into()];
        let (result, out) = agent.run(ControlCommands::Show { json: false, offline: true }, &[]).await;
        result.unwrap();
        assert!(out.contains("PARTY_TRAFFIC_FEE=unset"), "{out}");
        assert!(out.contains("top-ups paid in a token other than CC are refused"), "{out}");
    }

    #[tokio::test]
    async fn sync_uploads_a_missing_tail_and_a_remote_ahead_refuses() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa7; 32], &rpc, "sync");
        agent.linked(&rpc, 3).await;
        rpc.state().entries.truncate(1);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("uploading 2 entries"), "{out}");
        assert_eq!(rpc.seqs(), vec![1, 2, 3]);
        assert_eq!(rpc.state().appends, vec![vec![2, 3]]);

        // The rpc ahead of the local copy: refused, both heads named, nothing signed.
        let rpc = FakeRpc::start().await;
        let ahead = TestAgent::new([0xa8; 32], &rpc, "ahead");
        let chain = TestChain::build(ahead.seed, &ahead.party, Network::Devnet, 2).unwrap();
        let store = ahead.store();
        let lock = store.lock().await.unwrap();
        let head = store.append(&lock, &chain.entries[..1], &[]).unwrap();
        drop(lock);
        crate::upsert_env_value(&ahead.env_file, MARKER_ENV, &ControlMarker { network: Network::Devnet, head }.to_string()).unwrap();
        rpc.hold(&chain.entries);
        let (result, _) = ahead.run(add_ed25519(0x52, true), &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("ahead of the local copy") && err.contains("rpc seq 2") && err.contains("local seq 1"), "{err}");
        assert!(rpc.state().appends.is_empty());
        let (result, _) = ahead.run(ControlCommands::Sync, &[]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("ahead"));
    }

    #[tokio::test]
    async fn a_pending_entry_is_accepted_refused_and_never_re_signed() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xa9; 32], &rpc, "pending");
        agent.linked(&rpc, 1).await;
        let ctx = agent.context().await;
        // The rpc stored the entry but the reply was lost: sync accepts it.
        let pending = agent.chain.add_pending(0x61).unwrap();
        let signed = {
            let lock = ctx.store.lock().await.unwrap();
            ctx.store.sign_entry(&lock, &ctx.signer, &pending, 1).unwrap()
        };
        rpc.hold(std::slice::from_ref(&signed));
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("holds the pending entry at seq 2"), "{out}");
        assert_eq!(agent.head().map(|h| h.seq), Some(2));
        assert!(agent.marker().unwrap().starts_with("devnet:2:"));
        agent.chain.push(&pending).unwrap();

        // The rpc never got it: sync uploads it; a definite refusal is kept.
        let pending = agent.chain.add_pending(0x62).unwrap();
        {
            let lock = ctx.store.lock().await.unwrap();
            ctx.store.sign_entry(&lock, &ctx.signer, &pending, 1).unwrap();
        }
        rpc.state().append_failure = Some(Status::failed_precondition("request refused (code 4090)"));
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("code 4090") && err.contains("not signed again"), "{err}");
        let stored = agent.store().snapshot().unwrap().unwrap();
        assert_eq!(stored.pending.as_ref().map(|p| p.status), Some(PendingStatus::Refused));
        let appends = rpc.state().appends.len();
        // No new signature at that seq, by any command.
        for command in [ControlCommands::Sync, add_ed25519(0x63, true)] {
            let (result, _) = agent.run(command, &[]).await;
            assert!(format!("{:#}", result.unwrap_err()).contains("discard-pending"));
        }
        assert_eq!(rpc.state().appends.len(), appends, "the refused entry is not uploaded again");
        // discard-pending releases it after a typed confirmation and keeps a copy.
        let (result, out) = agent.run(ControlCommands::DiscardPending, &["discard"]).await;
        assert!(result.is_err() && out.contains("Warning"), "{out}");
        let (result, _) = agent.run(ControlCommands::DiscardPending, &["discard 3"]).await;
        result.unwrap();
        assert!(agent.store().snapshot().unwrap().unwrap().pending.is_none());
        let evidence = std::fs::read_dir(agent.store().dir().join("evidence")).unwrap().count();
        assert_eq!(evidence, 1);
        let (result, _) = agent.run(add_ed25519(0x64, true), &[]).await;
        result.unwrap();
        assert_eq!(rpc.seqs(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn a_clock_off_by_more_than_a_minute_refuses_before_signing() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xaa; 32], &rpc, "skew");
        agent.linked(&rpc, 1).await;
        rpc.state().clock_offset_secs = 300;
        let (result, _) = agent.run(add_ed25519(0x71, true), &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("behind the rpc's") && err.contains("fix the clock"), "{err}");
        assert!(rpc.state().appends.is_empty());
        assert!(agent.store().snapshot().unwrap().unwrap().pending.is_none(), "nothing signed");
        rpc.state().clock_offset_secs = -30;
        let (result, _) = agent.run(add_ed25519(0x71, true), &[]).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn restore_rebuilds_a_deleted_control_dir_from_the_rpc() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xab; 32], &rpc, "restore");
        agent.linked(&rpc, 2).await;
        let marker = agent.marker().unwrap();
        std::fs::remove_dir_all(agent.store().dir()).unwrap();
        let (result, _) = agent.run(ControlCommands::Show { json: false, offline: true }, &[]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("control restore"));
        let pin = ControlMarker::parse(&marker).unwrap().head;
        let phrase = format!("restore {}:{}", pin.seq, &pin.hash.to_string()[..8]);
        let (result, _) = agent.run(ControlCommands::Restore, &["restore"]).await;
        assert!(result.is_err());
        let (result, out) = agent.run(ControlCommands::Restore, &[phrase.as_str()]).await;
        result.unwrap();
        assert!(out.contains("Restored"), "{out}");
        assert_eq!(agent.head(), Some(pin));
        // A remote log below the marker is refused.
        rpc.state().entries.truncate(1);
        std::fs::remove_dir_all(agent.store().dir()).unwrap();
        let (result, _) = agent.run(ControlCommands::Restore, &[phrase.as_str()]).await;
        assert!(format!("{:#}", result.unwrap_err()).contains("behind the linked marker"));
    }

    fn policy_for(party: &PartyId, network: Network) -> Vec<u8> {
        let now = agent_logic::clock::now_secs_i64();
        PolicyDocument {
            network,
            agent_party: party.clone(),
            version: 1,
            prev_policy_hash: Hash32([0; 32]),
            issued_at: UtcTime::from_unix_secs(now).unwrap(),
            expires_at: UtcTime::from_unix_secs(now + 3600).unwrap(),
            not_before: None,
            rules: agent_control::DEFAULT_LINKED_RULES,
        }
        .encode()
        .unwrap()
    }

    #[tokio::test]
    async fn controller_keygen_and_sign_round_trip() {
        use super::super::controller_tool::{ControllerCommands, KEY_FILE, run_controller_with};
        let dir = temp_dir("controller").join("key");
        let mut out = Vec::new();
        run_controller_with(ControllerCommands::Keygen { dir: dir.clone() }, &Scripted::none(), &mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let key = super::super::controller_tool::load_key(&dir.join(KEY_FILE)).unwrap();
        let public = key.verifying_key().to_bytes();
        let fingerprint = agent_control::controller_id_for(agent_control::ControllerType::Ed25519, &public).fingerprint();
        assert!(text.contains(&hex::encode(public)) && text.contains(&fingerprint), "{text}");
        assert!(run_controller_with(ControllerCommands::Keygen { dir: dir.clone() }, &Scripted::none(), &mut Vec::new()).await.is_err());

        let party = party_for([0xac; 32], "agent-1").unwrap();
        let policy = policy_for(&party, Network::Devnet);
        let sign = |network: &str, payload: &[u8]| ControllerCommands::Sign {
            key: dir.join(KEY_FILE),
            payload: Some(base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload)),
            file: None,
            network: network.into(),
            current_policy: None,
        };
        let mut out = Vec::new();
        run_controller_with(sign("devnet", &policy), &Scripted::new(["sign"]), &mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let hex_sig = text.lines().last().unwrap();
        assert_eq!(hex_sig.len(), 128);
        assert_eq!(hex_sig, hex_sig.to_ascii_lowercase());
        let signature: [u8; 64] = hex::decode(hex_sig).unwrap().try_into().unwrap();
        agent_control::verify_ed25519_strict(&public, &policy, &signature).unwrap();
        assert!(text.contains("Policy v1") && text.contains("Type 'sign'"), "{text}");
        // Another network, a foreign scheme or no confirmation: no signature.
        let err = run_controller_with(sign("mainnet", &policy), &Scripted::new(["sign"]), &mut Vec::new()).await.unwrap_err();
        assert!(err.to_string().contains("not mainnet"), "{err}");
        let chain = TestChain::build([0xac; 32], &party, Network::Devnet, 1).unwrap();
        let err = run_controller_with(sign("devnet", chain.entries[0].bytes()), &Scripted::new(["sign"]), &mut Vec::new()).await.unwrap_err();
        assert!(err.to_string().contains("only policy, command or read-session"), "{err}");
        // A sign-in challenge is refused too, even one that names this key.
        let challenge = agent_control::ControllerChallenge {
            network: Network::Devnet,
            rpc: agent_control::RpId::parse("orderbook-devnet.silvana.dev").unwrap(),
            controller_id: agent_control::controller_id_for(agent_control::ControllerType::Ed25519, &public),
            agent_party: party.clone(),
            nonce: agent_control::Nonce16([0x5c; 16]),
            issued_at: agent_control::UtcTime::parse("2026-11-23T10:00:00Z").unwrap(),
            expires_at: agent_control::UtcTime::parse("2026-11-23T10:01:00Z").unwrap(),
        }
        .encode()
        .unwrap();
        let err = run_controller_with(sign("devnet", &challenge), &Scripted::new(["sign"]), &mut Vec::new()).await.unwrap_err();
        assert!(err.to_string().contains("only policy, command or read-session"), "{err}");
        assert!(run_controller_with(sign("devnet", &policy), &Scripted::new(["yes"]), &mut Vec::new()).await.is_err());
        assert!(run_controller_with(sign("devnet", &policy), &Scripted::none(), &mut Vec::new()).await.is_err());
    }

    fn appends_of(state: &FakeState) -> Vec<Option<HeadRef>> {
        state.claims.iter().filter(|(m, _)| m == "AppendControlEntries").map(|(_, c)| *c).collect()
    }

    #[tokio::test]
    async fn an_append_token_claims_the_head_after_the_upload_and_others_the_accepted_one() {
        let rpc = FakeRpc::start().await;
        let agent = TestAgent::new([0xad; 32], &rpc, "append-claim");
        let (code, _) = offer(&rpc, LinkPurpose::Link, Some(&agent.party), &[0x36]);
        let (result, _) = agent.run(link_cmd(&code), &["link"]).await;
        result.unwrap();
        let first = agent.head().unwrap();
        {
            let state = rpc.state();
            assert_eq!(appends_of(&state), vec![Some(first)], "the first link claims its own entry");
            let others: Vec<_> = state.claims.iter().filter(|(m, _)| m != "AppendControlEntries").collect();
            assert!(!others.is_empty() && others.iter().all(|(_, c)| c.is_none()), "{:?}", state.claims);
            assert_eq!(state.highest_claim, Some(first));
        }
        rpc.state().claims.clear();
        let (result, _) = agent.run(add_ed25519(0x53, true), &[]).await;
        result.unwrap();
        let second = agent.head().unwrap();
        let state = rpc.state();
        assert_eq!(appends_of(&state), vec![Some(second)]);
        for (method, claim) in &state.claims {
            let expected = if method == "AppendControlEntries" { second } else { first };
            assert_eq!(*claim, Some(expected), "{method}");
        }
        assert_eq!(state.highest_claim, Some(second));
    }

    #[tokio::test]
    async fn the_fake_holds_the_rpcs_append_rules() {
        use agent_logic::control_store::ControlLock;
        use super::super::rpc::{AppendTokenSource, ControlRpc};
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xae; 32], &rpc, "append-rules");
        agent.linked(&rpc, 1).await;
        let ctx = agent.context().await;
        let now = agent_logic::clock::now_secs_i64();
        agent.chain.created_at = UtcTime::from_unix_secs(now - 60).unwrap();
        let pending = agent.chain.add_pending(0x54).unwrap();
        let lock = ctx.store.lock().await.unwrap();
        let signed = ctx.store.sign_entry(&lock, &ctx.signer, &pending, now).unwrap();
        let entries = std::slice::from_ref(&signed);

        // A token claiming the accepted head (the old client) is refused: claim != result head.
        let (config, token_config) = (ctx.config.clone(), ctx.config.clone());
        let accepted: AppendTokenSource =
            Arc::new(move |_: &ControlStore, _: &ControlLock, _: &HeadRef| config.control_jwt().map(|(t, _)| t));
        let channel = tonic::transport::Channel::from_shared(rpc.url.clone()).unwrap().connect_lazy();
        let old = ControlRpc::new(channel, Arc::new(move || token_config.control_jwt().map(|(t, _)| t)), accepted);
        let err = old.append(&ctx.store, &lock, entries, &[]).await.unwrap_err();
        assert_eq!(err.definite_code(), Some(4091), "{err}");
        assert_eq!(rpc.seqs(), vec![1]);

        // The head after the upload is accepted, and so is the same upload again.
        let client = ControlRpc::connect(&ctx.config).await.unwrap();
        let head = HeadRef { seq: 2, hash: *signed.hash() };
        assert_eq!(client.append(&ctx.store, &lock, entries, &[]).await.unwrap(), head);
        assert_eq!(client.append(&ctx.store, &lock, entries, &[]).await.unwrap(), head);
        assert_eq!(rpc.seqs(), vec![1, 2]);
        // Re-sending only an older prefix is behind the stored head.
        let err = client.append(&ctx.store, &lock, &agent.chain.entries[..1], &[]).await.unwrap_err();
        assert_eq!(err.definite_code(), Some(4091), "{err}");
        // A head the local store does not hold gets no token, and nothing is sent.
        let mut fork = TestChain::new(agent.seed, &agent.party, Network::Devnet).unwrap();
        fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
        let link = fork.link_pending(0x22).unwrap();
        fork.push(&link).unwrap();
        let sent = rpc.state().appends.len();
        let err = client.append(&ctx.store, &lock, &fork.entries, &[]).await.unwrap_err();
        assert!(err.to_string().contains("does not hold") && err.definite_code().is_none(), "{err}");
        assert_eq!(rpc.state().appends.len(), sent);
        drop(lock);

        // A backdated entry above the stored tip and the recorded heads: 4090.
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb0; 32], &rpc, "append-skew");
        agent.linked(&rpc, 1).await;
        let ctx = agent.context().await;
        let old = agent.chain.add_pending(0x55).unwrap();
        {
            let lock = ctx.store.lock().await.unwrap();
            ctx.store.sign_entry(&lock, &ctx.signer, &old, 1).unwrap();
        }
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("code 4090"), "{err}");
        assert_eq!(rpc.seqs(), vec![1]);
        let stored = agent.store().snapshot().unwrap().unwrap();
        assert_eq!(stored.pending.map(|p| (p.status, p.refused_code)), Some((PendingStatus::Refused, Some(4090))));
    }

    #[tokio::test]
    async fn the_rpc_clock_comes_from_server_time_then_the_date_header() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xaf; 32], &rpc, "server-time");
        agent.linked(&rpc, 1).await;
        rpc.state().clock_offset_secs = 300;
        // Without the fake's date header the transport still sends one at the real time,
        // so the first case also shows server_time winning over a conflicting header.
        for (omit_server_time, omit_date) in [(false, true), (true, false)] {
            {
                let mut state = rpc.state();
                state.omit_server_time = omit_server_time;
                state.omit_date = omit_date;
            }
            let (result, _) = agent.run(add_ed25519(0x73, true), &[]).await;
            let err = format!("{:#}", result.unwrap_err());
            assert!(err.contains("behind the rpc's") && err.contains("fix the clock"), "{omit_server_time} {omit_date}: {err}");
        }
        assert!(rpc.state().appends.is_empty(), "nothing signed");
        {
            let mut state = rpc.state();
            state.clock_offset_secs = 20;
            state.omit_server_time = false;
        }
        let (result, _) = agent.run(add_ed25519(0x73, true), &[]).await;
        result.unwrap();
        assert_eq!(rpc.seqs(), vec![1, 2]);
    }

    #[tokio::test]
    async fn an_invite_link_at_onboarding_prints_the_party_fingerprint() {
        let rpc = FakeRpc::start().await;
        let agent = TestAgent::new([0xb1; 32], &rpc, "invite");
        let (code, _) = offer(&rpc, LinkPurpose::Link, None, &[0x37]);
        let commitment = agent_control::parse_link_code(&code).unwrap();
        let ctx = agent.context().await;
        let client = super::super::ControlRpc::connect(&ctx.config).await.unwrap();
        let mut out = Vec::new();
        let invite = "AGT-A4DQOBYHA4DQOBYHA4DQOBYHA4";
        let head = super::super::link::run_invite_link(&ctx, &client, &Scripted::new(["link"]), &mut out, invite, commitment)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.seq, 1);
        assert_eq!(appends_of(&rpc.state()), vec![Some(head)]);
        let out = String::from_utf8_lossy(&out).into_owned();
        let fingerprint = super::super::link::party_fingerprint(&agent.party);
        assert!(out.contains(&format!("confirm this agent by checking its party fingerprint: {fingerprint}")), "{out}");
    }

    #[tokio::test]
    async fn an_invite_link_says_what_to_check_on_the_agent_page_before_the_prompt() {
        use agent_control::{BackupPolicy, Origin, RpId, RpPolicy, WebAuthnKey};
        let rpc = FakeRpc::start().await;
        let agent = TestAgent::new([0xf3; 32], &rpc, "invite-check");
        let spki = hex::decode(
            "3059301306072a8648ce3d020106082a8648ce3d03010703420004261efbd3550cf068ef013ed7366ba32f\
             5d6fe557b4b2abce8ade58cba168a55e1788a0b29a56a6abec4084c0c96bd3dcbca6b507f35dbea9e985708479d8bdc9",
        )
        .unwrap();
        let rp_id = RpId::parse(RpPolicy::DEVNET_RP_ID).unwrap();
        let origins = vec![Origin::parse("https://book-devnet.silvana.link").unwrap()];
        let key = WebAuthnKey::new(&spki, vec![7u8; 16], rp_id, origins, BackupPolicy::Any).unwrap();
        let capabilities = CapabilitySet::of(&[Capability::Policy, Capability::Direct, Capability::Read]);
        let passkey =
            ControllerDescriptor::new(ControllerKey::WebAuthn(key), Label::parse("Mac passkey").unwrap(), capabilities).unwrap();
        let passkey_fingerprint = passkey.id().fingerprint();
        let (code, _) = offer_of(&rpc, LinkPurpose::Link, None, vec![passkey], 0x38);
        let commitment = agent_control::parse_link_code(&code).unwrap();
        let ctx = agent.context().await;
        let client = super::super::ControlRpc::connect(&ctx.config).await.unwrap();
        let mut out = Vec::new();
        let invite = "AGT-A4DQOBYHA4DQOBYHA4DQOBYHA4";
        super::super::link::run_invite_link(&ctx, &client, &Scripted::new(["link"]), &mut out, invite, commitment)
            .await
            .unwrap()
            .unwrap();
        let out = String::from_utf8_lossy(&out).into_owned();
        let fingerprint = super::super::link::party_fingerprint(&agent.party);
        let check = format!(
            "On the Agent page, under Create agent, check: party fingerprint {fingerprint}, owner account 42, \
             your passkey {passkey_fingerprint}.\nYou have 15 min to type 'link'."
        );
        let shown = out.find(&check).unwrap_or_else(|| panic!("{out}"));
        assert!(out.find("Type 'link' to continue").is_some_and(|prompt| prompt > shown), "{out}");
        assert!(out.contains("a re-run of `onboard` does not link"), "{out}");
    }

    /// The fake's rows equal the local log, byte for byte.
    fn assert_rows_match(rpc: &FakeRpc, entries: &[SignedEntry]) {
        let state = rpc.state();
        let rows: Vec<(u64, Vec<u8>, Vec<u8>)> =
            state.entries.iter().map(|r| (r.seq, r.canonical.clone(), r.signature.clone())).collect();
        let local: Vec<(u64, Vec<u8>, Vec<u8>)> =
            entries.iter().map(|e| (e.seq(), e.bytes().to_vec(), e.signature().to_vec())).collect();
        assert_eq!(rows, local);
    }

    #[tokio::test]
    async fn drill_an_edited_row_is_repaired_by_sync() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb2; 32], &rpc, "edited-row");
        agent.linked(&rpc, 4).await;
        {
            let mut state = rpc.state();
            let row = state.entries.iter_mut().find(|r| r.seq == 2).unwrap();
            let at = row.canonical.len() - 2;
            row.canonical[at] ^= 1;
        }
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 2") && out.contains("uploading 1 entries"), "{out}");
        assert_eq!(rpc.state().appends, vec![vec![2, 3, 4]], "one request through the stored tip");
        assert_rows_match(&rpc, &agent.chain.entries);
        // The logs are equal now: a second sync sends nothing.
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("In sync: local seq 4"), "{out}");
        assert_eq!(rpc.state().appends.len(), 1);
    }

    #[tokio::test]
    async fn drill_a_deleted_row_is_filled_by_sync() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb3; 32], &rpc, "deleted-row");
        agent.linked(&rpc, 4).await;
        rpc.state().entries.retain(|r| r.seq != 2);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 2"), "{out}");
        assert_eq!(rpc.state().appends, vec![vec![2, 3, 4]]);
        assert_rows_match(&rpc, &agent.chain.entries);
        // A control change works again on the repaired log.
        let (result, _) = agent.run(add_ed25519(0x56, true), &[]).await;
        result.unwrap();
        assert_eq!(rpc.seqs(), vec![1, 2, 3, 4, 5]);
    }

    fn row_of(entry: &SignedEntry) -> ControlLogEntry {
        ControlLogEntry {
            seq: entry.seq(),
            canonical: entry.bytes().to_vec(),
            signature: entry.signature().to_vec(),
            entry_hash: entry.hash().to_string(),
        }
    }

    #[tokio::test]
    async fn drill_a_validly_signed_different_row_is_refused_as_a_fork() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb4; 32], &rpc, "forked-row");
        agent.linked(&rpc, 3).await;
        // Another entry at seq 2, signed by the agent key.
        let mut fork = TestChain::build(agent.seed, &agent.party, Network::Devnet, 1).unwrap();
        fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
        let other = fork.add_pending(0x77).unwrap();
        let other = fork.push(&other).unwrap();
        assert_ne!(other.hash(), agent.chain.entries[1].hash());
        rpc.state().entries[1] = row_of(&other);
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("fork at seq 2") && err.contains("Nothing is signed"), "{err}");
        assert!(rpc.state().appends.is_empty(), "nothing uploaded");

        // Past a broken row: seq 2 edited, seq 3 replaced by another validly signed entry.
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb5; 32], &rpc, "forked-past-broken");
        agent.linked(&rpc, 3).await;
        let mut fork = TestChain::build(agent.seed, &agent.party, Network::Devnet, 2).unwrap();
        assert_eq!(fork.entries, agent.chain.entries[..2]);
        fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
        let third = fork.add_pending(0x78).unwrap();
        let third = fork.push(&third).unwrap();
        {
            let mut state = rpc.state();
            state.entries[1].canonical.push(b'x');
            state.entries[2] = row_of(&third);
        }
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("fork at seq 3"), "{err}");
        assert!(rpc.state().appends.is_empty());
    }

    #[tokio::test]
    async fn drill_a_span_over_64_entries_is_repaired_in_chunks() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb6; 32], &rpc, "long-repair");
        let chain = TestChain::churn(agent.seed, &agent.party, Network::Devnet, 40).unwrap();
        assert_eq!(chain.entries.len(), 81);
        agent.linked_chain(&rpc, chain, &[]).await;
        rpc.state().entries.retain(|r| !(3..=72).contains(&r.seq));
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 3") && out.contains("uploading 70 entries"), "{out}");
        let appends = rpc.state().appends.clone();
        assert_eq!(appends, vec![(3..=66).collect::<Vec<u64>>(), (67..=72).collect()]);
        // The last chunk claims the tip the rpc verifies again, past the rows it held.
        let claims = vec![Some(head_at(&agent.chain, 66)), Some(head_at(&agent.chain, 81))];
        assert_eq!(appends_of(&rpc.state()), claims);
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    fn head_at(chain: &TestChain, seq: u64) -> HeadRef {
        let entry = &chain.entries[usize::try_from(seq - 1).unwrap()];
        HeadRef { seq, hash: *entry.hash() }
    }

    /// A log of 200 entries: a link, 99 additions each withdrawn, and one more addition.
    /// Its controller seeds are 0x21, 0x30 and 0x80-0xe2, never the agent's.
    fn two_hundred(agent: &TestAgent) -> TestChain {
        let mut chain = TestChain::churn(agent.seed, &agent.party, Network::Devnet, 99).unwrap();
        let add = chain.add_pending(0x30).unwrap();
        chain.push(&add).unwrap();
        assert_eq!(chain.entries.len(), 200);
        chain
    }

    /// Edits the fake's row at each of `seqs`, so it no longer verifies.
    fn edit_rows(rpc: &FakeRpc, seqs: impl IntoIterator<Item = u64>) {
        let mut state = rpc.state();
        for seq in seqs {
            let row = state.entries.iter_mut().find(|r| r.seq == seq).unwrap();
            let at = row.canonical.len() - 2;
            row.canonical[at] ^= 1;
        }
    }

    #[tokio::test]
    async fn drill_one_edited_row_of_200_is_repaired_by_one_upload_claiming_the_tip() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xe8; 32], &rpc, "edited-of-200");
        let chain = two_hundred(&agent);
        agent.linked_chain(&rpc, chain, &[]).await;
        edit_rows(&rpc, [50]);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 50") && out.contains("uploading 1 entries"), "{out}");
        {
            let state = rpc.state();
            assert_eq!(state.appends, vec![vec![50]], "only the edited row is sent");
            assert_eq!(appends_of(&state), vec![Some(head_at(&agent.chain, 200))], "it claims the re-verified tip");
        }
        assert_rows_match(&rpc, &agent.chain.entries);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("In sync: local seq 200"), "{out}");
        assert_eq!(rpc.state().appends.len(), 1);
    }

    #[tokio::test]
    async fn drill_two_bad_regions_each_claim_the_tip_up_to_the_next_one() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xe9; 32], &rpc, "two-regions");
        let chain = two_hundred(&agent);
        agent.linked_chain(&rpc, chain, &[]).await;
        edit_rows(&rpc, 40..=42);
        rpc.state().entries.retain(|r| !(150..=151).contains(&r.seq));
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 40") && out.contains("uploading 5 entries"), "{out}");
        {
            let state = rpc.state();
            assert_eq!(state.appends, vec![vec![40, 41, 42], vec![150, 151]]);
            let claims = vec![Some(head_at(&agent.chain, 149)), Some(head_at(&agent.chain, 200))];
            assert_eq!(appends_of(&state), claims);
        }
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn drill_a_bad_region_over_64_entries_before_good_rows_claims_each_chunks_tip() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xea; 32], &rpc, "long-region");
        let chain = two_hundred(&agent);
        agent.linked_chain(&rpc, chain, &[]).await;
        edit_rows(&rpc, 50..=129);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 50") && out.contains("uploading 80 entries"), "{out}");
        {
            let state = rpc.state();
            assert_eq!(state.appends, vec![(50..=113).collect::<Vec<u64>>(), (114..=129).collect()]);
            let claims = vec![Some(head_at(&agent.chain, 113)), Some(head_at(&agent.chain, 200))];
            assert_eq!(appends_of(&state), claims, "a chunk before a bad row ends at itself");
        }
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn an_append_may_claim_a_held_head_above_its_last_entry() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xbf; 32], &rpc, "append-claiming");
        agent.linked(&rpc, 5).await;
        edit_rows(&rpc, [2]);
        let ctx = agent.context().await;
        let lock = ctx.store.lock().await.unwrap();
        let client = super::super::ControlRpc::connect(&ctx.config).await.unwrap();
        let second = &agent.chain.entries[1..2];
        // Claiming the upload's own last entry is refused: the rpc verifies seqs 3-5 again.
        let err = client.append(&ctx.store, &lock, second, &[]).await.unwrap_err();
        assert_eq!(err.definite_code(), Some(4091), "{err}");
        // A claim below the upload, or one the store does not hold, is refused before sending.
        let sent = rpc.state().appends.len();
        let below = head_at(&agent.chain, 1);
        let err = client.append_claiming(&ctx.store, &lock, second, &[], &below).await.unwrap_err();
        assert!(err.to_string().contains("below the upload") && !err.is_transient(), "{err}");
        let unheld = HeadRef { seq: 5, hash: below.hash };
        let err = client.append_claiming(&ctx.store, &lock, second, &[], &unheld).await.unwrap_err();
        assert!(err.to_string().contains("does not hold") && !err.is_transient(), "{err}");
        assert_eq!(rpc.state().appends.len(), sent);
        let tip = head_at(&agent.chain, 5);
        assert_eq!(client.append_claiming(&ctx.store, &lock, second, &[], &tip).await.unwrap(), tip);
        drop(lock);
        assert_eq!(appends_of(&rpc.state()).last(), Some(&Some(tip)));
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn a_partial_repair_carries_the_documents_of_the_rows_verified_again() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xc0; 32], &rpc, "reverified-documents");
        let mut chain = TestChain::churn(agent.seed, &agent.party, Network::Devnet, 40).unwrap();
        let mut documents = Vec::new();
        for _ in 0..3 {
            let accept = chain.accept_pending(0x21).unwrap();
            documents.push(accept.policy_document().unwrap().to_vec());
            chain.push(&accept).unwrap();
        }
        assert_eq!(chain.entries.len(), 84);
        // The fake holds every row but no document; then seq 5 is edited.
        agent.linked_chain(&rpc, chain, &documents).await;
        edit_rows(&rpc, [5]);
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 5"), "{out}");
        {
            let state = rpc.state();
            assert_eq!(state.appends, vec![vec![5]]);
            assert_eq!(state.append_documents, vec![3], "the documents of seqs 82-84 go with it");
            assert_eq!(appends_of(&state), vec![Some(head_at(&agent.chain, 84))]);
            assert_eq!(state.documents.len(), 3);
        }
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn a_repair_too_large_for_one_request_goes_run_by_run() {
        // Two separate bad rows far apart: past one request, each run is sent alone.
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xbb; 32], &rpc, "runs");
        let chain = TestChain::churn(agent.seed, &agent.party, Network::Devnet, 40).unwrap();
        agent.linked_chain(&rpc, chain, &[]).await;
        {
            let mut state = rpc.state();
            state.entries.retain(|r| r.seq != 5);
            let row = state.entries.iter_mut().find(|r| r.seq == 70).unwrap();
            row.signature[0] ^= 1;
        }
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("broken at seq 5") && out.contains("uploading 2 entries"), "{out}");
        assert_eq!(rpc.state().appends, vec![vec![5], vec![70]]);
        let claims = vec![Some(head_at(&agent.chain, 69)), Some(head_at(&agent.chain, 81))];
        assert_eq!(appends_of(&rpc.state()), claims, "each run claims the tip up to the next bad row");
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    /// A chain of a link and `n` policy acceptances, with their documents.
    fn accepting(agent: &TestAgent, n: usize) -> (TestChain, Vec<Vec<u8>>) {
        let mut chain = TestChain::build(agent.seed, &agent.party, Network::Devnet, 1).unwrap();
        let mut documents = Vec::new();
        for _ in 0..n {
            let accept = chain.accept_pending(0x21).unwrap();
            documents.push(accept.policy_document().unwrap().to_vec());
            chain.push(&accept).unwrap();
        }
        (chain, documents)
    }

    #[tokio::test]
    async fn documents_go_in_batches_of_at_most_sixteen_on_every_upload_path() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb7; 32], &rpc, "documents");
        let (chain, documents) = accepting(&agent, 20);
        agent.linked_chain(&rpc, chain, &documents).await;
        // The rows are equal but the rpc lost every document.
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("In sync: local seq 21"), "{out}");
        {
            let state = rpc.state();
            assert_eq!(state.append_documents, vec![16, 4], "two batches, never above the limit");
            assert_eq!(state.appends, vec![vec![21], vec![21]], "each with the stored head");
            assert_eq!(state.documents.len(), 20);
        }
        // The rows of every acceptance are lost with their documents: re-upload chunks
        // carry at most 16 documents each.
        {
            let mut state = rpc.state();
            state.entries.retain(|r| r.seq == 1);
            state.documents.clear();
            state.appends.clear();
            state.append_documents.clear();
        }
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("uploading 20 entries"), "{out}");
        let state = rpc.state();
        assert_eq!(state.appends, vec![(2..=17).collect::<Vec<u64>>(), (18..=21).collect()]);
        assert_eq!(state.append_documents, vec![16, 4]);
        assert!(state.append_documents.iter().all(|n| *n <= MAX_APPEND_DOCUMENTS));
        assert_eq!(state.documents.len(), 20);
        drop(state);
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn a_rate_limited_upload_is_retried_and_otherwise_stays_pending() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb8; 32], &rpc, "rate-limited");
        agent.linked(&rpc, 1).await;
        rpc.state().append_failure = Some(refused(4083));
        let (result, _) = agent.run(add_ed25519(0x57, true), &[]).await;
        result.unwrap();
        assert_eq!(rpc.state().appends, vec![vec![2], vec![2]], "retried once the limit passed");
        assert_eq!(agent.head().map(|h| h.seq), Some(2));
        // Rate-limited throughout: not refused, kept pending, uploaded by a later sync.
        rpc.state().always_fail_append = Some(refused(4083));
        let (result, _) = agent.run(add_ed25519(0x58, true), &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("stays pending") && !err.contains("not signed again"), "{err}");
        let pending = agent.store().snapshot().unwrap().unwrap().pending.unwrap();
        assert_eq!((pending.status, pending.refused_code), (PendingStatus::Uploading, None));
        assert_eq!(rpc.state().appends.len(), 2 + 4, "one attempt and three retries");
        rpc.state().always_fail_append = None;
        let (result, out) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert!(out.contains("In sync: local seq 3"), "{out}");
        assert!(agent.store().snapshot().unwrap().unwrap().pending.is_none());
    }

    #[tokio::test]
    async fn drill_a_full_database_restore_needs_the_operator_to_reseed_the_heads_row() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xb9; 32], &rpc, "full-restore");
        agent.linked(&rpc, 3).await;
        // A token claim never vouches: with the tail lost and the heads row restored to
        // seq 1, the old entries are refused although every token claims seq 3.
        {
            let mut state = rpc.state();
            state.entries.truncate(1);
            state.recorded_head = Some(HeadRef { seq: 1, hash: *agent.chain.entries[0].hash() });
        }
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("code 4090"), "{err}");
        assert_eq!(rpc.state().highest_claim.map(|c| c.seq), Some(3));
        // The rows and the heads row restored together, the rpc restarted: the operator re-seeds.
        {
            let mut state = rpc.state();
            state.entries.clear();
            state.documents.clear();
            state.recorded_head = None;
            state.accepted_head = None;
        }
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("code 4090") && err.contains("re-seed agent_control_heads") && err.contains("claimed_seq/claimed_hash"),
            "{err}"
        );
        assert!(err.contains(&agent.head().unwrap().hash.to_string()), "names the head to confirm: {err}");
        assert!(rpc.seqs().is_empty());
        rpc.state().recorded_head = agent.head();
        let (result, _) = agent.run(ControlCommands::Sync, &[]).await;
        result.unwrap();
        assert_rows_match(&rpc, &agent.chain.entries);
    }

    #[tokio::test]
    async fn the_fake_answers_like_the_rpc() {
        let rpc = FakeRpc::start().await;
        let mut agent = TestAgent::new([0xba; 32], &rpc, "fake-rules");
        agent.linked(&rpc, 3).await;
        // An unknown descriptor is 4094, refused as permission denied.
        let unknown = agent_control::format_link_code(&agent_control::Commitment([7u8; 16]));
        let (result, _) = agent.run(link_cmd(&unknown), &["link"]).await;
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("PermissionDenied") && err.contains("code 4094"), "{err}");
        // A fork stores its evidence.
        let mut fork = TestChain::build(agent.seed, &agent.party, Network::Devnet, 1).unwrap();
        fork.created_at = fork.created_at.checked_add_secs(3600).unwrap();
        let other = fork.add_pending(0x79).unwrap();
        let other = fork.push(&other).unwrap();
        let ctx = agent.context().await;
        let lock = ctx.store.lock().await.unwrap();
        let (seed, party) = (agent.seed, agent.party.clone());
        let forged: super::super::rpc::AppendTokenSource =
            Arc::new(move |_: &ControlStore, _: &agent_logic::control_store::ControlLock, head: &HeadRef| {
                forged_head_token(seed, &party, head)
            });
        let forger = super::super::ControlRpc::new(
            tonic::transport::Channel::from_shared(rpc.url.clone()).unwrap().connect_lazy(),
            Arc::new(|| Ok(String::new())),
            forged,
        );
        let err = forger.append(&ctx.store, &lock, std::slice::from_ref(&other), &[]).await.unwrap_err();
        assert_eq!(err.definite_code(), Some(4089), "{err}");
        {
            let state = rpc.state();
            assert_eq!(state.forks.len(), 1);
            assert_eq!(state.forks[0].seq(), 2);
            assert_eq!(state.forks[0].remote(), &other);
        }
        drop(lock);
        // A Broken log is served with its verdict and where to upload from.
        rpc.state().entries.retain(|r| r.seq != 2);
        let token = ctx.config.control_jwt().unwrap().0;
        let channel = tonic::transport::Channel::from_shared(rpc.url.clone()).unwrap().connect_lazy();
        let mut client = orderbook_proto::orderbook::agent_control_agent_service_client::AgentControlAgentServiceClient::new(channel);
        let mut request = tonic::Request::new(GetAgentControlLogRequest { from_seq: 1, limit: 256 });
        request.metadata_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
        let page = client.get_control_log(request).await.unwrap().into_inner();
        assert_eq!(page.verification, ControlVerification::Broken as i32);
        assert_eq!((page.upload_from, page.problem_seq), (Some(2), Some(2)));
        assert_eq!(page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
        // The agent reads past the gap.
        let remote = ctx.signer.public_key();
        let log = super::super::ControlRpc::connect(&ctx.config).await.unwrap().fetch_log().await.unwrap();
        assert_eq!((log.gap_at, log.stored_tip, log.broken, log.upload_from), (Some(2), 3, true, Some(2)));
        assert_eq!(log.beyond_gap.len(), 1);
        let view = super::super::verify_remote(Network::Devnet, &agent.party, &remote, &log).unwrap();
        assert_eq!((view.head().map(|h| h.seq), view.broken_at), (Some(1), Some(2)));
        // A token for another service is refused.
        let mut request = tonic::Request::new(GetAgentControlLogRequest { from_seq: 1, limit: 1 });
        let plain = ctx.config.jwt().unwrap();
        request.metadata_mut().insert("authorization", format!("Bearer {plain}").parse().unwrap());
        assert_eq!(client.get_control_log(request).await.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    /// A control token of the agent key claiming `head`, minted without the store's checks.
    fn forged_head_token(seed: [u8; 32], party: &PartyId, head: &HeadRef) -> anyhow::Result<String> {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use ed25519_dalek::Signer;
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let now = agent_logic::clock::now_secs_i64();
        let header = serde_json::json!({"typ": "JWT", "alg": "EdDSA", "jwk": {"kty": "OKP", "crv": "Ed25519", "x": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes())}});
        let claims = serde_json::json!({"sub": party.as_str(), "iat": now, "exp": now + 60, "role": "agent", "aud": FAKE_AUDIENCE, "ctl_seq": head.seq, "ctl_head": head.hash.to_string()});
        let input = format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let signature = key.sign(input.as_bytes());
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes())))
    }
}
