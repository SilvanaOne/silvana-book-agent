//! Durable local state of the `usdcx` commands, one directory per network and
//! party: the burns this agent signed and the deposits it sent from Ethereum.
//!
//! The burn log is the client's own guard, independent of the server: a
//! second signature for one withdrawal is allowed only when it spends a
//! holding every earlier signature for that withdrawal spent (the burn
//! consumes its input holdings, so at most one of them can commit), and a new
//! withdrawal is refused while an earlier one may still commit.
//!
//! Every read-modify-write runs under an exclusive lock on the log, and every
//! save is atomic and durable (temporary file, fsync, rename, directory fsync).

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The burn log of a party directory.
pub const BURNS_FILE: &str = "burns.json";
/// The deposit log of a party directory.
pub const DEPOSITS_FILE: &str = "deposits.json";

/// How long a command waits for another one holding the same log.
const LOCK_WAIT: Duration = Duration::from_secs(30);
const LOCK_POLL: Duration = Duration::from_millis(250);

/// The state's folder inside the agent folder.
pub const STATE_DIR: &str = "usdcx";

/// The default base directory, `<agent folder>/usdcx`.
pub fn default_base(agent_dir: &Path) -> PathBuf {
    agent_dir.join(STATE_DIR)
}

/// Where earlier versions kept the state: `$HOME/.silvana-agent/usdcx`.
pub fn legacy_base(home: Option<&OsStr>) -> Option<PathBuf> {
    home.filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".silvana-agent").join(STATE_DIR))
}

/// The party directory under `base`, after the one-time move of this party's state from
/// `legacy_base`; that base keeps one folder per network and party, so only this one moves.
pub fn settle_party_dir(
    base: &Path,
    legacy_base: Option<&Path>,
    network: &str,
    party: &str,
) -> Result<PathBuf> {
    let dir = party_dir(base, network, party);
    if let Some(legacy) = legacy_base.map(|b| party_dir(b, network, party)) {
        let verify = |copy: &Path| -> Result<()> {
            BurnLog::load(copy)?;
            DepositLog::load(copy)?;
            Ok(())
        };
        let locks = ["burns.json.lock", "deposits.json.lock"];
        agent_logic::agent_dir::move_once("USDCx state", &legacy, &dir, &locks, &verify)?;
    }
    Ok(dir)
}

/// The directory of one party's state on one network.
pub fn party_dir(base: &Path, network: &str, party: &str) -> PathBuf {
    base.join(network).join(file_component(party))
}

/// A party id as a file name: every character outside `[A-Za-z0-9._-]`
/// becomes `_` (party ids are `hint::fingerprint`, so this stays unique).
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

/// An exclusive lock on one log of a party directory, released on drop.
pub struct LogLock {
    file: File,
}

impl LogLock {
    /// Waits up to [`LOCK_WAIT`] for the lock of `dir/file`.
    pub async fn acquire(dir: &Path, file: &str) -> Result<Self> {
        create_private_dir(dir)?;
        let path = dir.join(format!("{file}.lock"));
        let handle = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let deadline = tokio::time::Instant::now().checked_add(LOCK_WAIT);
        loop {
            match handle.try_lock() {
                Ok(()) => return Ok(Self { file: handle }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if deadline.is_none_or(|d| tokio::time::Instant::now() >= d) {
                        bail!(
                            "another usdcx command of this party holds {} — retry when it finishes",
                            path.display()
                        );
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("lock {}", path.display()));
                }
            }
        }
    }
}

impl Drop for LogLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
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

/// Reads `path`; a missing file is the default value.
pub(crate) fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a valid usdcx log", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Writes `value` to `path` atomically and durably.
pub(crate) fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    let json = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(&json)?;
        file.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    sync_dir(dir).with_context(|| format!("sync {}", dir.display()))?;
    Ok(())
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

// ============================================================================
// Burns
// ============================================================================

/// Where a withdrawal stands, as far as this agent knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BurnStatus {
    /// Signed at least once; it may or may not have committed.
    Signed,
    /// The burn committed: DA will release or revoke it.
    Committed,
    /// None of its signatures committed or ever can.
    Failed,
    /// DA accepted it: Circle releases the USDC on Ethereum.
    Accepted,
    /// DA revoked it: reclaim the USDCx.
    Revoked,
    /// The USDCx of the revoked withdrawal was reclaimed.
    Reclaimed,
}

impl BurnStatus {
    /// Whether a burn of this withdrawal may still commit.
    pub fn is_unsettled(self) -> bool {
        self == BurnStatus::Signed
    }

    /// Whether the burn is known to be on the ledger.
    pub fn is_committed(self) -> bool {
        matches!(
            self,
            BurnStatus::Committed
                | BurnStatus::Accepted
                | BurnStatus::Revoked
                | BurnStatus::Reclaimed
        )
    }
}

/// One withdrawal (one request id) and every signature given for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnEntry {
    pub request_id: String,
    pub amount: String,
    /// `0x` + 40 lowercase hex.
    pub destination: String,
    /// The holdings every signature for this request spent.
    pub shared_holdings: Vec<String>,
    /// The command ids signed for this request.
    pub command_ids: Vec<String>,
    /// Ledger end read before the first signature: scans start here.
    pub ledger_offset: i64,
    pub first_signed_at: DateTime<Utc>,
    pub last_signed_at: DateTime<Utc>,
    pub status: BurnStatus,
    #[serde(default)]
    pub update_id: Option<String>,
    #[serde(default)]
    pub attestation_cid: Option<String>,
    /// DA's trace id (accepted) or reason (revoked).
    #[serde(default)]
    pub detail: Option<String>,
    /// The revoked attestation to reclaim, once revoked.
    #[serde(default)]
    pub revoked_cid: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BurnLog {
    #[serde(default)]
    pub burns: Vec<BurnEntry>,
}

/// What a signature adds to a withdrawal's entry.
pub struct Signature<'a> {
    pub request_id: &'a str,
    pub amount: &'a str,
    pub destination: &'a str,
    pub holdings: &'a [String],
    pub command_id: &'a str,
    pub ledger_offset: i64,
    pub at: DateTime<Utc>,
}

impl BurnLog {
    pub fn load(dir: &Path) -> Result<Self> {
        load(&dir.join(BURNS_FILE))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save(&dir.join(BURNS_FILE), self)
    }

    pub fn get(&self, request_id: &str) -> Option<&BurnEntry> {
        self.burns.iter().find(|b| b.request_id == request_id)
    }

    pub fn get_mut(&mut self, request_id: &str) -> Option<&mut BurnEntry> {
        self.burns.iter_mut().find(|b| b.request_id == request_id)
    }

    /// Withdrawals that may still commit.
    pub fn unsettled(&self) -> impl Iterator<Item = &BurnEntry> {
        self.burns.iter().filter(|b| b.status.is_unsettled())
    }

    /// Records a signature for a withdrawal, refusing it when it could make a
    /// second burn of the same withdrawal commit: a new signature must spend a
    /// holding that every earlier one spent. Refuses a new withdrawal while
    /// another may still commit, and different terms under a known request id.
    pub fn record_signature(&mut self, s: &Signature<'_>) -> Result<()> {
        if s.holdings.is_empty() {
            bail!("the burn spends no holdings");
        }
        if let Some(other) = self
            .burns
            .iter()
            .find(|b| b.status.is_unsettled() && b.request_id != s.request_id)
        {
            bail!(
                "withdrawal {} may still commit: settle it before signing another",
                other.request_id
            );
        }
        match self.get_mut(s.request_id) {
            None => {
                self.burns.push(BurnEntry {
                    request_id: s.request_id.to_string(),
                    amount: s.amount.to_string(),
                    destination: s.destination.to_string(),
                    shared_holdings: s.holdings.to_vec(),
                    command_ids: vec![s.command_id.to_string()],
                    ledger_offset: s.ledger_offset,
                    first_signed_at: s.at,
                    last_signed_at: s.at,
                    status: BurnStatus::Signed,
                    update_id: None,
                    attestation_cid: None,
                    detail: None,
                    revoked_cid: None,
                });
                Ok(())
            }
            Some(entry) => {
                if entry.status.is_committed() {
                    bail!("withdrawal {} has already burned", entry.request_id);
                }
                if entry.amount != s.amount || entry.destination != s.destination {
                    bail!(
                        "withdrawal {} was signed for {} USDCx to {}; a request id is never reused \
                         for other terms",
                        entry.request_id,
                        entry.amount,
                        entry.destination
                    );
                }
                let shared: Vec<String> = entry
                    .shared_holdings
                    .iter()
                    .filter(|h| s.holdings.contains(h))
                    .cloned()
                    .collect();
                if shared.is_empty() {
                    bail!(
                        "this burn of withdrawal {} spends none of the holdings every earlier \
                         signature for it spent ({}): signing it could burn twice",
                        entry.request_id,
                        entry.shared_holdings.join(", ")
                    );
                }
                entry.shared_holdings = shared;
                if !entry.command_ids.iter().any(|c| c == s.command_id) {
                    entry.command_ids.push(s.command_id.to_string());
                }
                entry.last_signed_at = s.at;
                entry.status = BurnStatus::Signed;
                Ok(())
            }
        }
    }
}

// ============================================================================
// Deposits
// ============================================================================

/// Where a deposit sent from this agent stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepositStatus {
    /// Recorded; no deposit transaction signed yet (an approve may be on its way).
    Started,
    /// The deposit transaction is signed and persisted; it may be on its way.
    Sent,
    /// Mined with a DepositedToRemote event for the party.
    Confirmed,
    /// The DepositAttestation is on Canton, not yet minted.
    Attested,
    /// The USDCx is minted.
    Minted,
    /// Nothing reached xReserve (refused, reverted, replaced or never sent).
    Failed,
}

impl DepositStatus {
    /// Whether the deposit may still be mined (a new one is refused meanwhile).
    pub fn may_be_unmined(self) -> bool {
        matches!(self, DepositStatus::Started | DepositStatus::Sent)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, DepositStatus::Minted | DepositStatus::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositEntry {
    /// Local id (UUIDv7).
    pub id: String,
    pub amount: String,
    /// The Ethereum account, lowercase.
    pub depositor: String,
    /// Whether the agent signed it (false: registered from a wallet).
    pub signed_here: bool,
    /// Ethereum block before sending: scans start here.
    pub eth_block: u64,
    /// Ledger end before sending: the mint is found after it.
    pub ledger_offset: i64,
    #[serde(default)]
    pub approve_tx: Option<String>,
    #[serde(default)]
    pub tx_hash: Option<String>,
    #[serde(default)]
    pub tx_nonce: Option<u64>,
    /// The signed deposit transaction (no secret), kept for a rebroadcast.
    #[serde(default)]
    pub raw_tx: Option<String>,
    /// The deposit nonce from the receipt (re-read while it may still reorg).
    #[serde(default)]
    pub deposit_nonce: Option<String>,
    #[serde(default)]
    pub message_hash: Option<String>,
    #[serde(default)]
    pub attestation_cid: Option<String>,
    #[serde(default)]
    pub update_id: Option<String>,
    #[serde(default)]
    pub holding_cid: Option<String>,
    pub status: DepositStatus,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DepositLog {
    #[serde(default)]
    pub deposits: Vec<DepositEntry>,
}

impl DepositLog {
    pub fn load(dir: &Path) -> Result<Self> {
        load(&dir.join(DEPOSITS_FILE))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save(&dir.join(DEPOSITS_FILE), self)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut DepositEntry> {
        self.deposits.iter_mut().find(|d| d.id == id)
    }

    /// A deposit this agent signed that may still be mined.
    pub fn open_unmined(&self) -> Option<&DepositEntry> {
        self.deposits
            .iter()
            .find(|d| d.signed_here && d.status.may_be_unmined())
    }

    pub fn open(&self) -> impl Iterator<Item = &DepositEntry> {
        self.deposits.iter().filter(|d| !d.status.is_terminal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-11-12T10:{minute:02}:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn sig<'a>(request_id: &'a str, holdings: &'a [String], command_id: &'a str) -> Signature<'a> {
        Signature {
            request_id,
            amount: "10",
            destination: "0x1234567890abcdef1234567890abcdef12345678",
            holdings,
            command_id,
            ledger_offset: 7,
            at: at(0),
        }
    }

    fn cids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_second_signature_must_share_a_holding_with_every_earlier_one() {
        let mut log = BurnLog::default();
        log.record_signature(&sig("r1", &cids(&["h1", "h2"]), "c1"))
            .unwrap();
        // Disjoint holdings could both commit: refused.
        let err = log
            .record_signature(&sig("r1", &cids(&["h3"]), "c2"))
            .unwrap_err();
        assert!(format!("{err:#}").contains("could burn twice"), "{err:#}");
        // Sharing h2 is allowed and narrows the shared set to h2.
        log.record_signature(&sig("r1", &cids(&["h2", "h3"]), "c1"))
            .unwrap();
        assert_eq!(log.get("r1").unwrap().shared_holdings, cids(&["h2"]));
        // A third signature sharing nothing with all earlier ones (h3 was not in the first) is refused.
        assert!(
            log.record_signature(&sig("r1", &cids(&["h3", "h4"]), "c1"))
                .is_err()
        );
        assert!(
            log.record_signature(&sig("r1", &cids(&["h2"]), "c1"))
                .is_ok()
        );
        assert_eq!(log.get("r1").unwrap().command_ids, cids(&["c1"]));
    }

    #[test]
    fn a_new_withdrawal_waits_for_every_earlier_one_to_settle() {
        let mut log = BurnLog::default();
        log.record_signature(&sig("r1", &cids(&["h1"]), "c1"))
            .unwrap();
        let err = log
            .record_signature(&sig("r2", &cids(&["h9"]), "c9"))
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("r1 may still commit"),
            "{err:#}"
        );
        log.get_mut("r1").unwrap().status = BurnStatus::Failed;
        log.record_signature(&sig("r2", &cids(&["h9"]), "c9"))
            .unwrap();
        // A committed burn is settled too, but is never signed again.
        log.get_mut("r2").unwrap().status = BurnStatus::Committed;
        assert!(
            log.record_signature(&sig("r2", &cids(&["h9"]), "c9"))
                .is_err()
        );
        assert!(
            log.record_signature(&sig("r3", &cids(&["h5"]), "c5"))
                .is_ok()
        );
    }

    #[test]
    fn a_request_id_keeps_its_terms() {
        let mut log = BurnLog::default();
        let h1 = cids(&["h1"]);
        log.record_signature(&sig("r1", &h1, "c1")).unwrap();
        log.get_mut("r1").unwrap().status = BurnStatus::Failed;
        let mut other = sig("r1", &h1, "c1");
        other.amount = "11";
        assert!(log.record_signature(&other).is_err());
        let mut other = sig("r1", &h1, "c1");
        other.destination = "0x0000000000000000000000000000000000000001";
        assert!(log.record_signature(&other).is_err());
        assert!(log.record_signature(&sig("r1", &[], "c1")).is_err());
    }

    #[test]
    fn logs_survive_a_reload_and_party_dirs_are_file_names() {
        let base = std::env::temp_dir().join(format!("usdcx-state-{}", std::process::id()));
        let dir = party_dir(&base, "mainnet", "tester::1220ab");
        assert!(dir.ends_with("mainnet/tester__1220ab"));
        let mut log = BurnLog::default();
        log.record_signature(&sig("r1", &cids(&["h1"]), "c1"))
            .unwrap();
        log.save(&dir).unwrap();
        let back = BurnLog::load(&dir).unwrap();
        assert_eq!(back.get("r1").unwrap().status, BurnStatus::Signed);
        assert!(DepositLog::load(&dir).unwrap().deposits.is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_state_lives_in_the_agent_folder_and_moves_only_this_partys_state() {
        let root = std::env::temp_dir().join(format!("usdcx-move-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("agent");
        assert_eq!(default_base(&folder), folder.join("usdcx"));
        assert_eq!(
            legacy_base(Some(OsStr::new("/h"))),
            Some(PathBuf::from("/h/.silvana-agent/usdcx"))
        );
        assert_eq!(legacy_base(None), None);
        let legacy = root.join("home/.silvana-agent/usdcx");
        let mut log = BurnLog::default();
        log.record_signature(&sig("r1", &cids(&["h1"]), "c1"))
            .unwrap();
        log.save(&party_dir(&legacy, "mainnet", "tester::1220ab"))
            .unwrap();
        log.save(&party_dir(&legacy, "mainnet", "other::1220cd"))
            .unwrap();
        let before = fs::read(party_dir(&legacy, "mainnet", "tester::1220ab").join(BURNS_FILE)).unwrap();
        let base = default_base(&folder);
        let dir = settle_party_dir(&base, Some(&legacy), "mainnet", "tester::1220ab").unwrap();
        assert_eq!(dir, party_dir(&base, "mainnet", "tester::1220ab"));
        assert_eq!(BurnLog::load(&dir).unwrap().get("r1").unwrap().status, BurnStatus::Signed);
        assert!(!base.join("mainnet").join("other__1220cd").exists(), "only this party's state");
        assert_eq!(fs::read(party_dir(&legacy, "mainnet", "tester::1220ab").join(BURNS_FILE)).unwrap(), before);
        // Moved once; later runs use it. A state not moved from the old one is refused.
        assert_eq!(settle_party_dir(&base, Some(&legacy), "mainnet", "tester::1220ab").unwrap(), dir);
        fs::create_dir_all(party_dir(&root.join("b"), "mainnet", "other::1220cd")).unwrap();
        let err = settle_party_dir(&root.join("b"), Some(&legacy), "mainnet", "other::1220cd").unwrap_err();
        assert!(format!("{err:#}").contains(&*legacy.to_string_lossy()), "{err:#}");
        // A copy that does not load is refused and removed.
        fs::create_dir_all(party_dir(&legacy, "mainnet", "bad::1220ef")).unwrap();
        fs::write(party_dir(&legacy, "mainnet", "bad::1220ef").join(BURNS_FILE), b"x").unwrap();
        assert!(settle_party_dir(&base, Some(&legacy), "mainnet", "bad::1220ef").is_err());
        assert!(!party_dir(&base, "mainnet", "bad::1220ef").exists());
        assert!(!base.join("mainnet").join("bad__1220ef.migrating").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_agent_folder_holding_the_old_state_uses_it_in_place() {
        let root = std::env::temp_dir().join(format!("usdcx-in-place-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let legacy = root.join("home/.silvana-agent/usdcx");
        let mut log = BurnLog::default();
        log.record_signature(&sig("r1", &cids(&["h1"]), "c1"))
            .unwrap();
        log.save(&party_dir(&legacy, "mainnet", "tester::1220ab"))
            .unwrap();
        let base = default_base(&root.join("home/.silvana-agent"));
        assert_eq!(base, legacy);
        let dir = settle_party_dir(&base, Some(&legacy), "mainnet", "tester::1220ab").unwrap();
        assert_eq!(dir, party_dir(&legacy, "mainnet", "tester::1220ab"));
        assert!(!fs::exists(dir.join(agent_logic::agent_dir::MOVED_FROM_FILE)).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_log_lock_is_exclusive() {
        let base = std::env::temp_dir().join(format!("usdcx-lock-{}", std::process::id()));
        let first = LogLock::acquire(&base, BURNS_FILE).await.unwrap();
        // The same process takes a second handle: try_lock reports it held.
        let path = base.join(format!("{BURNS_FILE}.lock"));
        let other = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(first);
        assert!(other.try_lock().is_ok());
        let _ = fs::remove_dir_all(&base);
    }
}
