//! `cloud-agent controller keygen | sign`: a local Ed25519 controller key, and its
//! signature over a policy, command or read-session document. No agent config or network.

use std::io::Write;
use std::path::{Path, PathBuf};

use agent_control::{
    Command, ControllerType, Network, PolicyDocument, ReadSessionRequest, SCHEME_COMMAND,
    SCHEME_POLICY, SCHEME_READ_SESSION, controller_id_for, describe_rules, widening,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};

use super::prompt::{Prompter, Yes, confirm_typed};

/// Name of the key file inside its directory.
pub const KEY_FILE: &str = "controller.key";
/// Largest document read for signing.
const MAX_DOCUMENT_BYTES: usize = 70_000;

/// The `controller` commands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum ControllerCommands {
    /// Generate an Ed25519 controller key in its own directory (created 0700)
    Keygen {
        /// The directory for the key; it must not hold a key yet
        #[arg(long)]
        dir: PathBuf,
    },
    /// Sign a policy, command or read-session document with a controller key
    Sign {
        /// The key file written by `controller keygen`
        #[arg(long)]
        key: PathBuf,
        /// The document, base64url
        #[arg(long, required_unless_present = "file", conflicts_with = "file")]
        payload: Option<String>,
        /// A file holding the document (raw or base64url)
        #[arg(long)]
        file: Option<PathBuf>,
        /// The network the document must be for: devnet or mainnet
        #[arg(long)]
        network: String,
        /// The policy in force (raw or base64url), to show what a new policy widens
        #[arg(long)]
        current_policy: Option<PathBuf>,
    },
}

/// Runs one `controller` command on the terminal.
pub async fn run_controller(command: ControllerCommands) -> Result<()> {
    let mut out = std::io::stdout();
    run_controller_with(command, &super::prompt::Terminal, &mut out).await
}

/// [`run_controller`] with explicit answers and output.
pub async fn run_controller_with(command: ControllerCommands, prompter: &dyn Prompter, out: &mut dyn Write) -> Result<()> {
    match command {
        ControllerCommands::Keygen { dir } => keygen(&dir, out),
        ControllerCommands::Sign { key, payload, file, network, current_policy } => {
            let network = Network::parse(network.trim()).map_err(|_| anyhow!("--network must be devnet or mainnet"))?;
            let bytes = match (payload, file) {
                (Some(payload), None) => decode_b64url(&payload)?,
                (None, Some(file)) => read_document(&file)?,
                _ => bail!("give either --payload or --file"),
            };
            let current = current_policy.as_deref().map(read_document).transpose()?;
            let key = load_key(&key)?;
            let signature = sign(&key, &bytes, network, current.as_deref(), prompter, out).await?;
            let _ = writeln!(out, "\nEd25519 signature over the full document (hex):");
            let _ = writeln!(out, "{}", hex::encode(signature));
            Ok(())
        }
    }
}

fn decode_b64url(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    URL_SAFE_NO_PAD
        .decode(text)
        .or_else(|_| URL_SAFE.decode(text))
        .map_err(|_| anyhow!("the payload is not base64url"))
}

/// A document file: its raw bytes, or base64url text.
fn read_document(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(bytes.len() <= MAX_DOCUMENT_BYTES, "{} is too large for a control document", path.display());
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
    builder.create(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict {}", dir.display()))?;
    }
    Ok(())
}

/// The public key and fingerprint of a controller seed.
fn describe_key(key: &SigningKey) -> (String, String) {
    let public = key.verifying_key().to_bytes();
    (hex::encode(public), controller_id_for(ControllerType::Ed25519, &public).fingerprint())
}

fn keygen(dir: &Path, out: &mut dyn Write) -> Result<()> {
    let path = dir.join(KEY_FILE);
    ensure!(!path.exists(), "{} already exists: keygen never replaces a key", path.display());
    if let Ok(mut listing) = std::fs::read_dir(dir) {
        ensure!(listing.next().is_none(), "{} is not empty: the key gets a directory of its own", dir.display());
    }
    let mut seed = [0u8; 32];
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut seed).map_err(|e| anyhow!("no randomness: {e}"))?;
    let key = SigningKey::from_bytes(&seed);
    let (public_hex, fingerprint) = describe_key(&key);
    let text = agent_logic::secret::Zeroizing::new(format!(
        "type=ed25519\nseed={}\npublic_key={public_hex}\n",
        hex::encode(seed)
    ));
    seed.fill(0);
    create_private_dir(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).with_context(|| format!("create {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all().with_context(|| format!("sync {}", path.display()))?;
    let _ = writeln!(out, "Key file:    {}", path.display());
    let _ = writeln!(out, "Public key:  {public_hex}");
    let _ = writeln!(out, "Fingerprint: {fingerprint}");
    let _ = writeln!(
        out,
        "Add it to an agent with: cloud-agent control add --ed25519 {public_hex} --label <label> --capabilities <list>"
    );
    Ok(())
}

/// The key of a key file; its stated public key must match the seed.
pub fn load_key(path: &Path) -> Result<SigningKey> {
    let text = agent_logic::secret::Zeroizing::new(
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    );
    let (mut kind, mut seed, mut public) = (None, None, None);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("type", v)) => kind = Some(v.to_string()),
            Some(("seed", v)) => seed = Some(v.to_string()),
            Some(("public_key", v)) => public = Some(v.to_string()),
            _ => bail!("{} is not a controller key file", path.display()),
        }
    }
    ensure!(kind.as_deref() == Some("ed25519"), "{} is not an Ed25519 controller key", path.display());
    let seed = agent_logic::secret::Zeroizing::new(seed.ok_or_else(|| anyhow!("{} has no seed", path.display()))?);
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(seed.trim(), &mut bytes).map_err(|_| anyhow!("{} has an invalid seed", path.display()))?;
    let key = SigningKey::from_bytes(&bytes);
    bytes.fill(0);
    ensure!(
        public.as_deref() == Some(describe_key(&key).0.as_str()),
        "{} states a public key that is not its seed's",
        path.display()
    );
    Ok(key)
}

/// The first-line scheme of a document.
fn scheme_of(bytes: &[u8]) -> Option<&str> {
    let first = bytes.split(|b| *b == b'\n').next()?;
    std::str::from_utf8(first).ok()?.strip_prefix("scheme=")
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
    let (public_hex, fingerprint) = describe_key(key);
    let controller_id = controller_id_for(ControllerType::Ed25519, &key.verifying_key().to_bytes());
    match scheme_of(bytes) {
        Some(SCHEME_POLICY) => {
            let doc = PolicyDocument::decode(bytes).map_err(|e| anyhow!("the policy does not decode ({})", e.code_name()))?;
            ensure!(doc.network == network, "refusing to sign: the policy is for {}, not {network}", doc.network);
            let _ = writeln!(out, "Policy v{} for {} on {}", doc.version, doc.agent_party, doc.network);
            let _ = writeln!(out, "  issued {}, expires {}", doc.issued_at, doc.expires_at);
            if let Some(not_before) = doc.not_before {
                let _ = writeln!(out, "  not before {not_before}");
            }
            for line in describe_rules(&doc.rules) {
                let _ = writeln!(out, "  {line}");
            }
            if let Some(current) = current_policy {
                let old = PolicyDocument::decode(current)
                    .map_err(|e| anyhow!("the current policy does not decode ({})", e.code_name()))?;
                let widened = widening(&old.rules, &doc.rules);
                let mut parts = Vec::new();
                for (on, name) in [(widened.transfers, "transfers"), (widened.trading, "trading"), (widened.other, "other")] {
                    if on {
                        parts.push(name);
                    }
                }
                let _ = writeln!(
                    out,
                    "  against v{}: {}",
                    old.version,
                    if parts.is_empty() { "narrows or keeps every category".to_string() } else { format!("widens {}", parts.join(", ")) }
                );
            }
        }
        Some(SCHEME_COMMAND) => {
            let command = Command::decode(bytes).map_err(|e| anyhow!("the command does not decode ({})", e.code_name()))?;
            ensure!(command.network == network, "refusing to sign: the command is for {}, not {network}", command.network);
            let op = serde_json::to_value(agent_control::json::CommandOpJson::from(&command.op)).unwrap_or_default();
            let _ = writeln!(out, "Command {} for {} on {}", command.command_id, command.agent_party, command.network);
            let _ = writeln!(out, "  operation: {}", command.op.name());
            if let Some(fields) = op.as_object() {
                for (name, value) in fields.iter().filter(|(name, _)| name.as_str() != "name") {
                    let _ = writeln!(out, "    {name}: {}", value.as_str().map_or_else(|| value.to_string(), str::to_string));
                }
            }
            let _ = writeln!(out, "  summary: {}", command.summary.as_str());
            let _ = writeln!(out, "  at log seq {} ({})", command.log_seq, command.log_head);
            let _ = writeln!(out, "  issued {}, expires {}", command.issued_at, command.expires_at);
        }
        Some(SCHEME_READ_SESSION) => {
            let session = ReadSessionRequest::decode(bytes)
                .map_err(|e| anyhow!("the read session does not decode ({})", e.code_name()))?;
            ensure!(session.network == network, "refusing to sign: the read session is for {}, not {network}", session.network);
            ensure!(
                session.controller_id == controller_id,
                "refusing to sign: the read session names controller {}, not this key ({fingerprint})",
                session.controller_id.fingerprint()
            );
            let _ = writeln!(out, "Read session on {} for {} agent(s):", session.network, session.agents.len());
            for agent in &session.agents {
                let _ = writeln!(out, "  {agent}");
            }
            let _ = writeln!(out, "  issued {}, expires {}", session.issued_at, session.expires_at);
        }
        Some(other) => bail!("refusing to sign: a controller key signs only policy, command or read-session documents, not {other}"),
        None => bail!("refusing to sign: not a control document"),
    }
    let _ = writeln!(out, "Signing as {fingerprint} ({public_hex}).");
    confirm_typed(prompter, out, "sign", Yes::Refused).await?;
    Ok(key.sign(bytes).to_bytes())
}
