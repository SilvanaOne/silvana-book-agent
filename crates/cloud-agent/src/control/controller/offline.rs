//! `controller keygen | sign`: a local Ed25519 controller key, and its signature over a
//! policy, command or read-session document. No agent config and no network.

use std::io::Write;
use std::path::Path;

use agent_control::{
    Command, Network, PolicyDocument, ReadSessionRequest, SCHEME_COMMAND, SCHEME_POLICY,
    SCHEME_READ_SESSION, describe_rules, widening,
};
use agent_controller::ControllerSigner;
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use ed25519_dalek::SigningKey;

use super::super::prompt::{Prompter, Yes, confirm_typed};

pub use agent_controller::KEY_FILE;

/// Largest document read for signing.
const MAX_DOCUMENT_BYTES: usize = 70_000;

pub(crate) fn decode_b64url(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .map_err(|_| anyhow!("the payload is not base64url"))
}

/// A document file: its raw bytes, or base64url text.
pub(crate) fn read_document(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        bytes.len() <= MAX_DOCUMENT_BYTES,
        "{} is too large for a control document",
        path.display()
    );
    if bytes.starts_with(b"scheme=") {
        return Ok(bytes);
    }
    decode_b64url(&String::from_utf8_lossy(&bytes))
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {}", dir.display()))?;
    }
    Ok(())
}

pub(crate) fn keygen(dir: &Path, out: &mut dyn Write) -> Result<()> {
    let path = dir.join(KEY_FILE);
    ensure!(
        !path.exists(),
        "{} already exists: keygen never replaces a key",
        path.display()
    );
    if let Ok(mut listing) = std::fs::read_dir(dir) {
        ensure!(
            listing.next().is_none(),
            "{} is not empty: the key gets a directory of its own",
            dir.display()
        );
    }
    let mut seed = agent_logic::secret::Zeroizing::new([0u8; 32]);
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, seed.as_mut_slice())
        .map_err(|e| anyhow!("no randomness: {e}"))?;
    let signer = ControllerSigner::from_seed(&seed);
    let text = signer.key_file_text();
    create_private_dir(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    let public_hex = hex::encode(signer.public_key());
    let _ = writeln!(out, "Key file:    {}", path.display());
    let _ = writeln!(out, "Public key:  {public_hex}");
    let _ = writeln!(out, "Fingerprint: {}", signer.fingerprint());
    let _ = writeln!(
        out,
        "Add it to an agent with: cloud-agent control add --ed25519 {public_hex} --label <label> --capabilities <list>"
    );
    Ok(())
}

/// The key of a key file: at most 1 KiB, readable by its owner only, each line once.
pub fn load_key(path: &Path) -> Result<ControllerSigner> {
    ControllerSigner::load(path).map_err(|e| anyhow!("{e}"))
}

/// The first-line scheme of a document.
fn scheme_of(bytes: &[u8]) -> Option<&str> {
    let first = bytes.split(|b| *b == b'\n').next()?;
    std::str::from_utf8(first).ok()?.strip_prefix("scheme=")
}

/// A decoded document the offline `sign` signs.
enum Offline {
    Policy(Box<PolicyDocument>),
    Command(Command),
    ReadSession(ReadSessionRequest),
}

/// Shows the decoded document, asks for a typed `sign`, then signs its full bytes.
pub async fn sign(
    key: &SigningKey,
    bytes: &[u8],
    network: Network,
    current_policy: Option<&[u8]>,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<[u8; 64]> {
    let signer = ControllerSigner::from_signing_key(key.clone());
    sign_with(&signer, bytes, network, current_policy, prompter, out).await
}

/// [`sign`] with a loaded key.
pub(crate) async fn sign_with(
    signer: &ControllerSigner,
    bytes: &[u8],
    network: Network,
    current_policy: Option<&[u8]>,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<[u8; 64]> {
    let public_hex = hex::encode(signer.public_key());
    let fingerprint = signer.fingerprint();
    let document = match scheme_of(bytes) {
        Some(SCHEME_POLICY) => {
            let doc = PolicyDocument::decode(bytes)
                .map_err(|e| anyhow!("the policy does not decode ({})", e.code_name()))?;
            ensure!(
                doc.network == network,
                "refusing to sign: the policy is for {}, not {network}",
                doc.network
            );
            let _ = writeln!(
                out,
                "Policy v{} for {} on {}",
                doc.version, doc.agent_party, doc.network
            );
            let _ = writeln!(
                out,
                "  issued {}, expires {}",
                doc.issued_at, doc.expires_at
            );
            if let Some(not_before) = doc.not_before {
                let _ = writeln!(out, "  not before {not_before}");
            }
            for line in describe_rules(&doc.rules) {
                let _ = writeln!(out, "  {line}");
            }
            if let Some(current) = current_policy {
                let old = PolicyDocument::decode(current).map_err(|e| {
                    anyhow!("the current policy does not decode ({})", e.code_name())
                })?;
                let widened = widening(&old.rules, &doc.rules);
                let _ = writeln!(
                    out,
                    "  against v{}: {}",
                    old.version,
                    super::super::policy_accept::widening_text(&widened)
                );
                for line in super::super::policy_accept::package_diff(&old.rules, &doc.rules) {
                    let _ = writeln!(out, "  {line}");
                }
            }
            Offline::Policy(Box::new(doc))
        }
        Some(SCHEME_COMMAND) => {
            let command = Command::decode(bytes)
                .map_err(|e| anyhow!("the command does not decode ({})", e.code_name()))?;
            ensure!(
                command.network == network,
                "refusing to sign: the command is for {}, not {network}",
                command.network
            );
            let _ = writeln!(
                out,
                "Command {} for {} on {}",
                command.command_id, command.agent_party, command.network
            );
            super::render::write_command_op(out, &command.op, "  ");
            let _ = writeln!(out, "  summary: {}", command.summary.as_str());
            let _ = writeln!(
                out,
                "  at log seq {} ({})",
                command.log_seq, command.log_head
            );
            let _ = writeln!(
                out,
                "  issued {}, expires {}",
                command.issued_at, command.expires_at
            );
            Offline::Command(command)
        }
        Some(SCHEME_READ_SESSION) => {
            let session = ReadSessionRequest::decode(bytes)
                .map_err(|e| anyhow!("the read session does not decode ({})", e.code_name()))?;
            ensure!(
                session.network == network,
                "refusing to sign: the read session is for {}, not {network}",
                session.network
            );
            ensure!(
                session.controller_id == *signer.controller_id(),
                "refusing to sign: the read session names controller {}, not this key ({fingerprint})",
                session.controller_id.fingerprint()
            );
            let _ = writeln!(
                out,
                "Read session on {} for {} agent(s):",
                session.network,
                session.agents.len()
            );
            for agent in &session.agents {
                let _ = writeln!(out, "  {agent}");
            }
            let _ = writeln!(
                out,
                "  issued {}, expires {}",
                session.issued_at, session.expires_at
            );
            Offline::ReadSession(session)
        }
        Some(other) => bail!(
            "refusing to sign: a controller key signs only policy, command or read-session documents, not {other}"
        ),
        None => bail!("refusing to sign: not a control document"),
    };
    let _ = writeln!(out, "Signing as {fingerprint} ({public_hex}).");
    confirm_typed(prompter, out, "sign", Yes::Refused).await?;
    let signed = match &document {
        Offline::Policy(doc) => signer.sign_policy(doc),
        Offline::Command(command) => signer.sign_command(command),
        Offline::ReadSession(session) => signer.sign_read_session(session),
    }
    .map_err(|e| anyhow!("{e}"))?;
    ensure!(
        signed.bytes == bytes,
        "refusing to sign: the document is not in canonical form"
    );
    signed
        .signature()
        .ok_or_else(|| anyhow!("the key produced no Ed25519 signature"))
}
