//! Durable control directory of one agent: the accepted log, its head high-water
//! mark, policies, the pending entry, commands, counters and evidence.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_control::{
    EntryBody, Hash32, HeadRef, Network, PartyId, PolicyDocuments, SignedEntry, VerifiedLog,
    decode_entries, namespace_for_key, replay, verify_extension, verify_log,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Overrides the base directory of every control store.
pub const CONTROL_DIR_ENV: &str = "SILVANA_CONTROL_DIR";
/// The linked marker in the agent's env file: `<network>:<seq>:<hash>`.
pub const MARKER_ENV: &str = "AGENT_CONTROL_HEAD";
/// The network of the agent, `devnet` or `mainnet` for control.
pub const CHAIN_ENV: &str = "CANTON_CHAIN";

pub const LOG_FILE: &str = "log.json";
pub const HEAD_FILE: &str = "head.json";
pub const PENDING_FILE: &str = "pending.json";
pub const COMMANDS_FILE: &str = "commands.json";
pub const COUNTERS_FILE: &str = "counters.json";
pub const POLICIES_DIR: &str = "policies";
pub const EVIDENCE_DIR: &str = "evidence";
pub const REGISTER_EVIDENCE_FILE: &str = "register_agent.json";
const CONTROL_LOCK: &str = "control.lock";
const COUNTERS_LOCK: &str = "counters.lock";

/// Longest wait for `control.lock`.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);
/// Longest wait for `counters.lock`.
pub const COUNTERS_LOCK_WAIT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(100);
/// Spends older than this leave the rolling counters.
pub const COUNTER_WINDOW_SECS: i64 = 86_400;
/// Command records are kept this long (7 days) after they expire.
pub const COMMAND_RETENTION_SECS: i64 = 604_800;
/// Most spends counters.json holds; beyond it capped intents are refused.
pub const MAX_COUNTER_ENTRIES: usize = 65_536;
/// Most command records commands.json holds.
pub const MAX_COMMAND_RECORDS: usize = 65_536;
/// Largest control file read (32 MiB).
const MAX_FILE_BYTES: u64 = 33_554_432;
/// commands.json stays below this: past it, records that can no longer run go first, and a
/// new record is refused.
const COMMANDS_BUDGET_BYTES: usize = 16_777_216;
/// How long past its expiry a record may go early to keep commands.json in its budget.
const EARLY_PRUNE_SECS: i64 = 3_600;

/// The base directory: `SILVANA_CONTROL_DIR`, else `$HOME/.silvana-agent`.
pub fn base_dir(control_dir: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(dir) = control_dir.filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = home
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow!("HOME is not set: set {CONTROL_DIR_ENV}"))?;
    Ok(PathBuf::from(home).join(".silvana-agent"))
}

/// What `CANTON_CHAIN` names for control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainNetwork {
    Unset,
    Mapped(Network),
    /// A chain control does not support, such as testnet.
    Unmapped(String),
}

pub fn chain_network(chain: Option<&str>) -> ChainNetwork {
    match chain.map(str::trim).filter(|c| !c.is_empty()) {
        None => ChainNetwork::Unset,
        Some(c) => match Network::parse(c) {
            Ok(network) => ChainNetwork::Mapped(network),
            Err(_) => ChainNetwork::Unmapped(c.to_string()),
        },
    }
}

/// The durable linked marker: the network and the last accepted head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlMarker {
    pub network: Network,
    pub head: HeadRef,
}

impl ControlMarker {
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || anyhow!("{MARKER_ENV} is not <network>:<seq>:<hash>");
        let mut parts = text.trim().split(':');
        let (Some(network), Some(seq), Some(hash), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        let network = Network::parse(network).map_err(|_| invalid())?;
        let seq = agent_control::canon::values::parse_u64(seq).map_err(|_| invalid())?;
        ensure!(seq >= 1, invalid());
        let hash = Hash32::parse(hash).map_err(|_| invalid())?;
        Ok(Self {
            network,
            head: HeadRef { seq, hash },
        })
    }

    pub fn of(log: &VerifiedLog) -> Self {
        Self {
            network: log.network(),
            head: log.head(),
        }
    }

    /// Refuses a log under another network, behind the marker or diverging from it.
    pub fn check(&self, log: &VerifiedLog) -> Result<()> {
        ensure!(
            self.network == log.network(),
            "the linked marker names {} but the control store is for {}",
            self.network,
            log.network()
        );
        match log.entry(self.head.seq) {
            None => bail!(
                "the control store (head seq {}) is behind the linked marker {MARKER_ENV}={self}",
                log.head().seq
            ),
            Some(entry) if *entry.hash() != self.head.hash => bail!(
                "the control store diverges from the linked marker {MARKER_ENV}={self} at seq {}",
                self.head.seq
            ),
            Some(_) => Ok(()),
        }
    }
}

impl fmt::Display for ControlMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.network, self.head.seq, self.head.hash)
    }
}

/// The control settings of the process environment.
#[derive(Clone, Debug, Default)]
pub struct ControlEnv {
    pub control_dir: Option<OsString>,
    pub home: Option<OsString>,
    pub chain: Option<String>,
    pub marker: Option<String>,
    /// `PARTY_AGENT`: the party whose key the marker belongs to.
    pub party: Option<String>,
}

impl ControlEnv {
    pub fn from_process() -> Self {
        let text = |name: &str| std::env::var(name).ok();
        Self {
            control_dir: std::env::var_os(CONTROL_DIR_ENV),
            home: std::env::var_os("HOME"),
            chain: text(CHAIN_ENV),
            marker: text(MARKER_ENV),
            party: text("PARTY_AGENT"),
        }
    }

    pub fn base(&self) -> Result<PathBuf> {
        base_dir(self.control_dir.as_deref(), self.home.as_deref())
    }

    /// The marker; blank is unset, anything malformed is an error.
    pub fn marker(&self) -> Result<Option<ControlMarker>> {
        match self.marker.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            None => Ok(None),
            Some(text) => ControlMarker::parse(text).map(Some),
        }
    }

    pub fn network(&self) -> ChainNetwork {
        chain_network(self.chain.as_deref())
    }

    /// Whether the marker covers `public_key`: always, unless `PARTY_AGENT` names another key.
    pub fn marker_covers(&self, public_key: &[u8; 32]) -> bool {
        match self
            .party
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            None => true,
            Some(party) => match PartyId::parse(party) {
                Ok(party) => party.namespace() == namespace_for_key(public_key),
                Err(_) => true,
            },
        }
    }
}

/// A party id as a file name: every character outside `[A-Za-z0-9._-]` becomes `_`.
fn file_component(party: &str) -> String {
    party
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// An exclusive lock on one lock file of a control directory, released on drop.
#[derive(Debug)]
pub struct ControlLock {
    file: File,
    dir: PathBuf,
}

impl ControlLock {
    fn open_handle(dir: &Path, name: &str) -> Result<(File, PathBuf)> {
        create_private_dir(dir)?;
        let path = dir.join(name);
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let handle = options
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        Ok((handle, path))
    }

    async fn acquire(dir: &Path, name: &str, wait: Duration) -> Result<Self> {
        let (handle, path) = Self::open_handle(dir, name)?;
        let deadline = tokio::time::Instant::now().checked_add(wait);
        loop {
            match handle.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        file: handle,
                        dir: dir.to_path_buf(),
                    });
                }
                Err(fs::TryLockError::WouldBlock) => {
                    if deadline.is_none_or(|d| tokio::time::Instant::now() >= d) {
                        bail!(
                            "another process holds {} — retry when it finishes",
                            path.display()
                        );
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("lock {}", path.display()));
                }
            }
        }
    }

    /// The same wait on a blocking thread.
    fn acquire_blocking(dir: &Path, name: &str, wait: Duration) -> Result<Self> {
        let (handle, path) = Self::open_handle(dir, name)?;
        let deadline = std::time::Instant::now().checked_add(wait);
        loop {
            match handle.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        file: handle,
                        dir: dir.to_path_buf(),
                    });
                }
                Err(fs::TryLockError::WouldBlock) => {
                    if deadline.is_none_or(|d| std::time::Instant::now() >= d) {
                        bail!(
                            "another process holds {} — retry when it finishes",
                            path.display()
                        );
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("lock {}", path.display()));
                }
            }
        }
    }
}

impl Drop for ControlLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Where a pending entry stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingStatus {
    /// Signed and sent, or about to be; the rpc's answer is not known.
    Uploading,
    /// The rpc refused it. It is kept, and its seq is never signed again automatically.
    Refused,
}

/// The one signed entry the rpc has not accepted yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRecord {
    pub entry: SignedEntry,
    pub document: Option<Vec<u8>>,
    pub status: PendingStatus,
    pub refused_code: Option<u16>,
    pub recorded_at: i64,
}

/// The verified contents of a store.
#[derive(Clone, Debug)]
pub struct StoredLog {
    pub log: VerifiedLog,
    /// Every stored document the log references, each under its checked hash.
    pub documents: PolicyDocuments,
    pub pending: Option<PendingRecord>,
}

/// What one read found: maybe a log, maybe a pending entry.
struct Contents {
    log: Option<(VerifiedLog, PolicyDocuments)>,
    pending: Option<PendingRecord>,
}

impl Contents {
    fn into_stored(self) -> Option<StoredLog> {
        let (log, documents) = self.log?;
        Some(StoredLog {
            log,
            documents,
            pending: self.pending,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFile {
    seq: u64,
    canonical_b64: String,
    signature_hex: String,
}

impl EntryFile {
    fn of(entry: &SignedEntry) -> Self {
        Self {
            seq: entry.seq(),
            canonical_b64: BASE64.encode(entry.bytes()),
            signature_hex: hex::encode(entry.signature()),
        }
    }

    fn row(&self) -> Result<(Vec<u8>, [u8; 64])> {
        let bytes = BASE64
            .decode(&self.canonical_b64)
            .map_err(|_| anyhow!("entry {} is not base64", self.seq))?;
        let mut signature = [0u8; 64];
        hex::decode_to_slice(&self.signature_hex, &mut signature)
            .map_err(|_| anyhow!("entry {} has no 64-byte signature", self.seq))?;
        Ok((bytes, signature))
    }

    fn decode(&self) -> Result<SignedEntry> {
        let (bytes, signature) = self.row()?;
        let entry = SignedEntry::new(bytes, signature)
            .map_err(|e| anyhow!("entry {} does not decode: {}", self.seq, e.code_name()))?;
        ensure!(
            entry.seq() == self.seq,
            "entry {} carries seq {}",
            self.seq,
            entry.seq()
        );
        Ok(entry)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogFile {
    network: String,
    agent_party: String,
    entries: Vec<EntryFile>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadFile {
    network: String,
    agent_party: String,
    seq: u64,
    hash: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingFile {
    network: String,
    agent_party: String,
    entry: EntryFile,
    #[serde(default)]
    document_b64: Option<String>,
    status: PendingStatus,
    #[serde(default)]
    refused_code: Option<u16>,
    recorded_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    hash: String,
    document_b64: String,
}

/// Where one controller command stands in this agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Seen and kept; nothing was signed for it.
    Received,
    /// Its one attempt is recorded; once `signing` is set a signature may exist.
    Executing,
    Executed,
    Refused,
    Failed,
}

impl CommandStatus {
    /// Executed, refused or failed: settled for good.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Executed | Self::Refused | Self::Failed)
    }
}

/// One use to count against the daily caps once the command executed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandUsage {
    pub bucket: String,
    pub instrument_admin: String,
    pub instrument_id: String,
    /// Exact decimal.
    pub amount: String,
}

/// The answer kept for a signed attempt whose transaction is never found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandVerdict {
    /// `Refused` or `Failed`.
    pub status: CommandStatus,
    pub code: u16,
    /// Whether the ledger-service gave the code.
    pub from_ledger: bool,
}

/// The quote a swap attempt settles, recorded before its signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwapAttempt {
    pub quote_id: String,
    pub lp: String,
    /// Exact decimals: the sold amount, the amount received before any fee, the CC fees.
    pub sold: String,
    pub received: String,
    pub fee_cc: String,
    /// The agent's own input holdings.
    pub inputs: Vec<String>,
    /// The quote's signed expiry, unix seconds.
    pub valid_until: i64,
}

/// The one attempt of a command: what recovery needs after a crash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAttempt {
    /// The Canton command id, `ctl-<command_id>`.
    pub canton_command_id: String,
    /// The ledger end read before the attempt; recovery scans from here, and moves it past
    /// what a cut scan covered.
    pub ledger_offset: i64,
    pub started_at: i64,
    /// Set durably right before the signature.
    #[serde(default)]
    pub signing: bool,
    #[serde(default)]
    pub usage: Vec<CommandUsage>,
    #[serde(default)]
    pub verdict: Option<CommandVerdict>,
    /// A swap's quote, set before its signature.
    #[serde(default)]
    pub swap: Option<SwapAttempt>,
}

/// The agent's signed receipt, kept so that a repost is byte-identical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredReceipt {
    pub bytes_b64: String,
    /// 64 bytes as lowercase hex.
    pub signature_hex: String,
}

impl StoredReceipt {
    pub fn new(bytes: &[u8], signature: &[u8; 64]) -> Self {
        Self {
            bytes_b64: BASE64.encode(bytes),
            signature_hex: hex::encode(signature),
        }
    }

    /// The receipt bytes and the signature.
    pub fn decode(&self) -> Result<(Vec<u8>, [u8; 64])> {
        let bytes = BASE64
            .decode(&self.bytes_b64)
            .context("a stored receipt is not base64")?;
        let mut signature = [0u8; 64];
        hex::decode_to_slice(&self.signature_hex, &mut signature)
            .context("a stored receipt signature is not 64 hex bytes")?;
        Ok((bytes, signature))
    }
}

/// How a command settled; the receipt is signed before the record is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandEnd {
    Executed { update_id: String },
    Refused { code: u16 },
    Failed { code: u16 },
}

/// One controller command this agent saw, by id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRecord {
    pub command_id: String,
    pub status: CommandStatus,
    /// Unix seconds.
    pub expires_at: i64,
    pub updated_at: i64,
    /// The signed command, base64.
    #[serde(default)]
    pub canonical_b64: String,
    #[serde(default)]
    pub attempt: Option<CommandAttempt>,
    #[serde(default)]
    pub update_id: Option<String>,
    #[serde(default)]
    pub reason_code: Option<u16>,
    #[serde(default)]
    pub receipt: Option<StoredReceipt>,
    #[serde(default)]
    pub receipt_posted: bool,
}

impl CommandRecord {
    /// Whether a signature for it may exist.
    pub fn may_be_signed(&self) -> bool {
        self.attempt.as_ref().is_some_and(|a| a.signing)
    }

    /// Settled and delivered, or never begun, an hour past its expiry: it can no longer run.
    fn prunable_early(&self, now: i64) -> bool {
        let done = (self.status.is_final() && self.receipt_posted) || self.status == CommandStatus::Received;
        done && self.expires_at.saturating_add(EARLY_PRUNE_SECS) < now
    }
}

/// The commands this agent has seen; each runs at most one signed attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandBook {
    commands: Vec<CommandRecord>,
}

impl CommandBook {
    pub fn get(&self, command_id: &str) -> Option<&CommandRecord> {
        self.commands.iter().find(|c| c.command_id == command_id)
    }

    pub fn records(&self) -> &[CommandRecord] {
        &self.commands
    }

    fn get_mut(&mut self, command_id: &str) -> Result<&mut CommandRecord> {
        self.commands
            .iter_mut()
            .find(|c| c.command_id == command_id)
            .ok_or_else(|| anyhow!("command {command_id} is not recorded"))
    }

    /// Keeps `command_id` with its signed bytes, once; the same id with other bytes is refused.
    pub fn receive(
        &mut self,
        command_id: &str,
        canonical: &[u8],
        expires_at: i64,
        now: i64,
    ) -> Result<&CommandRecord> {
        let encoded = BASE64.encode(canonical);
        if let Some(at) = self.commands.iter().position(|c| c.command_id == command_id) {
            let known = self
                .commands
                .get(at)
                .ok_or_else(|| anyhow!("command {command_id} is not recorded"))?;
            ensure!(
                known.canonical_b64 == encoded,
                "command {command_id} was already used for other bytes"
            );
            return Ok(known);
        }
        ensure!(
            self.commands.len() < MAX_COMMAND_RECORDS,
            "too many command records"
        );
        self.commands.push(CommandRecord {
            command_id: command_id.to_string(),
            status: CommandStatus::Received,
            expires_at,
            updated_at: now,
            canonical_b64: encoded,
            attempt: None,
            update_id: None,
            reason_code: None,
            receipt: None,
            receipt_posted: false,
        });
        self.commands
            .last()
            .ok_or_else(|| anyhow!("command {command_id} is not recorded"))
    }

    /// Starts the one attempt of a received command (single use).
    pub fn begin(&mut self, command_id: &str, attempt: CommandAttempt, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Received && record.attempt.is_none(),
            "command {command_id} was already used"
        );
        record.status = CommandStatus::Executing;
        record.attempt = Some(attempt);
        record.updated_at = now;
        Ok(())
    }

    /// Marks the attempt as signing, with the uses to count once it executed.
    pub fn mark_signing(
        &mut self,
        command_id: &str,
        usage: Vec<CommandUsage>,
        now: i64,
    ) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Executing,
            "command {command_id} is not executing"
        );
        let attempt = record
            .attempt
            .as_mut()
            .ok_or_else(|| anyhow!("command {command_id} has no attempt"))?;
        ensure!(!attempt.signing, "command {command_id} was signed already");
        attempt.signing = true;
        attempt.usage = usage;
        record.updated_at = now;
        Ok(())
    }

    /// Records the quote an executing swap is about to sign; never after its signature.
    pub fn set_swap(&mut self, command_id: &str, swap: SwapAttempt, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Executing,
            "command {command_id} is not executing"
        );
        let attempt = record
            .attempt
            .as_mut()
            .ok_or_else(|| anyhow!("command {command_id} has no attempt"))?;
        ensure!(!attempt.signing, "command {command_id} was signed already");
        attempt.swap = Some(swap);
        record.updated_at = now;
        Ok(())
    }

    /// Keeps the answer to sign if the signed attempt is never found on the ledger.
    pub fn set_verdict(&mut self, command_id: &str, verdict: CommandVerdict, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Executing,
            "command {command_id} is not executing"
        );
        ensure!(
            matches!(verdict.status, CommandStatus::Refused | CommandStatus::Failed),
            "a verdict refuses or fails"
        );
        let attempt = record
            .attempt
            .as_mut()
            .ok_or_else(|| anyhow!("command {command_id} has no attempt"))?;
        attempt.verdict = Some(verdict);
        record.updated_at = now;
        Ok(())
    }

    /// Moves a signed attempt's scan start past what a scan covered without finding it.
    pub fn advance_scan(&mut self, command_id: &str, offset: i64, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Executing && record.may_be_signed(),
            "command {command_id} is not a signed attempt"
        );
        let attempt = record
            .attempt
            .as_mut()
            .ok_or_else(|| anyhow!("command {command_id} has no attempt"))?;
        if offset > attempt.ledger_offset {
            attempt.ledger_offset = offset;
            record.updated_at = now;
        }
        Ok(())
    }

    /// Ends an attempt that never reached its signature; the command is received again.
    pub fn release(&mut self, command_id: &str, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            record.status == CommandStatus::Executing,
            "command {command_id} is not executing"
        );
        ensure!(
            !record.may_be_signed(),
            "command {command_id} may be signed: it is only reconciled"
        );
        record.status = CommandStatus::Received;
        record.attempt = None;
        record.updated_at = now;
        Ok(())
    }

    /// Settles a command with its signed receipt; a settled one never changes, and only
    /// an attempt that may have been signed can have executed.
    pub fn finish(
        &mut self,
        command_id: &str,
        end: &CommandEnd,
        receipt: StoredReceipt,
        now: i64,
    ) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(
            !record.status.is_final(),
            "command {command_id} is settled already"
        );
        match end {
            CommandEnd::Executed { update_id } => {
                ensure!(
                    record.status == CommandStatus::Executing && record.may_be_signed(),
                    "command {command_id} was never signed"
                );
                record.status = CommandStatus::Executed;
                record.update_id = Some(update_id.clone());
            }
            CommandEnd::Refused { code } => {
                record.status = CommandStatus::Refused;
                record.reason_code = Some(*code);
            }
            CommandEnd::Failed { code } => {
                record.status = CommandStatus::Failed;
                record.reason_code = Some(*code);
            }
        }
        record.receipt = Some(receipt);
        record.updated_at = now;
        Ok(())
    }

    pub fn mark_receipt_posted(&mut self, command_id: &str, now: i64) -> Result<()> {
        let record = self.get_mut(command_id)?;
        ensure!(record.receipt.is_some(), "command {command_id} has no receipt");
        record.receipt_posted = true;
        record.updated_at = now;
        Ok(())
    }

    /// Records whose attempt is in flight or must be reconciled.
    pub fn executing(&self) -> impl Iterator<Item = &CommandRecord> {
        self.commands
            .iter()
            .filter(|c| c.status == CommandStatus::Executing)
    }

    /// Settled records whose receipt the rpc has not taken yet.
    pub fn unposted(&self) -> impl Iterator<Item = &CommandRecord> {
        self.commands
            .iter()
            .filter(|c| c.status.is_final() && !c.receipt_posted)
    }

    /// Drops records a week past expiry; one still executing is kept until it settles.
    fn prune(&mut self, now: i64) {
        self.commands.retain(|c| {
            c.status == CommandStatus::Executing
                || c.expires_at.saturating_add(COMMAND_RETENTION_SECS) > now
        });
    }

    /// The book as JSON; past `budget`, records that can no longer run go first, oldest first.
    fn encode_within(&mut self, now: i64, budget: usize) -> Result<Vec<u8>> {
        let json = serde_json::to_vec_pretty(self)?;
        let mut over = json.len().saturating_sub(budget);
        if over == 0 {
            return Ok(json);
        }
        let mut candidates: Vec<(i64, usize, usize)> = Vec::new();
        for (at, record) in self.commands.iter().enumerate() {
            if record.prunable_early(now) {
                let alone = serde_json::to_vec_pretty(record)?;
                // Nested in the book, each line is indented four more spaces.
                let lines = alone.iter().filter(|b| **b == b'\n').count().saturating_add(1);
                let size = alone.len().saturating_add(lines.saturating_mul(4)).saturating_add(2);
                candidates.push((record.updated_at, at, size));
            }
        }
        candidates.sort_unstable();
        let mut dropped = vec![false; self.commands.len()];
        for (_, at, size) in candidates {
            if over == 0 {
                break;
            }
            if let Some(flag) = dropped.get_mut(at) {
                *flag = true;
            }
            over = over.saturating_sub(size);
        }
        let mut flags = dropped.into_iter();
        self.commands.retain(|_| !flags.next().unwrap_or(false));
        Ok(serde_json::to_vec_pretty(self)?)
    }
}

/// One spend counted against a rolling 24-hour cap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpendRecord {
    /// Idempotency key, such as `transfer:<command_id>`.
    pub key: String,
    pub instrument: String,
    /// Exact decimal.
    pub amount: String,
    /// Unix seconds.
    pub at: i64,
}

/// The spends of the last 24 hours.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterBook {
    spends: Vec<SpendRecord>,
}

impl CounterBook {
    pub fn spends(&self) -> &[SpendRecord] {
        &self.spends
    }

    /// True when the book is full and capped intents must be refused.
    pub fn is_full(&self) -> bool {
        self.spends.len() >= MAX_COUNTER_ENTRIES
    }

    /// Adds a spend once per (key, instrument); returns whether it was new.
    pub fn record(&mut self, spend: SpendRecord) -> Result<bool> {
        if self
            .spends
            .iter()
            .any(|s| s.key == spend.key && s.instrument == spend.instrument)
        {
            return Ok(false);
        }
        ensure!(!self.is_full(), "the spend counters are full");
        self.spends.push(spend);
        Ok(true)
    }

    /// Removes the spends recorded under `key`.
    pub fn release(&mut self, key: &str) {
        self.spends.retain(|s| s.key != key);
    }

    fn prune(&mut self, now: i64) {
        self.spends
            .retain(|s| s.at.saturating_add(COUNTER_WINDOW_SECS) > now);
    }
}

/// What RegisterAgent sent, kept as evidence of the registration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterEvidence {
    pub public_key_hex: String,
    pub invite_code: String,
    pub canonical_b64: String,
    pub signature_b64: String,
    pub recorded_at: i64,
}

/// The control directory of one party on one network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlStore {
    dir: PathBuf,
    network: Network,
    party: PartyId,
}

impl ControlStore {
    /// `<base>/<network>/<party>/control`.
    pub fn open(base: &Path, network: Network, party: &PartyId) -> Self {
        Self {
            dir: base
                .join(network.as_str())
                .join(file_component(party.as_str()))
                .join("control"),
            network,
            party: party.clone(),
        }
    }

    /// The one stored party under `base/<network>` whose namespace is `public_key`'s.
    pub fn find(base: &Path, network: Network, public_key: &[u8; 32]) -> Result<Option<Self>> {
        let root = base.join(network.as_str());
        let suffix = format!("__{}", namespace_for_key(public_key));
        let listing = match fs::read_dir(&root) {
            Ok(listing) => listing,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", root.display())),
        };
        let mut found: Vec<Self> = Vec::new();
        for item in listing {
            let item = item.with_context(|| format!("read {}", root.display()))?;
            let name = item.file_name();
            let Some(name) = name.to_str().filter(|n| n.ends_with(&suffix)) else {
                continue;
            };
            let dir = item.path().join("control");
            let Some(party) = stored_party(&dir)? else {
                continue;
            };
            ensure!(
                file_component(party.as_str()) == name,
                "{} holds the control files of another party",
                dir.display()
            );
            found.push(Self {
                dir,
                network,
                party,
            });
        }
        ensure!(
            found.len() <= 1,
            "several control directories under {} belong to this key",
            root.display()
        );
        Ok(found.pop())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn party(&self) -> &PartyId {
        &self.party
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// True when a log, a head or a pending entry is stored.
    pub fn exists(&self) -> bool {
        [LOG_FILE, HEAD_FILE, PENDING_FILE]
            .iter()
            .any(|name| self.path(name).exists())
    }

    /// Waits up to [`LOCK_WAIT`] for `control.lock`.
    pub async fn lock(&self) -> Result<ControlLock> {
        ControlLock::acquire(&self.dir, CONTROL_LOCK, LOCK_WAIT).await
    }

    fn check_lock(&self, lock: &ControlLock) -> Result<()> {
        ensure!(
            lock.dir == self.dir,
            "the lock is for another control directory"
        );
        Ok(())
    }

    /// The verified store without the lock and without repairs.
    pub fn snapshot(&self) -> Result<Option<StoredLog>> {
        self.read(false).map(Contents::into_stored)
    }

    /// The verified store under the lock; a head.json behind the log is repaired.
    pub fn load(&self, lock: &ControlLock) -> Result<Option<StoredLog>> {
        self.check_lock(lock)?;
        self.read(true).map(Contents::into_stored)
    }

    /// Saves are atomic (temp file, fsync, rename); head.json is read first because
    /// the log is written first, so a lock-free reader never sees the log behind it.
    fn read(&self, repair: bool) -> Result<Contents> {
        let head: Option<HeadFile> = read_json(&self.path(HEAD_FILE))?;
        let log: Option<LogFile> = read_json(&self.path(LOG_FILE))?;
        let pending: Option<PendingFile> = read_json(&self.path(PENDING_FILE))?;
        let log = match (log, &head) {
            (None, Some(_)) => bail!(
                "{} exists but {} is missing: run `cloud-agent control restore`",
                self.path(HEAD_FILE).display(),
                self.path(LOG_FILE).display()
            ),
            (None, None) => {
                let pending = match &pending {
                    Some(file) => self.check_pending_file(file, None)?,
                    None => None,
                };
                return Ok(Contents { log: None, pending });
            }
            (Some(log), _) => self.verify_log_file(&log)?,
        };
        let documents = self.load_documents(&log)?;
        match &head {
            Some(head) => self.check_head(&log, head, repair)?,
            None if repair => self.write_head(&log)?,
            None => {}
        }
        let pending = match &pending {
            Some(file) => self.check_pending_file(file, Some(&log))?,
            None => None,
        };
        Ok(Contents {
            log: Some((log, documents)),
            pending,
        })
    }

    fn check_identity(&self, network: &str, party: &str, what: &str) -> Result<()> {
        ensure!(
            network == self.network.as_str() && party == self.party.as_str(),
            "{what} in {} belongs to another network or party",
            self.dir.display()
        );
        Ok(())
    }

    fn verify_log_file(&self, file: &LogFile) -> Result<VerifiedLog> {
        self.check_identity(&file.network, &file.agent_party, LOG_FILE)?;
        let rows = file
            .entries
            .iter()
            .map(EntryFile::row)
            .collect::<Result<Vec<_>>>()?;
        let entries = decode_entries(&rows).map_err(|e| {
            anyhow!(
                "{} does not decode at seq {}: {}",
                self.path(LOG_FILE).display(),
                e.seq,
                e.code_name()
            )
        })?;
        for (file_entry, entry) in file.entries.iter().zip(&entries) {
            ensure!(
                file_entry.seq == entry.seq(),
                "{} lists entry {} as seq {}",
                self.path(LOG_FILE).display(),
                entry.seq(),
                file_entry.seq
            );
        }
        verify_log(self.network, &self.party, &entries).map_err(|e| {
            anyhow!(
                "the control log in {} does not verify at seq {}: {}",
                self.dir.display(),
                e.seq,
                e.code_name()
            )
        })
    }

    /// head.json must name an entry of the log; a log ahead of it is fine.
    fn check_head(&self, log: &VerifiedLog, head: &HeadFile, repair: bool) -> Result<()> {
        self.check_identity(&head.network, &head.agent_party, HEAD_FILE)?;
        let hash = Hash32::parse(&head.hash)
            .map_err(|_| anyhow!("{} has no valid hash", self.path(HEAD_FILE).display()))?;
        match log.entry(head.seq) {
            None => bail!(
                "the control log in {} (seq {}) is behind its high-water mark (seq {}): \
                 run `cloud-agent control restore`",
                self.dir.display(),
                log.head().seq,
                head.seq
            ),
            Some(entry) if *entry.hash() != hash => bail!(
                "the control log in {} diverges from its high-water mark at seq {}",
                self.dir.display(),
                head.seq
            ),
            Some(_) => {}
        }
        if repair && log.head().seq > head.seq {
            self.write_head(log)?;
        }
        Ok(())
    }

    fn write_head(&self, log: &VerifiedLog) -> Result<()> {
        let head = log.head();
        save_json(
            &self.path(HEAD_FILE),
            &HeadFile {
                network: self.network.as_str().to_string(),
                agent_party: self.party.as_str().to_string(),
                seq: head.seq,
                hash: head.hash.to_string(),
            },
        )
    }

    /// The documents the log's accept_policy entries reference, each hash checked.
    fn load_documents(&self, log: &VerifiedLog) -> Result<PolicyDocuments> {
        let mut documents = PolicyDocuments::new();
        for entry in log.entries() {
            let EntryBody::AcceptPolicy(body) = entry.entry().body() else {
                continue;
            };
            if let Some(bytes) = self.get_policy(&body.policy_hash)? {
                documents.insert_keyed(body.policy_hash, bytes);
            }
        }
        Ok(documents)
    }

    /// The pending entry; a stale one (already in the log) reads as none.
    fn check_pending_file(
        &self,
        file: &PendingFile,
        log: Option<&VerifiedLog>,
    ) -> Result<Option<PendingRecord>> {
        self.check_identity(&file.network, &file.agent_party, PENDING_FILE)?;
        let entry = file.entry.decode()?;
        let (next_seq, prev) = match log {
            Some(log) => (log.head().seq.saturating_add(1), log.head().hash),
            None => (1, Hash32([0; 32])),
        };
        if let Some(stored) = log.and_then(|l| l.entry(entry.seq())) {
            ensure!(
                stored.hash() == entry.hash(),
                "the pending entry at seq {} differs from the accepted one: the agent key \
                 signed two entries at that seq",
                entry.seq()
            );
            return Ok(None);
        }
        ensure!(
            entry.seq() == next_seq && *entry.prev() == prev,
            "the pending entry (seq {}) does not follow the control log",
            entry.seq()
        );
        let document = match &file.document_b64 {
            Some(text) => Some(
                BASE64
                    .decode(text)
                    .map_err(|_| anyhow!("the pending policy document is not base64"))?,
            ),
            None => None,
        };
        Ok(Some(PendingRecord {
            entry,
            document,
            status: file.status,
            refused_code: file.refused_code,
            recorded_at: file.recorded_at,
        }))
    }

    /// Appends accepted entries: documents, then log.json, then head.json.
    pub fn append(
        &self,
        lock: &ControlLock,
        entries: &[SignedEntry],
        documents: &[Vec<u8>],
    ) -> Result<HeadRef> {
        let stored = self.load(lock)?;
        let log = match &stored {
            Some(stored) => verify_extension(&stored.log, entries),
            None => verify_log(self.network, &self.party, entries),
        }
        .map_err(|e| {
            anyhow!(
                "refusing to append: entry {} does not extend the control log: {}",
                e.seq,
                e.code_name()
            )
        })?;
        let mut all = stored.map(|s| s.documents).unwrap_or_default();
        for document in documents {
            all.insert(document.clone());
        }
        replay(&log, &all).map_err(|e| {
            anyhow!(
                "refusing to append: entry {} does not replay: {}",
                e.seq,
                e.code_name()
            )
        })?;
        for document in documents {
            self.put_policy(document)?;
        }
        save_json(
            &self.path(LOG_FILE),
            &LogFile {
                network: self.network.as_str().to_string(),
                agent_party: self.party.as_str().to_string(),
                entries: log.entries().iter().map(EntryFile::of).collect(),
            },
        )?;
        self.write_head(&log)?;
        Ok(log.head())
    }

    /// Stores a policy document under its SHA-256.
    pub fn put_policy(&self, bytes: &[u8]) -> Result<Hash32> {
        let hash = Hash32(Sha256::digest(bytes).into());
        save_json(
            &self.policy_path(&hash),
            &PolicyFile {
                hash: hash.to_string(),
                document_b64: BASE64.encode(bytes),
            },
        )?;
        Ok(hash)
    }

    fn policy_path(&self, hash: &Hash32) -> PathBuf {
        self.dir.join(POLICIES_DIR).join(format!("{hash}.json"))
    }

    /// The document stored under `hash`; one whose bytes hash otherwise is refused.
    pub fn get_policy(&self, hash: &Hash32) -> Result<Option<Vec<u8>>> {
        let path = self.policy_path(hash);
        let Some(file) = read_json::<PolicyFile>(&path)? else {
            return Ok(None);
        };
        let bytes = BASE64
            .decode(&file.document_b64)
            .map_err(|_| anyhow!("{} is not base64", path.display()))?;
        let actual = Hash32(Sha256::digest(&bytes).into());
        ensure!(
            file.hash == hash.to_string() && actual == *hash,
            "{} does not hold the document of its name",
            path.display()
        );
        Ok(Some(bytes))
    }

    /// The pending entry, if any (also a link entry before any log).
    pub fn pending(&self, lock: &ControlLock) -> Result<Option<PendingRecord>> {
        self.check_lock(lock)?;
        Ok(self.read(true)?.pending)
    }

    /// Records `entry` as pending before it is uploaded; another pending entry is refused.
    pub fn record_pending(
        &self,
        lock: &ControlLock,
        entry: &SignedEntry,
        document: Option<&[u8]>,
        now: i64,
    ) -> Result<()> {
        self.check_lock(lock)?;
        let contents = self.read(true)?;
        if let Some(existing) = &contents.pending {
            ensure!(
                existing.entry.hash() == entry.hash(),
                "{}",
                pending_refusal(existing)
            );
            return Ok(());
        }
        let (next_seq, prev) = match &contents.log {
            Some((log, _)) => (log.head().seq.saturating_add(1), log.head().hash),
            None => (1, Hash32([0; 32])),
        };
        ensure!(
            entry.seq() == next_seq && *entry.prev() == prev,
            "the entry (seq {}) does not follow the control log",
            entry.seq()
        );
        self.save_pending(entry, document, PendingStatus::Uploading, None, now)
    }

    fn save_pending(
        &self,
        entry: &SignedEntry,
        document: Option<&[u8]>,
        status: PendingStatus,
        refused_code: Option<u16>,
        now: i64,
    ) -> Result<()> {
        save_json(
            &self.path(PENDING_FILE),
            &PendingFile {
                network: self.network.as_str().to_string(),
                agent_party: self.party.as_str().to_string(),
                entry: EntryFile::of(entry),
                document_b64: document.map(|d| BASE64.encode(d)),
                status,
                refused_code,
                recorded_at: now,
            },
        )
    }

    /// Marks the pending entry refused; it is kept and its seq is not signed again.
    pub fn refuse_pending(&self, lock: &ControlLock, code: Option<u16>, now: i64) -> Result<()> {
        let pending = self
            .pending(lock)?
            .ok_or_else(|| anyhow!("no pending control entry"))?;
        self.save_pending(
            &pending.entry,
            pending.document.as_deref(),
            PendingStatus::Refused,
            code,
            now,
        )
    }

    /// Appends the pending entry once the rpc reports it as its head.
    pub fn accept_pending(&self, lock: &ControlLock, accepted: &HeadRef) -> Result<HeadRef> {
        let pending = self
            .pending(lock)?
            .ok_or_else(|| anyhow!("no pending control entry"))?;
        ensure!(
            pending.status == PendingStatus::Uploading,
            "the pending entry at seq {} was refused",
            pending.entry.seq()
        );
        ensure!(
            accepted.seq == pending.entry.seq() && accepted.hash == *pending.entry.hash(),
            "the rpc's head (seq {}) is not the pending entry (seq {})",
            accepted.seq,
            pending.entry.seq()
        );
        let documents: Vec<Vec<u8>> = pending.document.iter().cloned().collect();
        let head = self.append(lock, std::slice::from_ref(&pending.entry), &documents)?;
        remove_file(&self.path(PENDING_FILE))?;
        Ok(head)
    }

    /// Refuses to sign a new entry while one is pending; a refused seq stays blocked.
    pub fn check_can_sign(&self, lock: &ControlLock) -> Result<()> {
        match self.pending(lock)? {
            None => Ok(()),
            Some(pending) => Err(anyhow!("{}", pending_refusal(&pending))),
        }
    }

    /// Signs `entry` and records it as pending before it is uploaded. The same bytes
    /// already pending are returned as stored; any other pending entry is refused.
    pub fn sign_entry(
        &self,
        lock: &ControlLock,
        signer: &crate::signer::AgentSigner,
        entry: &agent_control::PendingEntry,
        now: i64,
    ) -> Result<SignedEntry> {
        if let Some(pending) = self.pending(lock)? {
            ensure!(
                pending.status == PendingStatus::Uploading
                    && pending.entry.bytes() == entry.signable().bytes(),
                "{}",
                pending_refusal(&pending)
            );
            return Ok(pending.entry);
        }
        let signed = signer.sign_control_entry(entry)?;
        self.record_pending(lock, &signed, entry.policy_document(), now)?;
        Ok(signed)
    }

    /// Reads commands.json under the lock.
    pub fn commands(&self, lock: &ControlLock) -> Result<CommandBook> {
        self.check_lock(lock)?;
        Ok(read_json(&self.path(COMMANDS_FILE))?.unwrap_or_default())
    }

    /// Reads and updates commands.json under the lock, pruning old records.
    pub fn update_commands<T>(
        &self,
        lock: &ControlLock,
        now: i64,
        f: impl FnOnce(&mut CommandBook) -> Result<T>,
    ) -> Result<T> {
        self.update_commands_within(lock, now, COMMANDS_BUDGET_BYTES, f)
    }

    /// [`Self::update_commands`] with commands.json kept within `budget` bytes, as far as
    /// records that can no longer run allow; past it a new record is refused.
    fn update_commands_within<T>(
        &self,
        lock: &ControlLock,
        now: i64,
        budget: usize,
        f: impl FnOnce(&mut CommandBook) -> Result<T>,
    ) -> Result<T> {
        self.check_lock(lock)?;
        let path = self.path(COMMANDS_FILE);
        let mut book: CommandBook = read_json(&path)?.unwrap_or_default();
        book.prune(now);
        let before = book.commands.len();
        let out = f(&mut book)?;
        let added = book.commands.len() > before;
        let json = book.encode_within(now, budget)?;
        ensure!(
            !added || json.len() <= budget,
            "{} is full: no new command is kept until older ones expire",
            path.display()
        );
        save_bytes(&path, &json)?;
        Ok(out)
    }

    /// Reads and updates counters.json under `counters.lock`, pruning spends older than a day.
    pub async fn update_counters<T, F>(&self, now: i64, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut CounterBook) -> Result<T> + Send + 'static,
    {
        let dir = self.dir.clone();
        let work = move || -> Result<T> {
            let _lock = ControlLock::acquire_blocking(&dir, COUNTERS_LOCK, COUNTERS_LOCK_WAIT)?;
            let path = dir.join(COUNTERS_FILE);
            let mut book: CounterBook = read_json(&path)?.unwrap_or_default();
            book.prune(now);
            let out = f(&mut book)?;
            save_json(&path, &book)?;
            Ok(out)
        };
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|e| anyhow!("the counters need a tokio runtime: {e}"))?;
        let bound = COUNTERS_LOCK_WAIT.saturating_add(Duration::from_secs(5));
        match tokio::time::timeout(bound, runtime.spawn_blocking(work)).await {
            Ok(Ok(result)) => result,
            Ok(Err(join)) => Err(anyhow!("updating the counters failed: {join}")),
            Err(_) => Err(anyhow!("updating the counters timed out after {bound:?}")),
        }
    }

    /// Keeps what RegisterAgent sent.
    pub fn write_evidence(&self, evidence: &RegisterEvidence) -> Result<()> {
        save_json(
            &self.dir.join(EVIDENCE_DIR).join(REGISTER_EVIDENCE_FILE),
            evidence,
        )
    }

    pub fn read_evidence(&self) -> Result<Option<RegisterEvidence>> {
        read_json(&self.dir.join(EVIDENCE_DIR).join(REGISTER_EVIDENCE_FILE))
    }
}

fn pending_refusal(pending: &PendingRecord) -> String {
    let seq = pending.entry.seq();
    match pending.status {
        PendingStatus::Uploading => format!(
            "a signed control entry at seq {seq} awaits the rpc's answer: run \
             `cloud-agent control sync` before signing another"
        ),
        PendingStatus::Refused => format!(
            "the rpc refused the signed control entry at seq {seq}{}; it is kept and seq {seq} \
             is not signed again automatically",
            pending
                .refused_code
                .map(|c| format!(" (code {c})"))
                .unwrap_or_default()
        ),
    }
}

/// The party a control directory's files name, if it has any.
fn stored_party(dir: &Path) -> Result<Option<PartyId>> {
    let party = if let Some(head) = read_json::<HeadFile>(&dir.join(HEAD_FILE))? {
        Some(head.agent_party)
    } else if let Some(log) = read_json::<LogFile>(&dir.join(LOG_FILE))? {
        Some(log.agent_party)
    } else {
        read_json::<PendingFile>(&dir.join(PENDING_FILE))?.map(|p| p.agent_party)
    };
    party
        .map(|p| {
            PartyId::parse(&p)
                .map_err(|_| anyhow!("{} names an invalid party", dir.display()))
        })
        .transpose()
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

/// Reads `path` as JSON; a missing file is `None`.
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(
        u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_FILE_BYTES),
        "{} is too large",
        path.display()
    );
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("{} is not a valid control file", path.display()))
}

/// Writes `value` to `path` atomically and durably, mode 0600 in a 0700 directory.
fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    save_bytes(path, &json)
}

/// [`save_json`] of encoded bytes; what could not be read back is never written.
fn save_bytes(path: &Path, json: &[u8]) -> Result<()> {
    ensure!(
        u64::try_from(json.len()).is_ok_and(|n| n <= MAX_FILE_BYTES),
        "{} would be too large to read back; nothing was written",
        path.display()
    );
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    replace_file(path, json, FileMode::Exactly(0o600))
}

/// Replaces `path` with `bytes` atomically and durably (temp file, fsync, rename, directory
/// fsync), through a symlink; an existing file keeps its mode, a new one gets `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    replace_file(path, bytes, FileMode::KeepOr(mode))
}

/// The mode [`replace_file`] gives the file it writes.
#[derive(Clone, Copy)]
enum FileMode {
    /// The existing file's mode, else this one.
    KeepOr(u32),
    Exactly(u32),
}

static TEMP_FILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The file a write to `path` replaces: the target of a symlink, else `path` itself.
fn write_target(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)
            .with_context(|| format!("{} is a symlink to a missing file", path.display())),
        Ok(_) => Ok(path.to_path_buf()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(unix)]
fn mode_of(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(meta.permissions().mode() & 0o7777)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Result<Option<u32>> {
    Ok(None)
}

/// A temp file that did not exist before, in `dir`, with exactly `mode`.
fn create_temp(dir: &Path, name: &OsStr, mode: u32) -> Result<(PathBuf, File)> {
    for _ in 0..16 {
        let n = TEMP_FILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{}.{}.{n}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        match options.open(&tmp) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) = file.set_permissions(fs::Permissions::from_mode(mode)) {
                        let _ = fs::remove_file(&tmp);
                        return Err(e).with_context(|| format!("set the mode of {}", tmp.display()));
                    }
                }
                #[cfg(not(unix))]
                let _ = mode;
                return Ok((tmp, file));
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", tmp.display())),
        }
    }
    bail!("could not create a temporary file in {}", dir.display())
}

fn replace_file(path: &Path, bytes: &[u8], file_mode: FileMode) -> Result<()> {
    let target = write_target(path)?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow!("{} names no file", path.display()))?
        .to_os_string();
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mode = match file_mode {
        FileMode::KeepOr(mode) => mode_of(&target)?.unwrap_or(mode),
        FileMode::Exactly(mode) => mode,
    };
    let (tmp, mut file) = create_temp(&dir, &name, mode)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", tmp.display()));
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(replace_error(e, &target));
    }
    sync_dir(&dir).with_context(|| format!("sync {}", dir.display()))
}

/// Why a rename over `target` failed; EBUSY is a single-file bind mount.
fn replace_error(e: std::io::Error, target: &Path) -> anyhow::Error {
    if e.kind() == ErrorKind::ResourceBusy {
        return anyhow!(
            "cannot replace {}: it is busy, likely a single-file bind mount; mount its \
             directory instead (the file was not changed)",
            target.display()
        );
    }
    anyhow::Error::new(e).context(format!("replace {}", target.display()))
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_dir(dir).with_context(|| format!("sync {}", dir.display()))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    match File::open(dir).and_then(|d| d.sync_all()) {
        // Some filesystems cannot sync a directory
        Err(e) if matches!(e.kind(), ErrorKind::InvalidInput | ErrorKind::Unsupported) => Ok(()),
        other => other,
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testing {
    //! Deterministic linked logs for tests in this and dependent crates.

    use agent_control::ControlState;
    use agent_control::{
        Capability, CapabilitySet, ControllerDescriptor, ControllerKey, EntryBuilder, Label,
        LinkDescriptor, LinkPurpose, Network, Nonce16, OwnerAccount, PartyId, PendingEntry,
        RpPolicy, SignedEntry, UtcTime, namespace_for_key, sign_agent,
    };
    use anyhow::{Context, Result, anyhow};
    use ed25519_dalek::SigningKey;

    /// The party `hint::<namespace of seed's key>`.
    pub fn party_for(seed: [u8; 32], hint: &str) -> Result<PartyId> {
        let key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        PartyId::parse(&format!("{hint}::{}", namespace_for_key(&key)))
            .map_err(|_| anyhow!("invalid test party"))
    }

    /// A test chain signed directly with a test seed.
    pub struct TestChain {
        pub key: SigningKey,
        pub state: ControlState,
        pub entries: Vec<SignedEntry>,
        pub created_at: UtcTime,
    }

    impl TestChain {
        pub fn new(seed: [u8; 32], party: &PartyId, network: Network) -> Result<Self> {
            let key = SigningKey::from_bytes(&seed);
            let state =
                ControlState::empty(network, party.clone(), key.verifying_key().to_bytes());
            Ok(Self {
                key,
                state,
                entries: Vec::new(),
                created_at: UtcTime::from_unix_secs(1_795_000_000).context("time")?,
            })
        }

        fn tick(&mut self) -> UtcTime {
            self.created_at = self
                .created_at
                .checked_add_secs(60)
                .unwrap_or(self.created_at);
            self.created_at
        }

        /// The next entry, built but not signed.
        pub fn link_pending(&mut self, controller_seed: u8) -> Result<PendingEntry> {
            let controller_key = SigningKey::from_bytes(&[controller_seed; 32])
                .verifying_key()
                .to_bytes();
            let controller = ControllerDescriptor::new(
                ControllerKey::Ed25519(controller_key),
                Label::parse("Test key").map_err(|_| anyhow!("label"))?,
                CapabilitySet::of(&[Capability::Policy, Capability::Direct, Capability::Read]),
            )
            .map_err(|e| anyhow!("controller: {}", e.code_name()))?;
            let now = self.tick();
            let descriptor = LinkDescriptor::new(
                self.state.network(),
                LinkPurpose::Link,
                OwnerAccount::new(42).map_err(|_| anyhow!("owner"))?,
                Some(self.state.agent_party().clone()),
                Nonce16([controller_seed; 16]),
                now.checked_add_secs(86_400).context("time")?,
                vec![controller],
            )
            .map_err(|e| anyhow!("descriptor: {}", e.code_name()))?;
            let bytes = descriptor
                .encode()
                .map_err(|e| anyhow!("encode: {}", e.code_name()))?;
            let commitment = descriptor
                .commitment()
                .map_err(|e| anyhow!("commitment: {}", e.code_name()))?;
            EntryBuilder::new(&self.state)
                .link(
                    &bytes,
                    &commitment,
                    &RpPolicy::for_network(self.state.network()),
                    false,
                    now,
                )
                .map_err(|e| anyhow!("link: {}", e.code_name()))
        }

        /// An add_controller entry for a local Ed25519 key, built but not signed.
        pub fn add_pending(&mut self, controller_seed: u8) -> Result<PendingEntry> {
            let key = SigningKey::from_bytes(&[controller_seed; 32])
                .verifying_key()
                .to_bytes();
            let now = self.tick();
            EntryBuilder::new(&self.state)
                .add_ed25519(
                    key,
                    Label::parse("Extra key").map_err(|_| anyhow!("label"))?,
                    CapabilitySet::of(&[Capability::Read]),
                    now,
                )
                .map_err(|e| anyhow!("add: {}", e.code_name()))
        }

        /// A remove_controller entry for the Ed25519 controller of `controller_seed`; a
        /// grant still pending is withdrawn at once.
        pub fn withdraw_pending(&mut self, controller_seed: u8) -> Result<PendingEntry> {
            let key = SigningKey::from_bytes(&[controller_seed; 32])
                .verifying_key()
                .to_bytes();
            let id = agent_control::controller_id_for(agent_control::ControllerType::Ed25519, &key);
            let now = self.tick();
            EntryBuilder::new(&self.state)
                .remove_controller(&id, false, now)
                .map_err(|e| anyhow!("remove: {}", e.code_name()))
        }

        /// A chain of a link, then `cycles` additions each withdrawn: `1 + 2 * cycles` entries.
        pub fn churn(seed: [u8; 32], party: &PartyId, network: Network, cycles: u8) -> Result<Self> {
            let mut chain = Self::new(seed, party, network)?;
            let link = chain.link_pending(0x21)?;
            chain.push(&link)?;
            for i in 0..cycles {
                let controller = i.wrapping_add(0x80);
                let add = chain.add_pending(controller)?;
                chain.push(&add)?;
                let withdraw = chain.withdraw_pending(controller)?;
                chain.push(&withdraw)?;
            }
            Ok(chain)
        }

        /// An accept_policy entry for the next policy version, signed by the Ed25519
        /// controller of `controller_seed`; built but not signed by the agent.
        pub fn accept_pending(&mut self, controller_seed: u8) -> Result<PendingEntry> {
            use agent_control::{ControllerProof, ControllerType, Hash32, PolicyDocument, ProofEnvelope};
            use ed25519_dalek::Signer;
            let mut now = self.tick();
            if !self.state.controllers().any(|c| c.is_effective_at(now)) {
                let delay = agent_control::controller_change_delay_secs(self.state.network());
                now = now.checked_add_secs(i64::try_from(delay)?).context("time")?;
                self.created_at = now;
            }
            let latest = self.state.latest_policy();
            let document = PolicyDocument {
                network: self.state.network(),
                agent_party: self.state.agent_party().clone(),
                version: latest.map_or(0, |p| p.version()).saturating_add(1),
                prev_policy_hash: latest.map_or(Hash32([0; 32]), |p| *p.hash()),
                issued_at: now,
                expires_at: now.checked_add_secs(3600).context("time")?,
                not_before: None,
                rules: agent_control::DEFAULT_LINKED_RULES,
            }
            .encode()
            .map_err(|e| anyhow!("policy: {}", e.code_name()))?;
            let key = SigningKey::from_bytes(&[controller_seed; 32]);
            let id = agent_control::controller_id_for(
                ControllerType::Ed25519,
                &key.verifying_key().to_bytes(),
            );
            let proof = ControllerProof::Ed25519 {
                signature: key.sign(&document).to_bytes(),
            };
            let envelope = ProofEnvelope::new(id, &document, proof);
            EntryBuilder::new(&self.state)
                .accept_policy(&document, &envelope, now)
                .map_err(|e| anyhow!("accept: {}", e.code_name()))
        }

        /// Signs `pending` with the chain key and applies it.
        pub fn push(&mut self, pending: &PendingEntry) -> Result<SignedEntry> {
            let signed = pending
                .finish(sign_agent(&self.key, pending.signable()))
                .map_err(|e| anyhow!("finish: {}", e.code_name()))?;
            self.state = pending.state_after().clone();
            self.entries.push(signed.clone());
            Ok(signed)
        }

        /// A chain of `n` entries: a link, then additions.
        pub fn build(seed: [u8; 32], party: &PartyId, network: Network, n: usize) -> Result<Self> {
            let mut chain = Self::new(seed, party, network)?;
            let link = chain.link_pending(0x21)?;
            chain.push(&link)?;
            for i in 1..n {
                let seed = u8::try_from(i).unwrap_or(u8::MAX).wrapping_add(0x40);
                let add = chain.add_pending(seed)?;
                chain.push(&add)?;
            }
            Ok(chain)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const SEED: [u8; 32] = [7u8; 32];

    fn temp_dir(tag: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
        let dir = std::env::temp_dir().join(format!("control-store-{tag}-{}", hex::encode(bytes)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(base: &Path) -> ControlStore {
        ControlStore::open(base, Network::Devnet, &party_for(SEED, "agent-1").unwrap())
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn append_round_trips_with_0700_0600() {
        let base = temp_dir("roundtrip");
        let store = store(&base);
        assert!(!store.exists());
        assert!(store.snapshot().unwrap().is_none());
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 3).unwrap();
        let lock = store.lock().await.unwrap();
        let head = store.append(&lock, &chain.entries[..1], &[]).unwrap();
        assert_eq!(head.seq, 1);
        let head = store.append(&lock, &chain.entries[1..], &[]).unwrap();
        assert_eq!(head, chain.state.head().unwrap());
        drop(lock);
        let loaded = store.snapshot().unwrap().unwrap();
        assert_eq!(loaded.log.head(), head);
        assert_eq!(loaded.log.entries(), chain.entries.as_slice());
        assert!(store.exists());
        #[cfg(unix)]
        {
            assert_eq!(mode(store.dir()), 0o700);
            assert_eq!(mode(&store.dir().join(LOG_FILE)), 0o600);
            assert_eq!(mode(&store.dir().join(HEAD_FILE)), 0o600);
        }
        // An entry that does not extend the log is refused and nothing changes.
        let other = TestChain::build([8u8; 32], &party_for([8u8; 32], "agent-1").unwrap(), Network::Devnet, 1).unwrap();
        let lock = store.lock().await.unwrap();
        assert!(store.append(&lock, &other.entries, &[]).is_err());
        assert_eq!(store.load(&lock).unwrap().unwrap().log.head(), head);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn log_behind_high_water_mark_refused() {
        let base = temp_dir("behind");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 3).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries[..2], &[]).unwrap();
        let old_log = fs::read(store.dir().join(LOG_FILE)).unwrap();
        store.append(&lock, &chain.entries[2..], &[]).unwrap();
        // Roll log.json back by one entry; head.json still names seq 3.
        fs::write(store.dir().join(LOG_FILE), &old_log).unwrap();
        let err = store.load(&lock).unwrap_err().to_string();
        assert!(err.contains("behind its high-water mark"), "{err}");
        assert!(store.snapshot().is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn log_ahead_of_head_is_repaired() {
        let base = temp_dir("ahead");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 2).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries[..1], &[]).unwrap();
        let old_head = fs::read(store.dir().join(HEAD_FILE)).unwrap();
        store.append(&lock, &chain.entries[1..], &[]).unwrap();
        // A crash between log.json and head.json leaves head.json one behind.
        fs::write(store.dir().join(HEAD_FILE), &old_head).unwrap();
        assert_eq!(store.snapshot().unwrap().unwrap().log.head().seq, 2);
        assert_eq!(fs::read(store.dir().join(HEAD_FILE)).unwrap(), old_head, "a snapshot never writes");
        store.load(&lock).unwrap();
        let head: HeadFile = read_json(&store.dir().join(HEAD_FILE)).unwrap().unwrap();
        assert_eq!(head.seq, 2);
        // head.json naming a different entry is a divergence.
        let mut forged = head.clone();
        forged.hash = Hash32([0xab; 32]).to_string();
        save_json(&store.dir().join(HEAD_FILE), &forged).unwrap();
        let err = store.load(&lock).unwrap_err().to_string();
        assert!(err.contains("diverges"), "{err}");
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn missing_log_with_head_refused_and_tampering_detected() {
        let base = temp_dir("missing");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 2).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries, &[]).unwrap();
        let log = fs::read(store.dir().join(LOG_FILE)).unwrap();
        fs::remove_file(store.dir().join(LOG_FILE)).unwrap();
        let err = store.load(&lock).unwrap_err().to_string();
        assert!(err.contains("is missing"), "{err}");
        // An edited entry fails verification.
        let mut file: LogFile = serde_json::from_slice(&log).unwrap();
        let mut bytes = BASE64.decode(&file.entries[1].canonical_b64).unwrap();
        let last = bytes.len() - 2;
        bytes[last] ^= 1;
        file.entries[1].canonical_b64 = BASE64.encode(&bytes);
        save_json(&store.dir().join(LOG_FILE), &file).unwrap();
        assert!(store.load(&lock).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn concurrent_appenders_never_fork() {
        let base = temp_dir("concurrent");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 3).unwrap();
        {
            let lock = store.lock().await.unwrap();
            store.append(&lock, &chain.entries[..1], &[]).unwrap();
        }
        // Two writers race to append the same seq 2 and 3: exactly one wins each.
        let a = store.clone();
        let b = store.clone();
        let entries_a = chain.entries[1..].to_vec();
        let entries_b = chain.entries[1..].to_vec();
        let (ra, rb) = tokio::join!(
            async move {
                let lock = a.lock().await.unwrap();
                a.append(&lock, &entries_a, &[])
            },
            async move {
                let lock = b.lock().await.unwrap();
                b.append(&lock, &entries_b, &[])
            }
        );
        assert!(ra.is_ok() != rb.is_ok(), "{ra:?} {rb:?}");
        let loaded = store.snapshot().unwrap().unwrap();
        assert_eq!(loaded.log.entries(), chain.entries.as_slice());
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn counters_shared_between_two_store_handles() {
        let base = temp_dir("counters");
        let cli = store(&base);
        let daemon = store(&base);
        let spend = |key: &str, at: i64| SpendRecord {
            key: key.into(),
            instrument: "cc".into(),
            amount: "9".into(),
            at,
        };
        let s1 = spend("fee:t1", 1000);
        assert!(cli.update_counters(1000, move |b| b.record(s1)).await.unwrap());
        let s1 = spend("fee:t1", 1001);
        assert!(!daemon.update_counters(1001, move |b| b.record(s1)).await.unwrap(), "same key counted once");
        let s2 = spend("fee:t2", 1002);
        assert!(daemon.update_counters(1002, move |b| b.record(s2)).await.unwrap());
        let n = cli.update_counters(1003, |b| Ok(b.spends().len())).await.unwrap();
        assert_eq!(n, 2);
        // A day later both have left the window.
        let n = cli
            .update_counters(1002 + COUNTER_WINDOW_SECS, |b| Ok(b.spends().len()))
            .await
            .unwrap();
        assert_eq!(n, 0);
        let s3 = spend("fee:t3", 5000);
        cli.update_counters(5000, move |b| b.record(s3)).await.unwrap();
        cli.update_counters(5001, |b| {
            b.release("fee:t3");
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(cli.update_counters(5002, |b| Ok(b.spends().len())).await.unwrap(), 0);
        let _ = fs::remove_dir_all(&base);
    }

    fn attempt(offset: i64) -> CommandAttempt {
        CommandAttempt {
            canton_command_id: "ctl-c1".into(),
            ledger_offset: offset,
            started_at: 100,
            signing: false,
            usage: Vec::new(),
            verdict: None,
            swap: None,
        }
    }

    #[tokio::test]
    async fn a_swap_quote_is_kept_only_before_the_signature() {
        let base = temp_dir("swap-attempt");
        let store = store(&base);
        let lock = store.lock().await.unwrap();
        let swap = |quote: &str| SwapAttempt {
            quote_id: quote.into(),
            lp: "lp::1220aa".into(),
            sold: "10".into(),
            received: "2.5".into(),
            fee_cc: "0".into(),
            inputs: vec!["h1".into()],
            valid_until: 900,
        };
        store.update_commands(&lock, 100, |b| b.receive("c1", b"cmd", 1000, 100).map(|_| ())).unwrap();
        assert!(store.update_commands(&lock, 100, |b| b.set_swap("c1", swap("q0"), 100)).is_err(), "not executing");
        store.update_commands(&lock, 101, |b| b.begin("c1", attempt(7), 101)).unwrap();
        store.update_commands(&lock, 101, |b| b.set_swap("c1", swap("q1"), 101)).unwrap();
        // A re-quote before the signature replaces it; a release drops it with the attempt.
        store.update_commands(&lock, 102, |b| b.set_swap("c1", swap("q2"), 102)).unwrap();
        store.update_commands(&lock, 103, |b| b.mark_signing("c1", Vec::new(), 103)).unwrap();
        let err = store.update_commands(&lock, 104, |b| b.set_swap("c1", swap("q3"), 104)).unwrap_err();
        assert!(err.to_string().contains("signed already"), "{err}");
        let record = store.commands(&lock).unwrap().get("c1").cloned().unwrap();
        assert_eq!(record.attempt.and_then(|a| a.swap).map(|s| s.quote_id), Some("q2".to_string()));
        // An attempt written before swaps were recorded still reads.
        let old = r#"{"canton_command_id":"ctl-c1","ledger_offset":7,"started_at":100,"signing":true}"#;
        assert_eq!(serde_json::from_str::<CommandAttempt>(old).unwrap().swap, None);
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn command_ids_single_use() {
        let base = temp_dir("commands");
        let store = store(&base);
        let lock = store.lock().await.unwrap();
        store.update_commands(&lock, 100, |b| b.receive("c1", b"cmd", 1000, 100).map(|_| ())).unwrap();
        let err = store.update_commands(&lock, 100, |b| b.receive("c1", b"other", 1000, 100).map(|_| ())).unwrap_err();
        assert!(err.to_string().contains("other bytes"), "{err}");
        store.update_commands(&lock, 101, |b| b.begin("c1", attempt(7), 101)).unwrap();
        let err = store.update_commands(&lock, 101, |b| b.begin("c1", attempt(8), 101)).unwrap_err();
        assert!(err.to_string().contains("already used"), "{err}");
        // An attempt that never reached its signature is received again; a signing one is not.
        store.update_commands(&lock, 102, |b| b.release("c1", 102)).unwrap();
        store.update_commands(&lock, 102, |b| b.begin("c1", attempt(9), 102)).unwrap();
        let usage = vec![CommandUsage {
            bucket: "transfers".into(),
            instrument_admin: "dso::1220aa".into(),
            instrument_id: "Amulet".into(),
            amount: "10".into(),
        }];
        store.update_commands(&lock, 103, |b| b.mark_signing("c1", usage.clone(), 103)).unwrap();
        let err = store.update_commands(&lock, 103, |b| b.release("c1", 103)).unwrap_err();
        assert!(err.to_string().contains("only reconciled"), "{err}");
        assert!(store.update_commands(&lock, 103, |b| b.mark_signing("c1", Vec::new(), 103)).is_err());
        let receipt = StoredReceipt::new(b"receipt", &[5u8; 64]);
        let executed = CommandEnd::Executed { update_id: "u1".into() };
        store.update_commands(&lock, 104, |b| b.finish("c1", &executed, receipt.clone(), 104)).unwrap();
        let failed = CommandEnd::Failed { code: 1409 };
        let err = store.update_commands(&lock, 105, |b| b.finish("c1", &failed, receipt.clone(), 105)).unwrap_err();
        assert!(err.to_string().contains("settled already"), "{err}");
        let book = store.commands(&lock).unwrap();
        assert_eq!(book.unposted().count(), 1);
        store.update_commands(&lock, 106, |b| b.mark_receipt_posted("c1", 106)).unwrap();
        let record = store.commands(&lock).unwrap().get("c1").cloned().unwrap();
        assert_eq!(record.status, CommandStatus::Executed);
        assert_eq!(record.update_id.as_deref(), Some("u1"));
        assert_eq!(record.attempt.as_ref().map(|a| (a.ledger_offset, a.usage.clone())), Some((9, usage)));
        assert!(record.receipt_posted && record.may_be_signed());
        assert_eq!(record.receipt.unwrap().decode().unwrap(), (b"receipt".to_vec(), [5u8; 64]));
        // Seven days after expiry the record is pruned.
        let later = 1000 + COMMAND_RETENTION_SECS;
        assert!(store.update_commands(&lock, later, |b| Ok(b.get("c1").is_none())).unwrap());
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn only_a_signed_attempt_executes_and_an_executing_one_is_never_pruned() {
        let base = temp_dir("commands-exec");
        let store = store(&base);
        let lock = store.lock().await.unwrap();
        let receipt = StoredReceipt::new(b"r", &[1u8; 64]);
        let executed = CommandEnd::Executed { update_id: "u".into() };
        store.update_commands(&lock, 100, |b| b.receive("c1", b"cmd", 1000, 100).map(|_| ())).unwrap();
        let err = store.update_commands(&lock, 100, |b| b.finish("c1", &executed, receipt.clone(), 100)).unwrap_err();
        assert!(err.to_string().contains("never signed"), "{err}");
        store.update_commands(&lock, 101, |b| b.begin("c1", attempt(1), 101)).unwrap();
        let verdict = CommandVerdict { status: CommandStatus::Refused, code: 4124, from_ledger: true };
        store.update_commands(&lock, 101, |b| b.set_verdict("c1", verdict.clone(), 101)).unwrap();
        let executed_verdict = CommandVerdict { status: CommandStatus::Executed, ..verdict };
        assert!(store.update_commands(&lock, 101, |b| b.set_verdict("c1", executed_verdict, 101)).is_err());
        let later = 1000 + COMMAND_RETENTION_SECS;
        let kept = store.update_commands(&lock, later, |b| Ok(b.executing().count())).unwrap();
        assert_eq!(kept, 1);
        // A refusal settles from either open state.
        store.update_commands(&lock, later, |b| b.finish("c1", &CommandEnd::Refused { code: 1106 }, receipt, later)).unwrap();
        let record = store.commands(&lock).unwrap().get("c1").cloned().unwrap();
        assert_eq!((record.status, record.reason_code), (CommandStatus::Refused, Some(1106)));
        let _ = fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn pending_entries_are_kept_until_accepted_and_never_resigned_after_refusal() {
        let base = temp_dir("pending");
        let store = store(&base);
        let mut chain = TestChain::build(SEED, store.party(), Network::Devnet, 1).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries, &[]).unwrap();
        let pending = chain.add_pending(0x51).unwrap();
        let signed = chain.push(&pending).unwrap();
        store.check_can_sign(&lock).unwrap();
        store.record_pending(&lock, &signed, None, 10).unwrap();
        // The same entry again is idempotent; another one at that seq is refused.
        store.record_pending(&lock, &signed, None, 11).unwrap();
        let err = store.check_can_sign(&lock).unwrap_err().to_string();
        assert!(err.contains("awaits the rpc's answer"), "{err}");
        let mut fork = TestChain::new(SEED, store.party(), Network::Devnet).unwrap();
        fork.state = store_state(&store);
        fork.created_at = chain.created_at.checked_add_secs(5).unwrap();
        let other = fork.add_pending(0x52).unwrap();
        let other = fork.push(&other).unwrap();
        assert_eq!(other.seq(), signed.seq());
        assert!(store.record_pending(&lock, &other, None, 12).is_err());
        // The log head is unchanged until the rpc accepts.
        assert_eq!(store.load(&lock).unwrap().unwrap().log.head().seq, 1);
        // A wrong head from the rpc is refused; the right one appends.
        let wrong = HeadRef { seq: 2, hash: *other.hash() };
        assert!(store.accept_pending(&lock, &wrong).is_err());
        let head = HeadRef { seq: 2, hash: *signed.hash() };
        assert_eq!(store.accept_pending(&lock, &head).unwrap(), head);
        assert!(store.pending(&lock).unwrap().is_none());
        assert!(!store.dir().join(PENDING_FILE).exists());

        // A refused entry stays and blocks its seq.
        let next = chain.add_pending(0x53).unwrap();
        let next = chain.push(&next).unwrap();
        store.record_pending(&lock, &next, None, 20).unwrap();
        store.refuse_pending(&lock, Some(4090), 21).unwrap();
        let err = store.check_can_sign(&lock).unwrap_err().to_string();
        assert!(err.contains("refused") && err.contains("code 4090") && err.contains("not signed again"), "{err}");
        assert!(store.accept_pending(&lock, &HeadRef { seq: 3, hash: *next.hash() }).is_err());
        let kept = store.pending(&lock).unwrap().unwrap();
        assert_eq!((kept.status, kept.refused_code), (PendingStatus::Refused, Some(4090)));
        assert_eq!(kept.entry, next);
        let _ = fs::remove_dir_all(&base);
    }

    fn store_state(store: &ControlStore) -> ControlState {
        let stored = store.snapshot().unwrap().unwrap();
        replay(&stored.log, &stored.documents).unwrap()
    }

    use agent_control::ControlState;

    #[tokio::test]
    async fn a_pending_entry_already_in_the_log_reads_as_none() {
        let base = temp_dir("stale");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 2).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries[..1], &[]).unwrap();
        store.record_pending(&lock, &chain.entries[1], None, 1).unwrap();
        // The append landed but the pending file was not removed (crash).
        let pending = fs::read(store.dir().join(PENDING_FILE)).unwrap();
        store.accept_pending(&lock, &chain.state.head().unwrap()).unwrap();
        fs::write(store.dir().join(PENDING_FILE), pending).unwrap();
        assert!(store.pending(&lock).unwrap().is_none());
        store.check_can_sign(&lock).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn find_locates_the_store_by_key_and_evidence_round_trips() {
        let base = temp_dir("find");
        let party = party_for(SEED, "agent-1").unwrap();
        let key = SigningKey::from_bytes(&SEED).verifying_key().to_bytes();
        assert!(ControlStore::find(&base, Network::Devnet, &key).unwrap().is_none());
        let store = ControlStore::open(&base, Network::Devnet, &party);
        let chain = TestChain::build(SEED, &party, Network::Devnet, 1).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let lock = store.lock().await.unwrap();
            store.append(&lock, &chain.entries, &[]).unwrap();
        });
        assert_eq!(ControlStore::find(&base, Network::Devnet, &key).unwrap(), Some(store.clone()));
        assert!(ControlStore::find(&base, Network::Mainnet, &key).unwrap().is_none());
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes();
        assert!(ControlStore::find(&base, Network::Devnet, &other).unwrap().is_none());
        let evidence = RegisterEvidence {
            public_key_hex: hex::encode(key),
            invite_code: "AGT-x".into(),
            canonical_b64: "Y2Fu".into(),
            signature_b64: "c2ln".into(),
            recorded_at: 5,
        };
        store.write_evidence(&evidence).unwrap();
        assert_eq!(store.read_evidence().unwrap(), Some(evidence));
        let _ = fs::remove_dir_all(&base);
    }

    use ed25519_dalek::SigningKey;

    #[test]
    fn markers_and_chains_parse_strictly() {
        let hash = Hash32([0x5a; 32]);
        let text = format!("devnet:3:{hash}");
        let marker = ControlMarker::parse(&text).unwrap();
        assert_eq!(marker.to_string(), text);
        assert_eq!(marker.head.seq, 3);
        for bad in [
            String::new(),
            "devnet:3".into(),
            format!("testnet:3:{hash}"),
            format!("devnet:0:{hash}"),
            format!("devnet:03:{hash}"),
            format!("devnet:3:{}", hash.to_string().to_uppercase()),
            format!("devnet:3:{hash}:x"),
        ] {
            assert!(ControlMarker::parse(&bad).is_err(), "{bad}");
        }
        assert_eq!(chain_network(None), ChainNetwork::Unset);
        assert_eq!(chain_network(Some(" ")), ChainNetwork::Unset);
        assert_eq!(chain_network(Some("mainnet")), ChainNetwork::Mapped(Network::Mainnet));
        assert_eq!(chain_network(Some("testnet")), ChainNetwork::Unmapped("testnet".into()));
        let env = ControlEnv { marker: Some("  ".into()), ..ControlEnv::default() };
        assert_eq!(env.marker().unwrap(), None);
        let env = ControlEnv { marker: Some("bogus".into()), ..ControlEnv::default() };
        assert!(env.marker().is_err());
        // The marker covers the PARTY_AGENT key, or every key when it is unset.
        let key = SigningKey::from_bytes(&SEED).verifying_key().to_bytes();
        let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes();
        let env = ControlEnv { party: Some(party_for(SEED, "a").unwrap().to_string()), ..ControlEnv::default() };
        assert!(env.marker_covers(&key) && !env.marker_covers(&other));
        assert!(ControlEnv::default().marker_covers(&other));
        assert!(base_dir(None, None).is_err());
        assert_eq!(base_dir(Some(OsStr::new("/x")), Some(OsStr::new("/h"))).unwrap(), PathBuf::from("/x"));
        assert_eq!(base_dir(None, Some(OsStr::new("/h"))).unwrap(), PathBuf::from("/h/.silvana-agent"));
    }

    #[tokio::test]
    async fn sign_entry_records_before_upload_and_never_resigns_a_refused_seq() {
        let seed = [0x31u8; 32];
        let party = party_for(seed, "agent-1").unwrap();
        let base = temp_dir("sign-entry");
        let store = ControlStore::open(&base, Network::Devnet, &party);
        let mut chain = TestChain::new(seed, &party, Network::Devnet).unwrap();
        let key = crate::secret::Secret::seal(&mut { seed }).unwrap();
        let env = ControlEnv {
            control_dir: Some(base.clone().into_os_string()),
            chain: Some("devnet".into()),
            party: Some(party.to_string()),
            ..ControlEnv::default()
        };
        let signer = crate::signer::resolve_with(&key, &env).unwrap();
        assert!(!signer.guard().is_linked());
        let link = chain.link_pending(0x21).unwrap();
        let lock = store.lock().await.unwrap();
        let signed = store.sign_entry(&lock, &signer, &link, 1).unwrap();
        assert_eq!(signed, chain.push(&link).unwrap());
        // Asked again, the stored entry comes back; nothing is accepted yet.
        assert_eq!(store.sign_entry(&lock, &signer, &link, 2).unwrap(), signed);
        assert!(store.load(&lock).unwrap().is_none());
        assert!(store.exists());
        // Another entry at that seq is refused.
        let mut other = TestChain::new(seed, &party, Network::Devnet).unwrap();
        other.created_at = other.created_at.checked_add_secs(10).unwrap();
        let other_link = other.link_pending(0x22).unwrap();
        assert!(store.sign_entry(&lock, &signer, &other_link, 3).is_err());
        // Refused: kept, and its seq is not signed again, not even with the same bytes.
        store.refuse_pending(&lock, Some(4090), 4).unwrap();
        let err = store.sign_entry(&lock, &signer, &link, 5).unwrap_err().to_string();
        assert!(err.contains("not signed again"), "{err}");
        let _ = fs::remove_dir_all(&base);
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn write_atomic_replaces_durably_keeps_the_mode_and_follows_symlinks() {
        let dir = temp_dir("atomic");
        let path = dir.join(".env");
        write_atomic(&path, b"A=1\n", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"A=1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(mode(&path), 0o600, "a new file is 0600");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            write_atomic(&path, b"A=2\n", 0o600).unwrap();
            assert_eq!(mode(&path), 0o640, "an existing file keeps its mode");
            // A symlinked file is written through, and the link stays a link.
            let real = dir.join("real.env");
            fs::write(&real, b"B=1\n").unwrap();
            let link = dir.join("link.env");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            write_atomic(&link, b"B=2\n", 0o600).unwrap();
            assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert_eq!(fs::read(&real).unwrap(), b"B=2\n");
            // A symlink to nothing is refused, not replaced.
            let dangling = dir.join("dangling.env");
            std::os::unix::fs::symlink(dir.join("missing.env"), &dangling).unwrap();
            assert!(write_atomic(&dangling, b"C=1\n", 0o600).is_err());
            assert!(fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        }
        assert_eq!(fs::read(&path).unwrap(), b"A=2\n");
        // A failed replace leaves the old file whole and no temp file behind.
        let blocked = dir.join("blocked");
        fs::create_dir_all(blocked.join("inner")).unwrap();
        assert!(write_atomic(&blocked, b"x", 0o600).is_err());
        assert!(blocked.join("inner").is_dir());
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
        // EBUSY (a bind-mounted file) gets a clear message.
        let busy = replace_error(std::io::Error::from(ErrorKind::ResourceBusy), &path).to_string();
        assert!(busy.contains("bind mount") && busy.contains("not changed"), "{busy}");
        #[cfg(unix)]
        assert!(replace_error(std::io::Error::from_raw_os_error(16), &path).to_string().contains("bind mount"));
        let other = format!("{:#}", replace_error(std::io::Error::from(ErrorKind::PermissionDenied), &path));
        assert!(other.contains("replace") && !other.contains("bind mount"), "{other}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn control_files_stay_0600_and_leave_no_temp_files() {
        let base = temp_dir("save");
        let store = store(&base);
        let chain = TestChain::build(SEED, store.party(), Network::Devnet, 2).unwrap();
        let lock = store.lock().await.unwrap();
        store.append(&lock, &chain.entries, &[]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let log = store.dir().join(LOG_FILE);
            fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
            save_json(&log, &serde_json::from_slice::<LogFile>(&fs::read(&log).unwrap()).unwrap()).unwrap();
            assert_eq!(mode(&log), 0o600, "control files are always 0600");
        }
        assert!(leftovers(store.dir()).is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn accept_pending_builds_policy_acceptances_in_order() {
        let party = party_for(SEED, "agent-1").unwrap();
        let mut chain = TestChain::build(SEED, &party, Network::Devnet, 1).unwrap();
        for version in 1..=3u64 {
            let accept = chain.accept_pending(0x21).unwrap();
            assert!(accept.policy_document().is_some());
            chain.push(&accept).unwrap();
            assert_eq!(chain.state.latest_policy().map(|p| p.version()), Some(version));
        }
        assert_eq!(chain.entries.len(), 4);
        let churn = TestChain::churn(SEED, &party, Network::Devnet, 40).unwrap();
        assert_eq!(churn.entries.len(), 81, "past the controller cap: every grant is withdrawn");
        assert!(agent_control::verify_log(Network::Devnet, &party, &churn.entries).is_ok());
    }

    #[tokio::test]
    async fn commands_json_stays_within_its_budget_and_readable() {
        let base = temp_dir("commands-budget");
        let store = store(&base);
        let lock = store.lock().await.unwrap();
        let (big, budget) = (vec![7u8; 3000], 64 * 1024);
        let receipt = StoredReceipt::new(&[1u8; 2000], &[2u8; 64]);
        let refused = CommandEnd::Refused { code: 1104 };
        // Settled, delivered and expired an hour ago: they go first, oldest first.
        for i in 0..40i64 {
            let id = format!("old{i:02}");
            store
                .update_commands_within(&lock, 10_000, budget, |b| {
                    b.receive(&id, &big, 1_000, 1_000 + i)?;
                    b.finish(&id, &refused, receipt.clone(), 1_000 + i)?;
                    b.mark_receipt_posted(&id, 1_000 + i)
                })
                .unwrap();
        }
        let size = || fs::metadata(store.path(COMMANDS_FILE)).unwrap().len();
        let book = store.commands(&lock).unwrap();
        assert!(book.get("old39").is_some() && book.get("old00").is_none(), "the oldest went first");
        assert!(size() <= budget as u64, "{}", size());
        // A record that may still run is never dropped: past the budget a new one is refused,
        // nothing is written, and the book stays readable and settles.
        let mut full = None;
        for i in 0..40 {
            let id = format!("live{i:02}");
            let kept = store.update_commands_within(&lock, 10_000, budget, |b| b.receive(&id, &big, 20_000, 10_000).map(|_| ()));
            if let Err(e) = kept {
                full = Some((id, e.to_string()));
                break;
            }
        }
        let (id, err) = full.expect("a new record is refused past the budget");
        assert!(err.contains("is full"), "{err}");
        let book = store.commands(&lock).unwrap();
        assert!(book.get(&id).is_none() && book.get("live00").is_some());
        assert!(book.records().iter().all(|r| r.command_id.starts_with("live")), "every settled one made room first");
        store
            .update_commands_within(&lock, 10_001, budget, |b| b.finish("live00", &refused, receipt.clone(), 10_001))
            .unwrap();
        assert_eq!(store.commands(&lock).unwrap().get("live00").map(|r| r.status), Some(CommandStatus::Refused));
        // What could not be read back is never written.
        let huge = vec![b' '; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
        assert!(save_bytes(&store.path(COUNTERS_FILE), &huge).is_err());
        assert!(!store.path(COUNTERS_FILE).exists());
        drop(lock);
        let _ = fs::remove_dir_all(&base);
    }
}
