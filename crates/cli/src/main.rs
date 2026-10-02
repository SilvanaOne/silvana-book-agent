//! Cloud Agent CLI — thin wrapper around the `orderbook-cloud-agent` library.
#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use agent_logic::config::{BaseConfig, ConfigOverrides};
use agent_logic::secret::Zeroizing;
use cloud_agent::{
    InfoCommands, PreapprovalCommands, TransferCommands, SignCommands,
    UserServiceCommands, SubscriptionCommands, LockCommands, FaucetCommands,
    AtomicCommands,
    populate_instruments, run_cloud_agent, run_fill, run_info, run_preapproval,
    run_subscription, run_transfer, run_sign, run_user_service, run_faucet,
    run_lock, run_atomic, run_generate_private_key, run_onboard, read_env_value,
    run_atomic_keygen, run_info_network_anonymous, run_info_party_from_env,
    run_sign_standalone,
    fill_loop,
    config,
};

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "cloud-agent")]
#[command(about = "Orderbook cloud agent\n\nGet started:\n  ./cloud-agent onboard --agent-name your-agent-name --email your-email")]
struct Cli {
    /// Path to agent configuration file
    #[arg(short, long, default_value = "agent.toml")]
    config: PathBuf,

    /// Party ID (overrides PARTY_AGENT)
    #[arg(long, global = true, value_name = "PARTY_ID")]
    party: Option<String>,

    /// Base58 Ed25519 private key (overrides PARTY_AGENT_PRIVATE_KEY; onboard keeps a key already in .env).
    /// Omit to be prompted when a party is set but no key is configured.
    #[arg(long, global = true, value_name = "BASE58")]
    private_key: Option<String>,

    /// Hex secp256k1 quote-signing key (overrides ATOMIC_QUOTE_PRIVATE_KEY; not used by onboard)
    #[arg(long, global = true, value_name = "HEX")]
    quote_private_key: Option<String>,

    /// Path to env file to load (default: search for `.env` in CWD and parents).
    /// An explicit --env-file overrides variables already set in the environment.
    #[arg(long, global = true, value_name = "PATH")]
    env_file: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Dry run: prepare and verify transaction but do not sign or execute
    #[arg(long, global = true)]
    dry_run: bool,

    /// Force: sign and execute even if verification fails
    #[arg(long, global = true)]
    force: bool,

    /// Prompt for confirmation before signing each transaction
    #[arg(long, global = true)]
    confirm: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run as settlement agent (long-running, places orders + settles)
    Agent {
        /// Disable order placement
        #[arg(long)]
        settlement_only: bool,
        /// Disable settlement
        #[arg(long)]
        orders_only: bool,
        /// Skip restoring state from previous session
        #[arg(long)]
        no_restore: bool,
        /// Accept all proposals without verification (for migration from old worker without saved state)
        #[arg(long)]
        no_reject: bool,
    },
    /// Query information
    Info {
        #[command(subcommand)]
        command: InfoCommands,
    },
    /// Preapproval operations
    Preapproval {
        #[command(subcommand)]
        command: PreapprovalCommands,
    },
    /// Subscription payment operations
    Subscription {
        #[command(subcommand)]
        command: SubscriptionCommands,
    },
    /// Transfer operations (CC and CIP-56 tokens)
    Transfer {
        #[command(subcommand)]
        command: TransferCommands,
    },
    /// Signing operations
    Sign {
        #[command(subcommand)]
        command: SignCommands,
    },
    /// User service operations (onboarding)
    UserService {
        #[command(subcommand)]
        command: UserServiceCommands,
    },
    /// Faucet — request tokens (CC or CIP-56)
    Faucet {
        #[command(subcommand)]
        command: FaucetCommands,
    },
    /// Lock operations (LockService / LockController)
    Lock {
        #[command(subcommand)]
        command: LockCommands,
    },
    /// Atomic DVP (RFQ V2) operations: quote key, ticket service, venues, tickets, splits
    Atomic {
        #[command(subcommand)]
        command: AtomicCommands,
    },
    /// Buy a specified amount via RFQ, repeating until filled
    Buy {
        /// Market ID to buy on
        #[arg(long)]
        market: String,
        /// Total amount to buy (base instrument quantity)
        #[arg(long, value_parser = positive_number)]
        amount: f64,
        /// Maximum price per unit (default: mid + 3%)
        #[arg(long, value_parser = positive_number)]
        price_limit: Option<f64>,
        /// Minimum amount per settlement (default: 5.0)
        #[arg(long, default_value = "5.0", value_parser = positive_number)]
        min_settlement: f64,
        /// Maximum amount per settlement (default: total amount)
        #[arg(long, value_parser = positive_number)]
        max_settlement: Option<f64>,
        /// Retry interval in seconds, at least 1 (default: 60)
        #[arg(long, default_value = "60", value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Settle via RFQ V2 / AtomicDVP (one atomic transaction per round)
        #[arg(long)]
        atomic: bool,
        /// Settlement-fee token preference for --atomic, priority order
        /// (repeatable, e.g. --fee-token USDC --fee-token CC). Default: CC.
        #[arg(long = "fee-token")]
        fee_token: Vec<String>,
    },
    /// Sell a specified amount via RFQ, repeating until filled
    Sell {
        /// Market ID to sell on
        #[arg(long)]
        market: String,
        /// Total amount to sell (base instrument quantity)
        #[arg(long, value_parser = positive_number)]
        amount: f64,
        /// Minimum price per unit (default: mid - 3%)
        #[arg(long, value_parser = positive_number)]
        price_limit: Option<f64>,
        /// Minimum amount per settlement (default: 5.0)
        #[arg(long, default_value = "5.0", value_parser = positive_number)]
        min_settlement: f64,
        /// Maximum amount per settlement (default: total amount)
        #[arg(long, value_parser = positive_number)]
        max_settlement: Option<f64>,
        /// Retry interval in seconds, at least 1 (default: 60)
        #[arg(long, default_value = "60", value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Settle via RFQ V2 / AtomicDVP (one atomic transaction per round)
        #[arg(long)]
        atomic: bool,
        /// Settlement-fee token preference for --atomic, priority order
        /// (repeatable, e.g. --fee-token USDC --fee-token CC). Default: CC.
        #[arg(long = "fee-token")]
        fee_token: Vec<String>,
    },
    /// Generate a new Ed25519 private key (no config needed)
    GeneratePrivateKey,
    /// Self-service onboarding: generate keys, register, sign topology, complete ledger setup
    ///
    /// Usage: ./cloud-agent onboard --agent-name your-agent-name --email your-email --invite-code your-invite-code
    Onboard {
        /// Orderbook gRPC URL (optional, default: devnet)
        #[arg(long, default_value = "https://orderbook-devnet.silvana.dev:443")]
        rpc: String,
        /// Agent display name (required)
        #[arg(long)]
        agent_name: String,
        /// Contact email (required)
        #[arg(long)]
        email: String,
        /// Invite code — required, for waiting list registration
        #[arg(long, required = true)]
        invite_code: String,
        /// Party ID — optional, skip waiting list and go straight to ledger onboarding
        /// (prompts for the private key if none is configured)
        #[arg(long)]
        party: Option<String>,
        /// Base58-encoded Ed25519 private key — optional, used with --party
        #[arg(long)]
        private_key: Option<String>,
        /// Seconds between status polls, 1-3600 — optional (default: 10)
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u64).range(1..=3600))]
        poll_interval: u64,
    },
}


/// Clap parser for amounts and prices: a finite number above 0.
fn positive_number(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e: std::num::ParseFloatError| e.to_string())?;
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err("must be a finite number above 0".to_string())
    }
}

// ============================================================================
// Main
// ============================================================================

/// Longest wait for leftover background work once a command has finished.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// Bound on the instrument registry fetch that precedes configured commands.
const INSTRUMENTS_BUDGET: Duration = Duration::from_secs(60);
/// Bound on the anonymous `info network` query.
const NETWORK_INFO_BUDGET: Duration = Duration::from_secs(90);
/// How long the terminal prompt waits for the private key.
const KEY_PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest wait for one `stty` run on the terminal.
#[cfg(unix)]
const STTY_WAIT: Duration = Duration::from_secs(2);

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Env changes happen here, while the process is still single-threaded.
    load_env_file(cli.env_file.as_deref(), matches!(cli.command, Commands::Onboard { .. }))?;

    // Initialize logging (LOG_DESTINATION=console|file)
    agent_logic::logging::init_logging(
        cli.verbose,
        &["cloud_agent", "agent_logic", "tx_verifier"],
        "cloud-agent",
    )?;
    agent_logic::panic_hook::install();

    match prepare(cli)? {
        Some(job) => run_on_runtime(job.run(), SHUTDOWN_GRACE),
        None => Ok(()),
    }
}

/// Run `work` on a multi-threaded runtime; leftover tasks get `grace` to stop.
fn run_on_runtime<T>(work: impl Future<Output = Result<T>>, grace: Duration) -> Result<T> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    // Set explicitly: tokio panics on a bad TOKIO_WORKER_THREADS it reads itself
    if let Some(n) = worker_threads(std::env::var_os("TOKIO_WORKER_THREADS"))? {
        builder.worker_threads(n);
    }
    let runtime = start_runtime(|| builder.enable_all().build())?;
    let result = runtime.block_on(work);
    runtime.shutdown_timeout(grace);
    result
}

/// Build the runtime; tokio panics instead of erroring when its first worker thread cannot start.
fn start_runtime(
    build: impl FnOnce() -> std::io::Result<tokio::runtime::Runtime>,
) -> Result<tokio::runtime::Runtime> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(build)) {
        Ok(built) => built.context("failed to start the async runtime"),
        Err(payload) => Err(anyhow!(
            "failed to start the async runtime: {}",
            agent_logic::supervise::panic_message(payload.as_ref())
        )),
    }
}

/// Largest accepted `TOKIO_WORKER_THREADS`.
const MAX_WORKER_THREADS: usize = 1024;

/// `TOKIO_WORKER_THREADS`: unset leaves tokio's default; anything but 1..=1024 is an error.
fn worker_threads(raw: Option<std::ffi::OsString>) -> Result<Option<usize>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let text = raw
        .to_str()
        .ok_or_else(|| anyhow!("TOKIO_WORKER_THREADS is not valid unicode"))?
        .trim();
    match text.parse::<usize>() {
        Ok(n) if (1..=MAX_WORKER_THREADS).contains(&n) => Ok(Some(n)),
        _ => bail!("TOKIO_WORKER_THREADS={text:?} must be a whole number in 1..={MAX_WORKER_THREADS}"),
    }
}

/// An explicit `--env-file` overrides variables already set in the environment;
/// the default `.env` search only fills in unset ones.
fn load_env_file(path: Option<&Path>, onboarding: bool) -> Result<()> {
    let (file, overwrite) = match path {
        Some(path) if path.exists() => (path.to_path_buf(), true),
        // onboard creates the env file itself — a missing target is expected
        Some(_) if onboarding => return Ok(()),
        Some(path) => bail!("env file not found: {}", path.display()),
        None => match cloud_agent::env::find_dotenv()? {
            Some(found) => (found, false),
            None => return Ok(()),
        },
    };
    let body = cloud_agent::env::read_env_file(&file)?;
    // SAFETY: first thing in main; no runtime or other thread has started yet
    let applied = unsafe { cloud_agent::env::apply_env_file(&body, overwrite) };
    applied.with_context(|| format!("Failed to load env file {}", file.display()))
}

/// Global flags passed through to the command.
#[derive(Clone, Copy, Default)]
struct Flags {
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
}

/// Work that needs the async runtime, decided before the runtime starts.
enum Job {
    Onboard(Box<OnboardArgs>),
    NetworkInfo { grpc_url: String },
    Command(Box<ConfiguredCommand>),
}

struct OnboardArgs {
    rpc: String,
    party: Option<String>,
    key: Option<Zeroizing<String>>,
    invite_code: String,
    agent_name: String,
    email: String,
    env_file: PathBuf,
    poll_interval: u64,
}

/// A command with its loaded config.
struct ConfiguredCommand {
    config: BaseConfig,
    command: Commands,
    flags: Flags,
    quote_for_atomic: Option<Zeroizing<String>>,
}

impl Job {
    async fn run(self) -> Result<()> {
        match self {
            Job::Onboard(args) => {
                let OnboardArgs { rpc, party, key, invite_code, agent_name, email, env_file, poll_interval } = *args;
                run_onboard(rpc, party, key, invite_code, agent_name, email, env_file, poll_interval).await
            }
            Job::NetworkInfo { grpc_url } => network_info_within(&grpc_url, NETWORK_INFO_BUDGET).await,
            Job::Command(command) => run_configured(*command).await,
        }
    }
}

/// Startup work that runs before the runtime: commands that need no runtime,
/// the key prompt, config load and the env scrub.
fn prepare(mut cli: Cli) -> Result<Option<Job>> {
    let nonblank = |s: String| (!s.trim().is_empty()).then_some(s);
    let top_party = cli.party.take().and_then(nonblank);
    let top_key = cli.private_key.take().and_then(nonblank).map(Zeroizing::new);
    let top_quote = cli
        .quote_private_key
        .take()
        .and_then(nonblank)
        .map(Zeroizing::new);
    let flags = Flags {
        verbose: cli.verbose,
        dry_run: cli.dry_run,
        force: cli.force,
        confirm: cli.confirm,
    };

    // Handle commands that don't need config first
    if let Commands::GeneratePrivateKey = &cli.command {
        return run_generate_private_key().map(|()| None);
    }

    if let Commands::Onboard { rpc, party, private_key, invite_code, agent_name, email, poll_interval } = cli.command {
        let env_file = cli.env_file.unwrap_or_else(|| PathBuf::from(".env"));
        let party = party.or(top_party);
        let mut key = private_key.and_then(nonblank).map(Zeroizing::new).or(top_key);
        let env_set = |name: &str| read_env_value(&env_file, name).is_some_and(|v| !v.is_empty());
        // An identity is known once a party or public key is recorded; the key then
        // comes from the flags, the process environment, or a prompt.
        let identity_known =
            party.is_some() || env_set("PARTY_AGENT") || env_set("PARTY_AGENT_PUBLIC_KEY");
        if identity_known && key.is_none() && !env_set("PARTY_AGENT_PRIVATE_KEY") {
            key = match std::env::var("PARTY_AGENT_PRIVATE_KEY").ok().and_then(nonblank) {
                Some(k) => Some(Zeroizing::new(k)),
                None => Some(prompt_private_key()?),
            };
        }
        return Ok(Some(Job::Onboard(Box::new(OnboardArgs {
            rpc,
            party,
            key,
            invite_code,
            agent_name,
            email,
            env_file,
            poll_interval,
        }))));
    }

    // These read public network data or generate an unrelated key, so they must
    // not demand — or prompt for — an agent private key.
    match &cli.command {
        Commands::Atomic { command: AtomicCommands::Keygen } => {
            return run_atomic_keygen(top_quote.as_ref().map(|k| k.as_str())).map(|()| None);
        }
        Commands::Info { command: InfoCommands::Party } => {
            return run_info_party_from_env(top_party.as_deref(), top_key.as_ref().map(|k| k.as_str()))
                .map(|()| None);
        }
        Commands::Info { command: InfoCommands::Network } => {
            let grpc_url = std::env::var("ORDERBOOK_GRPC_URL")
                .context("ORDERBOOK_GRPC_URL env var is required")?;
            return Ok(Some(Job::NetworkInfo { grpc_url }));
        }
        _ => {}
    }

    // `Agent` runs the full market-making loop and needs agent.toml to be present
    // and populated. All other subcommands work with serde defaults if the file
    // is missing (buy/sell/faucet/transfer/etc. only touch env-sourced fields).
    let needs_agent_toml = matches!(cli.command, Commands::Agent { .. });

    // Only the `atomic` commands need the quote key after config load.
    let quote_for_atomic = if matches!(cli.command, Commands::Atomic { .. }) {
        top_quote.clone()
    } else {
        None
    };
    // `sign` with its own key needs no agent identity at all.
    if let Commands::Sign { command } = cli.command {
        let local = match &command {
            SignCommands::Multihash { private_key, .. }
            | SignCommands::Message { private_key, .. }
            | SignCommands::Binary { private_key, .. } => {
                private_key.clone().and_then(nonblank).map(Zeroizing::new)
            }
        };
        let key = local.or(top_key);
        let signed = match key {
            Some(k) => run_sign_standalone(command, &k),
            None => {
                let overrides = resolve_key_overrides(top_party, None, top_quote)?;
                run_sign(config::load_or_defaults_with(&cli.config, overrides)?, command)
            }
        };
        return signed.map(|()| None);
    }

    let overrides = resolve_key_overrides(top_party, top_key, top_quote)?;

    let base_config = if needs_agent_toml {
        if !cli.config.exists() {
            tracing::warn!(
                "agent.toml not found at {}. This file is required for `cloud-agent agent`.",
                cli.config.display()
            );
        }
        config::load_with(&cli.config, overrides)
            .with_context(|| format!("Failed to load config from {:?}", cli.config))?
    } else {
        config::load_or_defaults_with(&cli.config, overrides)
            .with_context(|| format!("Failed to load config from {:?}", cli.config))?
    };

    scrub_private_key_env();

    Ok(Some(Job::Command(Box::new(ConfiguredCommand {
        config: base_config,
        command: cli.command,
        flags,
        quote_for_atomic,
    }))))
}

#[expect(clippy::disallowed_methods, reason = "startup only, before the runtime starts")]
fn scrub_private_key_env() {
    // The sealed copy inside the config is the only one needed from here on.
    // SAFETY: no other thread reads the environment at this point.
    unsafe { std::env::remove_var("PARTY_AGENT_PRIVATE_KEY") };
}

async fn run_configured(job: ConfiguredCommand) -> Result<()> {
    let ConfiguredCommand { config: mut base_config, command, flags, quote_for_atomic } = job;

    // Populate instrument registry (CC → Amulet + DSO, USDC → registry, …) from
    // orderbook-rpc. Required by any command that builds DVP/transfer expectations.
    // Best-effort: failures are logged but don't block commands that don't need it
    // (e.g. `sign`, `generate-private-key`).
    if let Err(e) = populate_instruments_within(&mut base_config, INSTRUMENTS_BUDGET).await {
        tracing::warn!("Failed to populate instrument registry from RPC: {:#}", e);
    }
    let base_config = base_config;

    // Install the best-effort error reporter (ReportErrors -> orderbook-rpc)
    // for EVERY command before dispatch — the production `agent` path never
    // enters a fill loop, so initializing only there (or in the unused
    // cancel_settlement path) left all agent error hooks as silent no-ops.
    // Idempotent; a no-op for commands that never report.
    agent_logic::error_reporter::init_from_config(&base_config);

    dispatch(base_config, command, flags, quote_for_atomic).await
}

/// [`populate_instruments`] bounded by `budget`; an elapsed fetch leaves `config` as it was.
async fn populate_instruments_within(config: &mut BaseConfig, budget: Duration) -> Result<()> {
    tokio::time::timeout(budget, populate_instruments(config))
        .await
        .map_err(|_| anyhow!("no response within {budget:?}"))?
}

/// The anonymous `info network` query, bounded by `budget`.
async fn network_info_within(grpc_url: &str, budget: Duration) -> Result<()> {
    tokio::time::timeout(budget, run_info_network_anonymous(grpc_url, 30, 60))
        .await
        .map_err(|_| anyhow!("info network: no response from {grpc_url} within {budget:?}"))?
}

async fn dispatch(
    base_config: BaseConfig,
    command: Commands,
    flags: Flags,
    quote_for_atomic: Option<Zeroizing<String>>,
) -> Result<()> {
    let Flags { verbose, dry_run, force, confirm } = flags;
    match command {
        Commands::Agent {
            settlement_only,
            orders_only,
            no_restore,
            no_reject,
        } => {
            let version_info = version_info();
            run_cloud_agent(base_config, settlement_only, orders_only, no_restore, no_reject, verbose, dry_run, force, confirm, Some(&version_info)).await
        }
        Commands::Info { command } => run_info(base_config, command).await,
        Commands::Preapproval { command } => run_preapproval(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Subscription { command } => run_subscription(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Transfer { command } => run_transfer(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Sign { command } => run_sign(base_config, command),
        Commands::UserService { command } => run_user_service(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Faucet { command } => run_faucet(base_config, command, verbose).await,
        Commands::Lock { command } => run_lock(base_config, command, verbose, dry_run, force, confirm).await,
        Commands::Atomic { command } => run_atomic(base_config, command, verbose, dry_run, force, confirm, quote_for_atomic).await,
        Commands::Buy { market, amount, price_limit, min_settlement, max_settlement, interval, atomic, fee_token } => {
            run_fill(base_config, fill_loop::FillDirection::Buy, market, amount, price_limit, min_settlement, max_settlement, interval, atomic, fee_token, verbose, dry_run, force, confirm).await
        }
        Commands::Sell { market, amount, price_limit, min_settlement, max_settlement, interval, atomic, fee_token } => {
            run_fill(base_config, fill_loop::FillDirection::Sell, market, amount, price_limit, min_settlement, max_settlement, interval, atomic, fee_token, verbose, dry_run, force, confirm).await
        }
        Commands::GeneratePrivateKey | Commands::Onboard { .. } => {
            bail!("internal: command dispatched before config load")
        }
    }
}

fn version_info() -> String {
    format!(
        "{}{} commit {}",
        short_sha(env!("VERGEN_GIT_SHA")),
        if env!("VERGEN_GIT_DIRTY") == "true" { "-dirty" } else { "" },
        env!("VERGEN_GIT_COMMIT_TIMESTAMP"),
    )
}

/// The first 12 characters of a commit hash, or all of a shorter one.
fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Identity overrides: flags win, then env; the private key is prompted for
/// when a party is known but no key is configured.
fn resolve_key_overrides(
    party: Option<String>,
    private_key: Option<Zeroizing<String>>,
    quote_private_key: Option<Zeroizing<String>>,
) -> Result<ConfigOverrides> {
    let env_has = |name: &str| {
        std::env::var(name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    let mut private_key = private_key;
    if private_key.is_none()
        && (party.is_some() || env_has("PARTY_AGENT"))
        && !env_has("PARTY_AGENT_PRIVATE_KEY")
    {
        private_key = Some(prompt_private_key()?);
    }
    Ok(ConfigOverrides {
        party,
        private_key,
        quote_private_key,
    })
}

/// Read the base58 private key without echo from the controlling terminal.
fn prompt_private_key() -> Result<Zeroizing<String>> {
    if !terminal_available() {
        anyhow::bail!(
            "PARTY_AGENT_PRIVATE_KEY is not set — pass --private-key, add it to .env, \
             or run in a terminal to be prompted"
        );
    }
    let restore = terminal_restorer();
    read_key_within(
        KEY_PROMPT_TIMEOUT,
        || read_key_with_retry(|| rpassword::prompt_password("Private key (base58, input hidden): ")),
        restore,
    )
}

/// Run a key read on its own thread so an unattended terminal cannot hold startup.
/// On a timeout `on_timeout` puts the terminal back and says whether it could.
fn read_key_within<F, R>(limit: Duration, read: F, on_timeout: R) -> Result<Zeroizing<String>>
where
    F: FnOnce() -> Result<Zeroizing<String>> + Send + 'static,
    R: FnOnce() -> bool,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("key-prompt".to_string())
        .spawn(move || {
            let _ = tx.send(read());
        })
        .context("could not start the private key prompt")?;
    match rx.recv_timeout(limit) {
        Ok(key) => key,
        Err(RecvTimeoutError::Timeout) => {
            // The prompt holds the terminal without echo until it returns
            let restored = on_timeout();
            agent_logic::errln!();
            let hint = if restored { "" } else { "; run `stty sane` if typing does not echo" };
            bail!(
                "PARTY_AGENT_PRIVATE_KEY is not set and no key was entered within {limit:?} — \
                 pass --private-key or add it to .env{hint}"
            )
        }
        Err(RecvTimeoutError::Disconnected) => bail!(
            "the private key prompt failed — pass --private-key or add \
             PARTY_AGENT_PRIVATE_KEY to .env"
        ),
    }
}

/// Takes the terminal settings now; the returned hook puts them back.
#[cfg(unix)]
fn terminal_restorer() -> impl FnOnce() -> bool {
    let saved = tty_snapshot();
    move || saved.as_deref().is_some_and(tty_restore)
}

#[cfg(not(unix))]
fn terminal_restorer() -> impl FnOnce() -> bool {
    || true
}

/// The controlling terminal's settings, as `stty -g` prints them.
#[cfg(unix)]
fn tty_snapshot() -> Option<String> {
    stty(&["-g"]).filter(|saved| !saved.is_empty())
}

/// Apply settings from [`tty_snapshot`]; true when `stty` accepted them.
#[cfg(unix)]
fn tty_restore(saved: &str) -> bool {
    stty(&[saved]).is_some()
}

/// Run `stty` on the controlling terminal; its output, or `None` when it
/// fails or runs past [`STTY_WAIT`].
#[cfg(unix)]
fn stty(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let tty = std::fs::File::open("/dev/tty").ok()?;
    let mut child = Command::new("stty")
        .args(args)
        .stdin(tty)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now().checked_add(STTY_WAIT);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out.trim().to_string());
            }
            Ok(Some(_)) => return None,
            Ok(None) if deadline.is_some_and(|d| std::time::Instant::now() < d) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// rpassword prompts on and reads from the controlling terminal, so check that one.
fn terminal_available() -> bool {
    #[cfg(unix)]
    {
        std::fs::File::open("/dev/tty").is_ok()
    }
    #[cfg(not(unix))]
    {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    }
}

/// One retry on a decode error; a read failure carries a `--private-key` hint.
fn read_key_with_retry(
    mut read: impl FnMut() -> std::io::Result<String>,
) -> Result<Zeroizing<String>> {
    for attempt in 0..2 {
        let raw = Zeroizing::new(read().context(
            "could not read the private key from the terminal — pass --private-key \
             or add PARTY_AGENT_PRIVATE_KEY to .env",
        )?);
        let key = Zeroizing::new(raw.trim().to_string());
        match agent_logic::config::validate_private_key(&key) {
            Ok(()) => return Ok(key),
            Err(e) if attempt == 0 => agent_logic::errln!("Invalid key: {e:#}"),
            Err(e) => return Err(e.context("invalid private key")),
        }
    }
    Err(anyhow!("invalid private key"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::collections::VecDeque;
    use std::time::Instant;

    const KEY: &str =
        "EB92Q6V2a78t9ppqMuKLppyfzFgyYJciQEVHZKnXAhjEwVpx9aMbQN84SR4ceo3mbLUxQF7TLzaEujaTJnS7eRF";

    fn reader(inputs: Vec<std::io::Result<String>>) -> impl FnMut() -> std::io::Result<String> {
        let mut queue: VecDeque<_> = inputs.into();
        move || queue.pop_front().expect("reader called too often")
    }

    #[test]
    fn test_read_key_retries_once() {
        let key = read_key_with_retry(reader(vec![Ok("garbage".into()), Ok(KEY.into())])).unwrap();
        assert_eq!(&*key, KEY);
    }

    #[test]
    fn test_read_key_fails_after_second_bad_input() {
        let err = read_key_with_retry(reader(vec![Ok("garbage".into()), Ok("garbage".into())]))
            .unwrap_err();
        assert!(format!("{err:#}").contains("invalid private key"));
    }

    #[test]
    fn test_read_key_trims_input() {
        let key = read_key_with_retry(reader(vec![Ok(format!("  {KEY}\n"))])).unwrap();
        assert_eq!(&*key, KEY);
    }

    #[test]
    fn test_read_key_io_error_names_flag() {
        let err = read_key_with_retry(reader(vec![Err(std::io::Error::other("closed"))]))
            .unwrap_err();
        assert!(format!("{err:#}").contains("--private-key"));
    }

    /// A timeout hook that records whether it ran and reports `restored`.
    fn hook(restored: bool) -> (std::sync::Arc<std::sync::atomic::AtomicBool>, impl FnOnce() -> bool) {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        (ran, move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            restored
        })
    }

    fn unanswered() -> Result<Zeroizing<String>> {
        std::thread::sleep(Duration::from_secs(3));
        Ok(Zeroizing::new(KEY.to_string()))
    }

    // The prompt used to time out with the terminal left without echo or line editing
    #[test]
    fn an_unanswered_prompt_gives_up_with_the_not_set_error() {
        let started = Instant::now();
        let (ran, restore) = hook(true);
        let err = read_key_within(Duration::from_millis(100), unanswered, restore).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst), "the terminal is put back");
        let text = format!("{err:#}");
        assert!(text.contains("PARTY_AGENT_PRIVATE_KEY is not set"), "{text}");
        assert!(text.contains("no key was entered within 100ms"), "{text}");
        assert!(!text.contains("stty sane"), "{text}");
    }

    #[test]
    fn a_terminal_that_cannot_be_put_back_gets_a_hint() {
        let (ran, restore) = hook(false);
        let err = read_key_within(Duration::from_millis(100), unanswered, restore).unwrap_err();
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        let text = format!("{err:#}");
        assert!(text.contains("no key was entered within 100ms") && text.contains("run `stty sane`"), "{text}");
    }

    #[test]
    fn an_answered_prompt_returns_the_key() {
        let (ran, restore) = hook(true);
        let key = read_key_within(
            Duration::from_secs(5),
            || read_key_with_retry(reader(vec![Ok(KEY.into())])),
            restore,
        )
        .unwrap();
        assert_eq!(&*key, KEY);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst), "an answered prompt restores itself");
        let (ran, restore) = hook(true);
        let err = read_key_within(
            Duration::from_secs(5),
            || read_key_with_retry(reader(vec![Ok("garbage".into()), Ok("garbage".into())])),
            restore,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("invalid private key"));
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn a_failed_prompt_thread_is_an_error() {
        let (ran, restore) = hook(true);
        let err = read_key_within(Duration::from_secs(5), || panic!("tty went away"), restore).unwrap_err();
        assert!(format!("{err:#}").contains("private key prompt failed"), "{err:#}");
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst), "an unwinding prompt restores itself");
    }

    // Settings stty refuses, or no terminal at all, report a failed restore
    #[cfg(unix)]
    #[test]
    fn a_refused_terminal_restore_reports_failure() {
        assert!(!tty_restore("not-a-terminal-setting"));
    }

    #[test]
    fn the_runtime_has_timers_and_io() {
        let port = run_on_runtime(
            async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                Ok(listener.local_addr()?.port())
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_ne!(port, 0);
    }

    // tokio panics, rather than erroring, when the OS refuses its first worker thread
    #[test]
    fn a_panicking_runtime_build_is_an_error() {
        let msg = |r: Result<tokio::runtime::Runtime>| r.err().map(|e| format!("{e:#}")).unwrap_or_default();
        let err = msg(start_runtime(|| panic!("OS can't spawn worker thread: probe")));
        assert!(err.contains("failed to start the async runtime"), "{err}");
        assert!(err.contains("OS can't spawn worker thread: probe"), "{err}");
        let err = msg(start_runtime(|| Err(std::io::Error::other("no driver"))));
        assert!(err.contains("failed to start the async runtime") && err.contains("no driver"), "{err}");
        assert!(start_runtime(|| tokio::runtime::Builder::new_current_thread().build()).is_ok());
    }

    // tokio panicked on these while reading TOKIO_WORKER_THREADS itself
    #[test]
    fn worker_threads_are_validated_before_the_runtime_reads_them() {
        let parse = |s: &str| worker_threads(Some(s.into()));
        assert_eq!(worker_threads(None).unwrap(), None);
        assert_eq!(parse("4").unwrap(), Some(4));
        assert_eq!(parse(" 1024 ").unwrap(), Some(1024));
        for bad in ["", " ", "0", "abc", "4x", "-1", "1025", "2000", "99999999999999999999"] {
            let err = parse(bad).unwrap_err().to_string();
            assert!(err.contains("TOKIO_WORKER_THREADS"), "{bad:?}: {err}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let not_unicode = std::ffi::OsString::from_vec(vec![0x34, 0xFF]);
            assert!(worker_threads(Some(not_unicode)).is_err());
        }
    }

    #[test]
    fn leftover_blocking_work_does_not_hold_the_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = run_on_runtime(
                async {
                    drop(tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(30))));
                    Ok(7)
                },
                Duration::from_millis(100),
            );
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("runtime shutdown waited for leftover blocking work");
        assert_eq!(result.unwrap(), 7);
    }

    /// A listener that completes TCP connects but never answers.
    fn silent_server() -> (std::net::TcpListener, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    #[tokio::test]
    async fn a_silent_instrument_registry_is_bounded() {
        let (_server, url) = silent_server();
        let mut config = BaseConfig::test_minimal().unwrap();
        config.orderbook_grpc_url = url;
        let started = Instant::now();
        let err = populate_instruments_within(&mut config, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(format!("{err:#}").contains("no response within 300ms"), "{err:#}");
        assert!(config.instrument_registries.is_empty());
    }

    #[tokio::test]
    async fn a_silent_network_info_query_is_bounded() {
        let (_server, url) = silent_server();
        let started = Instant::now();
        let err = network_info_within(&url, Duration::from_millis(300)).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        let text = format!("{err:#}");
        assert!(text.contains("info network: no response from") && text.contains("within 300ms"), "{text}");
    }

    #[tokio::test]
    async fn early_commands_are_refused_by_dispatch() {
        let onboard = Commands::Onboard {
            rpc: String::new(),
            agent_name: String::new(),
            email: String::new(),
            invite_code: String::new(),
            party: None,
            private_key: None,
            poll_interval: 10,
        };
        for command in [Commands::GeneratePrivateKey, onboard] {
            let config = BaseConfig::test_minimal().unwrap();
            let err = dispatch(config, command, Flags::default(), None).await.unwrap_err();
            assert!(format!("{err:#}").contains("dispatched before config load"), "{err:#}");
        }
    }

    #[test]
    fn short_sha_never_slices_past_the_end() {
        assert_eq!(short_sha("0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
        assert!(version_info().contains(" commit "));
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("cloud-agent").chain(args.iter().copied()))
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    fn refused(args: &[&str]) -> bool {
        matches!(parse(args), Err(e) if e.kind() == clap::error::ErrorKind::ValueValidation)
    }

    #[test]
    fn fill_amounts_and_prices_must_be_finite_and_positive() {
        for side in ["buy", "sell"] {
            assert!(parse(&[side, "--market", "M", "--amount=10"]).is_ok(), "{side} defaults must parse");
            for flag in ["--amount", "--price-limit", "--min-settlement", "--max-settlement"] {
                for bad in ["NaN", "inf", "-inf", "0", "-0", "-1", "abc"] {
                    let value = format!("{flag}={bad}");
                    let args = if flag == "--amount" {
                        vec![side, "--market", "M", value.as_str()]
                    } else {
                        vec![side, "--market", "M", "--amount=10", value.as_str()]
                    };
                    assert!(refused(&args), "{side} {value} must be refused");
                }
            }
            let ok = parse(&[side, "--market", "M", "--amount", "0.5", "--price-limit", "1e-6", "--min-settlement", "0.1", "--max-settlement", "2"]);
            match ok.map(|c| c.command) {
                Ok(Commands::Buy { amount, price_limit, min_settlement, max_settlement, interval, .. })
                | Ok(Commands::Sell { amount, price_limit, min_settlement, max_settlement, interval, .. }) => {
                    assert_eq!((amount, price_limit, min_settlement, max_settlement, interval), (0.5, Some(1e-6), 0.1, Some(2.0), 60));
                }
                _ => panic!("{side} with valid values must parse"),
            }
        }
    }

    #[test]
    fn fill_interval_must_be_at_least_one_second() {
        for side in ["buy", "sell"] {
            let args = |n: &'static str| [side, "--market", "M", "--amount", "1", "--interval", n];
            assert!(refused(&args("0")), "{side} --interval 0 must be refused");
            assert!(parse(&args("1")).is_ok());
        }
    }

    #[test]
    fn onboard_poll_interval_is_bounded() {
        let args = |n: &'static str| {
            ["onboard", "--agent-name", "a", "--email", "e", "--invite-code", "i", "--poll-interval", n]
        };
        assert!(refused(&args("0")));
        assert!(refused(&args("3601")));
        assert!(parse(&args("1")).is_ok());
        assert!(parse(&args("3600")).is_ok());
        let default = parse(&["onboard", "--agent-name", "a", "--email", "e", "--invite-code", "i"]).unwrap();
        assert!(matches!(default.command, Commands::Onboard { poll_interval: 10, .. }));
    }
}
