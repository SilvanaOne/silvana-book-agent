//! `usdcx`: move USDC between Ethereum and Canton through Circle's xReserve
//! and Digital Asset's USDCx bridge.
//!
//! Canton operations (onboard, mint, burn, reclaim, pre-approval, offboard)
//! are typed operations of the ledger service: the server builds each
//! transaction, this agent verifies it strictly (pinned parties and packages,
//! matching hash), signs it with the party's key and executes it. Ethereum
//! deposits are either printed for a wallet or signed here with a key typed
//! at a hidden prompt, which is never stored.
//!
//! Exit codes: 0 done; 1 nothing was committed (the command may be run
//! again); 3 something was or may have been submitted and is not finished —
//! continue with `usdcx track` or the same `--request-id`, never by starting
//! the operation again.

#![cfg_attr(not(test), allow(renamed_and_removed_lints), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented, clippy::indexing_slicing, clippy::string_slice, clippy::unchecked_duration_subtraction, clippy::arithmetic_side_effects, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro, clippy::disallowed_methods), warn(renamed_and_removed_lints))]

pub mod acs;
mod agreement;
mod burn;
pub mod chain;
pub mod circle;
mod deposit;
pub mod eth;
pub mod ethaddr;
pub mod state;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use agent_logic::config::BaseConfig;
use agent_logic::secret::Zeroizing;
use anyhow::{Result, anyhow, bail};
use clap::Subcommand;
use tx_verifier::usdcx::UsdcxNetwork;

use crate::ledger_client::DAppProviderClient;
use chain::EthNetwork;

/// Request timeout of the ledger client of these commands: above the
/// ledger service's own 90 s budget and orderbook-rpc's 120 s bound, so
/// their answer arrives before the client gives up.
const REQUEST_TIMEOUT_SECS: u64 = 150;

#[derive(Subcommand)]
pub enum UsdcxCommands {
    /// The party's bridge state: agreement, deposits, withdrawals, USDCx balance
    Status,
    /// Onboard the party to the USDCx bridge (DA accepts within about a minute)
    Onboard {
        /// Let DA mint deposits directly, without a DepositAttestation to mint
        #[arg(long)]
        pre_approval: bool,
        /// Reuse the request id an interrupted onboarding printed
        #[arg(long)]
        request_id: Option<String>,
        /// How long to wait for DA's answer
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
    },
    /// Ethereum USDC → Canton USDCx: print the transactions for a wallet, or
    /// send them with an Ethereum key typed at a hidden prompt
    Deposit {
        /// USDC amount (at most 6 decimals)
        #[arg(long)]
        amount: String,
        /// Only print DA's deposit page and the transactions to send from a wallet
        #[arg(long)]
        print_only: bool,
        /// With --print-only: check this Ethereum address's balance and allowance
        #[arg(long, requires = "print_only")]
        from: Option<String>,
        /// Stop at the DepositAttestation instead of minting it
        #[arg(long)]
        no_mint: bool,
        /// How long to wait for the deposit to reach Canton
        #[arg(long, default_value_t = 45)]
        timeout_mins: u64,
    },
    /// Mint a DepositAttestation of the party
    Mint {
        /// The attestation to mint (required when several are pending)
        #[arg(long)]
        attestation_cid: Option<String>,
        /// Wait for an attestation to appear
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 45)]
        timeout_mins: u64,
    },
    /// Canton USDCx → Ethereum USDC (Circle releases it, minus its fee)
    Burn {
        /// USDCx amount to burn (at most 6 decimals)
        #[arg(long)]
        amount: String,
        /// Ethereum address that receives the USDC
        #[arg(long)]
        to: String,
        /// Continue the withdrawal an interrupted run printed
        #[arg(long)]
        request_id: Option<String>,
        /// Burn the amount plus Circle's fee, so that the amount arrives
        #[arg(long)]
        fee_on_top: bool,
        /// How long to wait for DA's outcome
        #[arg(long, default_value_t = 60)]
        timeout_mins: u64,
    },
    /// Take back the USDCx of a withdrawal DA revoked
    Reclaim {
        /// The RevokedWithdrawAttestation
        #[arg(long)]
        revoked_cid: String,
    },
    /// Switch DA's direct minting of deposits on or off
    PreApproval {
        #[arg(long, conflicts_with = "disable", required_unless_present = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
    },
    /// End the party's bridge agreement
    Offboard {
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Continue open withdrawals and deposits
    Track {
        /// Register a deposit sent from a wallet
        #[arg(long)]
        eth_tx: Option<String>,
        /// Wait until they finish
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 45)]
        timeout_mins: u64,
    },
}

impl UsdcxCommands {
    /// Whether the command sends from Ethereum with a prompted key.
    pub fn needs_ethereum_key(&self) -> bool {
        matches!(
            self,
            UsdcxCommands::Deposit {
                print_only: false,
                ..
            }
        )
    }
}

/// Options shared by the `usdcx` commands.
#[derive(Debug, Clone, Default)]
pub struct UsdcxOptions {
    /// `mainnet` or `testnet`; defaults to the network of the configured synchronizer.
    pub network: Option<String>,
    /// Base of the local state; defaults to `~/.silvana-agent/usdcx`.
    pub state_dir: Option<PathBuf>,
    /// Do not ask for confirmations.
    pub yes: bool,
}

/// Marks an error after which an operation was or may have been submitted or
/// sent and is not finished: exit code 3.
#[derive(Debug, Clone, Copy)]
pub struct Pending;

impl fmt::Display for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the operation may have been submitted and is not finished")
    }
}

impl std::error::Error for Pending {}

/// A [`Pending`] error with `message`.
pub(crate) fn pending(message: impl fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Pending).context(message.to_string())
}

/// Whether `err` means "may have been submitted, not finished" (exit code 3).
pub fn is_pending(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Pending>().is_some()
}

/// Error texts after which a submission may have committed: an earlier
/// execute that may still land, the ledger service's unknown outcome or open
/// burn, and a command id the participant already has.
const IN_DOUBT: [&str; 5] = [
    "USDCX_OUTCOME_UNKNOWN",
    "USDCX_BURN_PENDING",
    "DUPLICATE_COMMAND",
    "SUBMISSION_ALREADY_IN_FLIGHT",
    "Command already submitted",
];

/// Whether a submission error leaves its outcome in doubt: the operation
/// may have committed, so it is finished from the ledger or by running the
/// same command again, never reported as failed.
pub(crate) fn in_doubt(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    crate::ledger_client::is_ambiguous_execute_error(err)
        || IN_DOUBT.iter().any(|m| text.contains(m))
}

/// What every command works with.
pub(crate) struct Ctx {
    pub client: DAppProviderClient,
    pub party: String,
    pub net: &'static UsdcxNetwork,
    pub eth: &'static EthNetwork,
    /// The party's state directory for this network.
    pub dir: PathBuf,
    pub verbose: bool,
    pub dry_run: bool,
    /// Skip confirmations.
    pub yes: bool,
    /// Confirm every submission (the global `--confirm`).
    pub confirm_all: bool,
}

impl Ctx {
    pub fn reader(&mut self) -> acs::Reader<'_> {
        acs::Reader {
            client: &mut self.client,
            net: self.net,
            party: &self.party,
        }
    }

    /// Asks before an irreversible step (always for `risky` ones) unless `--yes`.
    pub async fn confirm(&self, risky: bool, action: &str, details: &str) -> Result<()> {
        if self.yes || self.dry_run || !(risky || self.confirm_all) {
            return Ok(());
        }
        let lock = agent_logic::confirm::new_confirm_lock();
        agent_logic::confirm::confirm_transaction(&lock, action, details).await
    }
}

/// The network named by `--network`, else the one of the configured synchronizer.
fn network(options: &UsdcxOptions, config: &BaseConfig) -> Result<&'static UsdcxNetwork> {
    let configured = config.synchronizer_id.trim();
    let net = match options.network.as_deref() {
        Some(name) => UsdcxNetwork::by_name(name)?,
        None if configured.is_empty() => {
            bail!("no synchronizer is configured: pass --network mainnet or --network testnet")
        }
        None => UsdcxNetwork::by_synchronizer(configured)?,
    };
    if !configured.is_empty() && configured != net.synchronizer_id {
        bail!(
            "the configured synchronizer {configured} is not the {} synchronizer {}",
            net.name,
            net.synchronizer_id
        );
    }
    Ok(net)
}

/// Runs one `usdcx` command. `eth_key` is the Ethereum key typed at a hidden
/// prompt, for the commands that send from Ethereum.
#[allow(clippy::too_many_arguments)]
pub async fn run_usdcx(
    config: BaseConfig,
    options: UsdcxOptions,
    command: UsdcxCommands,
    verbose: bool,
    dry_run: bool,
    force: bool,
    confirm: bool,
    eth_key: Option<Zeroizing<String>>,
) -> Result<()> {
    if force {
        bail!(
            "--force is not accepted by usdcx commands: they are signed only when the \
             transaction verifies"
        );
    }
    let net = network(&options, &config)?;
    let eth = EthNetwork::by_name(net.name)?;
    let base = match options.state_dir.clone() {
        Some(dir) => dir,
        None => state::default_base()?,
    };
    let dir = state::party_dir(&base, net.name, &config.party_id);
    let client = DAppProviderClient::new(
        &config.orderbook_grpc_url,
        &config.party_id,
        &config.role,
        &config.private_key,
        config.token_ttl_secs,
        Some(config.node_name.as_str()),
        &config.ledger_service_public_key,
        Some(config.connection_timeout_secs),
        Some(REQUEST_TIMEOUT_SECS),
    )
    .await?;
    let mut ctx = Ctx {
        client,
        party: config.party_id.clone(),
        net,
        eth,
        dir,
        verbose,
        dry_run,
        yes: options.yes,
        confirm_all: confirm,
    };
    match command {
        UsdcxCommands::Status => agreement::status(&mut ctx).await,
        UsdcxCommands::Onboard {
            pre_approval,
            request_id,
            timeout_secs,
        } => {
            agreement::onboard(
                &mut ctx,
                pre_approval,
                request_id,
                Duration::from_secs(timeout_secs),
            )
            .await
        }
        UsdcxCommands::Deposit {
            amount,
            print_only,
            from,
            no_mint,
            timeout_mins,
        } => {
            let args = deposit::DepositArgs {
                amount,
                print_only,
                from,
                no_mint,
                timeout: minutes(timeout_mins)?,
            };
            deposit::deposit(&mut ctx, args, eth_key).await
        }
        UsdcxCommands::Mint {
            attestation_cid,
            wait,
            timeout_mins,
        } => deposit::mint(&mut ctx, attestation_cid, wait, minutes(timeout_mins)?).await,
        UsdcxCommands::Burn {
            amount,
            to,
            request_id,
            fee_on_top,
            timeout_mins,
        } => {
            let args = burn::BurnArgs {
                amount,
                to,
                request_id,
                fee_on_top,
                timeout: minutes(timeout_mins)?,
            };
            burn::burn(&mut ctx, args).await
        }
        UsdcxCommands::Reclaim { revoked_cid } => burn::reclaim(&mut ctx, &revoked_cid).await,
        UsdcxCommands::PreApproval { enable, disable } => {
            agreement::pre_approval(&mut ctx, enable && !disable).await
        }
        UsdcxCommands::Offboard { reason } => agreement::offboard(&mut ctx, &reason).await,
        UsdcxCommands::Track {
            eth_tx,
            wait,
            timeout_mins,
        } => {
            let timeout = minutes(timeout_mins)?;
            if let Some(hash) = eth_tx {
                deposit::register_wallet_deposit(&mut ctx, &hash).await?;
            }
            let burns = burn::track(&mut ctx, wait, timeout).await;
            let deposits = deposit::track(&mut ctx, wait, timeout).await;
            match (burns, deposits) {
                (Ok(()), Ok(())) => Ok(()),
                // A pending one wins: something is still open.
                (Err(b), Err(d)) => Err(if is_pending(&d) && !is_pending(&b) {
                    d
                } else {
                    b
                }),
                (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
            }
        }
    }
}

fn minutes(m: u64) -> Result<Duration> {
    m.checked_mul(60)
        .map(Duration::from_secs)
        .ok_or_else(|| anyhow!("--timeout-mins {m} is too large"))
}
