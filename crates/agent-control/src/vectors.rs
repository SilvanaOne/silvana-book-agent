//! Golden vectors: inputs with every derived value and verdict, shared with other verifiers.
//! `vectors_print` rewrites the fixture; `vectors_match_the_fixture` recomputes it from the file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ::p256::ecdsa::Signature as P256Signature;
use ::p256::ecdsa::signature::Signer as _;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::builder::{EntryBuilder, PendingEntry};
use crate::canon::codes::{
    base32_encode, format_invite_code, format_link_code, parse_link_code, split_invite_code,
};
use crate::canon::time::UtcTime;
use crate::canon::values::{
    Amount, Commitment, EthAddress, Hash32, InstrumentId, InstrumentRef, Label, MarketId, Network,
    Nonce16, Origin, OwnerAccount, PackageId, RpId, Sig64, Text, Token, Uuid7, decode_b64url,
    encode_b64url, parse_bool, parse_u64, sha256,
};
use crate::challenge::{
    CHALLENGE_CLOCK_SKEW_SECS, ControllerChallenge, ExpectedChallenge, MAX_CHALLENGE_BYTES,
    MAX_CHALLENGE_SECS, check_challenge, verify_challenge_proof,
};
use crate::command::{
    COMMAND_CLOCK_SKEW_SECS, Command, CommandKind, CommandOp, MAX_COMMAND_BYTES,
    MAX_COMMAND_VALIDITY_SECS, VerifiedCommand, verify_command,
};
use crate::controller::{
    BackupPolicy, Capability, CapabilitySet, ControllerDescriptor, ControllerId, ControllerKey,
    ControllerType, WebAuthnKey, controller_id_for,
};
use crate::entry::{
    AcceptPolicyBody, AddControllerBody, DescriptorRef, EntryBody, EntryKind, LinkBody, LogEntry,
    RemoveControllerBody, SetCapabilitiesBody, SignedEntry,
};
use crate::error::{
    CanonError, ChallengeError, CodeError, CommandError, CommandFailure, LinkError, LogError,
    PolicyError, ProofError, ReadSessionError, ReplayError, SignatureError, SignedDocError,
    ValueError, ValueKind, WebAuthnError,
};
use crate::head::{HeadClaim, MAX_HEAD_CLAIM_BYTES};
use crate::json::{
    CommandJson, ControllerJson, HeadClaimJson, LinkDescriptorJson, PolicyJson, ProofEnvelopeJson,
    ReadRequestJson, ReadSessionJson, ReceiptJson, SignedEntryJson,
};
use crate::link::{LinkDescriptor, LinkPurpose, RpPolicy, commitment_of, verify_link_descriptor};
use crate::log::{
    ForkEvidence, HeadCheck, HeadRef, LogComparison, MAX_LOG_ENTRIES, VerifiedLog, check_head,
    compare_logs, decode_entries, verify_extension, verify_log,
};
use crate::party::{PartyId, namespace_for_key};
use crate::policy::eval::{Leg, SettleFee, SettleTerms};
use crate::policy::{
    Allowlist, CommandScope, Coverage, DEFAULT_LINKED_RULES, EvalContext, Flow, Intent, LimitItem,
    Limits, MAX_DELAY_SECS, MAX_NOT_BEFORE_SECS, MAX_PACKAGES, MAX_POLICY_BYTES,
    MAX_POLICY_VALIDITY_SECS, MAX_SCOPE_COMMANDS, MAX_SCOPE_LPS, MAX_SCOPE_PAIRS, MAX_SCOPES,
    OperationKind, POLICY_CLOCK_SKEW_SECS, PlatformParties, PolicyDocument, PolicyInstrument,
    ReasonCode, Recipient, Rules, ScopePair, Unlisted, UsageBucket, UsageTotals, Widening,
    check_packages, evaluate, expected_effective_at, required_delay_secs, verify_policy_submission,
    widening,
};
use crate::proof::{
    ControllerProof, ProofEnvelope, ProofOutcome, ProofType, verify_controller_proof,
};
use crate::read_session::{
    MAX_READ_REQUEST_BYTES, MAX_READ_SESSION_BYTES, MAX_READ_SESSION_SECS, MAX_SESSION_AGENTS,
    READ_REQUEST_WINDOW_MS, READ_SESSION_CLOCK_SKEW_SECS, ReadRequest, ReadSessionRequest,
    SessionId, VerifiedReadSession, verify_read_request, verify_read_session,
};
use crate::receipt::{
    AnyReceipt, CodeSource, MAX_RECEIPT_BYTES, MAX_RECEIPT_V2_BYTES, Receipt, ReceiptOutcome,
    ReceiptSubject, SwapReceipt, SwapResult,
};
use crate::sig::ed25519::verify_ed25519_strict;
use crate::sig::p256::{P256_SPKI_PREFIX, parse_p256_spki, verify_p256_p1363};
use crate::sig::webauthn::{
    FLAG_AT, FLAG_BE, FLAG_BS, FLAG_ED, FLAG_UP, FLAG_UV, WebAuthnAssertion, WebAuthnParams,
    check_sign_count, verify_assertion,
};
use crate::signable::sign_agent;
use crate::state::{
    AcceptedPolicy, ControlState, ControllerChange, MAX_AGENT_CONTROLLERS, PendingChange,
    PendingTarget, PolicyDocuments, controller_change_delay_secs, force_confirmation_phrase,
    minimum_rule, replay,
};
use crate::testkit::{self, Chain, DAY, DEVNET_DELAY, MAINNET_DELAY, at};
use crate::{
    CONTROLLER_ID_DOMAIN, MAX_ENTRY_BYTES, MAX_LINK_BYTES, MAX_PROOF_BYTES, MIN_DOCUMENT_BYTES,
};

const SCHEMA: &str = "silvana.agent-control.vectors.v1";
const REGENERATE: &str = "cargo test -p agent-control vectors_print -- --ignored --nocapture";
const FIXTURE: &str = "tests/vectors/agent-control-vectors.json";
const LOCAL_ORIGIN: &str = "http://localhost:8787";
const PROOF_PAYLOAD: &[u8] = b"scheme=silvana.agent.policy.v1\nnetwork=devnet\nversion=1\n";
const SYNCED: u8 = FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS;
const DEVICE: u8 = FLAG_UP | FLAG_UV;
const DEVNET_RPC: &str = "orderbook-devnet.silvana.dev";
const MAINNET_RPC: &str = "orderbook-mainnet.silvana.dev";

const NOTES: [&str; 15] = [
    "Every key here is a deterministic test key; never use one for anything else.",
    "Bytes are UTF-8 text in a field such as `canonical`, or hex in `canonicalHex` when they are not UTF-8.",
    "u64 values and times are strings; `code` is the number of `codeName`; a decoding line is not part of conformance.",
    "A document's `fields` is its JSON view; a log entry's is the signed-entry view; a receipt is v1 or v2 by its scheme line.",
    "Log cases decode the pool rows, verify them under the case's network and agentParty, then replay them with the listed policy documents stored under keyHex.",
    "`entries`, `base`, `appended`, `local` and `remote` list pool row ids; `stage` is decode, verify, replay, compare, fork or minimum_rule.",
    "A state is reported as of its last entry; each probe, at or after that entry, queries the replayed state at its time.",
    "The controller-change delay comes from the entry's own network: 3600 s on devnet, 86400 s on mainnet. Devnet logs are timed in units of it.",
    "A standing probe reports one controller at its time: whether the log ever recorded it, its capabilities in effect, its pending change, and whether it may sign in.",
    "`controller` names an entry of `controllers`; `session` names a case of `readSessions`; `descriptor` names a document.",
    "A command case's `cc` is Canton Coin as the verifier's own config names it; a case without one verifies with Canton Coin unknown.",
    "Each `expect` holds `ok`; a refusal adds `codeName` and `code`, and log refusals add `stage` and `seq`.",
    "A decision's command counts as verified by `decisionContext.commandSigner` at the case's `verifiedAt`, or at its `now` when absent.",
    "`decisions`, `packageChecks` and `controllerChallenges` exercise checks that only the agent, the ledger-service and the rpc run.",
    "order-book vendors a verbatim copy at test-vectors/agent-control-vectors.json.",
];

// ---------------------------------------------------------------------------
// JSON helpers

type Obj = Map<String, Value>;
type Run = fn(&Value, &Ctx) -> Obj;

trait Named {
    fn name(&self) -> &'static str;
    fn num(&self) -> u16;
}

macro_rules! named {
    ($($t:ty),* $(,)?) => {$(
        impl Named for $t {
            fn name(&self) -> &'static str {
                self.code_name()
            }

            fn num(&self) -> u16 {
                self.code()
            }
        }
    )*};
}

named!(
    ValueError,
    CanonError,
    SignatureError,
    WebAuthnError,
    CodeError,
    LinkError,
    ProofError,
    LogError,
    ReplayError,
    PolicyError,
    CommandError,
    ReadSessionError,
    SignedDocError,
    ChallengeError,
);

fn st<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("string {key} missing in {v}"))
}

fn items<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn time(v: &Value, key: &str) -> UtcTime {
    UtcTime::parse(st(v, key)).unwrap()
}

fn party_at(v: &Value, key: &str) -> PartyId {
    PartyId::parse(st(v, key)).unwrap()
}

fn network(v: &Value) -> Network {
    Network::parse(st(v, "network")).unwrap()
}

fn hex_at(v: &Value, key: &str) -> Vec<u8> {
    hex::decode(st(v, key)).unwrap()
}

fn b64_at(v: &Value, key: &str) -> Vec<u8> {
    decode_b64url(st(v, key)).unwrap()
}

fn sig_at(v: &Value, key: &str) -> [u8; 64] {
    hex_at(v, key).try_into().unwrap()
}

fn bytes_in(v: &Value, key: &str) -> Vec<u8> {
    match v.get(key).and_then(Value::as_str) {
        Some(text) => text.as_bytes().to_vec(),
        None => hex_at(v, &format!("{key}Hex")),
    }
}

fn put(case: &mut Value, key: &str, value: impl Into<Value>) {
    case.as_object_mut()
        .unwrap()
        .insert(key.to_owned(), value.into());
}

fn with_bytes(mut case: Value, key: &str, bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => put(&mut case, key, text),
        Err(_) => put(&mut case, &format!("{key}Hex"), hex::encode(bytes)),
    }
    case
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn view<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn ok(extra: Value) -> Value {
    let mut out = Map::new();
    out.insert("ok".into(), true.into());
    if let Value::Object(fields) = extra {
        out.extend(fields);
    }
    Value::Object(out)
}

fn refused<E: Named>(e: &E) -> Value {
    json!({"ok": false, "codeName": e.name(), "code": e.num()})
}

fn refused_at<E: Named>(stage: &str, seq: u64, e: &E) -> Value {
    json!({"ok": false, "stage": stage, "seq": seq.to_string(), "codeName": e.name(), "code": e.num()})
}

fn either(r: Result<Value, Value>) -> Value {
    r.unwrap_or_else(|e| e)
}

fn one(key: &str, value: impl Into<Value>) -> Obj {
    let mut out = Map::new();
    out.insert(key.to_owned(), value.into());
    out
}

fn expect(v: Value) -> Obj {
    one("expect", v)
}

fn fields(v: Value) -> Obj {
    match v {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn caps_view(c: CapabilitySet) -> Value {
    if c.is_empty() {
        Value::Null
    } else {
        Value::String(c.to_string())
    }
}

fn head_view(h: Option<HeadRef>) -> Value {
    h.map_or(
        Value::Null,
        |h| json!({"seq": h.seq.to_string(), "hash": h.hash.to_string()}),
    )
}

fn outcome_view(o: &ProofOutcome) -> Value {
    json!({"signCount": o.sign_count, "backupEligible": o.backup_eligible, "backupState": o.backup_state})
}

fn proof_outcome(r: Result<ProofOutcome, ProofError>) -> Value {
    match r {
        Ok(o) => ok(outcome_view(&o)),
        Err(e) => refused(&e),
    }
}

fn unit_outcome<E: Named>(r: Result<(), E>) -> Value {
    match r {
        Ok(()) => ok(Value::Null),
        Err(e) => refused(&e),
    }
}

fn policy_view(p: &AcceptedPolicy) -> Value {
    json!({
        "seq": p.seq().to_string(),
        "hash": p.hash().to_string(),
        "version": p.version().to_string(),
        "signedBy": p.signed_by().to_string(),
        "acceptedAt": p.accepted_at().to_string(),
        "effectiveAt": p.effective_at().to_string(),
    })
}

fn pending_view(p: &PendingChange) -> Value {
    let target = match p.target {
        PendingTarget::Removal => Value::String("removal".into()),
        PendingTarget::Capabilities(c) => caps_view(c),
    };
    json!({"seq": p.seq.to_string(), "effectiveAt": p.effective_at.to_string(), "target": target})
}

fn state_view(state: &ControlState) -> Value {
    let last = state.last_created_at();
    let controllers: Vec<Value> = state
        .controllers()
        .map(|r| {
            json!({
                "controllerId": r.id().to_string(),
                "addedSeq": r.added_seq().to_string(),
                "effective": last.map_or(Value::Null, |t| caps_view(r.capabilities_at(t))),
                "pending": r.pending().map_or(Value::Null, pending_view),
                "final": r.final_capabilities().map_or(Value::Null, caps_view),
            })
        })
        .collect();
    json!({
        "head": head_view(state.head()),
        "lastCreatedAt": last.map(|t| t.to_string()),
        "ownerAccount": state.owner_account().map(|o| o.to_string()),
        "controllers": controllers,
        "inForce": last.and_then(|t| state.in_force_policy(t)).map_or(Value::Null, policy_view),
        "scheduled": last.and_then(|t| state.scheduled_policy(t)).map_or(Value::Null, policy_view),
        "forcePhrase": force_confirmation_phrase(state),
    })
}

fn probe(state: &ControlState, now: UtcTime) -> Value {
    let capabilities: Vec<Value> = state
        .controllers()
        .map(|r| json!({"controllerId": r.id().to_string(), "capabilities": caps_view(r.capabilities_at(now))}))
        .collect();
    let rules = state.rules_at(now).policy().map_or(json!({"source": "default"}), |p| {
        json!({"source": "accepted", "seq": p.seq().to_string(), "version": p.version().to_string(), "hash": p.hash().to_string()})
    });
    json!({
        "at": now.to_string(),
        "capabilities": capabilities,
        "holders": {
            "policy": state.holders(Capability::Policy, now).len(),
            "direct": state.holders(Capability::Direct, now).len(),
            "read": state.holders(Capability::Read, now).len(),
        },
        "rules": rules,
        "scheduled": state.scheduled_policy(now).map(|p| p.hash().to_string()),
        "pendingChanges": state.pending_changes(now).len(),
    })
}

fn controller_from(c: &Value) -> ControllerDescriptor {
    let key_bytes = hex_at(c, "publicKey");
    let key = match st(c, "type") {
        "ed25519" => ControllerKey::Ed25519(key_bytes.try_into().unwrap()),
        _ => {
            let origins = items(c, "origins")
                .iter()
                .map(|o| Origin::parse(o.as_str().unwrap()).unwrap())
                .collect();
            let passkey = WebAuthnKey::new(
                &key_bytes,
                b64_at(c, "credentialId"),
                RpId::parse(st(c, "rpId")).unwrap(),
                origins,
                BackupPolicy::parse(st(c, "backupPolicy")).unwrap(),
            )
            .unwrap();
            ControllerKey::WebAuthn(passkey)
        }
    };
    ControllerDescriptor::new(
        key,
        Label::parse(st(c, "label")).unwrap(),
        CapabilitySet::parse(st(c, "capabilities")).unwrap(),
    )
    .unwrap()
}

fn rp_policy_from(v: &Value) -> RpPolicy {
    let network = network(v);
    match v.get("rpIds").and_then(Value::as_array) {
        Some(ids) => RpPolicy::with_rp_ids(
            network,
            ids.iter()
                .map(|id| RpId::parse(id.as_str().unwrap()).unwrap())
                .collect(),
        ),
        None => RpPolicy::for_network(network),
    }
}

fn instrument_from(v: &Value) -> InstrumentRef {
    InstrumentRef {
        admin: party_at(v, "admin"),
        id: InstrumentId::parse(st(v, "id")).unwrap(),
    }
}

fn flow_from(v: &Value) -> Flow {
    let to = match (
        v.get("toParty").and_then(Value::as_str),
        v.get("toEth").and_then(Value::as_str),
    ) {
        (Some(p), _) => Some(Recipient::Party(PartyId::parse(p).unwrap())),
        (None, Some(a)) => Some(Recipient::Eth(EthAddress::parse(a).unwrap())),
        (None, None) => None,
    };
    Flow {
        instrument: instrument_from(v),
        amount: Amount::parse(st(v, "amount")).unwrap(),
        to,
    }
}

fn leg_from(v: &Value) -> Leg {
    Leg {
        instrument: instrument_from(v),
        amount: Amount::parse(st(v, "amount")).unwrap(),
    }
}

fn settle_from(v: &Value) -> SettleTerms {
    let fees = items(v, "fees")
        .iter()
        .map(|f| SettleFee {
            receiver: party_at(f, "receiver"),
            instrument: instrument_from(f),
            amount: Amount::parse(st(f, "amount")).unwrap(),
        })
        .collect();
    SettleTerms {
        lp: party_at(v, "lp"),
        sent: leg_from(&v["sent"]),
        received: leg_from(&v["received"]),
        net_received: Amount::parse(st(v, "netReceived")).unwrap(),
        fees,
        valid_until: time(v, "validUntil"),
    }
}

fn intent_from(v: &Value) -> Intent {
    Intent {
        kind: OperationKind::parse(st(v, "kind")).unwrap(),
        outgoing: items(v, "outgoing").iter().map(flow_from).collect(),
        instruments: items(v, "instruments")
            .iter()
            .map(instrument_from)
            .collect(),
        market: v
            .get("market")
            .and_then(Value::as_str)
            .map(|m| MarketId::parse(m).unwrap()),
        fee_total_cc: v
            .get("feeTotalCc")
            .and_then(Value::as_str)
            .map(|a| Amount::parse(a).unwrap()),
        sub: items(v, "sub").iter().map(intent_from).collect(),
        coverage: if v.get("coverage").and_then(Value::as_str) == Some("unverified") {
            Coverage::Unverified
        } else {
            Coverage::Fields
        },
        settle: v.get("settle").map(settle_from),
    }
}

fn widening_view(w: &Widening) -> Value {
    json!({
        "transfers": w.transfers,
        "trading": w.trading,
        "other": w.other,
        "packages": w.packages,
        "scopes": w.scopes,
        "any": w.any(),
    })
}

fn decode_kind(kind: &str, bytes: &[u8]) -> Result<(), CanonError> {
    match kind {
        "link_descriptor" => LinkDescriptor::decode(bytes).map(drop),
        "proof" => ProofEnvelope::decode(bytes).map(drop),
        "log_entry" => LogEntry::decode(bytes).map(drop),
        "policy" => PolicyDocument::decode(bytes).map(drop),
        "command" => Command::decode(bytes).map(drop),
        "read_session" => ReadSessionRequest::decode(bytes).map(drop),
        "read_request" => ReadRequest::decode(bytes).map(drop),
        "receipt" => AnyReceipt::decode(bytes).map(drop),
        "head_claim" => HeadClaim::decode(bytes).map(drop),
        other => panic!("unknown document kind {other}"),
    }
}

// ---------------------------------------------------------------------------
// Context read back from a vectors file

#[derive(Default)]
struct Ctx {
    rows: BTreeMap<String, (Vec<u8>, [u8; 64])>,
    docs: BTreeMap<String, (Hash32, Vec<u8>)>,
    documents: BTreeMap<String, Vec<u8>>,
    controllers: BTreeMap<String, ControllerDescriptor>,
    sessions: BTreeMap<String, Value>,
    decision: Value,
}

impl Ctx {
    fn new(root: &Value) -> Self {
        let mut ctx = Self::default();
        for row in items(&root["logPool"], "entries") {
            let row_value = (bytes_in(row, "canonical"), sig_at(row, "signatureHex"));
            ctx.rows.insert(st(row, "id").to_owned(), row_value);
        }
        for doc in items(&root["logPool"], "policyDocuments") {
            let key = Hash32::parse(st(doc, "keyHex")).unwrap();
            ctx.docs
                .insert(st(doc, "id").to_owned(), (key, bytes_in(doc, "canonical")));
        }
        for doc in items(root, "documents") {
            ctx.documents
                .insert(st(doc, "name").to_owned(), bytes_in(doc, "canonical"));
        }
        for c in items(root, "controllers") {
            ctx.controllers
                .insert(st(c, "name").to_owned(), controller_from(c));
        }
        for s in items(root, "readSessions") {
            ctx.sessions.insert(st(s, "name").to_owned(), s.clone());
        }
        ctx.decision = root["decisionContext"].clone();
        ctx
    }

    fn rows(&self, ids: &[Value]) -> Vec<(Vec<u8>, [u8; 64])> {
        ids.iter()
            .map(|id| self.rows[id.as_str().unwrap()].clone())
            .collect()
    }

    fn doc(&self, id: &str) -> &[u8] {
        &self.docs[id].1
    }

    fn policy_docs(&self, case: &Value) -> PolicyDocuments {
        let mut docs = PolicyDocuments::new();
        for id in items(case, "documents") {
            let (key, bytes) = &self.docs[id.as_str().unwrap()];
            docs.insert_keyed(*key, bytes.clone());
        }
        docs
    }

    fn log_of(&self, net: Network, party: &PartyId, ids: &[Value]) -> Result<VerifiedLog, Value> {
        let entries =
            decode_entries(&self.rows(ids)).map_err(|e| refused_at("decode", e.seq, &e))?;
        verify_log(net, party, &entries).map_err(|e| refused_at("verify", e.seq, &e))
    }

    fn log(&self, case: &Value, key: &str) -> Result<VerifiedLog, Value> {
        self.log_of(
            network(case),
            &party_at(case, "agentParty"),
            items(case, key),
        )
    }

    fn state(&self, case: &Value) -> Result<ControlState, Value> {
        let log = self.log(case, "entries")?;
        replay(&log, &self.policy_docs(case)).map_err(|e| refused_at("replay", e.seq, &e))
    }

    fn session(&self, case: &Value) -> Result<VerifiedReadSession, Value> {
        let states = items(case, "agents")
            .iter()
            .map(|agent| self.state(agent))
            .collect::<Result<Vec<_>, _>>()?;
        let refs: Vec<&ControlState> = states.iter().collect();
        let envelope = ProofEnvelope::decode(&bytes_in(case, "envelope")).unwrap();
        verify_read_session(
            &bytes_in(case, "session"),
            &envelope,
            &refs,
            time(case, "now"),
        )
        .map_err(|e| refused(&e))
    }
}

// ---------------------------------------------------------------------------
// Runners: each derives a case's outputs from its inputs alone

const SECTIONS: &[(&str, Run)] = &[
    ("values", run_value),
    ("originRpId", run_origin),
    ("rpPolicy", run_rp_policy),
    ("ids", run_id),
    ("parties", run_party),
    ("controllers", run_controller),
    ("codes/commitments", run_code),
    ("codes/parse", run_code_parse),
    ("documents", run_document),
    ("negative", run_negative),
    ("webauthn", run_webauthn),
    ("signCount", run_sign_count),
    ("proofs", run_proof),
    ("links", run_link),
    ("logPool/entries", run_pool_entry),
    ("logPool/policyDocuments", run_pool_doc),
    ("logs", run_log),
    ("timeline", run_log),
    ("controllerDelay", run_log),
    ("extensions", run_extension),
    ("comparisons", run_comparison),
    ("forkEvidence", run_fork),
    ("headChecks", run_head_check),
    ("jwtClaims", run_jwt),
    ("minimumRule", run_minimum),
    ("widening", run_widening),
    ("policySubmissions", run_submission),
    ("commands", run_command),
    ("readSessions", run_read_session),
    ("readRequests", run_read_request),
    ("receipts", run_receipt),
    ("headClaims", run_head_claim),
    ("kinds/table", run_kind),
    ("kinds/fromTransactionOperation", run_tx_op),
    ("decisions", run_decision),
    ("controllerStanding", run_standing),
    ("packageChecks", run_package_check),
    ("controllerChallenges", run_challenge),
];

fn plain<T, E: Named>(r: Result<T, E>) -> Value {
    match r {
        Ok(_) => ok(Value::Null),
        Err(e) => refused(&e),
    }
}

fn run_value(c: &Value, _: &Ctx) -> Obj {
    let input = st(c, "input");
    let v = match st(c, "type") {
        "hex16" => plain(Nonce16::parse(input)),
        "hex32" => plain(Hash32::parse(input)),
        "hex64" => plain(Sig64::parse(input)),
        "u64" => match parse_u64(input) {
            Ok(n) => ok(json!({"value": n.to_string()})),
            Err(e) => refused(&e),
        },
        "bool" => plain(parse_bool(input)),
        "time" => match UtcTime::parse(input) {
            Ok(t) => ok(json!({"unixSecs": t.unix_secs().to_string()})),
            Err(e) => refused(&e),
        },
        "amount" => plain(Amount::parse(input)),
        "party" => plain(PartyId::parse(input)),
        "owner_account" => plain(OwnerAccount::parse(input)),
        "label" => plain(Label::parse(input)),
        "text140" => plain(Text::<140>::parse(input)),
        "text280" => plain(Text::<280>::parse(input)),
        "rp_id" => plain(RpId::parse(input)),
        "origin" => plain(Origin::parse(input)),
        "base64url" => match decode_b64url(input) {
            Some(bytes) => ok(json!({"bytesHex": hex::encode(bytes)})),
            None => refused(&ValueError(ValueKind::Base64Url)),
        },
        "uuid7" => plain(Uuid7::parse(input)),
        "instrument_id" => plain(InstrumentId::parse(input)),
        "market_id" => plain(MarketId::parse(input)),
        "token" => plain(Token::parse(input)),
        "eth_address" => plain(EthAddress::parse(input)),
        "network" => plain(Network::parse(input)),
        "capabilities" => plain(CapabilitySet::parse(input)),
        "controller_type" => plain(ControllerType::parse(input)),
        "backup_policy" => plain(BackupPolicy::parse(input)),
        "proof_type" => plain(ProofType::parse(input)),
        "purpose" => plain(LinkPurpose::parse(input)),
        "entry_kind" => plain(EntryKind::parse(input)),
        "unlisted" => plain(Unlisted::parse(input)),
        "receipt_outcome" => plain(ReceiptOutcome::parse(input)),
        "code_source" => plain(CodeSource::parse(input)),
        "package_id" => plain(PackageId::parse(input)),
        "command_kind" => plain(CommandKind::parse(input)),
        other => panic!("unknown value type {other}"),
    };
    expect(v)
}

fn run_origin(c: &Value, _: &Ctx) -> Obj {
    let origin = Origin::parse(st(c, "origin")).unwrap();
    let rp_id = RpId::parse(st(c, "rpId")).unwrap();
    one("matches", origin.matches_rp_id(&rp_id))
}

fn run_rp_policy(c: &Value, _: &Ctx) -> Obj {
    let origins: Vec<Origin> = items(c, "origins")
        .iter()
        .map(|o| Origin::parse(o.as_str().unwrap()).unwrap())
        .collect();
    let rp_id = RpId::parse(st(c, "rpId")).unwrap();
    expect(unit_outcome(rp_policy_from(c).check(&rp_id, &origins)))
}

fn run_id(c: &Value, _: &Ctx) -> Obj {
    let kind = ControllerType::parse(st(c, "type")).unwrap();
    let id = controller_id_for(kind, &hex_at(c, "keyHex"));
    fields(json!({"controllerId": id.to_string(), "fingerprint": id.fingerprint()}))
}

fn run_party(c: &Value, _: &Ctx) -> Obj {
    let key: [u8; 32] = hex_at(c, "keyHex").try_into().unwrap();
    let namespace = namespace_for_key(&key);
    let party = format!("{}::{namespace}", st(c, "hint"));
    fields(json!({"namespace": namespace, "party": party}))
}

fn run_controller(c: &Value, _: &Ctx) -> Obj {
    let descriptor = controller_from(c);
    fields(
        json!({"controllerId": descriptor.id().to_string(), "fingerprint": descriptor.id().fingerprint()}),
    )
}

fn run_code(c: &Value, _: &Ctx) -> Obj {
    let commitment = Commitment::parse(st(c, "commitmentHex")).unwrap();
    let body = format!("AGT-{}", base32_encode(&hex_at(c, "inviteBodyHex")));
    let invite = format_invite_code(&body, &commitment).unwrap();
    fields(json!({"inviteBody": body, "invite": invite, "link": format_link_code(&commitment)}))
}

fn run_code_parse(c: &Value, _: &Ctx) -> Obj {
    let input = st(c, "input");
    let v = match st(c, "function") {
        "split_invite_code" => match split_invite_code(input) {
            Ok((invite, commitment)) => ok(json!({
                "invite": invite,
                "commitmentHex": commitment.map(|c| c.to_string()),
            })),
            Err(e) => refused(&e),
        },
        "parse_link_code" => match parse_link_code(input) {
            Ok(commitment) => ok(json!({"commitmentHex": commitment.to_string()})),
            Err(e) => refused(&e),
        },
        _ => {
            let commitment = Commitment::parse(st(c, "commitmentHex")).unwrap();
            match format_invite_code(input, &commitment) {
                Ok(code) => ok(json!({"code": code})),
                Err(e) => refused(&e),
            }
        }
    };
    expect(v)
}

fn run_document(c: &Value, ctx: &Ctx) -> Obj {
    let bytes = bytes_in(c, "canonical");
    let mut out = one("sha256Hex", hex::encode(sha256(&bytes)));
    let fields_view = match st(c, "kind") {
        "link_descriptor" => {
            let d = LinkDescriptor::decode(&bytes).unwrap();
            let commitment = commitment_of(&bytes);
            out.insert("commitmentHex".into(), commitment.to_string().into());
            out.insert("linkCode".into(), format_link_code(&commitment).into());
            if let Some(body) = c.get("inviteBodyHex").and_then(Value::as_str) {
                let bare = format!("AGT-{}", base32_encode(&hex::decode(body).unwrap()));
                let code = format_invite_code(&bare, &commitment).unwrap();
                out.insert("inviteCode".into(), code.into());
            }
            view(&LinkDescriptorJson::from(&d))
        }
        "proof" => {
            let envelope = ProofEnvelope::decode(&bytes).unwrap();
            let controller = &ctx.controllers[st(c, "controller")];
            let verdict = verify_controller_proof(controller, &bytes_in(c, "payload"), &envelope);
            out.insert("verify".into(), proof_outcome(verdict));
            view(&ProofEnvelopeJson::from(&envelope))
        }
        "log_entry" => {
            let signed = SignedEntry::new(bytes.clone(), sig_at(c, "signatureHex")).unwrap();
            let verdict =
                verify_ed25519_strict(signed.entry().agent_key(), &bytes, signed.signature());
            out.insert("signature".into(), unit_outcome(verdict));
            view(&SignedEntryJson::from(&signed))
        }
        "policy" => view(&PolicyJson::from(&PolicyDocument::decode(&bytes).unwrap())),
        "command" => {
            let command = Command::decode(&bytes).unwrap();
            out.insert("cantonCommandId".into(), command.canton_command_id().into());
            view(&CommandJson::from(&command))
        }
        "read_session" => {
            let request = ReadSessionRequest::decode(&bytes).unwrap();
            out.insert(
                "sessionIdHex".into(),
                SessionId::of(&bytes).to_string().into(),
            );
            view(&ReadSessionJson::from(&request))
        }
        "read_request" => {
            let request = ReadRequest::decode(&bytes).unwrap();
            let key = parse_p256_spki(&hex_at(c, "sessionKeyHex")).unwrap();
            let verdict = verify_p256_p1363(&key, &bytes, &hex_at(c, "signatureHex"));
            out.insert("signature".into(), unit_outcome(verdict));
            view(&ReadRequestJson::from(&request))
        }
        "receipt" => {
            let receipt = AnyReceipt::decode(&bytes).unwrap();
            let party = receipt.receipt().agent_party.clone();
            let verdict = AnyReceipt::verify(&bytes, &sig_at(c, "signatureHex"), &party);
            out.insert("signature".into(), plain(verdict));
            view(&ReceiptJson::from(&receipt))
        }
        "head_claim" => {
            let claim = HeadClaim::decode(&bytes).unwrap();
            let verdict = HeadClaim::verify(
                &bytes,
                &sig_at(c, "signatureHex"),
                claim.network,
                &claim.agent_party,
            );
            out.insert("signature".into(), plain(verdict));
            view(&HeadClaimJson::from(&claim))
        }
        other => panic!("unknown document kind {other}"),
    };
    out.insert("fields".into(), fields_view);
    if let Some(envelopes) = c.get("envelopes").and_then(Value::as_array) {
        let checks: Vec<Value> = envelopes
            .iter()
            .map(|env| {
                let envelope = ProofEnvelope::decode(&bytes_in(env, "canonical")).unwrap();
                let controller = &ctx.controllers[st(env, "controller")];
                let ed25519 = match envelope.proof() {
                    ControllerProof::Ed25519 { signature } => Value::String(hex::encode(signature)),
                    ControllerProof::WebAuthn(_) => Value::Null,
                };
                json!({
                    "controllerId": envelope.controller_id().to_string(),
                    "ed25519SignatureHex": ed25519,
                    "fields": view(&ProofEnvelopeJson::from(&envelope)),
                    "verify": proof_outcome(verify_controller_proof(controller, &bytes, &envelope)),
                })
            })
            .collect();
        out.insert("envelopeChecks".into(), checks.into());
    }
    out
}

fn run_negative(c: &Value, _: &Ctx) -> Obj {
    expect(unit_outcome(decode_kind(
        st(c, "kind"),
        &bytes_in(c, "canonical"),
    )))
}

fn run_webauthn(c: &Value, _: &Ctx) -> Obj {
    let p = &c["params"];
    let spki = hex_at(p, "spkiHex");
    let rp_id = RpId::parse(st(p, "rpId")).unwrap();
    let origins: Vec<Origin> = items(p, "origins")
        .iter()
        .map(|o| Origin::parse(o.as_str().unwrap()).unwrap())
        .collect();
    let params = WebAuthnParams {
        spki: &spki,
        rp_id: &rp_id,
        origins: &origins,
        backup_policy: BackupPolicy::parse(st(p, "backupPolicy")).unwrap(),
    };
    let challenge: [u8; 32] = hex_at(c, "challengeHex").try_into().unwrap();
    let assertion = WebAuthnAssertion {
        authenticator_data: b64_at(c, "authenticatorDataB64url"),
        client_data_json: b64_at(c, "clientDataJsonB64url"),
        signature: b64_at(c, "signatureB64url"),
    };
    expect(match verify_assertion(&params, &challenge, &assertion) {
        Ok(info) => ok(json!({
            "signCount": info.sign_count,
            "backupEligible": info.backup_eligible,
            "backupState": info.backup_state,
        })),
        Err(e) => refused(&e),
    })
}

fn run_sign_count(c: &Value, _: &Ctx) -> Obj {
    let n = |key: &str| u32::try_from(c[key].as_u64().unwrap()).unwrap();
    expect(unit_outcome(check_sign_count(n("previous"), n("current"))))
}

fn run_proof(c: &Value, ctx: &Ctx) -> Obj {
    let controller = &ctx.controllers[st(c, "controller")];
    let envelope = ProofEnvelope::decode(&bytes_in(c, "envelope")).unwrap();
    expect(proof_outcome(verify_controller_proof(
        controller,
        &bytes_in(c, "payload"),
        &envelope,
    )))
}

fn run_link(c: &Value, ctx: &Ctx) -> Obj {
    let bytes = &ctx.documents[st(c, "descriptor")];
    let commitment = Commitment::parse(st(c, "commitmentHex")).unwrap();
    let expected = c["expectedParty"]
        .as_str()
        .map(|p| PartyId::parse(p).unwrap());
    let verdict = verify_link_descriptor(
        bytes,
        &commitment,
        network(c),
        expected.as_ref(),
        c["allowInvite"].as_bool().unwrap(),
        &rp_policy_from(&c["rpPolicy"]),
        time(c, "now"),
    );
    expect(match verdict {
        Ok(d) => ok(json!({
            "purpose": d.purpose().as_str(),
            "agentParty": d.agent_party().map(ToString::to_string),
            "controllers": d.controllers().len(),
        })),
        Err(e) => refused(&e),
    })
}

fn run_pool_entry(c: &Value, _: &Ctx) -> Obj {
    let bytes = bytes_in(c, "canonical");
    let mut out = one("entryHash", hex::encode(sha256(&bytes)));
    out.insert("decodes".into(), LogEntry::decode(&bytes).is_ok().into());
    out
}

fn run_pool_doc(c: &Value, _: &Ctx) -> Obj {
    one("sha256Hex", hex::encode(sha256(&bytes_in(c, "canonical"))))
}

fn run_log(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(ctx.state(c).map(|state| {
        let probes: Vec<Value> = items(c, "probeTimes")
            .iter()
            .map(|t| probe(&state, UtcTime::parse(t.as_str().unwrap()).unwrap()))
            .collect();
        ok(json!({"state": state_view(&state), "probes": probes}))
    })))
}

fn extension(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let base = ctx.log(c, "base")?;
    let appended = decode_entries(&ctx.rows(items(c, "appended")))
        .map_err(|e| refused_at("decode", e.seq, &e))?;
    let log = verify_extension(&base, &appended).map_err(|e| refused_at("verify", e.seq, &e))?;
    Ok(ok(json!({"head": head_view(Some(log.head()))})))
}

fn run_extension(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(extension(c, ctx)))
}

fn comparison(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let local = ctx.log(c, "local")?;
    let remote_party = c.get("remoteAgentParty").map_or_else(
        || party_at(c, "agentParty"),
        |_| party_at(c, "remoteAgentParty"),
    );
    let remote = ctx.log_of(network(c), &remote_party, items(c, "remote"))?;
    let result =
        match compare_logs(&local, &remote).map_err(|e| refused_at("compare", e.seq, &e))? {
            LogComparison::Equal => json!({"result": "equal"}),
            LogComparison::RemoteBehind { local, remote } => json!({
                "result": "remote_behind",
                "localHead": head_view(Some(local)),
                "remoteHead": head_view(Some(remote)),
            }),
            LogComparison::RemoteAhead { local, remote } => json!({
                "result": "remote_ahead",
                "localHead": head_view(Some(local)),
                "remoteHead": head_view(Some(remote)),
            }),
            LogComparison::Fork(evidence) => json!({
                "result": "fork",
                "fork": {
                    "seq": evidence.seq().to_string(),
                    "localHash": evidence.local().hash().to_string(),
                    "remoteHash": evidence.remote().hash().to_string(),
                    "verify": unit_outcome(evidence.verify(network(c), &party_at(c, "agentParty"))),
                },
            }),
        };
    Ok(ok(result))
}

fn run_comparison(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(comparison(c, ctx)))
}

fn run_fork(c: &Value, ctx: &Ctx) -> Obj {
    let entry = |key: &str| {
        let (bytes, signature) = ctx.rows[st(c, key)].clone();
        SignedEntry::new(bytes, signature).unwrap()
    };
    let evidence = ForkEvidence::new(entry("local"), entry("remote"));
    expect(
        match evidence.verify(network(c), &party_at(c, "agentParty")) {
            Ok(()) => ok(json!({"seq": evidence.seq().to_string()})),
            Err(e) => refused_at("fork", e.seq, &e),
        },
    )
}

fn head_check(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let log = ctx.log(c, "entries")?;
    let claimed = HeadRef {
        seq: parse_u64(st(&c["claimed"], "seq")).unwrap(),
        hash: Hash32::parse(st(&c["claimed"], "hash")).unwrap(),
    };
    Ok(ok(match check_head(&log, &claimed) {
        HeadCheck::Matches => json!({"result": "matches"}),
        HeadCheck::ClaimBehind { head } => {
            json!({"result": "claim_behind", "head": head_view(Some(head))})
        }
        HeadCheck::LogBehind { head } => {
            json!({"result": "log_behind", "head": head_view(Some(head))})
        }
        HeadCheck::Mismatch => json!({"result": "mismatch"}),
    }))
}

fn run_head_check(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(head_check(c, ctx)))
}

fn run_jwt(c: &Value, _: &Ctx) -> Obj {
    let seq = c["ctlSeq"].as_str().map(|s| parse_u64(s).unwrap());
    let head = c["ctlHead"].as_str();
    expect(match HeadRef::from_jwt_claims(seq, head) {
        Ok(found) => ok(json!({"head": head_view(found)})),
        Err(e) => refused(&e),
    })
}

fn minimum(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let state = ctx.state(c)?;
    let change = &c["change"];
    let id = ControllerId::parse(st(change, "controllerId")).unwrap();
    let change = match st(change, "kind") {
        "remove" => ControllerChange::Remove(id),
        _ => ControllerChange::SetCapabilities(
            id,
            CapabilitySet::parse(st(change, "capabilities")).unwrap(),
        ),
    };
    let outcome =
        minimum_rule(&state, &change).map_err(|e| refused_at("minimum_rule", e.seq, &e))?;
    Ok(ok(json!({
        "orphaned": caps_view(outcome.orphaned()),
        "remaining": {
            "policy": outcome.remaining(Capability::Policy),
            "direct": outcome.remaining(Capability::Direct),
            "read": outcome.remaining(Capability::Read),
        },
        "violation": outcome.is_violation(),
        "forcePhrase": force_confirmation_phrase(&state),
    })))
}

fn run_minimum(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(minimum(c, ctx)))
}

/// The delay is the next document's network's: a widening waits on the network it is for.
fn run_widening(c: &Value, ctx: &Ctx) -> Obj {
    let base = PolicyDocument::decode(ctx.doc(st(c, "baseline"))).unwrap();
    let next = PolicyDocument::decode(ctx.doc(st(c, "next"))).unwrap();
    let w = widening(&base.rules, &next.rules);
    let effective = expected_effective_at(time(c, "acceptedAt"), &base.rules, &next);
    let delay = required_delay_secs(next.network, &base.rules, &next.rules);
    fields(json!({
        "widens": widening_view(&w),
        "requiredDelaySecs": delay.to_string(),
        "effectiveAt": effective.map(|t| t.to_string()),
    }))
}

fn submission(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let state = ctx.state(c)?;
    let envelope = ProofEnvelope::decode(&bytes_in(c, "envelope")).unwrap();
    let verified =
        verify_policy_submission(&state, ctx.doc(st(c, "policy")), &envelope, time(c, "now"))
            .map_err(|e| refused(&e))?;
    Ok(ok(json!({
        "hash": verified.hash.to_string(),
        "version": verified.document.version.to_string(),
        "signer": verified.signer.to_string(),
        "widens": widening_view(&verified.widening),
        "requiredDelaySecs": verified.required_delay_secs.to_string(),
        "effectiveAt": verified.effective_at.to_string(),
    })))
}

fn run_submission(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(submission(c, ctx)))
}

fn command_case(c: &Value, ctx: &Ctx) -> Result<Value, Value> {
    let state = ctx.state(c)?;
    let envelope = ProofEnvelope::decode(&bytes_in(c, "envelope")).unwrap();
    let cc = c.get("cc").map(instrument_from);
    let verified = verify_command(
        &state,
        &bytes_in(c, "command"),
        &envelope,
        time(c, "now"),
        cc.as_ref(),
    )
    .map_err(|e| refused(&e))?;
    Ok(ok(json!({
        "hash": verified.hash().to_string(),
        "controllerId": verified.controller_id().to_string(),
        "op": verified.command().op.name(),
        "cantonCommandId": verified.command().canton_command_id(),
        "signCount": verified.outcome().sign_count,
    })))
}

fn run_command(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(command_case(c, ctx)))
}

fn run_read_session(c: &Value, ctx: &Ctx) -> Obj {
    expect(either(ctx.session(c).map(|session| {
        ok(json!({
            "sessionIdHex": session.session_id.to_string(),
            "signCount": session.outcome.sign_count,
        }))
    })))
}

fn run_read_request(c: &Value, ctx: &Ctx) -> Obj {
    let session = ctx
        .session(&ctx.sessions[st(c, "session")])
        .unwrap_or_else(|e| panic!("session refused: {e}"));
    let verdict = verify_read_request(
        &session,
        &bytes_in(c, "request"),
        &hex_at(c, "signatureHex"),
        parse_u64(st(c, "nowMs")).unwrap(),
    );
    expect(match verdict {
        Ok(request) => ok(json!({"agentParty": request.agent_party.to_string()})),
        Err(e) => refused(&e),
    })
}

fn run_receipt(c: &Value, _: &Ctx) -> Obj {
    let verdict = AnyReceipt::verify(
        &bytes_in(c, "canonical"),
        &sig_at(c, "signatureHex"),
        &party_at(c, "expectedParty"),
    );
    expect(match verdict {
        Ok(receipt) => ok(json!({
            "scheme": receipt.scheme(),
            "outcome": receipt.receipt().outcome.as_str(),
        })),
        Err(e) => refused(&e),
    })
}

fn run_head_claim(c: &Value, _: &Ctx) -> Obj {
    let verdict = HeadClaim::verify(
        &bytes_in(c, "canonical"),
        &sig_at(c, "signatureHex"),
        network(c),
        &party_at(c, "expectedParty"),
    );
    expect(match verdict {
        Ok(claim) => ok(json!({"head": head_view(claim.head)})),
        Err(e) => refused(&e),
    })
}

fn run_kind(c: &Value, _: &Ctx) -> Obj {
    let kind = OperationKind::parse(st(c, "name")).unwrap();
    fields(json!({
        "transactionOperation": kind.transaction_operation(),
        "class": kind.class().as_str(),
        "unwind": kind.is_unwind(),
        "lp": kind.requires_lp(),
        "multicallSub": kind.is_multicall_sub(),
        "selfTransferable": kind.is_self_transferable(),
    }))
}

fn run_tx_op(c: &Value, _: &Ctx) -> Obj {
    let value = i32::try_from(c["value"].as_i64().unwrap()).unwrap();
    one(
        "kind",
        OperationKind::from_transaction_operation(value).map(OperationKind::name),
    )
}

fn verified_stub(bytes: &[u8], signer: ControllerId, verified_at: UtcTime) -> VerifiedCommand {
    let outcome = ProofOutcome {
        sign_count: None,
        backup_eligible: false,
        backup_state: false,
    };
    VerifiedCommand::stub(bytes, signer, outcome, verified_at)
}

fn rules_of(c: &Value, ctx: &Ctx) -> Rules {
    match st(c, "rules") {
        "default" => DEFAULT_LINKED_RULES,
        id => PolicyDocument::decode(ctx.doc(id)).unwrap().rules,
    }
}

fn run_decision(c: &Value, ctx: &Ctx) -> Obj {
    let h = &ctx.decision;
    let agent = party_at(h, "agentParty");
    let cc = instrument_from(&h["cc"]);
    let signer = ControllerId::parse(st(h, "commandSigner")).unwrap();
    let platform = match st(c, "platform") {
        "pinned" => PlatformParties {
            fee: Some(party_at(h, "feeParty")),
            prepaid_traffic: Some(party_at(h, "prepaidTrafficParty")),
        },
        _ => PlatformParties::NONE,
    };
    let mut usage = UsageTotals::new();
    for u in items(c, "usage") {
        usage.insert(
            UsageBucket::parse(st(u, "bucket")).unwrap(),
            &instrument_from(u),
            Amount::parse(st(u, "amount")).unwrap(),
        );
    }
    let rules = rules_of(c, ctx);
    let now = time(c, "now");
    let eval = EvalContext {
        agent_party: &agent,
        cc: &cc,
        platform: &platform,
        now,
        usage: &usage,
    };
    let verified_at = c.get("verifiedAt").map_or(now, |_| time(c, "verifiedAt"));
    let command = c
        .get("command")
        .and_then(Value::as_str)
        .map(|text| verified_stub(text.as_bytes(), signer, verified_at));
    let decision = evaluate(&rules, &eval, &intent_from(&c["intent"]), command.as_ref());
    let deltas: Vec<Value> = decision
        .usage
        .iter()
        .map(|u| {
            json!({
                "bucket": u.bucket.as_str(),
                "admin": u.instrument.admin.to_string(),
                "id": u.instrument.id.as_str(),
                "amount": u.amount.to_string(),
            })
        })
        .collect();
    expect(json!({
        "allowed": decision.allowed,
        "reason": decision.reason.code_name(),
        "reasonCode": decision.reason.as_u16(),
        "class": decision.class.as_str(),
        "usage": deltas,
    }))
}

/// One controller at `now`; it signs in only while it holds a capability in effect.
fn standing(state: &ControlState, id: &ControllerId, now: UtcTime) -> Value {
    let record = state.controller(id);
    let capabilities = record.map_or(CapabilitySet::EMPTY, |r| r.capabilities_at(now));
    json!({
        "at": now.to_string(),
        "recorded": state.is_recorded_controller(id),
        "capabilities": caps_view(capabilities),
        "pending": record.and_then(|r| r.pending_at(now)).map_or(Value::Null, pending_view),
        "signsIn": !capabilities.is_empty(),
    })
}

fn run_standing(c: &Value, ctx: &Ctx) -> Obj {
    let id = ControllerId::parse(st(c, "controllerId")).unwrap();
    expect(either(ctx.state(c).map(|state| {
        let probes: Vec<Value> = items(c, "probeTimes")
            .iter()
            .map(|t| standing(&state, &id, UtcTime::parse(t.as_str().unwrap()).unwrap()))
            .collect();
        ok(json!({"standing": probes}))
    })))
}

fn run_package_check(c: &Value, ctx: &Ctx) -> Obj {
    let rules = rules_of(c, ctx);
    let used: BTreeSet<PackageId> = items(c, "packages")
        .iter()
        .map(|p| PackageId::parse(p.as_str().unwrap()).unwrap())
        .collect();
    let (refused, reason) = match check_packages(&rules, &used) {
        Ok(()) => (None, ReasonCode::Allowed),
        Err(id) => (Some(id.to_string()), ReasonCode::PackageNotAllowed),
    };
    expect(json!({
        "allowed": refused.is_none(),
        "refused": refused,
        "reason": reason.code_name(),
        "reasonCode": reason.as_u16(),
    }))
}

fn challenge_view(c: &ControllerChallenge) -> Value {
    json!({
        "network": c.network.as_str(),
        "rpc": c.rpc.as_str(),
        "controllerId": c.controller_id.to_string(),
        "agentParty": c.agent_party.to_string(),
        "nonce": c.nonce.to_string(),
        "issuedAt": c.issued_at.to_string(),
        "expiresAt": c.expires_at.to_string(),
    })
}

fn run_challenge(c: &Value, _: &Ctx) -> Obj {
    let bytes = bytes_in(c, "canonical");
    let verdict = match st(c, "function") {
        "decode" => ControllerChallenge::decode(&bytes).map_err(ChallengeError::Canon),
        "check_challenge" => {
            let e = &c["expected"];
            let expected = ExpectedChallenge {
                network: network(e),
                rpc: RpId::parse(st(e, "rpc")).unwrap(),
                controller_id: ControllerId::parse(st(e, "controllerId")).unwrap(),
                agent_party: party_at(e, "agentParty"),
            };
            check_challenge(&bytes, &expected, time(c, "now"))
                .map(|checked| checked.challenge().clone())
        }
        "verify_challenge_proof" => {
            let envelope = ProofEnvelope::decode(&bytes_in(c, "envelope")).unwrap();
            let key: [u8; 32] = hex_at(c, "publicKeyHex").try_into().unwrap();
            verify_challenge_proof(&bytes, &envelope, &key)
        }
        other => panic!("unknown challenge function {other}"),
    };
    expect(match verdict {
        Ok(challenge) => ok(json!({"challenge": challenge_view(&challenge)})),
        Err(e) => refused(&e),
    })
}

// ---------------------------------------------------------------------------
// Generation and checking

fn at_path<'a>(root: &'a Value, path: &str) -> &'a Value {
    path.split('/').fold(root, |v, key| &v[key])
}

fn at_path_mut<'a>(root: &'a mut Value, path: &str) -> &'a mut Value {
    path.split('/').fold(root, |v, key| &mut v[key])
}

fn generate() -> Value {
    let mut root = inputs();
    let ctx = Ctx::new(&root);
    for (path, run) in SECTIONS {
        let cases = at_path_mut(&mut root, path)
            .as_array_mut()
            .unwrap_or_else(|| panic!("section {path} missing"));
        for case in cases.iter_mut() {
            let derived = run(case, &ctx);
            case.as_object_mut().unwrap().extend(derived);
        }
    }
    root
}

fn check_fixture(root: &Value) {
    let ctx = Ctx::new(root);
    for (path, run) in SECTIONS {
        let cases = at_path(root, path).as_array().unwrap();
        assert!(!cases.is_empty(), "section {path} is empty");
        for case in cases {
            for (key, value) in run(case, &ctx) {
                assert_eq!(
                    case.get(&key),
                    Some(&value),
                    "{path} {} {key}",
                    case.get("name").unwrap_or(&Value::Null)
                );
            }
        }
    }
}

fn first_difference(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys().chain(y.keys()) {
                let inner = format!("{path}/{key}");
                match (x.get(key), y.get(key)) {
                    (Some(p), Some(q)) => {
                        if let Some(found) = first_difference(p, q, &inner) {
                            return Some(found);
                        }
                    }
                    _ => return Some(inner),
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path} (length {} vs {})", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_difference(p, q, &format!("{path}/{i}")))
        }
        _ => (a != b).then(|| path.to_owned()),
    }
}

#[derive(Serialize)]
struct Rendered<'a> {
    schema: &'a Value,
    regenerate: &'a Value,
    #[serde(flatten)]
    rest: &'a Obj,
}

fn render(root: &Value) -> String {
    let mut rest = root.as_object().unwrap().clone();
    let schema = rest.remove("schema").unwrap();
    let regenerate = rest.remove("regenerate").unwrap();
    let rendered = Rendered {
        schema: &schema,
        regenerate: &regenerate,
        rest: &rest,
    };
    let mut out = serde_json::to_string_pretty(&rendered).unwrap();
    out.push('\n');
    out
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

#[test]
fn vectors_match_the_fixture() {
    let text = std::fs::read_to_string(fixture_path())
        .unwrap_or_else(|e| panic!("{FIXTURE}: {e}; regenerate with `{REGENERATE}`"));
    let fixture: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fixture["schema"], SCHEMA);
    check_fixture(&fixture);
    let generated = generate();
    if let Some(path) = first_difference(&fixture, &generated, "") {
        panic!(
            "vectors changed at {path}: regenerate with `{REGENERATE}` only as a coordinated change"
        );
    }
    assert!(
        text == render(&generated),
        "the fixture is not in the generated layout"
    );
}

#[test]
#[ignore = "rewrites the fixture"]
fn vectors_print() {
    let text = render(&generate());
    std::fs::write(fixture_path(), &text).unwrap();
    println!("wrote {} ({} bytes)", fixture_path().display(), text.len());
}

// ---------------------------------------------------------------------------
// Material: keys, controllers, chains and documents

fn agent(n: u8) -> PartyId {
    let key = testkit::ed_key(n).verifying_key().to_bytes();
    PartyId::parse(&format!("agent-{n}::{}", namespace_for_key(&key))).unwrap()
}

fn named_party(hint: &str, key: u8) -> PartyId {
    PartyId::parse(&format!("{hint}::{}", namespace_for_key(&[key; 32]))).unwrap()
}

fn alice() -> PartyId {
    named_party("alice", 3)
}

fn bob() -> PartyId {
    named_party("bob", 4)
}

fn lp_a() -> PartyId {
    named_party("lp-a", 5)
}

fn lp_b() -> PartyId {
    named_party("lp-b", 6)
}

fn fee_party() -> PartyId {
    named_party("fee", 7)
}

fn cc_ref() -> InstrumentRef {
    InstrumentRef {
        admin: named_party("DSO", 1),
        id: InstrumentId::parse("Amulet").unwrap(),
    }
}

fn usdc_ref() -> InstrumentRef {
    InstrumentRef {
        admin: named_party("Bank", 2),
        id: InstrumentId::parse("USDCx").unwrap(),
    }
}

fn eth() -> EthAddress {
    EthAddress::parse("0x52908400098527886e0f7030069857d2e4169ee7").unwrap()
}

fn amount(s: &str) -> Amount {
    Amount::parse(s).unwrap()
}

fn caps(list: &[Capability]) -> CapabilitySet {
    CapabilitySet::of(list)
}

fn label(s: &str) -> Label {
    Label::parse(s).unwrap()
}

fn ed_public(start: u8) -> [u8; 32] {
    testkit::ed_key(start).verifying_key().to_bytes()
}

fn ed_controller(start: u8, name: &str, capabilities: CapabilitySet) -> ControllerDescriptor {
    ControllerDescriptor::new(
        ControllerKey::Ed25519(ed_public(start)),
        label(name),
        capabilities,
    )
    .unwrap()
}

fn local_passkey(capabilities: CapabilitySet) -> ControllerDescriptor {
    let spki = testkit::p256_spki(&testkit::p256_key_from(0x51));
    let key = WebAuthnKey::new(
        &spki,
        testkit::seed(0xd1).to_vec(),
        RpId::parse("localhost").unwrap(),
        vec![Origin::parse(LOCAL_ORIGIN).unwrap()],
        BackupPolicy::DeviceBound,
    )
    .unwrap();
    ControllerDescriptor::new(
        ControllerKey::WebAuthn(key),
        label("Local passkey"),
        capabilities,
    )
    .unwrap()
}

fn two_origin_passkey(capabilities: CapabilitySet) -> ControllerDescriptor {
    let spki = testkit::p256_spki(&testkit::p256_key());
    let origins = vec![
        Origin::parse("https://app.silvana.link").unwrap(),
        Origin::parse("https://silvana.link").unwrap(),
    ];
    let key = WebAuthnKey::new(
        &spki,
        testkit::seed(0xc1).to_vec(),
        RpId::parse("silvana.link").unwrap(),
        origins,
        BackupPolicy::Any,
    )
    .unwrap();
    ControllerDescriptor::new(
        ControllerKey::WebAuthn(key),
        label("Laptop passkey"),
        capabilities,
    )
    .unwrap()
}

fn descriptor(
    purpose: LinkPurpose,
    nonce: u8,
    controllers: Vec<ControllerDescriptor>,
) -> LinkDescriptor {
    LinkDescriptor::new(
        Network::Devnet,
        purpose,
        OwnerAccount::new(42).unwrap(),
        Some(testkit::agent_party()),
        Nonce16([nonce; 16]),
        at(7 * DAY),
        controllers,
    )
    .unwrap()
}

fn add_local_descriptor() -> LinkDescriptor {
    descriptor(
        LinkPurpose::AddController,
        0x33,
        vec![local_passkey(caps(&[Capability::Read]))],
    )
}

fn party_descriptor() -> LinkDescriptor {
    descriptor(
        LinkPurpose::Link,
        0x6b,
        vec![
            two_origin_passkey(caps(&[Capability::Policy, Capability::Direct])),
            testkit::ed25519_controller(caps(&[Capability::Policy, Capability::Read])),
        ],
    )
}

fn relink_descriptor() -> LinkDescriptor {
    descriptor(
        LinkPurpose::Link,
        0x44,
        vec![ed_controller(0x81, "Backup key", CapabilitySet::ALL)],
    )
}

fn forge(
    state: &ControlState,
    created_at: UtcTime,
    effective_at: UtcTime,
    body: EntryBody,
) -> SignedEntry {
    let entry = LogEntry {
        network: state.network(),
        agent_party: state.agent_party().clone(),
        agent_key: *state.agent_key(),
        seq: state.next_seq().unwrap(),
        prev: state.head().map_or(Hash32([0; 32]), |h| h.hash),
        created_at,
        effective_at,
        body,
    };
    sign_entry(entry, &testkit::agent_key())
}

fn sign_entry(entry: LogEntry, key: &ed25519_dalek::SigningKey) -> SignedEntry {
    let bytes = entry.encode().unwrap();
    let signature = key.sign(&bytes).to_bytes();
    SignedEntry::from_parts(entry, bytes, signature)
}

fn accept_body(
    hash: Hash32,
    version: u64,
    signer: ControllerId,
    proof: ControllerProof,
) -> EntryBody {
    EntryBody::AcceptPolicy(AcceptPolicyBody {
        policy_hash: hash,
        policy_version: version,
        signed_by: signer,
        proof,
    })
}

fn ed_proof(start: u8, payload: &[u8]) -> ControllerProof {
    ControllerProof::Ed25519 {
        signature: testkit::ed_key(start).sign(payload).to_bytes(),
    }
}

fn add_body(
    owner: u64,
    controller: ControllerDescriptor,
    descriptor: Option<DescriptorRef>,
) -> EntryBody {
    EntryBody::AddController(AddControllerBody {
        owner_account: OwnerAccount::new(owner).unwrap(),
        descriptor,
        controller,
    })
}

fn link_body(d: &LinkDescriptor, invite: bool, commitment: Commitment) -> EntryBody {
    EntryBody::Link(LinkBody {
        owner_account: d.owner_account(),
        invite,
        nonce: *d.nonce(),
        expires_at: d.expires_at(),
        commitment,
        controllers: d.controllers().to_vec(),
    })
}

fn local_assertion(payload: &[u8], flags: u8) -> WebAuthnAssertion {
    let challenge = sha256(payload);
    let client_data_json = testkit::client_data(&challenge, LOCAL_ORIGIN);
    let authenticator_data = testkit::auth_data("localhost", flags, 1);
    let signature = testkit::sign_webauthn(
        &testkit::p256_key_from(0x51),
        &authenticator_data,
        &client_data_json,
    );
    WebAuthnAssertion {
        authenticator_data,
        client_data_json,
        signature,
    }
}

fn rules_doc(rules: Rules) -> PolicyDocument {
    PolicyDocument {
        network: Network::Devnet,
        agent_party: testkit::agent_party(),
        version: 1,
        prev_policy_hash: Hash32([0; 32]),
        issued_at: at(0),
        expires_at: at(3600),
        not_before: None,
        rules,
    }
}

fn limit(instrument: PolicyInstrument, value: &str) -> LimitItem {
    LimitItem {
        instrument,
        amount: amount(value),
    }
}

fn rich_rules() -> Rules {
    let mut r = DEFAULT_LINKED_RULES;
    r.delays.trading_secs = 600;
    r.delays.other_secs = 3600;
    r.maintenance.traffic_cc_per_day = Some(amount("25.5"));
    r.trading.lp_enabled = false;
    r.trading.markets = Allowlist {
        any: false,
        items: vec![
            MarketId::parse("CC/USDCx").unwrap(),
            MarketId::parse("cETH/USDCx").unwrap(),
        ],
    };
    r.trading.instruments = Allowlist {
        any: false,
        items: vec![PolicyInstrument::Token(usdc_ref()), PolicyInstrument::Cc],
    };
    r.trading.per_trade = Limits {
        unlisted: Unlisted::Deny,
        items: vec![
            limit(PolicyInstrument::Token(usdc_ref()), "500.25"),
            limit(PolicyInstrument::Cc, "1000"),
        ],
    };
    r.trading.per_day = Limits {
        unlisted: Unlisted::Unlimited,
        items: vec![limit(PolicyInstrument::Cc, "10000")],
    };
    r.transfers.recipients = Allowlist {
        any: false,
        items: vec![alice(), bob()],
    };
    r.transfers.eth_destinations = Allowlist {
        any: false,
        items: vec![eth()],
    };
    r.transfers.per_transfer = Limits {
        unlisted: Unlisted::Deny,
        items: vec![limit(PolicyInstrument::Cc, "100")],
    };
    r.locks.per_lock = Limits {
        unlisted: Unlisted::Unlimited,
        items: vec![limit(PolicyInstrument::Cc, "50")],
    };
    r.usdcx.enabled = false;
    r.fees.cc_per_consent = Some(amount("2.5"));
    r.fees.cc_per_day = Some(amount("20"));
    r
}

fn policy_example() -> PolicyDocument {
    PolicyDocument {
        not_before: Some(at(600)),
        expires_at: at(DAY),
        ..rules_doc(rich_rules())
    }
}

fn default_document() -> PolicyDocument {
    PolicyDocument {
        expires_at: at(DAY),
        ..rules_doc(DEFAULT_LINKED_RULES)
    }
}

/// A selection of three packages.
fn packages_example() -> PolicyDocument {
    let mut rules = DEFAULT_LINKED_RULES;
    rules.packages = selected(&["splice-amulet", "atomic-dvp", "registry"]);
    PolicyDocument {
        expires_at: at(DAY),
        ..rules_doc(rules)
    }
}

/// Two scopes: 0x84 with swaps in both directions and transfers, bounded per pair and to
/// two LPs; 0x82 with no command at all.
fn scopes_example() -> PolicyDocument {
    let usdc_for_cc = pair(
        PolicyInstrument::Token(usdc_ref()),
        PolicyInstrument::Cc,
        "6",
        "150",
    );
    let mut rules = DEFAULT_LINKED_RULES;
    rules.scopes = scopes(vec![
        scope(
            testkit::ed25519_id(0x84),
            &[CommandKind::Swap, CommandKind::TransferCc],
            vec![cc_for_usdc(), usdc_for_cc],
            vec![lp_a(), lp_b()],
        ),
        scope(testkit::ed25519_id(0x82), &[], vec![], vec![]),
    ]);
    PolicyDocument {
        expires_at: at(DAY),
        ..rules_doc(rules)
    }
}

fn command_at(
    head: HeadRef,
    party: &PartyId,
    id: &str,
    issued_at: UtcTime,
    secs: i64,
    op: CommandOp,
    summary: &str,
) -> Command {
    Command {
        network: Network::Devnet,
        agent_party: party.clone(),
        command_id: Uuid7::parse(id).unwrap(),
        log_seq: head.seq,
        log_head: head.hash,
        issued_at,
        expires_at: issued_at.checked_add_secs(secs).unwrap(),
        op,
        summary: Text::parse(summary).unwrap(),
    }
}

const CMD_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
const TOKEN_CMD_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8058";
const BURN_CMD_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8059";

fn pay_alice(value: &str) -> CommandOp {
    CommandOp::TransferCc {
        receiver: alice(),
        amount: amount(value),
        description: Some(Text::parse("rent").unwrap()),
        settlement_proposal_id: None,
    }
}

fn send_bob_usdc() -> CommandOp {
    CommandOp::TransferToken {
        receiver: bob(),
        instrument: usdc_ref(),
        amount: amount("2.25"),
        reference: Some(Text::parse("invoice 7").unwrap()),
    }
}

fn burn() -> CommandOp {
    CommandOp::UsdcxBurn {
        amount: amount("100"),
        destination: eth(),
    }
}

const SWAP_CMD_ID: &str = "01890a5d-ac96-774b-bcce-b302099a805a";
const SWAP_BUY_CMD_ID: &str = "01890a5d-ac96-774b-bcce-b302099a805b";

fn swap(
    sell: InstrumentRef,
    sold: &str,
    buy: InstrumentRef,
    minimum: &str,
    fee_cap: &str,
) -> CommandOp {
    CommandOp::Swap {
        sell,
        sell_amount: amount(sold),
        buy,
        min_receive: amount(minimum),
        max_fee_cc: amount(fee_cap),
    }
}

/// Sell 1000 CC for at least 150 USDCx, at most 2 CC in fees.
fn sell_cc() -> CommandOp {
    swap(cc_ref(), "1000", usdc_ref(), "150", "2")
}

/// Sell 150 USDCx for at least 990 CC net of fees, at most 2 CC in fees.
fn buy_cc() -> CommandOp {
    swap(usdc_ref(), "150", cc_ref(), "990", "2")
}

/// The twelve-entry log covering every kind, with the state after each entry. It is timed in
/// units of the devnet delay `D`: linked at T0, in effect at D, changes at D+ and 2D+.
struct Main {
    chain: Chain,
    states: Vec<ControlState>,
    v1: Vec<u8>,
    v2: Vec<u8>,
    v3: Vec<u8>,
}

impl Main {
    fn head(&self, n: usize) -> HeadRef {
        self.states[n - 1].head().unwrap()
    }
}

/// Pushes `pending` and records the state after it.
fn step(chain: &mut Chain, states: &mut Vec<ControlState>, pending: &PendingEntry) {
    chain.push(pending);
    states.push(chain.state.clone());
}

fn main_chain() -> Main {
    use Capability::{Direct, Policy, Read};
    let d = DEVNET_DELAY;
    let mut chain = Chain::linked();
    let mut states = vec![chain.state.clone()];
    let add = add_local_descriptor();
    let pending = chain
        .builder()
        .add_controller_from_code(
            &add.encode().unwrap(),
            &add.commitment().unwrap(),
            &testkit::rp(),
            at(d),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x81),
            label("Backup key"),
            caps(&[Policy]),
            at(d + 10),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .set_capabilities(
            &testkit::ed25519_id(0x21),
            caps(&[Direct, Read]),
            false,
            at(d + 20),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let v1 = testkit::policy_doc(&chain.state, 1, at(d + 30), DEFAULT_LINKED_RULES);
    let v1_pending = chain
        .builder()
        .accept_policy(
            &v1.encode().unwrap(),
            &testkit::passkey_envelope(&v1.encode().unwrap()),
            at(d + 30),
        )
        .unwrap();
    step(&mut chain, &mut states, &v1_pending);
    // 0x81 signs the moment its add takes effect.
    let v2 = testkit::policy_doc(&chain.state, 2, at(2 * d + 10), testkit::undirected_rules());
    let v2_bytes = v2.encode().unwrap();
    let pending = chain
        .builder()
        .accept_policy(
            &v2_bytes,
            &testkit::ed25519_envelope(0x81, &v2_bytes),
            at(2 * d + 10),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let v3 = testkit::policy_doc(&chain.state, 3, at(2 * d + 20), DEFAULT_LINKED_RULES);
    let v3_bytes = v3.encode().unwrap();
    let pending = chain
        .builder()
        .accept_policy(
            &v3_bytes,
            &testkit::passkey_envelope(&v3_bytes),
            at(2 * d + 20),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let local_id = *local_passkey(CapabilitySet::ALL).id();
    let pending = chain
        .builder()
        .remove_controller(&local_id, false, at(2 * d + 30))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x82),
            label("Spare key"),
            caps(&[Read]),
            at(2 * d + 40),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .remove_controller(&testkit::ed25519_id(0x82), false, at(2 * d + 50))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x83),
            label("Second key"),
            caps(&[Policy, Read]),
            at(2 * d + 60),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .set_capabilities(
            &testkit::ed25519_id(0x83),
            caps(&[Read]),
            false,
            at(2 * d + 70),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    Main {
        chain,
        states,
        v1: v1.encode().unwrap(),
        v2: v2_bytes,
        v3: v3_bytes,
    }
}

/// Linked, both controllers removed by force, then linked again once the removals apply.
fn relink_chain() -> (Chain, Vec<ControlState>) {
    let d = DEVNET_DELAY;
    let mut chain = Chain::linked();
    let mut states = vec![chain.state.clone()];
    let pending = chain
        .builder()
        .remove_controller(&testkit::passkey_id(), true, at(d + 10))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .remove_controller(&testkit::ed25519_id(0x21), true, at(d + 20))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let relink = relink_descriptor();
    let pending = chain
        .builder()
        .link(
            &relink.encode().unwrap(),
            &relink.commitment().unwrap(),
            &testkit::rp(),
            false,
            at(2 * d + 20),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    (chain, states)
}

/// A policy for `state`'s agent on `state`'s network that chains onto its latest one.
fn policy_on(
    state: &ControlState,
    version: u64,
    issued_at: UtcTime,
    rules: Rules,
) -> PolicyDocument {
    PolicyDocument {
        network: state.network(),
        ..testkit::policy_doc(state, version, issued_at, rules)
    }
}

/// Accepts `doc` at its `issued_at`, signed by Ed25519 `signer` or, for 0, by the passkey.
fn accept_doc(
    chain: &mut Chain,
    states: &mut Vec<ControlState>,
    signer: u8,
    doc: &PolicyDocument,
) -> Vec<u8> {
    let bytes = doc.encode().unwrap();
    let envelope = match signer {
        0 => testkit::passkey_envelope(&bytes),
        start => testkit::ed25519_envelope(start, &bytes),
    };
    let pending = chain
        .builder()
        .accept_policy(&bytes, &envelope, doc.issued_at)
        .unwrap();
    step(chain, states, &pending);
    bytes
}

/// A mainnet log timed by the mainnet delay `M`: an add, a withdrawn add, a narrowing, a
/// removal and a package widening, with the state after each entry and both policies.
fn mainnet_chain() -> (Chain, Vec<ControlState>, Vec<Vec<u8>>) {
    use Capability::{Direct, Policy, Read};
    let m = MAINNET_DELAY;
    let mut chain = Chain::linked_on(Network::Mainnet);
    let mut states = vec![chain.state.clone()];
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x81),
            label("Backup key"),
            caps(&[Policy, Direct]),
            at(m + 10),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x82),
            label("Spare key"),
            caps(&[Read]),
            at(m + 20),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let pending = chain
        .builder()
        .remove_controller(&testkit::ed25519_id(0x82), false, at(m + 30))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let v1 = policy_on(&chain.state, 1, at(2 * m + 20), mainnet_narrow_rules());
    let v1 = accept_doc(&mut chain, &mut states, 0x21, &v1);
    let pending = chain
        .builder()
        .remove_controller(&testkit::ed25519_id(0x81), false, at(2 * m + 30))
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let v2 = policy_on(&chain.state, 2, at(2 * m + 40), mainnet_wide_packages());
    let v2 = accept_doc(&mut chain, &mut states, 0x21, &v2);
    (chain, states, vec![v1, v2])
}

/// The mainnet narrowing: two packages selected, and 0x81 scoped to swaps.
fn mainnet_narrow_rules() -> Rules {
    let mut rules = DEFAULT_LINKED_RULES;
    rules.packages = selected(&["splice-amulet", "atomic-dvp"]);
    rules.scopes = vec![scope(
        testkit::ed25519_id(0x81),
        &[CommandKind::Swap],
        vec![],
        vec![],
    )];
    rules
}

/// The mainnet narrowing with every package allowed again: a packages widening only.
fn mainnet_wide_packages() -> Rules {
    let mut rules = mainnet_narrow_rules();
    rules.packages = Allowlist::ANY;
    rules
}

/// The mainnet narrowing with 0x81's scope dropped: a scopes widening only.
fn mainnet_wide_scopes() -> Rules {
    let mut rules = mainnet_narrow_rules();
    rules.scopes = Vec::new();
    rules
}

/// A package id, deterministic per name.
fn package(name: &str) -> PackageId {
    PackageId(sha256(name.as_bytes()))
}

/// A package selection of the named packages, in ascending order.
fn selected(names: &[&str]) -> Allowlist<PackageId> {
    let mut items: Vec<PackageId> = names.iter().map(|name| package(name)).collect();
    items.sort();
    Allowlist { any: false, items }
}

fn scope(
    controller: ControllerId,
    commands: &[CommandKind],
    mut pairs: Vec<ScopePair>,
    mut lps: Vec<PartyId>,
) -> CommandScope {
    pairs.sort_by(|a, b| (&a.sell, &a.buy).cmp(&(&b.sell, &b.buy)));
    lps.sort();
    CommandScope {
        controller,
        commands: commands.to_vec(),
        pairs,
        lps,
    }
}

/// Scopes in the controller order a policy lists them in.
fn scopes(mut list: Vec<CommandScope>) -> Vec<CommandScope> {
    list.sort_by_key(|s| s.controller);
    list
}

fn pair(
    sell: PolicyInstrument,
    buy: PolicyInstrument,
    min_price: &str,
    max_sell: &str,
) -> ScopePair {
    ScopePair {
        sell,
        buy,
        min_price: amount(min_price),
        max_sell: amount(max_sell),
    }
}

/// The scoped pair: CC sold for USDCx at 0.15 or better, at most 1000 CC a command.
fn cc_for_usdc() -> ScopePair {
    pair(
        PolicyInstrument::Cc,
        PolicyInstrument::Token(usdc_ref()),
        "0.15",
        "1000",
    )
}

/// Devnet: Ed25519 0x84 added with `direct`, scoped to one swap pair and LP, then widened and
/// emptied; `not_before` holds the same narrowing for 2D+600 instead.
struct ScopedChain {
    chain: Chain,
    states: Vec<ControlState>,
    docs: Vec<Vec<u8>>,
    not_before: (SignedEntry, ControlState, Vec<u8>),
}

fn scoped_rules(commands: &[CommandKind]) -> Rules {
    let mut rules = DEFAULT_LINKED_RULES;
    rules.scopes = vec![scope(
        testkit::ed25519_id(0x84),
        commands,
        vec![cc_for_usdc()],
        vec![lp_a()],
    )];
    rules
}

fn scoped_chain() -> ScopedChain {
    let d = DEVNET_DELAY;
    let mut chain = Chain::linked();
    let mut states = vec![chain.state.clone()];
    let pending = chain
        .builder()
        .add_ed25519(
            ed_public(0x84),
            label("Swap key"),
            caps(&[Capability::Direct]),
            at(d),
        )
        .unwrap();
    step(&mut chain, &mut states, &pending);
    let v1 = policy_on(
        &chain.state,
        1,
        at(d + 10),
        scoped_rules(&[CommandKind::Swap]),
    );
    let v1 = accept_doc(&mut chain, &mut states, 0, &v1);
    let after_v1 = chain.state.clone();
    let wider = scoped_rules(&[CommandKind::Swap, CommandKind::TransferCc]);
    let v2 = policy_on(&chain.state, 2, at(d + 20), wider);
    let v2 = accept_doc(&mut chain, &mut states, 0, &v2);
    let v3 = policy_on(&chain.state, 3, at(d + 30), scoped_rules(&[]));
    let v3 = accept_doc(&mut chain, &mut states, 0, &v3);
    let later = PolicyDocument {
        not_before: Some(at(2 * d + 600)),
        ..policy_on(&after_v1, 2, at(d + 20), scoped_rules(&[]))
    };
    let bytes = later.encode().unwrap();
    let pending = EntryBuilder::new(&after_v1)
        .accept_policy(&bytes, &testkit::passkey_envelope(&bytes), at(d + 20))
        .unwrap();
    let not_before = (
        testkit::sign_pending(&pending),
        pending.state_after().clone(),
        bytes,
    );
    ScopedChain {
        chain,
        states,
        docs: vec![v1, v2, v3],
        not_before,
    }
}

/// Devnet: two packages selected at D+100, any package at D+200 (due at 2D+200), and one
/// package at D+300, which cancels that widening.
fn packages_chain() -> (Chain, Vec<ControlState>, Vec<Vec<u8>>) {
    let d = DEVNET_DELAY;
    let mut chain = Chain::linked();
    let mut states = vec![chain.state.clone()];
    let mut docs = Vec::new();
    let rules = |packages: Allowlist<PackageId>| Rules {
        packages,
        ..DEFAULT_LINKED_RULES
    };
    for (version, issued, packages) in [
        (1, at(d + 100), selected(&["splice-amulet", "atomic-dvp"])),
        (2, at(d + 200), Allowlist::ANY),
        (3, at(d + 300), selected(&["splice-amulet"])),
    ] {
        let doc = policy_on(&chain.state, version, issued, rules(packages));
        docs.push(accept_doc(&mut chain, &mut states, 0, &doc));
    }
    (chain, states, docs)
}

/// An agent linked at T0 with the given controllers, as one signed entry.
fn linked_agent(n: u8, controllers: Vec<ControllerDescriptor>) -> (SignedEntry, ControlState) {
    let party = agent(n);
    let state = ControlState::empty(Network::Devnet, party.clone(), ed_public(n));
    let d = LinkDescriptor::new(
        Network::Devnet,
        LinkPurpose::Link,
        OwnerAccount::new(42).unwrap(),
        Some(party),
        Nonce16([n; 16]),
        at(7 * DAY),
        controllers,
    )
    .unwrap();
    let pending = EntryBuilder::new(&state)
        .link(
            &d.encode().unwrap(),
            &d.commitment().unwrap(),
            &testkit::rp(),
            false,
            at(0),
        )
        .unwrap();
    let signed = pending
        .finish(sign_agent(&testkit::ed_key(n), pending.signable()))
        .unwrap();
    (signed, pending.state_after().clone())
}

fn session_key_spki() -> Vec<u8> {
    testkit::p256_spki(&testkit::p256_key_from(0x61))
}

fn session_request(agents: &[u8], issued_at: UtcTime, secs: i64) -> ReadSessionRequest {
    ReadSessionRequest::new(
        Network::Devnet,
        testkit::passkey_id(),
        parse_p256_spki(&session_key_spki()).unwrap(),
        Nonce16([0x77; 16]),
        issued_at,
        issued_at.checked_add_secs(secs).unwrap(),
        agents.iter().map(|n| agent(*n)).collect(),
    )
}

fn p1363(start: u8, bytes: &[u8]) -> Vec<u8> {
    let signature: P256Signature = testkit::p256_key_from(start).sign(bytes);
    signature.to_bytes().to_vec()
}

fn ms(t: UtcTime) -> u64 {
    u64::try_from(t.unix_secs()).unwrap() * 1000
}

fn read_request(
    session: &ReadSessionRequest,
    method: &str,
    party: PartyId,
    limit: u64,
    issued_ms: u64,
) -> ReadRequest {
    ReadRequest {
        network: Network::Devnet,
        session_id: session.session_id().unwrap(),
        method: Token::parse(method).unwrap(),
        agent_party: party,
        range_offset: 0,
        range_limit: limit,
        issued_at_ms: issued_ms,
        nonce: Nonce16([0x88; 16]),
    }
}

#[derive(Default)]
struct Pool {
    entries: Vec<Value>,
    docs: Vec<Value>,
}

impl Pool {
    fn entry(&mut self, id: &str, entry: &SignedEntry) -> String {
        self.row(id, entry.bytes(), entry.signature())
    }

    fn row(&mut self, id: &str, bytes: &[u8], signature: &[u8; 64]) -> String {
        assert!(
            self.entries.iter().all(|e| e["id"] != id),
            "duplicate row {id}"
        );
        let row = json!({"id": id, "signatureHex": hex::encode(signature)});
        self.entries.push(with_bytes(row, "canonical", bytes));
        id.to_owned()
    }

    fn doc(&mut self, id: &str, bytes: &[u8]) -> String {
        self.doc_keyed(id, Hash32(sha256(bytes)), bytes)
    }

    fn doc_keyed(&mut self, id: &str, key: Hash32, bytes: &[u8]) -> String {
        if !self.docs.iter().any(|d| d["id"] == id) {
            let doc = json!({"id": id, "keyHex": key.to_string()});
            self.docs.push(with_bytes(doc, "canonical", bytes));
        }
        id.to_owned()
    }

    fn rules(&mut self, id: &str, rules: Rules) -> String {
        self.doc(id, &rules_doc(rules).encode().unwrap())
    }

    fn rules_on(&mut self, id: &str, network: Network, rules: Rules) -> String {
        let doc = PolicyDocument {
            network,
            ..rules_doc(rules)
        };
        self.doc(id, &doc.encode().unwrap())
    }
}

fn ids(prefix: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("{prefix}.{i}")).collect()
}

fn plus(mut base: Vec<String>, extra: &[&str]) -> Vec<String> {
    base.extend(extra.iter().map(|s| (*s).to_owned()));
    base
}

fn main_ids(n: usize) -> Vec<String> {
    ids("main", n)
}

fn log_case(name: &str, entries: &[String], documents: &[&str], probes: &[UtcTime]) -> Value {
    let probes: Vec<String> = probes.iter().map(ToString::to_string).collect();
    json!({
        "name": name,
        "network": "devnet",
        "agentParty": testkit::agent_party().to_string(),
        "entries": entries,
        "documents": documents,
        "probeTimes": probes,
    })
}

fn on_mainnet(mut c: Value) -> Value {
    put(&mut c, "network", "mainnet");
    c
}

/// Every chain the cases draw on, with the state after each of its entries.
struct Chains {
    main: Main,
    relink: Vec<ControlState>,
    mainnet: Vec<ControlState>,
    mainnet_docs: Vec<Vec<u8>>,
    scoped: ScopedChain,
    packages: Vec<ControlState>,
    package_docs: Vec<Vec<u8>>,
}

/// Puts each chain's rows and policy documents in the pool; the rows every devnet chain
/// shares with `main` (its link at T0) are listed once, as `main.1`.
fn chains(pool: &mut Pool) -> Chains {
    let main = main_chain();
    let (relink, relink_states) = relink_chain();
    let (mainnet, mainnet_states, mainnet_docs) = mainnet_chain();
    let scoped = scoped_chain();
    let (packages, package_states, package_docs) = packages_chain();
    for (i, entry) in main.chain.entries.iter().enumerate() {
        pool.entry(&format!("main.{}", i + 1), entry);
    }
    for (prefix, chain) in [
        ("relink", &relink),
        ("scoped", &scoped.chain),
        ("pk", &packages),
    ] {
        assert_eq!(chain.entries[0], main.chain.entries[0]);
        for (i, entry) in chain.entries.iter().enumerate().skip(1) {
            pool.entry(&format!("{prefix}.{}", i + 1), entry);
        }
    }
    for (i, entry) in mainnet.entries.iter().enumerate() {
        pool.entry(&format!("mainnet.{}", i + 1), entry);
    }
    pool.entry("scoped_nb.4", &scoped.not_before.0);
    pool.doc("main.v1", &main.v1);
    pool.doc("main.v2", &main.v2);
    pool.doc("main.v3", &main.v3);
    let docs = [
        ("mainnet", &mainnet_docs),
        ("scoped", &scoped.docs),
        ("pk", &package_docs),
    ];
    for (prefix, list) in docs {
        for (i, bytes) in list.iter().enumerate() {
            pool.doc(&format!("{prefix}.v{}", i + 1), bytes);
        }
    }
    pool.doc("scoped_nb.v2", &scoped.not_before.2);
    Chains {
        main,
        relink: relink_states,
        mainnet: mainnet_states,
        mainnet_docs,
        scoped,
        packages: package_states,
        package_docs,
    }
}

fn mutate(base: &str, from: &str, to: &str) -> String {
    assert!(base.contains(from), "{from:?} is not in {base}");
    base.replacen(from, to, 1)
}

/// Moves the indexed items under `prefix` into `order`, renumbering them in place.
fn reorder(text: &str, prefix: &str, order: &[usize]) -> String {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut blocks: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for line in text.lines() {
        let item = line.strip_prefix(prefix).and_then(|rest| {
            let pos = rest.find(['.', '='])?;
            let index = rest[..pos].parse::<usize>().ok()?;
            Some((index, rest[pos..].to_owned()))
        });
        match item {
            Some((index, tail)) => blocks.entry(index).or_default().push(tail),
            None if blocks.is_empty() => before.push(line.to_owned()),
            None => after.push(line.to_owned()),
        }
    }
    let mut out = before;
    for (new, old) in order.iter().enumerate() {
        for tail in &blocks[old] {
            out.push(format!("{prefix}{new}{tail}"));
        }
    }
    out.extend(after);
    out.join("\n") + "\n"
}

fn line_value<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("{key} not in {text}"))
}

fn set_line(text: &str, key: &str, value: &str) -> String {
    let old = format!("\n{key}={}\n", line_value(text, key));
    mutate(text, &old, &format!("\n{key}={value}\n"))
}

// ---------------------------------------------------------------------------
// Inputs

fn inputs() -> Value {
    let mut pool = Pool::default();
    let all = chains(&mut pool);
    let main = &all.main;
    let documents = documents(main);
    let mut root = Map::new();
    let mut set = |key: &str, value: Value| {
        root.insert(key.to_owned(), value);
    };
    set("schema", SCHEMA.into());
    set("regenerate", REGENERATE.into());
    set("notes", json!(NOTES));
    set("constants", constants());
    set("errorCodes", error_codes());
    set("keys", key_list());
    set("values", values_cases().into());
    set("originRpId", origin_cases().into());
    set("rpPolicy", rp_policy_cases().into());
    set("ids", id_cases().into());
    set("parties", party_cases().into());
    set("controllers", controller_cases().into());
    set("codes", code_cases());
    set("negative", negative_cases(&documents).into());
    set("documents", documents.into());
    set("webauthn", webauthn_cases().into());
    set("signCount", sign_count_cases().into());
    set("proofs", proof_cases().into());
    set("links", link_cases().into());
    set("logs", log_cases(&mut pool, main).into());
    let mut timeline = timeline_cases(&mut pool, main);
    timeline.extend(rule_timeline_cases(&mut pool, &all));
    set("timeline", timeline.into());
    let mut delays = controller_delay_cases(&mut pool, main, &all.relink);
    delays.extend(network_delay_cases(&mut pool, &all));
    set("controllerDelay", delays.into());
    set("controllerStanding", standing_cases(&all).into());
    set("extensions", extension_cases().into());
    set("comparisons", comparison_cases(&mut pool, main).into());
    set("forkEvidence", fork_cases().into());
    set("headChecks", head_check_cases(main).into());
    set("jwtClaims", jwt_cases().into());
    set("minimumRule", minimum_cases().into());
    set("widening", widening_cases(&mut pool).into());
    set(
        "policySubmissions",
        submission_cases(&mut pool, &all).into(),
    );
    set("commands", command_cases(&all).into());
    let (sessions, requests) = read_cases(&mut pool);
    set("readSessions", sessions.into());
    set("readRequests", requests.into());
    set("receipts", receipt_cases(main).into());
    set("headClaims", head_claim_cases(main).into());
    set("kinds", kind_cases());
    set("decisionContext", decision_context());
    set("decisions", decision_cases(&mut pool).into());
    set("packageChecks", package_check_cases(&mut pool).into());
    set("controllerChallenges", challenge_cases().into());
    set(
        "logPool",
        json!({"entries": pool.entries, "policyDocuments": pool.docs}),
    );
    Value::Object(root)
}

/// Every stable number a verifier reports beside the ones the cases produce.
fn error_codes() -> Value {
    let reasons: Vec<Value> = ReasonCode::ALL
        .into_iter()
        .map(|r| json!({"name": r.code_name(), "code": r.as_u16()}))
        .collect();
    let failures: Vec<Value> = CommandFailure::ALL
        .into_iter()
        .map(|f| json!({"name": f.code_name(), "code": f.code()}))
        .collect();
    let commands: Vec<Value> = [
        CommandError::WrongNetwork,
        CommandError::WrongParty,
        CommandError::UnknownController,
        CommandError::SignerLacksDirect,
        CommandError::StaleLogHead,
        CommandError::IssuedInFuture,
        CommandError::Expired,
        CommandError::TimeOverflow,
        CommandError::OutOfScope,
        CommandError::SwapPairNotAllowed,
        CommandError::SwapBelowMinPrice,
        CommandError::SwapAboveMaxSell,
        CommandError::LpNotAllowed,
    ]
    .iter()
    .map(|e| json!({"name": e.code_name(), "code": e.code()}))
    .collect();
    json!({"reasons": reasons, "commandFailures": failures, "commands": commands})
}

fn constants() -> Value {
    let delay = |network: Network| controller_change_delay_secs(network);
    json!({
        "minDocumentBytes": MIN_DOCUMENT_BYTES,
        "maxBytes": {
            "link": MAX_LINK_BYTES,
            "proof": MAX_PROOF_BYTES,
            "control": MAX_ENTRY_BYTES,
            "policy": MAX_POLICY_BYTES,
            "command": MAX_COMMAND_BYTES,
            "readSession": MAX_READ_SESSION_BYTES,
            "readRequest": MAX_READ_REQUEST_BYTES,
            "receipt": MAX_RECEIPT_BYTES,
            "receiptV2": MAX_RECEIPT_V2_BYTES,
            "controlHead": MAX_HEAD_CLAIM_BYTES,
            "controllerChallenge": MAX_CHALLENGE_BYTES,
        },
        "maxLogEntries": MAX_LOG_ENTRIES,
        "maxAgentControllers": MAX_AGENT_CONTROLLERS,
        "controllerChangeDelaySecs": {
            "devnet": delay(Network::Devnet),
            "mainnet": delay(Network::Mainnet),
        },
        "maxPackages": MAX_PACKAGES,
        "maxScopes": MAX_SCOPES,
        "maxScopeCommands": MAX_SCOPE_COMMANDS,
        "maxScopePairs": MAX_SCOPE_PAIRS,
        "maxScopeLps": MAX_SCOPE_LPS,
        "commandKinds": CommandKind::ALL.map(CommandKind::as_str),
        "maxChallengeSecs": MAX_CHALLENGE_SECS,
        "challengeClockSkewSecs": CHALLENGE_CLOCK_SKEW_SECS,
        "maxPolicyValiditySecs": MAX_POLICY_VALIDITY_SECS,
        "maxNotBeforeSecs": MAX_NOT_BEFORE_SECS,
        "maxDelaySecs": MAX_DELAY_SECS,
        "policyClockSkewSecs": POLICY_CLOCK_SKEW_SECS,
        "maxCommandValiditySecs": MAX_COMMAND_VALIDITY_SECS,
        "commandClockSkewSecs": COMMAND_CLOCK_SKEW_SECS,
        "maxReadSessionSecs": MAX_READ_SESSION_SECS,
        "maxSessionAgents": MAX_SESSION_AGENTS,
        "readSessionClockSkewSecs": READ_SESSION_CLOCK_SKEW_SECS,
        "readRequestWindowMs": READ_REQUEST_WINDOW_MS,
        "cantonCommandPrefix": crate::command::CANTON_COMMAND_PREFIX,
        "controllerIdDomain": CONTROLLER_ID_DOMAIN,
        "p256SpkiPrefixHex": hex::encode(P256_SPKI_PREFIX),
    })
}

fn key_list() -> Value {
    let ed = |name: &str, start: u8, role: &str| {
        json!({
            "name": name,
            "type": "ed25519",
            "role": role,
            "testOnly": true,
            "seedHex": hex::encode(testkit::seed(start)),
            "publicKeyHex": hex::encode(ed_public(start)),
        })
    };
    let p256 = |name: &str, start: u8, role: &str| {
        json!({
            "name": name,
            "type": "p256",
            "role": role,
            "testOnly": true,
            "scalarHex": hex::encode(testkit::seed(start)),
            "spkiHex": hex::encode(testkit::p256_spki(&testkit::p256_key_from(start))),
        })
    };
    let mut keys: Vec<Value> = (1..=4u8)
        .map(|n| {
            let mut key = ed(&format!("agent-{n}"), n, "agent");
            put(&mut key, "party", agent(n).to_string());
            key
        })
        .collect();
    keys.extend([
        ed("recovery", 0x21, "controller"),
        ed("backup", 0x81, "controller"),
        ed("spare", 0x82, "controller"),
        ed("second", 0x83, "controller"),
        ed("other", 0x84, "controller"),
        ed("stranger", 0x99, "unknown controller"),
        ed("intruder", 0x77, "wrong agent key"),
        p256("laptop", 0x41, "passkey"),
        p256("local", 0x51, "passkey"),
        p256("session", 0x61, "read-session key"),
        p256("outsider", 0x71, "wrong key"),
    ]);
    Value::Array(keys)
}

fn strs(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn values_cases() -> Vec<Value> {
    let ns = format!("1220{}", "ab".repeat(32));
    let hex16 = "5a".repeat(16);
    let hex32 = "0f".repeat(32);
    let table: Vec<(&str, Vec<String>)> = vec![
        (
            "hex16",
            vec![
                hex16.clone(),
                hex16.to_uppercase(),
                "5a".repeat(15) + "5",
                "5a".repeat(17),
                "g".repeat(32),
                String::new(),
            ],
        ),
        (
            "hex32",
            vec![
                hex32.clone(),
                "0".repeat(64),
                hex32.to_uppercase(),
                "0f".repeat(31) + "0",
                "0f".repeat(33),
            ],
        ),
        (
            "hex64",
            vec!["ab".repeat(64), "ab".repeat(63), "AB".repeat(64)],
        ),
        (
            "u64",
            strs(&[
                "0",
                "1",
                "18446744073709551615",
                "00",
                "01",
                "18446744073709551616",
                "99999999999999999999",
                "-1",
                "+1",
                "1.0",
                " 1",
                "1e3",
                "",
            ]),
        ),
        ("bool", strs(&["true", "false", "True", "1", "yes", ""])),
        (
            "time",
            strs(&[
                "2026-11-22T12:00:00Z",
                "2020-01-01T00:00:00Z",
                "9999-12-31T23:59:59Z",
                "2024-02-29T00:00:00Z",
                "2400-02-29T12:00:00Z",
                "2026-11-22T12:00:00.000Z",
                "2026-11-22T12:00:00+00:00",
                "2026-11-22T12:00:00z",
                "2026-11-22t12:00:00Z",
                "2026-11-22T24:00:00Z",
                "2026-11-22T12:00:60Z",
                "2026-11-22T12:60:00Z",
                "2023-02-29T00:00:00Z",
                "2100-02-29T00:00:00Z",
                "2019-12-31T23:59:59Z",
                "2026-13-01T00:00:00Z",
                "2026-11-31T00:00:00Z",
                "2026-00-10T00:00:00Z",
                "2026-1-01T00:00:00Z",
                "2026-11-22 12:00:00Z",
            ]),
        ),
        (
            "amount",
            strs(&[
                "0",
                "5",
                "0.5",
                "10.25",
                "12345678901234567",
                "0.0000000001",
                "99999999999999999.9999999999",
                "00",
                "01",
                "1.0",
                "1.50",
                "1.",
                ".5",
                "-1",
                "+1",
                "1e3",
                "0.00000000001",
                "123456789012345678",
                " 1",
                "1 ",
                "",
                "0.0",
            ]),
        ),
        (
            "party",
            vec![
                format!("agent-1::{ns}"),
                format!("{}::{ns}", "h".repeat(185)),
                format!("A_b-9::{ns}"),
                format!("agent-1::{}", ns.to_uppercase()),
                format!("agent 1::{ns}"),
                format!("agent-1:{ns}"),
                format!("agent-1::1221{}", "ab".repeat(32)),
                format!("agent-1::1220{}", "a".repeat(63)),
                format!("{}::{ns}", "h".repeat(186)),
                format!("::{ns}"),
                format!("a:b::{ns}"),
            ],
        ),
        (
            "owner_account",
            strs(&["1", "42", "18446744073709551615", "0", "01", "-1", ""]),
        ),
        (
            "label",
            vec![
                "Recovery key".into(),
                "a".into(),
                "A-b_c.d 9".into(),
                "x".repeat(64),
                "x".repeat(65),
                " leading".into(),
                "trailing ".into(),
                "two  spaces".into(),
                "-dash".into(),
                "caf\u{e9}".into(),
                String::new(),
                "tab\tx".into(),
            ],
        ),
        (
            "text140",
            vec![
                "rent".into(),
                "Pay alice 10 CC".into(),
                "~!@#$%^&*()=+[]{}|;:'\",.<>/?".into(),
                "t".repeat(140),
                "t".repeat(141),
                " x".into(),
                "x ".into(),
                "a  b".into(),
                String::new(),
                "a\u{7f}b".into(),
            ],
        ),
        ("text280", vec!["t".repeat(280), "t".repeat(281)]),
        (
            "rp_id",
            vec![
                "silvana.link".into(),
                "localhost".into(),
                "app.silvana.one".into(),
                "xn--80ak6aa92e.com".into(),
                "a1.b2".into(),
                "Silvana.link".into(),
                "silvana".into(),
                "192.168.0.1".into(),
                "-a.com".into(),
                "a-.com".into(),
                "a..com".into(),
                ".a.com".into(),
                "a.com.".into(),
                "a_b.com".into(),
                format!("{}.com", "a".repeat(64)),
                format!(
                    "{}.{}.{}.{}",
                    "a".repeat(63),
                    "b".repeat(63),
                    "c".repeat(63),
                    "d".repeat(62)
                ),
                "localhost.".into(),
                String::new(),
            ],
        ),
        (
            "origin",
            strs(&[
                "https://app.silvana.link",
                "https://silvana.link",
                "http://localhost",
                "http://localhost:3000",
                "https://app.silvana.link:8443",
                "https://app.silvana.link/",
                "https://APP.silvana.link",
                "https://app.silvana.link:443",
                "http://localhost:80",
                "http://app.silvana.link",
                "https://localhost",
                "https://127.0.0.1",
                "https://app.silvana.link:0",
                "https://app.silvana.link:65536",
                "https://app.silvana.link:08443",
                "https://user@app.silvana.link",
                "https://app.silvana.link?x=1",
                "https://app.silvana.link#x",
                "app.silvana.link",
                "https://app.silvana.link/path",
            ]),
        ),
        (
            "base64url",
            strs(&[
                "AQID", "AQ", "AQI", "_-8", "AQ==", "AQ=", "A", "AR", "+/8", "", "AQ D",
            ]),
        ),
        (
            "uuid7",
            strs(&[
                "01890a5d-ac96-774b-bcce-b302099a8057",
                "01890a5d-ac96-474b-bcce-b302099a8057",
                "01890A5D-AC96-774B-BCCE-B302099A8057",
                "01890a5d-ac96-774b-7cce-b302099a8057",
                "01890a5dac96774bbcceb302099a8057",
            ]),
        ),
        (
            "instrument_id",
            vec![
                "Amulet".into(),
                "USDCx".into(),
                "c.ETH-1_2".into(),
                "i".repeat(64),
                "i".repeat(65),
                "a/b".into(),
                "a b".into(),
                String::new(),
            ],
        ),
        (
            "market_id",
            vec![
                "CC/USDCx".into(),
                "m".repeat(64),
                "m".repeat(65),
                "a b".into(),
                "a:b".into(),
            ],
        ),
        (
            "token",
            vec![
                "settle-42".into(),
                "1220ab:update.1_x".into(),
                "t".repeat(256),
                "t".repeat(257),
                "a b".into(),
                "a/b".into(),
            ],
        ),
        (
            "eth_address",
            strs(&[
                "0x52908400098527886e0f7030069857d2e4169ee7",
                "0x52908400098527886E0F7030069857D2E4169EE7",
                "0X52908400098527886e0f7030069857d2e4169ee7",
                "0x52908400098527886e0f7030069857d2e4169ee",
                "52908400098527886e0f7030069857d2e4169ee7ab",
            ]),
        ),
        ("network", strs(&["devnet", "mainnet", "testnet", "Devnet"])),
        (
            "capabilities",
            strs(&[
                "policy,direct,read",
                "policy,direct",
                "policy,read",
                "direct,read",
                "policy",
                "direct",
                "read",
                "",
                "read,policy",
                "policy,policy",
                "admin",
                "policy, read",
                "policy,direct,read,",
            ]),
        ),
        (
            "controller_type",
            strs(&["ed25519", "webauthn", "Ed25519", "p256"]),
        ),
        ("backup_policy", strs(&["any", "device_bound", "synced"])),
        ("proof_type", strs(&["ed25519", "webauthn", "es256"])),
        ("purpose", strs(&["link", "add_controller", "set_owner"])),
        (
            "entry_kind",
            strs(&[
                "link",
                "add_controller",
                "remove_controller",
                "set_capabilities",
                "accept_policy",
                "set_owner",
            ]),
        ),
        ("unlisted", strs(&["unlimited", "deny", "allow"])),
        (
            "receipt_outcome",
            strs(&["executed", "refused", "failed", "expired", "ok"]),
        ),
        ("code_source", strs(&["agent", "ledger", "rpc"])),
        (
            "package_id",
            vec![
                hex32.clone(),
                "f".repeat(64),
                hex32.to_uppercase(),
                hex32[..63].to_owned(),
                format!("{hex32}0"),
                format!("0x{}", &hex32[..62]),
                format!("{} ", &hex32[..63]),
                String::new(),
            ],
        ),
        (
            "command_kind",
            strs(&[
                "swap",
                "transfer_cc",
                "transfer_token",
                "usdcx_burn",
                "Swap",
                "transfer",
                "swap ",
                "usdcx-burn",
                "",
            ]),
        ),
    ];
    table
        .into_iter()
        .flat_map(|(kind, inputs)| {
            inputs
                .into_iter()
                .map(move |input| json!({"type": kind, "input": input}))
        })
        .collect()
}

fn origin_cases() -> Vec<Value> {
    [
        ("http://localhost:3000", "localhost"),
        ("http://localhost", "localhost"),
        ("https://app.silvana.link", "silvana.link"),
        ("https://silvana.link", "silvana.link"),
        ("https://a.b.silvana.link", "silvana.link"),
        ("https://evilsilvana.link", "silvana.link"),
        ("https://app.silvana.one", "silvana.link"),
        ("http://localhost:3000", "silvana.link"),
        ("https://app.silvana.link", "localhost"),
        ("https://silvana.link", "app.silvana.link"),
    ]
    .into_iter()
    .map(|(origin, rp_id)| json!({"origin": origin, "rpId": rp_id}))
    .collect()
}

fn rp_policy_cases() -> Vec<Value> {
    let case = |network: &str, rp_ids: Option<&[&str]>, rp_id: &str, origins: &[&str]| json!({"network": network, "rpIds": rp_ids, "rpId": rp_id, "origins": origins});
    vec![
        case(
            "devnet",
            None,
            "silvana.link",
            &["https://app.silvana.link"],
        ),
        case("devnet", None, "localhost", &["http://localhost:3000"]),
        case("devnet", None, "silvana.link", &["http://localhost:3000"]),
        case("devnet", None, "silvana.link", &["https://app.silvana.one"]),
        case("devnet", None, "localhost", &["https://app.silvana.link"]),
        case("devnet", None, "silvana.one", &["https://app.silvana.one"]),
        case(
            "mainnet",
            None,
            "silvana.one",
            &["https://app.silvana.one", "https://silvana.one"],
        ),
        case(
            "mainnet",
            None,
            "silvana.link",
            &["https://app.silvana.link"],
        ),
        case("mainnet", None, "localhost", &["http://localhost:3000"]),
        case(
            "mainnet",
            Some(&["localhost"][..]),
            "localhost",
            &["http://localhost:3000"],
        ),
        case(
            "devnet",
            Some(&["example.com"][..]),
            "example.com",
            &["https://www.example.com"],
        ),
        case(
            "devnet",
            Some(&["example.com"][..]),
            "silvana.link",
            &["https://app.silvana.link"],
        ),
    ]
}

fn id_cases() -> Vec<Value> {
    let mut out: Vec<Value> = [0x21u8, 0x81, 0x82, 0x83, 0x84, 0x99]
        .into_iter()
        .map(|start| json!({"type": "ed25519", "keyHex": hex::encode(ed_public(start))}))
        .collect();
    out.extend([0x41u8, 0x51].into_iter().map(|start| {
        let spki = testkit::p256_spki(&testkit::p256_key_from(start));
        json!({"type": "webauthn", "keyHex": hex::encode(spki)})
    }));
    out
}

fn party_cases() -> Vec<Value> {
    let mut out: Vec<Value> = (1..=4u8)
        .map(|n| json!({"hint": format!("agent-{n}"), "keyHex": hex::encode(ed_public(n))}))
        .collect();
    let mut identity = [0u8; 32];
    identity[0] = 1;
    out.push(json!({"hint": "weak", "keyHex": hex::encode(identity)}));
    out
}

fn controller_cases() -> Vec<Value> {
    [
        (
            "recovery",
            testkit::ed25519_controller(caps(&[Capability::Read])),
        ),
        (
            "laptop",
            testkit::passkey_controller(caps(&[Capability::Policy, Capability::Direct])),
        ),
        ("local", local_passkey(caps(&[Capability::Read]))),
        (
            "backup",
            ed_controller(0x81, "Backup key", caps(&[Capability::Policy])),
        ),
        (
            "stranger",
            ed_controller(0x99, "Stranger key", CapabilitySet::ALL),
        ),
    ]
    .into_iter()
    .map(|(name, d)| {
        let mut v = view(&ControllerJson::from(&d));
        put(&mut v, "name", name);
        v
    })
    .collect()
}

fn code_cases() -> Value {
    let invite = testkit::invite_descriptor().commitment().unwrap();
    let body_hex = "07".repeat(16);
    let commitments: Vec<Value> = [invite, Commitment([0; 16]), Commitment([0xff; 16])]
        .into_iter()
        .map(|c| json!({"commitmentHex": c.to_string(), "inviteBodyHex": body_hex}))
        .collect();
    let body = format!("AGT-{}", base32_encode(&[7; 16]));
    let link = format_link_code(&invite);
    let full = format_invite_code(&body, &invite).unwrap();
    let commitment_text = link.trim_start_matches("LNK-").to_owned();
    let parse = |function: &str, input: String| json!({"function": function, "input": input});
    let mut bad_trailing = commitment_text.clone();
    bad_trailing.pop();
    bad_trailing.push('B');
    let parse_cases = vec![
        parse("split_invite_code", full.clone()),
        parse("split_invite_code", full.to_lowercase()),
        parse("split_invite_code", body.clone()),
        parse("split_invite_code", link.clone()),
        parse("split_invite_code", format!(" {full}")),
        parse("split_invite_code", format!("{full}-AAAA")),
        parse("split_invite_code", "AGT-A4DQ".into()),
        parse(
            "split_invite_code",
            format!("{}1-{commitment_text}", &body[..body.len() - 1]),
        ),
        parse("split_invite_code", format!("{body}-{bad_trailing}")),
        parse("parse_link_code", link.clone()),
        parse("parse_link_code", link.to_lowercase()),
        parse("parse_link_code", full.clone()),
        parse("parse_link_code", format!("{link}-AB")),
        parse("parse_link_code", "LNK-7PDL".into()),
        parse("parse_link_code", format!("LNK-{bad_trailing}")),
        parse(
            "parse_link_code",
            format!("LNK-{}8", &commitment_text[..25]),
        ),
        json!({"function": "format_invite_code", "input": body, "commitmentHex": invite.to_string()}),
        json!({"function": "format_invite_code", "input": full, "commitmentHex": invite.to_string()}),
    ];
    json!({"commitments": commitments, "parse": parse_cases})
}

fn doc(name: &str, kind: &str, bytes: &[u8]) -> Value {
    with_bytes(json!({"name": name, "kind": kind}), "canonical", bytes)
}

fn envelope_json(controller: &str, envelope: &ProofEnvelope) -> Value {
    with_bytes(
        json!({"controller": controller}),
        "canonical",
        &envelope.encode().unwrap(),
    )
}

fn signed_by_agent(name: &str, kind: &str, bytes: &[u8], key: &ed25519_dalek::SigningKey) -> Value {
    let mut d = doc(name, kind, bytes);
    put(
        &mut d,
        "signatureHex",
        hex::encode(key.sign(bytes).to_bytes()),
    );
    d
}

/// When the main log's commands are issued, after its last entry.
fn main_command_time() -> UtcTime {
    at(2 * DEVNET_DELAY + 100)
}

/// When the main log's receipts and head claim are issued.
fn main_receipt_time() -> UtcTime {
    at(2 * DEVNET_DELAY + 200)
}

/// The swap of `sell_cc` at the main head, as the documents and receipts name it.
fn main_swap(main: &Main) -> Command {
    command_at(
        main.head(12),
        &testkit::agent_party(),
        SWAP_CMD_ID,
        main_command_time(),
        600,
        sell_cc(),
        "Swap 1000 CC for USDCx",
    )
}

/// The v2 receipt of the main swap, settled with LP a: 1000 CC for 151 USDCx, 1.5 CC fees.
fn swap_receipt(main: &Main) -> SwapReceipt {
    let command = main_swap(main);
    SwapReceipt {
        receipt: Receipt {
            network: Network::Devnet,
            agent_party: testkit::agent_party(),
            agent_key: ed_public(1),
            subject: ReceiptSubject::Command(command.command_id.clone()),
            subject_hash: command.hash().unwrap(),
            outcome: ReceiptOutcome::Executed,
            update_id: Some(Token::parse("update-0002").unwrap()),
            reason_code: None,
            code_source: None,
            log_seq: main.head(12).seq,
            issued_at: main_receipt_time(),
        },
        result: SwapResult {
            quote_id: Token::parse("quote-7f3a").unwrap(),
            lp: lp_a(),
            sold: amount("1000"),
            received: amount("151"),
            fee_cc: amount("1.5"),
        },
    }
}

fn receipts(main: &Main) -> Vec<(&'static str, Receipt)> {
    let head = main.head(12);
    let party = testkit::agent_party();
    let issued = main_command_time();
    let command = command_at(
        head,
        &party,
        CMD_ID,
        issued,
        600,
        pay_alice("10"),
        "Pay alice 10 CC",
    );
    let token = command_at(
        head,
        &party,
        TOKEN_CMD_ID,
        issued,
        600,
        send_bob_usdc(),
        "Send bob 2.25 USDCx",
    );
    let burn_command = command_at(
        head,
        &party,
        BURN_CMD_ID,
        issued,
        600,
        burn(),
        "Burn 100 USDCx",
    );
    let swap_command = main_swap(main);
    let policy_hash = policy_example().hash().unwrap();
    let receipt = |subject: ReceiptSubject,
                   subject_hash: Hash32,
                   outcome: ReceiptOutcome,
                   update: Option<&str>,
                   reason: Option<(u64, CodeSource)>| Receipt {
        network: Network::Devnet,
        agent_party: party.clone(),
        agent_key: ed_public(1),
        subject,
        subject_hash,
        outcome,
        update_id: update.map(|u| Token::parse(u).unwrap()),
        reason_code: reason.map(|(code, _)| code),
        code_source: reason.map(|(_, source)| source),
        log_seq: head.seq,
        issued_at: main_receipt_time(),
    };
    let subject = |c: &Command| ReceiptSubject::Command(c.command_id.clone());
    vec![
        (
            "receipt.executed",
            receipt(
                subject(&command),
                command.hash().unwrap(),
                ReceiptOutcome::Executed,
                Some("update-0001"),
                None,
            ),
        ),
        (
            "receipt.refused_agent",
            receipt(
                subject(&token),
                token.hash().unwrap(),
                ReceiptOutcome::Refused,
                None,
                Some((4, CodeSource::Agent)),
            ),
        ),
        (
            "receipt.failed_ledger",
            receipt(
                subject(&burn_command),
                burn_command.hash().unwrap(),
                ReceiptOutcome::Failed,
                None,
                Some((4117, CodeSource::Ledger)),
            ),
        ),
        (
            "receipt.expired",
            receipt(
                subject(&command),
                command.hash().unwrap(),
                ReceiptOutcome::Expired,
                None,
                None,
            ),
        ),
        (
            "receipt.policy_refused",
            receipt(
                ReceiptSubject::Policy(policy_hash),
                policy_hash,
                ReceiptOutcome::Refused,
                None,
                Some((18, CodeSource::Agent)),
            ),
        ),
        (
            "receipt.swap_failed",
            receipt(
                subject(&swap_command),
                swap_command.hash().unwrap(),
                ReceiptOutcome::Failed,
                None,
                Some((
                    u64::from(CommandFailure::BelowMinimum.code()),
                    CodeSource::Agent,
                )),
            ),
        ),
        (
            "receipt.swap_out_of_scope",
            receipt(
                subject(&swap_command),
                swap_command.hash().unwrap(),
                ReceiptOutcome::Refused,
                None,
                Some((
                    u64::from(CommandError::OutOfScope.code()),
                    CodeSource::Agent,
                )),
            ),
        ),
    ]
}

fn documents(main: &Main) -> Vec<Value> {
    let party = testkit::agent_party();
    let head = main.head(12);
    let mut out = Vec::new();
    let invite = testkit::invite_descriptor();
    let mut invite_doc = doc("link.invite", "link_descriptor", &invite.encode().unwrap());
    put(&mut invite_doc, "inviteBodyHex", "07".repeat(16));
    out.push(invite_doc);
    out.push(doc(
        "link.party",
        "link_descriptor",
        &party_descriptor().encode().unwrap(),
    ));
    out.push(doc(
        "link.add",
        "link_descriptor",
        &add_local_descriptor().encode().unwrap(),
    ));
    out.push(doc(
        "link.relink",
        "link_descriptor",
        &relink_descriptor().encode().unwrap(),
    ));
    for (name, controller, envelope) in [
        (
            "proof.ed25519",
            "recovery",
            testkit::ed25519_envelope(0x21, PROOF_PAYLOAD),
        ),
        (
            "proof.webauthn",
            "laptop",
            testkit::passkey_envelope(PROOF_PAYLOAD),
        ),
    ] {
        let mut d = doc(name, "proof", &envelope.encode().unwrap());
        put(&mut d, "controller", controller);
        put(&mut d, "payload", text(PROOF_PAYLOAD));
        out.push(d);
    }
    for n in [1usize, 2, 3, 4, 5, 6, 8, 10, 12] {
        let entry = &main.chain.entries[n - 1];
        let mut d = doc(&format!("log_entry.main.{n}"), "log_entry", entry.bytes());
        put(&mut d, "signatureHex", hex::encode(entry.signature()));
        out.push(d);
    }
    let signed = |name: &str, kind: &str, bytes: Vec<u8>, signers: &[&str]| {
        let mut d = doc(name, kind, &bytes);
        let envelopes: Vec<Value> = signers
            .iter()
            .map(|signer| {
                let envelope = match *signer {
                    "laptop" => testkit::passkey_envelope(&bytes),
                    "recovery" => testkit::ed25519_envelope(0x21, &bytes),
                    _ => testkit::ed25519_envelope(0x81, &bytes),
                };
                envelope_json(signer, &envelope)
            })
            .collect();
        put(&mut d, "envelopes", envelopes);
        d
    };
    out.push(signed(
        "policy.default",
        "policy",
        default_document().encode().unwrap(),
        &["backup"],
    ));
    out.push(signed(
        "policy.example",
        "policy",
        policy_example().encode().unwrap(),
        &["laptop", "recovery"],
    ));
    out.push(signed(
        "policy.packages",
        "policy",
        packages_example().encode().unwrap(),
        &["laptop"],
    ));
    out.push(signed(
        "policy.scopes",
        "policy",
        scopes_example().encode().unwrap(),
        &["laptop"],
    ));
    let command = |id: &str, op: CommandOp, summary: &str| {
        command_at(head, &party, id, main_command_time(), 600, op, summary)
            .encode()
            .unwrap()
    };
    out.push(signed(
        "command.swap",
        "command",
        main_swap(main).encode().unwrap(),
        &["laptop"],
    ));
    out.push(signed(
        "command.swap_buy",
        "command",
        command(SWAP_BUY_CMD_ID, buy_cc(), "Swap 150 USDCx for CC"),
        &["laptop"],
    ));
    out.push(signed(
        "command.example",
        "command",
        command(CMD_ID, pay_alice("10"), "Pay alice 10 CC"),
        &["laptop", "recovery"],
    ));
    out.push(signed(
        "command.transfer_token",
        "command",
        command(TOKEN_CMD_ID, send_bob_usdc(), "Send bob 2.25 USDCx"),
        &["recovery"],
    ));
    out.push(signed(
        "command.usdcx_burn",
        "command",
        command(BURN_CMD_ID, burn(), "Burn 100 USDCx"),
        &["laptop"],
    ));
    let session = session_request(&[1, 2, 3], at(DAY + 100), 3600);
    out.push(signed(
        "read_session.example",
        "read_session",
        session.encode().unwrap(),
        &["laptop"],
    ));
    let issued = ms(at(DAY + 200)) + 250;
    for (name, method, limit) in [
        ("read_request.example", "ListAgentActivity", 50),
        ("read_request.holdings", "GetAgentHoldings", 0),
    ] {
        let bytes = read_request(&session, method, agent(2), limit, issued)
            .encode()
            .unwrap();
        let mut d = doc(name, "read_request", &bytes);
        put(&mut d, "sessionKeyHex", hex::encode(session_key_spki()));
        put(&mut d, "signatureHex", hex::encode(p1363(0x61, &bytes)));
        out.push(d);
    }
    for (name, receipt) in receipts(main) {
        out.push(signed_by_agent(
            name,
            "receipt",
            &receipt.encode().unwrap(),
            &testkit::agent_key(),
        ));
    }
    out.push(signed_by_agent(
        "receipt.swap_executed",
        "receipt",
        &swap_receipt(main).encode().unwrap(),
        &testkit::agent_key(),
    ));
    let empty = HeadClaim::for_state(&testkit::empty_state(), at(0));
    out.push(signed_by_agent(
        "head_claim.empty",
        "head_claim",
        &empty.encode().unwrap(),
        &testkit::agent_key(),
    ));
    let linked = HeadClaim::for_state(&main.chain.state, main_receipt_time());
    out.push(signed_by_agent(
        "head_claim.linked",
        "head_claim",
        &linked.encode().unwrap(),
        &testkit::agent_key(),
    ));
    out
}

// ---------------------------------------------------------------------------
// Negative documents

fn negative_cases(documents: &[Value]) -> Vec<Value> {
    let base = |name: &str| -> String {
        let d = documents
            .iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("document {name}"));
        st(d, "canonical").to_owned()
    };
    let h = base("head_claim.linked");
    let l = base("link.invite");
    let la = base("link.add");
    let lp = base("link.party");
    let pe = base("proof.ed25519");
    let pw = base("proof.webauthn");
    let e1 = base("log_entry.main.1");
    let e2 = base("log_entry.main.2");
    let e3 = base("log_entry.main.3");
    let e4 = base("log_entry.main.4");
    let e5 = base("log_entry.main.5");
    let e8 = base("log_entry.main.8");
    let pd = base("policy.default");
    let px = base("policy.example");
    let c = base("command.example");
    let ct = base("command.transfer_token");
    let cb = base("command.usdcx_burn");
    let rs = base("read_session.example");
    let rq = base("read_request.example");
    let rc = base("receipt.executed");
    let rr = base("receipt.refused_agent");
    let rp = base("receipt.policy_refused");
    let pp = base("policy.packages");
    let ps = base("policy.scopes");
    let cs = base("command.swap");
    let rv = base("receipt.swap_executed");
    let head_value = line_value(&h, "head").to_owned();
    let party_line = line_value(&h, "agent_party").to_owned();
    let (hint, namespace) = party_line.split_once("::").unwrap();
    let mut out: Vec<(String, &str, Vec<u8>)> = Vec::new();
    let mut add = |name: &str, kind: &'static str, bytes: Vec<u8>| {
        out.push((name.to_owned(), kind, bytes));
    };
    let hk = "head_claim";
    add(
        "too_short",
        hk,
        b"scheme=silvana.agent.control-head.v1\nnetwork=devnet\n".to_vec(),
    );
    add(
        "too_large",
        hk,
        format!("{h}pad={}\n", "x".repeat(800)).into_bytes(),
    );
    add(
        "invalid_byte: CR",
        hk,
        h.replacen('\n', "\r\n", 1).into_bytes(),
    );
    add(
        "invalid_byte: TAB",
        hk,
        mutate(&h, "network=devnet", "network=\tdevnet").into_bytes(),
    );
    add(
        "invalid_byte: NUL",
        hk,
        mutate(&h, "network=devnet", "network=dev\0net").into_bytes(),
    );
    add(
        "invalid_byte: non-ASCII",
        hk,
        mutate(&h, "network=devnet", "network=d\u{e9}vnet").into_bytes(),
    );
    let mut high = h.clone().into_bytes();
    high.insert(10, 0x80);
    add("invalid_byte: 0x80", hk, high);
    add("invalid_byte: BOM", hk, format!("\u{feff}{h}").into_bytes());
    add(
        "missing_final_newline",
        hk,
        h.trim_end_matches('\n').as_bytes().to_vec(),
    );
    add("empty_line", hk, h.replacen('\n', "\n\n", 1).into_bytes());
    add(
        "empty_line: doubled final LF",
        hk,
        format!("{h}\n").into_bytes(),
    );
    add(
        "wrong_scheme: version",
        hk,
        mutate(&h, "control-head.v1", "control-head.v2").into_bytes(),
    );
    add(
        "wrong_scheme: another document",
        hk,
        rc.clone().into_bytes(),
    );
    add(
        "malformed_line",
        hk,
        mutate(&h, "network=devnet", "network devnet").into_bytes(),
    );
    add(
        "unexpected_key: unknown key",
        hk,
        mutate(&h, "network=devnet", "netwrk=devnet").into_bytes(),
    );
    let mut lines: Vec<&str> = h.lines().collect();
    lines.swap(1, 2);
    add(
        "unexpected_key: swapped lines",
        hk,
        (lines.join("\n") + "\n").into_bytes(),
    );
    add(
        "unexpected_key: duplicated line",
        hk,
        mutate(&h, "network=devnet\n", "network=devnet\nnetwork=devnet\n").into_bytes(),
    );
    let without_last = h
        .trim_end_matches('\n')
        .rsplit_once('\n')
        .unwrap()
        .0
        .to_owned()
        + "\n";
    add("missing_line", hk, without_last.into_bytes());
    add(
        "empty_value",
        hk,
        mutate(&h, "network=devnet", "network=").into_bytes(),
    );
    add(
        "invalid_network",
        hk,
        mutate(&h, "network=devnet", "network=testnet").into_bytes(),
    );
    add("trailing_line", hk, format!("{h}extra=1\n").into_bytes());
    add(
        "invalid_head: seq 0 with a head",
        hk,
        set_line(&h, "seq", "0").into_bytes(),
    );
    add(
        "invalid_head: zero head",
        hk,
        set_line(&h, "head", &"0".repeat(64)).into_bytes(),
    );
    add(
        "invalid_u64: leading zero",
        hk,
        set_line(&h, "seq", "012").into_bytes(),
    );
    add(
        "invalid_hex32: uppercase head",
        hk,
        set_line(&h, "head", &head_value.to_uppercase()).into_bytes(),
    );
    add(
        "invalid_time: lowercase z",
        hk,
        mutate(&h, "Z\n", "z\n").into_bytes(),
    );
    add(
        "invalid_party: uppercase namespace",
        hk,
        set_line(
            &h,
            "agent_party",
            &format!("{hint}::{}", namespace.to_uppercase()),
        )
        .into_bytes(),
    );
    let lk = "link_descriptor";
    add(
        "party_required",
        lk,
        set_line(&la, "agent_party", "").into_bytes(),
    );
    add(
        "count_out_of_range: no controllers",
        lk,
        set_line(&l, "controllers.count", "0").into_bytes(),
    );
    add(
        "count_out_of_range: seven controllers",
        lk,
        set_line(&l, "controllers.count", "7").into_bytes(),
    );
    add(
        "count_out_of_range: two in add_controller",
        lk,
        set_line(&la, "controllers.count", "2").into_bytes(),
    );
    add(
        "capability_not_covered: count too low",
        lk,
        set_line(&l, "controllers.count", "1").into_bytes(),
    );
    add(
        "missing_line: count too high",
        lk,
        set_line(&l, "controllers.count", "3").into_bytes(),
    );
    add(
        "capability_not_covered: no reader",
        lk,
        set_line(&l, "controllers.0.capabilities", "policy").into_bytes(),
    );
    add(
        "unsorted_list: controllers",
        lk,
        reorder(&l, "controllers.", &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: controllers",
        lk,
        reorder(&l, "controllers.", &[0, 0]).into_bytes(),
    );
    let id0 = line_value(&l, "controllers.0.controller_id").to_owned();
    let mut tampered_id = id0.clone();
    tampered_id.pop();
    tampered_id.push(if id0.ends_with('4') { '5' } else { '4' });
    add(
        "controller_id_mismatch",
        lk,
        set_line(&l, "controllers.0.controller_id", &tampered_id).into_bytes(),
    );
    add(
        "invalid_public_key: non-canonical ed25519",
        lk,
        set_line(&l, "controllers.0.public_key", &"ff".repeat(32)).into_bytes(),
    );
    add(
        "weak_public_key: identity",
        lk,
        set_line(
            &l,
            "controllers.0.public_key",
            &format!("01{}", "00".repeat(31)),
        )
        .into_bytes(),
    );
    let ed_key_text = line_value(&l, "controllers.0.public_key").to_owned();
    add(
        "invalid_hex32: uppercase ed25519 key",
        lk,
        set_line(&l, "controllers.0.public_key", &ed_key_text.to_uppercase()).into_bytes(),
    );
    let spki_text = line_value(&l, "controllers.1.public_key").to_owned();
    add(
        "invalid_spki: uppercase",
        lk,
        set_line(&l, "controllers.1.public_key", &spki_text.to_uppercase()).into_bytes(),
    );
    add(
        "invalid_spki: short",
        lk,
        set_line(&l, "controllers.1.public_key", &spki_text[..180]).into_bytes(),
    );
    let mut off_curve = spki_text.clone();
    let last = off_curve.pop().unwrap();
    off_curve.push(if last == '0' { '1' } else { '0' });
    add(
        "invalid_public_key: SPKI off the curve",
        lk,
        set_line(&l, "controllers.1.public_key", &off_curve).into_bytes(),
    );
    add(
        "invalid_credential_id: 15 bytes",
        lk,
        set_line(&l, "controllers.1.credential_id", &encode_b64url(&[7; 15])).into_bytes(),
    );
    let credential = line_value(&l, "controllers.1.credential_id").to_owned();
    add(
        "invalid_credential_id: padded",
        lk,
        set_line(&l, "controllers.1.credential_id", &format!("{credential}=")).into_bytes(),
    );
    add(
        "invalid_rp_id: uppercase",
        lk,
        set_line(&l, "controllers.1.rp_id", "Silvana.link").into_bytes(),
    );
    add(
        "origin_outside_rp_id",
        lk,
        set_line(&l, "controllers.1.origins.0", "https://app.silvana.one").into_bytes(),
    );
    add(
        "invalid_origin: trailing slash",
        lk,
        set_line(&l, "controllers.1.origins.0", "https://app.silvana.link/").into_bytes(),
    );
    add(
        "count_out_of_range: no origins",
        lk,
        set_line(&l, "controllers.1.origins.count", "0").into_bytes(),
    );
    add(
        "count_out_of_range: five origins",
        lk,
        set_line(&l, "controllers.1.origins.count", "5").into_bytes(),
    );
    add(
        "invalid_require_uv",
        lk,
        set_line(&l, "controllers.1.require_uv", "false").into_bytes(),
    );
    add(
        "invalid_backup_policy",
        lk,
        set_line(&l, "controllers.1.backup_policy", "synced").into_bytes(),
    );
    add(
        "not_allowed_on_network: localhost on mainnet",
        lk,
        set_line(&la, "network", "mainnet").into_bytes(),
    );
    add(
        "invalid_purpose",
        lk,
        set_line(&l, "purpose", "set_owner").into_bytes(),
    );
    add(
        "invalid_owner_account: zero",
        lk,
        set_line(&l, "owner_account", "0").into_bytes(),
    );
    add(
        "invalid_hex16: uppercase nonce",
        lk,
        set_line(&l, "nonce", &"5A".repeat(16)).into_bytes(),
    );
    add(
        "invalid_controller_type",
        lk,
        set_line(&l, "controllers.0.type", "p256").into_bytes(),
    );
    add(
        "invalid_capabilities: order",
        lk,
        set_line(&l, "controllers.1.capabilities", "direct,policy").into_bytes(),
    );
    add(
        "unexpected_key: index with a leading zero",
        lk,
        mutate(
            &l,
            "controllers.0.controller_id=",
            "controllers.00.controller_id=",
        )
        .into_bytes(),
    );
    add(
        "unsorted_list: origins",
        lk,
        reorder(&lp, "controllers.1.origins.", &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: origins",
        lk,
        reorder(&lp, "controllers.1.origins.", &[0, 0]).into_bytes(),
    );
    add(
        "too_large: link descriptor",
        lk,
        format!("{l}{}", "x".repeat(2600)).into_bytes(),
    );
    let pk = "proof";
    add(
        "invalid_authenticator_data: 36 bytes",
        pk,
        set_line(&pw, "proof.authenticator_data", &encode_b64url(&[1; 36])).into_bytes(),
    );
    add(
        "invalid_client_data_json: over 2048 bytes",
        pk,
        set_line(&pw, "proof.client_data_json", &encode_b64url(&[b'x'; 2049])).into_bytes(),
    );
    add(
        "invalid_der_signature: 7 bytes",
        pk,
        set_line(&pw, "proof.signature", &encode_b64url(&[0x30; 7])).into_bytes(),
    );
    add(
        "invalid_proof_type",
        pk,
        set_line(&pe, "proof.type", "es256").into_bytes(),
    );
    let signature_text = line_value(&pe, "proof.signature").to_owned();
    add(
        "invalid_hex64: short signature",
        pk,
        set_line(&pe, "proof.signature", &signature_text[..126]).into_bytes(),
    );
    add(
        "unexpected_key: ed25519 fields under webauthn",
        pk,
        set_line(&pe, "proof.type", "webauthn").into_bytes(),
    );
    let payload_hash = line_value(&pe, "payload_sha256").to_owned();
    add(
        "invalid_hex32: uppercase payload hash",
        pk,
        set_line(&pe, "payload_sha256", &payload_hash.to_uppercase()).into_bytes(),
    );
    let ek = "log_entry";
    add(
        "invalid_entry_kind",
        ek,
        set_line(&e3, "kind", "set_owner").into_bytes(),
    );
    add(
        "partial_descriptor",
        ek,
        set_line(&e2, "nonce", "").into_bytes(),
    );
    add(
        "count_out_of_range: link entry with seven controllers",
        ek,
        set_line(&e1, "controllers.count", "7").into_bytes(),
    );
    add(
        "empty_value: effective_at",
        ek,
        set_line(&e1, "effective_at", "").into_bytes(),
    );
    add(
        "invalid_bool: invite",
        ek,
        set_line(&e1, "invite", "yes").into_bytes(),
    );
    add(
        "capability_not_covered: link entry",
        ek,
        set_line(&e1, "controllers.0.capabilities", "policy").into_bytes(),
    );
    add(
        "invalid_proof_type: accept_policy",
        ek,
        set_line(&e5, "proof.type", "passkey").into_bytes(),
    );
    add(
        "invalid_capabilities: set_capabilities",
        ek,
        set_line(&e4, "capabilities", "all").into_bytes(),
    );
    add(
        "invalid_bool: forced",
        ek,
        set_line(&e8, "forced", "no").into_bytes(),
    );
    let prev = line_value(&e3, "prev").to_owned();
    add(
        "invalid_hex32: uppercase prev",
        ek,
        set_line(&e3, "prev", &prev.to_uppercase()).into_bytes(),
    );
    let qk = "policy";
    add(
        "invalid_version",
        qk,
        set_line(&pd, "version", "0").into_bytes(),
    );
    add(
        "invalid_validity: expires at issue",
        qk,
        set_line(&pd, "expires_at", "2026-11-22T12:00:00Z").into_bytes(),
    );
    add(
        "invalid_validity: over a day",
        qk,
        set_line(&pd, "expires_at", "2026-11-23T12:00:01Z").into_bytes(),
    );
    add(
        "invalid_not_before: before issue",
        qk,
        set_line(&pd, "not_before", "2026-11-22T11:59:59Z").into_bytes(),
    );
    add(
        "invalid_not_before: over 30 days",
        qk,
        set_line(&pd, "not_before", "2026-12-22T12:00:01Z").into_bytes(),
    );
    add(
        "delay_out_of_range",
        qk,
        set_line(&pd, "delay.transfers_secs", "2592001").into_bytes(),
    );
    add(
        "invalid_bool: paused",
        qk,
        set_line(&pd, "paused", "no").into_bytes(),
    );
    add(
        "invalid_amount: traffic cap",
        qk,
        set_line(&pd, "maintenance.traffic_cc_per_day", "1.50").into_bytes(),
    );
    add(
        "count_out_of_range: any with items",
        qk,
        set_line(&pd, "trading.markets.count", "1").into_bytes(),
    );
    add(
        "unsorted_list: markets",
        qk,
        reorder(&px, "trading.markets.", &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: markets",
        qk,
        reorder(&px, "trading.markets.", &[0, 0]).into_bytes(),
    );
    add(
        "invalid_market_id",
        qk,
        set_line(&px, "trading.markets.0", "CC USDCx").into_bytes(),
    );
    add(
        "invalid_instrument_admin",
        qk,
        set_line(&px, "trading.instruments.1.admin", "CC").into_bytes(),
    );
    add(
        "invalid_cc_alias",
        qk,
        set_line(&px, "trading.instruments.1.id", "Amulet").into_bytes(),
    );
    add(
        "invalid_unlisted",
        qk,
        set_line(&px, "trading.per_trade.unlisted", "allow").into_bytes(),
    );
    add(
        "unsorted_list: limit items",
        qk,
        reorder(&px, "trading.per_trade.", &[1, 0]).into_bytes(),
    );
    add(
        "count_out_of_range: 33 limit items",
        qk,
        set_line(&px, "trading.per_trade.count", "33").into_bytes(),
    );
    let eth_text = line_value(&px, "transfers.eth_destinations.0").to_owned();
    add(
        "invalid_eth_address: uppercase",
        qk,
        set_line(
            &px,
            "transfers.eth_destinations.0",
            &eth_text.to_uppercase().replacen("0X", "0x", 1),
        )
        .into_bytes(),
    );
    add(
        "unsorted_list: recipients",
        qk,
        reorder(&px, "transfers.recipients.", &[1, 0]).into_bytes(),
    );
    let ck = "command";
    add(
        "invalid_amount: zero",
        ck,
        set_line(&c, "op.amount", "0").into_bytes(),
    );
    add(
        "invalid_command_op",
        ck,
        set_line(&c, "op", "transfer_all").into_bytes(),
    );
    let too_late = UtcTime::parse(line_value(&c, "issued_at"))
        .unwrap()
        .checked_add_secs(901)
        .unwrap()
        .to_string();
    add(
        "invalid_validity: over 900 s",
        ck,
        set_line(&c, "expires_at", &too_late).into_bytes(),
    );
    add(
        "invalid_validity: expires at issue",
        ck,
        set_line(&c, "expires_at", line_value(&c, "issued_at")).into_bytes(),
    );
    add(
        "invalid_uuid7: version 4",
        ck,
        set_line(&c, "command_id", "01890a5d-ac96-474b-bcce-b302099a8057").into_bytes(),
    );
    add(
        "invalid_text: double space",
        ck,
        set_line(&c, "summary", "Pay  alice 10 CC").into_bytes(),
    );
    add(
        "invalid_text: description over 140",
        ck,
        set_line(&c, "op.description", &"r".repeat(141)).into_bytes(),
    );
    add(
        "invalid_token: settlement id",
        ck,
        set_line(&c, "op.settlement_proposal_id", "a b").into_bytes(),
    );
    let destination = line_value(&cb, "op.destination").to_owned();
    add(
        "invalid_eth_address: burn destination",
        ck,
        set_line(
            &cb,
            "op.destination",
            &destination.to_uppercase().replacen("0X", "0x", 1),
        )
        .into_bytes(),
    );
    add(
        "invalid_instrument_id",
        ck,
        set_line(&ct, "op.instrument_id", "USD Cx").into_bytes(),
    );
    add(
        "unexpected_key: token fields under transfer_cc",
        ck,
        set_line(&ct, "op", "transfer_cc").into_bytes(),
    );
    let sk = "read_session";
    let session_key = line_value(&rs, "session_key").to_owned();
    add(
        "invalid_spki: uppercase session key",
        sk,
        set_line(&rs, "session_key", &session_key.to_uppercase()).into_bytes(),
    );
    let mut bad_session_key = session_key.clone();
    let last = bad_session_key.pop().unwrap();
    bad_session_key.push(if last == '0' { '1' } else { '0' });
    add(
        "invalid_public_key: session key off the curve",
        sk,
        set_line(&rs, "session_key", &bad_session_key).into_bytes(),
    );
    add(
        "invalid_validity: over an hour",
        sk,
        set_line(&rs, "expires_at", "2026-11-23T13:01:41Z").into_bytes(),
    );
    add(
        "count_out_of_range: no agents",
        sk,
        set_line(&rs, "agents.count", "0").into_bytes(),
    );
    add(
        "unsorted_list: agents",
        sk,
        reorder(&rs, "agents.", &[1, 0, 2]).into_bytes(),
    );
    add(
        "duplicate_item: agents",
        sk,
        reorder(&rs, "agents.", &[0, 0, 2]).into_bytes(),
    );
    let rk = "read_request";
    add(
        "invalid_range",
        rk,
        set_line(&rq, "range_offset", "18446744073709551615").into_bytes(),
    );
    add(
        "invalid_time: issued_at_ms before 2020",
        rk,
        set_line(&rq, "issued_at_ms", "1577836799999").into_bytes(),
    );
    add(
        "invalid_time: issued_at_ms after 9999",
        rk,
        set_line(&rq, "issued_at_ms", "253402300800000").into_bytes(),
    );
    add(
        "invalid_token: method",
        rk,
        set_line(&rq, "method", "List Agent").into_bytes(),
    );
    let session_id = line_value(&rq, "session_id").to_owned();
    add(
        "invalid_hex16: uppercase session id",
        rk,
        set_line(&rq, "session_id", &session_id.to_uppercase()).into_bytes(),
    );
    let xk = "receipt";
    add(
        "outcome_mismatch: executed without update_id",
        xk,
        set_line(&rc, "update_id", "").into_bytes(),
    );
    add(
        "outcome_mismatch: refused with update_id",
        xk,
        set_line(&rr, "update_id", "update-1").into_bytes(),
    );
    add(
        "outcome_mismatch: refused without reason",
        xk,
        set_line(&rr, "reason_code", "").into_bytes(),
    );
    add(
        "outcome_mismatch: reason without source",
        xk,
        set_line(&rr, "code_source", "").into_bytes(),
    );
    add(
        "outcome_mismatch: executed policy",
        xk,
        set_line(&rp, "outcome", "executed").into_bytes(),
    );
    add(
        "invalid_receipt_subject",
        xk,
        set_line(&rc, "subject", "order").into_bytes(),
    );
    add(
        "invalid_outcome",
        xk,
        set_line(&rc, "outcome", "done").into_bytes(),
    );
    add(
        "invalid_code_source",
        xk,
        set_line(&rr, "code_source", "rpc").into_bytes(),
    );
    let subject_id = line_value(&rc, "subject_id").to_owned();
    add(
        "invalid_uuid7: uppercase subject id",
        xk,
        set_line(&rc, "subject_id", &subject_id.to_uppercase()).into_bytes(),
    );
    add(
        "invalid_hex32: policy subject id",
        xk,
        set_line(&rp, "subject_id", CMD_ID).into_bytes(),
    );
    // The package rule: any, or 1 to 256 ascending ids.
    let first_package = line_value(&pp, "packages.0").to_owned();
    add(
        "invalid_package_id: 63 hex characters",
        qk,
        set_line(&pp, "packages.0", &first_package[..63]).into_bytes(),
    );
    add(
        "invalid_package_id: uppercase",
        qk,
        set_line(&pp, "packages.0", &first_package.to_uppercase()).into_bytes(),
    );
    add(
        "unsorted_list: packages",
        qk,
        reorder(&pp, "packages.", &[1, 0, 2]).into_bytes(),
    );
    add(
        "duplicate_item: packages",
        qk,
        reorder(&pp, "packages.", &[0, 0, 2]).into_bytes(),
    );
    add(
        "count_out_of_range: 257 packages",
        qk,
        set_line(&pp, "packages.count", "257").into_bytes(),
    );
    add(
        "count_out_of_range: a selection of none",
        qk,
        set_line(&pp, "packages.count", "0").into_bytes(),
    );
    add(
        "count_out_of_range: any with packages",
        qk,
        set_line(&pp, "packages.any", "true").into_bytes(),
    );
    add(
        "invalid_bool: packages.any",
        qk,
        set_line(&pp, "packages.any", "yes").into_bytes(),
    );
    // Command scopes; `rich` is the scope with two kinds, pairs and LPs.
    let rich = if line_value(&ps, "scopes.0.commands.count") == "2" {
        0
    } else {
        1
    };
    let empty_scope = 1 - rich;
    let s = |key: &str| format!("scopes.{rich}.{key}");
    let cc_pair = if line_value(&ps, &s("pairs.0.sell.admin")) == "cc" {
        0
    } else {
        1
    };
    add(
        "count_out_of_range: 33 scopes",
        qk,
        set_line(&ps, "scopes.count", "33").into_bytes(),
    );
    add(
        "unsorted_list: scopes",
        qk,
        reorder(&ps, "scopes.", &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: scopes",
        qk,
        reorder(&ps, "scopes.", &[0, 0]).into_bytes(),
    );
    let scope_controller = line_value(&ps, "scopes.0.controller").to_owned();
    add(
        "invalid_hex32: uppercase scope controller",
        qk,
        set_line(&ps, "scopes.0.controller", &scope_controller.to_uppercase()).into_bytes(),
    );
    add(
        "invalid_command_op: an unknown scope kind",
        qk,
        set_line(&ps, &s("commands.0"), "transfer").into_bytes(),
    );
    add(
        "invalid_command_op: an uppercase scope kind",
        qk,
        set_line(&ps, &s("commands.0"), "Swap").into_bytes(),
    );
    add(
        "count_out_of_range: five scope kinds",
        qk,
        set_line(&ps, &s("commands.count"), "5").into_bytes(),
    );
    add(
        "unsorted_list: scope kinds",
        qk,
        reorder(&ps, &s("commands."), &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: scope kinds",
        qk,
        reorder(&ps, &s("commands."), &[0, 0]).into_bytes(),
    );
    add(
        "count_out_of_range: 17 scope pairs",
        qk,
        set_line(&ps, &s("pairs.count"), "17").into_bytes(),
    );
    add(
        "unsorted_list: scope pairs",
        qk,
        reorder(&ps, &s("pairs."), &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: scope pairs",
        qk,
        reorder(&ps, &s("pairs."), &[0, 0]).into_bytes(),
    );
    let sold_admin = line_value(&ps, &s("pairs.0.sell.admin")).to_owned();
    let sold_id = line_value(&ps, &s("pairs.0.sell.id")).to_owned();
    let same_pair = set_line(
        &set_line(&ps, &s("pairs.0.buy.admin"), &sold_admin),
        &s("pairs.0.buy.id"),
        &sold_id,
    );
    add("same_instrument: a scope pair", qk, same_pair.into_bytes());
    add(
        "invalid_cc_alias: a scope pair",
        qk,
        set_line(&ps, &s(&format!("pairs.{cc_pair}.sell.id")), "Amulet").into_bytes(),
    );
    add(
        "invalid_amount: a min_price with a leading zero",
        qk,
        set_line(&ps, &s("pairs.0.min_price"), "06").into_bytes(),
    );
    add(
        "invalid_amount: a max_sell that is not minimal",
        qk,
        set_line(&ps, &s("pairs.0.max_sell"), "150.0").into_bytes(),
    );
    add(
        "count_out_of_range: 17 scope lps",
        qk,
        set_line(&ps, &s("lps.count"), "17").into_bytes(),
    );
    add(
        "unsorted_list: scope lps",
        qk,
        reorder(&ps, &s("lps."), &[1, 0]).into_bytes(),
    );
    add(
        "duplicate_item: scope lps",
        qk,
        reorder(&ps, &s("lps."), &[0, 0]).into_bytes(),
    );
    add(
        "invalid_party: a scope lp",
        qk,
        set_line(&ps, &s("lps.0"), "lp-a").into_bytes(),
    );
    add(
        "unexpected_key: a scope without its pairs count",
        qk,
        mutate(
            &ps,
            &format!("\nscopes.{empty_scope}.pairs.count=0\n"),
            "\n",
        )
        .into_bytes(),
    );
    // The swap command.
    let sell_admin = line_value(&cs, "op.sell_instrument_admin").to_owned();
    let sell_id = line_value(&cs, "op.sell_instrument_id").to_owned();
    let same_swap = set_line(
        &set_line(&cs, "op.buy_instrument_admin", &sell_admin),
        "op.buy_instrument_id",
        &sell_id,
    );
    add("same_instrument: a swap", ck, same_swap.into_bytes());
    add(
        "invalid_amount: a swap selling zero",
        ck,
        set_line(&cs, "op.sell_amount", "0").into_bytes(),
    );
    add(
        "invalid_amount: a swap with a zero minimum",
        ck,
        set_line(&cs, "op.min_receive", "0").into_bytes(),
    );
    add(
        "invalid_amount: a fee cap that is not minimal",
        ck,
        set_line(&cs, "op.max_fee_cc", "2.0").into_bytes(),
    );
    add(
        "empty_value: a swap without a fee cap",
        ck,
        set_line(&cs, "op.max_fee_cc", "").into_bytes(),
    );
    add(
        "invalid_party: a bare sell admin",
        ck,
        set_line(&cs, "op.sell_instrument_admin", "DSO").into_bytes(),
    );
    add(
        "invalid_instrument_id: a buy id with a space",
        ck,
        set_line(&cs, "op.buy_instrument_id", "USD Cx").into_bytes(),
    );
    add(
        "unexpected_key: swap fields under transfer_cc",
        ck,
        set_line(&cs, "op", "transfer_cc").into_bytes(),
    );
    add(
        "unexpected_key: transfer fields under swap",
        ck,
        set_line(&c, "op", "swap").into_bytes(),
    );
    // The v2 receipt of an executed swap.
    add(
        "outcome_mismatch: a v2 receipt that failed",
        xk,
        set_line(&rv, "outcome", "failed").into_bytes(),
    );
    let swap_hash = line_value(&rv, "subject_hash").to_owned();
    add(
        "outcome_mismatch: a v2 receipt for a policy",
        xk,
        set_line(
            &set_line(&rv, "subject", "policy"),
            "subject_id",
            &swap_hash,
        )
        .into_bytes(),
    );
    add(
        "invalid_amount: a v2 receipt that sold nothing",
        xk,
        set_line(&rv, "result.sold", "0").into_bytes(),
    );
    add(
        "invalid_amount: a v2 receipt that received nothing",
        xk,
        set_line(&rv, "result.received", "0").into_bytes(),
    );
    add(
        "invalid_amount: a v2 fee that is not minimal",
        xk,
        set_line(&rv, "result.fee_cc", "1.50").into_bytes(),
    );
    add(
        "invalid_token: a quote id with a space",
        xk,
        set_line(&rv, "result.quote_id", "quote 7").into_bytes(),
    );
    add(
        "invalid_party: a bare lp",
        xk,
        set_line(&rv, "result.lp", "lp-a").into_bytes(),
    );
    let unsettled: String = rv
        .lines()
        .filter(|line| !line.starts_with("result."))
        .map(|line| format!("{line}\n"))
        .collect();
    add(
        "missing_line: a v2 receipt without its result",
        xk,
        unsettled.into_bytes(),
    );
    add(
        "trailing_line: a v1 receipt with a result",
        xk,
        mutate(&rv, "receipt.v2", "receipt.v1").into_bytes(),
    );
    add(
        "wrong_scheme: a v3 receipt",
        xk,
        mutate(&rv, "receipt.v2", "receipt.v3").into_bytes(),
    );
    add(
        "too_large: a v2 receipt",
        xk,
        format!("{rv}{}", "x".repeat(3100)).into_bytes(),
    );
    out.into_iter()
        .map(|(name, kind, bytes)| {
            assert!(
                decode_kind(kind, &bytes).is_err(),
                "negative {name} decodes"
            );
            with_bytes(json!({"name": name, "kind": kind}), "canonical", &bytes)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// WebAuthn assertions

struct Wa {
    name: String,
    spki: Vec<u8>,
    rp_id: String,
    origins: Vec<String>,
    backup: &'static str,
    challenge: [u8; 32],
    auth: Vec<u8>,
    client: Vec<u8>,
    signature: Vec<u8>,
}

fn wa_challenge() -> [u8; 32] {
    sha256(PROOF_PAYLOAD)
}

fn wa_json(extra: &str) -> String {
    format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"{}"{extra}}}"#,
        encode_b64url(&wa_challenge()),
        testkit::ORIGIN
    )
}

impl Wa {
    fn new(name: &str, flags: u8, counter: u32, ext: &[u8], client: Option<Vec<u8>>) -> Self {
        let challenge = wa_challenge();
        let client = client.unwrap_or_else(|| testkit::client_data(&challenge, testkit::ORIGIN));
        let mut auth = testkit::auth_data(testkit::RP_ID, flags, counter);
        auth.extend_from_slice(ext);
        let key = testkit::p256_key();
        Self {
            name: name.to_owned(),
            spki: testkit::p256_spki(&key),
            rp_id: testkit::RP_ID.to_owned(),
            origins: vec![testkit::ORIGIN.to_owned()],
            backup: "any",
            challenge,
            signature: testkit::sign_webauthn(&key, &auth, &client),
            auth,
            client,
        }
    }

    fn client(name: &str, json: &str) -> Self {
        Self::client_bytes(name, json.as_bytes().to_vec())
    }

    fn client_bytes(name: &str, bytes: Vec<u8>) -> Self {
        Self::new(name, SYNCED, 0, b"", Some(bytes))
    }

    fn resign(mut self) -> Self {
        self.signature = testkit::sign_webauthn(&testkit::p256_key(), &self.auth, &self.client);
        self
    }

    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "params": {
                "spkiHex": hex::encode(&self.spki),
                "rpId": self.rp_id,
                "origins": self.origins,
                "backupPolicy": self.backup,
            },
            "challengeHex": hex::encode(self.challenge),
            "authenticatorDataB64url": encode_b64url(&self.auth),
            "clientDataJsonB64url": encode_b64url(&self.client),
            "signatureB64url": encode_b64url(&self.signature),
        })
    }
}

fn nested(depth: usize) -> String {
    wa_json(&format!(
        r#","x":{}0{}"#,
        "[".repeat(depth),
        "]".repeat(depth)
    ))
}

fn nonminimal_der(der: &[u8]) -> Vec<u8> {
    let r_len = usize::from(der[3]);
    let mut out = vec![0x30, der[1] + 1, 0x02, der[3] + 1, 0x00];
    out.extend_from_slice(&der[4..4 + r_len]);
    out.extend_from_slice(&der[4 + r_len..]);
    out
}

fn webauthn_cases() -> Vec<Value> {
    let b64 = encode_b64url(&wa_challenge());
    let origin = testkit::ORIGIN;
    let with_backup = |mut w: Wa, backup: &'static str| {
        w.backup = backup;
        w
    };
    let with_spki = |mut w: Wa, spki: Vec<u8>| {
        w.spki = spki;
        w
    };
    let base_spki = testkit::p256_spki(&testkit::p256_key());
    let point = testkit::p256_key().verifying_key().to_encoded_point(false);
    let compressed_point = testkit::p256_key().verifying_key().to_encoded_point(true);
    let mut flipped = base_spki.clone();
    flipped[90] ^= 1;
    let mut compressed =
        hex::decode("3039301306072a8648ce3d020106082a8648ce3d030107032200").unwrap();
    compressed.extend_from_slice(compressed_point.as_bytes());
    let mut secp256k1 = hex::decode("3056301006072a8648ce3d020106052b8104000a034200").unwrap();
    secp256k1.extend_from_slice(point.as_bytes());
    let mut high = Wa::new("accept: high-S signature", SYNCED, 0, b"", None);
    let signature = P256Signature::from_der(&high.signature).unwrap();
    high.signature = testkit::high_s(&signature).to_der().as_bytes().to_vec();
    let local = {
        let challenge = wa_challenge();
        let client = testkit::client_data(&challenge, LOCAL_ORIGIN);
        let auth = testkit::auth_data("localhost", DEVICE, 3);
        let key = testkit::p256_key_from(0x51);
        Wa {
            name: "accept: localhost passkey".into(),
            spki: testkit::p256_spki(&key),
            rp_id: "localhost".into(),
            origins: vec![LOCAL_ORIGIN.into()],
            backup: "device_bound",
            challenge,
            signature: testkit::sign_webauthn(&key, &auth, &client),
            auth,
            client,
        }
    };
    let mut truncated = Wa::new("auth_data_too_short: 36 bytes", SYNCED, 0, b"", None);
    truncated.auth.truncate(36);
    let mut other_rp = Wa::new("rp_id_hash_mismatch", SYNCED, 0, b"", None);
    other_rp.auth = testkit::auth_data("evil.example", SYNCED, 0);
    let mut non_utf8 = wa_json("").into_bytes();
    non_utf8.insert(1, 0xff);
    let mut trailing_der = Wa::new(
        "invalid_signature_encoding: trailing byte",
        SYNCED,
        0,
        b"",
        None,
    );
    trailing_der.signature.push(0);
    let mut short_der = Wa::new("invalid_signature_encoding: 7 bytes", SYNCED, 0, b"", None);
    short_der.signature = vec![0x30, 0x05, 0x02, 0x01, 0x01, 0x02, 0x00];
    let mut padded_der = Wa::new(
        "invalid_signature_encoding: non-minimal integer",
        SYNCED,
        0,
        b"",
        None,
    );
    padded_der.signature = nonminimal_der(&padded_der.signature);
    let mut wrong_key = Wa::new("bad_signature: another key", SYNCED, 0, b"", None);
    wrong_key.signature = testkit::sign_webauthn(
        &testkit::p256_key_from(0x51),
        &wrong_key.auth,
        &wrong_key.client,
    );
    let mut changed = Wa::new(
        "bad_signature: counter changed after signing",
        SYNCED,
        0,
        b"",
        None,
    );
    changed.auth[36] = 9;
    let standard = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        wa_challenge(),
    );
    let mut cases = vec![
        Wa::new("accept: synced passkey, counter 0", SYNCED, 0, b"", None),
        with_backup(
            Wa::new(
                "accept: device-bound key under device_bound",
                DEVICE,
                7,
                b"",
                None,
            ),
            "device_bound",
        ),
        Wa::new("accept: device-bound key under any", DEVICE, 7, b"", None),
        high,
        Wa::client(
            "accept: extra keys",
            &wa_json(r#","other_keys_can_be_added_here":"x","tokenBinding":{"status":"present"}"#),
        ),
        Wa::client("accept: no crossOrigin", &wa_json("")),
        Wa::client(
            "accept: duplicated unknown key",
            &wa_json(r#","x":1,"x":2"#),
        ),
        Wa::client("accept: depth 16 with the object", &nested(15)),
        Wa::client(
            "accept: brackets inside a string",
            &wa_json(&format!(r#","x":"{}\"{}""#, "[".repeat(40), "{".repeat(40))),
        ),
        Wa::client(
            "accept: whitespace around the object",
            &format!(" \n{}\t", wa_json("")),
        ),
        Wa::new(
            "accept: extension data with ED",
            SYNCED | FLAG_ED,
            0,
            &[0xa0],
            None,
        ),
        Wa::new(
            "accept: ignored flag bits",
            SYNCED | 0x02 | 0x20,
            0,
            b"",
            None,
        ),
        Wa::new(
            "accept: 1024-byte authenticator data",
            SYNCED | FLAG_ED,
            0,
            &[0u8; 987],
            None,
        ),
        local,
        with_spki(
            Wa::new("invalid_public_key: off the curve", SYNCED, 0, b"", None),
            flipped,
        ),
        with_spki(
            Wa::new("invalid_public_key: 65-byte point", SYNCED, 0, b"", None),
            point.as_bytes().to_vec(),
        ),
        with_spki(
            Wa::new("invalid_public_key: compressed", SYNCED, 0, b"", None),
            compressed,
        ),
        with_spki(
            Wa::new("invalid_public_key: secp256k1", SYNCED, 0, b"", None),
            secp256k1,
        ),
        Wa::client(
            "client_data_too_large",
            &wa_json(&format!(r#","pad":"{}""#, "x".repeat(2048))),
        ),
        Wa::client(
            "client_data_invalid: duplicate challenge",
            &wa_json(&format!(r#","challenge":"{b64}""#)),
        ),
        Wa::client(
            "client_data_invalid: duplicate type",
            &wa_json(r#","type":"webauthn.get""#),
        ),
        Wa::client(
            "client_data_invalid: escaped duplicate type",
            &wa_json(r#","type":"webauthn.get""#),
        ),
        Wa::client(
            "client_data_invalid: crossOrigin null",
            &wa_json(r#","crossOrigin":null"#),
        ),
        Wa::client(
            "client_data_invalid: crossOrigin string",
            &wa_json(r#","crossOrigin":"false""#),
        ),
        Wa::client("client_data_invalid: not JSON", "not json"),
        Wa::client(
            "client_data_invalid: trailing text",
            &format!("{} trailing", wa_json("")),
        ),
        Wa::client(
            "client_data_invalid: missing origin",
            &format!(r#"{{"type":"webauthn.get","challenge":"{b64}"}}"#),
        ),
        Wa::client(
            "client_data_invalid: origin not a string",
            &format!(r#"{{"type":"webauthn.get","challenge":"{b64}","origin":1}}"#),
        ),
        Wa::client(
            "client_data_invalid: array",
            &format!(r#"["webauthn.get","{b64}","{origin}"]"#),
        ),
        Wa::client(
            "client_data_invalid: lone surrogate",
            &wa_json("").replace(origin, r"\ud800"),
        ),
        Wa::client_bytes("client_data_invalid: not UTF-8", non_utf8),
        Wa::client(
            "client_data_invalid: BOM",
            &format!("\u{feff}{}", wa_json("")),
        ),
        Wa::client("client_data_invalid: depth 17", &nested(16)),
        Wa::client(
            "wrong_type",
            &wa_json("").replace("webauthn.get", "webauthn.create"),
        ),
        Wa::client(
            "challenge_mismatch: padded",
            &wa_json("").replace(&b64, &format!("{b64}=")),
        ),
        Wa::client(
            "challenge_mismatch: another digest",
            &wa_json("").replace(&b64, &encode_b64url(&sha256(b"another payload"))),
        ),
        Wa::client(
            "challenge_mismatch: hex",
            &wa_json("").replace(&b64, &hex::encode(wa_challenge())),
        ),
        Wa::client(
            "origin_not_allowed: uppercase host",
            &wa_json("").replace(origin, "https://APP.silvana.link"),
        ),
        Wa::client(
            "origin_not_allowed: trailing slash",
            &wa_json("").replace(origin, "https://app.silvana.link/"),
        ),
        Wa::client(
            "origin_not_allowed: parent domain",
            &wa_json("").replace(origin, "https://silvana.link"),
        ),
        Wa::client(
            "origin_not_allowed: default port",
            &wa_json("").replace(origin, "https://app.silvana.link:443"),
        ),
        Wa::client("cross_origin: true", &wa_json(r#","crossOrigin":true"#)),
        Wa::client(
            "cross_origin: topOrigin",
            &wa_json(r#","topOrigin":"https://app.silvana.link""#),
        ),
        Wa::client(
            "cross_origin: topOrigin null",
            &wa_json(r#","crossOrigin":false,"topOrigin":null"#),
        ),
        truncated.resign(),
        Wa::new(
            "auth_data_too_short: ED without data",
            SYNCED | FLAG_ED,
            0,
            b"",
            None,
        ),
        Wa::new(
            "auth_data_too_large",
            SYNCED | FLAG_ED,
            0,
            &[0u8; 988],
            None,
        ),
        other_rp.resign(),
        Wa::new("user_not_present", FLAG_UV, 0, b"", None),
        Wa::new("user_not_verified", FLAG_UP, 0, b"", None),
        Wa::new(
            "attested_data_present",
            DEVICE | FLAG_AT,
            0,
            &[0u8; 20],
            None,
        ),
        Wa::new("trailing_auth_data", SYNCED, 0, &[0xa0], None),
        Wa::new(
            "invalid_backup_flags",
            FLAG_UP | FLAG_UV | FLAG_BS,
            0,
            b"",
            None,
        ),
        with_backup(
            Wa::new(
                "backup_not_allowed",
                FLAG_UP | FLAG_UV | FLAG_BE,
                3,
                b"",
                None,
            ),
            "device_bound",
        ),
        trailing_der,
        short_der,
        padded_der,
        wrong_key,
        changed,
    ];
    if standard != b64 {
        cases.push(Wa::client(
            "challenge_mismatch: standard alphabet",
            &wa_json("").replace(&b64, &standard),
        ));
    }
    cases.iter().map(Wa::json).collect()
}

fn sign_count_cases() -> Vec<Value> {
    [(0u32, 0u32), (5, 6), (0, 1), (6, 6), (6, 5), (1, 0)]
        .into_iter()
        .map(|(previous, current)| json!({"previous": previous, "current": current}))
        .collect()
}

fn proof_cases() -> Vec<Value> {
    let payload = PROOF_PAYLOAD;
    let case = |name: &str, controller: &str, payload: &[u8], envelope: ProofEnvelope| {
        let v = with_bytes(
            json!({"name": name, "controller": controller}),
            "payload",
            payload,
        );
        with_bytes(v, "envelope", &envelope.encode().unwrap())
    };
    let recovery = testkit::ed25519_id(0x21);
    let local_id = *local_passkey(CapabilitySet::ALL).id();
    vec![
        case(
            "ed25519",
            "recovery",
            payload,
            testkit::ed25519_envelope(0x21, payload),
        ),
        case(
            "webauthn",
            "laptop",
            payload,
            testkit::passkey_envelope(payload),
        ),
        case(
            "webauthn: device-bound localhost key",
            "local",
            payload,
            ProofEnvelope::new(
                local_id,
                payload,
                ControllerProof::WebAuthn(local_assertion(payload, DEVICE)),
            ),
        ),
        case(
            "backup_not_allowed: synced flags on a device-bound key",
            "local",
            payload,
            ProofEnvelope::new(
                local_id,
                payload,
                ControllerProof::WebAuthn(local_assertion(payload, SYNCED)),
            ),
        ),
        case(
            "controller_mismatch",
            "recovery",
            payload,
            testkit::ed25519_envelope(0x81, payload),
        ),
        case(
            "payload_mismatch",
            "recovery",
            b"another payload, long enough to look like one",
            testkit::ed25519_envelope(0x21, payload),
        ),
        case(
            "proof_type_mismatch",
            "laptop",
            payload,
            ProofEnvelope::new(testkit::passkey_id(), payload, ed_proof(0x21, payload)),
        ),
        case(
            "proof_type_mismatch: webauthn proof for ed25519",
            "recovery",
            payload,
            ProofEnvelope::new(
                recovery,
                payload,
                ControllerProof::WebAuthn(testkit::assertion_over(payload)),
            ),
        ),
        case(
            "bad_signature: ed25519 over the hash",
            "recovery",
            payload,
            ProofEnvelope::new(recovery, payload, ed_proof(0x21, &sha256(payload))),
        ),
        case(
            "challenge_mismatch: assertion over other bytes",
            "laptop",
            payload,
            ProofEnvelope::new(
                testkit::passkey_id(),
                payload,
                ControllerProof::WebAuthn(testkit::assertion_over(b"other bytes")),
            ),
        ),
    ]
}

fn link_cases() -> Vec<Value> {
    let invite = testkit::invite_descriptor()
        .commitment()
        .unwrap()
        .to_string();
    let party_commitment = party_descriptor().commitment().unwrap().to_string();
    let add = add_local_descriptor().commitment().unwrap().to_string();
    let agent1 = testkit::agent_party().to_string();
    let agent2 = agent(2).to_string();
    let devnet = json!({"network": "devnet"});
    let case = |name: &str,
                descriptor: &str,
                commitment: &str,
                network: &str,
                expected: Option<&str>,
                allow: bool,
                rp: &Value,
                now: UtcTime| {
        json!({
            "name": name,
            "descriptor": descriptor,
            "commitmentHex": commitment,
            "network": network,
            "expectedParty": expected,
            "allowInvite": allow,
            "rpPolicy": rp,
            "now": now.to_string(),
        })
    };
    vec![
        case(
            "invite accepted for onboarding",
            "link.invite",
            &invite,
            "devnet",
            Some(&agent1),
            true,
            &devnet,
            at(0),
        ),
        case(
            "invite_not_allowed",
            "link.invite",
            &invite,
            "devnet",
            Some(&agent1),
            false,
            &devnet,
            at(0),
        ),
        case(
            "invite with no expected party",
            "link.invite",
            &invite,
            "devnet",
            None,
            false,
            &devnet,
            at(0),
        ),
        case(
            "commitment_mismatch",
            "link.invite",
            &party_commitment,
            "devnet",
            Some(&agent1),
            true,
            &devnet,
            at(0),
        ),
        case(
            "wrong_network",
            "link.invite",
            &invite,
            "mainnet",
            Some(&agent1),
            true,
            &json!({"network": "mainnet"}),
            at(0),
        ),
        case(
            "wrong_network: rp policy of another network",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent1),
            false,
            &json!({"network": "mainnet"}),
            at(0),
        ),
        case(
            "party-bound descriptor",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent1),
            false,
            &devnet,
            at(0),
        ),
        case(
            "party_mismatch",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent2),
            false,
            &devnet,
            at(0),
        ),
        case(
            "valid at expires_at",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent1),
            false,
            &devnet,
            at(7 * DAY),
        ),
        case(
            "expired",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent1),
            false,
            &devnet,
            at(7 * DAY + 1),
        ),
        case(
            "rp_id_not_allowed",
            "link.party",
            &party_commitment,
            "devnet",
            Some(&agent1),
            false,
            &json!({"network": "devnet", "rpIds": ["silvana.one"]}),
            at(0),
        ),
        case(
            "rp_id_not_allowed: localhost not listed",
            "link.add",
            &add,
            "devnet",
            Some(&agent1),
            false,
            &json!({"network": "devnet", "rpIds": ["silvana.link"]}),
            at(0),
        ),
        case(
            "add_controller descriptor",
            "link.add",
            &add,
            "devnet",
            Some(&agent1),
            false,
            &devnet,
            at(DAY),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Logs, replay and the C18 delay

fn log_cases(pool: &mut Pool, main: &Main) -> Vec<Value> {
    use Capability::{Direct, Read};
    let d = DEVNET_DELAY;
    let linked = &main.states[0];
    let s4 = &main.states[3];
    let s5 = &main.states[4];
    let s8 = &main.states[7];
    let agent_key = testkit::agent_key();
    let intruder = testkit::ed_key(0x77);
    let other_party = named_party("agent-1", 9).to_string();
    let mainnet = |mut c: Value| {
        put(&mut c, "network", "mainnet");
        c
    };
    let with_party = |mut c: Value, party: &str| {
        put(&mut c, "agentParty", party);
        c
    };
    // Rows that fail before replay.
    let mut entry = main.chain.entries[1].entry().clone();
    entry.prev = Hash32([1; 32]);
    pool.entry("v.prev.2", &sign_entry(entry, &agent_key));
    let mut entry = main.chain.entries[0].entry().clone();
    entry.agent_key = intruder.verifying_key().to_bytes();
    pool.entry("v.key.1", &sign_entry(entry, &intruder));
    let mut entry = main.chain.entries[1].entry().clone();
    entry.agent_key = intruder.verifying_key().to_bytes();
    pool.entry("v.key.2", &sign_entry(entry, &intruder));
    let mut signature = *main.chain.entries[2].signature();
    signature[10] ^= 1;
    pool.row("v.sig.3", main.chain.entries[2].bytes(), &signature);
    let tampered = mutate(
        &text(main.chain.entries[3].bytes()),
        &format!("created_at={}", at(d + 20)),
        &format!("created_at={}", at(d + 21)),
    );
    pool.row(
        "v.tamper.4",
        tampered.as_bytes(),
        main.chain.entries[3].signature(),
    );
    let mut undecodable = main.chain.entries[4].bytes().to_vec();
    undecodable.push(b'x');
    pool.row("v.row.5", &undecodable, main.chain.entries[4].signature());
    let reader = ed_controller(0x81, "Backup key", caps(&[Read]));
    pool.entry(
        "f.backdated.2",
        &forge(
            linked,
            at(-1),
            at(d - 1),
            add_body(42, reader.clone(), None),
        ),
    );
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let weak_party = PartyId::parse(&format!("weak::{}", namespace_for_key(&identity))).unwrap();
    let invite = testkit::invite_descriptor();
    let weak = LogEntry {
        network: Network::Devnet,
        agent_party: weak_party.clone(),
        agent_key: identity,
        seq: 1,
        prev: Hash32([0; 32]),
        created_at: at(0),
        effective_at: at(d),
        body: link_body(&invite, true, invite.commitment().unwrap()),
    };
    pool.row("w.1", &weak.encode().unwrap(), &[0; 64]);
    // Entries that verify but fail replay.
    let empty = testkit::empty_state();
    let relink = relink_descriptor();
    let relink_commitment = relink.commitment().unwrap();
    let invite_commitment = invite.commitment().unwrap();
    pool.entry(
        "f.first_not_link.1",
        &forge(&empty, at(d), at(2 * d), add_body(42, reader.clone(), None)),
    );
    pool.entry(
        "f.link_commitment.1",
        &forge(
            &empty,
            at(0),
            at(d),
            link_body(&invite, true, Commitment([0; 16])),
        ),
    );
    pool.entry(
        "f.link_expired.1",
        &forge(
            &empty,
            at(8 * DAY),
            at(8 * DAY + d),
            link_body(&invite, true, invite_commitment),
        ),
    );
    pool.entry(
        "f.link_effective.1",
        &forge(
            &empty,
            at(0),
            at(d - 1),
            link_body(&invite, true, invite_commitment),
        ),
    );
    pool.entry(
        "f.link_controlled.2",
        &forge(
            linked,
            at(d + 5),
            at(2 * d + 5),
            link_body(&relink, false, relink_commitment),
        ),
    );
    pool.entry(
        "f.owner.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            add_body(7, reader.clone(), None),
        ),
    );
    pool.entry(
        "f.duplicate.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            add_body(42, testkit::ed25519_controller(caps(&[Read])), None),
        ),
    );
    let agent_as_controller = ed_controller(0x01, "Agent", caps(&[Read]));
    pool.entry(
        "f.agent_key.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            add_body(42, agent_as_controller, None),
        ),
    );
    let local = local_passkey(caps(&[Read]));
    pool.entry(
        "f.commitment_required.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            add_body(42, local.clone(), None),
        ),
    );
    pool.entry(
        "f.add_effective.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d - 1),
            add_body(42, reader.clone(), None),
        ),
    );
    let add = add_local_descriptor();
    let add_ref = DescriptorRef {
        nonce: *add.nonce(),
        expires_at: add.expires_at(),
        commitment: add.commitment().unwrap(),
    };
    pool.entry(
        "f.add_commitment.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            add_body(
                42,
                local.clone(),
                Some(DescriptorRef {
                    commitment: Commitment([0; 16]),
                    ..add_ref
                }),
            ),
        ),
    );
    pool.entry(
        "f.add_expired.2",
        &forge(
            linked,
            at(8 * DAY),
            at(8 * DAY + d),
            add_body(42, local, Some(add_ref)),
        ),
    );
    let remove = |id: ControllerId, forced: bool| {
        EntryBody::RemoveController(RemoveControllerBody {
            controller_id: id,
            forced,
        })
    };
    pool.entry(
        "f.forced.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            remove(testkit::ed25519_id(0x21), false),
        ),
    );
    let set = |id: ControllerId, capabilities: CapabilitySet| {
        EntryBody::SetCapabilities(SetCapabilitiesBody {
            controller_id: id,
            capabilities,
            forced: false,
        })
    };
    pool.entry(
        "f.no_change.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            set(testkit::ed25519_id(0x21), caps(&[Read])),
        ),
    );
    pool.entry(
        "f.unknown.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            remove(testkit::ed25519_id(0x81), false),
        ),
    );
    let local_id = *local_passkey(CapabilitySet::ALL).id();
    pool.entry(
        "f.remove_again.9",
        &forge(s8, at(2 * d + 35), at(3 * d + 35), remove(local_id, false)),
    );
    pool.entry(
        "f.set_direct.2",
        &forge(
            linked,
            at(2 * d),
            at(3 * d),
            set(testkit::passkey_id(), caps(&[Direct])),
        ),
    );
    // accept_policy refusals, signed at D+30 after main.4.
    let now = at(d + 30);
    let v1_hash = Hash32(sha256(&main.v1));
    let passkey = testkit::passkey_id();
    let webauthn = |bytes: &[u8]| ControllerProof::WebAuthn(testkit::assertion_over(bytes));
    pool.entry(
        "f.missing_doc.5",
        &forge(
            s4,
            now,
            now,
            accept_body(v1_hash, 1, passkey, webauthn(&main.v1)),
        ),
    );
    pool.doc_keyed("f.swapped_doc", v1_hash, &main.v3);
    pool.entry(
        "f.lacks_policy.5",
        &forge(
            s4,
            now,
            now,
            accept_body(
                v1_hash,
                1,
                testkit::ed25519_id(0x21),
                ed_proof(0x21, &main.v1),
            ),
        ),
    );
    pool.entry(
        "f.pending_signer.5",
        &forge(
            s4,
            now,
            now,
            accept_body(
                v1_hash,
                1,
                testkit::ed25519_id(0x81),
                ed_proof(0x81, &main.v1),
            ),
        ),
    );
    pool.entry(
        "f.unknown_signer.5",
        &forge(
            s4,
            now,
            now,
            accept_body(
                v1_hash,
                1,
                testkit::ed25519_id(0x99),
                ed_proof(0x99, &main.v1),
            ),
        ),
    );
    pool.entry(
        "f.version.5",
        &forge(
            s4,
            now,
            now,
            accept_body(v1_hash, 2, passkey, webauthn(&main.v1)),
        ),
    );
    pool.entry(
        "f.challenge.5",
        &forge(
            s4,
            now,
            now,
            accept_body(v1_hash, 1, passkey, webauthn(b"other bytes")),
        ),
    );
    pool.entry(
        "f.proof_type.5",
        &forge(
            s4,
            now,
            now,
            accept_body(v1_hash, 1, passkey, ed_proof(0x21, &main.v1)),
        ),
    );
    pool.entry(
        "f.policy_effective.5",
        &forge(
            s4,
            now,
            at(d + 31),
            accept_body(v1_hash, 1, passkey, webauthn(&main.v1)),
        ),
    );
    let base = testkit::policy_doc(s4, 1, now, DEFAULT_LINKED_RULES);
    let docs = vec![
        (
            "f.expired_doc",
            PolicyDocument {
                issued_at: at(d - 60),
                expires_at: at(d + 29),
                ..base.clone()
            },
        ),
        (
            "f.future_doc",
            PolicyDocument {
                issued_at: at(d + 331),
                expires_at: at(d + 3600),
                ..base.clone()
            },
        ),
        (
            "f.party_doc",
            PolicyDocument {
                agent_party: agent(2),
                ..base.clone()
            },
        ),
        (
            "f.network_doc",
            PolicyDocument {
                network: Network::Mainnet,
                ..base
            },
        ),
    ];
    let mut forged_docs = Vec::new();
    for (id, doc) in docs {
        let bytes = doc.encode().unwrap();
        pool.doc(id, &bytes);
        let row = format!("{id}.5");
        pool.entry(
            &row,
            &forge(
                s4,
                now,
                now,
                accept_body(Hash32(sha256(&bytes)), 1, passkey, webauthn(&bytes)),
            ),
        );
        forged_docs.push((row, id));
    }
    let later = at(d + 40);
    let again = testkit::policy_doc(s5, 1, later, DEFAULT_LINKED_RULES);
    let mut unchained = testkit::policy_doc(s5, 2, later, DEFAULT_LINKED_RULES);
    unchained.prev_policy_hash = Hash32([0; 32]);
    for (id, doc) in [("f.again_doc", again), ("f.unchained_doc", unchained)] {
        let bytes = doc.encode().unwrap();
        pool.doc(id, &bytes);
        pool.entry(
            &format!("{id}.6"),
            &forge(
                s5,
                later,
                later,
                accept_body(
                    Hash32(sha256(&bytes)),
                    doc.version,
                    passkey,
                    webauthn(&bytes),
                ),
            ),
        );
    }
    let probes = [at(2 * d + 70), at(3 * d + 30), at(3 * d + 60)];
    let v123 = ["main.v1", "main.v2", "main.v3"];
    let m = main_ids;
    let mut out = vec![
        log_case("every kind, twelve entries", &m(12), &v123, &probes),
        log_case("linked from an invite", &m(1), &[], &[at(0)]),
        log_case("empty_log", &[], &[], &[]),
        log_case("unexpected_seq: gap", &plus(m(1), &["main.3"]), &[], &[]),
        log_case(
            "unexpected_seq: duplicate",
            &plus(m(2), &["main.2"]),
            &[],
            &[],
        ),
        log_case("prev_mismatch", &plus(m(1), &["v.prev.2"]), &[], &[]),
        mainnet(log_case("wrong_network", &m(3), &[], &[])),
        with_party(log_case("wrong_party", &m(3), &[], &[]), &other_party),
        log_case("key_party_mismatch", &strs(&["v.key.1"]), &[], &[]),
        log_case("key_changed", &plus(m(1), &["v.key.2"]), &[], &[]),
        log_case("time_regression", &plus(m(1), &["f.backdated.2"]), &[], &[]),
        log_case(
            "bad_signature: flipped signature",
            &plus(m(2), &["v.sig.3"]),
            &[],
            &[],
        ),
        log_case(
            "bad_signature: edited byte",
            &plus(m(3), &["v.tamper.4"]),
            &[],
            &[],
        ),
        log_case(
            "decode: a row that does not decode",
            &plus(m(4), &["v.row.5"]),
            &[],
            &[],
        ),
        with_party(
            log_case(
                "weak_public_key: small-order agent key",
                &strs(&["w.1"]),
                &[],
                &[],
            ),
            weak_party.as_str(),
        ),
        log_case(
            "first_entry_not_link",
            &strs(&["f.first_not_link.1"]),
            &[],
            &[],
        ),
        log_case(
            "commitment_mismatch: link",
            &strs(&["f.link_commitment.1"]),
            &[],
            &[],
        ),
        log_case(
            "descriptor_expired: link",
            &strs(&["f.link_expired.1"]),
            &[],
            &[],
        ),
        log_case(
            "effective_at_mismatch: link",
            &strs(&["f.link_effective.1"]),
            &[],
            &[],
        ),
        log_case(
            "link_while_controlled",
            &plus(m(1), &["f.link_controlled.2"]),
            &[],
            &[],
        ),
        log_case("owner_mismatch", &plus(m(1), &["f.owner.2"]), &[], &[]),
        log_case(
            "duplicate_controller",
            &plus(m(1), &["f.duplicate.2"]),
            &[],
            &[],
        ),
        log_case(
            "agent_key_as_controller",
            &plus(m(1), &["f.agent_key.2"]),
            &[],
            &[],
        ),
        log_case(
            "commitment_required",
            &plus(m(1), &["f.commitment_required.2"]),
            &[],
            &[],
        ),
        log_case(
            "effective_at_mismatch: add",
            &plus(m(1), &["f.add_effective.2"]),
            &[],
            &[],
        ),
        log_case(
            "commitment_mismatch: add",
            &plus(m(1), &["f.add_commitment.2"]),
            &[],
            &[],
        ),
        log_case(
            "descriptor_expired: add",
            &plus(m(1), &["f.add_expired.2"]),
            &[],
            &[],
        ),
        log_case(
            "forced_mismatch: remove",
            &plus(m(1), &["f.forced.2"]),
            &[],
            &[],
        ),
        log_case(
            "forced_mismatch: set_capabilities",
            &plus(m(1), &["f.set_direct.2"]),
            &[],
            &[],
        ),
        log_case(
            "no_change: set_capabilities",
            &plus(m(1), &["f.no_change.2"]),
            &[],
            &[],
        ),
        log_case(
            "no_change: removal already pending",
            &plus(m(8), &["f.remove_again.9"]),
            &v123,
            &[],
        ),
        log_case(
            "unknown_controller",
            &plus(m(1), &["f.unknown.2"]),
            &[],
            &[],
        ),
        log_case(
            "missing_policy_document",
            &plus(m(4), &["f.missing_doc.5"]),
            &[],
            &[],
        ),
        log_case(
            "policy_document_mismatch",
            &plus(m(4), &["f.missing_doc.5"]),
            &["f.swapped_doc"],
            &[],
        ),
        log_case(
            "signer_lacks_policy",
            &plus(m(4), &["f.lacks_policy.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "unknown_controller: signer",
            &plus(m(4), &["f.unknown_signer.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "policy_version_mismatch",
            &plus(m(4), &["f.version.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "challenge_mismatch: proof over other bytes",
            &plus(m(4), &["f.challenge.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "proof_type_mismatch",
            &plus(m(4), &["f.proof_type.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "effective_at_mismatch: accept_policy",
            &plus(m(4), &["f.policy_effective.5"]),
            &["main.v1"],
            &[],
        ),
        log_case(
            "policy_version_not_increasing",
            &plus(m(5), &["f.again_doc.6"]),
            &["main.v1", "f.again_doc"],
            &[],
        ),
        log_case(
            "policy_prev_mismatch",
            &plus(m(5), &["f.unchained_doc.6"]),
            &["main.v1", "f.unchained_doc"],
            &[],
        ),
    ];
    let names = [
        "policy_expired",
        "policy_issued_in_future",
        "policy_wrong_party",
        "policy_wrong_network",
    ];
    for ((row, doc), name) in forged_docs.iter().zip(names) {
        out.push(log_case(name, &plus(m(4), &[row.as_str()]), &[*doc], &[]));
    }
    out
}

/// Policy timing on devnet; the transfers delay of the default rules is a day.
fn timeline_cases(pool: &mut Pool, main: &Main) -> Vec<Value> {
    let d = DEVNET_DELAY;
    let linked = &main.states[0];
    let mut narrow = DEFAULT_LINKED_RULES;
    narrow.transfers.enabled = false;
    let accept = |state: &ControlState, doc: &PolicyDocument, when: UtcTime| {
        let bytes = doc.encode().unwrap();
        let pending = EntryBuilder::new(state)
            .accept_policy(&bytes, &testkit::passkey_envelope(&bytes), when)
            .unwrap();
        (
            testkit::sign_pending(&pending),
            pending.state_after().clone(),
            bytes,
        )
    };
    let t = at(d + 100);
    let (entry, _, bytes) = accept(linked, &testkit::policy_doc(linked, 1, t, narrow), t);
    pool.entry("tl.narrow.2", &entry);
    pool.doc("tl.narrow", &bytes);
    let (entry, s2, bytes) = accept(
        linked,
        &testkit::policy_doc(linked, 1, t, testkit::undirected_rules()),
        t,
    );
    pool.entry("tl.2", &entry);
    pool.doc("tl.v1", &bytes);
    let mut second = testkit::undirected_rules();
    second.trading.lp_enabled = false;
    second.transfers.recipients = Allowlist {
        any: false,
        items: vec![alice()],
    };
    let (entry, s3, bytes) = accept(
        &s2,
        &testkit::policy_doc(&s2, 2, at(d + 200), second.clone()),
        at(d + 200),
    );
    pool.entry("tl.3", &entry);
    pool.doc("tl.v2", &bytes);
    let (entry, _, bytes) = accept(
        &s3,
        &testkit::policy_doc(&s3, 3, at(d + DAY + 300), second),
        at(d + DAY + 300),
    );
    pool.entry("tl.4", &entry);
    pool.doc("tl.v3", &bytes);
    let not_before = PolicyDocument {
        not_before: Some(t.checked_add_secs(3600).unwrap()),
        ..testkit::policy_doc(linked, 1, t, DEFAULT_LINKED_RULES)
    };
    let (entry, _, bytes) = accept(linked, &not_before, t);
    pool.entry("nb.2", &entry);
    pool.doc("nb.doc", &bytes);
    let later = PolicyDocument {
        not_before: Some(t.checked_add_secs(2 * DAY).unwrap()),
        ..testkit::policy_doc(linked, 1, t, testkit::undirected_rules())
    };
    let (entry, _, bytes) = accept(linked, &later, t);
    pool.entry("nbw.2", &entry);
    pool.doc("nbw.doc", &bytes);
    let m = main_ids;
    vec![
        log_case(
            "a narrowing applies at once",
            &plus(m(1), &["tl.narrow.2"]),
            &["tl.narrow"],
            &[t],
        ),
        log_case(
            "a widening waits for the transfers delay",
            &m(6),
            &["main.v1", "main.v2"],
            &[at(2 * d + DAY + 9), at(2 * d + DAY + 10)],
        ),
        log_case(
            "a narrowing during the delay cancels the widening",
            &m(7),
            &["main.v1", "main.v2", "main.v3"],
            &[at(2 * d + DAY + 10)],
        ),
        log_case(
            "a second widening replaces the first",
            &plus(m(1), &["tl.2", "tl.3"]),
            &["tl.v1", "tl.v2"],
            &[at(d + DAY + 100), at(d + DAY + 199), at(d + DAY + 200)],
        ),
        log_case(
            "a due widening is promoted before the next accept",
            &plus(m(1), &["tl.2", "tl.3", "tl.4"]),
            &["tl.v1", "tl.v2", "tl.v3"],
            &[at(d + DAY + 300)],
        ),
        log_case(
            "not_before holds back a narrowing",
            &plus(m(1), &["nb.2"]),
            &["nb.doc"],
            &[at(d + 3699), at(d + 3700)],
        ),
        log_case(
            "not_before later than the delay wins",
            &plus(m(1), &["nbw.2"]),
            &["nbw.doc"],
            &[
                at(d + DAY + 100),
                at(d + 2 * DAY + 99),
                at(d + 2 * DAY + 100),
            ],
        ),
        log_case(
            "accepted policies never expire",
            &m(12),
            &["main.v1", "main.v2", "main.v3"],
            &[at(30 * DAY)],
        ),
    ]
}

/// The controller-change delay on devnet, `D` = 3600 s.
fn controller_delay_cases(pool: &mut Pool, main: &Main, relink: &[ControlState]) -> Vec<Value> {
    let d = DEVNET_DELAY;
    let linked = &main.states[0];
    let pending = EntryBuilder::new(linked)
        .set_capabilities(
            &testkit::passkey_id(),
            caps(&[Capability::Policy]),
            true,
            at(d + 10),
        )
        .unwrap();
    pool.entry("cd.2", &testkit::sign_pending(&pending));
    let early = relink_descriptor();
    pool.entry(
        "f.relink_early.4",
        &forge(
            &relink[2],
            at(2 * d + 19),
            at(3 * d + 19),
            link_body(&early, false, early.commitment().unwrap()),
        ),
    );
    let invite = testkit::invite_descriptor();
    pool.entry(
        "f.invite_again.4",
        &forge(
            &relink[2],
            at(2 * d + 30),
            at(3 * d + 30),
            link_body(&invite, true, invite.commitment().unwrap()),
        ),
    );
    let m = main_ids;
    let relinked = plus(m(1), &["relink.2", "relink.3", "relink.4"]);
    vec![
        log_case(
            "link: controllers wait the delay",
            &m(1),
            &[],
            &[at(d - 1), at(d)],
        ),
        log_case(
            "add: effective one delay after its signature",
            &m(2),
            &[],
            &[at(2 * d - 1), at(2 * d)],
        ),
        log_case(
            "a removal keeps capabilities until it takes effect",
            &m(8),
            &["main.v1", "main.v2", "main.v3"],
            &[at(3 * d + 29), at(3 * d + 30)],
        ),
        log_case(
            "withdrawing a pending add applies at once",
            &m(10),
            &["main.v1", "main.v2", "main.v3"],
            &[at(2 * d + 50)],
        ),
        log_case(
            "dropping a capability a pending change would grant applies at once",
            &m(12),
            &["main.v1", "main.v2", "main.v3"],
            &[at(3 * d + 59), at(3 * d + 60)],
        ),
        log_case(
            "a capability removal is delayed",
            &plus(m(1), &["cd.2"]),
            &[],
            &[at(2 * d + 9), at(2 * d + 10)],
        ),
        log_case(
            "a forced full removal keeps capabilities until each removal applies",
            &plus(m(1), &["relink.2", "relink.3"]),
            &[],
            &[
                at(2 * d + 9),
                at(2 * d + 10),
                at(2 * d + 19),
                at(2 * d + 20),
            ],
        ),
        log_case(
            "a re-link once the removals apply",
            &relinked,
            &[],
            &[at(2 * d + 20), at(3 * d + 19), at(3 * d + 20)],
        ),
        log_case(
            "link_while_controlled: a re-link before the removals apply",
            &plus(m(1), &["relink.2", "relink.3", "f.relink_early.4"]),
            &[],
            &[],
        ),
        log_case(
            "invite_not_first: an invite after a full removal",
            &plus(m(1), &["relink.2", "relink.3", "f.invite_again.4"]),
            &[],
            &[],
        ),
        log_case(
            "signer_lacks_policy: a pending controller signs",
            &plus(m(4), &["f.pending_signer.5"]),
            &["main.v1"],
            &[],
        ),
    ]
}

/// The mainnet delay, `M` = 86400 s, and entries timed by the other network's delay.
fn network_delay_cases(pool: &mut Pool, all: &Chains) -> Vec<Value> {
    let (d, m) = (DEVNET_DELAY, MAINNET_DELAY);
    let mainnet = &all.mainnet;
    let controllers = vec![testkit::ed25519_controller(CapabilitySet::ALL)];
    let mainnet_link = testkit::link_descriptor_on(Network::Mainnet, controllers, 0x44);
    let devnet_link = relink_descriptor();
    let link = |descriptor: &LinkDescriptor| {
        link_body(descriptor, false, descriptor.commitment().unwrap())
    };
    let reader = ed_controller(0x83, "Second key", caps(&[Capability::Read]));
    let remove = |id: ControllerId| {
        EntryBody::RemoveController(RemoveControllerBody {
            controller_id: id,
            forced: false,
        })
    };
    let empty_mainnet = testkit::empty_state_on(Network::Mainnet);
    let empty_devnet = testkit::empty_state();
    let backup = testkit::ed25519_id(0x81);
    let rows = [
        (
            "x.mainnet_link_3600.1",
            &empty_mainnet,
            0,
            d,
            link(&mainnet_link),
        ),
        (
            "x.mainnet_link_86399.1",
            &empty_mainnet,
            0,
            m - 1,
            link(&mainnet_link),
        ),
        (
            "x.devnet_link_86400.1",
            &empty_devnet,
            0,
            m,
            link(&devnet_link),
        ),
        (
            "x.mainnet_add_3600.2",
            &mainnet[0],
            m + 10,
            d,
            add_body(42, reader.clone(), None),
        ),
        (
            "x.mainnet_add_86399.2",
            &mainnet[0],
            m + 10,
            m - 1,
            add_body(42, reader.clone(), None),
        ),
        (
            "x.devnet_add_86400.2",
            &all.main.states[0],
            d + 10,
            m,
            add_body(42, reader, None),
        ),
        (
            "x.mainnet_remove_3600.6",
            &mainnet[4],
            2 * m + 30,
            d,
            remove(backup),
        ),
        (
            "x.mainnet_remove_86399.6",
            &mainnet[4],
            2 * m + 30,
            m - 1,
            remove(backup),
        ),
        (
            "x.devnet_remove_86400.9",
            &all.main.states[7],
            2 * d + 35,
            m,
            remove(backup),
        ),
    ];
    for (id, state, created, delay, body) in rows {
        let entry = forge(state, at(created), at(created + delay), body);
        pool.entry(id, &entry);
    }
    let mn = |n: usize| ids("mainnet", n);
    let v123 = ["main.v1", "main.v2", "main.v3"];
    let mismatch =
        |name: &str, entries: Vec<String>, docs: &[&str]| log_case(name, &entries, docs, &[]);
    vec![
        on_mainnet(log_case(
            "mainnet link: controllers wait the mainnet delay",
            &mn(1),
            &[],
            &[at(m - 1), at(m)],
        )),
        on_mainnet(log_case(
            "mainnet add: effective one mainnet delay after its signature",
            &mn(2),
            &[],
            &[at(2 * m + 9), at(2 * m + 10)],
        )),
        on_mainnet(log_case(
            "mainnet withdrawal: a pending add withdrawn applies at once",
            &mn(4),
            &[],
            &[at(m + 30)],
        )),
        on_mainnet(log_case(
            "mainnet removal: capabilities kept until it takes effect",
            &mn(6),
            &["mainnet.v1"],
            &[at(3 * m + 29), at(3 * m + 30)],
        )),
        on_mainnet(log_case(
            "mainnet: the whole log once every change applied",
            &mn(7),
            &["mainnet.v1", "mainnet.v2"],
            &[at(3 * m + 40)],
        )),
        log_case(
            "wrong_network: a mainnet log read as devnet",
            &mn(2),
            &[],
            &[],
        ),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet link on the devnet delay",
            strs(&["x.mainnet_link_3600.1"]),
            &[],
        )),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet link one second early",
            strs(&["x.mainnet_link_86399.1"]),
            &[],
        )),
        mismatch(
            "effective_at_mismatch: a devnet link on the mainnet delay",
            strs(&["x.devnet_link_86400.1"]),
            &[],
        ),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet add on the devnet delay",
            plus(mn(1), &["x.mainnet_add_3600.2"]),
            &[],
        )),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet add one second early",
            plus(mn(1), &["x.mainnet_add_86399.2"]),
            &[],
        )),
        mismatch(
            "effective_at_mismatch: a devnet add on the mainnet delay",
            plus(main_ids(1), &["x.devnet_add_86400.2"]),
            &[],
        ),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet removal on the devnet delay",
            plus(mn(5), &["x.mainnet_remove_3600.6"]),
            &["mainnet.v1"],
        )),
        on_mainnet(mismatch(
            "effective_at_mismatch: a mainnet removal one second early",
            plus(mn(5), &["x.mainnet_remove_86399.6"]),
            &["mainnet.v1"],
        )),
        mismatch(
            "effective_at_mismatch: a devnet removal on the mainnet delay",
            plus(main_ids(8), &["x.devnet_remove_86400.9"]),
            &v123,
        ),
    ]
}

/// Packages and scopes over time on both networks: a narrowing applies at acceptance, a
/// widening waits the network's controller delay, and an accept on another delay is refused.
fn rule_timeline_cases(pool: &mut Pool, all: &Chains) -> Vec<Value> {
    let (d, m) = (DEVNET_DELAY, MAINNET_DELAY);
    let mainnet = &all.mainnet;
    let passkey = testkit::passkey_id();
    let recovery = testkit::ed25519_id(0x21);
    let webauthn = |bytes: &[u8]| ControllerProof::WebAuthn(testkit::assertion_over(bytes));
    let wide_scopes = policy_on(&mainnet[5], 2, at(2 * m + 40), mainnet_wide_scopes());
    let wide_scopes = wide_scopes.encode().unwrap();
    pool.doc("mainnet.v2s", &wide_scopes);
    let pending = EntryBuilder::new(&mainnet[5])
        .accept_policy(
            &wide_scopes,
            &testkit::ed25519_envelope(0x21, &wide_scopes),
            at(2 * m + 40),
        )
        .unwrap();
    pool.entry("mainnet.scopes.7", &testkit::sign_pending(&pending));
    let mainnet_wide = &all.mainnet_docs[1];
    let package_wide = &all.package_docs[1];
    let scope_wide = &all.scoped.docs[1];
    let rows = [
        (
            "x.mainnet_packages_3600.7",
            &mainnet[5],
            2 * m + 40,
            d,
            accept_body(
                Hash32(sha256(mainnet_wide)),
                2,
                recovery,
                ed_proof(0x21, mainnet_wide),
            ),
        ),
        (
            "x.mainnet_packages_86399.7",
            &mainnet[5],
            2 * m + 40,
            m - 1,
            accept_body(
                Hash32(sha256(mainnet_wide)),
                2,
                recovery,
                ed_proof(0x21, mainnet_wide),
            ),
        ),
        (
            "x.mainnet_scopes_3600.7",
            &mainnet[5],
            2 * m + 40,
            d,
            accept_body(
                Hash32(sha256(&wide_scopes)),
                2,
                recovery,
                ed_proof(0x21, &wide_scopes),
            ),
        ),
        (
            "x.mainnet_scopes_86399.7",
            &mainnet[5],
            2 * m + 40,
            m - 1,
            accept_body(
                Hash32(sha256(&wide_scopes)),
                2,
                recovery,
                ed_proof(0x21, &wide_scopes),
            ),
        ),
        (
            "x.devnet_packages_86400.3",
            &all.packages[1],
            d + 200,
            m,
            accept_body(
                Hash32(sha256(package_wide)),
                2,
                passkey,
                webauthn(package_wide),
            ),
        ),
        (
            "x.devnet_scopes_86400.4",
            &all.scoped.states[2],
            d + 20,
            m,
            accept_body(Hash32(sha256(scope_wide)), 2, passkey, webauthn(scope_wide)),
        ),
    ];
    for (id, state, created, delay, body) in rows {
        let entry = forge(state, at(created), at(created + delay), body);
        pool.entry(id, &entry);
    }
    let m1 = main_ids(1);
    let after_link = |n: &[&str]| plus(m1.clone(), n);
    let mn = |n: usize| ids("mainnet", n);
    vec![
        log_case(
            "packages: a narrowing applies at once",
            &after_link(&["pk.2"]),
            &["pk.v1"],
            &[at(d + 100)],
        ),
        log_case(
            "packages: a widening waits the devnet delay, not the policy's",
            &after_link(&["pk.2", "pk.3"]),
            &["pk.v1", "pk.v2"],
            &[at(2 * d + 199), at(2 * d + 200)],
        ),
        log_case(
            "packages: a narrowing cancels a scheduled widening",
            &after_link(&["pk.2", "pk.3", "pk.4"]),
            &["pk.v1", "pk.v2", "pk.v3"],
            &[at(d + 300), at(2 * d + 200)],
        ),
        log_case(
            "effective_at_mismatch: a devnet package widening on the mainnet delay",
            &after_link(&["pk.2", "x.devnet_packages_86400.3"]),
            &["pk.v1", "pk.v2"],
            &[],
        ),
        log_case(
            "scopes: scoping a pending controller is a narrowing and applies at once",
            &after_link(&["scoped.2", "scoped.3"]),
            &["scoped.v1"],
            &[at(d + 10)],
        ),
        log_case(
            "scopes: a kind added waits the devnet delay",
            &after_link(&["scoped.2", "scoped.3", "scoped.4"]),
            &["scoped.v1", "scoped.v2"],
            &[at(2 * d + 19), at(2 * d + 20)],
        ),
        log_case(
            "scopes: emptying the kinds cancels a scheduled widening",
            &after_link(&["scoped.2", "scoped.3", "scoped.4", "scoped.5"]),
            &["scoped.v1", "scoped.v2", "scoped.v3"],
            &[at(d + 30), at(2 * d + 20)],
        ),
        log_case(
            "scopes: not_before holds back a narrowing",
            &after_link(&["scoped.2", "scoped.3", "scoped_nb.4"]),
            &["scoped.v1", "scoped_nb.v2"],
            &[at(2 * d + 599), at(2 * d + 600)],
        ),
        log_case(
            "effective_at_mismatch: a devnet scope widening on the mainnet delay",
            &after_link(&["scoped.2", "scoped.3", "x.devnet_scopes_86400.4"]),
            &["scoped.v1", "scoped.v2"],
            &[],
        ),
        on_mainnet(log_case(
            "mainnet packages: a narrowing applies at once",
            &mn(5),
            &["mainnet.v1"],
            &[at(2 * m + 20)],
        )),
        on_mainnet(log_case(
            "mainnet packages: a widening waits the mainnet delay",
            &mn(7),
            &["mainnet.v1", "mainnet.v2"],
            &[at(3 * m + 39), at(3 * m + 40)],
        )),
        on_mainnet(log_case(
            "mainnet scopes: dropping a scope waits the mainnet delay",
            &plus(mn(6), &["mainnet.scopes.7"]),
            &["mainnet.v1", "mainnet.v2s"],
            &[at(3 * m + 39), at(3 * m + 40)],
        )),
        on_mainnet(log_case(
            "effective_at_mismatch: a mainnet package widening on the devnet delay",
            &plus(mn(6), &["x.mainnet_packages_3600.7"]),
            &["mainnet.v1", "mainnet.v2"],
            &[],
        )),
        on_mainnet(log_case(
            "effective_at_mismatch: a mainnet package widening one second early",
            &plus(mn(6), &["x.mainnet_packages_86399.7"]),
            &["mainnet.v1", "mainnet.v2"],
            &[],
        )),
        on_mainnet(log_case(
            "effective_at_mismatch: a mainnet scope widening on the devnet delay",
            &plus(mn(6), &["x.mainnet_scopes_3600.7"]),
            &["mainnet.v1", "mainnet.v2s"],
            &[],
        )),
        on_mainnet(log_case(
            "effective_at_mismatch: a mainnet scope widening one second early",
            &plus(mn(6), &["x.mainnet_scopes_86399.7"]),
            &["mainnet.v1", "mainnet.v2s"],
            &[],
        )),
    ]
}

/// One controller just before and at each change's `effective_at`, on both networks; a
/// withdrawal applies at its own entry, so its "before" comes from the log without it.
fn standing_cases(all: &Chains) -> Vec<Value> {
    let (d, m) = (DEVNET_DELAY, MAINNET_DELAY);
    let case =
        |name: &str, entries: Vec<String>, docs: &[&str], id: ControllerId, probes: &[UtcTime]| {
            let mut c = log_case(name, &entries, docs, probes);
            put(&mut c, "controllerId", id.to_string());
            c
        };
    assert!(all.relink.len() == 4 && all.scoped.states.len() == 5);
    let main = main_ids;
    let mn = |n: usize| ids("mainnet", n);
    let relinked = plus(main(1), &["relink.2", "relink.3"]);
    let swap_key = plus(main(1), &["scoped.2", "scoped.3"]);
    let id = testkit::ed25519_id;
    let v123 = ["main.v1", "main.v2", "main.v3"];
    vec![
        case(
            "devnet link: pending until one delay after the link",
            main(1),
            &[],
            id(0x21),
            &[at(d - 1), at(d)],
        ),
        case(
            "devnet add: refused one second before effective_at, signs in at it",
            main(3),
            &[],
            id(0x81),
            &[at(2 * d + 9), at(2 * d + 10)],
        ),
        case(
            "devnet add: a scoped direct key",
            swap_key,
            &["scoped.v1"],
            id(0x84),
            &[at(2 * d - 1), at(2 * d)],
        ),
        case(
            "devnet removal: signs in until effective_at",
            relinked,
            &[],
            id(0x21),
            &[at(2 * d + 19), at(2 * d + 20)],
        ),
        case(
            "devnet withdrawal: pending one second before",
            main(9),
            &v123,
            id(0x82),
            &[at(2 * d + 49)],
        ),
        case(
            "devnet withdrawal: gone at its own entry",
            main(10),
            &v123,
            id(0x82),
            &[at(2 * d + 50)],
        ),
        case(
            "devnet capability withdrawal: the pending grant shrinks at once",
            main(12),
            &v123,
            id(0x83),
            &[at(2 * d + 70), at(3 * d + 60)],
        ),
        case(
            "an unknown controller is never recorded",
            main(12),
            &v123,
            id(0x99),
            &[at(3 * d)],
        ),
        on_mainnet(case(
            "mainnet link: pending until one delay after the link",
            mn(1),
            &[],
            id(0x21),
            &[at(m - 1), at(m)],
        )),
        on_mainnet(case(
            "mainnet add: refused one second before effective_at, signs in at it",
            mn(2),
            &[],
            id(0x81),
            &[at(2 * m + 9), at(2 * m + 10)],
        )),
        on_mainnet(case(
            "mainnet withdrawal: pending one second before",
            mn(3),
            &[],
            id(0x82),
            &[at(m + 29)],
        )),
        on_mainnet(case(
            "mainnet withdrawal: gone at its own entry",
            mn(4),
            &[],
            id(0x82),
            &[at(m + 30)],
        )),
        on_mainnet(case(
            "mainnet removal: signs in until effective_at",
            mn(6),
            &["mainnet.v1"],
            id(0x81),
            &[at(3 * m + 29), at(3 * m + 30)],
        )),
    ]
}

fn extension_cases() -> Vec<Value> {
    let m = main_ids;
    let case = |name: &str, base: Vec<String>, appended: &[String]| {
        let mut c = log_case(name, &base, &[], &[]);
        let entries = c["entries"].clone();
        put(&mut c, "base", entries);
        put(&mut c, "appended", appended);
        c.as_object_mut().unwrap().remove("entries");
        c.as_object_mut().unwrap().remove("probeTimes");
        c.as_object_mut().unwrap().remove("documents");
        c
    };
    let all = m(12);
    vec![
        case("a proper append", m(3), &all[3..]),
        case("empty_log: nothing appended", m(3), &[]),
        case("unexpected_seq: a gap", m(3), &all[4..]),
        case("unexpected_seq: an overlap", m(3), &all[2..]),
        case("key_changed", m(1), &strs(&["v.key.2"])),
    ]
}

fn comparison_cases(pool: &mut Pool, main: &Main) -> Vec<Value> {
    let pending = EntryBuilder::new(&main.states[7])
        .add_ed25519(
            ed_public(0x84),
            label("Other key"),
            caps(&[Capability::Read]),
            at(2 * DEVNET_DELAY + 45),
        )
        .unwrap();
    pool.entry("fork.9", &testkit::sign_pending(&pending));
    let (rs2, _) = linked_agent(2, vec![testkit::passkey_controller(CapabilitySet::ALL)]);
    pool.entry("cmp.agent2.1", &rs2);
    let m = main_ids;
    let case = |name: &str, local: Vec<String>, remote: Vec<String>| {
        let mut c = log_case(name, &local, &[], &[]);
        let entries = c["entries"].clone();
        put(&mut c, "local", entries);
        put(&mut c, "remote", remote);
        for key in ["entries", "probeTimes", "documents"] {
            c.as_object_mut().unwrap().remove(key);
        }
        c
    };
    let mut other = case(
        "wrong_party: logs of two agents",
        m(12),
        strs(&["cmp.agent2.1"]),
    );
    put(&mut other, "remoteAgentParty", agent(2).to_string());
    vec![
        case("equal", m(12), m(12)),
        case("remote_behind: a hidden tail", m(12), m(8)),
        case("remote_ahead", m(8), m(12)),
        case("fork", m(12), plus(m(8), &["fork.9"])),
        other,
    ]
}

fn fork_cases() -> Vec<Value> {
    let agent1 = testkit::agent_party().to_string();
    let case = |name: &str, local: &str, remote: &str, network: &str, party: &str| json!({"name": name, "network": network, "agentParty": party, "local": local, "remote": remote});
    vec![
        case(
            "two entries at seq 9",
            "main.9",
            "fork.9",
            "devnet",
            &agent1,
        ),
        case("not_a_fork", "main.9", "main.9", "devnet", &agent1),
        case("unexpected_seq", "main.9", "main.8", "devnet", &agent1),
        case("wrong_network", "main.9", "fork.9", "mainnet", &agent1),
        case(
            "wrong_party",
            "main.9",
            "fork.9",
            "devnet",
            &agent(2).to_string(),
        ),
    ]
}

fn head_check_cases(main: &Main) -> Vec<Value> {
    let head = main.head(12);
    let second = main.head(2);
    let case = |name: &str, seq: u64, hash: Hash32| {
        let mut c = log_case(name, &main_ids(12), &[], &[]);
        put(
            &mut c,
            "claimed",
            json!({"seq": seq.to_string(), "hash": hash.to_string()}),
        );
        for key in ["probeTimes", "documents"] {
            c.as_object_mut().unwrap().remove(key);
        }
        c
    };
    vec![
        case("matches", head.seq, head.hash),
        case("claim_behind: an older head", 2, second.hash),
        case("log_behind: a newer head", 13, Hash32([1; 32])),
        case("mismatch: another hash at seq 3", 3, second.hash),
        case("mismatch: seq 0", 0, Hash32([0; 32])),
    ]
}

fn jwt_cases() -> Vec<Value> {
    let hex = "ab".repeat(32);
    let zeros = "00".repeat(32);
    let upper = hex.to_uppercase();
    let short = "ab".repeat(31);
    [
        ("both absent", None, None),
        ("both present", Some("3"), Some(hex.as_str())),
        ("seq without head", Some("3"), None),
        ("head without seq", None, Some(hex.as_str())),
        ("seq 0", Some("0"), Some(zeros.as_str())),
        ("uppercase head", Some("3"), Some(upper.as_str())),
        ("short head", Some("3"), Some(short.as_str())),
        ("empty head", Some("3"), Some("")),
    ]
    .into_iter()
    .map(|(name, seq, head)| json!({"name": name, "ctlSeq": seq, "ctlHead": head}))
    .collect()
}

fn minimum_cases() -> Vec<Value> {
    let m = main_ids;
    let v123: [&str; 3] = ["main.v1", "main.v2", "main.v3"];
    let case = |name: &str,
                entries: Vec<String>,
                docs: &[&str],
                kind: &str,
                id: ControllerId,
                capabilities: Option<&str>| {
        let mut c = log_case(name, &entries, docs, &[]);
        c.as_object_mut().unwrap().remove("probeTimes");
        let mut change = json!({"kind": kind, "controllerId": id.to_string()});
        if let Some(capabilities) = capabilities {
            put(&mut change, "capabilities", capabilities);
        }
        put(&mut c, "change", change);
        c
    };
    let passkey = testkit::passkey_id();
    let recovery = testkit::ed25519_id(0x21);
    vec![
        case(
            "removing the only policy holder (a pending add counts)",
            m(1),
            &[],
            "remove",
            passkey,
            None,
        ),
        case(
            "removing one of two holders",
            m(4),
            &[],
            "remove",
            passkey,
            None,
        ),
        case(
            "dropping read from the only reader",
            m(1),
            &[],
            "set_capabilities",
            recovery,
            Some("direct"),
        ),
        case(
            "a pending removal does not count",
            m(8),
            &v123,
            "remove",
            recovery,
            None,
        ),
        case(
            "every capability held elsewhere",
            m(12),
            &v123,
            "set_capabilities",
            passkey,
            Some("direct"),
        ),
        case(
            "the phrase names a scheduled widening",
            m(6),
            &["main.v1", "main.v2"],
            "remove",
            passkey,
            None,
        ),
        case(
            "unknown_controller",
            m(1),
            &[],
            "remove",
            testkit::ed25519_id(0x99),
            None,
        ),
    ]
}

/// Each case's documents are on `network`, which sets the package and scope delay.
fn widening_cases(pool: &mut Pool) -> Vec<Value> {
    let default_id = pool.rules("rules.default", DEFAULT_LINKED_RULES);
    let mainnet_default_id = pool.rules_on(
        "rules.default.mainnet",
        Network::Mainnet,
        DEFAULT_LINKED_RULES,
    );
    let network = std::cell::Cell::new(Network::Devnet);
    let mut cases = Vec::new();
    let mut n = 0;
    let mut add = |pool: &mut Pool,
                   name: &str,
                   baseline: Option<Rules>,
                   next: Rules,
                   not_before: Option<i64>| {
        n += 1;
        let net = network.get();
        let baseline_id = match (baseline, net) {
            (Some(rules), _) => pool.rules_on(&format!("widen.{n}.base"), net, rules),
            (None, Network::Devnet) => default_id.clone(),
            (None, Network::Mainnet) => mainnet_default_id.clone(),
        };
        let next_doc = PolicyDocument {
            network: net,
            not_before: not_before.map(at),
            ..rules_doc(next)
        };
        let next_id = pool.doc(&format!("widen.{n}.next"), &next_doc.encode().unwrap());
        cases.push(json!({"name": name, "baseline": baseline_id, "next": next_id, "acceptedAt": at(100).to_string()}));
    };
    let cc_limit = |value: &str| Limits {
        unlisted: Unlisted::Unlimited,
        items: vec![limit(PolicyInstrument::Cc, value)],
    };
    let r = || DEFAULT_LINKED_RULES;
    let edit = |f: &dyn Fn(&mut Rules)| {
        let mut rules = DEFAULT_LINKED_RULES;
        f(&mut rules);
        rules
    };
    add(pool, "the same rules", None, r(), None);
    add(
        pool,
        "transfers: directed_only off",
        None,
        testkit::undirected_rules(),
        None,
    );
    add(
        pool,
        "transfers: enabled again",
        Some(edit(&|x| x.transfers.enabled = false)),
        r(),
        None,
    );
    add(
        pool,
        "transfers: recipients list to any",
        Some(edit(&|x| {
            x.transfers.recipients = Allowlist {
                any: false,
                items: vec![alice()],
            }
        })),
        r(),
        None,
    );
    add(
        pool,
        "transfers: a recipient added",
        Some(edit(&|x| {
            x.transfers.recipients = Allowlist {
                any: false,
                items: vec![alice()],
            }
        })),
        edit(&|x| {
            x.transfers.recipients = Allowlist {
                any: false,
                items: vec![alice(), bob()],
            }
        }),
        None,
    );
    add(
        pool,
        "transfers: a recipient removed (narrowing)",
        Some(edit(&|x| {
            x.transfers.recipients = Allowlist {
                any: false,
                items: vec![alice(), bob()],
            }
        })),
        edit(&|x| {
            x.transfers.recipients = Allowlist {
                any: false,
                items: vec![alice()],
            }
        }),
        None,
    );
    add(
        pool,
        "transfers: an Ethereum destination added",
        Some(edit(&|x| {
            x.transfers.eth_destinations = Allowlist {
                any: false,
                items: vec![],
            }
        })),
        edit(&|x| {
            x.transfers.eth_destinations = Allowlist {
                any: false,
                items: vec![eth()],
            }
        }),
        None,
    );
    add(
        pool,
        "transfers: a per-transfer cap rises",
        Some(edit(&|x| x.transfers.per_transfer = cc_limit("10"))),
        edit(&|x| x.transfers.per_transfer = cc_limit("20")),
        None,
    );
    add(
        pool,
        "transfers: a per-transfer cap falls (narrowing)",
        Some(edit(&|x| x.transfers.per_transfer = cc_limit("20"))),
        edit(&|x| x.transfers.per_transfer = cc_limit("10")),
        None,
    );
    add(
        pool,
        "transfers: a cap is introduced (narrowing)",
        None,
        edit(&|x| x.transfers.per_transfer = cc_limit("10")),
        None,
    );
    add(
        pool,
        "transfers: unlisted deny to unlimited",
        Some(edit(&|x| {
            x.transfers.per_day = Limits {
                unlisted: Unlisted::Deny,
                items: vec![],
            }
        })),
        r(),
        None,
    );
    add(
        pool,
        "transfers: denied to listed",
        Some(edit(&|x| {
            x.transfers.per_day = Limits {
                unlisted: Unlisted::Deny,
                items: vec![],
            }
        })),
        edit(&|x| {
            x.transfers.per_day = Limits {
                unlisted: Unlisted::Deny,
                items: vec![limit(PolicyInstrument::Cc, "5")],
            }
        }),
        None,
    );
    add(
        pool,
        "transfers: a lower delay waits for the old one",
        None,
        edit(&|x| x.delays.transfers_secs = 3600),
        None,
    );
    add(
        pool,
        "transfers: a higher delay (narrowing)",
        None,
        edit(&|x| x.delays.transfers_secs = 172_800),
        None,
    );
    add(
        pool,
        "trading: lp enabled again, no delay",
        Some(edit(&|x| x.trading.lp_enabled = false)),
        r(),
        None,
    );
    add(
        pool,
        "trading: a cap rises under the defaults, no delay",
        Some(edit(&|x| x.trading.per_trade = cc_limit("10"))),
        edit(&|x| x.trading.per_trade = cc_limit("100")),
        None,
    );
    add(
        pool,
        "trading: a market added under a trading delay",
        Some(edit(&|x| {
            x.delays.trading_secs = 3600;
            x.trading.markets = Allowlist {
                any: false,
                items: vec![MarketId::parse("CC/USDCx").unwrap()],
            };
        })),
        edit(&|x| {
            x.delays.trading_secs = 3600;
            x.trading.markets = Allowlist::ANY;
        }),
        None,
    );
    add(
        pool,
        "other: unpause",
        Some(edit(&|x| {
            x.paused = true;
            x.delays.other_secs = 7200;
        })),
        edit(&|x| x.delays.other_secs = 7200),
        None,
    );
    add(
        pool,
        "other: locks enabled again",
        Some(edit(&|x| x.locks.enabled = false)),
        r(),
        None,
    );
    add(
        pool,
        "other: usdcx enabled again",
        Some(edit(&|x| x.usdcx.enabled = false)),
        r(),
        None,
    );
    add(
        pool,
        "other: maintenance enabled again",
        Some(edit(&|x| x.maintenance.enabled = false)),
        r(),
        None,
    );
    add(
        pool,
        "other: a fee cap lifted",
        Some(edit(&|x| x.fees.cc_per_consent = Some(amount("1")))),
        r(),
        None,
    );
    add(
        pool,
        "other: the traffic cap rises",
        Some(edit(&|x| {
            x.maintenance.traffic_cc_per_day = Some(amount("5"))
        })),
        edit(&|x| x.maintenance.traffic_cc_per_day = Some(amount("6"))),
        None,
    );
    add(
        pool,
        "other: a per-lock cap rises",
        Some(edit(&|x| x.locks.per_lock = cc_limit("1"))),
        edit(&|x| x.locks.per_lock = cc_limit("2")),
        None,
    );
    add(
        pool,
        "other: a lower other delay",
        Some(edit(&|x| x.delays.other_secs = 600)),
        r(),
        None,
    );
    add(
        pool,
        "transfers and trading: the longest delay",
        Some(edit(&|x| {
            x.trading.lp_enabled = false;
            x.delays.trading_secs = 600;
        })),
        edit(&|x| {
            x.transfers.directed_only = false;
            x.delays.trading_secs = 600;
        }),
        None,
    );
    add(
        pool,
        "not_before later than the delay wins",
        None,
        testkit::undirected_rules(),
        Some(2 * DAY),
    );
    add(
        pool,
        "not_before earlier than the delay",
        None,
        testkit::undirected_rules(),
        Some(3600),
    );
    add(
        pool,
        "not_before holds back a narrowing",
        None,
        edit(&|x| x.transfers.enabled = false),
        Some(3600),
    );
    add(
        pool,
        "the cc alias: a DSO-ref list under deny is not a widening",
        Some(edit(&|x| x.trading.per_trade = cc_limit("10"))),
        edit(&|x| {
            x.trading.per_trade = Limits {
                unlisted: Unlisted::Deny,
                items: vec![limit(PolicyInstrument::Token(cc_ref()), "1000")],
            }
        }),
        None,
    );
    add(
        pool,
        "the cc alias: lifting the unlisted cap widens",
        Some(edit(&|x| x.trading.per_trade = cc_limit("10"))),
        edit(&|x| {
            x.trading.per_trade = Limits {
                unlisted: Unlisted::Unlimited,
                items: vec![limit(PolicyInstrument::Token(cc_ref()), "1000")],
            }
        }),
        None,
    );
    // Packages and scopes wait the controller delay of the document's network.
    let (a, b) = ("splice-amulet", "atomic-dvp");
    let with_packages = |names: &[&str]| Rules {
        packages: selected(names),
        ..DEFAULT_LINKED_RULES
    };
    let no_delays = |mut rules: Rules| {
        rules.delays.transfers_secs = 0;
        rules.delays.trading_secs = 0;
        rules.delays.other_secs = 0;
        rules
    };
    let (k84, k82) = (testkit::ed25519_id(0x84), testkit::ed25519_id(0x82));
    let with_scopes = |list: Vec<CommandScope>| Rules {
        scopes: scopes(list),
        ..DEFAULT_LINKED_RULES
    };
    let kinds = |id: ControllerId, kinds: &[CommandKind]| scope(id, kinds, vec![], vec![]);
    let swaps = |pairs: Vec<ScopePair>, lps: Vec<PartyId>| {
        with_scopes(vec![scope(k84, &[CommandKind::Swap], pairs, lps)])
    };
    let usdc_for_cc = || {
        pair(
            PolicyInstrument::Token(usdc_ref()),
            PolicyInstrument::Cc,
            "6",
            "150",
        )
    };
    let priced = |min: &str, max: &str| {
        pair(
            PolicyInstrument::Cc,
            PolicyInstrument::Token(usdc_ref()),
            min,
            max,
        )
    };
    use CommandKind::{Swap, TransferCc, TransferToken};
    add(
        pool,
        "packages: a selection (narrowing)",
        None,
        with_packages(&[a]),
        None,
    );
    add(
        pool,
        "packages: a selection opened to any",
        Some(with_packages(&[a])),
        r(),
        None,
    );
    add(
        pool,
        "packages: an id added",
        Some(with_packages(&[a])),
        with_packages(&[a, b]),
        None,
    );
    add(
        pool,
        "packages: an id removed (narrowing)",
        Some(with_packages(&[a, b])),
        with_packages(&[a]),
        None,
    );
    add(
        pool,
        "packages: an id replaced",
        Some(with_packages(&[a])),
        with_packages(&[b]),
        None,
    );
    add(
        pool,
        "packages: policy delays of 0 do not shorten it",
        Some(no_delays(with_packages(&[a]))),
        no_delays(r()),
        None,
    );
    add(
        pool,
        "packages: a longer other delay does not lengthen it",
        Some(edit(&|x| {
            x.packages = selected(&[a]);
            x.delays.other_secs = 7200;
        })),
        edit(&|x| x.delays.other_secs = 7200),
        None,
    );
    add(
        pool,
        "packages and transfers: the longest delay",
        Some(with_packages(&[a])),
        testkit::undirected_rules(),
        None,
    );
    add(
        pool,
        "packages: not_before later than the delay wins",
        Some(with_packages(&[a])),
        r(),
        Some(2 * DAY),
    );
    add(
        pool,
        "scopes: an entry added (narrowing)",
        None,
        with_scopes(vec![kinds(k84, &[Swap])]),
        None,
    );
    add(
        pool,
        "scopes: a kind dropped (narrowing)",
        Some(with_scopes(vec![kinds(k84, &[Swap, TransferCc])])),
        with_scopes(vec![kinds(k84, &[Swap])]),
        None,
    );
    add(
        pool,
        "scopes: the kinds emptied (narrowing)",
        Some(with_scopes(vec![kinds(k84, &[Swap])])),
        with_scopes(vec![kinds(k84, &[])]),
        None,
    );
    add(
        pool,
        "scopes: a kind added",
        Some(with_scopes(vec![kinds(k84, &[Swap])])),
        with_scopes(vec![kinds(k84, &[Swap, TransferCc])]),
        None,
    );
    add(
        pool,
        "scopes: an entry removed",
        Some(with_scopes(vec![kinds(k84, &[Swap])])),
        r(),
        None,
    );
    add(
        pool,
        "scopes: an entry listing every kind removed",
        Some(with_scopes(vec![kinds(k84, &CommandKind::ALL)])),
        r(),
        None,
    );
    add(
        pool,
        "scopes: narrowing one controller while widening another widens",
        Some(with_scopes(vec![
            kinds(k84, &[Swap]),
            kinds(k82, &[TransferCc, TransferToken]),
        ])),
        with_scopes(vec![kinds(k84, &[]), kinds(k82, &CommandKind::ALL)]),
        None,
    );
    add(
        pool,
        "scopes: narrowing two controllers narrows",
        Some(with_scopes(vec![
            kinds(k84, &[Swap]),
            kinds(k82, &[TransferCc, TransferToken]),
        ])),
        with_scopes(vec![kinds(k84, &[]), kinds(k82, &[TransferCc])]),
        None,
    );
    add(
        pool,
        "scopes: policy delays of 0 do not shorten it",
        Some(no_delays(with_scopes(vec![kinds(k84, &[Swap])]))),
        no_delays(with_scopes(vec![kinds(k84, &[Swap, TransferCc])])),
        None,
    );
    add(
        pool,
        "pairs: a pair added",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.15", "1000"), usdc_for_cc()], vec![]),
        None,
    );
    add(
        pool,
        "pairs: min_price lowered",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.1499999999", "1000")], vec![]),
        None,
    );
    add(
        pool,
        "pairs: max_sell raised",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.15", "1000.0000000001")], vec![]),
        None,
    );
    add(
        pool,
        "pairs: every pair removed",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![], vec![]),
        None,
    );
    add(
        pool,
        "pairs: min_price raised (narrowing)",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.2", "1000")], vec![]),
        None,
    );
    add(
        pool,
        "pairs: max_sell lowered (narrowing)",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.15", "10")], vec![]),
        None,
    );
    add(
        pool,
        "pairs: listed after every pair (narrowing)",
        Some(swaps(vec![], vec![])),
        swaps(vec![priced("0.15", "1000")], vec![]),
        None,
    );
    add(
        pool,
        "pairs: dropped while swaps are not allowed (narrowing)",
        Some(with_scopes(vec![scope(
            k84,
            &[TransferCc],
            vec![priced("0.15", "1000")],
            vec![],
        )])),
        with_scopes(vec![kinds(k84, &[TransferCc])]),
        None,
    );
    add(
        pool,
        "lps: an LP added",
        Some(swaps(vec![], vec![lp_a()])),
        swaps(vec![], vec![lp_a(), lp_b()]),
        None,
    );
    add(
        pool,
        "lps: the list removed",
        Some(swaps(vec![], vec![lp_a()])),
        swaps(vec![], vec![]),
        None,
    );
    add(
        pool,
        "lps: an LP dropped (narrowing)",
        Some(swaps(vec![], vec![lp_a(), lp_b()])),
        swaps(vec![], vec![lp_a()]),
        None,
    );
    add(
        pool,
        "lps: listed after every LP (narrowing)",
        Some(swaps(vec![], vec![])),
        swaps(vec![], vec![lp_a()]),
        None,
    );
    network.set(Network::Mainnet);
    add(
        pool,
        "mainnet packages: a selection (narrowing)",
        None,
        with_packages(&[a]),
        None,
    );
    add(
        pool,
        "mainnet packages: a selection opened to any waits a day",
        Some(with_packages(&[a])),
        r(),
        None,
    );
    add(
        pool,
        "mainnet packages: an id added waits a day",
        Some(with_packages(&[a])),
        with_packages(&[a, b]),
        None,
    );
    add(
        pool,
        "mainnet packages: policy delays of 0 do not shorten it",
        Some(no_delays(with_packages(&[a]))),
        no_delays(r()),
        None,
    );
    add(
        pool,
        "mainnet scopes: a kind added waits a day",
        Some(with_scopes(vec![kinds(k84, &[Swap])])),
        with_scopes(vec![kinds(k84, &[Swap, TransferCc])]),
        None,
    );
    add(
        pool,
        "mainnet scopes: the kinds emptied (narrowing)",
        Some(with_scopes(vec![kinds(k84, &[Swap])])),
        with_scopes(vec![kinds(k84, &[])]),
        None,
    );
    add(
        pool,
        "mainnet pairs: min_price lowered waits a day",
        Some(swaps(vec![priced("0.15", "1000")], vec![])),
        swaps(vec![priced("0.1499999999", "1000")], vec![]),
        None,
    );
    add(
        pool,
        "mainnet lps: an LP added waits a day",
        Some(swaps(vec![], vec![lp_a()])),
        swaps(vec![], vec![lp_a(), lp_b()]),
        None,
    );
    add(
        pool,
        "mainnet transfers: the policy's own delay still applies",
        Some(edit(&|x| x.delays.transfers_secs = 3600)),
        edit(&|x| {
            x.delays.transfers_secs = 3600;
            x.transfers.directed_only = false;
        }),
        None,
    );
    cases
}

fn submission_cases(pool: &mut Pool, all: &Chains) -> Vec<Value> {
    let d = DEVNET_DELAY;
    let main = &all.main;
    let s4 = &main.states[3];
    let s5 = &main.states[4];
    let s7 = &main.states[6];
    let s12 = &main.states[11];
    let now = at(d + 30);
    let mut docs = Vec::new();
    let mut doc = |pool: &mut Pool, id: &str, d: &PolicyDocument| {
        let bytes = d.encode().unwrap();
        pool.doc(id, &bytes);
        docs.push((id.to_owned(), bytes));
    };
    let base = testkit::policy_doc(s4, 1, now, DEFAULT_LINKED_RULES);
    let scoped = |id: ControllerId| Rules {
        scopes: vec![scope(id, &[CommandKind::Swap], vec![], vec![])],
        ..DEFAULT_LINKED_RULES
    };
    doc(
        pool,
        "submit.unknown_scope",
        &testkit::policy_doc(s4, 1, now, scoped(testkit::ed25519_id(0x99))),
    );
    doc(
        pool,
        "submit.pending_scope",
        &testkit::policy_doc(s4, 1, now, scoped(testkit::ed25519_id(0x81))),
    );
    doc(
        pool,
        "submit.packages",
        &testkit::policy_doc(
            s4,
            1,
            now,
            Rules {
                packages: selected(&["splice-amulet"]),
                ..DEFAULT_LINKED_RULES
            },
        ),
    );
    doc(
        pool,
        "submit.withdrawn_scope",
        &testkit::policy_doc(s12, 4, at(2 * d + 80), scoped(testkit::ed25519_id(0x82))),
    );
    doc(
        pool,
        "submit.undirected",
        &testkit::policy_doc(s4, 1, now, testkit::undirected_rules()),
    );
    doc(
        pool,
        "submit.wrong_party",
        &PolicyDocument {
            agent_party: agent(2),
            ..base.clone()
        },
    );
    doc(
        pool,
        "submit.mainnet",
        &PolicyDocument {
            network: Network::Mainnet,
            ..base.clone()
        },
    );
    doc(
        pool,
        "submit.future",
        &PolicyDocument {
            issued_at: at(d + 331),
            expires_at: at(d + 3600),
            ..base.clone()
        },
    );
    doc(
        pool,
        "submit.skew",
        &PolicyDocument {
            issued_at: at(d + 330),
            expires_at: at(d + 3600),
            ..base.clone()
        },
    );
    doc(
        pool,
        "submit.expired",
        &PolicyDocument {
            issued_at: at(d - 3600),
            expires_at: at(d + 29),
            ..base.clone()
        },
    );
    doc(
        pool,
        "submit.again",
        &testkit::policy_doc(s5, 1, at(d + 40), DEFAULT_LINKED_RULES),
    );
    let mut unchained = testkit::policy_doc(s5, 2, at(d + 40), DEFAULT_LINKED_RULES);
    unchained.prev_policy_hash = Hash32([0; 32]);
    doc(pool, "submit.unchained", &unchained);
    doc(
        pool,
        "submit.v4",
        &testkit::policy_doc(s7, 4, at(2 * d + 25), DEFAULT_LINKED_RULES),
    );
    let bytes_of = |id: &str| {
        docs.iter()
            .find(|(d, _)| d == id)
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| main.v1.clone())
    };
    let version0 = set_line(&text(&main.v1), "version", "0");
    pool.doc("submit.version0", version0.as_bytes());
    let case = |name: &str, entries: usize, policy: &str, envelope: ProofEnvelope, now: UtcTime| {
        let docs: Vec<&str> = ["main.v1", "main.v2", "main.v3"]
            .into_iter()
            .take(match entries {
                4 => 0,
                5 => 1,
                _ => 3,
            })
            .collect();
        let mut c = log_case(name, &main_ids(entries), &docs, &[]);
        c.as_object_mut().unwrap().remove("probeTimes");
        put(&mut c, "policy", policy);
        put(&mut c, "now", now.to_string());
        with_bytes(c, "envelope", &envelope.encode().unwrap())
    };
    let v1 = &main.v1;
    let passkey = |bytes: &[u8]| testkit::passkey_envelope(bytes);
    let ed = |start: u8, bytes: &[u8]| testkit::ed25519_envelope(start, bytes);
    let undirected = bytes_of("submit.undirected");
    let v4 = bytes_of("submit.v4");
    let general = |name: &str,
                   network: &str,
                   entries: Vec<String>,
                   docs: &[&str],
                   policy: &str,
                   envelope: ProofEnvelope,
                   now: UtcTime| {
        let mut c = log_case(name, &entries, docs, &[]);
        c.as_object_mut().unwrap().remove("probeTimes");
        put(&mut c, "network", network);
        put(&mut c, "policy", policy);
        put(&mut c, "now", now.to_string());
        with_bytes(c, "envelope", &envelope.encode().unwrap())
    };
    vec![
        case("accepted: no widening", 4, "main.v1", passkey(v1), now),
        case(
            "accepted: a transfers widening",
            4,
            "submit.undirected",
            passkey(&undirected),
            now,
        ),
        case(
            "signer_lacks_policy: a pending controller",
            4,
            "main.v1",
            ed(0x81, v1),
            now,
        ),
        case(
            "signer_lacks_policy: a reader",
            4,
            "main.v1",
            ed(0x21, v1),
            now,
        ),
        case("unknown_controller", 4, "main.v1", ed(0x99, v1), now),
        case(
            "wrong_party",
            4,
            "submit.wrong_party",
            passkey(&bytes_of("submit.wrong_party")),
            now,
        ),
        case(
            "wrong_network",
            4,
            "submit.mainnet",
            passkey(&bytes_of("submit.mainnet")),
            now,
        ),
        case(
            "issued_in_future",
            4,
            "submit.future",
            passkey(&bytes_of("submit.future")),
            now,
        ),
        case(
            "accepted: issued at the skew limit",
            4,
            "submit.skew",
            passkey(&bytes_of("submit.skew")),
            now,
        ),
        case(
            "expired",
            4,
            "submit.expired",
            passkey(&bytes_of("submit.expired")),
            now,
        ),
        case(
            "payload_mismatch",
            4,
            "main.v1",
            passkey(b"other bytes"),
            now,
        ),
        case(
            "proof_type_mismatch",
            4,
            "main.v1",
            ProofEnvelope::new(testkit::passkey_id(), v1, ed_proof(0x21, v1)),
            now,
        ),
        case(
            "version_not_increasing",
            5,
            "submit.again",
            passkey(&bytes_of("submit.again")),
            at(d + 40),
        ),
        case(
            "prev_mismatch",
            5,
            "submit.unchained",
            passkey(&bytes_of("submit.unchained")),
            at(d + 40),
        ),
        case(
            "accepted: an ed25519 signer",
            7,
            "submit.v4",
            ed(0x81, &v4),
            at(2 * d + 25),
        ),
        case(
            "bad_signature",
            7,
            "submit.v4",
            ProofEnvelope::new(
                testkit::ed25519_id(0x81),
                &v4,
                ed_proof(0x81, b"other bytes"),
            ),
            at(2 * d + 25),
        ),
        case(
            "unknown_scope_controller",
            4,
            "submit.unknown_scope",
            passkey(&bytes_of("submit.unknown_scope")),
            now,
        ),
        case(
            "accepted: a scope for a pending controller narrows at once",
            4,
            "submit.pending_scope",
            passkey(&bytes_of("submit.pending_scope")),
            now,
        ),
        case(
            "accepted: a scope for a withdrawn controller",
            12,
            "submit.withdrawn_scope",
            passkey(&bytes_of("submit.withdrawn_scope")),
            at(2 * d + 80),
        ),
        case(
            "accepted: a package selection narrows at once",
            4,
            "submit.packages",
            passkey(&bytes_of("submit.packages")),
            now,
        ),
        general(
            "accepted: a scope widening waits the devnet delay",
            "devnet",
            plus(main_ids(1), &["scoped.2", "scoped.3"]),
            &["scoped.v1"],
            "scoped.v2",
            passkey(&all.scoped.docs[1]),
            at(d + 20),
        ),
        general(
            "accepted: a package widening waits the mainnet delay",
            "mainnet",
            ids("mainnet", 6),
            &["mainnet.v1"],
            "mainnet.v2",
            ed(0x21, &all.mainnet_docs[1]),
            at(2 * MAINNET_DELAY + 40),
        ),
        general(
            "wrong_network: a devnet policy for a mainnet log",
            "mainnet",
            ids("mainnet", 6),
            &["mainnet.v1"],
            "submit.undirected",
            ed(0x21, &undirected),
            at(2 * MAINNET_DELAY + 40),
        ),
        case(
            "invalid_version: decoding",
            4,
            "submit.version0",
            passkey(version0.as_bytes()),
            now,
        ),
    ]
}

fn command_cases(chains: &Chains) -> Vec<Value> {
    let d = DEVNET_DELAY;
    let main = &chains.main;
    let relink = &chains.relink;
    let party = testkit::agent_party();
    let head = main.head(12);
    let now = main_command_time();
    let make = |head: HeadRef, party: &PartyId, issued: UtcTime, op: CommandOp| {
        command_at(head, party, CMD_ID, issued, 600, op, "Pay alice 10 CC")
            .encode()
            .unwrap()
    };
    let example = make(head, &party, now, pay_alice("10"));
    let token = command_at(
        head,
        &party,
        TOKEN_CMD_ID,
        now,
        600,
        send_bob_usdc(),
        "Send bob 2.25 USDCx",
    )
    .encode()
    .unwrap();
    let burn_bytes = command_at(
        head,
        &party,
        BURN_CMD_ID,
        now,
        600,
        burn(),
        "Burn 100 USDCx",
    )
    .encode()
    .unwrap();
    let case = |name: &str,
                entries: Vec<String>,
                command: &[u8],
                envelope: ProofEnvelope,
                now: UtcTime| {
        let docs: &[&str] = if entries.len() >= 5 {
            &["main.v1", "main.v2", "main.v3"]
        } else {
            &[]
        };
        let mut c = log_case(name, &entries, docs, &[]);
        c.as_object_mut().unwrap().remove("probeTimes");
        put(&mut c, "now", now.to_string());
        let c = with_bytes(c, "command", command);
        with_bytes(c, "envelope", &envelope.encode().unwrap())
    };
    let all = main_ids(12);
    let passkey = |bytes: &[u8]| testkit::passkey_envelope(bytes);
    let ed = |start: u8, bytes: &[u8]| testkit::ed25519_envelope(start, bytes);
    let local_id = *local_passkey(CapabilitySet::ALL).id();
    let mut wrong_network = Command::decode(&example).unwrap();
    wrong_network.network = Network::Mainnet;
    let wrong_network = wrong_network.encode().unwrap();
    let wrong_party = make(head, &agent(2), now, pay_alice("10"));
    let stale = make(main.head(11), &party, now, pay_alice("10"));
    let future = make(head, &party, at(2 * d + 221), pay_alice("10"));
    let skew = make(head, &party, at(2 * d + 220), pay_alice("10"));
    let zero = set_line(&text(&example), "op.amount", "0").into_bytes();
    let relinked = plus(main_ids(1), &["relink.2"]);
    let pending_removal = make(
        relink[1].head().unwrap(),
        &party,
        at(2 * d + 9),
        pay_alice("10"),
    );
    let swap_bytes = main_swap(main).encode().unwrap();
    vec![
        case(
            "accepted: a passkey",
            all.clone(),
            &example,
            passkey(&example),
            now,
        ),
        case(
            "accepted: an ed25519 key",
            all.clone(),
            &token,
            ed(0x21, &token),
            now,
        ),
        case(
            "accepted: a usdcx_burn command",
            all.clone(),
            &burn_bytes,
            passkey(&burn_bytes),
            now,
        ),
        case(
            "accepted: at expires_at",
            all.clone(),
            &example,
            passkey(&example),
            at(2 * d + 700),
        ),
        case(
            "expired",
            all.clone(),
            &example,
            passkey(&example),
            at(2 * d + 701),
        ),
        case(
            "issued_in_future",
            all.clone(),
            &future,
            passkey(&future),
            now,
        ),
        case(
            "accepted: issued at the skew limit",
            all.clone(),
            &skew,
            passkey(&skew),
            now,
        ),
        case(
            "signer_lacks_direct: policy only",
            all.clone(),
            &example,
            ed(0x81, &example),
            now,
        ),
        case(
            "signer_lacks_direct: a reader passkey",
            all.clone(),
            &example,
            ProofEnvelope::new(
                local_id,
                &example,
                ControllerProof::WebAuthn(local_assertion(&example, DEVICE)),
            ),
            now,
        ),
        case(
            "unknown_controller",
            all.clone(),
            &example,
            ed(0x99, &example),
            now,
        ),
        case("stale_log_head", all.clone(), &stale, passkey(&stale), now),
        case(
            "wrong_network",
            all.clone(),
            &wrong_network,
            passkey(&wrong_network),
            now,
        ),
        case(
            "wrong_party",
            all.clone(),
            &wrong_party,
            passkey(&wrong_party),
            now,
        ),
        case(
            "payload_mismatch",
            all.clone(),
            &example,
            ed(0x21, b"other bytes"),
            now,
        ),
        case(
            "bad_signature",
            all.clone(),
            &example,
            ProofEnvelope::new(
                testkit::ed25519_id(0x21),
                &example,
                ed_proof(0x21, b"other bytes"),
            ),
            now,
        ),
        case(
            "invalid_amount: decoding",
            all.clone(),
            &zero,
            passkey(&zero),
            now,
        ),
        case(
            "accepted: a pending removal keeps direct",
            relinked.clone(),
            &pending_removal,
            passkey(&pending_removal),
            at(2 * d + 9),
        ),
        case(
            "signer_lacks_direct: the removal took effect",
            relinked,
            &pending_removal,
            passkey(&pending_removal),
            at(2 * d + 10),
        ),
        with_cc(case(
            "accepted: a swap by an unscoped passkey",
            all.clone(),
            &swap_bytes,
            passkey(&swap_bytes),
            now,
        )),
        case(
            "accepted: an unscoped swap needs no Canton Coin config",
            all,
            &swap_bytes,
            passkey(&swap_bytes),
            now,
        ),
    ]
    .into_iter()
    .chain(scoped_command_cases(&chains.scoped))
    .collect()
}

fn instrument_json(i: &InstrumentRef) -> Value {
    json!({"admin": i.admin.to_string(), "id": i.id.as_str()})
}

/// The case with Canton Coin named as the verifier's config names it.
fn with_cc(mut c: Value) -> Value {
    put(&mut c, "cc", instrument_json(&cc_ref()));
    c
}

/// Command scopes: the unscoped passkey sends anything, the swap-scoped Ed25519 key 0x84
/// only swaps of CC for USDCx within its bounds, and only once its add applies at 2D.
fn scoped_command_cases(scoped: &ScopedChain) -> Vec<Value> {
    let d = DEVNET_DELAY;
    let party = testkit::agent_party();
    let scoped_entries = plus(main_ids(1), &["scoped.2", "scoped.3"]);
    let emptied_entries = plus(
        main_ids(1),
        &["scoped.2", "scoped.3", "scoped.4", "scoped.5"],
    );
    let later_entries = plus(main_ids(1), &["scoped.2", "scoped.3", "scoped_nb.4"]);
    let scoped_head = scoped.states[2].head().unwrap();
    let emptied_head = scoped.states[4].head().unwrap();
    let later_head = scoped.not_before.1.head().unwrap();
    let t = at(2 * d + 100);
    let command = |head: HeadRef, id: &str, issued: UtcTime, op: CommandOp, summary: &str| {
        command_at(head, &party, id, issued, 600, op, summary)
            .encode()
            .unwrap()
    };
    let swap_at = |head: HeadRef, issued: UtcTime, op: CommandOp| {
        command(head, SWAP_CMD_ID, issued, op, "Swap CC for USDCx")
    };
    let case =
        |name: &str, entries: &[String], docs: &[&str], bytes: &[u8], signer: u8, now: UtcTime| {
            let mut c = log_case(name, entries, docs, &[]);
            c.as_object_mut().unwrap().remove("probeTimes");
            put(&mut c, "now", now.to_string());
            let envelope = match signer {
                0 => testkit::passkey_envelope(bytes),
                start => testkit::ed25519_envelope(start, bytes),
            };
            let c = with_bytes(c, "command", bytes);
            with_cc(with_bytes(c, "envelope", &envelope.encode().unwrap()))
        };
    let scoped_docs = ["scoped.v1"];
    let emptied_docs = ["scoped.v1", "scoped.v2", "scoped.v3"];
    let later_docs = ["scoped.v1", "scoped_nb.v2"];
    let sell = swap_at(scoped_head, t, sell_cc());
    let pay = command(scoped_head, CMD_ID, t, pay_alice("10"), "Pay alice 10 CC");
    let early = swap_at(scoped_head, at(2 * d - 60), sell_cc());
    let reverse = swap_at(scoped_head, t, buy_cc());
    let below = swap_at(
        scoped_head,
        t,
        swap(cc_ref(), "1000", usdc_ref(), "149.9999999999", "2"),
    );
    let at_floor = swap_at(scoped_head, t, swap(cc_ref(), "1", usdc_ref(), "0.15", "0"));
    let above = swap_at(
        scoped_head,
        t,
        swap(cc_ref(), "1000.0000000001", usdc_ref(), "200", "2"),
    );
    let emptied_swap = swap_at(emptied_head, t, sell_cc());
    let emptied_pay = command(emptied_head, CMD_ID, t, pay_alice("10"), "Pay alice 10 CC");
    let fresh = swap_at(later_head, at(2 * d + 500), sell_cc());
    let mut unknown_cc = case(
        "swap_pair_not_allowed: Canton Coin unknown to the verifier",
        &scoped_entries,
        &scoped_docs,
        &sell,
        0x84,
        t,
    );
    unknown_cc.as_object_mut().unwrap().remove("cc");
    vec![
        case(
            "scopes: the unscoped passkey swaps",
            &scoped_entries,
            &scoped_docs,
            &sell,
            0,
            t,
        ),
        case(
            "scopes: the unscoped passkey sends transfer_cc",
            &scoped_entries,
            &scoped_docs,
            &pay,
            0,
            t,
        ),
        case(
            "scopes: the swap-scoped key swaps",
            &scoped_entries,
            &scoped_docs,
            &sell,
            0x84,
            t,
        ),
        case(
            "command_out_of_scope: the swap-scoped key sends transfer_cc",
            &scoped_entries,
            &scoped_docs,
            &pay,
            0x84,
            t,
        ),
        case(
            "signer_lacks_direct: a pending key one second before its add applies",
            &scoped_entries,
            &scoped_docs,
            &early,
            0x84,
            at(2 * d - 1),
        ),
        case(
            "scopes: the same key at the second its add applies",
            &scoped_entries,
            &scoped_docs,
            &early,
            0x84,
            at(2 * d),
        ),
        case(
            "swap_pair_not_allowed: the other direction",
            &scoped_entries,
            &scoped_docs,
            &reverse,
            0x84,
            t,
        ),
        unknown_cc,
        case(
            "swap_below_min_price: one unit under the floor",
            &scoped_entries,
            &scoped_docs,
            &below,
            0x84,
            t,
        ),
        case(
            "accepted: exactly at the floor price",
            &scoped_entries,
            &scoped_docs,
            &at_floor,
            0x84,
            t,
        ),
        case(
            "swap_above_max_sell: one unit over the cap",
            &scoped_entries,
            &scoped_docs,
            &above,
            0x84,
            t,
        ),
        case(
            "command_out_of_scope: an empty scope refuses everything",
            &emptied_entries,
            &emptied_docs,
            &emptied_swap,
            0x84,
            t,
        ),
        case(
            "scopes: the unscoped passkey still sends transfers",
            &emptied_entries,
            &emptied_docs,
            &emptied_pay,
            0,
            t,
        ),
        case(
            "scopes: verified one second before a scheduled narrowing",
            &later_entries,
            &later_docs,
            &fresh,
            0x84,
            at(2 * d + 599),
        ),
        case(
            "command_out_of_scope: the same swap once the narrowing applies",
            &later_entries,
            &later_docs,
            &fresh,
            0x84,
            at(2 * d + 600),
        ),
    ]
}

fn read_cases(pool: &mut Pool) -> (Vec<Value>, Vec<Value>) {
    let all = testkit::passkey_controller(CapabilitySet::ALL);
    let no_read = vec![
        testkit::passkey_controller(caps(&[Capability::Policy, Capability::Direct])),
        testkit::ed25519_controller(caps(&[Capability::Read])),
    ];
    for n in 1..=4u8 {
        let controllers = if n == 4 {
            no_read.clone()
        } else {
            vec![all.clone()]
        };
        let (entry, _) = linked_agent(n, controllers);
        pool.entry(&format!("rs{n}.1"), &entry);
    }
    let agent_json = |n: u8| json!({"network": "devnet", "agentParty": agent(n).to_string(), "entries": [format!("rs{n}.1")], "documents": []});
    let now = at(DAY + 100);
    let session_case =
        |name: &str, agents: &[u8], session: &[u8], envelope: &ProofEnvelope, now: UtcTime| {
            let agents: Vec<Value> = agents.iter().map(|n| agent_json(*n)).collect();
            let c = json!({"name": name, "agents": agents, "now": now.to_string()});
            let c = with_bytes(c, "session", session);
            with_bytes(c, "envelope", &envelope.encode().unwrap())
        };
    let example = session_request(&[1, 2, 3], now, 3600);
    let bytes = example.encode().unwrap();
    let envelope = testkit::passkey_envelope(&bytes);
    let with_four = session_request(&[1, 2, 4], now, 3600).encode().unwrap();
    let early = session_request(&[1, 2, 3], at(DEVNET_DELAY - 100), 3600)
        .encode()
        .unwrap();
    let future = session_request(&[1, 2, 3], at(DAY + 221), 3600)
        .encode()
        .unwrap();
    let mut foreign = session_request(&[1, 2, 3], now, 3600);
    foreign.controller_id = testkit::ed25519_id(0x21);
    let foreign = foreign.encode().unwrap();
    let mut mainnet = example.clone();
    mainnet.network = Network::Mainnet;
    let mainnet = mainnet.encode().unwrap();
    let too_long = set_line(&text(&bytes), "expires_at", "2026-11-23T13:01:41Z").into_bytes();
    let passkey = |b: &[u8]| testkit::passkey_envelope(b);
    let sessions = vec![
        session_case("accepted: three agents", &[1, 2, 3], &bytes, &envelope, now),
        session_case("unknown_agent", &[1, 2], &bytes, &envelope, now),
        session_case(
            "signer_lacks_read",
            &[1, 2, 4],
            &with_four,
            &passkey(&with_four),
            now,
        ),
        session_case(
            "signer_lacks_read: the controller is pending",
            &[1, 2, 3],
            &early,
            &passkey(&early),
            at(DEVNET_DELAY - 100),
        ),
        session_case(
            "unknown_controller",
            &[1, 2, 3],
            &foreign,
            &testkit::ed25519_envelope(0x21, &foreign),
            now,
        ),
        session_case(
            "issued_in_future",
            &[1, 2, 3],
            &future,
            &passkey(&future),
            now,
        ),
        session_case(
            "accepted: at expires_at",
            &[1, 2, 3],
            &bytes,
            &envelope,
            at(DAY + 3700),
        ),
        session_case("expired", &[1, 2, 3], &bytes, &envelope, at(DAY + 3701)),
        session_case(
            "wrong_network",
            &[1, 2, 3],
            &mainnet,
            &passkey(&mainnet),
            now,
        ),
        session_case(
            "payload_mismatch",
            &[1, 2, 3],
            &bytes,
            &passkey(b"other bytes"),
            now,
        ),
        session_case(
            "invalid_validity: decoding",
            &[1, 2, 3],
            &too_long,
            &passkey(&too_long),
            now,
        ),
    ];
    let issued = ms(at(DAY + 200)) + 250;
    let request_case = |name: &str, request: &ReadRequest, signature: Vec<u8>, now_ms: u64| {
        let c = json!({"name": name, "session": "accepted: three agents", "signatureHex": hex::encode(signature), "nowMs": now_ms.to_string()});
        with_bytes(c, "request", &request.encode().unwrap())
    };
    let signed = |r: &ReadRequest| p1363(0x61, &r.encode().unwrap());
    let example_request = read_request(&example, "ListAgentActivity", agent(2), 50, issued);
    let holdings = read_request(&example, "GetAgentHoldings", agent(2), 0, issued);
    let high = {
        let signature: P256Signature =
            testkit::p256_key_from(0x61).sign(&example_request.encode().unwrap());
        testkit::high_s(&signature).to_bytes().to_vec()
    };
    let mut wrong_network = example_request.clone();
    wrong_network.network = Network::Mainnet;
    let mut other_session = example_request.clone();
    other_session.session_id = SessionId([0x11; 16]);
    let outsider = read_request(&example, "ListAgentActivity", agent(4), 50, issued);
    let expiry_ms = ms(at(DAY + 3701));
    let late = read_request(&example, "ListAgentActivity", agent(2), 50, expiry_ms);
    let mut zero_r = signed(&example_request);
    zero_r[..32].fill(0);
    let mut short = signed(&example_request);
    short.pop();
    let requests = vec![
        request_case(
            "accepted",
            &example_request,
            signed(&example_request),
            issued + 1000,
        ),
        request_case(
            "accepted: holdings sign range 0/0",
            &holdings,
            signed(&holdings),
            issued,
        ),
        request_case(
            "accepted: a high-S signature",
            &example_request,
            high,
            issued,
        ),
        request_case(
            "accepted: 60000 ms after",
            &example_request,
            signed(&example_request),
            issued + 60_000,
        ),
        request_case(
            "outside_window: 60001 ms after",
            &example_request,
            signed(&example_request),
            issued + 60_001,
        ),
        request_case(
            "accepted: 60000 ms before",
            &example_request,
            signed(&example_request),
            issued - 60_000,
        ),
        request_case(
            "outside_window: 60001 ms before",
            &example_request,
            signed(&example_request),
            issued - 60_001,
        ),
        request_case(
            "wrong_network",
            &wrong_network,
            signed(&wrong_network),
            issued,
        ),
        request_case(
            "session_mismatch",
            &other_session,
            signed(&other_session),
            issued,
        ),
        request_case("agent_not_in_session", &outsider, signed(&outsider), issued),
        request_case("expired", &late, signed(&late), expiry_ms),
        request_case("invalid_signature_length", &example_request, short, issued),
        request_case(
            "invalid_signature_encoding: r is zero",
            &example_request,
            zero_r,
            issued,
        ),
        request_case(
            "bad_signature: another key",
            &example_request,
            p1363(0x71, &example_request.encode().unwrap()),
            issued,
        ),
    ];
    (sessions, requests)
}

fn receipt_cases(main: &Main) -> Vec<Value> {
    let agent_key = testkit::agent_key();
    let agent1 = testkit::agent_party().to_string();
    let case = |name: &str, bytes: &[u8], signature: [u8; 64], party: &str| {
        with_bytes(
            json!({"name": name, "signatureHex": hex::encode(signature), "expectedParty": party}),
            "canonical",
            bytes,
        )
    };
    let all = receipts(main);
    let mut out: Vec<Value> = all
        .iter()
        .map(|(name, r)| {
            let bytes = r.encode().unwrap();
            case(name, &bytes, agent_key.sign(&bytes).to_bytes(), &agent1)
        })
        .collect();
    let executed = all[0].1.encode().unwrap();
    let refused_bytes = all[1].1.encode().unwrap();
    let intruder = testkit::ed_key(0x77);
    let mut foreign = all[0].1.clone();
    foreign.agent_key = intruder.verifying_key().to_bytes();
    let foreign = foreign.encode().unwrap();
    out.push(case(
        "wrong_party",
        &executed,
        agent_key.sign(&executed).to_bytes(),
        &agent(2).to_string(),
    ));
    out.push(case(
        "key_party_mismatch",
        &foreign,
        intruder.sign(&foreign).to_bytes(),
        &agent1,
    ));
    out.push(case(
        "bad_signature",
        &executed,
        agent_key.sign(&refused_bytes).to_bytes(),
        &agent1,
    ));
    let swap = swap_receipt(main);
    let swapped = swap.encode().unwrap();
    out.push(case(
        "receipt.swap_executed",
        &swapped,
        agent_key.sign(&swapped).to_bytes(),
        &agent1,
    ));
    out.push(case(
        "wrong_party: a v2 receipt",
        &swapped,
        agent_key.sign(&swapped).to_bytes(),
        &agent(2).to_string(),
    ));
    let mut foreign_swap = swap.clone();
    foreign_swap.receipt.agent_key = intruder.verifying_key().to_bytes();
    let foreign_swap = foreign_swap.encode().unwrap();
    out.push(case(
        "key_party_mismatch: a v2 receipt",
        &foreign_swap,
        intruder.sign(&foreign_swap).to_bytes(),
        &agent1,
    ));
    out.push(case(
        "bad_signature: a v1 signature over a v2 receipt",
        &swapped,
        agent_key.sign(&executed).to_bytes(),
        &agent1,
    ));
    out
}

fn head_claim_cases(main: &Main) -> Vec<Value> {
    let agent_key = testkit::agent_key();
    let agent1 = testkit::agent_party().to_string();
    let case = |name: &str,
                claim: &HeadClaim,
                key: &ed25519_dalek::SigningKey,
                network: &str,
                party: &str| {
        let bytes = claim.encode().unwrap();
        let signature = key.sign(&bytes).to_bytes();
        with_bytes(
            json!({"name": name, "signatureHex": hex::encode(signature), "network": network, "expectedParty": party}),
            "canonical",
            &bytes,
        )
    };
    let empty = HeadClaim::for_state(&testkit::empty_state(), at(0));
    let linked = HeadClaim::for_state(&main.chain.state, main_receipt_time());
    let intruder = testkit::ed_key(0x77);
    let foreign = HeadClaim {
        agent_key: intruder.verifying_key().to_bytes(),
        ..linked.clone()
    };
    let mut bad = case("bad_signature", &linked, &agent_key, "devnet", &agent1);
    put(
        &mut bad,
        "signatureHex",
        hex::encode(agent_key.sign(&empty.encode().unwrap()).to_bytes()),
    );
    vec![
        case("an empty log", &empty, &agent_key, "devnet", &agent1),
        case(
            "the head of the main log",
            &linked,
            &agent_key,
            "devnet",
            &agent1,
        ),
        case("wrong_network", &linked, &agent_key, "mainnet", &agent1),
        case(
            "wrong_party",
            &linked,
            &agent_key,
            "devnet",
            &agent(2).to_string(),
        ),
        case("key_party_mismatch", &foreign, &intruder, "devnet", &agent1),
        bad,
    ]
}

fn kind_cases() -> Value {
    let table: Vec<Value> = OperationKind::ALL
        .into_iter()
        .map(|k| json!({"name": k.name()}))
        .collect();
    let ops: Vec<Value> = (-1..=28).map(|v: i64| json!({"value": v})).collect();
    json!({"table": table, "fromTransactionOperation": ops})
}

// ---------------------------------------------------------------------------
// Evaluator decisions

/// The controller a decision's command counts as verified for.
fn command_signer() -> ControllerId {
    ControllerId([2; 32])
}

fn decision_context() -> Value {
    json!({
        "agentParty": testkit::agent_party().to_string(),
        "cc": instrument_json(&cc_ref()),
        "feeParty": fee_party().to_string(),
        "prepaidTrafficParty": named_party("prepaid", 8).to_string(),
        "commandSigner": command_signer().to_string(),
    })
}

fn flow(i: &InstrumentRef, value: &str, to: &PartyId) -> Value {
    json!({"admin": i.admin.to_string(), "id": i.id.as_str(), "amount": value, "toParty": to.to_string()})
}

fn intent(kind: &str, flows: Vec<Value>) -> Value {
    json!({"kind": kind, "outgoing": flows})
}

fn decision_cases(pool: &mut Pool) -> Vec<Value> {
    let cc = cc_ref();
    let usdc = usdc_ref();
    let agent1 = testkit::agent_party();
    let fee = named_party("fee", 7);
    let prepaid = named_party("prepaid", 8);
    let (alice, bob) = (alice(), bob());
    let edit = |f: &dyn Fn(&mut Rules)| {
        let mut rules = DEFAULT_LINKED_RULES;
        f(&mut rules);
        rules
    };
    let cc_limit = |value: &str| Limits {
        unlisted: Unlisted::Unlimited,
        items: vec![limit(PolicyInstrument::Cc, value)],
    };
    let rules: Vec<(&str, Rules)> = vec![
        (
            "rules.paused",
            edit(&|x| {
                x.paused = true;
                x.transfers.directed_only = false;
            }),
        ),
        ("rules.trading_off", edit(&|x| x.trading.enabled = false)),
        ("rules.lp_off", edit(&|x| x.trading.lp_enabled = false)),
        (
            "rules.cc_cap_10",
            edit(&|x| x.trading.per_trade = cc_limit("10")),
        ),
        (
            "rules.dso_cap_10",
            edit(&|x| {
                x.trading.per_trade = Limits {
                    unlisted: Unlisted::Unlimited,
                    items: vec![limit(PolicyInstrument::Token(cc_ref()), "10")],
                }
            }),
        ),
        (
            "rules.alias_min",
            edit(&|x| {
                x.trading.per_trade = Limits {
                    unlisted: Unlisted::Unlimited,
                    items: vec![
                        limit(PolicyInstrument::Token(cc_ref()), "10"),
                        limit(PolicyInstrument::Cc, "1000"),
                    ],
                }
            }),
        ),
        (
            "rules.deny_cc_only",
            edit(&|x| {
                x.trading.per_trade = Limits {
                    unlisted: Unlisted::Deny,
                    items: vec![limit(PolicyInstrument::Cc, "1000")],
                }
            }),
        ),
        (
            "rules.deny_both",
            edit(&|x| {
                x.trading.per_trade = Limits {
                    unlisted: Unlisted::Deny,
                    items: vec![
                        limit(PolicyInstrument::Token(cc_ref()), "500"),
                        limit(PolicyInstrument::Cc, "1000"),
                    ],
                }
            }),
        ),
        (
            "rules.instruments_cc",
            edit(&|x| {
                x.trading.instruments = Allowlist {
                    any: false,
                    items: vec![PolicyInstrument::Cc],
                }
            }),
        ),
        (
            "rules.instruments_usdc",
            edit(&|x| {
                x.trading.instruments = Allowlist {
                    any: false,
                    items: vec![PolicyInstrument::Token(usdc_ref())],
                }
            }),
        ),
        (
            "rules.markets",
            edit(&|x| {
                x.trading.markets = Allowlist {
                    any: false,
                    items: vec![MarketId::parse("CC/USDCx").unwrap()],
                }
            }),
        ),
        (
            "rules.recipients_alice",
            edit(&|x| {
                x.transfers.directed_only = false;
                x.transfers.recipients = Allowlist {
                    any: false,
                    items: vec![self::alice()],
                };
            }),
        ),
        (
            "rules.eth_list",
            edit(&|x| {
                x.transfers.directed_only = false;
                x.transfers.eth_destinations = Allowlist {
                    any: false,
                    items: vec![eth()],
                };
            }),
        ),
        (
            "rules.daily_50",
            edit(&|x| {
                x.transfers.directed_only = false;
                x.transfers.per_day = cc_limit("50");
            }),
        ),
        (
            "rules.traffic_10",
            edit(&|x| x.maintenance.traffic_cc_per_day = Some(amount("10"))),
        ),
        (
            "rules.fees",
            edit(&|x| {
                x.fees.cc_per_consent = Some(amount("3"));
                x.fees.cc_per_day = Some(amount("10"));
            }),
        ),
        ("rules.locks_off", edit(&|x| x.locks.enabled = false)),
        (
            "rules.swap_scope",
            edit(&|x| {
                x.scopes = vec![scope(
                    command_signer(),
                    &[CommandKind::Swap],
                    vec![cc_for_usdc()],
                    vec![lp_a()],
                )]
            }),
        ),
    ];
    for (id, r) in rules {
        pool.rules(id, r);
    }
    let command = command_at(
        HeadRef {
            seq: 1,
            hash: Hash32([1; 32]),
        },
        &agent1,
        CMD_ID,
        at(0),
        600,
        pay_alice("10"),
        "Pay alice 10 CC",
    )
    .encode()
    .unwrap();
    let command = text(&command);
    let pay_text = command.clone();
    let case = |name: &str, rules: &str, platform: &str, intent: Value| json!({"name": name, "rules": rules, "platform": platform, "now": at(0).to_string(), "intent": intent});
    let with = |mut c: Value, key: &str, value: Value| {
        put(&mut c, key, value);
        c
    };
    let unverified = |mut i: Value| {
        put(&mut i, "coverage", "unverified");
        i
    };
    let multicall = |sub: Vec<Value>| json!({"kind": "execute_multicall", "sub": sub});
    let trade = |value: &str| intent("allocate", vec![flow(&cc, value, &bob)]);
    let usage = |bucket: &str, value: &str| json!([{"bucket": bucket, "admin": cc.admin.to_string(), "id": cc.id.as_str(), "amount": value}]);
    let max = "99999999999999999.9999999999";
    vec![
        case(
            "defaults: a trade is allowed",
            "default",
            "pinned",
            trade("5"),
        ),
        case(
            "defaults: an undirected transfer is refused",
            "default",
            "pinned",
            intent("transfer_cc", vec![flow(&cc, "10", &alice)]),
        ),
        with(
            case(
                "defaults: a matching command directs it",
                "default",
                "pinned",
                intent(
                    "transfer_cc",
                    vec![flow(&cc, "10", &alice), flow(&cc, "5", &agent1)],
                ),
            ),
            "command",
            command.clone().into(),
        ),
        with(
            case(
                "command_mismatch: another amount",
                "default",
                "pinned",
                intent("transfer_cc", vec![flow(&cc, "11", &alice)]),
            ),
            "command",
            command.clone().into(),
        ),
        with(
            case(
                "command_mismatch: a second leg",
                "default",
                "pinned",
                intent(
                    "transfer_cc",
                    vec![flow(&cc, "10", &alice), flow(&cc, "10", &bob)],
                ),
            ),
            "command",
            command.clone().into(),
        ),
        with(
            case(
                "command_mismatch: another recipient",
                "default",
                "pinned",
                intent("transfer_cc", vec![flow(&cc, "10", &bob)]),
            ),
            "command",
            command.clone().into(),
        ),
        with(
            case(
                "content_unverifiable: a command on unverified content",
                "default",
                "pinned",
                unverified(intent("transfer_cc", vec![flow(&cc, "10", &alice)])),
            ),
            "command",
            command.clone().into(),
        ),
        case(
            "C17: a split with an outflow is a transfer",
            "default",
            "pinned",
            intent("split_cc", vec![flow(&cc, "2", &alice)]),
        ),
        case(
            "C17: a split to self is maintenance",
            "default",
            "pinned",
            intent("split_cc", vec![flow(&cc, "2", &agent1)]),
        ),
        case(
            "C17: a CIP-56 transfer to self is maintenance",
            "default",
            "pinned",
            intent("transfer_cip56", vec![flow(&usdc, "5", &agent1)]),
        ),
        case(
            "C17: an unverified self-transfer",
            "default",
            "pinned",
            unverified(intent("transfer_cip56", vec![flow(&usdc, "5", &agent1)])),
        ),
        case(
            "C17: a multicall sub-operation with an outflow",
            "default",
            "pinned",
            multicall(vec![intent(
                "mc_accept_transfer_instruction",
                vec![flow(&cc, "2", &alice)],
            )]),
        ),
        case(
            "C17: prepaid traffic to the pinned party",
            "default",
            "pinned",
            intent("prepay_traffic", vec![flow(&cc, "2", &prepaid)]),
        ),
        case(
            "C17: prepaid traffic elsewhere when pinned",
            "default",
            "pinned",
            intent("prepay_traffic", vec![flow(&cc, "2", &alice)]),
        ),
        case(
            "C17: prepaid traffic anywhere when unpinned",
            "default",
            "none",
            intent("prepay_traffic", vec![flow(&cc, "2", &alice)]),
        ),
        case(
            "platform: CC to the pinned fee party is a fee",
            "default",
            "pinned",
            intent("transfer_cc", vec![flow(&cc, "2", &fee)]),
        ),
        case(
            "platform: unpinned, the same flow is a transfer",
            "default",
            "none",
            intent("transfer_cc", vec![flow(&cc, "2", &fee)]),
        ),
        case(
            "platform: another instrument to the fee party",
            "default",
            "pinned",
            intent("transfer_cc", vec![flow(&usdc, "2", &fee)]),
        ),
        case(
            "platform: a fee and an outflow",
            "default",
            "pinned",
            intent(
                "transfer_cc",
                vec![flow(&cc, "2", &fee), flow(&cc, "1", &alice)],
            ),
        ),
        case(
            "platform: unverified content",
            "default",
            "pinned",
            unverified(intent("transfer_cc", vec![flow(&cc, "2", &fee)])),
        ),
        case(
            "cc alias: at the cap",
            "rules.cc_cap_10",
            "pinned",
            trade("10"),
        ),
        case(
            "cc alias: over the cap",
            "rules.cc_cap_10",
            "pinned",
            trade("11"),
        ),
        case(
            "cc alias: the DSO ref caps CC",
            "rules.dso_cap_10",
            "pinned",
            trade("11"),
        ),
        case(
            "cc alias: the lower of both caps",
            "rules.alias_min",
            "pinned",
            trade("11"),
        ),
        case(
            "cc alias: deny needs both refs",
            "rules.deny_cc_only",
            "pinned",
            trade("1"),
        ),
        case(
            "cc alias: both refs listed",
            "rules.deny_both",
            "pinned",
            trade("400"),
        ),
        case(
            "cc alias: the alias is listed",
            "rules.instruments_cc",
            "pinned",
            trade("1"),
        ),
        case(
            "instrument_not_allowed",
            "rules.instruments_usdc",
            "pinned",
            trade("1"),
        ),
        case("paused: trading", "rules.paused", "pinned", trade("1")),
        case(
            "paused: maintenance passes",
            "rules.paused",
            "pinned",
            intent("split_cc", vec![flow(&cc, "2", &agent1)]),
        ),
        case(
            "paused: an unwind passes",
            "rules.paused",
            "pinned",
            intent("cancel_dvp_proposal", vec![]),
        ),
        case(
            "paused: identity passes",
            "rules.paused",
            "pinned",
            intent("jwt", vec![]),
        ),
        case("class_disabled", "rules.trading_off", "pinned", trade("1")),
        case(
            "class_disabled: an unwind passes",
            "rules.trading_off",
            "pinned",
            intent("cancel_dvp_proposal", vec![]),
        ),
        case(
            "class_disabled: a lock unwind passes",
            "rules.locks_off",
            "pinned",
            intent("terminate_lock", vec![]),
        ),
        case(
            "lp_disabled",
            "rules.lp_off",
            "pinned",
            intent("atomic_quote", vec![]),
        ),
        case(
            "market_not_allowed",
            "rules.markets",
            "pinned",
            with(trade("1"), "market", "cETH/USDCx".into()),
        ),
        case(
            "a listed market",
            "rules.markets",
            "pinned",
            with(trade("1"), "market", "CC/USDCx".into()),
        ),
        case(
            "recipient_not_allowed",
            "rules.recipients_alice",
            "pinned",
            intent("transfer_cc", vec![flow(&cc, "1", &bob)]),
        ),
        case(
            "a listed recipient",
            "rules.recipients_alice",
            "pinned",
            intent("transfer_cc", vec![flow(&cc, "1", &alice)]),
        ),
        case(
            "destination_not_allowed",
            "rules.eth_list",
            "pinned",
            json!({"kind": "usdcx_burn", "outgoing": [{"admin": usdc.admin.to_string(), "id": usdc.id.as_str(), "amount": "1", "toEth": "0x0000000000000000000000000000000000000001"}]}),
        ),
        with(
            case(
                "daily cap: within",
                "rules.daily_50",
                "pinned",
                intent("transfer_cc", vec![flow(&cc, "4", &alice)]),
            ),
            "usage",
            usage("transfers", "46"),
        ),
        with(
            case(
                "daily_cap",
                "rules.daily_50",
                "pinned",
                intent("transfer_cc", vec![flow(&cc, "4.0000000001", &alice)]),
            ),
            "usage",
            usage("transfers", "46"),
        ),
        case(
            "traffic: within",
            "rules.traffic_10",
            "pinned",
            intent("prepay_traffic", vec![flow(&cc, "10", &prepaid)]),
        ),
        case(
            "traffic_daily_cap",
            "rules.traffic_10",
            "pinned",
            intent("prepay_traffic", vec![flow(&cc, "10.5", &prepaid)]),
        ),
        with(
            case(
                "fees: within",
                "rules.fees",
                "pinned",
                json!({"kind": "tx_fees_authorization", "feeTotalCc": "2"}),
            ),
            "usage",
            usage("fees", "8"),
        ),
        with(
            case(
                "fee_per_consent_cap",
                "rules.fees",
                "pinned",
                json!({"kind": "pay_fee_authorization", "feeTotalCc": "3.5"}),
            ),
            "usage",
            usage("fees", "8"),
        ),
        with(
            case(
                "fee_daily_cap",
                "rules.fees",
                "pinned",
                json!({"kind": "off_chain_authorization", "feeTotalCc": "2.5"}),
            ),
            "usage",
            usage("fees", "8"),
        ),
        with(
            case(
                "fees: a DvP fee leg counts",
                "rules.fees",
                "pinned",
                intent("pay_dvp_fee", vec![flow(&cc, "2", &fee)]),
            ),
            "usage",
            usage("fees", "8"),
        ),
        case(
            "malformed_intent: a fee consent without a total",
            "rules.fees",
            "pinned",
            intent("tx_fees_authorization", vec![]),
        ),
        case(
            "multicall: one refused sub-operation refuses all",
            "default",
            "pinned",
            multicall(vec![
                intent("mc_accept_transfer_instruction", vec![]),
                intent("mc_batch_transfer", vec![flow(&cc, "1", &alice)]),
            ]),
        ),
        case(
            "malformed_intent: a nested multicall",
            "default",
            "pinned",
            multicall(vec![multicall(vec![intent("mc_allocate", vec![])])]),
        ),
        case(
            "malformed_intent: an empty multicall",
            "default",
            "pinned",
            multicall(vec![]),
        ),
        case(
            "malformed_intent: a sub-operation at the top",
            "default",
            "pinned",
            intent("mc_transfer", vec![]),
        ),
        case(
            "malformed_intent: identity with flows",
            "default",
            "pinned",
            intent("request_signature", vec![flow(&cc, "1", &alice)]),
        ),
        with(
            case(
                "command_mismatch: identity with a command",
                "default",
                "pinned",
                intent("jwt", vec![]),
            ),
            "command",
            command.into(),
        ),
        case(
            "amount_overflow",
            "rules.cc_cap_10",
            "pinned",
            intent("allocate", vec![flow(&cc, max, &bob), flow(&cc, max, &bob)]),
        ),
        case(
            "content_unverifiable: a limited class",
            "rules.cc_cap_10",
            "pinned",
            unverified(trade("1")),
        ),
        case(
            "unverified trading under the defaults",
            "default",
            "pinned",
            unverified(trade("1")),
        ),
    ]
    .into_iter()
    .chain(swap_decision_cases(&pay_text))
    .collect()
}

/// The swap binding: exactly the command's swap, CC fees to the pinned party within the cap,
/// the minimum met net of them; then the trading rules, fee caps and the signer's scope.
fn swap_decision_cases(pay_text: &str) -> Vec<Value> {
    let (cc, usdc) = (cc_ref(), usdc_ref());
    let agent1 = testkit::agent_party();
    let (fee, lp, other_lp) = (fee_party(), lp_a(), lp_b());
    let head = HeadRef {
        seq: 1,
        hash: Hash32([1; 32]),
    };
    let swap_text = |id: &str, op: CommandOp, summary: &str| {
        text(
            &command_at(head, &agent1, id, at(0), 600, op, summary)
                .encode()
                .unwrap(),
        )
    };
    let sell_text = swap_text(SWAP_CMD_ID, sell_cc(), "Swap 1000 CC for USDCx");
    let buy_text = swap_text(SWAP_BUY_CMD_ID, buy_cc(), "Swap 150 USDCx for CC");
    let leg = |i: &InstrumentRef, value: &str| json!({"admin": i.admin.to_string(), "id": i.id.as_str(), "amount": value});
    let fee_leg = |to: &PartyId, i: &InstrumentRef, value: &str| json!({"receiver": to.to_string(), "admin": i.admin.to_string(), "id": i.id.as_str(), "amount": value});
    let terms =
        |lp: &PartyId, sent: Value, received: Value, net: &str, fees: Vec<Value>, until: i64| {
            json!({
                "lp": lp.to_string(),
                "sent": sent,
                "received": received,
                "netReceived": net,
                "fees": fees,
                "validUntil": at(until).to_string(),
            })
        };
    let settle = |flows: Vec<Value>, settle: Option<Value>| {
        let mut i = json!({
            "kind": "atomic_dvp_settle",
            "outgoing": flows,
            "instruments": [instrument_json(&cc), instrument_json(&usdc)],
        });
        if let Some(s) = settle {
            put(&mut i, "settle", s);
        }
        i
    };
    // 1000 CC for 151 USDCx with a 1.5 CC fee and 48.5 CC change.
    let sell_flows = |lp: &PartyId| {
        vec![
            flow(&cc, "1000", lp),
            flow(&cc, "1.5", &fee),
            flow(&cc, "48.5", &agent1),
        ]
    };
    let sell_terms = |lp: &PartyId| {
        terms(
            lp,
            leg(&cc, "1000"),
            leg(&usdc, "151"),
            "151",
            vec![fee_leg(&fee, &cc, "1.5")],
            300,
        )
    };
    let selling = || settle(sell_flows(&lp), Some(sell_terms(&lp)));
    let edited = |edit: &dyn Fn(&mut Value)| {
        let mut i = selling();
        edit(&mut i);
        i
    };
    // 150 USDCx for `received` CC with a 1.5 CC fee, `net` CC net of it.
    let buying = |received: &str, net: &str| {
        settle(
            vec![flow(&usdc, "150", &lp), flow(&cc, "1.5", &fee)],
            Some(terms(
                &lp,
                leg(&usdc, "150"),
                leg(&cc, received),
                net,
                vec![fee_leg(&fee, &cc, "1.5")],
                300,
            )),
        )
    };
    let case = |name: &str, rules: &str, platform: &str, command: &str, intent: Value| {
        json!({
            "name": name,
            "rules": rules,
            "platform": platform,
            "now": at(0).to_string(),
            "intent": intent,
            "command": command,
        })
    };
    let sell = sell_text.as_str();
    let buy = buy_text.as_str();
    let mut stale = case(
        "command_out_of_scope: verified at another clock",
        "default",
        "pinned",
        sell,
        selling(),
    );
    put(&mut stale, "verifiedAt", at(1).to_string());
    let mut daily = case(
        "fee_daily_cap: a swap's fee counts",
        "rules.fees",
        "pinned",
        sell,
        selling(),
    );
    put(
        &mut daily,
        "usage",
        json!([{"bucket": "fees", "admin": cc.admin.to_string(), "id": cc.id.as_str(), "amount": "9"}]),
    );
    let feeless = settle(
        vec![flow(&cc, "1000", &lp), flow(&cc, "50", &agent1)],
        Some(terms(
            &lp,
            leg(&cc, "1000"),
            leg(&usdc, "151"),
            "151",
            vec![],
            300,
        )),
    );
    let elsewhere = settle(sell_flows(&other_lp), Some(sell_terms(&other_lp)));
    let transfer = intent("transfer_cc", vec![flow(&cc, "10", &alice())]);
    vec![
        case("swap: selling CC", "default", "pinned", sell, selling()),
        case(
            "swap: buying CC, the minimum met net of the fee",
            "default",
            "pinned",
            buy,
            buying("991.5", "990"),
        ),
        case(
            "command_mismatch: buying CC, one unit short net of the fee",
            "default",
            "pinned",
            buy,
            buying("991.4999999999", "989.9999999999"),
        ),
        case(
            "command_mismatch: the net received the content shows is short",
            "default",
            "pinned",
            buy,
            buying("991.5", "989.9999999999"),
        ),
        case(
            "command_mismatch: a sold amount one unit too large",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                i["settle"]["sent"]["amount"] = json!("1000.0000000001");
                i["outgoing"][0]["amount"] = json!("1000.0000000001");
            }),
        ),
        case(
            "command_mismatch: an extra outflow",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                let extra = flow(&usdc, "1", &bob());
                i["outgoing"].as_array_mut().unwrap().push(extra);
            }),
        ),
        case(
            "command_mismatch: a fee to the LP",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                i["settle"]["fees"] = json!([fee_leg(&lp, &cc, "1.5")]);
                i["outgoing"][1] = flow(&cc, "1.5", &lp);
            }),
        ),
        case(
            "command_mismatch: a fee in another instrument",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                i["settle"]["fees"] = json!([fee_leg(&fee, &usdc, "1.5")]);
                i["outgoing"][1] = flow(&usdc, "1.5", &fee);
            }),
        ),
        case(
            "command_mismatch: fees over the cap",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                let fees = [
                    fee_leg(&fee, &cc, "1.5"),
                    fee_leg(&fee, &cc, "0.5000000001"),
                ];
                i["settle"]["fees"] = json!(fees);
                i["outgoing"][2] = flow(&cc, "0.5000000001", &fee);
            }),
        ),
        case(
            "command_mismatch: LP change in the outflows",
            "default",
            "pinned",
            sell,
            edited(&|i| {
                let change = flow(&usdc, "849", &lp);
                i["outgoing"].as_array_mut().unwrap().push(change);
            }),
        ),
        case(
            "command_mismatch: no settle terms",
            "default",
            "pinned",
            sell,
            settle(sell_flows(&lp), None),
        ),
        case(
            "command_mismatch: a quote that outlives the command",
            "default",
            "pinned",
            sell,
            edited(&|i| i["settle"]["validUntil"] = json!(at(601).to_string())),
        ),
        case(
            "swap: a quote valid until the command expires",
            "default",
            "pinned",
            sell,
            edited(&|i| i["settle"]["validUntil"] = json!(at(600).to_string())),
        ),
        case(
            "command_mismatch: a fee with no fee party pinned",
            "default",
            "none",
            sell,
            selling(),
        ),
        case(
            "swap: a fee-less quote with no fee party pinned",
            "default",
            "none",
            sell,
            feeless,
        ),
        case(
            "command_mismatch: a swap command on a transfer",
            "default",
            "pinned",
            sell,
            intent("transfer_cc", vec![flow(&cc, "1000", &lp)]),
        ),
        case("paused: a swap", "rules.paused", "pinned", sell, selling()),
        case(
            "class_disabled: a swap",
            "rules.trading_off",
            "pinned",
            sell,
            selling(),
        ),
        case(
            "per_operation_cap: a swap's sold leg",
            "rules.cc_cap_10",
            "pinned",
            sell,
            selling(),
        ),
        case(
            "instrument_not_allowed: a swap",
            "rules.instruments_usdc",
            "pinned",
            sell,
            selling(),
        ),
        daily,
        case(
            "swap: within its scope's pair and LP",
            "rules.swap_scope",
            "pinned",
            sell,
            selling(),
        ),
        case(
            "command_out_of_scope: an LP the scope does not list",
            "rules.swap_scope",
            "pinned",
            sell,
            elsewhere,
        ),
        case(
            "command_out_of_scope: a pair the scope does not list",
            "rules.swap_scope",
            "pinned",
            buy,
            buying("991.5", "990"),
        ),
        case(
            "command_out_of_scope: a kind the scope does not list",
            "rules.swap_scope",
            "pinned",
            pay_text,
            transfer,
        ),
        stale,
    ]
}

/// The package rule against the package ids of a transaction, given in any order.
fn package_check_cases(pool: &mut Pool) -> Vec<Value> {
    let with_packages = |names: &[&str]| Rules {
        packages: selected(names),
        ..DEFAULT_LINKED_RULES
    };
    let cc_only = pool.rules(
        "rules.packages_cc",
        with_packages(&["splice-amulet", "splice-wallet"]),
    );
    let swaps = pool.rules(
        "rules.packages_swaps",
        with_packages(&["splice-amulet", "atomic-dvp", "registry"]),
    );
    let ids = |names: &[&str]| -> Vec<String> {
        names.iter().map(|name| package(name).to_string()).collect()
    };
    let case = |name: &str, rules: &str, used: Vec<String>| json!({"name": name, "rules": rules, "packages": used});
    let mut refused = ids(&["registry", "atomic-dvp", "splice-amulet"]);
    refused.sort();
    refused.reverse();
    vec![
        case(
            "any allows every package",
            "default",
            ids(&["splice-amulet", "atomic-dvp", "registry", "cip56-token"]),
        ),
        case(
            "any allows a transaction without packages",
            "default",
            vec![],
        ),
        case(
            "a selection allows its own ids",
            &cc_only,
            ids(&["splice-amulet"]),
        ),
        case(
            "a selection allows all of them together",
            &cc_only,
            ids(&["splice-wallet", "splice-amulet"]),
        ),
        case(
            "a selection allows a transaction without packages",
            &cc_only,
            vec![],
        ),
        case(
            "package_not_allowed: a swap under a CC-only selection",
            &cc_only,
            ids(&["splice-amulet", "atomic-dvp"]),
        ),
        case(
            "package_not_allowed: the first refused id in ascending order",
            &cc_only,
            refused,
        ),
        case(
            "a swap with the AtomicDVP and registry packages selected",
            &swaps,
            ids(&["registry", "atomic-dvp", "splice-amulet"]),
        ),
        case(
            "package_not_allowed: a CIP-56 transfer outside the selection",
            &swaps,
            ids(&["splice-amulet", "cip56-token"]),
        ),
        case(
            "package_not_allowed: a selected id with one hex digit changed",
            &swaps,
            vec![{
                let mut id = package("registry").0;
                id[31] ^= 1;
                PackageId(id).to_string()
            }],
        ),
    ]
}

/// Controller sign-in challenges: the exact layout, what the signer checks, and the rpc's
/// check of the Ed25519 proof over them.
fn challenge_cases() -> Vec<Value> {
    let issued = at(DAY);
    let sample = ControllerChallenge {
        network: Network::Devnet,
        rpc: RpId::parse(DEVNET_RPC).unwrap(),
        controller_id: testkit::ed25519_id(0x81),
        agent_party: testkit::agent_party(),
        nonce: Nonce16([0x5c; 16]),
        issued_at: issued,
        expires_at: issued.checked_add_secs(60).unwrap(),
    };
    let mainnet = ControllerChallenge {
        network: Network::Mainnet,
        rpc: RpId::parse(MAINNET_RPC).unwrap(),
        ..sample.clone()
    };
    let bytes = sample.encode().unwrap();
    let text_of = |c: &ControllerChallenge| text(&c.encode().unwrap());
    let base = text_of(&sample);
    let expected = |c: &ControllerChallenge| {
        json!({
            "network": c.network.as_str(),
            "rpc": c.rpc.as_str(),
            "controllerId": c.controller_id.to_string(),
            "agentParty": c.agent_party.to_string(),
        })
    };
    let decode = |name: &str, bytes: &[u8]| {
        with_bytes(
            json!({"name": name, "function": "decode"}),
            "canonical",
            bytes,
        )
    };
    let check = |name: &str, bytes: &[u8], wanted: Value, now: UtcTime| {
        let c = json!({"name": name, "function": "check_challenge", "expected": wanted, "now": now.to_string()});
        with_bytes(c, "canonical", bytes)
    };
    let proof = |name: &str, bytes: &[u8], envelope: &ProofEnvelope, key: &[u8; 32]| {
        let c = json!({"name": name, "function": "verify_challenge_proof", "publicKeyHex": hex::encode(key)});
        let c = with_bytes(c, "canonical", bytes);
        with_bytes(c, "envelope", &envelope.encode().unwrap())
    };
    let later = |secs: i64| issued.checked_add_secs(secs).unwrap();
    let valid_for = |secs: i64| ControllerChallenge {
        expires_at: later(secs),
        ..sample.clone()
    };
    let label = |c: &str| c.repeat(63);
    let longest = ControllerChallenge {
        rpc: RpId::parse(&format!(
            "{}.{}.{}.{}",
            label("a"),
            label("b"),
            label("c"),
            "d".repeat(61)
        ))
        .unwrap(),
        agent_party: PartyId::parse(&format!(
            "{}::{}",
            "h".repeat(185),
            namespace_for_key(&[7; 32])
        ))
        .unwrap(),
        ..mainnet.clone()
    };
    let local = ControllerChallenge {
        rpc: RpId::parse("localhost").unwrap(),
        ..sample.clone()
    };
    let invalid_validity = |secs: i64| {
        let expires = format!("expires_at={}", later(60));
        mutate(&base, &expires, &format!("expires_at={}", later(secs)))
    };
    let id = sample.controller_id.to_string();
    let swapped = mutate(
        &base,
        &format!("rpc={DEVNET_RPC}\ncontroller_id={id}\n"),
        &format!("controller_id={id}\nrpc={DEVNET_RPC}\n"),
    );
    let nonce_line = format!("nonce={}\n", sample.nonce);
    let other_party = named_party("agent-2", 9);
    let key = ed_public(0x81);
    let other_key = ed_public(0x82);
    let envelope = testkit::ed25519_envelope(0x81, &bytes);
    let theirs = testkit::ed25519_envelope(0x82, &bytes);
    let misnamed = ProofEnvelope::new(testkit::ed25519_id(0x82), &bytes, ed_proof(0x81, &bytes));
    let renewed = ControllerChallenge {
        nonce: Nonce16([0x11; 16]),
        ..sample.clone()
    }
    .encode()
    .unwrap();
    let passkey = ProofEnvelope::new(
        sample.controller_id,
        &bytes,
        ControllerProof::WebAuthn(testkit::assertion_over(&bytes)),
    );
    let mut flipped = testkit::ed_key(0x81).sign(&bytes).to_bytes();
    flipped[7] ^= 1;
    let flipped = ProofEnvelope::new(
        sample.controller_id,
        &bytes,
        ControllerProof::Ed25519 { signature: flipped },
    );
    let elsewhere = ProofEnvelope::new(
        sample.controller_id,
        &bytes,
        ed_proof(0x81, b"another document"),
    );
    let mut weak = [0u8; 32];
    weak[0] = 1;
    let weak_id = controller_id_for(ControllerType::Ed25519, &weak);
    let weak_bytes = ControllerChallenge {
        controller_id: weak_id,
        ..sample.clone()
    }
    .encode()
    .unwrap();
    let forged = ProofEnvelope::new(
        weak_id,
        &weak_bytes,
        ControllerProof::Ed25519 { signature: [0; 64] },
    );
    let mainnet_bytes = mainnet.encode().unwrap();
    let mainnet_envelope = testkit::ed25519_envelope(0x81, &mainnet_bytes);
    let wanted = expected(&sample);
    let with = |mut v: Value, key: &str, value: Value| {
        put(&mut v, key, value);
        v
    };
    vec![
        decode("decode: devnet", &bytes),
        decode("decode: mainnet", &mainnet_bytes),
        decode(
            "decode: valid for one second",
            &valid_for(1).encode().unwrap(),
        ),
        decode("decode: the longest fields fit", &longest.encode().unwrap()),
        decode("decode: a localhost rpc", &local.encode().unwrap()),
        decode(
            "wrong_scheme: v2",
            mutate(&base, "controller-challenge.v1", "controller-challenge.v2").as_bytes(),
        ),
        decode(
            "wrong_scheme: a read session scheme",
            mutate(&base, "controller-challenge.v1", "read-session.v1").as_bytes(),
        ),
        decode(
            "invalid_network: testnet",
            mutate(&base, "network=devnet", "network=testnet").as_bytes(),
        ),
        decode(
            "invalid_rp_id: an uppercase rpc",
            mutate(&base, DEVNET_RPC, "Orderbook-devnet.silvana.dev").as_bytes(),
        ),
        decode(
            "invalid_rp_id: an rpc with a port",
            mutate(&base, DEVNET_RPC, "orderbook-devnet.silvana.dev:443").as_bytes(),
        ),
        decode(
            "invalid_rp_id: an rpc URL",
            mutate(&base, DEVNET_RPC, "https://orderbook-devnet.silvana.dev").as_bytes(),
        ),
        decode(
            "invalid_rp_id: an IP address",
            mutate(&base, DEVNET_RPC, "192.0.2.1").as_bytes(),
        ),
        decode(
            "invalid_hex32: an uppercase controller id",
            mutate(&base, &id, &id.to_uppercase()).as_bytes(),
        ),
        decode(
            "invalid_party: a space in the hint",
            mutate(&base, "agent_party=agent-1::", "agent_party=agent 1::").as_bytes(),
        ),
        decode(
            "invalid_hex16: a short nonce",
            mutate(&base, "nonce=5c5c", "nonce=5c").as_bytes(),
        ),
        decode(
            "invalid_time: no seconds",
            mutate(
                &base,
                &format!("issued_at={issued}"),
                &format!("issued_at={}", &issued.to_string()[..16]),
            )
            .as_bytes(),
        ),
        decode("unexpected_key: swapped lines", swapped.as_bytes()),
        decode(
            "unexpected_key: no nonce",
            mutate(&base, &nonce_line, "").as_bytes(),
        ),
        decode("trailing_line", format!("{base}extra=1\n").as_bytes()),
        decode("invalid_byte: CRLF", base.replace('\n', "\r\n").as_bytes()),
        decode(
            "too_large",
            format!("{base}{}", "x".repeat(1024)).as_bytes(),
        ),
        decode(
            "invalid_validity: zero seconds",
            invalid_validity(0).as_bytes(),
        ),
        decode(
            "invalid_validity: 61 seconds",
            invalid_validity(61).as_bytes(),
        ),
        decode(
            "invalid_validity: expires before issue",
            invalid_validity(-1).as_bytes(),
        ),
        decode("too_short", b"junk"),
        check("check: at issued_at", &bytes, wanted.clone(), issued),
        check(
            "check: 120 s before issued_at",
            &bytes,
            wanted.clone(),
            later(-120),
        ),
        check(
            "check: 120 s after issued_at",
            &bytes,
            wanted.clone(),
            later(120),
        ),
        check(
            "outside_window: 121 s before issued_at",
            &bytes,
            wanted.clone(),
            later(-121),
        ),
        check(
            "outside_window: 121 s after issued_at",
            &bytes,
            wanted.clone(),
            later(121),
        ),
        check(
            "check: a mainnet challenge on mainnet",
            &mainnet_bytes,
            expected(&mainnet),
            issued,
        ),
        check(
            "wrong_network: a devnet challenge for a mainnet signer",
            &bytes,
            with(wanted.clone(), "network", "mainnet".into()),
            issued,
        ),
        check(
            "wrong_network: a mainnet challenge for a devnet signer",
            &mainnet_bytes,
            with(expected(&mainnet), "network", "devnet".into()),
            issued,
        ),
        check(
            "wrong_rpc: the other network's rpc",
            &bytes,
            with(wanted.clone(), "rpc", MAINNET_RPC.into()),
            issued,
        ),
        check(
            "wrong_controller",
            &bytes,
            with(
                wanted.clone(),
                "controllerId",
                testkit::ed25519_id(0x82).to_string().into(),
            ),
            issued,
        ),
        check(
            "wrong_party",
            &bytes,
            with(wanted.clone(), "agentParty", other_party.to_string().into()),
            issued,
        ),
        check(
            "trailing_line: a padded challenge",
            format!("{base}extra=1\n").as_bytes(),
            wanted.clone(),
            issued,
        ),
        check("too_short: junk", b"junk", wanted, issued),
        proof(
            "proof: an Ed25519 proof by the named key",
            &bytes,
            &envelope,
            &key,
        ),
        proof(
            "proof: a mainnet challenge",
            &mainnet_bytes,
            &mainnet_envelope,
            &key,
        ),
        proof(
            "wrong_controller: another key's own valid proof",
            &bytes,
            &theirs,
            &other_key,
        ),
        proof(
            "controller_mismatch: the envelope names another controller",
            &bytes,
            &misnamed,
            &key,
        ),
        proof(
            "payload_mismatch: a proof over another challenge",
            &renewed,
            &envelope,
            &key,
        ),
        proof(
            "proof_type_mismatch: a passkey proof",
            &bytes,
            &passkey,
            &key,
        ),
        proof("bad_signature: one flipped bit", &bytes, &flipped, &key),
        proof(
            "bad_signature: a signature over another document",
            &bytes,
            &elsewhere,
            &key,
        ),
        proof(
            "weak_public_key: a small-order key named everywhere",
            &weak_bytes,
            &forged,
            &weak,
        ),
        proof("too_short: junk", b"junk", &envelope, &key),
    ]
}
