//! A fake rpc serving both controller services over loopback, and test agents whose logs
//! are really signed, for this crate's tests and for its users' tests.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use agent_control::{
    Capability, CapabilitySet, ControlState, ControllerChallenge, ControllerDescriptor,
    ControllerId, ControllerKey, ControllerType, EntryBuilder, HeadRef, Label, LinkDescriptor,
    LinkPurpose, Network, Nonce16, OwnerAccount, PartyId, PendingEntry, PolicyDocuments,
    ProofEnvelope, ReadSessionRequest, Receipt, ReceiptOutcome, ReceiptSubject, RpId, RpPolicy,
    SignedEntry, Text, Token, UtcTime, Uuid7, VerifiedLog, controller_change_delay_secs,
    controller_id_for, namespace_for_key, sign_agent, verify_challenge_proof, verify_command,
    verify_log, verify_policy_submission,
};
use ed25519_dalek::SigningKey;
use orderbook_proto::orderbook::{
    ControlCapability, ControlCommandStatus, ControlCommandView, ControlHead, ControlLogEntry,
    ControlPolicyDocument, ControlPolicyStatus, ControlPolicyView, ControlReceipt,
    ControlVerification, ControllerAgentRequest, ControllerPageRequest, ControllerSignInRequest,
    ControllerSignInResponse, ControllerView, EndControllerSessionRequest,
    EndControllerSessionResponse, GetAgentActivityResponse, GetAgentHoldingsResponse,
    GetAgentLedgerResponse, GetAgentTrafficResponse, GetCommandRequest, GetCommandResponse,
    GetControlLogRequest, GetControlLogResponse, GetControlStateRequest, GetControlStateResponse,
    GetControllerChallengeRequest, GetControllerChallengeResponse, GetPolicyRequest,
    GetPolicyResponse, ListCommandsRequest, ListCommandsResponse, ListNodePackagesRequest,
    ListNodePackagesResponse, SubmitCommandRequest, SubmitCommandResponse, SubmitPolicyRequest,
    SubmitPolicyResponse,
};
use sha2::Digest;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http;
use tonic::metadata::MetadataMap;
use tonic::{Response, Status};

use crate::client::{ControllerClient, ControllerConfig};
use crate::pin::SeenHeads;
use crate::signer::{ControllerSigner, KEY_FILE};

/// A fresh empty directory under the system temp dir.
pub fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut random = [0u8; 6];
    let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let dir = std::env::temp_dir().join(format!(
        "agent-controller-{tag}-{}-{n}-{}",
        std::process::id(),
        hex::encode(random)
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn utc_now() -> Result<UtcTime, String> {
    crate::clock::local_now().map_err(|e| e.to_string())
}

fn shifted(at: UtcTime, secs: i64) -> UtcTime {
    at.checked_add_secs(secs).unwrap_or(at)
}

/// The Ed25519 key of a controller seed byte.
pub fn controller_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// The signer of a controller seed byte.
pub fn controller_signer(seed: u8) -> ControllerSigner {
    ControllerSigner::from_seed(&[seed; 32])
}

/// The controller id of a controller seed byte.
pub fn controller_id(seed: u8) -> ControllerId {
    controller_id_for(
        ControllerType::Ed25519,
        &controller_key(seed).verifying_key().to_bytes(),
    )
}

/// A key file of `seed` in a fresh directory, 0600.
pub fn key_file(tag: &str, seed: u8) -> Result<PathBuf, String> {
    let dir = temp_dir(tag);
    let path = dir.join(KEY_FILE);
    std::fs::write(&path, controller_signer(seed).key_file_text().as_bytes())
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// A read-session request naming `controller`, valid five minutes from now.
pub fn read_session_for(controller: ControllerId) -> Result<ReadSessionRequest, String> {
    const GENERATOR_SPKI: &str = "3059301306072a8648ce3d020106082a8648ce3d030107034200046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
    let spki = hex::decode(GENERATOR_SPKI).map_err(|e| e.to_string())?;
    let key = agent_control::parse_p256_spki(&spki).map_err(|e| e.code_name().to_string())?;
    let party = party_for([5; 32], "agent-1")?;
    let now = utc_now()?;
    Ok(ReadSessionRequest::new(
        Network::Devnet,
        controller,
        key,
        Nonce16([3; 16]),
        now,
        shifted(now, 300),
        vec![party],
    ))
}

/// The party `hint::<namespace of seed's key>`.
pub fn party_for(seed: [u8; 32], hint: &str) -> Result<PartyId, String> {
    let key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    PartyId::parse(&format!("{hint}::{}", namespace_for_key(&key)))
        .map_err(|_| "invalid test party".to_string())
}

/// An agent with a really signed log, linked long enough ago for its controllers to be in effect.
pub struct TestAgent {
    key: SigningKey,
    pub party: PartyId,
    pub network: Network,
    state: ControlState,
    pub entries: Vec<SignedEntry>,
    pub documents: Vec<Vec<u8>>,
    last: UtcTime,
}

fn capabilities(text: &str) -> Result<CapabilitySet, String> {
    CapabilitySet::parse(text).map_err(|_| format!("bad capabilities {text:?}"))
}

impl TestAgent {
    /// A devnet agent linked with Ed25519 controllers of `seeds`, each holding every capability.
    pub fn devnet(seed: [u8; 32], seeds: &[u8]) -> Result<Self, String> {
        let controllers: Vec<(u8, &str)> =
            seeds.iter().map(|s| (*s, "policy,direct,read")).collect();
        Self::linked(seed, Network::Devnet, &controllers)
    }

    /// An agent linked one delay and ten minutes ago with these controllers.
    pub fn linked(
        seed: [u8; 32],
        network: Network,
        controllers: &[(u8, &str)],
    ) -> Result<Self, String> {
        let key = SigningKey::from_bytes(&seed);
        let party = party_for(seed, "agent-1")?;
        let state = ControlState::empty(network, party.clone(), key.verifying_key().to_bytes());
        let delay =
            i64::try_from(controller_change_delay_secs(network)).map_err(|e| e.to_string())?;
        let linked_at = shifted(utc_now()?, delay.saturating_add(600).saturating_neg());
        let mut agent = Self {
            key,
            party,
            network,
            state,
            entries: Vec::new(),
            documents: Vec::new(),
            last: linked_at,
        };
        let mut descriptors = Vec::new();
        for (seed, caps) in controllers {
            let public = controller_key(*seed).verifying_key().to_bytes();
            let label = Label::parse(&format!("Key {seed}")).map_err(|_| "label".to_string())?;
            descriptors.push(
                ControllerDescriptor::new(
                    ControllerKey::Ed25519(public),
                    label,
                    capabilities(caps)?,
                )
                .map_err(|e| format!("controller: {}", e.code_name()))?,
            );
        }
        let descriptor = LinkDescriptor::new(
            network,
            LinkPurpose::Link,
            OwnerAccount::new(42).map_err(|_| "owner".to_string())?,
            Some(agent.party.clone()),
            Nonce16([0x21; 16]),
            shifted(linked_at, 86_400),
            descriptors,
        )
        .map_err(|e| format!("descriptor: {}", e.code_name()))?;
        let bytes = descriptor
            .encode()
            .map_err(|e| format!("encode: {}", e.code_name()))?;
        let commitment = descriptor
            .commitment()
            .map_err(|e| format!("commitment: {}", e.code_name()))?;
        let pending = EntryBuilder::new(&agent.state)
            .link(
                &bytes,
                &commitment,
                &RpPolicy::for_network(network),
                false,
                linked_at,
            )
            .map_err(|e| format!("link: {}", e.code_name()))?;
        agent.push(&pending)?;
        Ok(agent)
    }

    fn at(&mut self, ago_secs: i64) -> Result<UtcTime, String> {
        let wanted = shifted(utc_now()?, ago_secs.saturating_neg());
        self.last = if wanted < self.last {
            self.last
        } else {
            wanted
        };
        Ok(self.last)
    }

    fn push(&mut self, pending: &PendingEntry) -> Result<(), String> {
        let signed = pending
            .finish(sign_agent(&self.key, pending.signable()))
            .map_err(|e| format!("finish: {}", e.code_name()))?;
        if let Some(document) = pending.policy_document() {
            self.documents.push(document.to_vec());
        }
        self.state = pending.state_after().clone();
        self.entries.push(signed);
        Ok(())
    }

    /// Adds the Ed25519 controller of `seed`, signed `ago_secs` ago.
    pub fn add_ed25519(&mut self, seed: u8, caps: &str, ago_secs: i64) -> Result<(), String> {
        let public = controller_key(seed).verifying_key().to_bytes();
        let label = Label::parse(&format!("Key {seed}")).map_err(|_| "label".to_string())?;
        let at = self.at(ago_secs)?;
        let pending = EntryBuilder::new(&self.state)
            .add_ed25519(public, label, capabilities(caps)?, at)
            .map_err(|e| format!("add: {}", e.code_name()))?;
        self.push(&pending)
    }

    /// Removes the controller of `seed`, signed `ago_secs` ago.
    pub fn remove(&mut self, seed: u8, ago_secs: i64) -> Result<(), String> {
        let at = self.at(ago_secs)?;
        let pending = EntryBuilder::new(&self.state)
            .remove_controller(&controller_id(seed), true, at)
            .map_err(|e| format!("remove: {}", e.code_name()))?;
        self.push(&pending)
    }

    /// Accepts a signed policy now.
    pub fn accept(&mut self, document: &[u8], envelope: &ProofEnvelope) -> Result<(), String> {
        let at = self.at(0)?;
        let pending = EntryBuilder::new(&self.state)
            .accept_policy(document, envelope, at)
            .map_err(|e| format!("accept: {}", e.code_name()))?;
        self.push(&pending)
    }

    pub fn state(&self) -> &ControlState {
        &self.state
    }

    pub fn head(&self) -> Option<HeadRef> {
        self.state.head()
    }

    pub fn verified_log(&self) -> Result<VerifiedLog, String> {
        verify_log(self.network, &self.party, &self.entries).map_err(|e| e.code_name().to_string())
    }

    /// A receipt for a command, signed by this agent's key (or by `forger` instead).
    pub fn receipt(
        &self,
        id: &Uuid7,
        command: &[u8],
        outcome: ReceiptOutcome,
        forger: Option<&SigningKey>,
    ) -> Result<ControlReceipt, String> {
        let executed = outcome == ReceiptOutcome::Executed;
        let refused = matches!(outcome, ReceiptOutcome::Refused | ReceiptOutcome::Failed);
        let receipt = Receipt {
            network: self.network,
            agent_party: self.party.clone(),
            agent_key: self.key.verifying_key().to_bytes(),
            subject: ReceiptSubject::Command(id.clone()),
            subject_hash: agent_control::Hash32(sha2::Sha256::digest(command).into()),
            outcome,
            update_id: if executed {
                Token::parse("update-1").ok()
            } else {
                None
            },
            reason_code: refused.then_some(1104),
            code_source: refused.then_some(agent_control::CodeSource::Agent),
            log_seq: self.head().map_or(0, |h| h.seq),
            issued_at: utc_now()?,
        };
        let bytes = receipt
            .encode()
            .map_err(|e| format!("receipt: {}", e.code_name()))?;
        let signable = receipt
            .signable()
            .map_err(|e| format!("receipt: {}", e.code_name()))?;
        let signature = sign_agent(forger.unwrap_or(&self.key), &signable);
        Ok(ControlReceipt {
            canonical: bytes,
            signature: signature.to_vec(),
            outcome: match outcome {
                ReceiptOutcome::Executed => 1,
                ReceiptOutcome::Refused => 2,
                _ => 3,
            },
            ..Default::default()
        })
    }
}

// ---- a prost codec for hand-written services ----

struct Prost<E, D>(PhantomData<fn() -> (E, D)>);

impl<E, D> Default for Prost<E, D> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

struct ProstEncoder<E>(PhantomData<fn() -> E>);
struct ProstDecoder<D>(PhantomData<fn() -> D>);

impl<E: prost::Message> Encoder for ProstEncoder<E> {
    type Item = E;
    type Error = Status;
    fn encode(&mut self, item: E, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|e| Status::internal(e.to_string()))
    }
}

impl<D: prost::Message + Default> Decoder for ProstDecoder<D> {
    type Item = D;
    type Error = Status;
    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<D>, Status> {
        D::decode(src)
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
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

type Handler<Q, R> = Arc<dyn Fn(&MetadataMap, Q) -> Result<R, Status> + Send + Sync>;

struct Unary<Q, R> {
    handler: Handler<Q, R>,
    date: Option<String>,
}

impl<Q: Send + 'static, R: Send + 'static> tonic::server::UnaryService<Q> for Unary<Q, R> {
    type Response = R;
    type Future = tonic::codegen::BoxFuture<Response<R>, Status>;
    fn call(&mut self, request: tonic::Request<Q>) -> Self::Future {
        let (metadata, _, message) = request.into_parts();
        let result = (self.handler)(&metadata, message);
        let date: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>> =
            self.date.as_ref().and_then(|d| d.parse().ok());
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

// ---- the fake rpc ----

/// An edit of the next challenges, to test the signer's checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChallengeEdit {
    Rpc(String),
    Party(PartyId),
    Network(Network),
    Controller(ControllerId),
    /// `issued_at` this many seconds before the fake's clock.
    IssuedAgo(i64),
    /// Bytes that are not a challenge.
    Garbage(Vec<u8>),
}

struct Minted {
    key: [u8; 32],
    deadline: i64,
}

struct Session {
    party: PartyId,
    controller: ControllerId,
}

/// What the fake holds and how it answers.
pub struct FakeState {
    pub network: Network,
    /// The host challenges name.
    pub rpc_host: String,
    /// The rows GetControllerLog serves; a test may withhold or edit any of them.
    pub rows: Vec<ControlLogEntry>,
    /// The documents GetControllerLog serves.
    pub documents: Vec<ControlPolicyDocument>,
    /// The true state sign-ins and submissions are judged by.
    pub truth: Option<ControlState>,
    /// The state view's head instead of the true one.
    pub state_head: Option<ControlHead>,
    /// Canton Coin as the rpc's catalog names it, for pair-scoped swaps.
    pub cc: Option<agent_control::InstrumentRef>,
    pub policies: Vec<ControlPolicyView>,
    pub commands: Vec<ControlCommandView>,
    pub holdings: GetAgentHoldingsResponse,
    pub traffic: GetAgentTrafficResponse,
    pub packages: ListNodePackagesResponse,
    /// The fake's clock runs this far ahead of the local one.
    pub clock_offset_secs: i64,
    pub challenge_edit: Option<ChallengeEdit>,
    /// Refuse this many sign-ins with 4097.
    pub refuse_sign_ins: u32,
    /// Answer every session call with 4096.
    pub always_no_session: bool,
    /// Largest log page served.
    pub page_limit: u32,
    /// Store each submission, then answer Unavailable, as when its answer is lost.
    pub lose_submit_answers: bool,
    /// Answer these methods Unavailable.
    pub unavailable: Vec<String>,
    /// Answer each of these methods Unavailable once it was called this many times.
    pub unavailable_after: Vec<(String, usize)>,
    /// Store submitted policies with this status, whatever the log holds.
    pub claimed_policy_status: Option<ControlPolicyStatus>,
    /// Store submitted commands with this status; the submit is still answered pending.
    pub claimed_command_status: Option<ControlCommandStatus>,
    /// Store each submitted command as other bytes under its id, with this agent's executed
    /// receipt for those bytes; the submit is still answered with the signed ones.
    pub substitute: Option<TestAgent>,
    /// An agent that accepts each submitted policy at once; its log is then served.
    pub acceptor: Option<TestAgent>,
    /// Every method called, in order.
    pub calls: Vec<String>,
    pub sign_ins: u32,
    challenges: BTreeMap<[u8; 32], Minted>,
    sessions: BTreeMap<[u8; 32], Session>,
}

impl FakeState {
    fn new(network: Network) -> Self {
        Self {
            network,
            rpc_host: RpId::LOCALHOST.to_string(),
            rows: Vec::new(),
            documents: Vec::new(),
            truth: None,
            state_head: None,
            cc: None,
            policies: Vec::new(),
            commands: Vec::new(),
            holdings: GetAgentHoldingsResponse::default(),
            traffic: GetAgentTrafficResponse::default(),
            packages: ListNodePackagesResponse::default(),
            clock_offset_secs: 0,
            challenge_edit: None,
            refuse_sign_ins: 0,
            always_no_session: false,
            page_limit: 256,
            lose_submit_answers: false,
            unavailable: Vec::new(),
            unavailable_after: Vec::new(),
            claimed_policy_status: None,
            claimed_command_status: None,
            substitute: None,
            acceptor: None,
            calls: Vec::new(),
            sign_ins: 0,
            challenges: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }

    fn now_secs(&self) -> i64 {
        crate::clock::local_secs()
            .unwrap_or(0)
            .saturating_add(self.clock_offset_secs)
    }

    fn now(&self) -> Result<UtcTime, Status> {
        UtcTime::from_unix_secs(self.now_secs()).ok_or_else(|| Status::internal("clock"))
    }

    fn timestamp(&self) -> Option<prost_types::Timestamp> {
        Some(prost_types::Timestamp {
            seconds: self.now_secs(),
            nanos: 0,
        })
    }

    /// Ends every session: the next call gets 4096.
    pub fn expire_sessions(&mut self) {
        self.sessions.clear();
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn count(&self, method: &str) -> usize {
        self.calls.iter().filter(|c| c.as_str() == method).count()
    }

    /// Sets a command's status and receipt.
    pub fn settle(
        &mut self,
        id: &str,
        status: ControlCommandStatus,
        receipt: Option<ControlReceipt>,
    ) {
        for view in self.commands.iter_mut().filter(|v| v.command_id == id) {
            view.status = status as i32;
            view.receipt = receipt.clone();
        }
    }

    /// Sets a policy's status.
    pub fn set_policy_status(&mut self, hash: &str, status: ControlPolicyStatus) {
        for view in self.policies.iter_mut().filter(|v| v.policy_hash == hash) {
            view.status = status as i32;
        }
    }
}

fn refused(code: u16) -> Status {
    Status::permission_denied(format!("request refused (code {code})"))
}

fn hash_of(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

fn head_proto(head: Option<HeadRef>) -> ControlHead {
    head.map_or_else(ControlHead::default, |h| ControlHead {
        seq: h.seq,
        hash: h.hash.to_string(),
    })
}

fn caps_proto(set: CapabilitySet) -> Vec<i32> {
    set.iter()
        .map(|c| match c {
            Capability::Policy => ControlCapability::Policy as i32,
            Capability::Direct => ControlCapability::Direct as i32,
            Capability::Read => ControlCapability::Read as i32,
        })
        .collect()
}

fn ts(at: UtcTime) -> Option<prost_types::Timestamp> {
    Some(prost_types::Timestamp {
        seconds: at.unix_secs(),
        nanos: 0,
    })
}

type Shared = Arc<Mutex<FakeState>>;

fn lock(shared: &Shared) -> MutexGuard<'_, FakeState> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn challenge(
    state: &mut FakeState,
    q: GetControllerChallengeRequest,
) -> Result<GetControllerChallengeResponse, Status> {
    let party = PartyId::parse(&q.agent_party).map_err(|_| refused(4084))?;
    let key: [u8; 32] = q
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| refused(4084))?;
    agent_control::parse_ed25519_public_key(&key).map_err(|_| refused(4084))?;
    let mut nonce = [0u8; 16];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut nonce)
        .map_err(|_| Status::internal("no randomness"))?;
    let now = state.now()?;
    let mut minted = ControllerChallenge {
        network: state.network,
        rpc: RpId::parse(&state.rpc_host).map_err(|_| Status::internal("bad host"))?,
        controller_id: controller_id_for(ControllerType::Ed25519, &key),
        agent_party: party,
        nonce: Nonce16(nonce),
        issued_at: now,
        expires_at: shifted(now, 60),
    };
    let mut garbage = None;
    match state.challenge_edit.clone() {
        Some(ChallengeEdit::Rpc(host)) => {
            minted.rpc = RpId::parse(&host).map_err(|_| Status::internal("bad host"))?
        }
        Some(ChallengeEdit::Party(party)) => minted.agent_party = party,
        Some(ChallengeEdit::Network(network)) => minted.network = network,
        Some(ChallengeEdit::Controller(id)) => minted.controller_id = id,
        Some(ChallengeEdit::IssuedAgo(secs)) => {
            minted.issued_at = shifted(now, secs.saturating_neg());
            minted.expires_at = shifted(minted.issued_at, 60);
        }
        Some(ChallengeEdit::Garbage(bytes)) => garbage = Some(bytes),
        None => {}
    }
    let bytes = match garbage {
        Some(bytes) => bytes,
        None => minted.encode().map_err(|_| Status::internal("encode"))?,
    };
    state.challenges.insert(
        hash_of(&bytes),
        Minted {
            key,
            deadline: state.now_secs().saturating_add(60),
        },
    );
    Ok(GetControllerChallengeResponse {
        challenge: bytes,
        expires_at: ts(minted.expires_at),
        server_time: state.timestamp(),
    })
}

fn sign_in(
    state: &mut FakeState,
    q: ControllerSignInRequest,
) -> Result<ControllerSignInResponse, Status> {
    let minted = state
        .challenges
        .remove(&hash_of(&q.challenge))
        .ok_or_else(|| refused(4097))?;
    if state.now_secs() > minted.deadline {
        return Err(refused(4097));
    }
    let envelope = ProofEnvelope::decode(&q.proof).map_err(|_| refused(4097))?;
    let challenge =
        verify_challenge_proof(&q.challenge, &envelope, &minted.key).map_err(|_| refused(4097))?;
    if state.refuse_sign_ins > 0 {
        state.refuse_sign_ins = state.refuse_sign_ins.saturating_sub(1);
        return Err(refused(4097));
    }
    let now = state.now()?;
    let truth = state.truth.as_ref().ok_or_else(|| refused(4097))?;
    if *truth.agent_party() != challenge.agent_party || challenge.network != state.network {
        return Err(refused(4097));
    }
    let record = truth
        .controller(&challenge.controller_id)
        .ok_or_else(|| refused(4097))?;
    if record.capabilities_at(now).is_empty() {
        return Err(refused(4097));
    }
    let mut raw = [0u8; 32];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut raw)
        .map_err(|_| Status::internal("no randomness"))?;
    use base64::Engine;
    let token = format!(
        "{}{}",
        crate::SESSION_TOKEN_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    );
    state.sessions.insert(
        hash_of(token.as_bytes()),
        Session {
            party: challenge.agent_party,
            controller: challenge.controller_id,
        },
    );
    state.sign_ins = state.sign_ins.saturating_add(1);
    Ok(ControllerSignInResponse {
        session_token: token,
        expires_at: ts(shifted(now, 600)),
        server_time: state.timestamp(),
    })
}

/// The session of a call, re-checked against the true state; the capabilities in effect.
fn session_of(
    state: &mut FakeState,
    metadata: &MetadataMap,
    party: &str,
) -> Result<(ControllerId, CapabilitySet), Status> {
    if state.always_no_session {
        return Err(refused(4096));
    }
    let token = metadata
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| refused(4096))?;
    let key = hash_of(token.as_bytes());
    let (session_party, controller) = match state.sessions.get(&key) {
        Some(session) => (session.party.clone(), session.controller),
        None => return Err(refused(4096)),
    };
    if !party.is_empty() && party != session_party.as_str() {
        return Err(refused(4082));
    }
    let now = state.now()?;
    let caps = state
        .truth
        .as_ref()
        .and_then(|truth| truth.controller(&controller))
        .map_or(CapabilitySet::EMPTY, |record| record.capabilities_at(now));
    if caps.is_empty() {
        state.sessions.remove(&key);
        return Err(refused(4096));
    }
    Ok((controller, caps))
}

fn need(caps: CapabilitySet, any_of: &[Capability]) -> Result<(), Status> {
    if any_of.iter().any(|c| caps.contains(*c)) {
        Ok(())
    } else {
        Err(refused(4086))
    }
}

fn log_page(state: &FakeState, q: GetControlLogRequest) -> GetControlLogResponse {
    let from = q.from_seq.max(1);
    let limit = usize::try_from(q.limit.clamp(1, state.page_limit.max(1))).unwrap_or(1);
    let rows: Vec<&ControlLogEntry> = state.rows.iter().filter(|row| row.seq >= from).collect();
    let entries: Vec<ControlLogEntry> = rows.iter().take(limit).map(|row| (*row).clone()).collect();
    GetControlLogResponse {
        has_more: rows.len() > entries.len(),
        entries,
        documents: state.documents.clone(),
        head: Some(head_proto(
            state.truth.as_ref().and_then(ControlState::head),
        )),
    }
}

fn state_view(state: &FakeState) -> Result<GetControlStateResponse, Status> {
    let now = state.now()?;
    let Some(truth) = state.truth.as_ref() else {
        return Ok(GetControlStateResponse::default());
    };
    let controllers = truth
        .controllers()
        .map(|record| ControllerView {
            controller_id: record.id().to_string(),
            label: record.descriptor().label().as_str().to_string(),
            capabilities: caps_proto(record.capabilities_at(now)),
            fingerprint: record.id().fingerprint(),
            added_seq: record.added_seq(),
            in_effect: record.is_effective_at(now),
            ..Default::default()
        })
        .collect();
    let in_force = truth.in_force_policy(now).map(|p| ControlPolicyView {
        policy_hash: p.hash().to_string(),
        version: p.version(),
        status: ControlPolicyStatus::Accepted as i32,
        ..Default::default()
    });
    Ok(GetControlStateResponse {
        agent_party: truth.agent_party().to_string(),
        network: state.network.as_str().to_string(),
        verification: ControlVerification::Verified as i32,
        head: Some(
            state
                .state_head
                .clone()
                .unwrap_or_else(|| head_proto(truth.head())),
        ),
        default_policy: in_force.is_none(),
        policy_in_force: in_force,
        controllers,
        pending_policies: state
            .policies
            .iter()
            .filter(|p| p.status == ControlPolicyStatus::Pending as i32)
            .cloned()
            .collect(),
        commands: state.commands.clone(),
        as_of: state.timestamp(),
        ..Default::default()
    })
}

fn submit_policy(
    state: &mut FakeState,
    signer: ControllerId,
    q: SubmitPolicyRequest,
) -> Result<SubmitPolicyResponse, Status> {
    let envelope = ProofEnvelope::decode(&q.proof).map_err(|_| refused(4084))?;
    if *envelope.controller_id() != signer || q.controller_id != signer.to_string() {
        return Err(refused(4082));
    }
    let now = state.now()?;
    let truth = state.truth.as_ref().ok_or_else(|| refused(4088))?;
    let verified = verify_policy_submission(truth, &q.document, &envelope, now).map_err(|e| {
        if e.code_name() == "signer_lacks_policy" {
            refused(4086)
        } else {
            refused(4084)
        }
    })?;
    let mut view = ControlPolicyView {
        policy_hash: verified.hash.to_string(),
        version: verified.document.version,
        signed_by: signer.to_string(),
        document: q.document,
        proof: q.proof,
        status: ControlPolicyStatus::Pending as i32,
        effective_at: ts(verified.effective_at),
        issued_at: ts(verified.document.issued_at),
        expires_at: ts(verified.document.expires_at),
        submitted_at: state.timestamp(),
        widening: verified.widening.any(),
        ..Default::default()
    };
    if let Some(status) = state.claimed_policy_status {
        view.status = status as i32;
    }
    if let Some(mut agent) = state.acceptor.take() {
        let accepted = agent.accept(&view.document, &envelope);
        publish_into(state, &agent);
        state.acceptor = Some(agent);
        accepted.map_err(|_| Status::internal("accept"))?;
        view.status = ControlPolicyStatus::Accepted as i32;
    }
    state.policies.push(view.clone());
    if state.lose_submit_answers {
        return Err(Status::unavailable("the answer was lost"));
    }
    Ok(SubmitPolicyResponse { policy: Some(view) })
}

fn submit_command(
    state: &mut FakeState,
    signer: ControllerId,
    q: SubmitCommandRequest,
) -> Result<SubmitCommandResponse, Status> {
    let envelope = ProofEnvelope::decode(&q.proof).map_err(|_| refused(4084))?;
    if *envelope.controller_id() != signer || q.controller_id != signer.to_string() {
        return Err(refused(4082));
    }
    let now = state.now()?;
    let truth = state.truth.as_ref().ok_or_else(|| refused(4088))?;
    let verified =
        verify_command(truth, &q.command, &envelope, now, state.cc.as_ref()).map_err(|e| {
            if e.code_name() == "signer_lacks_direct" || (1108..=1112).contains(&e.code()) {
                refused(4086)
            } else {
                refused(4084)
            }
        })?;
    let command = verified.command();
    if state
        .commands
        .iter()
        .any(|c| c.command_id == command.command_id.as_str())
    {
        return Err(refused(4084));
    }
    let view = ControlCommandView {
        command_id: command.command_id.as_str().to_string(),
        agent_party: command.agent_party.to_string(),
        command: q.command,
        controller_id: signer.to_string(),
        proof: q.proof,
        operation_kind: command.op.name().to_string(),
        summary: command.summary.as_str().to_string(),
        status: ControlCommandStatus::Pending as i32,
        log_head: Some(ControlHead {
            seq: command.log_seq,
            hash: command.log_head.to_string(),
        }),
        issued_at: ts(command.issued_at),
        expires_at: ts(command.expires_at),
        submitted_at: state.timestamp(),
        ..Default::default()
    };
    let mut stored = view.clone();
    if let Some(status) = state.claimed_command_status {
        stored.status = status as i32;
    }
    if let Some(agent) = &state.substitute {
        let mut other = command.clone();
        other.summary = Text::parse("another command").map_err(|_| Status::internal("text"))?;
        stored.command = other.encode().map_err(|_| Status::internal("encode"))?;
        let receipt = agent
            .receipt(
                &other.command_id,
                &stored.command,
                ReceiptOutcome::Executed,
                None,
            )
            .map_err(Status::internal)?;
        stored.receipt = Some(receipt);
        stored.status = ControlCommandStatus::Executed as i32;
    }
    state.commands.push(stored);
    if state.lose_submit_answers {
        return Err(Status::unavailable("the answer was lost"));
    }
    Ok(SubmitCommandResponse {
        command: Some(view),
    })
}

fn outage(state: &FakeState, method: &str) -> Result<(), Status> {
    if state.unavailable.iter().any(|m| m == method)
        || state
            .unavailable_after
            .iter()
            .any(|(m, after)| m == method && state.count(method) > *after)
    {
        return Err(Status::unavailable("down"));
    }
    Ok(())
}

fn publish_into(state: &mut FakeState, agent: &TestAgent) {
    state.rows = agent
        .entries
        .iter()
        .map(|e| ControlLogEntry {
            seq: e.seq(),
            canonical: e.bytes().to_vec(),
            signature: e.signature().to_vec(),
            entry_hash: e.hash().to_string(),
        })
        .collect();
    state.documents = agent
        .documents
        .iter()
        .map(|d| ControlPolicyDocument {
            policy_hash: hex::encode(hash_of(d)),
            document: d.clone(),
        })
        .collect();
    state.truth = Some(agent.state().clone());
}

#[derive(Clone)]
struct SignInService(Shared);
#[derive(Clone)]
struct ControllerService(Shared);

impl tonic::server::NamedService for SignInService {
    const NAME: &'static str = "silvana.orderbook.v1.AgentControllerSignInService";
}

impl tonic::server::NamedService for ControllerService {
    const NAME: &'static str = "silvana.orderbook.v1.AgentControllerService";
}

fn http_date(offset: i64) -> Option<String> {
    let secs = crate::clock::local_secs().ok()?.saturating_add(offset);
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|at| at.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

async fn serve<Q, R, B>(
    req: http::Request<B>,
    date: Option<String>,
    handler: Handler<Q, R>,
) -> http::Response<tonic::body::Body>
where
    Q: prost::Message + Default + Send + 'static,
    R: prost::Message + Send + 'static,
    B: tonic::codegen::Body + Send + 'static,
    B::Error: Into<tonic::codegen::StdError> + Send + 'static,
{
    tonic::server::Grpc::new(Prost::<R, Q>::default())
        .unary(Unary { handler, date }, req)
        .await
}

fn method_of<B>(shared: &Shared, req: &http::Request<B>) -> (String, Option<String>) {
    let path = req.uri().path().to_string();
    let method = path.rsplit('/').next().unwrap_or_default().to_string();
    let mut state = lock(shared);
    state.calls.push(method.clone());
    (method, http_date(state.clock_offset_secs))
}

impl<B> tonic::codegen::Service<http::Request<B>> for SignInService
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
        let (method, date) = method_of(&shared, &req);
        Box::pin(async move {
            Ok(match method.as_str() {
                "GetControllerChallenge" => {
                    let handler: Handler<
                        GetControllerChallengeRequest,
                        GetControllerChallengeResponse,
                    > = Arc::new(move |_, q| challenge(&mut lock(&shared), q));
                    serve(req, date, handler).await
                }
                _ => {
                    let handler: Handler<ControllerSignInRequest, ControllerSignInResponse> =
                        Arc::new(move |_, q| sign_in(&mut lock(&shared), q));
                    serve(req, date, handler).await
                }
            })
        })
    }
}

impl<B> tonic::codegen::Service<http::Request<B>> for ControllerService
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
        let (method, date) = method_of(&shared, &req);
        Box::pin(async move {
            let s = shared;
            Ok(match method.as_str() {
                "GetControllerState" => {
                    let h: Handler<GetControlStateRequest, GetControlStateResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            session_of(&mut state, m, &q.agent_party)?;
                            state_view(&state)
                        });
                    serve(req, date, h).await
                }
                "GetControllerLog" => {
                    let h: Handler<GetControlLogRequest, GetControlLogResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            session_of(&mut state, m, &q.agent_party)?;
                            Ok(log_page(&state, q))
                        });
                    serve(req, date, h).await
                }
                "GetControllerPolicy" => {
                    let h: Handler<GetPolicyRequest, GetPolicyResponse> = Arc::new(move |m, q| {
                        let mut state = lock(&s);
                        outage(&state, "GetControllerPolicy")?;
                        let (id, caps) = session_of(&mut state, m, "")?;
                        let view = state
                            .policies
                            .iter()
                            .find(|p| p.policy_hash == q.policy_hash)
                            .cloned();
                        match view {
                            Some(view)
                                if caps.contains(Capability::Read)
                                    || view.signed_by == id.to_string() =>
                            {
                                Ok(GetPolicyResponse { policy: Some(view) })
                            }
                            _ => Err(refused(4072)),
                        }
                    });
                    serve(req, date, h).await
                }
                "SubmitControllerPolicy" => {
                    let h: Handler<SubmitPolicyRequest, SubmitPolicyResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            outage(&state, "SubmitControllerPolicy")?;
                            let (id, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Policy])?;
                            submit_policy(&mut state, id, q)
                        });
                    serve(req, date, h).await
                }
                "SubmitControllerCommand" => {
                    let h: Handler<SubmitCommandRequest, SubmitCommandResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            outage(&state, "SubmitControllerCommand")?;
                            let (id, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Direct])?;
                            submit_command(&mut state, id, q)
                        });
                    serve(req, date, h).await
                }
                "GetControllerCommand" => {
                    let h: Handler<GetCommandRequest, GetCommandResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            outage(&state, "GetControllerCommand")?;
                            let (id, caps) = session_of(&mut state, m, "")?;
                            let view = state
                                .commands
                                .iter()
                                .find(|c| c.command_id == q.command_id)
                                .cloned();
                            match view {
                                Some(view)
                                    if caps.contains(Capability::Read)
                                        || view.controller_id == id.to_string() =>
                                {
                                    Ok(GetCommandResponse {
                                        command: Some(view),
                                    })
                                }
                                _ => Err(refused(4072)),
                            }
                        });
                    serve(req, date, h).await
                }
                "ListControllerCommands" => {
                    let h: Handler<ListCommandsRequest, ListCommandsResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Read])?;
                            let total = u64::try_from(state.commands.len()).unwrap_or(u64::MAX);
                            Ok(ListCommandsResponse {
                                commands: state.commands.clone(),
                                total,
                            })
                        });
                    serve(req, date, h).await
                }
                "GetControllerHoldings" => {
                    let h: Handler<ControllerAgentRequest, GetAgentHoldingsResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Read])?;
                            let mut answer = state.holdings.clone();
                            answer.as_of = state.timestamp();
                            Ok(answer)
                        });
                    serve(req, date, h).await
                }
                "GetControllerTraffic" => {
                    let h: Handler<ControllerAgentRequest, GetAgentTrafficResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Read])?;
                            Ok(state.traffic.clone())
                        });
                    serve(req, date, h).await
                }
                "GetControllerActivity" => {
                    let h: Handler<ControllerPageRequest, GetAgentActivityResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Read])?;
                            Ok(GetAgentActivityResponse::default())
                        });
                    serve(req, date, h).await
                }
                "GetControllerLedger" => {
                    let h: Handler<ControllerPageRequest, GetAgentLedgerResponse> =
                        Arc::new(move |m, q| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, &q.agent_party)?;
                            need(caps, &[Capability::Read])?;
                            Ok(GetAgentLedgerResponse::default())
                        });
                    serve(req, date, h).await
                }
                "ListNodePackages" => {
                    let h: Handler<ListNodePackagesRequest, ListNodePackagesResponse> =
                        Arc::new(move |m, _| {
                            let mut state = lock(&s);
                            let (_, caps) = session_of(&mut state, m, "")?;
                            need(caps, &[Capability::Read, Capability::Policy])?;
                            let mut answer = state.packages.clone();
                            answer.as_of = state.timestamp();
                            Ok(answer)
                        });
                    serve(req, date, h).await
                }
                _ => {
                    let h: Handler<EndControllerSessionRequest, EndControllerSessionResponse> =
                        Arc::new(move |m, _| {
                            let mut state = lock(&s);
                            let token = m
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.strip_prefix("Bearer "))
                                .ok_or_else(|| refused(4096))?;
                            state.sessions.remove(&hash_of(token.as_bytes()));
                            Ok(EndControllerSessionResponse {})
                        });
                    serve(req, date, h).await
                }
            })
        })
    }
}

/// The fake rpc on a loopback port.
pub struct FakeRpc {
    pub url: String,
    pub state: Arc<Mutex<FakeState>>,
    server: tokio::task::JoinHandle<()>,
}

impl FakeRpc {
    pub async fn start(network: Network) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let url = format!(
            "http://{}",
            listener.local_addr().map_err(|e| e.to_string())?
        );
        let state = Arc::new(Mutex::new(FakeState::new(network)));
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let router = tonic::transport::Server::builder()
            .add_service(SignInService(state.clone()))
            .add_service(ControllerService(state.clone()));
        let handle = tokio::runtime::Handle::try_current().map_err(|e| e.to_string())?;
        let server = handle.spawn(async move {
            let _ = router.serve_with_incoming(incoming).await;
        });
        Ok(Self { url, state, server })
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        lock(&self.state)
    }

    /// Serves `agent`'s log and documents, and judges by its state.
    pub fn publish(&self, agent: &TestAgent) {
        publish_into(&mut self.state(), agent);
    }

    /// A channel to the fake.
    pub fn channel(&self) -> Result<tonic::transport::Channel, String> {
        tonic::transport::Endpoint::from_shared(self.url.clone())
            .map(|endpoint| endpoint.connect_lazy())
            .map_err(|e| e.to_string())
    }

    /// A client of `agent` as the controller of `seed`, its key file and pin in a fresh directory.
    pub fn client(
        &self,
        agent: &TestAgent,
        seed: u8,
    ) -> Result<(ControllerClient, PathBuf), String> {
        let key = key_file("client", seed)?;
        let client = self.client_with_key(agent, &key)?;
        Ok((client, key))
    }

    /// A client over an existing key file.
    pub fn client_with_key(
        &self,
        agent: &TestAgent,
        key: &Path,
    ) -> Result<ControllerClient, String> {
        let signer = ControllerSigner::load(key).map_err(|e| e.to_string())?;
        let config = ControllerConfig {
            network: agent.network,
            rpc_host: RpId::parse(RpId::LOCALHOST).map_err(|_| "host".to_string())?,
            agent_party: agent.party.clone(),
        };
        Ok(ControllerClient::new(
            self.channel()?,
            config,
            signer,
            SeenHeads::beside(key),
        ))
    }
}

impl Drop for FakeRpc {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// The documents of a test agent as the client collects them.
pub fn documents_of(agent: &TestAgent) -> PolicyDocuments {
    let mut docs = PolicyDocuments::new();
    for document in &agent.documents {
        docs.insert(document.clone());
    }
    docs
}
