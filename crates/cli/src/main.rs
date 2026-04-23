//! Cloud Agent CLI — thin wrapper around the `orderbook-cloud-agent` library.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use cloud_agent::{
    InfoCommands, PreapprovalCommands, TransferCommands, SignCommands,
    UserServiceCommands, SubscriptionCommands, LockCommands, FaucetCommands,
    populate_instruments, run_cloud_agent, run_fill, run_info, run_preapproval,
    run_subscription, run_transfer, run_sign, run_user_service, run_faucet,
    run_lock, run_generate_private_key, run_onboard,
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
        /// Party ID — optional, skip waiting list and go straight to ledger onboarding (requires --private-key)
        #[arg(long, requires = "private_key")]
        party: Option<String>,
        /// Base58-encoded Ed25519 private key — optional, required when --party is provided
        #[arg(long)]
        private_key: Option<String>,
        /// Path to .env file — optional (default: .env)
        #[arg(long, default_value = ".env")]
        env_file: PathBuf,
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
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

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

    if let Commands::Onboard { rpc, party, private_key, invite_code, agent_name, email, env_file, poll_interval } = cli.command {
        return run_onboard(rpc, party, private_key, invite_code, agent_name, email, env_file, poll_interval).await;
    }

    // `Agent` runs the full market-making loop and needs agent.toml to be present
    // and populated. All other subcommands work with serde defaults if the file
    // is missing (buy/sell/faucet/transfer/etc. only touch env-sourced fields).
    let needs_agent_toml = matches!(cli.command, Commands::Agent { .. });

    let mut base_config = if needs_agent_toml {
        if !cli.config.exists() {
            tracing::warn!(
                "agent.toml not found at {}. This file is required for `cloud-agent agent`.",
                cli.config.display()
            );
        }
        config::load(&cli.config)
            .with_context(|| format!("Failed to load config from {:?}", cli.config))?
    } else {
        config::load_or_defaults(&cli.config)
            .with_context(|| format!("Failed to load config from {:?}", cli.config))?
    };

    // Populate instrument registry (CC → Amulet + DSO, USDC → registry, …) from
    // orderbook-rpc. Required by any command that builds DVP/transfer expectations.
    // Best-effort: failures are logged but don't block commands that don't need it
    // (e.g. `sign`, `generate-private-key`).
    if let Err(e) = populate_instruments(&mut base_config).await {
        tracing::warn!("Failed to populate instrument registry from RPC: {:#}", e);
    }
    let base_config = base_config;

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
        Commands::Buy { market, amount, price_limit, min_settlement, max_settlement, interval } => {
            run_fill(base_config, fill_loop::FillDirection::Buy, market, amount, price_limit, min_settlement, max_settlement, interval, verbose, dry_run, force, confirm).await
        }
        Commands::Sell { market, amount, price_limit, min_settlement, max_settlement, interval } => {
            run_fill(base_config, fill_loop::FillDirection::Sell, market, amount, price_limit, min_settlement, max_settlement, interval, verbose, dry_run, force, confirm).await
        }
        Commands::GeneratePrivateKey => unreachable!(),
        Commands::Onboard { .. } => unreachable!(),
    }
}
