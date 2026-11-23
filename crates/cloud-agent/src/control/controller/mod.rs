//! `cloud-agent controller`: a local Ed25519 controller key, offline (`keygen`, `sign`) or
//! signed in to the rpc, where it verifies the agent's log here before it signs anything.

pub mod offline;
pub mod online;
pub mod render;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use agent_control::Network;
use anyhow::{Result, anyhow, bail};

use super::prompt::Prompter;

pub use offline::{KEY_FILE, load_key, sign};

/// The connection flags of every online command.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct OnlineArgs {
    /// The agent's party (else `agent_party` of [controller] in agent.toml)
    #[arg(long, value_name = "PARTY")]
    pub agent: Option<String>,
    /// The controller key file (else `key_file` of [controller])
    #[arg(long, value_name = "PATH")]
    pub key: Option<PathBuf>,
    /// devnet or mainnet (else `network` of [controller], else CANTON_CHAIN)
    #[arg(long)]
    pub network: Option<String>,
    /// Another rpc: https, or http on loopback only
    #[arg(long, hide = true, value_name = "URL")]
    pub rpc: Option<String>,
    /// Print JSON
    #[arg(long)]
    pub json: bool,
}

/// What a new policy changes in the rules it starts from.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct PolicyEdits {
    /// Pause the agent: no trading, transfers or other operations
    #[arg(long, conflicts_with = "resume")]
    pub pause: bool,
    /// Lift a pause
    #[arg(long)]
    pub resume: bool,
    /// Allow transfers (on) or stop them (off)
    #[arg(long, value_parser = ["on", "off"])]
    pub transfers: Option<String>,
    /// `any`, or the comma-separated package ids the agent may use
    #[arg(long, value_name = "any|ID,ID,...")]
    pub packages: Option<String>,
    /// The direct controller whose command scope changes (id or fingerprint)
    #[arg(long, value_name = "CONTROLLER")]
    pub scope: Option<String>,
    /// The command kinds it may send, comma-separated (swap, transfer_cc, transfer_token, usdcx_burn), or `none`
    #[arg(long, requires = "scope", value_name = "KINDS|none")]
    pub scope_commands: Option<String>,
    /// A swap pair `SELL,BUY,MIN_PRICE,MAX_SELL`; an instrument is `cc` or `ID@ADMIN` (repeatable; replaces the list)
    #[arg(
        long = "scope-pair",
        requires = "scope",
        value_name = "SELL,BUY,MIN_PRICE,MAX_SELL"
    )]
    pub scope_pairs: Vec<String>,
    /// Allow every swap pair, without bounds
    #[arg(long, requires = "scope", conflicts_with = "scope_pairs")]
    pub scope_any_pair: bool,
    /// An LP the swaps may settle with (repeatable; replaces the list)
    #[arg(long = "scope-lp", requires = "scope", value_name = "PARTY")]
    pub scope_lps: Vec<String>,
    /// Allow every LP
    #[arg(long, requires = "scope", conflicts_with = "scope_lps")]
    pub scope_any_lp: bool,
    /// Drop the scope: the controller may send every command again
    #[arg(
        long,
        requires = "scope",
        conflicts_with_all = ["scope_commands", "scope_pairs", "scope_any_pair", "scope_lps", "scope_any_lp"]
    )]
    pub scope_remove: bool,
    /// The whole rules from a file: a policy document, or its rule lines (see `policy show --rules`)
    #[arg(long, value_name = "PATH", conflicts_with_all = ["pause", "resume", "transfers", "packages", "scope"])]
    pub rules: Option<PathBuf>,
}

impl PolicyEdits {
    pub(crate) fn is_empty(&self) -> bool {
        !self.pause
            && !self.resume
            && self.transfers.is_none()
            && self.packages.is_none()
            && self.scope.is_none()
            && self.rules.is_none()
    }
}

/// The options of a command that drafts and signs.
#[derive(Clone, Debug, clap::Args)]
pub struct CommandOptions {
    /// The summary the command carries (else one is written)
    #[arg(long)]
    pub summary: Option<String>,
    /// Seconds the command stays valid, 1-900
    #[arg(
        long,
        default_value_t = agent_controller::COMMAND_VALIDITY_SECS,
        value_parser = clap::value_parser!(u64).range(1..=agent_controller::MAX_COMMAND_VALIDITY_SECS)
    )]
    pub validity: u64,
    /// Wait for the agent's receipt (exit 3 while pending)
    #[arg(long)]
    pub wait: bool,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum PolicyCommands {
    /// The policy in force and the scheduled one, from the verified log
    Show {
        #[command(flatten)]
        online: OnlineArgs,
        /// Print the rules in force as lines for `policy set --rules`
        #[arg(long)]
        rules: bool,
    },
    /// Sign a new policy: the rules in force (or the scheduled ones) with these changes
    Set {
        #[command(flatten)]
        online: OnlineArgs,
        #[command(flatten)]
        edits: Box<PolicyEdits>,
        /// Start from the rules in force or from the scheduled policy
        #[arg(long, value_parser = ["in-force", "scheduled"], default_value = "in-force")]
        base: String,
        /// Seconds the agent has to accept it, 1-86400
        #[arg(
            long,
            default_value_t = agent_controller::POLICY_VALIDITY_SECS,
            value_parser = clap::value_parser!(u64).range(1..=agent_controller::MAX_POLICY_VALIDITY_SECS)
        )]
        validity: u64,
        /// Skip the typed confirmation; refused when the policy widens anything
        #[arg(long)]
        yes: bool,
        /// Wait until the agent accepts or refuses it (exit 3 while pending)
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum CommandCommands {
    /// Send Canton Coin from the agent
    TransferCc {
        #[command(flatten)]
        online: OnlineArgs,
        /// The receiving party
        #[arg(long, value_name = "PARTY")]
        to: String,
        /// The amount, a plain decimal
        #[arg(long)]
        amount: String,
        /// The transfer's description
        #[arg(long)]
        description: Option<String>,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// Send a CIP-56 token from the agent
    TransferToken {
        #[command(flatten)]
        online: OnlineArgs,
        /// The receiving party
        #[arg(long, value_name = "PARTY")]
        to: String,
        /// The instrument's admin (registry) party
        #[arg(long, value_name = "PARTY")]
        instrument_admin: String,
        /// The instrument's on-chain id
        #[arg(long)]
        instrument_id: String,
        /// The amount, a plain decimal
        #[arg(long)]
        amount: String,
        /// The transfer's reference
        #[arg(long)]
        reference: Option<String>,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// Sell exactly an amount for at least a minimum, through RFQ V2, settled atomically
    Swap {
        #[command(flatten)]
        online: OnlineArgs,
        /// The sold instrument's admin party
        #[arg(long, value_name = "PARTY")]
        sell_admin: String,
        /// The sold instrument's id
        #[arg(long)]
        sell_id: String,
        /// The exact amount sold
        #[arg(long)]
        sell_amount: String,
        /// The bought instrument's admin party
        #[arg(long, value_name = "PARTY")]
        buy_admin: String,
        /// The bought instrument's id
        #[arg(long)]
        buy_id: String,
        /// The least amount received, net of the CC fee when buying CC
        #[arg(long)]
        min_receive: String,
        /// The most CC paid in fees; 0 takes only fee-less quotes
        #[arg(long, default_value = "0")]
        max_fee_cc: String,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// A command and its receipt (CMD-<id>)
    Show {
        #[command(flatten)]
        online: OnlineArgs,
        /// The command's code, CMD-<id>
        #[arg(long)]
        code: String,
        /// Wait for the agent's receipt (exit 3 while pending)
        #[arg(long)]
        wait: bool,
    },
}

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
    /// The agent as its log verifies here: this key's standing, the controllers, the policy
    Status(OnlineArgs),
    /// The control log, verified here
    Log(OnlineArgs),
    /// The agent's holdings, as the rpc reports them
    Holdings(OnlineArgs),
    /// The agent's trading activity, as the rpc reports it
    Activity {
        #[command(flatten)]
        online: OnlineArgs,
        #[arg(long, default_value = "20", value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long, default_value = "0")]
        offset: u32,
    },
    /// The agent's ledger transactions, as the rpc reports them
    Ledger {
        #[command(flatten)]
        online: OnlineArgs,
        #[arg(long, default_value = "20", value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long, default_value = "0")]
        offset: u32,
    },
    /// The agent's traffic, as the rpc reports it
    Traffic(OnlineArgs),
    /// The packages the node vets, as the rpc reports them
    Packages {
        #[command(flatten)]
        online: OnlineArgs,
        /// Include packages the node no longer vets
        #[arg(long)]
        removed: bool,
    },
    /// Show or sign the agent's policy
    Policy {
        #[command(subcommand)]
        command: PolicyCommands,
    },
    /// Sign a direct command, or show one
    Command {
        #[command(subcommand)]
        command: CommandCommands,
    },
    /// The agent's direct commands, as the rpc reports them
    Commands {
        #[command(flatten)]
        online: OnlineArgs,
        #[arg(long, default_value = "20", value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        #[arg(long, default_value = "0")]
        offset: u32,
    },
}

impl ControllerCommands {
    /// True for the commands that use no network.
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Keygen { .. } | Self::Sign { .. })
    }
}

/// How long `--wait` goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitBounds {
    /// Seconds a wait for a command goes on after it expires: the agent may still settle it.
    pub grace_secs: i64,
    /// The longest wait.
    pub max: Duration,
}

impl Default for WaitBounds {
    fn default() -> Self {
        Self {
            grace_secs: super::commands::RECOVERY_GRACE_SECS,
            max: Duration::from_secs(1800),
        }
    }
}

/// What controller mode takes from the process and the global flags.
#[derive(Clone, Debug, Default)]
pub struct ControllerOptions {
    /// agent.toml; only its [controller] section is read.
    pub config: PathBuf,
    /// Draft and show, but sign and send nothing (the sign-in still signs its challenge).
    pub dry_run: bool,
    /// This machine's PARTY_AGENT, which is never a target.
    pub local_party: Option<String>,
    /// CANTON_CHAIN, the network when neither `--network` nor [controller] names one.
    pub chain: Option<String>,
    pub wait: WaitBounds,
}

/// An error after which a submitted command or policy is still pending: exit code 3.
#[derive(Debug, Clone, Copy)]
pub struct StillPending;

impl std::fmt::Display for StillPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("submitted and still pending")
    }
}

impl std::error::Error for StillPending {}

pub(crate) fn still_pending(message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(StillPending).context(message.to_string())
}

/// Whether `err` means "submitted, still pending" (exit code 3).
pub fn is_pending(err: &anyhow::Error) -> bool {
    err.downcast_ref::<StillPending>().is_some()
}

/// Runs one `controller` command on the terminal.
pub async fn run_controller(command: ControllerCommands, options: ControllerOptions) -> Result<()> {
    let mut out = std::io::stdout();
    run_controller_in(&options, command, &super::prompt::Terminal, &mut out).await
}

/// [`run_controller`] with explicit answers and output, and default options.
pub async fn run_controller_with(
    command: ControllerCommands,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    run_controller_in(&ControllerOptions::default(), command, prompter, out).await
}

/// [`run_controller`] with explicit options, answers and output.
pub async fn run_controller_in(
    options: &ControllerOptions,
    command: ControllerCommands,
    prompter: &dyn Prompter,
    out: &mut dyn Write,
) -> Result<()> {
    if options.dry_run && command.is_offline() {
        bail!(
            "--dry-run does not apply to `controller keygen` or `controller sign`: nothing was written or signed; run it without --dry-run"
        );
    }
    match command {
        ControllerCommands::Keygen { dir } => offline::keygen(&dir, out),
        ControllerCommands::Sign {
            key,
            payload,
            file,
            network,
            current_policy,
        } => {
            let network = Network::parse(network.trim())
                .map_err(|_| anyhow!("--network must be devnet or mainnet"))?;
            let bytes = match (payload, file) {
                (Some(payload), None) => offline::decode_b64url(&payload)?,
                (None, Some(file)) => offline::read_document(&file)?,
                _ => bail!("give either --payload or --file"),
            };
            let current = current_policy
                .as_deref()
                .map(offline::read_document)
                .transpose()?;
            let signer = load_key(&key)?;
            let signature =
                offline::sign_with(&signer, &bytes, network, current.as_deref(), prompter, out)
                    .await?;
            let _ = writeln!(out, "\nEd25519 signature over the full document (hex):");
            let _ = writeln!(out, "{}", hex::encode(signature));
            Ok(())
        }
        online_command => online::run(options, online_command, prompter, out).await,
    }
}
