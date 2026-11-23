//! The controller key: a strictly read key file, and typed signing only. The key never
//! signs raw bytes; each method signs the canonical encoding of one checked document.

use std::fmt;
use std::io::Read;
use std::path::Path;

use agent_control::{
    CheckedChallenge, Command, ControllerId, ControllerProof, ControllerType, PolicyDocument,
    ProofEnvelope, ReadSessionRequest, controller_id_for,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

use crate::error::ControllerError;

/// Name of the key file inside its directory.
pub const KEY_FILE: &str = "controller.key";
/// Largest key file read.
pub const MAX_KEY_FILE_BYTES: u64 = 1024;

/// A signed document: the exact bytes and the proof envelope over them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedDocument {
    pub bytes: Vec<u8>,
    pub envelope: ProofEnvelope,
}

impl SignedDocument {
    /// The Ed25519 signature inside the envelope.
    pub fn signature(&self) -> Option<[u8; 64]> {
        match self.envelope.proof() {
            ControllerProof::Ed25519 { signature } => Some(*signature),
            ControllerProof::WebAuthn(_) => None,
        }
    }

    /// The envelope's canonical bytes.
    pub fn proof_bytes(&self) -> Result<Vec<u8>, ControllerError> {
        self.envelope.encode().map_err(|e| {
            ControllerError::Refusal(format!("the proof does not encode ({})", e.code_name()))
        })
    }
}

/// An Ed25519 controller key; the seed is zeroized on drop and never shown.
pub struct ControllerSigner {
    key: SigningKey,
    public: [u8; 32],
    id: ControllerId,
}

impl fmt::Debug for ControllerSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControllerSigner")
            .field("controller", &self.id.fingerprint())
            .finish_non_exhaustive()
    }
}

fn key_error(path: &Path, text: &str) -> ControllerError {
    ControllerError::KeyFile(format!("{}: {text}", path.display()))
}

impl ControllerSigner {
    /// A signer over an in-memory key.
    pub fn from_signing_key(key: SigningKey) -> Self {
        let public = key.verifying_key().to_bytes();
        let id = controller_id_for(ControllerType::Ed25519, &public);
        Self { key, public, id }
    }

    /// A signer over `seed`; the caller clears its own copy.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self::from_signing_key(SigningKey::from_bytes(seed))
    }

    /// Reads a key file written by `controller keygen`: at most [`MAX_KEY_FILE_BYTES`], no
    /// group or other permission bits, each of `type`, `seed` and `public_key` exactly once.
    pub fn load(path: &Path) -> Result<Self, ControllerError> {
        let named = std::fs::metadata(path)
            .map_err(|e| key_error(path, &format!("cannot read it ({e})")))?;
        if !named.is_file() {
            return Err(key_error(path, "not a regular file"));
        }
        let file = std::fs::File::open(path)
            .map_err(|e| key_error(path, &format!("cannot open it ({e})")))?;
        // The checks hold for the file opened, which must be the one named.
        let metadata = file
            .metadata()
            .map_err(|e| key_error(path, &format!("cannot read it ({e})")))?;
        if !metadata.is_file() {
            return Err(key_error(path, "not a regular file"));
        }
        if metadata.len() > MAX_KEY_FILE_BYTES {
            return Err(key_error(path, "larger than a controller key file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.dev() != named.dev() || metadata.ino() != named.ino() {
                return Err(key_error(path, "it was replaced while it was opened"));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(key_error(
                    path,
                    "other users may read or change it; restrict it with chmod 600",
                ));
            }
        }
        // Allocated once, so that no copy of the seed is left in a freed buffer.
        let capacity = usize::try_from(MAX_KEY_FILE_BYTES.saturating_add(1)).unwrap_or(0);
        let mut raw = Zeroizing::new(Vec::with_capacity(capacity));
        file.take(MAX_KEY_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut raw)
            .map_err(|e| key_error(path, &format!("cannot read it ({e})")))?;
        if u64::try_from(raw.len()).map_or(true, |n| n > MAX_KEY_FILE_BYTES) {
            return Err(key_error(path, "larger than a controller key file"));
        }
        let text =
            std::str::from_utf8(&raw).map_err(|_| key_error(path, "not a controller key file"))?;
        Self::parse(text).map_err(|why| key_error(path, why))
    }

    fn parse(text: &str) -> Result<Self, &'static str> {
        let (mut kind, mut seed, mut public): (Option<&str>, Option<&str>, Option<&str>) =
            (None, None, None);
        for line in text.lines() {
            let slot = match line.split_once('=') {
                Some(("type", v)) => (&mut kind, v),
                Some(("seed", v)) => (&mut seed, v),
                Some(("public_key", v)) => (&mut public, v),
                _ => return Err("not a controller key file"),
            };
            if slot.0.replace(slot.1).is_some() {
                return Err("a line is repeated");
            }
        }
        if kind != Some("ed25519") {
            return Err("not an Ed25519 controller key");
        }
        let seed = seed.ok_or("it has no seed")?;
        let mut bytes = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(seed, bytes.as_mut_slice())
            .map_err(|_| "its seed is not 64 hex characters")?;
        let signer = Self::from_seed(&bytes);
        let stated = public.ok_or("it states no public key")?;
        if stated != hex::encode(signer.public) {
            return Err("it states a public key that is not its seed's");
        }
        Ok(signer)
    }

    /// The text of a key file for this key.
    pub fn key_file_text(&self) -> Zeroizing<String> {
        let raw = Zeroizing::new(self.key.to_bytes());
        let seed = Zeroizing::new(hex::encode(raw.as_slice()));
        Zeroizing::new(format!(
            "type=ed25519\nseed={}\npublic_key={}\n",
            seed.as_str(),
            hex::encode(self.public)
        ))
    }

    pub fn public_key(&self) -> &[u8; 32] {
        &self.public
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    pub fn controller_id(&self) -> &ControllerId {
        &self.id
    }

    pub fn fingerprint(&self) -> String {
        self.id.fingerprint()
    }

    fn envelope(&self, bytes: Vec<u8>) -> SignedDocument {
        let signature = self.key.sign(&bytes).to_bytes();
        let envelope = ProofEnvelope::new(self.id, &bytes, ControllerProof::Ed25519 { signature });
        SignedDocument { bytes, envelope }
    }

    /// Signs a challenge that passed `check_challenge` and names this key.
    pub fn sign_challenge(
        &self,
        challenge: &CheckedChallenge,
    ) -> Result<SignedDocument, ControllerError> {
        if challenge.challenge().controller_id != self.id {
            return Err(ControllerError::Refusal(
                "refusing to sign: the challenge names another controller".into(),
            ));
        }
        Ok(self.envelope(challenge.bytes().to_vec()))
    }

    /// Signs the canonical encoding of `document`.
    pub fn sign_policy(
        &self,
        document: &PolicyDocument,
    ) -> Result<SignedDocument, ControllerError> {
        let bytes = document.encode().map_err(|e| {
            ControllerError::Refusal(format!("the policy does not encode ({})", e.code_name()))
        })?;
        Ok(self.envelope(bytes))
    }

    /// Signs the canonical encoding of `command`.
    pub fn sign_command(&self, command: &Command) -> Result<SignedDocument, ControllerError> {
        let bytes = command.encode().map_err(|e| {
            ControllerError::Refusal(format!("the command does not encode ({})", e.code_name()))
        })?;
        Ok(self.envelope(bytes))
    }

    /// Signs a read-session request that names this key.
    pub fn sign_read_session(
        &self,
        request: &ReadSessionRequest,
    ) -> Result<SignedDocument, ControllerError> {
        if request.controller_id != self.id {
            return Err(ControllerError::Refusal(format!(
                "refusing to sign: the read session names controller {}, not this key ({})",
                request.controller_id.fingerprint(),
                self.fingerprint()
            )));
        }
        let bytes = request.encode().map_err(|e| {
            ControllerError::Refusal(format!(
                "the read session does not encode ({})",
                e.code_name()
            ))
        })?;
        Ok(self.envelope(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str, text: &str, mode: u32) -> std::path::PathBuf {
        let dir = crate::testkit::temp_dir(tag);
        let path = dir.join(KEY_FILE);
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        path
    }

    #[test]
    fn a_keygen_file_loads_and_states_its_key() {
        let signer = ControllerSigner::from_seed(&[7; 32]);
        let path = temp_file("load", &signer.key_file_text(), 0o600);
        let loaded = ControllerSigner::load(&path).unwrap();
        assert_eq!(loaded.public_key(), signer.public_key());
        assert_eq!(loaded.controller_id(), signer.controller_id());
        let debug = format!("{loaded:?}");
        assert!(
            !debug.contains(&hex::encode([7u8; 32])) && debug.contains(&signer.fingerprint()),
            "{debug}"
        );
    }

    #[test]
    fn the_key_file_refusals() {
        let signer = ControllerSigner::from_seed(&[8; 32]);
        let good = signer.key_file_text().to_string();
        let other = ControllerSigner::from_seed(&[9; 32]);
        let cases: Vec<(String, u32, &str)> = vec![
            (good.clone(), 0o644, "other users"),
            (good.clone(), 0o640, "other users"),
            (format!("{good}{}", " ".repeat(1100)), 0o600, "larger"),
            (format!("{good}type=ed25519\n"), 0o600, "repeated"),
            (
                good.replace("type=ed25519", "type=p256"),
                0o600,
                "not an Ed25519",
            ),
            (
                good.replace(
                    &hex::encode(signer.public_key()),
                    &hex::encode(other.public_key()),
                ),
                0o600,
                "not its seed",
            ),
            (
                "type=ed25519\npublic_key=00\n".to_string(),
                0o600,
                "no seed",
            ),
            (format!("{good}\n"), 0o600, "not a controller key file"),
            ("hello\n".to_string(), 0o600, "not a controller key file"),
        ];
        for (i, (text, mode, expected)) in cases.into_iter().enumerate() {
            if cfg!(not(unix)) && expected == "other users" {
                continue;
            }
            let path = temp_file(&format!("refuse-{i}"), &text, mode);
            let err = ControllerSigner::load(&path).unwrap_err().to_string();
            assert!(err.contains(expected), "case {i}: {err}");
        }
        let missing = crate::testkit::temp_dir("missing").join(KEY_FILE);
        assert!(
            ControllerSigner::load(&missing)
                .unwrap_err()
                .to_string()
                .contains("cannot read")
        );
    }

    #[test]
    fn a_read_session_for_another_controller_is_not_signed() {
        let signer = ControllerSigner::from_seed(&[10; 32]);
        let other = ControllerSigner::from_seed(&[11; 32]);
        let request = crate::testkit::read_session_for(*other.controller_id()).unwrap();
        let err = signer.sign_read_session(&request).unwrap_err().to_string();
        assert!(err.contains("names controller"), "{err}");
        let mine = crate::testkit::read_session_for(*signer.controller_id()).unwrap();
        let signed = signer.sign_read_session(&mine).unwrap();
        assert_eq!(signed.bytes, mine.encode().unwrap());
        agent_control::verify_ed25519_strict(
            signer.public_key(),
            &signed.bytes,
            &signed.signature().unwrap(),
        )
        .unwrap();
    }
}
