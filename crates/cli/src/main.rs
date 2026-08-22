//! Cloud Agent CLI — thin wrapper around the `orderbook-cloud-agent` library.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use agent_logic::config::ConfigOverrides;
use agent_logic::secret::Zeroizing;
use cloud_agent::{
    InfoCommands, PreapprovalCommands, TransferCommands, SignCommands,
    UserServiceCommands, SubscriptionCommands, LockCommands, FaucetCommands,
    AtomicCommands,
    populate_instruments, run_cloud_agent, run_fill, run_info, run_preapproval,
    run_subscription, run_transfer, run_sign, run_user_service, run_faucet,
    run_lock, run_atomic, run_generate_private_key, run_onboard, read_env_value,
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
    #[arg(long, value_name = "PARTY_ID")]
    party: Option<String>,

    /// Base58 Ed25519 private key (overrides PARTY_AGENT_PRIVATE_KEY; onboard keeps a key already in .env).
    /// Omit to be prompted when a party is set but no key is configured.
    #[arg(long, value_name = "BASE58")]
    private_key: Option<String>,

    /// Hex secp256k1 quote-signing key (overrides ATOMIC_QUOTE_PRIVATE_KEY; not used by onboard)
    #[arg(long, value_name = "HEX")]
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
        #[arg(long)]
        amount: f64,
        /// Maximum price per unit (default: mid + 3%)
        #[arg(long)]
        price_limit: Option<f64>,
        /// Minimum amount per settlement (default: 5.0)
        #[arg(long, default_value = "5.0")]
        min_settlement: f64,
        /// Maximum amount per settlement (default: total amount)
        #[arg(long)]
        max_settlement: Option<f64>,
        /// Retry interval in seconds (default: 60)
        #[arg(long, default_value = "60")]
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
        #[arg(long)]
        amount: f64,
        /// Minimum price per unit (default: mid - 3%)
        #[arg(long)]
        price_limit: Option<f64>,
        /// Minimum amount per settlement (default: 5.0)
        #[arg(long, default_value = "5.0")]
        min_settlement: f64,
        /// Maximum amount per settlement (default: total amount)
        #[arg(long)]
        max_settlement: Option<f64>,
        /// Retry interval in seconds (default: 60)
        #[arg(long, default_value = "60")]
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
        /// Seconds between status polls — optional (default: 10)
        #[arg(long, default_value = "10")]
        poll_interval: u64,
    },
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let nonblank = |s: String| (!s.trim().is_empty()).then_some(s);
    let top_party = cli.party.take().and_then(nonblank);
    let top_key = cli.private_key.take().and_then(nonblank).map(Zeroizing::new);
    let top_quote = cli
        .quote_private_key
        .take()
        .and_then(nonblank)
        .map(Zeroizing::new);

    match &cli.env_file {
        Some(path) if path.exists() => {
            dotenvy::from_path_override(path)
                .with_context(|| format!("Failed to load env file {}", path.display()))?;
        }
        // onboard creates the env file itself — a missing target is expected
        Some(_) if matches!(cli.command, Commands::Onboard { .. }) => {}
        Some(path) => anyhow::bail!("env file not found: {}", path.display()),
        None => {
            let _ = dotenvy::dotenv();
        }
    }

    // Initialize logging (LOG_DESTINATION=console|file)
    agent_logic::logging::init_logging(
        cli.verbose,
        &["cloud_agent", "agent_logic", "tx_verifier"],
        "cloud-agent",
    );

    // Handle commands that don't need config first
    if let Commands::GeneratePrivateKey = &cli.command {
        return run_generate_private_key();
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
        return run_onboard(rpc, party, key, invite_code, agent_name, email, env_file, poll_interval).await;
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
    let overrides = resolve_key_overrides(top_party, top_key, top_quote)?;

    let mut base_config = if needs_agent_toml {
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

    // The sealed copy inside the config is the only one needed from here on.
    // SAFETY: no other thread reads the environment at this point.
    unsafe { std::env::remove_var("PARTY_AGENT_PRIVATE_KEY") };

    // Populate instrument registry (CC → Amulet + DSO, USDC → registry, …) from
    // orderbook-rpc. Required by any command that builds DVP/transfer expectations.
    // Best-effort: failures are logged but don't block commands that don't need it
    // (e.g. `sign`, `generate-private-key`).
    if let Err(e) = populate_instruments(&mut base_config).await {
        tracing::warn!("Failed to populate instrument registry from RPC: {:#}", e);
    }
    let base_config = base_config;

    // Install the best-effort error reporter (ReportErrors -> orderbook-rpc)
    // for EVERY command before dispatch — the production `agent` path never
    // enters a fill loop, so initializing only there (or in the unused
    // cancel_settlement path) left all agent error hooks as silent no-ops.
    // Idempotent; a no-op for commands that never report.
    agent_logic::error_reporter::init_from_config(&base_config);

    let verbose = cli.verbose;
    let dry_run = cli.dry_run;
    let force = cli.force;
    let confirm = cli.confirm;

    let version_info = format!(
        "{}{} commit {}",
        &env!("VERGEN_GIT_SHA")[..12],
        if env!("VERGEN_GIT_DIRTY") == "true" { "-dirty" } else { "" },
        env!("VERGEN_GIT_COMMIT_TIMESTAMP"),
    );

    match cli.command {
        Commands::Agent {
            settlement_only,
            orders_only,
            no_restore,
            no_reject,
        } => run_cloud_agent(base_config, settlement_only, orders_only, no_restore, no_reject, verbose, dry_run, force, confirm, Some(&version_info)).await,
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
        Commands::GeneratePrivateKey => unreachable!(),
        Commands::Onboard { .. } => unreachable!(),
    }
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
    read_key_with_retry(|| rpassword::prompt_password("Private key (base58, input hidden): "))
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
            Err(e) if attempt == 0 => eprintln!("Invalid key: {e:#}"),
            Err(e) => return Err(e.context("invalid private key")),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
}
