//! Opt-in capture of each transaction right before it is signed, for test fixtures: with
//! `SILVANA_CAPTURE_DIR` set, one JSON line each in `<dir>/<YYYYMMDD>.jsonl` (UTC).

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use orderbook_proto::ledger::{PrepareTransactionRequest, TransactionOperation};
use orderbook_proto::rfqv2::PrepareAtomicTransactionRequest;
use orderbook_proto::rfqv2::prepare_atomic_transaction_request::Params as AtomicParams;
use prost::Message;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};
use tx_verifier::{HashCheck, OperationExpectation, VerificationResult};

/// The directory the records go to; unset or blank captures nothing.
pub const CAPTURE_DIR_ENV: &str = "SILVANA_CAPTURE_DIR";
/// The record format, `v` in each line.
pub const FORMAT_VERSION: u64 = 1;
/// Records waiting for the writer.
const QUEUE_RECORDS: usize = 64;
/// Prepared-transaction bytes waiting for the writer.
const QUEUE_BYTES: usize = 64 * 1024 * 1024;
/// A day's file takes no more records past this size.
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

/// The process-wide capture, read from the environment once.
static GLOBAL: OnceLock<Option<Capture>> = OnceLock::new();

/// The typed request as the agent built it; its signatures are never captured.
#[derive(Clone, Copy, Debug)]
pub enum Request<'a> {
    Ledger(&'a PrepareTransactionRequest),
    Atomic(&'a PrepareAtomicTransactionRequest),
}

/// What is about to be signed.
#[derive(Clone, Copy, Debug)]
pub struct Signing<'a> {
    pub party: &'a str,
    pub expectation: &'a OperationExpectation,
    pub request: Request<'a>,
    pub prepared_transaction: &'a Arc<[u8]>,
    pub server_hash_base64: &'a str,
    pub hashing_scheme_version: &'a str,
    pub command_id: &'a str,
    pub fees_json: &'a str,
    pub verification: &'a VerificationResult,
}

/// Queues `signing` when `SILVANA_CAPTURE_DIR` is set; it never blocks and never fails.
pub fn before_signing(signing: Signing<'_>) {
    #[cfg(test)]
    if test_sink::keep(signing) {
        return;
    }
    if let Some(capture) = global() {
        capture.queue(signing, agent_logic::clock::now_utc());
    }
}

/// Lets a test read what the submit paths capture on its own thread.
#[cfg(test)]
pub(crate) mod test_sink {
    use std::cell::RefCell;

    use serde_json::Value;

    thread_local! {
        static LINES: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) };
    }

    /// Keeps the record as a line while a test collects on this thread.
    pub(super) fn keep(signing: super::Signing<'_>) -> bool {
        LINES.with(|lines| match lines.borrow_mut().as_mut() {
            Some(kept) => {
                kept.push(super::Record::of(signing, agent_logic::clock::now_utc()).json());
                true
            }
            None => false,
        })
    }

    /// Runs `work` on this thread and returns the lines captured meanwhile.
    pub(crate) async fn collect<F: std::future::Future>(work: F) -> (F::Output, Vec<Value>) {
        LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
        let output = work.await;
        (output, LINES.with(|lines| lines.borrow_mut().take()).unwrap_or_default())
    }
}

fn global() -> Option<&'static Capture> {
    GLOBAL.get_or_init(|| Capture::start(std::env::var_os(CAPTURE_DIR_ENV))).as_ref()
}

/// The capture directory an env value names; unset or blank is none.
fn dir_of(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    let blank = value.to_str().is_some_and(|text| text.trim().is_empty()) || value.is_empty();
    (!blank).then(|| PathBuf::from(value))
}

/// The signature-free copy of a request that a record keeps.
#[derive(Clone, Debug, PartialEq)]
enum OwnedRequest {
    Ledger(Box<PrepareTransactionRequest>),
    Atomic(Box<PrepareAtomicTransactionRequest>),
}

impl OwnedRequest {
    fn of(request: Request<'_>) -> Self {
        match request {
            Request::Ledger(request) => {
                let mut request = Box::new(request.clone());
                request.request_signature = None;
                if let Some(command) = request.controller_command.as_mut() {
                    command.proof.clear();
                }
                Self::Ledger(request)
            }
            Request::Atomic(request) => {
                let mut request = Box::new(request.clone());
                request.request_signature = None;
                if let Some(command) = request.controller_command.as_mut() {
                    command.proof.clear();
                }
                // The settle transaction itself carries the quote's signature
                if let Some(AtomicParams::AtomicDvpSettle(settle)) = request.params.as_mut() {
                    settle.quote_signature_der_hex.clear();
                }
                Self::Atomic(request)
            }
        }
    }

    fn path(&self) -> &'static str {
        match self {
            Self::Ledger(_) => "ledger",
            Self::Atomic(_) => "atomic",
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::Ledger(_) => "silvana.ledger.v1.PrepareTransactionRequest",
            Self::Atomic(_) => "silvana.rfqv2.v1.PrepareAtomicTransactionRequest",
        }
    }

    fn operation(&self) -> Option<&'static str> {
        match self {
            Self::Ledger(request) => TransactionOperation::try_from(request.operation).ok().map(|op| op.as_str_name()),
            Self::Atomic(_) => None,
        }
    }

    fn encode(&self) -> Vec<u8> {
        match self {
            Self::Ledger(request) => request.encode_to_vec(),
            Self::Atomic(request) => request.encode_to_vec(),
        }
    }
}

/// One captured transaction; it is serialized on the writer thread.
#[derive(Clone, Debug)]
struct Record {
    at: DateTime<Utc>,
    kind: &'static str,
    party: String,
    command_id: String,
    expectation: OperationExpectation,
    request: OwnedRequest,
    prepared_transaction: Arc<[u8]>,
    server_hash_base64: String,
    hashing_scheme_version: String,
    computed_hash_hex: Option<String>,
    hash_check: HashCheck,
    coverage: String,
    fees_json: String,
}

impl Record {
    fn of(signing: Signing<'_>, at: DateTime<Utc>) -> Self {
        // A mismatch leaves `computed_hash` zero and keeps the local hash in the check
        let computed = signing.verification.computed_hash;
        let computed_hash_hex = match &signing.verification.hash_check {
            HashCheck::Mismatch { computed_hex, .. } => Some(computed_hex.clone()),
            HashCheck::Match | HashCheck::Uncomputed { .. } => (computed != [0u8; 32]).then(|| hex::encode(computed)),
        };
        Self {
            at,
            kind: crate::control_survey::operation_kind(signing.expectation),
            party: signing.party.to_string(),
            command_id: signing.command_id.to_string(),
            expectation: signing.expectation.clone(),
            request: OwnedRequest::of(signing.request),
            prepared_transaction: Arc::clone(signing.prepared_transaction),
            server_hash_base64: signing.server_hash_base64.to_string(),
            hashing_scheme_version: signing.hashing_scheme_version.to_string(),
            computed_hash_hex,
            hash_check: signing.verification.hash_check.clone(),
            coverage: format!("{:?}", signing.verification.coverage).to_lowercase(),
            fees_json: signing.fees_json.to_string(),
        }
    }

    /// `YYYYMMDD.jsonl` of the record's UTC day.
    fn file_name(&self) -> String {
        format!("{:04}{:02}{:02}.jsonl", self.at.year(), self.at.month(), self.at.day())
    }

    fn json(&self) -> Value {
        let text = |s: &str| Value::String(s.to_string());
        let or_null = |s: Option<String>| s.map_or(Value::Null, Value::String);
        let (hash_check, uncomputed) = match &self.hash_check {
            HashCheck::Match => ("match", None),
            HashCheck::Mismatch { .. } => ("mismatch", None),
            HashCheck::Uncomputed { reason } => ("uncomputed", Some(reason.clone())),
        };
        let mut line = Map::new();
        line.insert("v".into(), Value::from(FORMAT_VERSION));
        line.insert("at".into(), Value::String(self.at.to_rfc3339_opts(SecondsFormat::Micros, true)));
        line.insert("kind".into(), text(self.kind));
        line.insert("path".into(), text(self.request.path()));
        line.insert("party".into(), text(&self.party));
        line.insert("command_id".into(), text(&self.command_id));
        line.insert("operation".into(), or_null(self.request.operation().map(str::to_string)));
        line.insert("expectation".into(), serde_json::to_value(&self.expectation).unwrap_or(Value::Null));
        line.insert("request_type".into(), text(self.request.type_name()));
        line.insert("request".into(), Value::String(BASE64.encode(self.request.encode())));
        line.insert("hashing_scheme_version".into(), text(&self.hashing_scheme_version));
        line.insert("server_hash_base64".into(), text(&self.server_hash_base64));
        line.insert("server_hash_hex".into(), or_null(BASE64.decode(&self.server_hash_base64).ok().map(hex::encode)));
        line.insert("computed_hash_hex".into(), or_null(self.computed_hash_hex.clone()));
        line.insert("hash_check".into(), text(hash_check));
        line.insert("hash_uncomputed_reason".into(), or_null(uncomputed));
        line.insert("coverage".into(), text(&self.coverage));
        line.insert("fees_json".into(), text(&self.fees_json));
        line.insert("prepared_transaction".into(), Value::String(BASE64.encode(&self.prepared_transaction)));
        Value::Object(line)
    }

    /// The record as one line, newline included.
    fn line(&self) -> Result<String> {
        let mut line = serde_json::to_string(&self.json())?;
        line.push('\n');
        Ok(line)
    }
}

/// Logs the first capture problem as a warning, the rest at debug.
fn warn_once(warned: &AtomicBool, problem: &str) {
    if warned.swap(true, Ordering::SeqCst) {
        debug!("transaction capture: {problem}");
    } else {
        warn!("transaction capture: {problem} (later problems are logged at debug)");
    }
}

/// A capture directory with its writer thread.
#[derive(Debug)]
struct Capture {
    dir: PathBuf,
    sender: SyncSender<Record>,
    queued_bytes: Arc<AtomicUsize>,
    warned: Arc<AtomicBool>,
}

impl Capture {
    /// Starts the writer for `value`'s directory; none when it is unset or blank, or the
    /// thread does not start.
    fn start(value: Option<OsString>) -> Option<Self> {
        let dir = dir_of(value)?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_RECORDS);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let warned = Arc::new(AtomicBool::new(false));
        let writer = Writer { dir: dir.clone(), queued_bytes: Arc::clone(&queued_bytes), warned: Arc::clone(&warned) };
        match std::thread::Builder::new().name("tx-capture".into()).spawn(move || writer.run(receiver)) {
            Ok(_) => {
                info!("transaction capture: appending each transaction before it is signed to {}", dir.display());
                Some(Self { dir, sender, queued_bytes, warned })
            }
            Err(e) => {
                warn!("transaction capture is off: its writer thread did not start: {e}");
                None
            }
        }
    }

    /// Hands the record to the writer, or drops it when the queue is full.
    fn queue(&self, signing: Signing<'_>, at: DateTime<Utc>) {
        let len = signing.prepared_transaction.len();
        let before = self.queued_bytes.fetch_add(len, Ordering::SeqCst);
        if before.saturating_add(len) > QUEUE_BYTES {
            self.queued_bytes.fetch_sub(len, Ordering::SeqCst);
            warn_once(&self.warned, "the queue is full; a record was dropped");
            return;
        }
        if self.sender.try_send(Record::of(signing, at)).is_err() {
            self.queued_bytes.fetch_sub(len, Ordering::SeqCst);
            warn_once(&self.warned, &format!("a record for {} was dropped (queue full or writer gone)", self.dir.display()));
        }
    }
}

/// Appends records in the order they were queued.
struct Writer {
    dir: PathBuf,
    queued_bytes: Arc<AtomicUsize>,
    warned: Arc<AtomicBool>,
}

impl Writer {
    fn run(self, receiver: Receiver<Record>) {
        while let Ok(record) = receiver.recv() {
            if let Err(e) = append(&self.dir, &record) {
                warn_once(&self.warned, &format!("a record was not written: {e:#}"));
            }
            self.queued_bytes.fetch_sub(record.prepared_transaction.len(), Ordering::SeqCst);
        }
    }
}

/// Appends `record` to its day's file in `dir`, both private to this user.
fn append(dir: &Path, record: &Record) -> Result<()> {
    let line = record.line()?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let path = dir.join(record.file_name());
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size >= MAX_FILE_BYTES {
        bail!("{} holds {size} bytes, the most one day's file takes", path.display());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_proto::ledger::prepare_transaction_request::Params;
    use orderbook_proto::ledger::{ControllerCommand, MessageSignature, TransferCcParams};
    use tx_verifier::Coverage;

    const ME: &str = "agent::1220aa";

    fn temp_dir(tag: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
        std::env::temp_dir().join(format!("tx-capture-{tag}-{}", hex::encode(bytes)))
    }

    fn verification(computed_hash: [u8; 32], hash_check: HashCheck) -> VerificationResult {
        VerificationResult {
            accepted: true,
            computed_hash,
            summary: String::new(),
            warnings: Vec::new(),
            rejection_reason: None,
            burn_holding_cids: Vec::new(),
            hash_check,
            coverage: Coverage::Fields,
            would_refuse: Vec::new(),
        }
    }

    fn expectation() -> OperationExpectation {
        OperationExpectation::TransferCc {
            sender_party: ME.into(),
            receiver_party: "bob::1220bb".into(),
            amount: "2.5".into(),
            command_id: "cmd-1".into(),
            dso_party: None,
        }
    }

    /// A request as a directed command would carry it, signatures included.
    fn request() -> PrepareTransactionRequest {
        PrepareTransactionRequest {
            operation: TransactionOperation::TransferCc as i32,
            params: Some(Params::TransferCc(TransferCcParams {
                receiver_party: "bob::1220bb".into(),
                amount: "2.5".into(),
                ..Default::default()
            })),
            request_signature: Some(MessageSignature {
                signature: "SECRET-SIGNATURE".into(),
                public_key: "SECRET-KEY".into(),
                signing_scheme: "ed25519".into(),
            }),
            controller_command: Some(ControllerCommand {
                canonical: b"scheme=silvana.agent.command.v1\n".to_vec(),
                controller_id: "ab".repeat(32),
                proof: b"SECRET-PROOF".to_vec(),
            }),
        }
    }

    fn line_of(record: &Record) -> Value {
        let line = record.line().unwrap();
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'), "one line: {line}");
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn a_record_round_trips_its_bytes_and_keeps_no_signature() {
        let bytes: Arc<[u8]> = (0u8..=255).cycle().take(5000).collect::<Vec<u8>>().into();
        let server = [9u8; 32];
        let request = request();
        let expectation = expectation();
        let checked = verification([9u8; 32], HashCheck::Match);
        let at = DateTime::parse_from_rfc3339("2026-11-23T17:30:00.123456Z").unwrap().with_timezone(&Utc);
        let record = Record::of(
            Signing {
                party: ME,
                expectation: &expectation,
                request: Request::Ledger(&request),
                prepared_transaction: &bytes,
                server_hash_base64: &BASE64.encode(server),
                hashing_scheme_version: "V2",
                command_id: "cmd-1",
                fees_json: "[]",
                verification: &checked,
            },
            at,
        );
        assert_eq!(record.file_name(), "20261123.jsonl");
        let json = line_of(&record);
        let decoded = BASE64.decode(json["prepared_transaction"].as_str().unwrap()).unwrap();
        assert_eq!(decoded.as_slice(), &bytes[..], "the captured bytes are the prepared bytes");
        assert_eq!(json["v"], 1);
        assert_eq!(json["at"], "2026-11-23T17:30:00.123456Z");
        assert_eq!(json["kind"], "transfer_cc");
        assert_eq!(json["path"], "ledger");
        assert_eq!(json["operation"], "TRANSACTION_OPERATION_TRANSFER_CC");
        assert_eq!(json["command_id"], "cmd-1");
        assert_eq!(json["hashing_scheme_version"], "V2");
        assert_eq!(json["server_hash_hex"], hex::encode(server));
        assert_eq!(json["computed_hash_hex"], hex::encode([9u8; 32]));
        assert_eq!(json["hash_check"], "match");
        assert_eq!(json["coverage"], "fields");
        assert_eq!(json["fees_json"], "[]");
        assert_eq!(json["expectation"]["TransferCc"]["amount"], "2.5");
        // The request decodes back, without its signature or the controller's proof
        assert_eq!(json["request_type"], "silvana.ledger.v1.PrepareTransactionRequest");
        let back = PrepareTransactionRequest::decode(
            BASE64.decode(json["request"].as_str().unwrap()).unwrap().as_slice(),
        )
        .unwrap();
        let mut expected = request.clone();
        expected.request_signature = None;
        expected.controller_command.as_mut().unwrap().proof.clear();
        assert_eq!(back, expected);
        let text = record.line().unwrap();
        assert!(!text.contains("SECRET"), "{text}");
    }

    #[test]
    fn an_uncomputed_hash_and_an_atomic_request_are_recorded_as_such() {
        let bytes: Arc<[u8]> = vec![1u8, 2, 3].into();
        let request = PrepareAtomicTransactionRequest {
            params: Some(AtomicParams::AtomicDvpSettle(orderbook_proto::rfqv2::AtomicDvpSettleParams {
                venue_cid: "venue-1".into(),
                quote_signature_der_hex: "SECRET-QUOTE-SIGNATURE".into(),
                ..Default::default()
            })),
            request_signature: Some(orderbook_proto::rfqv2::AtomicMessageSignature {
                signature: "SECRET-SIGNATURE".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let expectation = OperationExpectation::IssueTickets { lp_party: ME.into(), ticket_count: 2 };
        let checked = verification([0u8; 32], HashCheck::Uncomputed { reason: "no seeds".into() });
        let record = Record::of(
            Signing {
                party: ME,
                expectation: &expectation,
                request: Request::Atomic(&request),
                prepared_transaction: &bytes,
                server_hash_base64: "not base64!",
                hashing_scheme_version: "V3",
                command_id: "cmd-2",
                fees_json: "",
                verification: &checked,
            },
            agent_logic::clock::now_utc(),
        );
        let json = line_of(&record);
        assert_eq!(json["kind"], "issue_tickets");
        assert_eq!(json["path"], "atomic");
        assert_eq!(json["operation"], Value::Null);
        assert_eq!(json["computed_hash_hex"], Value::Null);
        assert_eq!(json["server_hash_hex"], Value::Null);
        assert_eq!(json["server_hash_base64"], "not base64!");
        assert_eq!((json["hash_check"].as_str(), json["hash_uncomputed_reason"].as_str()), (Some("uncomputed"), Some("no seeds")));
        let back = PrepareAtomicTransactionRequest::decode(
            BASE64.decode(json["request"].as_str().unwrap()).unwrap().as_slice(),
        )
        .unwrap();
        assert_eq!(back.request_signature, None);
        match back.params {
            Some(AtomicParams::AtomicDvpSettle(settle)) => {
                assert_eq!((settle.venue_cid.as_str(), settle.quote_signature_der_hex.as_str()), ("venue-1", ""));
            }
            other => panic!("the settle params are kept: {other:?}"),
        }
        assert!(!record.line().unwrap().contains("SECRET"));
        assert_eq!(BASE64.decode(json["prepared_transaction"].as_str().unwrap()).unwrap(), vec![1u8, 2, 3]);
    }

    #[test]
    fn the_writer_appends_one_line_per_record_to_the_days_file() {
        let dir = temp_dir("write");
        let capture = Capture::start(Some(dir.clone().into_os_string())).unwrap();
        let bytes: Arc<[u8]> = vec![7u8; 1000].into();
        let request = request();
        let expectation = expectation();
        let checked = verification([0u8; 32], HashCheck::Mismatch { server_hex: "aa".into(), computed_hex: "bb".into() });
        let at = agent_logic::clock::now_utc();
        for command_id in ["c-1", "c-2"] {
            capture.queue(
                Signing {
                    party: ME,
                    expectation: &expectation,
                    request: Request::Ledger(&request),
                    prepared_transaction: &bytes,
                    server_hash_base64: &BASE64.encode([5u8; 32]),
                    hashing_scheme_version: "V2",
                    command_id,
                    fees_json: "[]",
                    verification: &checked,
                },
                at,
            );
        }
        let file = dir.join(format!("{:04}{:02}{:02}.jsonl", at.year(), at.month(), at.day()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let lines = loop {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let drained = capture.queued_bytes.load(Ordering::SeqCst) == 0;
            if (text.lines().count() >= 2 && drained) || std::time::Instant::now() > deadline {
                break text.lines().map(str::to_string).collect::<Vec<_>>();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(lines.len(), 2, "{lines:?}");
        for (line, command_id) in lines.iter().zip(["c-1", "c-2"]) {
            let json: Value = serde_json::from_str(line).unwrap();
            assert_eq!(json["command_id"], command_id);
            assert_eq!(json["hash_check"], "mismatch");
            assert_eq!(json["computed_hash_hex"], "bb", "a mismatch keeps the local hash");
            assert_eq!(BASE64.decode(json["prepared_transaction"].as_str().unwrap()).unwrap(), vec![7u8; 1000]);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(capture.queued_bytes.load(Ordering::SeqCst), 0);
        drop(capture);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_captured_when_the_directory_is_unset_or_blank() {
        for value in [None, Some(OsString::new()), Some(OsString::from("   "))] {
            assert_eq!(dir_of(value.clone()), None, "{value:?}");
            assert!(Capture::start(value).is_none());
        }
        assert_eq!(dir_of(Some("/var/tmp/x".into())), Some(PathBuf::from("/var/tmp/x")));
        // The process-wide capture follows the environment, read once
        if std::env::var_os(CAPTURE_DIR_ENV).is_none() {
            assert!(global().is_none());
        }
    }

    #[test]
    fn a_full_queue_drops_records_without_blocking() {
        let dir = temp_dir("full");
        let (sender, receiver) = mpsc::sync_channel(1);
        let capture = Capture {
            dir: dir.clone(),
            sender,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            warned: Arc::new(AtomicBool::new(false)),
        };
        let bytes: Arc<[u8]> = vec![1u8; 10].into();
        let request = request();
        let expectation = expectation();
        let checked = verification([0u8; 32], HashCheck::Match);
        let signing = Signing {
            party: ME,
            expectation: &expectation,
            request: Request::Ledger(&request),
            prepared_transaction: &bytes,
            server_hash_base64: "",
            hashing_scheme_version: "V2",
            command_id: "c",
            fees_json: "[]",
            verification: &checked,
        };
        let at = agent_logic::clock::now_utc();
        capture.queue(signing, at);
        capture.queue(signing, at);
        assert!(capture.warned.load(Ordering::SeqCst), "the dropped record is warned about");
        assert_eq!(capture.queued_bytes.load(Ordering::SeqCst), 10, "only the queued record counts");
        assert_eq!(receiver.try_iter().count(), 1);
        // A record larger than the byte budget is dropped too
        let huge: Arc<[u8]> = vec![0u8; QUEUE_BYTES].into();
        capture.queue(Signing { prepared_transaction: &huge, ..signing }, at);
        assert_eq!(capture.queued_bytes.load(Ordering::SeqCst), 10);
        assert!(!dir.exists(), "nothing was written");
    }

    #[test]
    fn a_write_failure_is_reported_and_keeps_the_writer_going() {
        let base = temp_dir("fail");
        std::fs::create_dir_all(&base).unwrap();
        // A file where the directory should be
        let dir = base.join("not-a-dir");
        std::fs::write(&dir, b"x").unwrap();
        let bytes: Arc<[u8]> = vec![1u8; 4].into();
        let request = request();
        let expectation = expectation();
        let checked = verification([0u8; 32], HashCheck::Match);
        let record = Record::of(
            Signing {
                party: ME,
                expectation: &expectation,
                request: Request::Ledger(&request),
                prepared_transaction: &bytes,
                server_hash_base64: "",
                hashing_scheme_version: "V2",
                command_id: "c",
                fees_json: "[]",
                verification: &checked,
            },
            agent_logic::clock::now_utc(),
        );
        assert!(append(&dir, &record).is_err());
        let good = base.join("ok");
        append(&good, &record).unwrap();
        assert_eq!(std::fs::read_to_string(good.join(record.file_name())).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}
