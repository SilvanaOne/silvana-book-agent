//! The seen-heads pin: the longest verified head this key has seen per agent, kept beside the
//! key. A log shorter than it, or with another entry at its seq, refuses every signature.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use agent_control::{Hash32, HeadRef, Network, PartyId, VerifiedLog};
use serde::{Deserialize, Serialize};

use crate::error::ControllerError;

/// Name of the pin file beside the key file.
pub const SEEN_HEADS_FILE: &str = "seen-heads.json";
/// Largest pin file read.
const MAX_PIN_FILE_BYTES: u64 = 1024 * 1024;
/// Most agents one pin file holds.
const MAX_PINNED_AGENTS: usize = 4096;
const PIN_FILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinFile {
    version: u32,
    heads: BTreeMap<String, PinnedHead>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedHead {
    seq: u64,
    hash: String,
}

/// How a verified log relates to the pinned head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinCheck {
    /// No head was pinned for this agent yet.
    New,
    /// The log ends at the pinned head.
    Same,
    /// The log extends the pinned head.
    Ahead { pinned: HeadRef },
    /// The log ends before the pinned head: a rollback or a hidden tail.
    Shorter { pinned: HeadRef },
    /// The log has another entry at the pinned seq.
    Divergent { pinned: HeadRef },
}

impl PinCheck {
    /// True when signatures may proceed.
    pub fn allows_signing(&self) -> bool {
        matches!(self, Self::New | Self::Same | Self::Ahead { .. })
    }
}

/// The pin file of one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenHeads {
    path: PathBuf,
}

fn pin_key(network: Network, party: &PartyId) -> String {
    format!("{network}:{party}")
}

impl SeenHeads {
    /// The pin file in the key file's directory.
    pub fn beside(key_file: &Path) -> Self {
        let dir = key_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self {
            path: dir.join(SEEN_HEADS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<PinFile, ControllerError> {
        let io =
            |e: std::io::Error| ControllerError::Io(format!("read {}: {e}", self.path.display()));
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PinFile {
                    version: PIN_FILE_VERSION,
                    heads: BTreeMap::new(),
                });
            }
            Err(e) => return Err(io(e)),
        };
        let mut raw = Vec::new();
        file.take(MAX_PIN_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut raw)
            .map_err(io)?;
        if u64::try_from(raw.len()).map_or(true, |n| n > MAX_PIN_FILE_BYTES) {
            return Err(ControllerError::Pin(format!(
                "{} is too large to be a pin file",
                self.path.display()
            )));
        }
        let parsed: PinFile = serde_json::from_slice(&raw).map_err(|e| {
            ControllerError::Pin(format!("{} is not a pin file ({e})", self.path.display()))
        })?;
        if parsed.version != PIN_FILE_VERSION {
            return Err(ControllerError::Pin(format!(
                "{} has an unknown version",
                self.path.display()
            )));
        }
        Ok(parsed)
    }

    fn pinned(
        &self,
        file: &PinFile,
        network: Network,
        party: &PartyId,
    ) -> Result<Option<HeadRef>, ControllerError> {
        let Some(head) = file.heads.get(&pin_key(network, party)) else {
            return Ok(None);
        };
        let hash = Hash32::parse(&head.hash).map_err(|_| {
            ControllerError::Pin(format!("{} holds a malformed head", self.path.display()))
        })?;
        Ok(Some(HeadRef {
            seq: head.seq,
            hash,
        }))
    }

    /// The pinned head of an agent, if any.
    pub fn get(
        &self,
        network: Network,
        party: &PartyId,
    ) -> Result<Option<HeadRef>, ControllerError> {
        let file = self.read()?;
        self.pinned(&file, network, party)
    }

    /// Compares a verified log with the pinned head.
    pub fn check(&self, log: &VerifiedLog) -> Result<PinCheck, ControllerError> {
        let Some(pinned) = self.get(log.network(), log.agent_party())? else {
            return Ok(PinCheck::New);
        };
        let head = log.head();
        if pinned.seq > head.seq {
            return Ok(PinCheck::Shorter { pinned });
        }
        match log.entry(pinned.seq) {
            Some(entry) if *entry.hash() == pinned.hash => Ok(if pinned.seq == head.seq {
                PinCheck::Same
            } else {
                PinCheck::Ahead { pinned }
            }),
            _ => Ok(PinCheck::Divergent { pinned }),
        }
    }

    /// Raises the pinned head to `head`; never lowers it. Written 0600 by atomic rename.
    pub fn raise(
        &self,
        network: Network,
        party: &PartyId,
        head: HeadRef,
    ) -> Result<(), ControllerError> {
        let mut file = self.read()?;
        if self
            .pinned(&file, network, party)?
            .is_some_and(|pinned| pinned.seq >= head.seq)
        {
            return Ok(());
        }
        let key = pin_key(network, party);
        if !file.heads.contains_key(&key) && file.heads.len() >= MAX_PINNED_AGENTS {
            return Err(ControllerError::Pin(format!(
                "{} holds too many agents",
                self.path.display()
            )));
        }
        file.heads.insert(
            key,
            PinnedHead {
                seq: head.seq,
                hash: head.hash.to_string(),
            },
        );
        let text = serde_json::to_vec_pretty(&file)
            .map_err(|e| ControllerError::Io(format!("encode {}: {e}", self.path.display())))?;
        write_atomic(&self.path, &text)
    }
}

/// Writes `bytes` to a temporary file beside `path` (0600), syncs it, then renames it over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ControllerError> {
    let io = |what: &str, e: std::io::Error| {
        ControllerError::Io(format!("{what} {}: {e}", path.display()))
    };
    let name = path.file_name().map_or_else(
        || SEEN_HEADS_FILE.into(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp).map_err(|e| io("create", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| io("restrict", e))?;
        }
        file.write_all(bytes).map_err(|e| io("write", e))?;
        file.sync_all().map_err(|e| io("sync", e))?;
        std::fs::rename(&temp, path).map_err(|e| io("replace", e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{TestAgent, temp_dir};

    #[test]
    fn the_pin_only_rises_and_refuses_a_shorter_or_divergent_log() {
        let dir = temp_dir("pin");
        let pins = SeenHeads::beside(&dir.join("controller.key"));
        let mut agent = TestAgent::devnet([0x31; 32], &[0x41]).unwrap();
        let short = agent.verified_log().unwrap();
        assert_eq!(pins.check(&short).unwrap(), PinCheck::New);
        agent.add_ed25519(0x42, "read", 0).unwrap();
        let long = agent.verified_log().unwrap();
        pins.raise(Network::Devnet, &agent.party, long.head())
            .unwrap();
        assert_eq!(pins.check(&long).unwrap(), PinCheck::Same);
        assert_eq!(
            pins.check(&short).unwrap(),
            PinCheck::Shorter {
                pinned: long.head()
            }
        );
        // Raising to an older head keeps the newer one.
        pins.raise(Network::Devnet, &agent.party, short.head())
            .unwrap();
        assert_eq!(
            pins.get(Network::Devnet, &agent.party).unwrap(),
            Some(long.head())
        );
        // Another chain with the same party and length diverges.
        let mut fork = TestAgent::devnet([0x31; 32], &[0x41]).unwrap();
        fork.add_ed25519(0x43, "read", 0).unwrap();
        let forked = fork.verified_log().unwrap();
        assert_eq!(
            pins.check(&forked).unwrap(),
            PinCheck::Divergent {
                pinned: long.head()
            }
        );
        assert!(
            !PinCheck::Divergent {
                pinned: long.head()
            }
            .allows_signing()
        );
        // Longer logs that extend the pin are fine.
        agent.add_ed25519(0x44, "read", 0).unwrap();
        assert_eq!(
            pins.check(&agent.verified_log().unwrap()).unwrap(),
            PinCheck::Ahead {
                pinned: long.head()
            }
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(pins.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // A pin file that is not one refuses.
        std::fs::write(pins.path(), b"{\"version\":1,\"heads\":{},\"extra\":1}").unwrap();
        assert!(matches!(pins.check(&long), Err(ControllerError::Pin(_))));
    }
}
