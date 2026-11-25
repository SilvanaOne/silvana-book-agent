//! Agent control: startup attach with the linked-marker rules, the bypass refusals of
//! a linked agent, the control commands, the marker in the env file, and the content check.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

pub mod change;
pub mod commands;
pub mod controller;
pub mod intent;
pub mod link;
pub mod pins;
pub mod policy_accept;
pub mod poller;
pub mod prompt;
pub mod rpc;
pub mod show;
pub mod sync;
#[cfg(test)]
pub(crate) mod test_support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use agent_control::{
    ForkEvidence, HeadRef, LogComparison, MAX_LOG_ENTRIES, Network, PartyId, SignedEntry,
    VerifiedLog, compare_logs, verify_ed25519_strict, verify_log, verify_party_key,
};
use agent_logic::config::BaseConfig;
use agent_logic::control_store::{
    ChainNetwork, ControlEnv, ControlLock, ControlMarker, ControlStore, MARKER_ENV,
};
use agent_logic::signer::{AgentSigner, ControlGuard};
use anyhow::{Context, Result, anyhow, bail, ensure};
use tracing::{error, info, warn};

/// The offline controller commands' earlier module path.
pub use controller as controller_tool;
pub use controller::{ControllerCommands, ControllerOptions, run_controller};
pub use intent::{ContentRefusal, OwnRequest, PlatformParties};
pub use rpc::{ControlRpc, RemoteLog, RpcError};

/// Longest wait for the remote log at startup, connection included.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What started the attach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The agent daemon.
    Agent,
    /// A `control` command, which compares logs itself.
    Control,
    /// The `sign` command.
    Sign,
    /// Any other command.
    Other,
}

/// The options of [`attach`].
#[derive(Clone, Copy, Debug)]
pub struct AttachOptions {
    pub purpose: Purpose,
    pub force: bool,
    pub no_reject: bool,
}

/// The platform parties the content check uses, once attach installed them.
static PLATFORM: RwLock<Option<PlatformParties>> = RwLock::new(None);

/// Installs the locally pinned platform parties of this process.
pub fn install_platform_parties(parties: PlatformParties) {
    *agent_logic::sync::write(&PLATFORM) = Some(parties);
}

/// The agent.toml settings every signature of this process applies: `[verify]` and
/// `[vetted_packages]`; the defaults until attach installs them.
#[derive(Clone, Default)]
pub struct SigningSettings {
    pub verify: tx_verifier::VerifySetting,
    pub vetted_packages: agent_logic::config::VettedPackages,
    /// Where a refusal's package name is looked up; only on a control network.
    pub catalog: Option<Arc<dyn crate::control_survey::PackageCatalog>>,
}

impl std::fmt::Debug for SigningSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningSettings")
            .field("verify", &self.verify)
            .field("vetted_packages", &self.vetted_packages)
            .field("catalog", &self.catalog.is_some())
            .finish()
    }
}

impl SigningSettings {
    pub fn from_config(config: &BaseConfig) -> Self {
        use agent_logic::config::VerifyModeSetting as M;
        let verify = match config.verify.mode {
            M::Default => tx_verifier::VerifySetting::Default,
            M::Observe => tx_verifier::VerifySetting::Observe,
            M::Enforce => tx_verifier::VerifySetting::Enforce,
        };
        let catalog = crate::control_survey::RpcCatalog::for_config(config)
            .map(|catalog| Arc::new(catalog) as Arc<dyn crate::control_survey::PackageCatalog>);
        Self { verify, vetted_packages: config.vetted_packages.clone(), catalog }
    }
}

static SIGNING: RwLock<Option<Arc<SigningSettings>>> = RwLock::new(None);

/// Installs the signing settings of this process; clients made later carry them.
pub fn install_signing_settings(settings: SigningSettings) {
    *agent_logic::sync::write(&SIGNING) = Some(Arc::new(settings));
}

/// The installed signing settings, else the defaults.
pub fn signing_settings() -> Arc<SigningSettings> {
    agent_logic::sync::read(&SIGNING).clone().unwrap_or_default()
}

/// The installed platform parties, else `PARTY_FEE` and `PARTY_PREPAID_TRAFFIC` from the env.
pub fn platform_parties() -> Result<PlatformParties> {
    if let Some(parties) = agent_logic::sync::read(&PLATFORM).clone() {
        return Ok(parties);
    }
    Ok(PlatformParties {
        fee: agent_logic::config::pinned_party_env("PARTY_FEE")?,
        prepaid_traffic: agent_logic::config::pinned_party_env("PARTY_PREPAID_TRAFFIC")?,
    })
}

/// The control network of `CANTON_CHAIN`, if it is one.
pub fn network_of(config: &BaseConfig) -> ChainNetwork {
    agent_logic::control_store::chain_network(config.canton_chain.as_deref())
}

/// Raises the marker `AGENT_CONTROL_HEAD=<network>:<seq>:<hash>` in the env file, under
/// control.lock; a higher seq is kept, another network or hash at the same seq refused.
pub fn record_marker(env_file: &Path, network: Network, head: HeadRef) -> Result<()> {
    let marker = ControlMarker { network, head };
    let current = crate::read_env_value(env_file, MARKER_ENV).filter(|text| !text.trim().is_empty());
    if let Some(text) = current {
        let current = ControlMarker::parse(&text)
            .with_context(|| format!("refusing to replace {MARKER_ENV} in {}", env_file.display()))?;
        ensure!(
            current.network == network,
            "refusing to replace {MARKER_ENV}={current} in {}: it names another network than {network}",
            env_file.display()
        );
        if current.head.seq > head.seq {
            return Ok(());
        }
        if current.head.seq == head.seq {
            ensure!(
                current.head.hash == head.hash,
                "refusing to replace {MARKER_ENV}={current} in {}: the control log has another entry at seq {}",
                env_file.display(),
                head.seq
            );
            return Ok(());
        }
    }
    crate::upsert_env_value(env_file, MARKER_ENV, &marker.to_string())
        .with_context(|| format!("could not record {MARKER_ENV} in {}", env_file.display()))
}

/// Appends the pending entry the rpc accepted as `accepted`, records the marker and
/// moves the guard forward.
pub async fn accept_and_mark(
    store: &ControlStore,
    accepted: &HeadRef,
    guard: &ControlGuard,
    env_file: &Path,
) -> Result<HeadRef> {
    let lock = store.lock().await?;
    accept_and_mark_locked(store, lock, accepted, guard, env_file).await
}

/// [`accept_and_mark`] under a lock the caller already holds: the marker is written under
/// it, and it is released before the reload.
pub async fn accept_and_mark_locked(
    store: &ControlStore,
    lock: ControlLock,
    accepted: &HeadRef,
    guard: &ControlGuard,
    env_file: &Path,
) -> Result<HeadRef> {
    let head = store.accept_pending(&lock, accepted)?;
    let marked = record_marker(env_file, store.network(), head);
    drop(lock);
    marked?;
    guard.reload_from(store, None).await?;
    Ok(head)
}

/// Why the remote log could not be read.
#[derive(Debug)]
pub enum ProbeError {
    /// A transport error, a timeout or Unimplemented: the rpc is unreachable.
    Unreachable(String),
    /// Any other answer from the rpc.
    Refused(String),
}

impl From<RpcError> for ProbeError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Unreachable(text) | RpcError::Unimplemented(text) => Self::Unreachable(text),
            RpcError::Refused { status, .. } => Self::Refused(status),
        }
    }
}

/// Where attach reads the remote log from.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait LogSource: Send + Sync {
    async fn fetch(&self, config: &BaseConfig, signer: &AgentSigner) -> Result<RemoteLog, ProbeError>;
}

/// The rpc's AgentControlAgentService.
pub struct RpcLogSource;

#[async_trait::async_trait]
impl LogSource for RpcLogSource {
    async fn fetch(&self, config: &BaseConfig, _signer: &AgentSigner) -> Result<RemoteLog, ProbeError> {
        let probe = async {
            let rpc = ControlRpc::connect(config).await?.with_timeout(PROBE_TIMEOUT);
            rpc.fetch_log().await
        };
        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Ok(result) => result.map_err(ProbeError::from),
            Err(_) => Err(ProbeError::Unreachable(format!("no answer within {PROBE_TIMEOUT:?}"))),
        }
    }
}

/// The rpc's log as this agent reads it: the longest prefix that decodes, counts up
/// from 1 and verifies, and the first seq past it that the rpc stores or reports.
#[derive(Clone, Debug, Default)]
pub(crate) struct RemoteView {
    pub prefix: Option<VerifiedLog>,
    /// Some when the rpc holds rows (or a head) past the prefix: a gap, an edit, a hidden tail.
    pub broken_at: Option<u64>,
}

impl RemoteView {
    pub fn head(&self) -> Option<HeadRef> {
        self.prefix.as_ref().map(VerifiedLog::head)
    }

    fn len(&self) -> u64 {
        self.head().map_or(0, |head| head.seq)
    }
}

/// The longest prefix of `decoded` that verifies, as the rpc's own planner reads it.
fn longest_valid(network: Network, party: &PartyId, mut decoded: Vec<SignedEntry>) -> Option<VerifiedLog> {
    if decoded.is_empty() {
        return None;
    }
    match verify_log(network, party, &decoded) {
        Ok(log) => Some(log),
        Err(e) => {
            let keep = usize::try_from(e.seq.saturating_sub(1)).unwrap_or(0).min(decoded.len());
            decoded.truncate(keep);
            if decoded.is_empty() {
                return None;
            }
            verify_log(network, party, &decoded).ok()
        }
    }
}

/// Reads the remote log for this agent; rows that fail count as missing, not as an error.
/// A verifying prefix under another key is refused.
pub(crate) fn verify_remote(
    network: Network,
    party: &PartyId,
    public_key: &[u8; 32],
    remote: &RemoteLog,
) -> Result<RemoteView> {
    let mut decoded = Vec::new();
    for (bytes, signature) in remote.entries.iter().take(MAX_LOG_ENTRIES) {
        match SignedEntry::new(bytes.clone(), *signature) {
            Ok(entry) => decoded.push(entry),
            Err(_) => break,
        }
    }
    let prefix = longest_valid(network, party, decoded);
    if let Some(log) = &prefix {
        ensure!(log.agent_key() == public_key, "it is signed by another key");
    }
    let view = RemoteView { prefix, broken_at: None };
    let broken_at = (remote.tip() > view.len()).then(|| view.len().saturating_add(1));
    Ok(RemoteView { broken_at, ..view })
}

/// How the rpc's copy stands against the local log.
#[derive(Debug)]
pub(crate) enum Standing {
    Equal,
    /// The rpc lacks these local seqs, or holds copies of them that do not verify; ascending.
    Behind { bad: Vec<u64> },
    /// The rpc stores or reports rows past the local head; `signed` when one is validly signed.
    Ahead { local: HeadRef, remote_tip: u64, signed: bool },
    /// A validly signed entry of the rpc differs from the local one.
    Fork(Box<ForkEvidence>),
}

/// `row` is this key's valid signature over an entry of the local log's network and party.
fn signed_for(local: &VerifiedLog, row: &SignedEntry) -> bool {
    let entry = row.entry();
    entry.network() == local.network()
        && entry.agent_party() == local.agent_party()
        && entry.agent_key() == local.agent_key()
        && verify_ed25519_strict(local.agent_key(), row.bytes(), row.signature()).is_ok()
}

/// Compares the rpc's rows with the local log: the verified prefix entry by entry, then
/// every row past it. Only a validly signed divergent row is a fork.
pub(crate) fn standing(local: &VerifiedLog, remote: &RemoteLog, view: &RemoteView) -> Result<Standing> {
    if let Some(prefix) = &view.prefix {
        match compare_logs(local, prefix).map_err(|e| anyhow!("the logs cannot be compared: {}", e.code_name()))? {
            LogComparison::Fork(evidence) => return Ok(Standing::Fork(evidence)),
            LogComparison::RemoteAhead { local, remote } => {
                return Ok(Standing::Ahead { local, remote_tip: remote.seq, signed: true });
            }
            LogComparison::Equal | LogComparison::RemoteBehind { .. } => {}
        }
    }
    let local_head = local.head();
    let prefix_len = view.len();
    let mut good: Vec<bool> = local.entries().iter().map(|entry| entry.seq() <= prefix_len).collect();
    let mut signed_beyond = false;
    for (seq, bytes, signature) in remote.rows().filter(|(seq, ..)| *seq > prefix_len) {
        let Ok(row) = SignedEntry::new(bytes.to_vec(), *signature) else {
            continue;
        };
        if row.seq() != seq {
            continue;
        }
        match local.entry(seq) {
            Some(mine) if mine.hash() == row.hash() => {
                let valid = mine.signature() == row.signature() || signed_for(local, &row);
                let slot = usize::try_from(seq.saturating_sub(1)).ok().and_then(|i| good.get_mut(i));
                if let (true, Some(slot)) = (valid, slot) {
                    *slot = true;
                }
            }
            Some(mine) => {
                let evidence = ForkEvidence::new(mine.clone(), row);
                if evidence.verify(local.network(), local.agent_party()).is_ok() {
                    return Ok(Standing::Fork(Box::new(evidence)));
                }
            }
            None => signed_beyond |= signed_for(local, &row),
        }
    }
    let remote_tip = remote.tip();
    if signed_beyond || remote_tip > local_head.seq {
        return Ok(Standing::Ahead { local: local_head, remote_tip, signed: signed_beyond });
    }
    let bad: Vec<u64> = local
        .entries()
        .iter()
        .zip(&good)
        .filter(|(_, good)| !**good)
        .map(|(entry, _)| entry.seq())
        .collect();
    Ok(if bad.is_empty() { Standing::Equal } else { Standing::Behind { bad } })
}

/// The control settings of `config` over the process ones.
fn control_env(config: &BaseConfig, process: ControlEnv) -> ControlEnv {
    ControlEnv {
        chain: config.canton_chain.clone().or(process.chain.clone()),
        party: Some(config.party_id.clone()),
        ..process
    }
}

/// Startup: loads the control state, compares it with the rpc's log and installs the
/// guard. A linked agent refuses `--force`, `--no-reject` and `sign`.
pub async fn attach(config: &BaseConfig, opts: AttachOptions) -> Result<Arc<ControlGuard>> {
    let env = control_env(config, ControlEnv::from_process());
    attach_with(config, opts, &env, &RpcLogSource).await
}

/// [`attach`] with explicit control settings and log source.
pub async fn attach_with(
    config: &BaseConfig,
    opts: AttachOptions,
    env: &ControlEnv,
    source: &dyn LogSource,
) -> Result<Arc<ControlGuard>> {
    install_platform_parties(PlatformParties::from_config(config));
    install_signing_settings(SigningSettings::from_config(config));
    let signer = agent_logic::signer::resolve_with(&config.private_key, env)
        .context("refusing to start")?;
    let guard = Arc::clone(signer.guard());
    let public_key = guard.public_key();
    let marker = if env.marker_covers(&public_key) {
        env.marker().context("refusing to start")?
    } else {
        None
    };
    if let Some(marker) = &marker {
        let view = guard.view().ok_or_else(|| {
            anyhow!("refusing to start: {MARKER_ENV}={marker} is set but this agent is not linked")
        })?;
        marker.check(view.log()).context("refusing to start")?;
    }
    if guard.is_linked() {
        guard.reload().await.context("refusing to start")?;
        if let Some(party) = guard.party() {
            ensure!(
                party.as_str() == config.party_id,
                "refusing to start: the control log is for {party}, not {}",
                config.party_id
            );
        }
    }

    let compare = match env.network() {
        ChainNetwork::Mapped(network) if opts.purpose != Purpose::Control => Some(network),
        _ => None,
    };
    if let Some(network) = compare {
        compare_with_rpc(config, &signer, &guard, network, marker.as_ref(), source).await?;
    }

    if guard.is_linked() {
        ensure!(!opts.force, "--force is refused for a linked agent");
        ensure!(
            !(opts.no_reject && opts.purpose == Purpose::Agent),
            "--no-reject is refused for a linked agent"
        );
        ensure!(opts.purpose != Purpose::Sign, "the sign command is refused for a linked agent");
        if let Some(head) = guard.head() {
            info!("agent control: linked at seq {} ({})", head.seq, head.hash);
        }
        if let Some(text) = pins::unset_warning(config).filter(|_| opts.purpose == Purpose::Agent) {
            warn!("agent control: {text}");
        }
    }
    Ok(guard)
}

/// The remote log against the local state; with the marker any failure stops startup.
/// Returns the notice logged for a remote copy that `control sync` brings in line.
async fn compare_with_rpc(
    config: &BaseConfig,
    signer: &AgentSigner,
    guard: &ControlGuard,
    network: Network,
    marker: Option<&ControlMarker>,
    source: &dyn LogSource,
) -> Result<Option<String>> {
    let continuing = if guard.is_linked() { "continuing with the local control log" } else { "continuing unlinked" };
    let remote = match source.fetch(config, signer).await {
        Ok(remote) => remote,
        Err(ProbeError::Unreachable(reason)) => {
            if let Some(marker) = marker {
                bail!(
                    "refusing to start: this agent is linked ({MARKER_ENV}={marker}) and the rpc's \
                     control log cannot be read: {reason}"
                );
            }
            warn!("agent control: the rpc's control log cannot be read ({reason}); {continuing}");
            return Ok(None);
        }
        Err(ProbeError::Refused(reason)) => {
            if let Some(marker) = marker {
                bail!(
                    "refusing to start: this agent is linked ({MARKER_ENV}={marker}) and the rpc \
                     refused the control log query: {reason}"
                );
            }
            warn!("agent control: the rpc refused the control log query ({reason}); {continuing}");
            return Ok(None);
        }
    };
    let party = match PartyId::parse(&config.party_id) {
        Ok(party) => party,
        Err(_) if marker.is_none() => {
            if !remote.is_empty() {
                warn!("agent control: {} is not a party id control accepts; {continuing}", config.party_id);
            }
            return Ok(None);
        }
        Err(_) => bail!("refusing to start: {} is not a party id control accepts", config.party_id),
    };
    let view = match verify_remote(network, &party, &guard.public_key(), &remote) {
        Ok(view) => view,
        Err(e) if marker.is_none() => {
            warn!("agent control: the rpc's control log for this agent does not verify ({e:#}); {continuing}");
            return Ok(None);
        }
        Err(e) => bail!("refusing to start: the rpc's control log for this agent is invalid: {e:#}"),
    };
    let Some(local) = guard.view() else {
        if let Some(remote) = view.head() {
            bail!(
                "refusing to start: the rpc holds a control log for this agent (seq {}) but there \
                 is no local copy: run `cloud-agent control restore`",
                remote.seq
            );
        }
        if view.broken_at.is_some() {
            warn!("agent control: the rpc holds control rows for this agent that do not verify; {continuing}");
        }
        return Ok(None);
    };
    let notice = |text: String| -> Result<Option<String>> {
        error!("agent control: {text}");
        Ok(Some(text))
    };
    if remote.is_empty() {
        return notice(format!(
            "the rpc holds no control log for this linked agent (local seq {}): run `cloud-agent control sync`",
            local.head().seq
        ));
    }
    match standing(local.log(), &remote, &view).map_err(|e| anyhow!("refusing to start: {e:#}"))? {
        Standing::Equal => Ok(None),
        Standing::Behind { bad } if view.broken_at.is_some() => notice(format!(
            "the rpc's control log for this agent is broken at seq {} ({} entries missing or not \
             verifying); the local copy is intact: run `cloud-agent control sync` to repair it",
            bad.first().copied().unwrap_or(1),
            bad.len()
        )),
        Standing::Behind { .. } => notice(format!(
            "the rpc's control log (seq {}) is behind the local one (seq {}): run `cloud-agent control sync`",
            view.len(),
            local.head().seq
        )),
        Standing::Ahead { local, remote_tip, signed: true } => bail!(
            "refusing to start: the rpc's control log (seq {remote_tip}) is ahead of the local copy (seq {}): \
             run `cloud-agent control restore`",
            local.seq
        ),
        Standing::Ahead { local, remote_tip, signed: false } => bail!(
            "refusing to start: the rpc reports control rows up to seq {remote_tip}, past the local copy \
             (seq {}), that this key did not sign; check the rpc's log",
            local.seq
        ),
        Standing::Fork(evidence) => {
            error!("agent control fork at seq {}", evidence.seq());
            bail!(
                "refusing to start: the rpc's control log diverges from the local one at seq {}",
                evidence.seq()
            )
        }
    }
}

/// What every control command works with.
pub struct ControlContext {
    pub config: BaseConfig,
    pub network: Network,
    pub party: PartyId,
    pub store: ControlStore,
    pub signer: AgentSigner,
    pub guard: Arc<ControlGuard>,
    /// The marker as the process started with it.
    pub marker: Option<ControlMarker>,
    /// The agent's env file, where the marker is written.
    pub env_file: Option<PathBuf>,
    /// The pins this context uses: the startup ones, filled in by [`Self::pick_up_pins`].
    platform: RwLock<PlatformParties>,
}

impl ControlContext {
    /// The platform parties this context uses: the startup pins, and any picked up since.
    pub fn platform(&self) -> PlatformParties {
        agent_logic::sync::read(&self.platform).clone()
    }

    /// Fills each unset pin from the env file, here and for the content check; a set pin is
    /// never replaced. With `linked_now` a pin still unset is warned about.
    pub fn pick_up_pins(&self, linked_now: bool) {
        let current = self.platform();
        let next = match &self.env_file {
            Some(file) => pins::fill_unset(file, &current, linked_now),
            None => current.clone(),
        };
        if next != current {
            *agent_logic::sync::write(&self.platform) = next.clone();
            install_platform_parties(next.clone());
        }
        if let Some(text) = pins::unset_warning_of(&next).filter(|_| linked_now) {
            warn!("agent control: {text}");
        }
    }

    fn parts(config: &BaseConfig, env: &ControlEnv) -> Result<(Network, PartyId, ControlStore)> {
        let network = match env.network() {
            ChainNetwork::Mapped(network) => network,
            ChainNetwork::Unset => bail!("control needs CANTON_CHAIN=devnet or CANTON_CHAIN=mainnet"),
            ChainNetwork::Unmapped(chain) => bail!("CANTON_CHAIN={chain} is not a control network (devnet or mainnet)"),
        };
        let party = PartyId::parse(&config.party_id)
            .map_err(|_| anyhow!("PARTY_AGENT={} is not a party id control accepts", config.party_id))?;
        let store = ControlStore::locate(env, network, &party)?;
        Ok((network, party, store))
    }

    fn assemble(
        config: BaseConfig,
        env: &ControlEnv,
        env_file: Option<PathBuf>,
        guard: Arc<ControlGuard>,
    ) -> Result<Self> {
        let (network, party, store) = Self::parts(&config, env)?;
        let signer = agent_logic::signer::resolve_with(&config.private_key, env)?;
        ensure!(Arc::ptr_eq(signer.guard(), &guard), "a different control guard is installed for this key");
        ensure!(
            verify_party_key(&party, &signer.public_key()),
            "PARTY_AGENT={party} does not belong to this agent's key"
        );
        let marker = if env.marker_covers(&signer.public_key()) { env.marker()? } else { None };
        let platform = RwLock::new(PlatformParties::from_config(&config));
        Ok(Self { config, network, party, store, signer, guard, marker, env_file, platform })
    }

    /// The context of every control command but `restore`: the attach rules apply.
    pub async fn open(config: BaseConfig, env: &ControlEnv, env_file: Option<PathBuf>) -> Result<Self> {
        let env = control_env(&config, env.clone());
        Self::parts(&config, &env)?;
        let opts = AttachOptions { purpose: Purpose::Control, force: false, no_reject: false };
        let guard = attach_with(&config, opts, &env, &RpcLogSource).await?;
        Self::assemble(config, &env, env_file, guard)
    }

    /// The daemon's context over the guard attach installed; `None` when CANTON_CHAIN is
    /// not a control network or the party is not one control accepts.
    pub fn for_daemon(config: BaseConfig, guard: Arc<ControlGuard>, env_file: Option<PathBuf>) -> Result<Option<Self>> {
        let env = control_env(&config, ControlEnv::from_process());
        if !matches!(env.network(), ChainNetwork::Mapped(_)) || PartyId::parse(&config.party_id).is_err() {
            return Ok(None);
        }
        Self::assemble(config, &env, env_file, guard).map(Some)
    }

    /// The context of `restore`: the marker is checked against the rpc's log, not the local store.
    /// A store an earlier version kept is moved without the marker rule: restore checks it.
    pub fn open_for_restore(config: BaseConfig, env: &ControlEnv, env_file: Option<PathBuf>) -> Result<Self> {
        let env = control_env(&config, env.clone());
        let unmarked = ControlEnv { marker: None, ..env.clone() };
        Self::parts(&config, &unmarked)?;
        let signer = agent_logic::signer::resolve_with(&config.private_key, &unmarked)
            .context("the local control store cannot be loaded; move it aside, then restore")?;
        let guard = Arc::clone(signer.guard());
        let mut context = Self::assemble(config, &unmarked, env_file, guard)?;
        context.marker = if env.marker_covers(&context.signer.public_key()) { env.marker()? } else { None };
        if let Some(marker) = &context.marker {
            ensure!(marker.network == context.network, "{MARKER_ENV} names {} but CANTON_CHAIN is {}", marker.network, context.network);
        }
        Ok(context)
    }

    /// The env file the marker goes to; signing needs it.
    pub fn require_env_file(&self) -> Result<&Path> {
        self.env_file.as_deref().ok_or_else(|| {
            anyhow!("no env file to record {MARKER_ENV} in: pass --env-file with the agent's .env before signing")
        })
    }

    /// Records `head` as the marker, or says what to set when no env file is known.
    pub fn mark<W: Write + ?Sized>(&self, head: HeadRef, out: &mut W) -> Result<()> {
        match &self.env_file {
            Some(file) => record_marker(file, self.network, head),
            None => {
                let marker = ControlMarker { network: self.network, head };
                let _ = writeln!(out, "Set {MARKER_ENV}={marker} in the agent's env file.");
                Ok(())
            }
        }
    }
}

/// The `control` commands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum ControlCommands {
    /// Link this agent to its owner's first controllers, from a link code (LNK-…)
    Link {
        /// The code from the Agent page
        #[arg(long)]
        code: String,
        /// Do not ask for the typed confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Add one controller: an add code (LNK-…) or a local Ed25519 key
    Add {
        /// The add code from the Agent page
        #[arg(long, required_unless_present = "ed25519", conflicts_with_all = ["ed25519", "label", "capabilities"])]
        code: Option<String>,
        /// An Ed25519 public key (64 hex characters or base58)
        #[arg(long, requires_all = ["label", "capabilities"])]
        ed25519: Option<String>,
        /// Label of the Ed25519 controller
        #[arg(long)]
        label: Option<String>,
        /// Capabilities of the Ed25519 controller, e.g. policy,direct,read
        #[arg(long)]
        capabilities: Option<String>,
        /// Do not ask for the typed confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Remove a controller: its capabilities end 1 h (devnet) or 24 h (mainnet) after the signature
    Remove {
        /// A unique id prefix (at least 8 hex characters) or the exact label
        controller: String,
        /// Allow leaving a capability without any controller (typed freeze; no --yes)
        #[arg(long)]
        force: bool,
        /// Do not ask for the typed confirmation (not with --force)
        #[arg(long, conflicts_with = "force")]
        yes: bool,
    },
    /// Set a controller's capabilities, e.g. policy,read
    Capabilities {
        /// A unique id prefix (at least 8 hex characters) or the exact label
        controller: String,
        /// The new capabilities
        capabilities: String,
        /// Allow leaving a capability without any controller (typed freeze; no --yes)
        #[arg(long)]
        force: bool,
        /// Do not ask for the typed confirmation (not with --force)
        #[arg(long, conflicts_with = "force")]
        yes: bool,
    },
    /// Show controllers, pending changes, the policy, the log head and the marker
    Show {
        #[arg(long)]
        json: bool,
        /// Use the local copy only
        #[arg(long)]
        offline: bool,
    },
    /// Policy commands
    Policy {
        #[command(subcommand)]
        command: PolicyCommands,
    },
    /// Execute a pending direct command after the typed confirmation
    Execute {
        /// The code from the Agent page: CMD-<command id>
        #[arg(long)]
        code: String,
    },
    /// List the direct commands waiting for this agent
    Commands,
    /// Rebuild the local control log from the rpc's verified copy
    Restore,
    /// Compare with the rpc's log, upload what it lacks and settle a pending entry
    Sync,
    /// Release a signed entry the rpc refused or never received
    DiscardPending,
}

/// `control policy` commands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum PolicyCommands {
    /// Show the policy in force and any scheduled one
    Show {
        /// Also list the policies waiting for the agent, as it would judge them
        #[arg(long)]
        pending: bool,
        #[arg(long)]
        json: bool,
    },
    /// Accept a pending policy after a typed confirmation; an invalid one is refused
    Accept {
        /// The policy's hash (64 hex characters); without it, the only pending policy
        #[arg(long)]
        hash: Option<String>,
        /// Do not ask for the typed confirmation (refused when the policy widens anything)
        #[arg(long)]
        yes: bool,
    },
}

/// Where a control command runs.
#[derive(Clone, Debug, Default)]
pub struct ControlOptions {
    /// Overrides SILVANA_CONTROL_DIR: the folder that holds `control/` instead of the agent folder.
    pub control_dir: Option<PathBuf>,
    /// The agent's env file, for the marker.
    pub env_file: Option<PathBuf>,
    /// `--dry-run`: nothing a controller directs is executed.
    pub dry_run: bool,
}

/// What `--dry-run` refuses outright: executing a direct command, before anything is
/// recorded or signed.
pub fn refuse_dry_run(options: &ControlOptions, command: &ControlCommands) -> Result<()> {
    ensure!(
        !(options.dry_run && matches!(command, ControlCommands::Execute { .. })),
        "--dry-run cannot execute a direct command: nothing was recorded or signed; run it without --dry-run"
    );
    Ok(())
}

/// Runs one `control` command on the terminal.
pub async fn run_control(config: BaseConfig, options: ControlOptions, command: ControlCommands) -> Result<()> {
    refuse_dry_run(&options, &command)?;
    let mut env = ControlEnv::from_process();
    if let Some(dir) = &options.control_dir {
        env.control_dir = Some(dir.clone().into_os_string());
    }
    let mut out = std::io::stdout();
    run_control_with(config, &env, options.env_file, command, &prompt::Terminal, &mut out).await
}

async fn connect(config: &BaseConfig) -> Result<ControlRpc> {
    ControlRpc::connect(config).await.map_err(|e| anyhow!("{e}"))
}

/// [`run_control`] with explicit settings, answers and output.
pub async fn run_control_with(
    config: BaseConfig,
    env: &ControlEnv,
    env_file: Option<PathBuf>,
    command: ControlCommands,
    prompter: &dyn prompt::Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    if matches!(command, ControlCommands::Restore) {
        let ctx = ControlContext::open_for_restore(config, env, env_file)?;
        let rpc = connect(&ctx.config).await?;
        return sync::run_restore(&ctx, &rpc, prompter, out).await;
    }
    let ctx = ControlContext::open(config, env, env_file).await?;
    match command {
        ControlCommands::Show { json, offline: true } => show::run_show(&ctx, None, out, json).await,
        ControlCommands::Show { json, offline: false } => {
            let rpc = connect(&ctx.config).await;
            match rpc {
                Ok(rpc) => show::run_show(&ctx, Some(&rpc), out, json).await,
                Err(e) => {
                    warn!("agent control: {e:#}; showing the local copy");
                    show::run_show(&ctx, None, out, json).await
                }
            }
        }
        ControlCommands::Policy { command: PolicyCommands::Show { pending, json } } => {
            let rpc = if pending { Some(connect(&ctx.config).await?) } else { None };
            show::run_policy_show(&ctx, rpc.as_ref(), out, json).await
        }
        ControlCommands::Policy { command: PolicyCommands::Accept { hash, yes } } => {
            let rpc = connect(&ctx.config).await?;
            policy_accept::run_accept(&ctx, &rpc, prompter, out, hash.as_deref(), yes).await.map(|_| ())
        }
        ControlCommands::Execute { code } => {
            let rpc = connect(&ctx.config).await?;
            let mut ledger = commands::LazyLedger::new(ctx.config.clone());
            let handled = commands::run_execute(&ctx, &rpc, &mut ledger, prompter, out, &code).await?;
            commands::exit_status(&handled)
        }
        ControlCommands::Commands => {
            let rpc = connect(&ctx.config).await?;
            commands::run_list(&ctx, &rpc, out).await
        }
        ControlCommands::Link { code, yes } => {
            let rpc = connect(&ctx.config).await?;
            link::run_link(&ctx, &rpc, prompter, out, &code, yes).await.map(|_| ())
        }
        ControlCommands::Add { code, ed25519, label, capabilities, yes } => {
            let rpc = connect(&ctx.config).await?;
            let source = match (&code, &ed25519) {
                (Some(code), None) => link::AddSource::Code(code),
                (None, Some(key)) => link::AddSource::Ed25519 {
                    key,
                    label: label.as_deref().ok_or_else(|| anyhow!("--label is required with --ed25519"))?,
                    capabilities: capabilities
                        .as_deref()
                        .ok_or_else(|| anyhow!("--capabilities is required with --ed25519"))?,
                },
                _ => bail!("give either --code or --ed25519"),
            };
            link::run_add(&ctx, &rpc, prompter, out, source, yes).await.map(|_| ())
        }
        ControlCommands::Remove { controller, force, yes } => {
            let rpc = connect(&ctx.config).await?;
            change::run_change(&ctx, &rpc, prompter, out, &controller, change::Change::Remove, force, yes)
                .await
                .map(|_| ())
        }
        ControlCommands::Capabilities { controller, capabilities, force, yes } => {
            let set = link::parse_capabilities(&capabilities)?;
            let rpc = connect(&ctx.config).await?;
            change::run_change(&ctx, &rpc, prompter, out, &controller, change::Change::Capabilities(set), force, yes)
                .await
                .map(|_| ())
        }
        ControlCommands::Sync => {
            let rpc = connect(&ctx.config).await?;
            sync::run_sync(&ctx, &rpc, out).await
        }
        ControlCommands::DiscardPending => {
            let rpc = connect(&ctx.config).await?;
            sync::run_discard_pending(&ctx, &rpc, prompter, out).await
        }
        ControlCommands::Restore => bail!("internal: restore is handled above"),
    }
}

#[cfg(test)]
mod tests;
