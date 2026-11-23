//! The controller's client of the rpc: sign-in, the agent as its log verifies locally, the
//! reads the rpc reports, and policies and commands drafted from the verified log.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use agent_control::binding::CC_INSTRUMENT_ID;
use agent_control::{
    Capability, CapabilitySet, Command, CommandOp, ControlState, ControllerId, ExpectedChallenge,
    Hash32, HeadRef, InstrumentRef, MAX_LOG_ENTRIES, Network, PartyId, PendingChange,
    PendingTarget, PolicyDocument, PolicyDocuments, RpId, Rules, SignedEntry, Text, UtcTime, Uuid7,
    VerifiedLog, Widening, canton_coin, check_challenge, expected_effective_at, replay,
    required_delay_secs, verify_command, verify_log, verify_policy_submission, widening,
};
use orderbook_proto::orderbook::agent_controller_service_client::AgentControllerServiceClient;
use orderbook_proto::orderbook::agent_controller_sign_in_service_client::AgentControllerSignInServiceClient;
use orderbook_proto::orderbook::{
    ControlCapability, ControlCommandStatus, ControlCommandView, ControlPolicyStatus,
    ControlPolicyView, ControlVerification, ControllerAgentRequest, ControllerPageRequest,
    ControllerSignInRequest, ControllerSignInResponse, EndControllerSessionRequest,
    EndControllerSessionResponse, GetAgentActivityResponse, GetAgentHoldingsResponse,
    GetAgentLedgerResponse, GetAgentTrafficResponse, GetCommandRequest, GetCommandResponse,
    GetControlLogRequest, GetControlLogResponse, GetControlStateRequest, GetControlStateResponse,
    GetControllerChallengeRequest, GetControllerChallengeResponse, GetPolicyRequest,
    GetPolicyResponse, ListCommandsRequest, ListCommandsResponse, ListNodePackagesRequest,
    ListNodePackagesResponse, SubmitCommandRequest, SubmitCommandResponse, SubmitPolicyRequest,
    SubmitPolicyResponse,
};
use sha2::Digest;
use tonic::transport::Channel;
use zeroize::Zeroizing;

use crate::clock::{Clock, date_header_secs, local_now};
use crate::error::ControllerError;
use crate::pin::{PinCheck, SeenHeads};
use crate::receipt::{ReceiptCheck, check_receipt};
use crate::signer::ControllerSigner;
use crate::{
    CALL_TIMEOUT, COMMAND_VALIDITY_SECS, END_TIMEOUT, LOG_PAGE_LIMIT, LOG_TIMEOUT,
    MAX_COMMAND_VALIDITY_SECS, MAX_POLICY_VALIDITY_SECS, MAX_RESPONSE_BYTES, NO_SESSION_CODE,
    SESSION_TOKEN_PREFIX, SIGN_IN_REFUSED_CODE,
};

/// Largest answer of the sign-in service.
const MAX_SIGN_IN_RESPONSE_BYTES: usize = 64 * 1024;
/// Longest session token accepted.
const MAX_TOKEN_BYTES: usize = 256;
/// Pause between two reads of a command or policy being waited for.
const WAIT_POLL: Duration = Duration::from_secs(5);

/// The agent, network and rpc host a client works for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerConfig {
    pub network: Network,
    /// The TLS host challenges must name.
    pub rpc_host: RpId,
    pub agent_party: PartyId,
}

/// What the rpc reports and the client did not verify.
#[derive(Clone, Debug, PartialEq)]
pub struct Reported<T>(pub T);

impl<T> Reported<T> {
    /// How every such value is labelled.
    pub const LABEL: &'static str = "reported by the rpc";

    pub fn into_inner(self) -> T {
        self.0
    }
}

/// The rpc's clock as an answer reports it; the `date` header is read otherwise.
trait ServerTime {
    fn server_secs(&self) -> Option<i64> {
        None
    }
}

fn secs_of(at: Option<&prost_types::Timestamp>) -> Option<i64> {
    at.map(|t| t.seconds).filter(|secs| *secs > 0)
}

impl ServerTime for GetControllerChallengeResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.server_time.as_ref())
    }
}
impl ServerTime for ControllerSignInResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.server_time.as_ref())
    }
}
impl ServerTime for GetControlStateResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.as_of.as_ref())
    }
}
impl ServerTime for ListNodePackagesResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.as_of.as_ref())
    }
}
impl ServerTime for GetAgentHoldingsResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.as_of.as_ref())
    }
}
impl ServerTime for GetControlLogResponse {}
impl ServerTime for GetPolicyResponse {}
impl ServerTime for SubmitPolicyResponse {}
impl ServerTime for SubmitCommandResponse {}
impl ServerTime for GetCommandResponse {}
impl ServerTime for ListCommandsResponse {}
impl ServerTime for GetAgentTrafficResponse {
    fn server_secs(&self) -> Option<i64> {
        secs_of(self.as_of.as_ref())
    }
}
impl ServerTime for GetAgentActivityResponse {}
impl ServerTime for GetAgentLedgerResponse {}
impl ServerTime for EndControllerSessionResponse {}

fn utc_of(at: Option<&prost_types::Timestamp>) -> Option<UtcTime> {
    secs_of(at).and_then(UtcTime::from_unix_secs)
}

/// The session the rpc granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub expires_at: Option<UtcTime>,
}

/// This key's standing in the verified log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The log never added this key.
    NotRecorded,
    /// Added once, removed since (or its removal is due).
    Removed,
    /// Added; the capabilities apply from `effective_at`.
    Pending {
        capabilities: CapabilitySet,
        effective_at: UtcTime,
    },
    /// In effect, with the change still pending, if any.
    InEffect {
        capabilities: CapabilitySet,
        pending: Option<PendingChange>,
    },
}

impl Standing {
    /// The standing of `id` in `state` at `now`.
    pub fn of(state: &ControlState, id: &ControllerId, now: UtcTime) -> Self {
        let Some(record) = state.controller(id) else {
            return if state.is_recorded_controller(id) {
                Self::Removed
            } else {
                Self::NotRecorded
            };
        };
        let capabilities = record.capabilities_at(now);
        let pending = record.pending_at(now).copied();
        if !capabilities.is_empty() {
            return Self::InEffect {
                capabilities,
                pending,
            };
        }
        match pending {
            Some(PendingChange {
                target: PendingTarget::Capabilities(capabilities),
                effective_at,
                ..
            }) => Self::Pending {
                capabilities,
                effective_at,
            },
            _ => Self::Removed,
        }
    }

    /// The capabilities in effect.
    pub fn capabilities(&self) -> CapabilitySet {
        match self {
            Self::InEffect { capabilities, .. } => *capabilities,
            _ => CapabilitySet::EMPTY,
        }
    }

    pub fn allows(&self, capability: Capability) -> bool {
        self.capabilities().contains(capability)
    }
}

/// An agent as its log verified here, with what the rpc reports beside it.
#[derive(Clone, Debug)]
pub struct VerifiedAgent {
    log: VerifiedLog,
    documents: PolicyDocuments,
    state: ControlState,
    standing: Standing,
    pin: PinCheck,
    disagreements: Vec<String>,
    reported: Reported<GetControlStateResponse>,
    checked_at: UtcTime,
}

impl VerifiedAgent {
    pub fn log(&self) -> &VerifiedLog {
        &self.log
    }

    pub fn documents(&self) -> &PolicyDocuments {
        &self.documents
    }

    pub fn state(&self) -> &ControlState {
        &self.state
    }

    pub fn head(&self) -> HeadRef {
        self.log.head()
    }

    /// This key's standing when the log was checked.
    pub fn standing(&self) -> Standing {
        self.standing
    }

    pub fn pin(&self) -> PinCheck {
        self.pin
    }

    /// Where the rpc's account differs from the verified log.
    pub fn disagreements(&self) -> &[String] {
        &self.disagreements
    }

    /// The rpc's own state view (pending policies and commands included).
    pub fn reported(&self) -> &Reported<GetControlStateResponse> {
        &self.reported
    }

    pub fn checked_at(&self) -> UtcTime {
        self.checked_at
    }

    /// Refuses every signature while the pin refuses this log.
    pub fn ensure_signing_allowed(&self) -> Result<(), ControllerError> {
        match self.pin {
            PinCheck::Shorter { pinned } => Err(ControllerError::Pin(format!(
                "the rpc served a log ending at seq {}, shorter than seq {} seen before (rollback or hidden entries)",
                self.log.head().seq,
                pinned.seq
            ))),
            PinCheck::Divergent { pinned } => Err(ControllerError::Pin(format!(
                "the rpc served a log whose entry at seq {} differs from the one seen before",
                pinned.seq
            ))),
            PinCheck::New | PinCheck::Same | PinCheck::Ahead { .. } => Ok(()),
        }
    }

    /// Whether the verified log holds the acceptance of the policy `hash`, in force or scheduled.
    pub fn holds_accepted_policy(&self, hash: &Hash32) -> bool {
        [
            self.state.latest_policy(),
            self.state.in_force_policy(self.checked_at),
        ]
        .into_iter()
        .flatten()
        .any(|policy| policy.hash() == hash)
    }

    /// The rules a policy draft starts from.
    pub fn base_rules(&self, base: PolicyBase, now: UtcTime) -> Result<Rules, ControllerError> {
        match base {
            PolicyBase::InForce => Ok(self.state.rules_at(now).rules().clone()),
            PolicyBase::Scheduled => self
                .state
                .scheduled_policy(now)
                .map(|policy| policy.rules().clone())
                .ok_or_else(|| ControllerError::Refusal("no policy is scheduled".into())),
        }
    }
}

/// Which rules a policy draft starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyBase {
    InForce,
    Scheduled,
}

/// A policy drafted from the verified log; not signed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDraft {
    pub document: PolicyDocument,
    pub bytes: Vec<u8>,
    pub hash: Hash32,
    /// The version in force when it was drafted.
    pub baseline_version: Option<u64>,
    pub widening: Widening,
    pub required_delay_secs: u64,
    pub effective_at: UtcTime,
}

impl PolicyDraft {
    pub fn widens(&self) -> bool {
        self.widening.any()
    }

    /// The typed confirmation.
    pub fn confirmation(&self) -> String {
        format!("sign policy v{}", self.document.version)
    }
}

/// A command drafted from the verified log; not signed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandDraft {
    pub command: Command,
    pub bytes: Vec<u8>,
}

impl CommandDraft {
    /// The typed confirmation: `transfer <amount> <asset>`, or `swap <amount> <sold> for <min> <bought>`.
    pub fn confirmation(&self) -> String {
        match &self.command.op {
            CommandOp::TransferCc { amount, .. } => format!("transfer {amount} CC"),
            CommandOp::TransferToken {
                instrument, amount, ..
            } => format!("transfer {amount} {}", instrument.id.as_str()),
            CommandOp::UsdcxBurn { amount, .. } => format!("burn {amount} USDCx"),
            CommandOp::Swap {
                sell,
                sell_amount,
                buy,
                min_receive,
                ..
            } => format!(
                "swap {sell_amount} {} for {min_receive} {}",
                sell.id.as_str(),
                buy.id.as_str()
            ),
        }
    }

    /// The code the agent's operator types: `CMD-<id>`.
    pub fn code(&self) -> String {
        format!("CMD-{}", self.command.command_id.as_str())
    }
}

/// A policy the rpc stored as pending.
#[derive(Clone, Debug, PartialEq)]
pub struct SubmittedPolicy {
    pub hash: Hash32,
    pub version: u64,
    pub effective_at: UtcTime,
    pub view: Reported<ControlPolicyView>,
}

/// A command the rpc stored as pending.
#[derive(Clone, Debug, PartialEq)]
pub struct SubmittedCommand {
    pub command: Command,
    pub bytes: Vec<u8>,
    pub view: Reported<ControlCommandView>,
}

/// A command as the rpc reports it, with its receipt checked here.
#[derive(Clone, Debug, PartialEq)]
pub struct Waited {
    pub view: Reported<ControlCommandView>,
    pub receipt: ReceiptCheck,
    /// False while the agent may still settle it: no verified receipt, and pending,
    /// delivered, or expired by the rpc's clock alone.
    pub finished: bool,
}

impl Waited {
    /// Counts the receipt only when the rpc reports the bytes this key signed.
    pub fn bound_to(mut self, signed: &[u8]) -> Self {
        if self.view.0.command != signed {
            self.receipt = ReceiptCheck::Unverified(
                "the rpc reports other command bytes than the ones this key signed".into(),
            );
        }
        self
    }
}

/// Open until a verified receipt or a final status: the rpc's expiry is its own clock's, and
/// the agent may still post its receipt after it.
fn command_is_open(status: i32, receipt: &ReceiptCheck) -> bool {
    !receipt.is_verified()
        && (status == ControlCommandStatus::Pending as i32
            || status == ControlCommandStatus::Delivered as i32
            || status == ControlCommandStatus::Expired as i32
            || status == ControlCommandStatus::Unspecified as i32)
}

fn policy_is_open(status: i32) -> bool {
    status == ControlPolicyStatus::Pending as i32
        || status == ControlPolicyStatus::Unspecified as i32
}

/// A fresh UUIDv7: 48 bits of Unix milliseconds, then random bits.
fn new_uuid7() -> Result<Uuid7, ControllerError> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .ok_or_else(|| ControllerError::Clock("the local clock is unreadable".into()))?;
    let mut random = [0u8; 10];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut random)
        .map_err(|e| ControllerError::Refusal(format!("no randomness for a command id: {e}")))?;
    let [_, _, m0, m1, m2, m3, m4, m5] = millis.to_be_bytes();
    let [r0, r1, r2, r3, r4, r5, r6, r7, r8, r9] = random;
    let bytes = [
        m0,
        m1,
        m2,
        m3,
        m4,
        m5,
        0x70 | (r0 & 0x0f),
        r1,
        0x80 | (r2 & 0x3f),
        r3,
        r4,
        r5,
        r6,
        r7,
        r8,
        r9,
    ];
    let hex = hex::encode(bytes);
    let part = |from: usize, to: usize| hex.get(from..to).unwrap_or_default();
    let text = format!(
        "{}-{}-{}-{}-{}",
        part(0, 8),
        part(8, 12),
        part(12, 16),
        part(16, 20),
        part(20, 32)
    );
    Uuid7::parse(&text).map_err(|_| ControllerError::Refusal("could not form a command id".into()))
}

/// Canton Coin as the local scope check names it: the leg whose id is `Amulet`, else a
/// reference naming neither leg. The agent and the ledger-service judge with their own DSO.
fn canton_coin_of(op: &CommandOp) -> Option<InstrumentRef> {
    let CommandOp::Swap { sell, buy, .. } = op else {
        return None;
    };
    let leg = [sell, buy]
        .into_iter()
        .find(|leg| leg.id.as_str() == CC_INSTRUMENT_ID)
        .unwrap_or(sell);
    canton_coin(&leg.admin).ok()
}

/// The summary a command carries when none is given.
fn default_summary(op: &CommandOp) -> Vec<String> {
    match op {
        CommandOp::TransferCc {
            receiver, amount, ..
        } => {
            vec![
                format!("transfer {amount} CC to {receiver}"),
                format!("transfer {amount} CC"),
            ]
        }
        CommandOp::TransferToken {
            receiver,
            instrument,
            amount,
            ..
        } => vec![
            format!("transfer {amount} {} to {receiver}", instrument.id.as_str()),
            format!("transfer {amount} {}", instrument.id.as_str()),
        ],
        CommandOp::UsdcxBurn {
            amount,
            destination,
        } => vec![format!("burn {amount} USDCx to {destination}")],
        CommandOp::Swap {
            sell,
            sell_amount,
            buy,
            min_receive,
            ..
        } => vec![format!(
            "swap {sell_amount} {} for at least {min_receive} {}",
            sell.id.as_str(),
            buy.id.as_str()
        )],
    }
}

fn capability_set(values: &[i32]) -> Option<CapabilitySet> {
    let mut set = CapabilitySet::EMPTY;
    for value in values {
        let capability = match ControlCapability::try_from(*value).ok()? {
            ControlCapability::Policy => Capability::Policy,
            ControlCapability::Direct => Capability::Direct,
            ControlCapability::Read => Capability::Read,
            ControlCapability::Unspecified => return None,
        };
        set = set.with(capability);
    }
    Some(set)
}

fn capabilities_text(set: CapabilitySet) -> String {
    if set.is_empty() {
        return "none".into();
    }
    set.iter()
        .map(Capability::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

fn verification_name(value: i32) -> &'static str {
    match ControlVerification::try_from(value) {
        Ok(ControlVerification::Unlinked) => "unlinked",
        Ok(ControlVerification::Verified) => "verified",
        Ok(ControlVerification::Broken) => "broken",
        Ok(ControlVerification::Forked) => "forked",
        Ok(ControlVerification::Unspecified) | Err(_) => "unknown",
    }
}

/// Where the rpc's state view differs from the verified log.
fn disagreements(
    log: &VerifiedLog,
    state: &ControlState,
    reported: &GetControlStateResponse,
    now: UtcTime,
) -> Vec<String> {
    let mut out = Vec::new();
    if reported.verification != ControlVerification::Verified as i32 {
        out.push(format!(
            "the rpc reports the log as {}",
            verification_name(reported.verification)
        ));
    }
    if reported.agent_party != state.agent_party().as_str()
        || reported.network != state.network().as_str()
    {
        out.push("the rpc answered for another agent or network".into());
    }
    let head = log.head();
    match &reported.head {
        Some(h) if h.seq == head.seq && h.hash == head.hash.to_string() => {}
        Some(h) => out.push(format!(
            "the rpc reports head seq {}, the verified log ends at seq {} ({})",
            h.seq, head.seq, head.hash
        )),
        None => out.push("the rpc reports no head".into()),
    }
    let at = utc_of(reported.as_of.as_ref()).unwrap_or(now);
    if !reported.controllers.is_empty() {
        let mut seen = BTreeSet::new();
        for view in &reported.controllers {
            let Ok(id) = ControllerId::parse(&view.controller_id) else {
                out.push("the rpc reports a malformed controller id".into());
                continue;
            };
            seen.insert(id);
            match state.controller(&id) {
                None => out.push(format!(
                    "the rpc reports controller {} that the log does not hold",
                    id.fingerprint()
                )),
                Some(record) => {
                    let local = record.capabilities_at(at);
                    match capability_set(&view.capabilities) {
                        Some(theirs) if theirs == local => {}
                        theirs => out.push(format!(
                            "controller {}: the rpc reports {}, the log gives {}",
                            id.fingerprint(),
                            theirs.map_or_else(|| "unknown values".to_string(), capabilities_text),
                            capabilities_text(local)
                        )),
                    }
                }
            }
        }
        for record in state.controllers() {
            if record.is_effective_at(at) && !seen.contains(record.id()) {
                out.push(format!(
                    "the rpc leaves out controller {}",
                    record.id().fingerprint()
                ));
            }
        }
    }
    let local = state.in_force_policy(at).map(|p| p.hash().to_string());
    let theirs = reported
        .policy_in_force
        .as_ref()
        .map(|p| p.policy_hash.clone());
    if local != theirs {
        out.push(format!(
            "the rpc reports {} in force, the log gives {}",
            theirs.map_or_else(
                || "the default rules".to_string(),
                |h| format!("policy {h}")
            ),
            local.map_or_else(
                || "the default rules".to_string(),
                |h| format!("policy {h}")
            )
        ));
    }
    out
}

/// Calls the rpc's controller services as one Ed25519 controller of one agent.
pub struct ControllerClient {
    channel: Channel,
    config: ControllerConfig,
    signer: ControllerSigner,
    pins: SeenHeads,
    token: Mutex<Option<Zeroizing<String>>>,
    clock: Clock,
    call_timeout: Duration,
    log_timeout: Duration,
}

impl std::fmt::Debug for ControllerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerClient")
            .field("config", &self.config)
            .field("signer", &self.signer)
            .field("signed_in", &self.has_session())
            .finish_non_exhaustive()
    }
}

impl ControllerClient {
    pub fn new(
        channel: Channel,
        config: ControllerConfig,
        signer: ControllerSigner,
        pins: SeenHeads,
    ) -> Self {
        Self {
            channel,
            config,
            signer,
            pins,
            token: Mutex::new(None),
            clock: Clock::default(),
            call_timeout: CALL_TIMEOUT,
            log_timeout: LOG_TIMEOUT,
        }
    }

    /// Bounds every later call by `call`, and a whole log read by `log`.
    pub fn with_timeouts(mut self, call: Duration, log: Duration) -> Self {
        self.call_timeout = call;
        self.log_timeout = log;
        self
    }

    pub fn config(&self) -> &ControllerConfig {
        &self.config
    }

    pub fn signer(&self) -> &ControllerSigner {
        &self.signer
    }

    pub fn pins(&self) -> &SeenHeads {
        &self.pins
    }

    /// The time to sign at, after the clock check.
    pub fn checked_now(&self) -> Result<UtcTime, ControllerError> {
        self.clock.checked_now()
    }

    pub fn has_session(&self) -> bool {
        self.lock_token().is_some()
    }

    fn lock_token(&self) -> std::sync::MutexGuard<'_, Option<Zeroizing<String>>> {
        self.token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn sign_in_client(&self) -> AgentControllerSignInServiceClient<Channel> {
        AgentControllerSignInServiceClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_SIGN_IN_RESPONSE_BYTES)
    }

    fn service_client(&self) -> AgentControllerServiceClient<Channel> {
        AgentControllerServiceClient::new(self.channel.clone())
            .max_decoding_message_size(MAX_RESPONSE_BYTES)
    }

    /// Runs one call within `timeout` and reads the rpc's clock from its answer.
    async fn bounded<R, F>(&self, timeout: Duration, call: F) -> Result<R, ControllerError>
    where
        R: ServerTime,
        F: Future<Output = Result<tonic::Response<R>, tonic::Status>>,
    {
        match tokio::time::timeout(timeout, call).await {
            Err(_) => Err(ControllerError::Unreachable(format!(
                "no answer within {timeout:?}"
            ))),
            Ok(Err(status)) => {
                if let Some(secs) = date_header_secs(status.metadata()) {
                    self.clock.observe(secs);
                }
                Err(ControllerError::from_status(&status))
            }
            Ok(Ok(response)) => {
                if let Some(secs) = response
                    .get_ref()
                    .server_secs()
                    .or_else(|| date_header_secs(response.metadata()))
                {
                    self.clock.observe(secs);
                }
                Ok(response.into_inner())
            }
        }
    }

    fn authorized<Q>(
        token: &str,
        message: Q,
        timeout: Duration,
    ) -> Result<tonic::Request<Q>, ControllerError> {
        let bearer = Zeroizing::new(format!("Bearer {token}"));
        let value: tonic::metadata::MetadataValue<tonic::metadata::Ascii> =
            bearer.parse().map_err(|_| {
                ControllerError::Refusal("the session token is not a header value".into())
            })?;
        let mut request = tonic::Request::new(message);
        request.metadata_mut().insert("authorization", value);
        request.set_timeout(timeout);
        Ok(request)
    }

    /// Signs in: asks for a challenge, checks it and the clocks, signs it. A refused sign-in
    /// asks for one fresh challenge.
    pub async fn sign_in(&self) -> Result<SignedIn, ControllerError> {
        match self.sign_in_once().await {
            Err(e) if e.code() == Some(SIGN_IN_REFUSED_CODE) => self.sign_in_once().await,
            other => other,
        }
    }

    async fn sign_in_once(&self) -> Result<SignedIn, ControllerError> {
        *self.lock_token() = None;
        let request = GetControllerChallengeRequest {
            agent_party: self.config.agent_party.to_string(),
            public_key: self.signer.public_key().to_vec(),
        };
        let mut client = self.sign_in_client();
        let issued = self
            .bounded(self.call_timeout, client.get_controller_challenge(request))
            .await?;
        let now = self.clock.checked_now()?;
        let expected = ExpectedChallenge {
            network: self.config.network,
            rpc: self.config.rpc_host.clone(),
            controller_id: *self.signer.controller_id(),
            agent_party: self.config.agent_party.clone(),
        };
        let checked = check_challenge(&issued.challenge, &expected, now)
            .map_err(ControllerError::Challenge)?;
        let signed = self.signer.sign_challenge(&checked)?;
        let proof = signed.proof_bytes()?;
        let mut client = self.sign_in_client();
        let request = ControllerSignInRequest {
            challenge: signed.bytes,
            proof,
        };
        let answer = self
            .bounded(self.call_timeout, client.controller_sign_in(request))
            .await?;
        let expires_at = utc_of(answer.expires_at.as_ref());
        let token = Zeroizing::new(answer.session_token);
        let usable = token.starts_with(SESSION_TOKEN_PREFIX)
            && token.len() <= MAX_TOKEN_BYTES
            && token.bytes().all(|b| b.is_ascii_graphic());
        if !usable {
            return Err(ControllerError::Refusal(
                "the rpc answered an unusable session token".into(),
            ));
        }
        *self.lock_token() = Some(token);
        Ok(SignedIn { expires_at })
    }

    /// Ends the session, if any, within [`END_TIMEOUT`]; the token is dropped either way.
    pub async fn end(&self) -> Result<(), ControllerError> {
        let Some(token) = self.lock_token().take() else {
            return Ok(());
        };
        let request = Self::authorized(&token, EndControllerSessionRequest {}, END_TIMEOUT)?;
        let mut client = self.service_client();
        self.bounded(END_TIMEOUT, client.end_controller_session(request))
            .await
            .map(|_| ())
    }

    /// One session call; signs in first when needed, and once more after a 4096.
    async fn session_call<Q, R, F, Fut>(
        &self,
        timeout: Duration,
        message: Q,
        call: F,
    ) -> Result<R, ControllerError>
    where
        Q: Clone,
        R: ServerTime,
        F: Fn(AgentControllerServiceClient<Channel>, tonic::Request<Q>) -> Fut,
        Fut: Future<Output = Result<tonic::Response<R>, tonic::Status>>,
    {
        let mut signed_in_again = false;
        loop {
            if !self.has_session() {
                self.sign_in().await?;
            }
            let token = self
                .lock_token()
                .clone()
                .ok_or_else(|| ControllerError::Refusal("no session".into()))?;
            let request = Self::authorized(&token, message.clone(), timeout)?;
            match self
                .bounded(timeout, call(self.service_client(), request))
                .await
            {
                Err(e) if e.code() == Some(NO_SESSION_CODE) && !signed_in_again => {
                    *self.lock_token() = None;
                    signed_in_again = true;
                }
                other => return other,
            }
        }
    }

    fn party(&self) -> String {
        self.config.agent_party.to_string()
    }

    async fn read_log(&self) -> Result<(Vec<SignedEntry>, PolicyDocuments), ControllerError> {
        let mut entries: Vec<SignedEntry> = Vec::new();
        let mut documents = PolicyDocuments::new();
        let mut from_seq: u64 = 1;
        loop {
            let request = GetControlLogRequest {
                agent_party: self.party(),
                from_seq,
                limit: LOG_PAGE_LIMIT,
            };
            let page = self
                .session_call(self.call_timeout, request, |mut c, r| async move {
                    c.get_controller_log(r).await
                })
                .await?;
            for document in page.documents {
                documents.insert(document.document);
            }
            let rows = page.entries.len();
            for row in page.entries {
                if row.seq != from_seq {
                    return Err(ControllerError::Log(format!(
                        "the rpc served seq {} where seq {from_seq} belongs",
                        row.seq
                    )));
                }
                let signature = <[u8; 64]>::try_from(row.signature.as_slice()).map_err(|_| {
                    ControllerError::Log(format!("the signature of seq {from_seq} is not 64 bytes"))
                })?;
                let entry = SignedEntry::new(row.canonical, signature).map_err(|e| {
                    ControllerError::Log(format!(
                        "seq {from_seq} does not decode ({})",
                        e.code_name()
                    ))
                })?;
                entries.push(entry);
                if entries.len() > MAX_LOG_ENTRIES {
                    return Err(ControllerError::Log(
                        "the rpc served more entries than a log may hold".into(),
                    ));
                }
                from_seq = from_seq
                    .checked_add(1)
                    .ok_or_else(|| ControllerError::Log("the seq overflows".into()))?;
            }
            if !page.has_more {
                break;
            }
            if rows == 0 {
                return Err(ControllerError::Log(
                    "the rpc said more entries follow but served none".into(),
                ));
            }
        }
        if entries.is_empty() {
            return Err(ControllerError::Log(
                "the rpc serves no log for this agent".into(),
            ));
        }
        Ok((entries, documents))
    }

    /// Pages the whole log, verifies and replays it with every document, compares it with the
    /// pin and with the rpc's state view, and reports this key's standing.
    pub async fn agent(&self) -> Result<VerifiedAgent, ControllerError> {
        let read = tokio::time::timeout(self.log_timeout, self.read_log()).await;
        let (entries, documents) = read.map_err(|_| {
            ControllerError::Unreachable(format!(
                "the log was not read within {:?}",
                self.log_timeout
            ))
        })??;
        let network = self.config.network;
        let party = &self.config.agent_party;
        let log = verify_log(network, party, &entries)
            .map_err(|e| ControllerError::Log(format!("{} at seq {}", e.code_name(), e.seq)))?;
        let state = replay(&log, &documents).map_err(|e| {
            ControllerError::Log(format!(
                "it does not replay: {} at seq {}",
                e.code_name(),
                e.seq
            ))
        })?;
        let request = GetControlStateRequest {
            agent_party: self.party(),
            from_seq: log.head().seq.saturating_add(1),
        };
        let reported = self
            .session_call(self.call_timeout, request, |mut c, r| async move {
                c.get_controller_state(r).await
            })
            .await?;
        let now = local_now()?;
        let pin = self.pins.check(&log)?;
        if pin.allows_signing() {
            self.pins.raise(network, party, log.head())?;
        }
        let standing = Standing::of(&state, self.signer.controller_id(), now);
        let disagreements = disagreements(&log, &state, &reported, now);
        Ok(VerifiedAgent {
            log,
            documents,
            state,
            standing,
            pin,
            disagreements,
            reported: Reported(reported),
            checked_at: now,
        })
    }

    pub async fn holdings(&self) -> Result<Reported<GetAgentHoldingsResponse>, ControllerError> {
        let request = ControllerAgentRequest {
            agent_party: self.party(),
        };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.get_controller_holdings(r).await
        })
        .await
        .map(Reported)
    }

    pub async fn traffic(&self) -> Result<Reported<GetAgentTrafficResponse>, ControllerError> {
        let request = ControllerAgentRequest {
            agent_party: self.party(),
        };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.get_controller_traffic(r).await
        })
        .await
        .map(Reported)
    }

    pub async fn activity(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Reported<GetAgentActivityResponse>, ControllerError> {
        let request = ControllerPageRequest {
            agent_party: self.party(),
            limit,
            offset,
        };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.get_controller_activity(r).await
        })
        .await
        .map(Reported)
    }

    pub async fn ledger(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Reported<GetAgentLedgerResponse>, ControllerError> {
        let request = ControllerPageRequest {
            agent_party: self.party(),
            limit,
            offset,
        };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.get_controller_ledger(r).await
        })
        .await
        .map(Reported)
    }

    pub async fn packages(
        &self,
        include_removed: bool,
    ) -> Result<Reported<ListNodePackagesResponse>, ControllerError> {
        let request = ListNodePackagesRequest { include_removed };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.list_node_packages(r).await
        })
        .await
        .map(Reported)
    }

    pub async fn commands(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Reported<ListCommandsResponse>, ControllerError> {
        let request = ListCommandsRequest {
            agent_party: self.party(),
            limit,
            offset,
        };
        self.session_call(self.call_timeout, request, |mut c, r| async move {
            c.list_controller_commands(r).await
        })
        .await
        .map(Reported)
    }

    /// A policy by hash; its document must hash to the name asked for.
    pub async fn policy(
        &self,
        hash: &Hash32,
    ) -> Result<Reported<ControlPolicyView>, ControllerError> {
        let request = GetPolicyRequest {
            policy_hash: hash.to_string(),
        };
        let answer = self
            .session_call(self.call_timeout, request, |mut c, r| async move {
                c.get_controller_policy(r).await
            })
            .await?;
        let view = answer
            .policy
            .ok_or_else(|| ControllerError::Refusal("the rpc answered no policy".into()))?;
        if Hash32(sha2::Sha256::digest(&view.document).into()) != *hash {
            return Err(ControllerError::Refusal(
                "the rpc answered a document that is not the policy asked for".into(),
            ));
        }
        Ok(Reported(view))
    }

    /// Drafts a policy with `rules`: the next version after the latest accepted one, chained
    /// to its hash, issued now and valid `validity_secs`.
    pub fn draft_policy(
        &self,
        agent: &VerifiedAgent,
        rules: Rules,
        validity_secs: u64,
    ) -> Result<PolicyDraft, ControllerError> {
        if !(1..=MAX_POLICY_VALIDITY_SECS).contains(&validity_secs) {
            return Err(ControllerError::Refusal(format!(
                "a policy is valid 1 to {MAX_POLICY_VALIDITY_SECS} seconds"
            )));
        }
        let now = self.clock.checked_now()?;
        let state = agent.state();
        let latest = state.latest_policy();
        let version = latest
            .map_or(0, |p| p.version())
            .checked_add(1)
            .ok_or_else(|| ControllerError::Refusal("the policy version overflows".into()))?;
        let validity = i64::try_from(validity_secs)
            .map_err(|_| ControllerError::Refusal("validity out of range".into()))?;
        let expires_at = now
            .checked_add_secs(validity)
            .ok_or_else(|| ControllerError::Refusal("the expiry is out of range".into()))?;
        let document = PolicyDocument {
            network: state.network(),
            agent_party: state.agent_party().clone(),
            version,
            prev_policy_hash: latest.map_or(Hash32([0; 32]), |p| *p.hash()),
            issued_at: now,
            expires_at,
            not_before: None,
            rules,
        };
        let bytes = document.encode().map_err(|e| {
            ControllerError::Refusal(format!("the policy does not encode ({})", e.code_name()))
        })?;
        let effective = state.rules_at(now);
        let baseline = effective.rules();
        let effective_at = expected_effective_at(now, baseline, &document)
            .ok_or_else(|| ControllerError::Refusal("the effective time is out of range".into()))?;
        Ok(PolicyDraft {
            hash: Hash32(sha2::Sha256::digest(&bytes).into()),
            baseline_version: effective.policy().map(|p| p.version()),
            widening: widening(baseline, &document.rules),
            required_delay_secs: required_delay_secs(document.network, baseline, &document.rules),
            effective_at,
            bytes,
            document,
        })
    }

    fn signing_checks(
        &self,
        agent: &VerifiedAgent,
        capability: Capability,
    ) -> Result<UtcTime, ControllerError> {
        if agent.state().network() != self.config.network
            || *agent.state().agent_party() != self.config.agent_party
        {
            return Err(ControllerError::Refusal(
                "the verified agent is not this client's".into(),
            ));
        }
        agent.ensure_signing_allowed()?;
        let now = self.clock.checked_now()?;
        let standing = Standing::of(agent.state(), self.signer.controller_id(), now);
        if !standing.allows(capability) {
            return Err(ControllerError::Refusal(format!(
                "this key holds no {} capability in effect for the agent",
                capability.as_str()
            )));
        }
        Ok(now)
    }

    /// Signs the draft, checks it as the agent will, and submits it. Once it is sent, an
    /// outcome that is not known is [`ControllerError::Unconfirmed`].
    pub async fn submit_policy(
        &self,
        agent: &VerifiedAgent,
        draft: &PolicyDraft,
    ) -> Result<SubmittedPolicy, ControllerError> {
        let now = self.signing_checks(agent, Capability::Policy)?;
        if now > draft.document.expires_at {
            return Err(ControllerError::Refusal(
                "the draft expired; draft it again".into(),
            ));
        }
        let signed = self.signer.sign_policy(&draft.document)?;
        if signed.bytes != draft.bytes {
            return Err(ControllerError::Refusal(
                "the draft does not encode as shown".into(),
            ));
        }
        let verified =
            verify_policy_submission(agent.state(), &signed.bytes, &signed.envelope, now).map_err(
                |e| {
                    ControllerError::Refusal(format!(
                        "the policy fails the agent's checks: {} (code {})",
                        e.code_name(),
                        e.code()
                    ))
                },
            )?;
        let request = SubmitPolicyRequest {
            agent_party: self.party(),
            document: signed.bytes.clone(),
            controller_id: self.signer.controller_id().to_string(),
            proof: signed.proof_bytes()?,
        };
        let answer = self
            .session_call(self.call_timeout, request, |mut c, r| async move {
                c.submit_controller_policy(r).await
            })
            .await
            .map_err(ControllerError::after_send)?;
        let view = answer
            .policy
            .ok_or_else(|| ControllerError::Unconfirmed("the rpc answered no policy".into()))?;
        if view.policy_hash != verified.hash.to_string() {
            return Err(ControllerError::Unconfirmed(
                "the rpc answered another policy than the one submitted".into(),
            ));
        }
        Ok(SubmittedPolicy {
            hash: verified.hash,
            version: verified.document.version,
            effective_at: verified.effective_at,
            view: Reported(view),
        })
    }

    /// Waits up to `max` for a policy to leave pending.
    pub async fn wait_policy(
        &self,
        hash: &Hash32,
        max: Duration,
    ) -> Result<(Reported<ControlPolicyView>, bool), ControllerError> {
        let start = tokio::time::Instant::now();
        loop {
            let view = self.policy(hash).await?;
            if !policy_is_open(view.0.status) {
                return Ok((view, true));
            }
            let left = max.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Ok((view, false));
            }
            tokio::time::sleep(left.min(WAIT_POLL)).await;
        }
    }

    /// Drafts a command at the verified head, issued now and valid `validity_secs`.
    pub fn draft_command(
        &self,
        agent: &VerifiedAgent,
        op: CommandOp,
        summary: Option<&str>,
        validity_secs: u64,
    ) -> Result<CommandDraft, ControllerError> {
        if !(1..=MAX_COMMAND_VALIDITY_SECS).contains(&validity_secs) {
            return Err(ControllerError::Refusal(format!(
                "a command is valid 1 to {MAX_COMMAND_VALIDITY_SECS} seconds (default {COMMAND_VALIDITY_SECS})"
            )));
        }
        let summary = match summary {
            Some(text) => Text::<280>::parse(text).map_err(|_| {
                ControllerError::Refusal(
                    "the summary must be 1 to 280 printable ASCII characters, single-spaced".into(),
                )
            })?,
            None => default_summary(&op)
                .iter()
                .find_map(|text| Text::<280>::parse(text).ok())
                .ok_or_else(|| {
                    ControllerError::Refusal("give a --summary of at most 280 characters".into())
                })?,
        };
        let now = self.clock.checked_now()?;
        let validity = i64::try_from(validity_secs)
            .map_err(|_| ControllerError::Refusal("validity out of range".into()))?;
        let expires_at = now
            .checked_add_secs(validity)
            .ok_or_else(|| ControllerError::Refusal("the expiry is out of range".into()))?;
        let head = agent.head();
        let command = Command {
            network: agent.state().network(),
            agent_party: agent.state().agent_party().clone(),
            command_id: new_uuid7()?,
            log_seq: head.seq,
            log_head: head.hash,
            issued_at: now,
            expires_at,
            op,
            summary,
        };
        let bytes = command.encode().map_err(|e| {
            ControllerError::Refusal(format!("the command does not encode ({})", e.code_name()))
        })?;
        Ok(CommandDraft { command, bytes })
    }

    /// Signs the draft, checks it as the agent will, and submits it. Once it is sent, an
    /// outcome that is not known is [`ControllerError::Unconfirmed`].
    pub async fn submit_command(
        &self,
        agent: &VerifiedAgent,
        draft: &CommandDraft,
    ) -> Result<SubmittedCommand, ControllerError> {
        let now = self.signing_checks(agent, Capability::Direct)?;
        if now > draft.command.expires_at {
            return Err(ControllerError::Refusal(
                "the draft expired; draft it again".into(),
            ));
        }
        let signed = self.signer.sign_command(&draft.command)?;
        if signed.bytes != draft.bytes {
            return Err(ControllerError::Refusal(
                "the draft does not encode as shown".into(),
            ));
        }
        let cc = canton_coin_of(&draft.command.op);
        verify_command(
            agent.state(),
            &signed.bytes,
            &signed.envelope,
            now,
            cc.as_ref(),
        )
        .map_err(|e| {
            ControllerError::Refusal(format!(
                "the command fails the agent's checks: {} (code {})",
                e.code_name(),
                e.code()
            ))
        })?;
        let request = SubmitCommandRequest {
            agent_party: self.party(),
            command: signed.bytes.clone(),
            controller_id: self.signer.controller_id().to_string(),
            proof: signed.proof_bytes()?,
        };
        let answer = self
            .session_call(self.call_timeout, request, |mut c, r| async move {
                c.submit_controller_command(r).await
            })
            .await
            .map_err(ControllerError::after_send)?;
        let view = answer
            .command
            .ok_or_else(|| ControllerError::Unconfirmed("the rpc answered no command".into()))?;
        if view.command_id != draft.command.command_id.as_str() || view.command != signed.bytes {
            return Err(ControllerError::Unconfirmed(
                "the rpc answered another command than the one submitted".into(),
            ));
        }
        Ok(SubmittedCommand {
            command: draft.command.clone(),
            bytes: signed.bytes,
            view: Reported(view),
        })
    }

    /// A command as the rpc reports it, with its receipt checked against the verified log.
    pub async fn command(
        &self,
        agent: &VerifiedAgent,
        id: &Uuid7,
    ) -> Result<Waited, ControllerError> {
        let request = GetCommandRequest {
            command_id: id.as_str().to_string(),
        };
        let answer = self
            .session_call(self.call_timeout, request, |mut c, r| async move {
                c.get_controller_command(r).await
            })
            .await?;
        let view = answer
            .command
            .ok_or_else(|| ControllerError::Refusal("the rpc answered no command".into()))?;
        let decoded = Command::decode(&view.command).map_err(|e| {
            ControllerError::Refusal(format!(
                "the rpc's command does not decode ({})",
                e.code_name()
            ))
        })?;
        if decoded.command_id != *id
            || decoded.agent_party != self.config.agent_party
            || decoded.network != self.config.network
        {
            return Err(ControllerError::Refusal(
                "the rpc answered another command than the one asked for".into(),
            ));
        }
        let receipt = check_receipt(agent, id, &view.command, view.receipt.as_ref());
        let finished = !command_is_open(view.status, &receipt);
        Ok(Waited {
            view: Reported(view),
            receipt,
            finished,
        })
    }

    /// Waits up to `max` for the agent's verified receipt, or for a final status of the rpc.
    pub async fn wait(
        &self,
        agent: &VerifiedAgent,
        id: &Uuid7,
        max: Duration,
    ) -> Result<Waited, ControllerError> {
        let start = tokio::time::Instant::now();
        loop {
            let waited = self.command(agent, id).await?;
            if waited.finished {
                return Ok(waited);
            }
            let left = max.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Ok(waited);
            }
            tokio::time::sleep(left.min(WAIT_POLL)).await;
        }
    }
}
